//! Lifecycle slice (A2, PB-1): Backlog/Ready/Done moves that start, continue
//! and stop work; execution intent, generations and the stop barrier;
//! first-start vs continuation instructions; settings resolution and launch
//! snapshots; bounded batch start with admission; status reports; "Send
//! update to chat"; once-only result delivery; restart reconciliation.
//!
//! Contract: docs/design/project-board/pb0-contract.md §5–§7. Every function
//! below is wired into the dispatcher; replace the bodies, keep signatures.
//! Reuse the shared task-command path for sends, `task_work` Stop for stops
//! and `child_threads`/`side_chats` creation through the chat-identity slice.
// Entry points the dispatcher does not call yet are wired by the owning slice.
#![allow(dead_code)]
use super::*;
use luna_proto::{
    BoardControlReceipt, BoardControlRequest, BoardExecutionIntent, BoardExecutionOptions,
    BoardExecutionOptionsRequest, BoardExecutionProfile, BoardMoveReceipt, BoardMoveRequest,
    BoardObservation, BoardReconcileReport, BoardResolveSettingsRequest, BoardResolvedSettings,
    BoardReviewProfile, BoardSendUpdateReceipt, BoardSendUpdateRequest, BoardSetStatusRequest,
    BoardStartBatchReceipt, BoardStartBatchRequest, BoardStartReceipt, BoardStartRequest, WorkItem,
};

impl EngineRpc {
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
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.move"))
    }

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
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.start"))
    }

    /// Bounded multi-ticket start (≤ `BOARD_BATCH_START_MAX`): each ticket
    /// is an independently idempotent start keyed `{request_id}:{item}` with
    /// a per-ticket Started/Reused/Queued/Failed result and admission reason.
    pub(crate) async fn board_start_batch(
        &self,
        caller: &BoardCaller,
        request: BoardStartBatchRequest,
    ) -> BoardResult<BoardStartBatchReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.startBatch"))
    }

    /// Stop / Continue / Retry / CancelQueued / Accept with durable receipts.
    /// Stop uses the same `task_work` command and scope as the chat's Stop
    /// button; unknown termination stays `StopUnknown`, never "stopped".
    pub(crate) async fn board_control(
        &self,
        caller: &BoardCaller,
        request: BoardControlRequest,
    ) -> BoardResult<BoardControlReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.control"))
    }

    /// Agent/human status report with provenance. The ticket's own working
    /// chat may report without an explicit user request; a stale generation
    /// is rejected; identical repeats are no-ops (no event); Done is never
    /// settable here.
    pub(crate) async fn board_set_status(
        &self,
        caller: &BoardCaller,
        request: BoardSetStatusRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.setStatus"))
    }

    /// "Send update to chat": deliver the brief delta since the last sent
    /// revision through the ordinary queue/steer path, recorded as a
    /// `BriefUpdate` launch snapshot. Never edits earlier messages.
    pub(crate) async fn board_send_update(
        &self,
        caller: &BoardCaller,
        request: BoardSendUpdateRequest,
    ) -> BoardResult<BoardSendUpdateReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.sendUpdate"))
    }

    /// Real account/host-visible harnesses, models, efforts, access modes and
    /// effective project defaults (read only, bounded). Reuses the model
    /// cache and provider defaults; never invents ids.
    pub(crate) async fn board_execution_options(
        &self,
        caller: &BoardCaller,
        request: BoardExecutionOptionsRequest,
    ) -> BoardResult<BoardExecutionOptions> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.executionOptions"))
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
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.resolveSettings"))
    }

    /// Engine-observed phase/attention/queue for a ticket, from runtime,
    /// task-work and attention evidence. Called by every read model. Until
    /// implemented, `store.rs` uses honest fallbacks (NotStarted / Complete
    /// in Done / Unknown) that never claim running or stopped work.
    pub(crate) fn board_observe(
        &self,
        item: &WorkItem,
        intent: Option<&BoardExecutionIntent>,
    ) -> BoardResult<BoardObservation> {
        let _ = (item, intent);
        Err(BoardError::not_implemented("board.observe"))
    }

    /// Save-time validation of requested settings against the catalog
    /// (unknown model/effort → `UnavailableModel`/`UnsupportedEffort` with
    /// supported choices; deferred settings are allowed). `store.rs` treats
    /// `NotImplemented` as "shape-validated only".
    pub(crate) fn board_validate_requested_settings(
        &self,
        project: &Space,
        implementation: &BoardExecutionProfile,
        review: &BoardReviewProfile,
    ) -> BoardResult<()> {
        let _ = (project, implementation, review);
        Err(BoardError::not_implemented("board.validateSettings"))
    }

    /// Boot reconciliation: restore intents and query receipts/runtime
    /// ownership before acting (a saved Ready alone never resends a start).
    /// Called once from the board maintenance task.
    pub(crate) async fn board_reconcile_on_boot(&self) -> BoardResult<BoardReconcileReport> {
        Err(BoardError::not_implemented("board.reconcile"))
    }

    /// Deliver a generation's outcome (success, failure, cancellation or
    /// required intervention) once to each requested result subscription;
    /// queue a continuation only for `ReportAndContinue`. Restart never
    /// redelivers.
    pub(crate) async fn board_deliver_outcome(
        &self,
        work_item_id: &str,
        execution_generation: u64,
    ) -> BoardResult<()> {
        let _ = (work_item_id, execution_generation);
        Err(BoardError::not_implemented("board.deliverOutcome"))
    }

    /// For the review slice: move a ticket to Done when the passing result
    /// still belongs to the current generation and intent revision. A stale
    /// or held ticket returns `None` and changes nothing. (Contract addition
    /// by A5; the lifecycle slice owns the body.)
    pub(crate) async fn board_mark_done_if_current(
        &self,
        work_item_id: &str,
        execution_generation: u64,
        intent_revision: u64,
    ) -> BoardResult<Option<BoardMutationReceipt>> {
        let _ = (work_item_id, execution_generation, intent_revision);
        Err(BoardError::not_implemented("board.markDoneIfCurrent"))
    }
}
