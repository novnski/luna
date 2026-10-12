//! Lifecycle slice (A2, PB-1): Backlog/Ready/Done moves that start, continue
//! and stop work; execution intent, generations and the stop barrier;
//! first-start vs continuation instructions; settings resolution and launch
//! snapshots; bounded batch start with admission; status reports; "Send
//! update to chat"; once-only result delivery; restart reconciliation.
//!
//! Contract: docs/design/project-board/pb0-contract.md §5–§7. Mechanism:
//!
//! * A transition first commits the desired column, a new `intent_revision`
//!   and the accepted request (`pending_start`) together with the column
//!   change and one journal event. Only then does [`drive`] act, always from
//!   the persisted intent, so two windows, a retry with the same request id,
//!   the maintenance tick and a restart all converge on one outcome.
//! * A dispatch freezes a `LaunchSnapshot` (requested vs resolved settings,
//!   the exact instruction text and its hash, command and message ids) and a
//!   new `execution_generation` before sending once through the ordinary
//!   task command path. Delivery is reconciled by the original command and
//!   message id; an unprovable acceptance stays Unknown and is never resent
//!   automatically.
//! * Stop is the chat's own Stop command (a session interrupt) for the exact
//!   generation, extended to the ticket's owned reviewer chats; the barrier
//!   settles only from runtime evidence. Ready behind an open barrier waits
//!   (Waiting for stop) and coalesces toggles into one continuation.
//! * Admission bounds project/host/provider concurrency, keeps one live
//!   writer per checkout and honors visible blockers; queued work stays
//!   Ready with its reason and is cancelled before sending on Backlog.
use super::*;
use luna_proto::{
    BOARD_BATCH_START_MAX, BoardAccountOption, BoardAdmissionReason, BoardBatchItemResult,
    BoardChatIdentityRequest, BoardChatRole, BoardColumn, BoardCompletionMode,
    BoardCompletionPolicy, BoardContextMode, BoardContextRef, BoardControlAction,
    BoardControlReceipt, BoardControlRequest, BoardDestination, BoardExecutionIntent,
    BoardExecutionOptions, BoardExecutionOptionsRequest, BoardExecutionProfile, BoardFollowUp,
    BoardHarnessOptions, BoardInstructionKind, BoardItemOrigin, BoardLaunchState, BoardMoveEffect,
    BoardMoveReceipt, BoardMoveRequest, BoardObservation, BoardOperation, BoardPendingStart,
    BoardPhase, BoardReconcileReport, BoardResolveSettingsRequest, BoardResolvedSettings,
    BoardReviewProfile, BoardSendUpdateReceipt, BoardSendUpdateRequest, BoardSetStatusRequest,
    BoardStartBatchReceipt, BoardStartBatchRequest, BoardStartOutcome, BoardStartReceipt,
    BoardStartRequest, BoardStatusReportKind, BoardStatusReportRecord, BoardStopBarrier,
    BoardStopState, BoardWorkerPlacement, CanonicalChatIdentity, CrossTaskSendMode,
    FirstPromptDelivery, LaunchSnapshot, OriginalContainer, ReasoningLevel, ResolvedExecution,
    ReviewRunState, ReviewVerdict, TaskLinkRole, WorkItem, WorkItemEventKind, WorkItemTaskLink,
};
use luna_sync::{BoardCommit, BoardItemWrite, BoardRankPlacement, BoardRowWrite};

mod delivery;
mod drive;
mod instructions;
mod observe;
mod settings;

use instructions::{ContinuationContext, InstructionInput, build_instruction, instruction_hash};
use observe::{attention, barrier_open};
use settings::DestinationPlan;

/// Explicit-start concurrency bounds (no user setting exists yet): live
/// board executions per project, per host and per provider. Checkout
/// ownership (one live writer per working directory) applies on top.
pub(crate) const BOARD_PROJECT_CAPACITY: u32 = 4;
pub(crate) const BOARD_HOST_CAPACITY: u32 = 8;
pub(crate) const BOARD_PROVIDER_CAPACITY: u32 = 4;
/// A run that outlives Stop by this much is shown as Unknown (never as
/// stopped) until its end is observed.
const STOP_UNKNOWN_AFTER_MS: i64 = 60_000;
/// A pending send whose run is no longer live after this long could not be
/// proven accepted.
const DELIVERY_GRACE_MS: i64 = 5_000;
const TICK_INTERVAL: Duration = Duration::from_secs(2);
/// Runtime status pings to Board watchers are coalesced over this window
/// and bounded to this many changed chats per ping.
const RUNTIME_PING_COALESCE: Duration = Duration::from_millis(150);
const RUNTIME_PING_CHATS_MAX: usize = 256;
const RECONCILE_BATCH: usize = 1_000;
const STATUS_SUMMARY_MAX_CHARS: usize = 2_000;
const STATUS_EVIDENCE_MAX: usize = 16;

/// The engine-internal lifecycle actor.
pub(crate) fn lifecycle_caller() -> BoardCaller {
    BoardCaller::Engine {
        component: "lifecycle",
    }
}

pub(crate) fn storage(error: impl std::fmt::Display) -> BoardError {
    BoardError::Storage {
        message: error.to_string(),
    }
}

/// A lifecycle commit that touches the ticket without changing its fields
/// (the revision still advances with the event).
pub(crate) fn no_change(_: &mut WorkItem) {}

fn op_hash<T: Serialize + Clone>(
    operation: BoardOperation,
    request: &T,
    clear: impl FnOnce(&mut T),
) -> BoardResult<String> {
    let mut payload = request.clone();
    clear(&mut payload);
    board_digest(&(operation.as_str(), payload))
}

/// One ticket's lifecycle state as read under its gate.
#[derive(Debug, Clone)]
pub(crate) struct Ticket {
    pub item: WorkItem,
    pub intent: BoardExecutionIntent,
    /// Stored intent row version (`None`: never persisted).
    pub version: Option<u64>,
}

/// One lifecycle mutation through the store's atomic primitive.
pub(crate) struct Commit<'a> {
    pub request_id: String,
    pub payload_hash: String,
    pub operation: BoardOperation,
    pub actor: BoardActor,
    /// Change the ticket record (its revision advances).
    pub touch: Option<&'a (dyn Fn(&mut WorkItem) + Sync)>,
    pub placement: Option<BoardRankPlacement>,
    /// Write `ticket.intent` (CAS on its stored version).
    pub intent: bool,
    pub rows: Vec<BoardRowWrite>,
    pub event: Option<WorkItemEventKind>,
    pub task_id: Option<String>,
}

/// The caller's request when an engine step commits on its behalf.
pub(crate) struct OuterRequest {
    pub request_id: String,
    pub payload_hash: String,
    pub operation: BoardOperation,
    pub actor: BoardActor,
}

/// An accepted Start/Continue/Retry, before it is driven.
struct ReadyRequest {
    request_id: String,
    payload_hash: String,
    operation: BoardOperation,
    placement: Option<BoardRankPlacement>,
    destination: BoardDestination,
    context: BoardContextMode,
    execution: Option<BoardExecutionProfile>,
    follow_up: BoardFollowUp,
    caller_task_id: Option<String>,
    retry: bool,
    adopt_latest_brief: bool,
}

impl EngineRpc {
    // ---- shared helpers ----

    /// Serializes lifecycle work on one ticket (other tickets proceed). A
    /// distinct key from the plain ticket gate, so chat-model/review code
    /// called from here may take that one without deadlocking.
    pub(crate) async fn board_lifecycle_gate(
        &self,
        work_item_id: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        self.board_hub()
            .gate(&format!("lifecycle:{work_item_id}"))
            .await
    }

    pub(crate) fn board_ticket(&self, id: &str) -> BoardResult<Ticket> {
        let item = self.board_load(id)?;
        let stored = self.store.board_intent(&item.id).map_err(storage)?;
        let version = stored.as_ref().map(|intent| intent.version);
        let intent = match stored {
            Some(intent) => intent,
            None => {
                let mut intent = BoardExecutionIntent::initial(&item.id, item.created_at_ms);
                // A converted chat already works on the ticket: its first
                // board dispatch is a continuation, never a first start.
                if matches!(item.origin, BoardItemOrigin::Converted { .. })
                    || self.board_worker(&item.id)?.is_some()
                {
                    intent.first_prompt = FirstPromptDelivery::NotApplicable;
                }
                intent
            }
        };
        Ok(Ticket {
            item,
            intent,
            version,
        })
    }

    pub(crate) fn board_reload(&self, ticket: &mut Ticket) -> BoardResult<()> {
        *ticket = self.board_ticket(&ticket.item.id)?;
        Ok(())
    }

    fn board_lifecycle_receipt(
        &self,
        actor: BoardActor,
        operation: BoardOperation,
        item: &WorkItem,
        task_id: Option<String>,
    ) -> BoardMutationReceipt {
        BoardMutationReceipt {
            request_id: String::new(),
            payload_hash: String::new(),
            operation,
            work_item_id: item.id.clone(),
            key: item.key.clone(),
            revision: item.revision,
            column: item.column,
            archived: item.archived,
            item: item.clone(),
            event_seq: None,
            actor,
            committed_at_ms: 0,
            task_id,
            navigation: self.board_default_navigation(item),
        }
    }

    /// Deterministic engine request id and payload hash for one lifecycle
    /// step: repeating the step replays its stored receipt.
    pub(crate) fn board_engine_request(
        &self,
        parts: &impl Serialize,
    ) -> BoardResult<(String, String)> {
        let digest = board_digest(parts)?;
        let request_id =
            lifecycle_caller().normalized_request_id(&format!("lc.{}", &digest[..40]))?;
        Ok((request_id, format!("lifecycle:{digest}")))
    }

    /// Persist an intent-only change (an observation of runtime evidence:
    /// no journal event, no receipt).
    pub(crate) fn board_intent_only(&self, ticket: &mut Ticket) -> BoardResult<()> {
        ticket.intent.updated_at_ms = crate::now_ms();
        let result = self.board_commit(BoardCommit {
            request_id: format!("lc-aux-{}", crate::new_id()),
            payload_hash: String::new(),
            committed_at_ms: crate::now_ms(),
            item: BoardItemWrite::None,
            rows: vec![BoardRowWrite::Intent {
                expected_version: ticket.version,
                intent: Box::new(ticket.intent.clone()),
            }],
            event: None,
            receipt: None,
        });
        let reload = self.board_reload(ticket);
        result?;
        reload
    }

    /// Commit a lifecycle mutation (ticket CAS + intent CAS + rows + one
    /// event). A concurrent brief edit only re-reads the ticket and retries;
    /// a concurrent intent writer fails the step (`Busy`), which the caller
    /// repeats from the persisted state.
    pub(crate) fn board_lifecycle_commit(
        &self,
        ticket: &mut Ticket,
        commit: Commit<'_>,
    ) -> BoardResult<Option<BoardMutationReceipt>> {
        let mut attempt = 0;
        loop {
            let now = crate::now_ms();
            let (item, receipt) = match commit.touch {
                Some(touch) => {
                    let mut item = ticket.item.clone();
                    touch(&mut item);
                    item.revision += 1;
                    item.updated_at_ms = now;
                    item.updated_by = commit.actor.clone();
                    let receipt = self.board_lifecycle_receipt(
                        commit.actor.clone(),
                        commit.operation,
                        &item,
                        commit.task_id.clone(),
                    );
                    (
                        BoardItemWrite::Replace {
                            expected_revision: ticket.item.revision,
                            item: Box::new(item),
                            placement: commit.placement.clone(),
                        },
                        Some(receipt),
                    )
                }
                None => (BoardItemWrite::None, None),
            };
            let mut rows = commit.rows.clone();
            if commit.intent {
                let mut intent = ticket.intent.clone();
                intent.updated_at_ms = now;
                rows.push(BoardRowWrite::Intent {
                    expected_version: ticket.version,
                    intent: Box::new(intent),
                });
            }
            let result = self.board_commit(BoardCommit {
                request_id: commit.request_id.clone(),
                payload_hash: commit.payload_hash.clone(),
                committed_at_ms: now,
                item,
                rows,
                event: commit
                    .event
                    .clone()
                    .map(|event| (event.clone(), event.is_activity())),
                receipt,
            });
            match result {
                Ok(receipt) => {
                    self.board_reload(ticket)?;
                    return Ok(receipt);
                }
                Err(BoardError::Busy { reason })
                    if reason == BOARD_CONCURRENT_CHANGE
                        && commit.touch.is_some()
                        && attempt < 2 =>
                {
                    attempt += 1;
                    ticket.item = self.board_load(&ticket.item.id)?;
                }
                Err(error) => {
                    let _ = self.board_reload(ticket);
                    return Err(error);
                }
            }
        }
    }

    fn board_reply_key(kind: &str, request_id: &str) -> String {
        format!("board-reply:{kind}:{request_id}")
    }

    /// Keep a composite lifecycle reply so a retry with the same request id
    /// returns the identical receipt.
    fn board_save_reply<T: Serialize>(
        &self,
        kind: &str,
        request_id: &str,
        value: &T,
    ) -> BoardResult<()> {
        let bytes = serde_json::to_vec(value).map_err(storage)?;
        let key = Self::board_reply_key(kind, request_id);
        self.board_save_snapshot(&key, &bytes)
    }

    fn board_saved_reply<T: serde::de::DeserializeOwned>(
        &self,
        kind: &str,
        request_id: &str,
    ) -> BoardResult<Option<T>> {
        self.store
            .load_snapshot(&Self::board_reply_key(kind, request_id))
            .map_err(storage)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(storage))
            .transpose()
    }

    fn board_placement(
        &self,
        after: Option<&str>,
        before: Option<&str>,
    ) -> BoardResult<Option<BoardRankPlacement>> {
        Ok(match (after, before) {
            (Some(after), _) => Some(BoardRankPlacement::After(self.board_load(after)?.id)),
            (None, Some(before)) => Some(BoardRankPlacement::Before(self.board_load(before)?.id)),
            (None, None) => None,
        })
    }

    fn board_stale(&self, ticket: &Ticket, expected: u64) -> BoardResult<BoardError> {
        Ok(BoardError::StaleRevision {
            work_item_id: ticket.item.id.clone(),
            expected,
            current: Box::new(self.board_view(ticket.item.clone())?),
        })
    }

    /// Whether an explicit destination names the existing worker's frozen
    /// container (a worker is never moved or duplicated).
    fn board_destination_matches(
        &self,
        caller: &BoardCaller,
        requested: &BoardDestination,
        link: &WorkItemTaskLink,
    ) -> bool {
        match (requested, &link.original_container) {
            (BoardDestination::Auto, _) => true,
            (BoardDestination::NewMain, Some(OriginalContainer::MainChat)) => true,
            (
                BoardDestination::SideOf { main_task_id },
                Some(OriginalContainer::SideOf { main_task_id: main }),
            ) => main_task_id == main,
            (
                BoardDestination::SiblingOf { side_task_id },
                Some(OriginalContainer::SideOf { main_task_id: main }),
            ) => self
                .board_chat_identity(
                    caller,
                    BoardChatIdentityRequest {
                        task_id: side_task_id.clone(),
                    },
                )
                .is_ok_and(|identity| identity.main_parent_id.as_deref() == Some(main.as_str())),
            _ => false,
        }
    }

    /// Accept a request to run (Backlog/Done → Ready, or Continue/Retry in
    /// place): validate the destination and settings for a new worker
    /// first, so an unavailable model or role leaves nothing behind.
    async fn board_accept_ready(
        &self,
        caller: &BoardCaller,
        ticket: &mut Ticket,
        request: ReadyRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let worker = self.board_worker(&ticket.item.id)?;
        let acting = self.board_acting_chat(caller, request.caller_task_id.as_deref())?;
        let mut pending = BoardPendingStart {
            request_id: request.request_id.clone(),
            actor: caller.actor(),
            caller_task_id: acting.clone(),
            destination: request.destination.clone(),
            resolved_destination: BoardDestination::NewMain,
            context: request.context.clone(),
            execution: request.execution.clone(),
            follow_up: request.follow_up,
            resolved: None,
            unresolved: Vec::new(),
            retry: request.retry,
            adopt_latest_brief: request.adopt_latest_brief,
            accepted_at_ms: crate::now_ms(),
        };
        match &worker {
            None => {
                let plan: DestinationPlan = self.board_plan_destination(
                    caller,
                    &ticket.item,
                    &request.destination,
                    acting.as_deref(),
                )?;
                let resolution = self
                    .board_resolve_new_worker(
                        caller,
                        &ticket.item,
                        request.execution.as_ref(),
                        &plan,
                    )
                    .await?;
                pending.resolved_destination = plan.destination;
                pending.resolved = Some(resolution.resolved);
                pending.unresolved = resolution.unresolved;
            }
            Some(link) => {
                if request
                    .execution
                    .as_ref()
                    .is_some_and(|execution| !execution.is_inherited())
                {
                    return Err(BoardError::invalid(
                        "execution",
                        "an existing working chat keeps its own configuration; change it in that chat",
                    ));
                }
                if !self.board_destination_matches(caller, &request.destination, link) {
                    return Err(BoardError::InvalidDestination {
                        reason:
                            "This ticket already has a working chat; it is continued where it is."
                                .into(),
                        canonical: Some(self.board_default_navigation(&ticket.item)),
                    });
                }
                pending.resolved_destination = match &link.original_container {
                    Some(OriginalContainer::SideOf { main_task_id }) => BoardDestination::SideOf {
                        main_task_id: main_task_id.clone(),
                    },
                    _ => BoardDestination::NewMain,
                };
            }
        }
        let mut rows = Vec::new();
        if let Some(recipient) = (request.follow_up != BoardFollowUp::None)
            .then(|| self.board_result_recipient(&pending))
            .flatten()
            && let Some(row) = self.board_subscription_row(
                &ticket.item,
                &recipient,
                request.follow_up,
                &caller.actor(),
            )?
        {
            rows.push(row);
        }
        let from = ticket.item.column;
        let intent = &mut ticket.intent;
        intent.intent_revision += 1;
        intent.desired = BoardColumn::Ready;
        intent.last_request_id = request.request_id.clone();
        intent.pending_start = Some(pending);
        intent.launch_failure = None;
        intent.queue = None;
        intent.needs_reconcile = true;
        let intent_revision = intent.intent_revision;
        let event = (from != BoardColumn::Ready).then_some(WorkItemEventKind::Moved {
            from,
            to: BoardColumn::Ready,
            intent_revision,
        });
        let to_ready = |item: &mut WorkItem| item.column = BoardColumn::Ready;
        let placement = (from != BoardColumn::Ready)
            .then(|| request.placement.clone().unwrap_or(BoardRankPlacement::Top));
        self.board_lifecycle_commit(
            ticket,
            Commit {
                request_id: request.request_id,
                payload_hash: request.payload_hash,
                operation: request.operation,
                actor: caller.actor(),
                touch: Some(&to_ready),
                placement,
                intent: true,
                rows,
                event,
                task_id: worker.map(|link| link.task_id),
            },
        )?
        .ok_or_else(|| storage("the transition returned no receipt"))
    }

    /// Accept a hold (Ready/Done → Backlog): cancel unsent work, Stop the
    /// exact generation when it is active. Done → Backlog reopens as held.
    fn board_accept_hold(
        &self,
        caller: &BoardCaller,
        ticket: &mut Ticket,
        request_id: &str,
        payload_hash: &str,
        operation: BoardOperation,
        placement: Option<BoardRankPlacement>,
    ) -> BoardResult<BoardMutationReceipt> {
        let from = ticket.item.column;
        let intent = &mut ticket.intent;
        intent.intent_revision += 1;
        intent.desired = BoardColumn::Backlog;
        intent.last_request_id = request_id.into();
        intent.pending_start = None;
        intent.launch_failure = None;
        intent.needs_reconcile = true;
        let event = if from == BoardColumn::Done {
            WorkItemEventKind::Reopened { from }
        } else {
            WorkItemEventKind::Moved {
                from,
                to: BoardColumn::Backlog,
                intent_revision: intent.intent_revision,
            }
        };
        let to_backlog = |item: &mut WorkItem| item.column = BoardColumn::Backlog;
        let task_id = intent.working_task_id.clone();
        self.board_lifecycle_commit(
            ticket,
            Commit {
                request_id: request_id.into(),
                payload_hash: payload_hash.into(),
                operation,
                actor: caller.actor(),
                touch: Some(&to_backlog),
                placement: Some(placement.unwrap_or(BoardRankPlacement::Top)),
                intent: true,
                rows: Vec::new(),
                event: Some(event),
                task_id,
            },
        )?
        .ok_or_else(|| storage("the transition returned no receipt"))
    }

    /// Ordering only (Ready → Ready and friends): never an execution
    /// effect, never a revision or intent change.
    fn board_rank_only(
        &self,
        ticket: &Ticket,
        request: OuterRequest,
        placement: Option<BoardRankPlacement>,
        rows: Vec<BoardRowWrite>,
    ) -> BoardResult<BoardMutationReceipt> {
        let now = crate::now_ms();
        let receipt = self.board_lifecycle_receipt(
            request.actor.clone(),
            request.operation,
            &ticket.item,
            None,
        );
        let (item, event) = match placement {
            Some(placement) => (
                BoardItemWrite::Rank {
                    work_item_id: ticket.item.id.clone(),
                    placement,
                    at_ms: now,
                    actor: request.actor.clone(),
                },
                Some((
                    WorkItemEventKind::Reordered {
                        column: ticket.item.column,
                        rank: String::new(),
                    },
                    false,
                )),
            ),
            None => (
                BoardItemWrite::Replace {
                    expected_revision: ticket.item.revision,
                    item: Box::new(ticket.item.clone()),
                    placement: None,
                },
                None,
            ),
        };
        self.board_commit(BoardCommit {
            request_id: request.request_id,
            payload_hash: request.payload_hash,
            committed_at_ms: now,
            item,
            rows,
            event,
            receipt: Some(receipt),
        })
        .map_err(|error| match error {
            BoardError::Busy { reason } if reason != BOARD_CONCURRENT_CHANGE => {
                BoardError::invalid("afterId", reason)
            }
            other => other,
        })?
        .ok_or_else(|| storage("the reorder returned no receipt"))
    }

    // ---- moves ----

    /// `board_move`: the one transition for drag, menu, keyboard and agents.
    /// Persist the desired intent (new `intent_revision`) before any effect;
    /// Backlog → Ready dispatches FirstStart (never dispatched) or
    /// Continuation (same worker); Ready → Backlog cancels an unsent launch or
    /// invokes the shared Stop for the exact admitted generation (including
    /// owned reviewers); Ready → Ready only re-ranks. Repeats with the same
    /// request id return the stored receipt; rapid moves coalesce to the
    /// latest intent while the stop barrier holds.
    pub(crate) async fn board_move(
        &self,
        caller: &BoardCaller,
        request: BoardMoveRequest,
    ) -> BoardResult<BoardMoveReceipt> {
        Box::pin(self.board_move_boxed(caller, request)).await
    }

    async fn board_move_boxed(
        &self,
        caller: &BoardCaller,
        request: BoardMoveRequest,
    ) -> BoardResult<BoardMoveReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let hash = op_hash(BoardOperation::Move, &request, |r| r.request_id.clear())?;
        let item = self.board_item_for(caller, &request.work_item_id, BoardAccess::Write)?;
        let _gate = self.board_lifecycle_gate(&item.id).await;
        if let Some(mutation) = self.board_replay(&request_id, &hash)? {
            if let Some(saved) = self.board_saved_reply("move", &request_id)? {
                return Ok(saved);
            }
            // Accepted earlier, effect unfinished: resume from the intent.
            let effect = Box::pin(self.board_drive(&mutation.work_item_id, false))
                .await
                .unwrap_or(BoardMoveEffect::Reconciling);
            return self.board_finish_move(&request_id, mutation, effect);
        }
        if request.to == BoardColumn::Done {
            return Err(BoardError::InvalidTransition {
                from: item.column,
                to: BoardColumn::Done,
                reason: "Done is engine-owned: a ticket completes when its current checks and review pass."
                    .into(),
            });
        }
        if item.archived {
            return Err(BoardError::InvalidTransition {
                from: item.column,
                to: request.to,
                reason: "Unarchive the ticket before moving it.".into(),
            });
        }
        let mut ticket = self.board_ticket(&item.id)?;
        if let Some(expected) = request.expected_revision
            && expected != ticket.item.revision
        {
            return Err(self.board_stale(&ticket, expected)?);
        }
        let from = ticket.item.column;
        let placement =
            self.board_placement(request.after_id.as_deref(), request.before_id.as_deref())?;
        if from == request.to {
            let effect = if placement.is_some() {
                BoardMoveEffect::None
            } else {
                BoardMoveEffect::Coalesced
            };
            let mutation = self.board_rank_only(
                &ticket,
                OuterRequest {
                    request_id: request_id.clone(),
                    payload_hash: hash.clone(),
                    operation: BoardOperation::Move,
                    actor: caller.actor(),
                },
                placement,
                Vec::new(),
            )?;
            return self.board_finish_move(&request_id, mutation, effect);
        }
        let mutation = if request.to == BoardColumn::Ready {
            self.board_accept_ready(
                caller,
                &mut ticket,
                ReadyRequest {
                    request_id: request_id.clone(),
                    payload_hash: hash,
                    operation: BoardOperation::Move,
                    placement,
                    destination: request.destination.clone(),
                    context: request.context.clone(),
                    execution: request.execution.clone(),
                    follow_up: request.follow_up,
                    caller_task_id: None,
                    retry: false,
                    adopt_latest_brief: false,
                },
            )
            .await?
        } else {
            self.board_accept_hold(
                caller,
                &mut ticket,
                &request_id,
                &hash,
                BoardOperation::Move,
                placement,
            )?
        };
        let effect = match Box::pin(self.board_drive(&ticket.item.id, false)).await {
            Ok(effect) => effect,
            Err(error) => {
                // The move is accepted; the failure is on the ticket as
                // Needs attention and is not retried automatically.
                tracing::info!(%error, work_item_id = %ticket.item.id, "board move effect failed");
                BoardMoveEffect::None
            }
        };
        self.board_finish_move(&request_id, mutation, effect)
    }

    fn board_finish_move(
        &self,
        request_id: &str,
        mutation: BoardMutationReceipt,
        effect: BoardMoveEffect,
    ) -> BoardResult<BoardMoveReceipt> {
        let ticket = self.board_ticket(&mutation.work_item_id)?;
        let phase = self
            .board_observation(&ticket.item, Some(&ticket.intent))
            .phase;
        let receipt = BoardMoveReceipt {
            mutation,
            intent: ticket.intent,
            effect,
            phase,
        };
        self.board_save_reply("move", request_id, &receipt)?;
        Ok(receipt)
    }

    // ---- start ----

    /// `board_start`: explicit Start with destination/context/overrides
    /// (equivalent to Move to Ready). Creates the worker through the
    /// chat-identity slice exactly once, freezes a `LaunchSnapshot` before
    /// queueing, then sends the first instruction once; an existing worker is
    /// continued in place (never moved, never re-sent the brief).
    pub(crate) async fn board_start(
        &self,
        caller: &BoardCaller,
        request: BoardStartRequest,
    ) -> BoardResult<BoardStartReceipt> {
        Box::pin(self.board_start_boxed(caller, request)).await
    }

    async fn board_start_boxed(
        &self,
        caller: &BoardCaller,
        request: BoardStartRequest,
    ) -> BoardResult<BoardStartReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let hash = op_hash(BoardOperation::Start, &request, |r| r.request_id.clear())?;
        let item = self.board_item_for(caller, &request.work_item_id, BoardAccess::Write)?;
        let _gate = self.board_lifecycle_gate(&item.id).await;
        if let Some(mutation) = self.board_replay(&request_id, &hash)? {
            if let Some(saved) = self.board_saved_reply("start", &request_id)? {
                return Ok(saved);
            }
            let effect = Box::pin(self.board_drive(&mutation.work_item_id, false)).await;
            return self.board_finish_start(&request_id, mutation, effect);
        }
        if item.archived {
            return Err(BoardError::InvalidTransition {
                from: item.column,
                to: BoardColumn::Ready,
                reason: "Unarchive the ticket before starting it.".into(),
            });
        }
        let mut ticket = self.board_ticket(&item.id)?;
        if let Some(expected) = request.expected_revision
            && expected != ticket.item.revision
        {
            return Err(self.board_stale(&ticket, expected)?);
        }
        let worker = self.board_worker(&ticket.item.id)?;
        let active = worker
            .as_ref()
            .is_some_and(|link| self.board_chat_live(&link.task_id).1)
            || matches!(
                ticket.intent.launch_state,
                BoardLaunchState::Pending | BoardLaunchState::Unsent
            );
        let waiting = ticket.intent.served_intent_revision < ticket.intent.intent_revision
            && !matches!(
                ticket.intent.queue,
                Some(BoardAdmissionReason::Dependency { .. })
            );
        if ticket.item.column == BoardColumn::Ready && (active || waiting) {
            // Running or already accepted: reuse in place, never a second
            // launch. A requested report still subscribes.
            if let Some(link) = &worker
                && !self.board_destination_matches(caller, &request.destination, link)
            {
                return Err(BoardError::InvalidDestination {
                    reason: "This ticket already has a working chat; it is continued where it is."
                        .into(),
                    canonical: Some(self.board_default_navigation(&ticket.item)),
                });
            }
            let acting = self.board_acting_chat(caller, request.caller_task_id.as_deref())?;
            let recipient = match caller {
                BoardCaller::Agent { task_id, .. } => Some(task_id.clone()),
                _ => acting,
            };
            let mut rows = Vec::new();
            if let Some(recipient) = recipient
                && let Some(row) = self.board_subscription_row(
                    &ticket.item,
                    &recipient,
                    request.follow_up,
                    &caller.actor(),
                )?
            {
                rows.push(row);
            }
            let mutation = self.board_rank_only(
                &ticket,
                OuterRequest {
                    request_id: request_id.clone(),
                    payload_hash: hash.clone(),
                    operation: BoardOperation::Start,
                    actor: caller.actor(),
                },
                None,
                rows,
            )?;
            let effect = match &ticket.intent.queue {
                Some(reason) if !active => BoardMoveEffect::Queued {
                    reason: reason.clone(),
                },
                _ => BoardMoveEffect::Coalesced,
            };
            return self.board_finish_start(&request_id, mutation, Ok(effect));
        }
        let mutation = self
            .board_accept_ready(
                caller,
                &mut ticket,
                ReadyRequest {
                    request_id: request_id.clone(),
                    payload_hash: hash,
                    operation: BoardOperation::Start,
                    placement: None,
                    destination: request.destination.clone(),
                    context: request.context.clone(),
                    execution: request.execution.clone(),
                    follow_up: request.follow_up,
                    caller_task_id: request.caller_task_id.clone(),
                    retry: false,
                    adopt_latest_brief: false,
                },
            )
            .await?;
        let effect = Box::pin(self.board_drive(&ticket.item.id, false)).await;
        self.board_finish_start(&request_id, mutation, effect)
    }

    fn board_finish_start(
        &self,
        request_id: &str,
        mutation: BoardMutationReceipt,
        effect: BoardResult<BoardMoveEffect>,
    ) -> BoardResult<BoardStartReceipt> {
        let ticket = self.board_ticket(&mutation.work_item_id)?;
        let worker = self.board_worker(&ticket.item.id)?;
        let failure = ticket.intent.launch_failure.clone();
        let outcome = match &effect {
            Err(error) => BoardStartOutcome::Failed {
                error: Box::new(error.clone()),
            },
            Ok(BoardMoveEffect::FirstStart { .. }) => BoardStartOutcome::Started,
            Ok(BoardMoveEffect::Queued { reason }) => BoardStartOutcome::Queued {
                reason: reason.clone(),
            },
            Ok(BoardMoveEffect::Reconciling) => BoardStartOutcome::Reconciling,
            Ok(_) if worker.is_none() && failure.is_some() => BoardStartOutcome::Failed {
                error: Box::new(BoardError::Unavailable {
                    reason: failure.map(|failure| failure.detail).unwrap_or_default(),
                }),
            },
            Ok(_) => BoardStartOutcome::Reused,
        };
        let launch = match &effect {
            Ok(
                BoardMoveEffect::FirstStart { launch_snapshot_id }
                | BoardMoveEffect::Continue { launch_snapshot_id },
            ) => self
                .store
                .board_launches(&ticket.item.id, 64)
                .map_err(storage)?
                .into_iter()
                .find(|launch| &launch.id == launch_snapshot_id),
            _ => None,
        };
        let worker = worker.map(|link| {
            let (role, main_parent_id, container) = match link.original_container.clone() {
                Some(OriginalContainer::SideOf { main_task_id }) => (
                    BoardChatRole::Side,
                    Some(main_task_id.clone()),
                    OriginalContainer::SideOf { main_task_id },
                ),
                _ => (BoardChatRole::Main, None, OriginalContainer::MainChat),
            };
            BoardWorkerPlacement {
                task_id: link.task_id,
                role,
                main_parent_id,
                container,
            }
        });
        let receipt = BoardStartReceipt {
            navigation: self.board_default_navigation(&ticket.item),
            mutation,
            effect: effect.unwrap_or(BoardMoveEffect::None),
            outcome,
            launch,
            worker,
        };
        self.board_save_reply("start", request_id, &receipt)?;
        Ok(receipt)
    }

    /// Bounded multi-ticket start (≤ `BOARD_BATCH_START_MAX`): each ticket
    /// is an independently idempotent start keyed `{request_id}:{item}` with
    /// a per-ticket Started/Reused/Queued/Failed result and admission reason.
    pub(crate) async fn board_start_batch(
        &self,
        caller: &BoardCaller,
        request: BoardStartBatchRequest,
    ) -> BoardResult<BoardStartBatchReceipt> {
        Box::pin(self.board_start_batch_boxed(caller, request)).await
    }

    async fn board_start_batch_boxed(
        &self,
        caller: &BoardCaller,
        request: BoardStartBatchRequest,
    ) -> BoardResult<BoardStartBatchReceipt> {
        caller.normalized_request_id(&request.request_id)?;
        if request.work_item_ids.is_empty() || request.work_item_ids.len() > BOARD_BATCH_START_MAX {
            return Err(BoardError::invalid(
                "workItemIds",
                format!("name between 1 and {BOARD_BATCH_START_MAX} tickets"),
            ));
        }
        let mut seen = HashSet::new();
        let mut results = Vec::with_capacity(request.work_item_ids.len());
        for id in &request.work_item_ids {
            let resolved = match self.board_item_for(caller, id, BoardAccess::Write) {
                Ok(item) => item.id,
                Err(error) => {
                    results.push(BoardBatchItemResult {
                        work_item_id: id.clone(),
                        outcome: BoardStartOutcome::Failed {
                            error: Box::new(error),
                        },
                        receipt: None,
                    });
                    continue;
                }
            };
            if !seen.insert(resolved.clone()) {
                return Err(BoardError::invalid(
                    "workItemIds",
                    "a ticket is listed twice",
                ));
            }
            let key = if request.request_id.len() + 1 + resolved.len()
                <= luna_proto::BOARD_REQUEST_ID_MAX_BYTES
            {
                format!("{}:{resolved}", request.request_id)
            } else {
                format!("b{}:{resolved}", &board_digest(&request.request_id)?[..24])
            };
            let result = Box::pin(self.board_start(
                caller,
                BoardStartRequest {
                    request_id: key,
                    work_item_id: resolved.clone(),
                    expected_revision: None,
                    destination: request.destination.clone(),
                    context: request.context.clone(),
                    execution: request.execution.clone(),
                    follow_up: request.follow_up,
                    caller_task_id: request.caller_task_id.clone(),
                },
            ))
            .await;
            results.push(match result {
                Ok(receipt) => BoardBatchItemResult {
                    work_item_id: resolved,
                    outcome: receipt.outcome.clone(),
                    receipt: Some(receipt),
                },
                Err(error) => BoardBatchItemResult {
                    work_item_id: resolved,
                    outcome: BoardStartOutcome::Failed {
                        error: Box::new(error),
                    },
                    receipt: None,
                },
            });
        }
        Ok(BoardStartBatchReceipt {
            request_id: request.request_id,
            results,
        })
    }

    // ---- controls ----

    /// Stop / Continue / Retry / CancelQueued / Accept with durable receipts.
    /// Stop uses the same command and scope as the chat's Stop button;
    /// unknown termination stays `StopUnknown`, never "stopped".
    pub(crate) async fn board_control(
        &self,
        caller: &BoardCaller,
        request: BoardControlRequest,
    ) -> BoardResult<BoardControlReceipt> {
        Box::pin(self.board_control_boxed(caller, request)).await
    }

    async fn board_control_boxed(
        &self,
        caller: &BoardCaller,
        request: BoardControlRequest,
    ) -> BoardResult<BoardControlReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let hash = op_hash(BoardOperation::Control, &request, |r| r.request_id.clear())?;
        let item = self.board_item_for(caller, &request.work_item_id, BoardAccess::Write)?;
        let _gate = self.board_lifecycle_gate(&item.id).await;
        if let Some(mutation) = self.board_replay(&request_id, &hash)? {
            if let Some(saved) = self.board_saved_reply("control", &request_id)? {
                return Ok(saved);
            }
            let effect = Box::pin(self.board_drive(&mutation.work_item_id, false))
                .await
                .unwrap_or(BoardMoveEffect::Reconciling);
            return self.board_finish_control(&request_id, request.action, mutation, effect);
        }
        let mut ticket = self.board_ticket(&item.id)?;
        if let Some(expected) = request.expected_revision
            && expected != ticket.item.revision
        {
            return Err(self.board_stale(&ticket, expected)?);
        }
        let column = ticket.item.column;
        let worker = self.board_worker(&ticket.item.id)?;
        let (mutation, effect) = match request.action {
            BoardControlAction::Stop => {
                let link = worker.ok_or_else(|| BoardError::InvalidTransition {
                    from: column,
                    to: column,
                    reason: "This ticket has no working chat to stop.".into(),
                })?;
                let generation = ticket.intent.execution_generation;
                let intent = &mut ticket.intent;
                intent.intent_revision += 1;
                intent.last_request_id = request_id.clone();
                intent.served_intent_revision = intent.intent_revision;
                intent.pending_start = None;
                intent.queue = None;
                let receipt = self
                    .board_shared_stop(
                        &mut ticket,
                        &link.task_id,
                        Some(OuterRequest {
                            request_id: request_id.clone(),
                            payload_hash: hash,
                            operation: BoardOperation::Control,
                            actor: caller.actor(),
                        }),
                    )
                    .await?
                    .ok_or_else(|| storage("the stop returned no receipt"))?;
                (
                    receipt,
                    BoardMoveEffect::StopRequested {
                        execution_generation: generation,
                    },
                )
            }
            BoardControlAction::Continue | BoardControlAction::Retry => {
                if column == BoardColumn::Done && request.action == BoardControlAction::Retry {
                    return Err(BoardError::InvalidTransition {
                        from: column,
                        to: BoardColumn::Ready,
                        reason: "Reopen the ticket before retrying it.".into(),
                    });
                }
                let mutation = self
                    .board_accept_ready(
                        caller,
                        &mut ticket,
                        ReadyRequest {
                            request_id: request_id.clone(),
                            payload_hash: hash,
                            operation: BoardOperation::Control,
                            placement: None,
                            destination: BoardDestination::Auto,
                            context: BoardContextMode::Fresh,
                            execution: None,
                            follow_up: BoardFollowUp::None,
                            caller_task_id: None,
                            retry: request.action == BoardControlAction::Retry,
                            adopt_latest_brief: request.adopt_latest_brief,
                        },
                    )
                    .await?;
                let effect = Box::pin(self.board_drive(&ticket.item.id, false))
                    .await
                    .unwrap_or(BoardMoveEffect::None);
                (mutation, effect)
            }
            BoardControlAction::CancelQueued => {
                let queued = column == BoardColumn::Ready
                    && (ticket.intent.queue.is_some()
                        || ticket.intent.served_intent_revision < ticket.intent.intent_revision
                        || ticket.intent.launch_state == BoardLaunchState::Unsent);
                if !queued {
                    return Err(BoardError::InvalidTransition {
                        from: column,
                        to: BoardColumn::Backlog,
                        reason: "Nothing is queued; use Stop or Move to Backlog.".into(),
                    });
                }
                let mutation = self.board_accept_hold(
                    caller,
                    &mut ticket,
                    &request_id,
                    &hash,
                    BoardOperation::Control,
                    None,
                )?;
                let effect = Box::pin(self.board_drive(&ticket.item.id, false))
                    .await
                    .unwrap_or(BoardMoveEffect::None);
                (mutation, effect)
            }
            BoardControlAction::Accept => {
                if ticket.item.completion.mode != BoardCompletionMode::ManualAcceptance {
                    return Err(BoardError::InvalidTransition {
                        from: column,
                        to: BoardColumn::Done,
                        reason: "Done is engine-owned; this ticket completes after its checks and review pass."
                            .into(),
                    });
                }
                let generation = ticket.intent.execution_generation;
                let revision = ticket.intent.intent_revision;
                let receipt = self
                    .board_complete(
                        &mut ticket,
                        generation,
                        revision,
                        Some(OuterRequest {
                            request_id: request_id.clone(),
                            payload_hash: hash,
                            operation: BoardOperation::Control,
                            actor: caller.actor(),
                        }),
                    )?
                    .ok_or_else(|| BoardError::InvalidTransition {
                        from: column,
                        to: BoardColumn::Done,
                        reason: "The current result cannot be accepted while work is active, stopping or unconfirmed."
                            .into(),
                    })?;
                if let Err(error) = self.board_deliver_locked(&ticket.item.id, generation).await {
                    tracing::warn!(%error, "board result delivery deferred");
                }
                (receipt, BoardMoveEffect::None)
            }
        };
        self.board_finish_control(&request_id, request.action, mutation, effect)
    }

    fn board_finish_control(
        &self,
        request_id: &str,
        action: BoardControlAction,
        mutation: BoardMutationReceipt,
        effect: BoardMoveEffect,
    ) -> BoardResult<BoardControlReceipt> {
        let ticket = self.board_ticket(&mutation.work_item_id)?;
        let receipt = BoardControlReceipt {
            mutation,
            action,
            effect,
            intent: ticket.intent,
        };
        self.board_save_reply("control", request_id, &receipt)?;
        Ok(receipt)
    }

    /// Done only when the intent and the execution generation still match
    /// the evidence (no stale callback can complete a held or newer run),
    /// and no owned run is still active or unconfirmed.
    fn board_complete(
        &self,
        ticket: &mut Ticket,
        execution_generation: u64,
        intent_revision: u64,
        outer: Option<OuterRequest>,
    ) -> BoardResult<Option<BoardMutationReceipt>> {
        let (request_id, payload_hash, operation, actor) = match outer {
            Some(outer) => (
                outer.request_id,
                outer.payload_hash,
                outer.operation,
                outer.actor,
            ),
            None => {
                let (request_id, hash) =
                    self.board_engine_request(&("done", &ticket.item.id, execution_generation))?;
                (
                    request_id,
                    hash,
                    BoardOperation::Control,
                    lifecycle_caller().actor(),
                )
            }
        };
        if ticket.item.column == BoardColumn::Done
            && ticket.intent.completed_generation == Some(execution_generation)
        {
            return self.board_replay(&request_id, &payload_hash);
        }
        let intent = &ticket.intent;
        let worker_live = intent
            .working_task_id
            .as_deref()
            .is_some_and(|task| self.board_chat_live(task).1);
        if ticket.item.column != BoardColumn::Ready
            || intent.desired != BoardColumn::Ready
            || execution_generation == 0
            || intent.execution_generation != execution_generation
            || intent.intent_revision != intent_revision
            || intent.stop.as_ref().is_some_and(barrier_open)
            || matches!(
                intent.launch_state,
                BoardLaunchState::Unsent | BoardLaunchState::Pending | BoardLaunchState::Unknown
            )
            || worker_live
        {
            return Ok(None);
        }
        let intent = &mut ticket.intent;
        intent.completed_generation = Some(execution_generation);
        intent.queue = None;
        intent.pending_start = None;
        intent.observed_phase = BoardPhase::Complete;
        intent.needs_reconcile = false;
        let to_done = |item: &mut WorkItem| item.column = BoardColumn::Done;
        let task_id = intent.working_task_id.clone();
        self.board_lifecycle_commit(
            ticket,
            Commit {
                request_id,
                payload_hash,
                operation,
                actor,
                touch: Some(&to_done),
                placement: Some(BoardRankPlacement::Top),
                intent: true,
                rows: Vec::new(),
                event: Some(WorkItemEventKind::Completed {
                    execution_generation,
                }),
                task_id,
            },
        )
    }

    /// For the review slice: move a ticket to Done when the passing result
    /// still belongs to the current generation and intent revision. A stale
    /// or held ticket returns `None` and changes nothing; the outcome is
    /// then delivered once to any requested recipient.
    pub(crate) async fn board_mark_done_if_current(
        &self,
        work_item_id: &str,
        execution_generation: u64,
        intent_revision: u64,
    ) -> BoardResult<Option<BoardMutationReceipt>> {
        let _gate = self.board_lifecycle_gate(work_item_id).await;
        let mut ticket = self.board_ticket(work_item_id)?;
        let receipt =
            self.board_complete(&mut ticket, execution_generation, intent_revision, None)?;
        if receipt.is_some()
            && let Err(error) = self
                .board_deliver_locked(work_item_id, execution_generation)
                .await
        {
            tracing::warn!(%error, work_item_id, "board result delivery deferred");
        }
        Ok(receipt)
    }

    // ---- status ----

    /// Agent/human status report with provenance. The ticket's own working
    /// chat may report without an explicit user request; a stale generation
    /// is rejected; identical repeats are no-ops (no event); Done is never
    /// settable here.
    pub(crate) async fn board_set_status(
        &self,
        caller: &BoardCaller,
        request: BoardSetStatusRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        Box::pin(self.board_set_status_boxed(caller, request)).await
    }

    async fn board_set_status_boxed(
        &self,
        caller: &BoardCaller,
        request: BoardSetStatusRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let hash = op_hash(BoardOperation::SetStatus, &request, |r| {
            r.request_id.clear()
        })?;
        if let Some(receipt) = self.board_replay(&request_id, &hash)? {
            return Ok(receipt);
        }
        let item = self.board_item_for(caller, &request.work_item_id, BoardAccess::Write)?;
        let _gate = self.board_lifecycle_gate(&item.id).await;
        if let Some(receipt) = self.board_replay(&request_id, &hash)? {
            return Ok(receipt);
        }
        let mut ticket = self.board_ticket(&item.id)?;
        let worker = self.board_worker(&ticket.item.id)?;
        let own_worker = match caller {
            BoardCaller::Agent {
                task_id,
                explicit_user_request,
                ..
            } => {
                let own = worker.as_ref().is_some_and(|link| &link.task_id == task_id);
                if !own && !explicit_user_request {
                    return Err(BoardError::NotAuthorized {
                        reason: "Only the ticket's working chat, or a chat the user asked to manage it, can report its status."
                            .into(),
                    });
                }
                own
            }
            BoardCaller::Authoring { .. } => {
                return Err(BoardError::NotAuthorized {
                    reason: "A ticket discussion cannot report execution status.".into(),
                });
            }
            BoardCaller::User { .. } | BoardCaller::Engine { .. } => false,
        };
        let report = &request.report;
        if report.summary.chars().count() > STATUS_SUMMARY_MAX_CHARS
            || report
                .summary
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(BoardError::invalid(
                "report.summary",
                format!(
                    "use at most {STATUS_SUMMARY_MAX_CHARS} characters without control characters"
                ),
            ));
        }
        if report.evidence.len() > STATUS_EVIDENCE_MAX
            || report.evidence.iter().any(|evidence| {
                evidence.label.chars().count() > 200
                    || [&evidence.path, &evidence.url, &evidence.outcome_id]
                        .into_iter()
                        .flatten()
                        .any(|value| value.len() > 2048)
            })
        {
            return Err(BoardError::invalid(
                "report.evidence",
                format!("use at most {STATUS_EVIDENCE_MAX} short evidence references"),
            ));
        }
        let generation = ticket.intent.execution_generation;
        if request
            .execution_generation
            .is_some_and(|claimed| claimed != generation)
            || request
                .expected_revision
                .is_some_and(|expected| expected != ticket.item.revision)
        {
            let expected = request
                .execution_generation
                .or(request.expected_revision)
                .unwrap_or_default();
            return Err(self.board_stale(&ticket, expected)?);
        }
        // A worker whose ticket was held or completed reports for an attempt
        // that is no longer current.
        if own_worker
            && report.kind != BoardStatusReportKind::Note
            && (ticket.item.column != BoardColumn::Ready
                || ticket.intent.desired != BoardColumn::Ready)
        {
            return Err(self.board_stale(&ticket, generation)?);
        }
        let record = BoardStatusReportRecord {
            report: report.clone(),
            actor: caller.actor(),
            execution_generation: generation,
            at_ms: crate::now_ms(),
        };
        let identical = ticket.item.last_report.as_ref().is_some_and(|last| {
            last.report == record.report
                && last.actor == record.actor
                && last.execution_generation == generation
        });
        if identical {
            return self.board_rank_only(
                &ticket,
                OuterRequest {
                    request_id: request_id.clone(),
                    payload_hash: hash.clone(),
                    operation: BoardOperation::SetStatus,
                    actor: caller.actor(),
                },
                None,
                Vec::new(),
            );
        }
        let kind = report.kind;
        let set_report = move |item: &mut WorkItem| item.last_report = Some(record.clone());
        self.board_lifecycle_commit(
            &mut ticket,
            Commit {
                request_id,
                payload_hash: hash,
                operation: BoardOperation::SetStatus,
                actor: caller.actor(),
                touch: Some(&set_report),
                placement: None,
                intent: false,
                rows: Vec::new(),
                event: Some(WorkItemEventKind::StatusReported {
                    report: kind,
                    execution_generation: generation,
                }),
                task_id: worker.map(|link| link.task_id),
            },
        )?
        .ok_or_else(|| storage("the status report returned no receipt"))
    }

    // ---- send update ----

    /// "Send update to chat": deliver the brief delta since the last sent
    /// revision through the ordinary queue/steer path, recorded as a
    /// `BriefUpdate` launch snapshot. Never edits earlier messages.
    pub(crate) async fn board_send_update(
        &self,
        caller: &BoardCaller,
        request: BoardSendUpdateRequest,
    ) -> BoardResult<BoardSendUpdateReceipt> {
        Box::pin(self.board_send_update_boxed(caller, request)).await
    }

    async fn board_send_update_boxed(
        &self,
        caller: &BoardCaller,
        request: BoardSendUpdateRequest,
    ) -> BoardResult<BoardSendUpdateReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let hash = op_hash(BoardOperation::SendUpdate, &request, |r| {
            r.request_id.clear()
        })?;
        let item = self.board_item_for(caller, &request.work_item_id, BoardAccess::Write)?;
        match caller {
            BoardCaller::Agent {
                explicit_user_request: false,
                ..
            } => {
                return Err(BoardError::NotAuthorized {
                    reason: "Sending a brief update to the working chat needs the user's request."
                        .into(),
                });
            }
            BoardCaller::Authoring { .. } => {
                return Err(BoardError::NotAuthorized {
                    reason: "A ticket discussion cannot message the working chat.".into(),
                });
            }
            _ => {}
        }
        let _gate = self.board_lifecycle_gate(&item.id).await;
        let digest = board_digest(&("briefUpdate", &item.id, &request_id))?;
        let launch_request_id = format!("bu-{}", &digest[..40]);
        if let Some(mutation) = self.board_replay(&request_id, &hash)? {
            if let Some(saved) = self.board_saved_reply("sendUpdate", &request_id)? {
                return Ok(saved);
            }
            // Committed but not confirmed sent: resend the frozen snapshot
            // with its original command id (never a second message).
            let launch = self
                .store
                .board_launch_by_request(&launch_request_id)
                .map_err(storage)?
                .ok_or_else(|| storage("the brief update snapshot is missing"))?;
            return self
                .board_deliver_update(&request_id, mutation, launch, request.mode)
                .await;
        }
        let mut ticket = self.board_ticket(&item.id)?;
        let link = self.board_worker(&ticket.item.id)?.ok_or_else(|| {
            BoardError::invalid(
                "workItemId",
                "this ticket has no working chat yet; Start it instead",
            )
        })?;
        let brief_changed_at = ticket
            .item
            .field_revisions
            .title
            .max(ticket.item.field_revisions.description);
        if request.expected_revision < brief_changed_at {
            return Err(self.board_stale(&ticket, request.expected_revision)?);
        }
        let launches = self
            .store
            .board_launches(&ticket.item.id, 64)
            .map_err(storage)?;
        let last_sent = launches
            .iter()
            .map(|launch| launch.brief_revision)
            .max()
            .unwrap_or(0);
        if ticket.item.brief_revision <= last_sent {
            return Err(BoardError::invalid(
                "expectedRevision",
                format!("the working chat already has brief revision {last_sent}"),
            ));
        }
        let item = ticket.item.clone();
        let text = build_instruction(&InstructionInput {
            kind: BoardInstructionKind::BriefUpdate,
            key: &item.key,
            work_item_id: &item.id,
            generation: ticket.intent.execution_generation,
            brief_revision: item.brief_revision,
            title: &item.title,
            description: &item.description,
            completion: &item.completion,
            reviewer: "the configured reviewer",
            context_refs: &item.context_refs,
            continuation: None,
        });
        let main_parent_id = match &link.original_container {
            Some(OriginalContainer::SideOf { main_task_id }) => Some(main_task_id.clone()),
            _ => None,
        };
        let launch = LaunchSnapshot {
            id: format!("ls-{}", &digest[..32]),
            work_item_id: item.id.clone(),
            launch_request_id,
            execution_generation: ticket.intent.execution_generation,
            intent_revision: ticket.intent.intent_revision,
            brief_revision: item.brief_revision,
            kind: BoardInstructionKind::BriefUpdate,
            sent_title: item.title.clone(),
            sent_description: item.description.clone(),
            instruction_hash: instruction_hash(&text),
            instruction_text: text,
            attachments: Vec::new(),
            input_items: Vec::new(),
            context_refs: item.context_refs.clone(),
            issue_context: self.board_issue_launch_context(&item.id)?,
            review_base: self
                .board_resolve_review_base(
                    &item.id,
                    &self.board_worker_settings(&item, &link.task_id)?.cwd,
                )
                .await?,
            requested: item.implementation.clone(),
            resolved: self.board_worker_settings(&item, &link.task_id)?,
            destination: match &main_parent_id {
                Some(main) => BoardDestination::SideOf {
                    main_task_id: main.clone(),
                },
                None => BoardDestination::NewMain,
            },
            context_mode: BoardContextMode::Fresh,
            task_id: link.task_id.clone(),
            main_parent_id,
            command_id: format!("board-update-{}", &digest[..32]),
            message_id: format!("board-update-msg-{}", &digest[..32]),
            command_receipt: None,
            follow_up: BoardFollowUp::None,
            result_recipient_task_id: None,
            created_by: caller.actor(),
            created_at_ms: crate::now_ms(),
        };
        let mutation = self
            .board_lifecycle_commit(
                &mut ticket,
                Commit {
                    request_id: request_id.clone(),
                    payload_hash: hash,
                    operation: BoardOperation::SendUpdate,
                    actor: caller.actor(),
                    touch: Some(&no_change),
                    placement: None,
                    intent: false,
                    rows: vec![BoardRowWrite::Launch(Box::new(launch.clone()))],
                    event: Some(WorkItemEventKind::BriefSent {
                        brief_revision: item.brief_revision,
                        task_id: link.task_id.clone(),
                    }),
                    task_id: Some(link.task_id.clone()),
                },
            )?
            .ok_or_else(|| storage("the brief update returned no receipt"))?;
        self.board_deliver_update(&request_id, mutation, launch, request.mode)
            .await
    }

    async fn board_deliver_update(
        &self,
        request_id: &str,
        mutation: BoardMutationReceipt,
        launch: LaunchSnapshot,
        mode: CrossTaskSendMode,
    ) -> BoardResult<BoardSendUpdateReceipt> {
        let behavior = match mode {
            CrossTaskSendMode::Queue => FollowUpBehavior::Queue,
            CrossTaskSendMode::Steer => FollowUpBehavior::Steer,
            CrossTaskSendMode::Auto => {
                if self.board_chat_live(&launch.task_id).1 {
                    FollowUpBehavior::Steer
                } else {
                    FollowUpBehavior::Queue
                }
            }
        };
        let mut command = legacy_task_command(
            launch.task_id.clone(),
            TaskCommandIntent::QueueMessage {
                message_id: launch.message_id.clone(),
                text: launch.instruction_text.clone(),
                attachments: Vec::new(),
                input_items: Vec::new(),
                hold_for_turn_end: true,
                follow_up_behavior: behavior,
                execution: None,
            },
        );
        command.command_id = launch.command_id.clone();
        command.actor_id = "luna-board".into();
        command.client_id = "luna-board-lifecycle".into();
        command.client_session_id = self.cleanup_boot_id.clone();
        let receipt = Box::pin(self.execute_task_command(command, true))
            .await
            .map_err(|error| BoardError::Unavailable {
                reason: format!("The update could not be queued: {error}"),
            })?;
        if receipt.status != CommandReceiptStatus::Applied {
            return Err(BoardError::Unavailable {
                reason: receipt.reason.map_or_else(
                    || {
                        "The working chat refused the update; retry with the same request id."
                            .into()
                    },
                    |reason| reason.message,
                ),
            });
        }
        let launches = self
            .store
            .board_launches(&launch.work_item_id, 64)
            .map_err(storage)?;
        let from_brief_revision = launches
            .iter()
            .filter(|other| other.id != launch.id)
            .map(|other| other.brief_revision)
            .max()
            .unwrap_or(0);
        let reply = BoardSendUpdateReceipt {
            mutation,
            from_brief_revision,
            to_brief_revision: launch.brief_revision,
            launch,
        };
        self.board_save_reply("sendUpdate", request_id, &reply)?;
        Ok(reply)
    }

    // ---- settings ----

    /// A ticket-shaped template for previewing a project's inherited
    /// settings (nothing is stored).
    fn board_template_item(&self, space: &Space) -> WorkItem {
        WorkItem {
            schema_version: luna_proto::BOARD_SCHEMA_VERSION,
            id: format!("wi-preview-{}", space.id),
            project_id: space.id.clone(),
            owner_host_id: space.device_id.clone(),
            key: "PREVIEW".into(),
            number: 0,
            title: String::new(),
            description: String::new(),
            brief_revision: 0,
            column: BoardColumn::Backlog,
            archived: false,
            deleted: false,
            rank: String::new(),
            revision: 0,
            field_revisions: Default::default(),
            implementation: BoardExecutionProfile::default(),
            review: BoardReviewProfile::default(),
            completion: BoardCompletionPolicy::default(),
            labels: Vec::new(),
            priority: None,
            origin: BoardItemOrigin::Manual,
            attachments: Vec::new(),
            input_items: Vec::new(),
            context_refs: Vec::new(),
            last_report: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            created_by: lifecycle_caller().actor(),
            updated_by: lifecycle_caller().actor(),
        }
    }

    /// Real account/host-visible harnesses, models, efforts, access modes and
    /// effective project defaults (read only, bounded). Reuses the model
    /// cache and provider defaults; never invents ids.
    /// One short title for a brief, from the device's title model (the one
    /// that names chats), run restricted outside the repository. Saves
    /// nothing; the user keeps or edits it.
    pub(crate) async fn board_generate_title(
        &self,
        caller: &BoardCaller,
        request: luna_proto::BoardGenerateTitleRequest,
    ) -> BoardResult<luna_proto::BoardGeneratedTitle> {
        self.board_project(caller, &request.project_id, BoardAccess::Read)?;
        let description = request.description.trim();
        if description.chars().count() < 20 {
            return Err(BoardError::invalid(
                "description",
                "describe the issue a little more first",
            ));
        }
        let titles = crate::titles::TitleGenerator::new(
            self.workspace.clone(),
            self.registry.clone(),
            self.repos.clone(),
        );
        let title = titles
            .generate_ticket_title(description, request.harness)
            .await
            .map_err(|reason| BoardError::Unavailable { reason })?;
        Ok(luna_proto::BoardGeneratedTitle { title })
    }

    pub(crate) async fn board_execution_options(
        &self,
        caller: &BoardCaller,
        request: BoardExecutionOptionsRequest,
    ) -> BoardResult<BoardExecutionOptions> {
        let space = self.board_project(caller, &request.project_id, BoardAccess::Read)?;
        let limit = request.limit.unwrap_or(50).clamp(1, 200);
        let query = request
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_lowercase);
        let saved = self
            .provider_defaults
            .as_ref()
            .map(|service| service.get())
            .transpose()
            .map_err(storage)?
            .unwrap_or_default();
        let mut harnesses = Vec::new();
        let mut ids: Vec<HarnessId> = self
            .registry
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.id)
            .collect();
        if ids.is_empty() {
            ids.push(self.board_default_harness());
        }
        for harness in ids {
            if request.harness.is_some_and(|wanted| wanted != harness) {
                continue;
            }
            // Only what the rest of the app offers: a provider hidden or
            // disabled there cannot be chosen, saved or started here.
            if !self.board_provider_available(harness) {
                continue;
            }
            let available = true;
            let models = self
                .model_cache
                .list(harness, None, false)
                .await
                .unwrap_or_default();
            let matching: Vec<_> = models
                .into_iter()
                .filter(|model| {
                    query.as_deref().is_none_or(|query| {
                        model.id.to_lowercase().contains(query)
                            || model.label.to_lowercase().contains(query)
                    })
                })
                .collect();
            let next_offset = (request.offset.saturating_add(limit) < matching.len())
                .then(|| request.offset + limit);
            let default_model = saved
                .providers
                .get(&harness)
                .and_then(|defaults| defaults.model.clone())
                .or_else(|| {
                    space
                        .execution_defaults
                        .provider
                        .as_ref()
                        .filter(|provider| provider.harness == harness)
                        .and_then(|provider| provider.model.clone())
                })
                .or_else(|| matching.first().map(|model| model.id.clone()));
            let accounts = match harness {
                HarnessId::Codex => {
                    let active = self
                        .agent_accounts
                        .resolve_execution(harness, None)
                        .ok()
                        .map(|execution| execution.account_id);
                    self.agent_accounts
                        .codex_account_ids()
                        .into_iter()
                        .map(|id| BoardAccountOption {
                            active: active.as_deref() == Some(id.as_str()),
                            label: id.clone(),
                            account_id: id,
                        })
                        .collect()
                }
                HarnessId::ClaudeCode => self
                    .agent_accounts
                    .resolve_execution(harness, None)
                    .ok()
                    .map(|execution| BoardAccountOption {
                        label: execution.account_id.clone(),
                        account_id: execution.account_id,
                        active: true,
                    })
                    .into_iter()
                    .collect(),
                _ => Vec::new(),
            };
            harnesses.push(BoardHarnessOptions {
                harness,
                available,
                accounts,
                permission_modes: crate::permissions::provider_capabilities_for(
                    harness,
                    default_model.as_deref(),
                )
                .supported_modes,
                default_model,
                models: matching
                    .into_iter()
                    .skip(request.offset)
                    .take(limit)
                    .collect(),
                next_offset,
            });
        }
        let template = self.board_template_item(&space);
        let plan = DestinationPlan {
            destination: BoardDestination::NewMain,
            parent_task_id: None,
            acting_task_id: None,
        };
        let effective_implementation = self
            .board_resolve_new_worker(caller, &template, None, &plan)
            .await
            .ok()
            .map(|resolution| resolution.resolved);
        let effective_review = match &effective_implementation {
            Some(implementation) => self
                .board_resolve_reviewer(&template, implementation)
                .await
                .ok()
                .map(|(review, _)| review),
            None => None,
        };
        Ok(BoardExecutionOptions {
            project_id: space.id.clone(),
            host_device_id: space.device_id.clone(),
            harnesses,
            project_defaults: space.execution_defaults.clone(),
            effective_implementation,
            effective_review,
        })
    }

    /// Preview the settings a Start would freeze (precedence: explicit Start
    /// override → ticket → compatible same-project parent → project defaults
    /// → saved provider defaults → catalog default; reviewer falls back to
    /// the implementation model). No side effect.
    pub(crate) async fn board_resolve_settings(
        &self,
        caller: &BoardCaller,
        request: BoardResolveSettingsRequest,
    ) -> BoardResult<BoardResolvedSettings> {
        let item = self.board_item_for(caller, &request.work_item_id, BoardAccess::Read)?;
        let (implementation, destination, unresolved) = match self.board_worker(&item.id)? {
            Some(link) => {
                if request
                    .execution
                    .as_ref()
                    .is_some_and(|execution| !execution.is_inherited())
                {
                    return Err(BoardError::invalid(
                        "execution",
                        "an existing working chat keeps its own configuration",
                    ));
                }
                let destination = match &link.original_container {
                    Some(OriginalContainer::SideOf { main_task_id }) => BoardDestination::SideOf {
                        main_task_id: main_task_id.clone(),
                    },
                    _ => BoardDestination::NewMain,
                };
                (
                    self.board_worker_settings(&item, &link.task_id)?,
                    destination,
                    Vec::new(),
                )
            }
            None => {
                let acting = self.board_acting_chat(caller, request.caller_task_id.as_deref())?;
                let plan = self.board_plan_destination(
                    caller,
                    &item,
                    &request.destination,
                    acting.as_deref(),
                )?;
                let resolution = self
                    .board_resolve_new_worker(caller, &item, request.execution.as_ref(), &plan)
                    .await?;
                (resolution.resolved, plan.destination, resolution.unresolved)
            }
        };
        let (review, fallback) = self.board_resolve_reviewer(&item, &implementation).await?;
        Ok(BoardResolvedSettings {
            implementation,
            review,
            review_fallback_to_implementation: fallback,
            destination,
            unresolved,
        })
    }

    /// Engine-observed phase/attention/queue for a ticket, from runtime,
    /// task-work and attention evidence. Called by every read model.
    pub(crate) fn board_observe(
        &self,
        item: &WorkItem,
        intent: Option<&BoardExecutionIntent>,
    ) -> BoardResult<BoardObservation> {
        Ok(self.board_observation(item, intent))
    }

    /// Save-time validation of requested settings against the catalog
    /// (unknown model/effort → `UnavailableModel`/`UnsupportedEffort` with
    /// supported choices; deferred settings are allowed).
    pub(crate) fn board_validate_requested_settings(
        &self,
        project: &Space,
        implementation: &BoardExecutionProfile,
        review: &BoardReviewProfile,
    ) -> BoardResult<()> {
        self.board_validate_profiles(project, implementation, review)
    }

    // ---- restart, maintenance, delivery ----

    /// Boot reconciliation: restore intents and query receipts/runtime
    /// ownership before acting (a saved Ready alone never resends a start).
    /// Called once from the board maintenance task.
    pub(crate) async fn board_reconcile_on_boot(&self) -> BoardResult<BoardReconcileReport> {
        let mut report = BoardReconcileReport::default();
        for before in self
            .store
            .board_intents_needing_reconcile(RECONCILE_BATCH)
            .map_err(storage)?
        {
            report.examined += 1;
            let id = before.work_item_id.clone();
            let _gate = self.board_lifecycle_gate(&id).await;
            if let Err(error) = Box::pin(self.board_drive(&id, true)).await {
                tracing::info!(%error, work_item_id = %id, "board reconcile deferred");
            }
            if let Err(error) = self.board_deliver_pending(&id).await {
                tracing::info!(%error, work_item_id = %id, "board result delivery deferred");
            }
            let Some(after) = self.store.board_intent(&id).map_err(storage)? else {
                continue;
            };
            if after.execution_generation > before.execution_generation
                || (before.launch_state == BoardLaunchState::Unsent
                    && after.launch_state != BoardLaunchState::Unsent)
            {
                report.resumed += 1;
            }
            if after.launch_state == BoardLaunchState::Unknown
                || matches!(after.first_prompt, FirstPromptDelivery::Unknown { .. })
                || after
                    .stop
                    .as_ref()
                    .is_some_and(|barrier| barrier.state == BoardStopState::Unknown)
            {
                report.left_unknown.push(id);
            }
        }
        Ok(report)
    }

    /// One lifecycle sweep over Ready and unsettled tickets: settle stops,
    /// reconcile deliveries, admit queued work, deliver owed results. Also
    /// the E2E clock hook; deliberately not an RPC method.
    pub async fn board_lifecycle_tick(&self) -> Result<u64, String> {
        let intents = self
            .store
            .board_intents_needing_reconcile(RECONCILE_BATCH)
            .map_err(|error| error.to_string())?;
        let mut driven = 0;
        for intent in intents {
            let id = intent.work_item_id;
            let _gate = self.board_lifecycle_gate(&id).await;
            let done = self
                .store
                .board_item(&id)
                .ok()
                .flatten()
                .is_some_and(|item| item.column == BoardColumn::Done);
            if !(done && !intent.needs_reconcile) {
                match Box::pin(self.board_drive(&id, false)).await {
                    Ok(_) => driven += 1,
                    Err(error) => {
                        tracing::debug!(%error, work_item_id = %id, "board lifecycle step deferred")
                    }
                }
            }
            if let Err(error) = self.board_deliver_pending(&id).await {
                tracing::debug!(%error, work_item_id = %id, "board result delivery deferred");
            }
        }
        Ok(driven)
    }

    /// Restart reconciliation first, then a bounded periodic sweep.
    pub(crate) fn start_board_lifecycle_maintenance(
        self: &std::sync::Arc<Self>,
        scope: &crate::maintenance::WorkerScope,
    ) {
        self.start_board_runtime_watch(scope);
        let weak = std::sync::Arc::downgrade(self);
        let cancel = scope.cancel.clone();
        scope.spawn(async move {
            if let Some(rpc) = weak.upgrade() {
                match rpc.board_reconcile_on_boot().await {
                    Ok(report) => tracing::info!(
                        examined = report.examined,
                        resumed = report.resumed,
                        unknown = report.left_unknown.len(),
                        "board lifecycle reconciled"
                    ),
                    Err(error) => tracing::warn!(%error, "board lifecycle reconcile deferred"),
                }
            }
            let mut interval = tokio::time::interval(TICK_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! { _ = cancel.cancelled() => break, _ = interval.tick() => {} }
                let Some(rpc) = weak.upgrade() else { break };
                if let Err(error) = rpc.board_lifecycle_tick().await {
                    tracing::warn!(%error, "board lifecycle sweep deferred");
                }
            }
        });
    }

    /// Wake Board watchers when a linked chat's runtime status changes (a
    /// worker or reviewer starts waiting for an approval or an answer,
    /// fails, or goes idle), so an open Board re-reads those tickets'
    /// phase and Needs attention without waiting for a journal event.
    /// Status heartbeats are ignored; bursts are coalesced.
    fn start_board_runtime_watch(
        self: &std::sync::Arc<Self>,
        scope: &crate::maintenance::WorkerScope,
    ) {
        let weak = std::sync::Arc::downgrade(self);
        let mut sessions = self.sessions.watch_sessions();
        let cancel = scope.cancel.clone();
        scope.spawn(async move {
            let mut last: HashMap<String, (luna_proto::SessionStatus, Option<String>)> =
                HashMap::new();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    changed = sessions.changed() => if changed.is_err() { break },
                }
                // Coalesce a burst of transitions into one ping.
                tokio::time::sleep(RUNTIME_PING_COALESCE).await;
                let current: HashMap<String, (luna_proto::SessionStatus, Option<String>)> =
                    sessions
                        .borrow_and_update()
                        .iter()
                        .map(|session| {
                            (
                                session.chat_id.clone(),
                                (session.status, session.last_completed_turn.clone()),
                            )
                        })
                        .collect();
                let mut changed: Vec<String> = current
                    .iter()
                    .filter(|(chat, state)| last.get(*chat) != Some(*state))
                    .map(|(chat, _)| chat.clone())
                    .collect();
                changed.extend(
                    last.keys()
                        .filter(|chat| !current.contains_key(*chat))
                        .cloned(),
                );
                last = current;
                if changed.is_empty() {
                    continue;
                }
                let Some(rpc) = weak.upgrade() else { break };
                let ids = rpc.board_items_for_chats(&changed);
                rpc.board_hub().notify_observed(ids);
            }
        });
    }

    /// Tickets whose working or reviewer chat is one of `task_ids`
    /// (bounded).
    fn board_items_for_chats(&self, task_ids: &[String]) -> Vec<String> {
        let mut ids = Vec::new();
        for task in task_ids.iter().take(RUNTIME_PING_CHATS_MAX) {
            let Ok(links) = self.store.board_task_links_for_task(task, true) else {
                continue;
            };
            for link in links {
                if matches!(link.role, TaskLinkRole::Working | TaskLinkRole::Reviewer)
                    && !ids.contains(&link.work_item_id)
                {
                    ids.push(link.work_item_id);
                }
            }
        }
        ids
    }

    /// Deliver a generation's outcome (success, failure, cancellation or
    /// required intervention) once to each requested result subscription;
    /// queue a continuation only for `ReportAndContinue`. Restart never
    /// redelivers.
    #[allow(dead_code)] // contract hook for the review/tools slices
    pub(crate) async fn board_deliver_outcome(
        &self,
        work_item_id: &str,
        execution_generation: u64,
    ) -> BoardResult<()> {
        let _gate = self.board_lifecycle_gate(work_item_id).await;
        self.board_deliver_locked(work_item_id, execution_generation)
            .await
            .map(|_| ())
    }

    // ---- QA hooks (in-process only; not RPC methods) ----

    /// Call a lifecycle operation as an agent-bound chat run with the board
    /// scopes its provider tool delivery grants (no explicit user request).
    pub async fn board_qa_call_as_agent(
        &self,
        task_id: &str,
        operation: &str,
        params: Value,
    ) -> Result<Value, String> {
        let chat = self
            .workspace
            .chat(task_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "unknown chat".to_owned())?;
        let caller = BoardCaller::Agent {
            task_id: task_id.into(),
            project_ids: chat.space_id.into_iter().collect(),
            granted_scopes: HashSet::from([
                BOARD_SCOPE_READ.to_owned(),
                BOARD_SCOPE_WRITE.to_owned(),
            ]),
            explicit_user_request: false,
        };
        fn parse<T: serde::de::DeserializeOwned>(params: Value) -> BoardResult<T> {
            serde_json::from_value(params)
                .map_err(|error| BoardError::invalid("params", error.to_string()))
        }
        fn encode<T: Serialize>(value: BoardResult<T>) -> BoardResult<Value> {
            value.and_then(|value| serde_json::to_value(value).map_err(storage))
        }
        let result = match operation {
            "move" => match parse(params) {
                Ok(request) => encode(self.board_move(&caller, request).await),
                Err(error) => Err(error),
            },
            "start" => match parse(params) {
                Ok(request) => encode(self.board_start(&caller, request).await),
                Err(error) => Err(error),
            },
            "startBatch" => match parse(params) {
                Ok(request) => encode(self.board_start_batch(&caller, request).await),
                Err(error) => Err(error),
            },
            "control" => match parse(params) {
                Ok(request) => encode(self.board_control(&caller, request).await),
                Err(error) => Err(error),
            },
            "setStatus" => match parse(params) {
                Ok(request) => encode(self.board_set_status(&caller, request).await),
                Err(error) => Err(error),
            },
            _ => Err(BoardError::invalid(
                "operation",
                "unknown lifecycle operation",
            )),
        };
        result.map_err(|error| error.to_rpc_message())
    }

    /// The review slice's Done gate, for the late-callback E2E.
    pub async fn board_qa_mark_done(
        &self,
        work_item_id: &str,
        execution_generation: u64,
        intent_revision: u64,
    ) -> Result<Option<Value>, String> {
        self.board_mark_done_if_current(work_item_id, execution_generation, intent_revision)
            .await
            .map(|receipt| receipt.and_then(|receipt| serde_json::to_value(receipt).ok()))
            .map_err(|error| error.to_rpc_message())
    }

    /// Run boot reconciliation again (idempotent), returning its report.
    pub async fn board_qa_reconcile(&self) -> Result<Value, String> {
        self.board_reconcile_on_boot()
            .await
            .map_err(|error| error.to_rpc_message())
            .and_then(|report| serde_json::to_value(report).map_err(|error| error.to_string()))
    }
}
