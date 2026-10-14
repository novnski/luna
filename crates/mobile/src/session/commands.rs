//! RM-04: the runtime session's command outbox. A phone creates, sends to,
//! queues for, steers, stops and archives its Mac's tasks only through
//! `ExecuteRemoteTaskCommand` on its authenticated socket; the Mac admits
//! each command for this device session into its durable receipt store.
//!
//! The outbox (`luna_sdk::outbox::Outbox`) belongs to the runtime, not to a
//! view: commands are delivered and reconciled whether or not a chat is
//! open, across suspension, network changes and app restarts
//! ([`RemoteSession::outbox_bytes`] / [`RemoteSession::restore_outbox`]).
//! Each verified socket runs one reconciler ([`run_outbox`]):
//!
//! 1. every entry whose answer was lost is read back by its id before
//!    anything is resent; with no receipt on the Mac it is resent unchanged;
//! 2. ready entries go out oldest first, per task in order, each fenced from
//!    a fresh snapshot of its task just before its first send;
//! 3. admitted (pending) receipts are re-read until terminal, and a queued
//!    message's dispatch receipt is followed to delivered or held.
//!
//! Live controls (stop, steer, send now) are fenced when the user acts and
//! need an open socket; they are never sent later from an offline outbox.
//! The failure contract is `docs/pending-tasks/control-this-mac/rm-04/README.md`.

use std::sync::Arc;
use std::time::Duration;

use luna_proto::remote_command::{
    REMOTE_TASK_CONTROL_V1, RemoteFollowUp, RemoteNewWorktree, RemotePermissionMode,
    RemoteTaskCommand, RemoteTaskIntent,
};
use luna_sdk::SdkError;
use luna_sdk::outbox::{
    DeliveryState, Outbox, OutboxEntry, OutboxError, OutboxStatus, apply_fences, new_command_id,
};

use super::{
    Core, Phase, RemoteSession, RemoteSessionError, SdkClient, from_sdk_ref, now_ms, wire_name,
};
use crate::runtime::{Cancellation, run};

/// Admitted receipts and deliveries are re-read this often while any is open.
const OUTBOX_POLL: Duration = Duration::from_secs(1);
/// After a failed exchange the reconciler waits this long on this socket.
const OUTBOX_RETRY: Duration = Duration::from_secs(3);
/// With nothing open it still wakes this often (a cheap no-op pass).
const OUTBOX_IDLE: Duration = Duration::from_secs(30);
/// Receipt and delivery reads per pass, so one pass stays bounded.
const READS_PER_PASS: usize = 8;

/// Where one command stands, as the phone shows it. Only the Mac's own
/// receipts move it past `uncertain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum OutboxEntryStatus {
    /// Kept on this phone for a task the Mac has not created yet.
    Draft,
    /// Waiting behind an earlier command for the same task.
    Waiting,
    /// Not sent: an earlier command for the same task was refused
    /// (`blocked_by`). Discard that one to release it.
    Held,
    /// Sent on the next verified connection.
    Ready,
    Sending,
    /// The answer was lost; its receipt is read by id before any resend.
    Uncertain,
    /// The Mac admitted it and owns it now.
    Accepted,
    /// Admitted, but the Mac could not prove its provider accepted it:
    /// check the task on the Mac before sending it again.
    Unknown,
    Applied,
    Refused,
}

/// The permission mode of a task this phone creates. New work has only
/// these: Full access never asks; Auto lets the provider's own reviewer
/// decide. Legacy Ask cannot be chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TaskPermissionMode {
    Full,
    Auto,
}

/// A queued message's delivery on the Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum OutboxDeliveryState {
    /// In the Mac's queue (behind the running turn or worktree setup).
    Queued,
    Dispatching,
    Delivered,
    /// Held on the Mac, for example after a failed worktree setup; it stays
    /// queued there until an explicit send.
    Held,
    Unknown,
}

/// One outbox command as the phone shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OutboxEntryRecord {
    pub command_id: String,
    pub task_id: String,
    /// The command's wire kind (`createTask`, `send`, `stop`, ...).
    pub kind: String,
    /// A message's text (sends only); its id is the command id.
    pub text: Option<String>,
    pub status: OutboxEntryStatus,
    /// The earlier command of the same task this one waits for or is held by.
    pub blocked_by: Option<String>,
    /// The Mac receipt's status wire name, once it answered.
    pub receipt_status: Option<String>,
    pub reason_code: Option<String>,
    /// Why it was refused, is unknown, or was not sent.
    pub detail: Option<String>,
    /// What the Mac suggests doing next.
    pub recovery: Option<String>,
    pub delivery: Option<OutboxDeliveryState>,
    pub delivery_detail: Option<String>,
    /// Times it was sent (always the same id and payload).
    pub sends: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[uniffi::export]
impl RemoteSession {
    /// RM-04: create a task in one of the Mac's projects. Returns the new
    /// task's id (minted here, so a retried create names the same task).
    /// Messages sent to it before the Mac created it stay drafts. Without
    /// `permission_mode` the Mac uses Auto when its provider and model offer
    /// it; otherwise the create is refused with `permissionModeRequired`
    /// (offer Full access, then create again).
    pub fn create_task(
        &self,
        project_id: String,
        permission_mode: Option<TaskPermissionMode>,
    ) -> Result<String, RemoteSessionError> {
        let task_id = new_command_id();
        self.core.enqueue(
            command(
                task_id.clone(),
                RemoteTaskIntent::CreateTask {
                    project_id,
                    permission_mode: permission_mode.map(|mode| match mode {
                        TaskPermissionMode::Full => RemotePermissionMode::Full,
                        TaskPermissionMode::Auto => RemotePermissionMode::Auto,
                    }),
                },
            ),
            false,
        )?;
        Ok(task_id)
    }

    /// RM-04: send a message to a task; returns its command id, which is
    /// also its message id on the Mac. To an idle task it starts the next
    /// turn; while the task is busy it waits in the Mac's queue, or with
    /// `steer` steers the running turn (a live control: it needs an open
    /// connection and a running turn). With `new_worktree` (only a task's
    /// first message) the turn runs in a fresh worktree of the task's
    /// project, from `worktree_base` or the project's `HEAD`; later messages
    /// wait behind its setup and are held if the setup fails or is cancelled.
    pub async fn send_message(
        &self,
        task_id: String,
        text: String,
        steer: bool,
        new_worktree: bool,
        worktree_base: Option<String>,
        cancel: Option<Arc<Cancellation>>,
    ) -> Result<String, RemoteSessionError> {
        let core = self.core.clone();
        run(cancel, async move {
            let id = new_command_id();
            let command = RemoteTaskCommand {
                command_id: id.clone(),
                task_id,
                expected_task_revision: None,
                expected_binding_generation: None,
                expected_turn_id: None,
                issued_at_ms: Some(now_ms()),
                intent: RemoteTaskIntent::Send {
                    message_id: id,
                    text,
                    follow_up: if steer {
                        RemoteFollowUp::Steer
                    } else {
                        RemoteFollowUp::Queue
                    },
                    new_worktree: new_worktree.then_some(RemoteNewWorktree {
                        base: worktree_base,
                    }),
                },
            };
            if steer {
                core.enqueue_live(command, true).await
            } else {
                core.enqueue(command, false)
            }
        })
        .await
    }

    /// RM-04: stop the task's running turn (a live control). Refused at
    /// once when nothing runs or the connection is not open.
    pub async fn stop_task(
        &self,
        task_id: String,
        cancel: Option<Arc<Cancellation>>,
    ) -> Result<String, RemoteSessionError> {
        let core = self.core.clone();
        run(cancel, async move {
            core.enqueue_live(command(task_id, RemoteTaskIntent::Stop {}), true)
                .await
        })
        .await
    }

    /// RM-04: remove a queued message, fenced by its queue revision (a
    /// message this phone queued and nobody edited is at revision 1).
    pub fn remove_queued(
        &self,
        task_id: String,
        message_id: String,
        expected_queue_revision: u64,
    ) -> Result<String, RemoteSessionError> {
        self.core.enqueue(
            command(
                task_id,
                RemoteTaskIntent::RemoveQueued {
                    message_id,
                    expected_queue_revision,
                },
            ),
            false,
        )
    }

    /// RM-04: interrupt the running turn (if any) and send this queued
    /// message now (a live control).
    pub async fn send_queued_now(
        &self,
        task_id: String,
        message_id: String,
        expected_queue_revision: u64,
        cancel: Option<Arc<Cancellation>>,
    ) -> Result<String, RemoteSessionError> {
        let core = self.core.clone();
        run(cancel, async move {
            let intent = RemoteTaskIntent::SendQueuedNow {
                message_id,
                expected_queue_revision,
            };
            core.enqueue_live(command(task_id, intent), false).await
        })
        .await
    }

    /// RM-04: steer this queued message into the running turn (a live
    /// control). Refused at once when nothing runs.
    pub async fn steer_queued_now(
        &self,
        task_id: String,
        message_id: String,
        expected_queue_revision: u64,
        cancel: Option<Arc<Cancellation>>,
    ) -> Result<String, RemoteSessionError> {
        let core = self.core.clone();
        run(cancel, async move {
            let intent = RemoteTaskIntent::SteerQueuedNow {
                message_id,
                expected_queue_revision,
            };
            core.enqueue_live(command(task_id, intent), true).await
        })
        .await
    }

    /// RM-04: archive or restore a task.
    pub fn set_task_archived(
        &self,
        task_id: String,
        archived: bool,
    ) -> Result<String, RemoteSessionError> {
        self.core.enqueue(
            command(task_id, RemoteTaskIntent::SetArchived { archived }),
            false,
        )
    }

    /// RM-04: withdraw a send that started a turn (receipt `run`) while it
    /// waits for a free turn on the Mac. A send that started is refused.
    pub fn cancel_capacity_wait(
        &self,
        task_id: String,
        command_id: String,
    ) -> Result<String, RemoteSessionError> {
        self.core.enqueue(
            command(task_id, RemoteTaskIntent::CancelCapacityWait { command_id }),
            false,
        )
    }

    /// RM-04: every outbox command, oldest first.
    pub fn outbox(&self) -> Vec<OutboxEntryRecord> {
        let inner = self.core.lock();
        let Some(follow) = inner.follow.as_ref() else {
            return Vec::new();
        };
        let outbox = &follow.outbox;
        outbox
            .entries()
            .iter()
            .map(|entry| record(outbox, entry))
            .collect()
    }

    /// RM-04: moves whenever the outbox changed; re-read it (and save
    /// [`RemoteSession::outbox_bytes`]) only when this moved.
    pub fn outbox_revision(&self) -> u64 {
        self.core
            .lock()
            .follow
            .as_ref()
            .map_or(0, |follow| follow.outbox.revision())
    }

    /// RM-04: why the outbox is not being delivered on this connection (an
    /// older Mac, or access without task control), if it is not.
    pub fn outbox_issue(&self) -> Option<String> {
        self.core.lock().follow.as_ref()?.outbox_issue.clone()
    }

    /// RM-04: the outbox to keep for the next run, bounded. Save it whenever
    /// [`RemoteSession::outbox_revision`] moved, in protected,
    /// non-synchronized storage scoped to this saved Mac, and delete it when
    /// the Mac is forgotten or paired again. Empty for an IOS-00 session.
    pub fn outbox_bytes(&self) -> Vec<u8> {
        self.core
            .lock()
            .follow
            .as_ref()
            .map(|follow| follow.outbox.encode())
            .unwrap_or_default()
    }

    /// RM-04: restore what [`RemoteSession::outbox_bytes`] saved for this
    /// same Mac and session, before any command is added. Commands that were
    /// in flight come back uncertain and are read back by id on the next
    /// socket. Returns why it was not restored, if it was not.
    pub fn restore_outbox(&self, bytes: Vec<u8>) -> Option<String> {
        let restored = {
            let mut inner = self.core.lock();
            let Some(follow) = inner.follow.as_mut() else {
                return Some("this session does not send commands".into());
            };
            if !follow.outbox.entries().is_empty() {
                return Some("the outbox already holds commands".into());
            }
            let decoded = Outbox::decode(&bytes, follow.outbox.key(), &self.core.actor_id);
            match decoded {
                Ok(outbox) => {
                    follow.outbox = outbox;
                    None
                }
                Err(error) => Some(error.to_string()),
            }
        };
        if restored.is_none() {
            self.core.outbox_wake.notify_one();
            self.core.signal();
        }
        restored
    }

    /// RM-04: remove a command the Mac does not own: never sent, or settled.
    /// Discarding a refused command releases the later ones it held.
    pub fn discard_outbox_entry(&self, command_id: String) -> Result<(), RemoteSessionError> {
        {
            let mut inner = self.core.lock();
            let follow =
                inner
                    .follow
                    .as_mut()
                    .ok_or_else(|| RemoteSessionError::InvalidRequest {
                        detail: "this session does not send commands".into(),
                    })?;
            follow.outbox.discard(&command_id).map_err(outbox_error)?;
        }
        self.core.outbox_wake.notify_one();
        self.core.signal();
        Ok(())
    }
}

fn command(task_id: String, intent: RemoteTaskIntent) -> RemoteTaskCommand {
    RemoteTaskCommand {
        command_id: new_command_id(),
        task_id,
        expected_task_revision: None,
        expected_binding_generation: None,
        expected_turn_id: None,
        issued_at_ms: Some(now_ms()),
        intent,
    }
}

fn outbox_error(error: OutboxError) -> RemoteSessionError {
    RemoteSessionError::InvalidRequest {
        detail: error.to_string(),
    }
}

fn record(outbox: &Outbox, entry: &OutboxEntry) -> OutboxEntryRecord {
    let id = entry.command_id();
    let status = match outbox.status(id).unwrap_or(OutboxStatus::Ready) {
        OutboxStatus::Draft => OutboxEntryStatus::Draft,
        OutboxStatus::Waiting => OutboxEntryStatus::Waiting,
        OutboxStatus::Held => OutboxEntryStatus::Held,
        OutboxStatus::Ready => OutboxEntryStatus::Ready,
        OutboxStatus::Sending => OutboxEntryStatus::Sending,
        OutboxStatus::Uncertain => OutboxEntryStatus::Uncertain,
        OutboxStatus::Accepted => OutboxEntryStatus::Accepted,
        OutboxStatus::Unknown => OutboxEntryStatus::Unknown,
        OutboxStatus::Applied => OutboxEntryStatus::Applied,
        OutboxStatus::Refused => OutboxEntryStatus::Refused,
    };
    let receipt = entry.receipt.as_ref();
    OutboxEntryRecord {
        command_id: id.to_owned(),
        task_id: entry.command.task_id.clone(),
        kind: entry.command.intent.kind().into(),
        text: match &entry.command.intent {
            RemoteTaskIntent::Send { text, .. } => Some(text.clone()),
            _ => None,
        },
        status,
        blocked_by: outbox.blocked_by(id).map(str::to_owned),
        receipt_status: receipt.map(|receipt| wire_name(&receipt.status)),
        reason_code: receipt
            .and_then(|receipt| receipt.reason.as_ref())
            .map(|reason| wire_name(&reason.code)),
        detail: receipt
            .and_then(|receipt| receipt.reason.as_ref())
            .map(|reason| reason.message.clone())
            .or_else(|| entry.refusal.clone()),
        recovery: receipt.and_then(|receipt| receipt.recovery_action.clone()),
        delivery: entry
            .delivery
            .as_ref()
            .map(|delivery| match delivery.state {
                DeliveryState::Queued => OutboxDeliveryState::Queued,
                DeliveryState::Dispatching => OutboxDeliveryState::Dispatching,
                DeliveryState::Delivered => OutboxDeliveryState::Delivered,
                DeliveryState::Held => OutboxDeliveryState::Held,
                DeliveryState::Unknown => OutboxDeliveryState::Unknown,
            }),
        delivery_detail: entry
            .delivery
            .as_ref()
            .and_then(|delivery| delivery.detail.clone()),
        sends: entry.sends,
        created_at_ms: entry.created_at_ms,
        updated_at_ms: entry.updated_at_ms,
    }
}

impl Core {
    /// Commands need a runtime session whose saved access operates tasks.
    fn can_operate(&self) -> Result<(), RemoteSessionError> {
        {
            let inner = self.lock();
            if matches!(inner.phase, Phase::Closed(..)) {
                drop(inner);
                return Err(self.closed_error());
            }
            if inner.follow.is_none() {
                return Err(RemoteSessionError::InvalidRequest {
                    detail: "this session does not send commands".into(),
                });
            }
        }
        if !self.operates {
            return Err(RemoteSessionError::Denied {
                detail:
                    "This phone's access does not include operating tasks; pair again on the Mac"
                        .into(),
            });
        }
        Ok(())
    }

    /// Add a command; the reconciler sends it when its task's turn comes.
    fn enqueue(
        &self,
        command: RemoteTaskCommand,
        fenced: bool,
    ) -> Result<String, RemoteSessionError> {
        self.can_operate()?;
        let id = command.command_id.clone();
        {
            let mut inner = self.lock();
            let follow =
                inner
                    .follow
                    .as_mut()
                    .ok_or_else(|| RemoteSessionError::InvalidRequest {
                        detail: "this session does not send commands".into(),
                    })?;
            let result = if fenced {
                follow.outbox.push_fenced(command, now_ms())
            } else {
                follow.outbox.push(command, now_ms())
            };
            result.map_err(outbox_error)?;
        }
        self.outbox_wake.notify_one();
        self.signal();
        Ok(id)
    }

    /// A live control: fenced from the Mac's state now, on an open socket
    /// that admits commands. `needs_turn`: refused at once when nothing runs.
    async fn enqueue_live(
        &self,
        mut command: RemoteTaskCommand,
        needs_turn: bool,
    ) -> Result<String, RemoteSessionError> {
        self.can_operate()?;
        let (client, host) = self.open_client()?;
        if !host
            .capabilities
            .iter()
            .any(|capability| capability == REMOTE_TASK_CONTROL_V1)
        {
            return Err(RemoteSessionError::Unsupported {
                detail: "This Mac's LUNA does not accept commands from the phone yet; update LUNA on the Mac".into(),
            });
        }
        let read = client
            .task_command_snapshot(&command.task_id, None, None)
            .await
            .map_err(|error| self.sdk_error(error))?;
        if needs_turn && read.snapshot.active_turn_id.is_none() {
            return Err(RemoteSessionError::InvalidRequest {
                detail: "Nothing is running on this task".into(),
            });
        }
        apply_fences(&mut command, &read.snapshot);
        self.enqueue(command, true)
    }

    /// Change the outbox for socket `epoch`; `None` once that socket ended.
    fn with_outbox<T>(&self, epoch: u64, change: impl FnOnce(&mut Outbox) -> T) -> Option<T> {
        let result = {
            let mut inner = self.lock();
            let follow = inner.follow.as_mut()?;
            if follow.epoch != epoch {
                return None;
            }
            let before = follow.outbox.revision();
            let result = change(&mut follow.outbox);
            (result, before != follow.outbox.revision())
        };
        if result.1 {
            self.signal();
        }
        Some(result.0)
    }

    fn outbox_entry(&self, command_id: &str) -> Option<OutboxEntry> {
        self.lock()
            .follow
            .as_ref()?
            .outbox
            .entry(command_id)
            .cloned()
    }
}

/// What one reconciler pass ended with.
enum Pass {
    /// Its socket ended: stop.
    Stop,
    /// Wait for a new command or this long.
    Wait(Duration),
}

/// RM-04: deliver and reconcile the outbox on socket `epoch` until that
/// socket ends. See the module documentation.
pub(super) async fn run_outbox(core: Arc<Core>, client: Arc<SdkClient>, epoch: u64) {
    loop {
        if !core.is_epoch(epoch) {
            return;
        }
        let wait = match reconcile(&core, &client, epoch).await {
            Pass::Stop => return,
            Pass::Wait(wait) => wait,
        };
        tokio::select! {
            _ = core.outbox_wake.notified() => {}
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

/// How a failed exchange leaves the pass: a refusal settles the entry; a
/// lost or unreadable answer ends the pass (the socket may be going away).
enum Exchange {
    Continue,
    Retry,
}

async fn reconcile(core: &Arc<Core>, client: &Arc<SdkClient>, epoch: u64) -> Pass {
    let actor = core.actor_id.clone();
    // 1. Read back every entry whose answer was lost, before any resend.
    let uncertain = match core.with_outbox(epoch, |outbox| outbox.uncertain()) {
        Some(ids) => ids,
        None => return Pass::Stop,
    };
    for id in uncertain {
        let Some(entry) = core.outbox_entry(&id) else {
            continue;
        };
        match client.remote_receipt(&entry.command, &actor).await {
            Ok(Some(receipt)) => {
                let applied = core.with_outbox(epoch, |outbox| {
                    outbox
                        .apply_receipt(receipt, now_ms())
                        .map_err(|error| error.to_string())
                });
                match applied {
                    None => return Pass::Stop,
                    Some(Err(detail)) => {
                        let _ =
                            core.with_outbox(epoch, |outbox| outbox.refuse(&id, &detail, now_ms()));
                    }
                    Some(Ok(())) => {}
                }
            }
            Ok(None) => {
                if core
                    .with_outbox(epoch, |outbox| outbox.unadmitted(&id, now_ms()))
                    .is_none()
                {
                    return Pass::Stop;
                }
            }
            // The id names another command on the Mac (another client's or
            // another payload's): this one was never admitted under it.
            Err(SdkError::InvalidResponse { detail, .. }) => {
                let _ = core.with_outbox(epoch, |outbox| outbox.refuse(&id, &detail, now_ms()));
            }
            Err(error) => match failed(core, epoch, &id, error, false) {
                Exchange::Continue => {}
                Exchange::Retry => return Pass::Wait(OUTBOX_RETRY),
            },
        }
    }
    // 2. Send what is ready, oldest first, per task in order (each entry at
    // most once per pass).
    let mut tried = std::collections::HashSet::new();
    loop {
        let next = match core.with_outbox(epoch, |outbox| outbox.next_ready().cloned()) {
            Some(next) => next,
            None => return Pass::Stop,
        };
        let Some(entry) = next else {
            break;
        };
        let id = entry.command_id().to_owned();
        if !tried.insert(id.clone()) {
            break;
        }
        if !entry.fenced {
            let snapshot = match &entry.command.intent {
                // Nothing to fence: the task does not exist yet, or the
                // withdrawn send carries its own identity.
                RemoteTaskIntent::CreateTask { .. }
                | RemoteTaskIntent::CancelCapacityWait { .. } => luna_proto::TaskCommandSnapshot {
                    task_id: entry.command.task_id.clone(),
                    ..Default::default()
                },
                _ => match client
                    .task_command_snapshot(&entry.command.task_id, None, None)
                    .await
                {
                    Ok(read) => read.snapshot,
                    Err(error) => match failed(core, epoch, &id, error, true) {
                        Exchange::Continue => continue,
                        Exchange::Retry => return Pass::Wait(OUTBOX_RETRY),
                    },
                },
            };
            if core
                .with_outbox(epoch, |outbox| outbox.fence(&id, &snapshot, now_ms()))
                .is_none()
            {
                return Pass::Stop;
            }
        }
        let command = match core.with_outbox(epoch, |outbox| outbox.begin_send(&id, now_ms())) {
            None => return Pass::Stop,
            Some(Err(_)) => break,
            Some(Ok(command)) => command,
        };
        match client.execute_remote(&command, &actor).await {
            Ok(receipt) => {
                let applied = core.with_outbox(epoch, |outbox| {
                    outbox
                        .apply_receipt(receipt, now_ms())
                        .map_err(|error| error.to_string())
                });
                match applied {
                    None => return Pass::Stop,
                    Some(Err(detail)) => {
                        let _ =
                            core.with_outbox(epoch, |outbox| outbox.refuse(&id, &detail, now_ms()));
                    }
                    Some(Ok(())) => {}
                }
            }
            Err(error) => match failed(core, epoch, &id, error, false) {
                Exchange::Continue => {}
                Exchange::Retry => return Pass::Wait(OUTBOX_RETRY),
            },
        }
    }
    // 3. Follow admitted receipts and queued deliveries, bounded per pass.
    let (pending, deliveries) =
        match core.with_outbox(epoch, |outbox| (outbox.pending(), outbox.deliveries())) {
            Some(open) => open,
            None => return Pass::Stop,
        };
    for id in pending.iter().take(READS_PER_PASS) {
        let Some(entry) = core.outbox_entry(id) else {
            continue;
        };
        match client.remote_receipt(&entry.command, &actor).await {
            Ok(Some(receipt)) => {
                if core
                    .with_outbox(epoch, |outbox| outbox.apply_receipt(receipt, now_ms()))
                    .is_none()
                {
                    return Pass::Stop;
                }
            }
            Ok(None) => {}
            Err(_) => return Pass::Wait(OUTBOX_RETRY),
        }
    }
    for id in deliveries.iter().take(READS_PER_PASS) {
        let Some(probe) = core
            .outbox_entry(id)
            .and_then(|entry| entry.delivery_probe())
        else {
            continue;
        };
        match client.task_command_receipt(&probe, None).await {
            Ok(dispatch) => {
                if core
                    .with_outbox(epoch, |outbox| {
                        outbox.apply_delivery(id, dispatch.as_ref(), now_ms())
                    })
                    .is_none()
                {
                    return Pass::Stop;
                }
            }
            Err(_) => return Pass::Wait(OUTBOX_RETRY),
        }
    }
    let open = core
        .with_outbox(epoch, |outbox| {
            !outbox.pending().is_empty() || !outbox.deliveries().is_empty()
        })
        .unwrap_or(false);
    Pass::Wait(if open { OUTBOX_POLL } else { OUTBOX_IDLE })
}

/// A failed exchange for entry `id`. A refusal the Mac answered settles the
/// entry (`unsent`: it was never sent, as for a refused fence read). A lost
/// or unreadable answer leaves a sending entry uncertain and retries later.
fn failed(core: &Arc<Core>, epoch: u64, id: &str, error: SdkError, unsent: bool) -> Exchange {
    let detail = from_sdk_ref(&error).to_string();
    if error.may_have_been_admitted() {
        if !unsent {
            let _ = core.with_outbox(epoch, |outbox| {
                outbox.send_failed(id, true, &detail, now_ms())
            });
        }
        return Exchange::Retry;
    }
    let _ = core.with_outbox(epoch, |outbox| outbox.refuse(id, &detail, now_ms()));
    Exchange::Continue
}
