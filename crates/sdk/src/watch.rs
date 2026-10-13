//! A task-command watch that never delivers a gap. Terminal receipts carry
//! their durable task sequence; the watch keeps the last one delivered as its
//! cursor, drops duplicates, and when a receipt skips ahead (a lagged stream,
//! or one committed while the stream started) it reads the missing events
//! from the durable page before delivering it. If the cursor is no longer
//! replayable it says so with [`TaskCommandUpdate::Reload`].

use std::collections::VecDeque;

use luna_proto::{CommandReceipt, TaskCommandRead, TaskCommandSnapshot};
use luna_rpc::RpcSubscription;
use serde::Serialize;

use crate::SdkError;
use crate::client::{Client, Transport, decode, validate_read};
use crate::ops::{Operation, TaskCommandSnapshotParams, TaskCommandWatchItem, WatchTaskCommands};

/// One watch update; serialized as the TypeScript SDK yields it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TaskCommandUpdate {
    /// The starting point: the snapshot and, after a requested cursor, every
    /// durable event since it.
    Snapshot { read: TaskCommandRead },
    /// One terminal command event, delivered once, in task-sequence order.
    Event { receipt: CommandReceipt },
    /// A receipt with no durable position yet (accepted, still pending). The
    /// cursor does not move.
    Progress { receipt: CommandReceipt },
    /// Events after the cursor are no longer replayable: replace local state
    /// with this snapshot. The cursor moves to its sequence.
    Reload { snapshot: TaskCommandSnapshot },
}

/// Dropping the watch cancels the engine's stream immediately.
pub struct TaskCommandWatch<'c, T> {
    client: &'c Client<T>,
    subscription: RpcSubscription,
    task_id: String,
    target_device_id: Option<String>,
    requested_after: Option<u64>,
    cursor: Option<u64>,
    ready: VecDeque<TaskCommandUpdate>,
}

impl<'c, T: Transport> TaskCommandWatch<'c, T> {
    pub(crate) async fn open(
        client: &'c Client<T>,
        task_id: &str,
        after: Option<u64>,
        target_device_id: Option<&str>,
    ) -> Result<Self, SdkError> {
        let params = serde_json::to_value(TaskCommandSnapshotParams {
            task_id: task_id.into(),
            after_task_sequence: after,
            limit: after.map(|_| 100),
            target_device_id: target_device_id.map(str::to_owned),
        })
        .map_err(|error| SdkError::Engine(format!("serialize watch: {error}")))?;
        let subscribe = client
            .rpc()
            .subscribe_checked(WatchTaskCommands::METHOD, params);
        let subscription = match client.deadline() {
            Some(after) => {
                tokio::time::timeout(after, subscribe)
                    .await
                    .map_err(|_| SdkError::Timeout {
                        operation: WatchTaskCommands::NAME,
                        after,
                    })?
            }
            None => subscribe.await,
        }
        .map_err(|error| SdkError::from_rpc(WatchTaskCommands::NAME, error))?;
        Ok(Self {
            client,
            subscription,
            task_id: task_id.into(),
            target_device_id: target_device_id.map(str::to_owned),
            requested_after: after,
            cursor: None,
            ready: VecDeque::new(),
        })
    }

    /// The task sequence of the last event delivered (or of the snapshot).
    pub fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    /// The next update; `None` once the engine ends the stream or the
    /// connection closes. Resubscribe from [`TaskCommandWatch::cursor`].
    pub async fn next(&mut self) -> Option<Result<TaskCommandUpdate, SdkError>> {
        loop {
            if let Some(update) = self.ready.pop_front() {
                return Some(Ok(update));
            }
            let value = self.subscription.recv().await?;
            let item = match decode::<TaskCommandWatchItem>(WatchTaskCommands::NAME, value) {
                Ok(item) => item,
                Err(error) => return Some(Err(error)),
            };
            if let Err(error) = self.accept(item).await {
                return Some(Err(error));
            }
        }
    }

    /// Read any durable events past the cursor now, for example after a
    /// reconnect or a long idle period with no receipt to reveal a gap.
    pub async fn resync(&mut self) -> Result<(), SdkError> {
        if let Some(cursor) = self.cursor {
            self.repair(cursor, None).await?;
        }
        Ok(())
    }

    /// Cancel the engine's stream (the same as dropping the watch).
    pub fn close(self) {}

    async fn accept(&mut self, item: TaskCommandWatchItem) -> Result<(), SdkError> {
        match item {
            TaskCommandWatchItem::Snapshot { mut read } => {
                if self.cursor.is_some() {
                    return Err(self.invalid("a second snapshot arrived"));
                }
                validate_read(
                    WatchTaskCommands::NAME,
                    &self.task_id,
                    self.requested_after,
                    &read,
                )?;
                if read.reload_required {
                    self.cursor = Some(read.snapshot.task_sequence);
                    self.ready.push_back(TaskCommandUpdate::Reload {
                        snapshot: read.snapshot,
                    });
                    return Ok(());
                }
                let mut cursor = read
                    .events
                    .last()
                    .map(|event| event.task_sequence)
                    .or(self.requested_after)
                    .unwrap_or(read.snapshot.task_sequence);
                // Complete a bounded first page before anything live.
                while read.has_more {
                    let page = self
                        .client
                        .task_command_snapshot(
                            &self.task_id,
                            Some(cursor),
                            self.target_device_id.as_deref(),
                        )
                        .await?;
                    if page.reload_required {
                        self.cursor = Some(page.snapshot.task_sequence);
                        self.ready.push_back(TaskCommandUpdate::Reload {
                            snapshot: page.snapshot,
                        });
                        return Ok(());
                    }
                    cursor = page
                        .events
                        .last()
                        .map_or(cursor, |event| event.task_sequence);
                    read.has_more = page.has_more && !page.events.is_empty();
                    read.events.extend(page.events);
                    read.snapshot = page.snapshot;
                }
                read.next_task_sequence = read.events.last().map(|event| event.task_sequence);
                self.cursor = Some(cursor);
                self.ready.push_back(TaskCommandUpdate::Snapshot { read });
                Ok(())
            }
            TaskCommandWatchItem::Receipt { receipt } => {
                let receipt = *receipt;
                let Some(cursor) = self.cursor else {
                    return Err(self.invalid("a receipt arrived before the snapshot"));
                };
                if receipt.task_id != self.task_id {
                    return Err(self.invalid(format!(
                        "receipt {} is for task {}",
                        receipt.command_id, receipt.task_id
                    )));
                }
                if !receipt.status.is_terminal() || receipt.event_sequence.is_none() {
                    self.ready
                        .push_back(TaskCommandUpdate::Progress { receipt });
                    return Ok(());
                }
                let sequence = receipt.snapshot.task_sequence;
                if sequence <= cursor {
                    return Ok(());
                }
                if sequence > cursor + 1 {
                    self.repair(cursor, Some(sequence)).await?;
                }
                if self.cursor.is_some_and(|cursor| sequence > cursor) {
                    self.cursor = Some(sequence);
                    self.ready.push_back(TaskCommandUpdate::Event { receipt });
                }
                Ok(())
            }
        }
    }

    /// Deliver the durable events after `cursor`, up to `until` when given.
    async fn repair(&mut self, mut cursor: u64, until: Option<u64>) -> Result<(), SdkError> {
        loop {
            let page = self
                .client
                .task_command_snapshot(
                    &self.task_id,
                    Some(cursor),
                    self.target_device_id.as_deref(),
                )
                .await?;
            if page.reload_required {
                self.cursor = Some(page.snapshot.task_sequence);
                self.ready.push_back(TaskCommandUpdate::Reload {
                    snapshot: page.snapshot,
                });
                return Ok(());
            }
            let has_more = page.has_more && !page.events.is_empty();
            for event in page.events {
                if until.is_some_and(|until| event.task_sequence >= until) {
                    // The receipt that revealed the gap is delivered by its caller.
                    self.cursor = Some(cursor);
                    return Ok(());
                }
                cursor = event.task_sequence;
                self.ready.push_back(TaskCommandUpdate::Event {
                    receipt: event.receipt,
                });
            }
            self.cursor = Some(cursor);
            if !has_more || until.is_some_and(|until| cursor + 1 >= until) {
                return Ok(());
            }
        }
    }

    fn invalid(&self, detail: impl Into<String>) -> SdkError {
        SdkError::invalid(WatchTaskCommands::NAME, detail)
    }
}
