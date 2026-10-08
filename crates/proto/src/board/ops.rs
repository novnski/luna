//! Request/response types for every `board.*` operation. One engine service
//! serves the Board UI, ordinary chats and provider-delivered tools.
//!
//! Requests never carry the caller: the engine binds it. Every mutation
//! carries a `request_id` (idempotency key, reused verbatim after an
//! uncertain result) and returns a durable receipt.
use serde::{Deserialize, Serialize};

use super::{
    BoardActor, BoardAdmissionReason, BoardColumn, BoardCompletionPolicy, BoardContextMode,
    BoardContextRef, BoardDestination, BoardExecutionIntent, BoardExecutionProfile, BoardFollowUp,
    BoardItemOrigin, BoardNavigationTarget, BoardPhase, BoardPriority, BoardProjectSummary,
    BoardReviewProfile, BoardScope, BoardSourceKind, BoardStatusReport, IssueReference,
    LaunchSnapshot, OutcomeReference, ResolvedExecution, ReviewCleanupState, ReviewFinding,
    ReviewRun, ReviewVerdict, TicketAuthoringSession, WorkItem, WorkItemView,
};
use crate::{CrossTaskSendMode, HarnessId, Model, PermissionMode, ProjectExecutionDefaults};

/// The operation a receipt belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardOperation {
    Create,
    Update,
    Rank,
    Archive,
    Unarchive,
    Move,
    Start,
    Control,
    SetStatus,
    SendUpdate,
    LinkChat,
    ConvertChat,
    CreateSideChat,
    CreateMainChat,
    Rename,
    ReviewStart,
    ReviewCancel,
    ReviewSubmit,
    IssueImport,
    IssueLink,
    IssueUnlink,
    SetDependency,
    IssueWriteback,
}

impl BoardOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Rank => "rank",
            Self::Archive => "archive",
            Self::Unarchive => "unarchive",
            Self::Move => "move",
            Self::Start => "start",
            Self::Control => "control",
            Self::SetStatus => "setStatus",
            Self::SendUpdate => "sendUpdate",
            Self::LinkChat => "linkChat",
            Self::ConvertChat => "convertChat",
            Self::CreateSideChat => "createSideChat",
            Self::CreateMainChat => "createMainChat",
            Self::Rename => "rename",
            Self::ReviewStart => "reviewStart",
            Self::ReviewCancel => "reviewCancel",
            Self::ReviewSubmit => "reviewSubmit",
            Self::IssueImport => "issueImport",
            Self::IssueLink => "issueLink",
            Self::IssueUnlink => "issueUnlink",
            Self::SetDependency => "setDependency",
            Self::IssueWriteback => "issueWriteback",
        }
    }
}

/// The durable result of one accepted mutation. A retry with the same
/// request id and payload returns this exact receipt (no second effect, no
/// second event).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardMutationReceipt {
    /// Engine-normalized request id (caller scope + idempotency key).
    pub request_id: String,
    pub payload_hash: String,
    pub operation: BoardOperation,
    pub work_item_id: String,
    pub key: String,
    /// Resulting revision and workflow state.
    pub revision: u64,
    pub column: BoardColumn,
    pub archived: bool,
    /// The committed record.
    pub item: WorkItem,
    /// The canonical event this mutation produced (`None` for a no-op).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
    pub actor: BoardActor,
    pub committed_at_ms: i64,
    /// Linked working chat when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub navigation: BoardNavigationTarget,
}

// ---------------------------------------------------------------------------
// Store / core (PB-0)
// ---------------------------------------------------------------------------

/// `board_create`: save only. Zero chats, zero launches, zero provider usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardCreateRequest {
    pub request_id: String,
    /// Required: All-projects creation must resolve a project explicitly.
    #[serde(default)]
    pub project_id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub implementation: Option<BoardExecutionProfile>,
    #[serde(default)]
    pub review: Option<BoardReviewProfile>,
    #[serde(default)]
    pub completion: Option<BoardCompletionPolicy>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub priority: Option<BoardPriority>,
    #[serde(default)]
    pub context_refs: Vec<BoardContextRef>,
    /// UI-declared provenance ("Create ticket from this chat" names the
    /// chat). Agent and authoring callers have their origin bound by the
    /// engine; a mismatching claim is rejected.
    #[serde(default)]
    pub origin: Option<BoardItemOrigin>,
    /// Default: top of Backlog.
    #[serde(default)]
    pub placement: BoardPlacement,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardPlacement {
    #[default]
    Top,
    Bottom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardReadRequest {
    /// Ticket id or human key (`LUN-42`; key lookups need `project_id` when
    /// the key prefix is ambiguous).
    pub work_item_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    /// Bound for each detail list (default 20, max 100).
    #[serde(default)]
    pub history_limit: Option<u32>,
}

/// Bounded list with project/All-projects scope and filters. Text and column
/// filters run in storage; attention/provider/source filters apply to the
/// fetched page, so a page may hold fewer than `limit` items while
/// `next_cursor` continues.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardListRequest {
    #[serde(default)]
    pub scope: BoardScope,
    /// Empty: every column.
    #[serde(default)]
    pub columns: Vec<BoardColumn>,
    #[serde(default)]
    pub archived: BoardArchivedFilter,
    /// Case-insensitive match on key, title and description.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub attention_only: bool,
    #[serde(default)]
    pub provider: Option<HarnessId>,
    #[serde(default)]
    pub source: Option<BoardSourceKind>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardArchivedFilter {
    /// Default board: archived tickets hidden.
    #[default]
    Exclude,
    Include,
    Only,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardListPage {
    /// Ordered by project, column (Backlog, Ready, Done), rank, id.
    pub items: Vec<WorkItemView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Projects present in scope (with counts), for the selector.
    pub projects: Vec<BoardProjectSummary>,
    /// Journal head when this page was read; watch after it.
    pub as_of_seq: u64,
}

/// Field-level patch against an expected revision. Absent fields are
/// untouched; column, archive, rank and links are separate operations.
/// Sent launch snapshots are never modified.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardItemChanges {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub implementation: Option<BoardExecutionProfile>,
    #[serde(default)]
    pub review: Option<BoardReviewProfile>,
    #[serde(default)]
    pub completion: Option<BoardCompletionPolicy>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    /// `Some(None)` clears the priority.
    #[serde(default)]
    pub priority: Option<Option<BoardPriority>>,
    #[serde(default)]
    pub context_refs: Option<Vec<BoardContextRef>>,
}

impl BoardItemChanges {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardUpdateRequest {
    pub request_id: String,
    pub work_item_id: String,
    pub expected_revision: u64,
    pub changes: BoardItemChanges,
}

/// Reorder within the current column. Never starts, stops, resends or
/// reviews anything; never changes the revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardRankRequest {
    pub request_id: String,
    pub work_item_id: String,
    /// The neighbor that should end up directly above (None: top).
    #[serde(default)]
    pub after_id: Option<String>,
    /// The neighbor that should end up directly below (None: bottom).
    #[serde(default)]
    pub before_id: Option<String>,
}

/// Archive hides; unarchive restores to the same column. Neither stops work
/// nor touches an external issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardArchiveRequest {
    pub request_id: String,
    pub work_item_id: String,
    pub archived: bool,
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

/// Reconcile an uncertain mutation by its original key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardReceiptRequest {
    pub request_id: String,
}

// ---------------------------------------------------------------------------
// Lifecycle (PB-1)
// ---------------------------------------------------------------------------

/// `board_move`: the one engine transition for drag, menu, keyboard and
/// agents. Persists desired intent first, chooses first start vs
/// continuation, coalesces races and reconciles actual Stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardMoveRequest {
    pub request_id: String,
    pub work_item_id: String,
    pub to: BoardColumn,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// Optional drop position in the destination column.
    #[serde(default)]
    pub after_id: Option<String>,
    #[serde(default)]
    pub before_id: Option<String>,
    /// First-start destination when this move dispatches a new worker.
    #[serde(default)]
    pub destination: BoardDestination,
    #[serde(default)]
    pub context: BoardContextMode,
    /// Explicit Start overrides (top of the precedence chain).
    #[serde(default)]
    pub execution: Option<BoardExecutionProfile>,
    #[serde(default)]
    pub follow_up: BoardFollowUp,
}

/// What a move actually did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardMoveEffect {
    /// Ordering/column only (e.g. Ready → Ready, Done → Backlog hold).
    None,
    FirstStart {
        launch_snapshot_id: String,
    },
    Continue {
        launch_snapshot_id: String,
    },
    /// Admitted but waiting (capacity, dependency, checkout, stop barrier).
    Queued {
        reason: BoardAdmissionReason,
    },
    /// Unsent launch cancelled before it could run.
    CancelledUnsent,
    StopRequested {
        execution_generation: u64,
    },
    /// Coalesced into an in-flight transition with the same latest intent.
    Coalesced,
    /// Outcome of an earlier attempt is unknown; reconciling first.
    Reconciling,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardMoveReceipt {
    pub mutation: BoardMutationReceipt,
    pub intent: BoardExecutionIntent,
    pub effect: BoardMoveEffect,
    pub phase: BoardPhase,
}

/// `board_start`: explicit start (equivalent to Move to Ready with explicit
/// destination/context/settings). Reuses an existing worker in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardStartRequest {
    pub request_id: String,
    pub work_item_id: String,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub destination: BoardDestination,
    #[serde(default)]
    pub context: BoardContextMode,
    #[serde(default)]
    pub execution: Option<BoardExecutionProfile>,
    #[serde(default)]
    pub follow_up: BoardFollowUp,
    /// UI: the chat the user started from (agent calls bind the caller).
    #[serde(default)]
    pub caller_task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardStartReceipt {
    pub mutation: BoardMutationReceipt,
    pub effect: BoardMoveEffect,
    /// `Started`, `Reused`, `Queued`, …
    pub outcome: BoardStartOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<LaunchSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<super::BoardWorkerPlacement>,
    pub navigation: BoardNavigationTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardStartOutcome {
    Started,
    /// An existing worker was continued (or was already active).
    Reused,
    Queued {
        reason: BoardAdmissionReason,
    },
    Reconciling,
    Failed {
        error: Box<super::BoardError>,
    },
}

/// Bounded multi-ticket start: independently idempotent per ticket
/// (`{request_id}:{work_item_id}`), with per-ticket results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardStartBatchRequest {
    pub request_id: String,
    pub work_item_ids: Vec<String>,
    #[serde(default)]
    pub destination: BoardDestination,
    #[serde(default)]
    pub context: BoardContextMode,
    #[serde(default)]
    pub execution: Option<BoardExecutionProfile>,
    #[serde(default)]
    pub follow_up: BoardFollowUp,
    #[serde(default)]
    pub caller_task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardStartBatchReceipt {
    pub request_id: String,
    pub results: Vec<BoardBatchItemResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardBatchItemResult {
    pub work_item_id: String,
    pub outcome: BoardStartOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<BoardStartReceipt>,
}

/// Explicit controls with durable receipts. Unsupported controls return a
/// reason instead of pretending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardControlAction {
    /// The chat's shared Stop (same command/scope as its Stop button).
    Stop,
    /// Continue the same worker with a continuation instruction.
    Continue,
    /// A deliberate user-authorized retry (grants more repair rounds).
    Retry,
    /// Cancel a queued, unsent launch.
    CancelQueued,
    /// Human acceptance under a Manual acceptance completion policy.
    Accept,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardControlRequest {
    pub request_id: String,
    pub work_item_id: String,
    pub action: BoardControlAction,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// Retry only: reuse the previous snapshot or adopt the latest brief.
    #[serde(default)]
    pub adopt_latest_brief: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardControlReceipt {
    pub mutation: BoardMutationReceipt,
    pub action: BoardControlAction,
    pub effect: BoardMoveEffect,
    pub intent: BoardExecutionIntent,
}

/// `board_set_status`: agent (or human) status report with provenance. The
/// running worker may report on its own ticket; Done is engine-owned and
/// cannot be set here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardSetStatusRequest {
    pub request_id: String,
    pub work_item_id: String,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// The generation the reporter believes it works in; a stale attempt
    /// cannot overwrite a newer one.
    #[serde(default)]
    pub execution_generation: Option<u64>,
    pub report: BoardStatusReport,
}

/// "Send update to chat": deliver the brief delta through the ordinary
/// queue/steer path. Never edits the original message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardSendUpdateRequest {
    pub request_id: String,
    pub work_item_id: String,
    pub expected_revision: u64,
    #[serde(default)]
    pub mode: CrossTaskSendMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardSendUpdateReceipt {
    pub mutation: BoardMutationReceipt,
    pub launch: LaunchSnapshot,
    pub from_brief_revision: u32,
    pub to_brief_revision: u32,
}

/// `board_execution_options`: real account/host-visible choices. Read only,
/// bounded, shared by manual and conversational creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardExecutionOptionsRequest {
    pub project_id: String,
    #[serde(default)]
    pub harness: Option<HarnessId>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardExecutionOptions {
    pub project_id: String,
    pub host_device_id: String,
    pub harnesses: Vec<BoardHarnessOptions>,
    pub project_defaults: ProjectExecutionDefaults,
    /// The effective defaults a ticket with an inherited profile would get.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_implementation: Option<ResolvedExecution>,
    /// No reviewer configured → the implementation model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_review: Option<ResolvedExecution>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardHarnessOptions {
    pub harness: HarnessId,
    pub available: bool,
    #[serde(default)]
    pub accounts: Vec<BoardAccountOption>,
    pub models: Vec<Model>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    pub permission_modes: Vec<PermissionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardAccountOption {
    pub account_id: String,
    pub label: String,
    pub active: bool,
}

/// Preview the settings a Start would freeze (no side effect).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardResolveSettingsRequest {
    pub work_item_id: String,
    #[serde(default)]
    pub destination: BoardDestination,
    #[serde(default)]
    pub execution: Option<BoardExecutionProfile>,
    #[serde(default)]
    pub caller_task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardResolvedSettings {
    pub implementation: ResolvedExecution,
    pub review: ResolvedExecution,
    pub review_fallback_to_implementation: bool,
    /// Destination as it would resolve for this caller.
    pub destination: BoardDestination,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardReconcileReport {
    pub examined: u64,
    pub resumed: u64,
    pub left_unknown: Vec<String>,
}

// ---------------------------------------------------------------------------
// Review (PB-1R)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardReviewStartRequest {
    pub request_id: String,
    pub work_item_id: String,
    #[serde(default)]
    pub execution_generation: Option<u64>,
    #[serde(default)]
    pub profile: Option<BoardReviewProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardReviewStatusRequest {
    #[serde(default)]
    pub work_item_id: Option<String>,
    #[serde(default)]
    pub review_run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardReviewStatus {
    pub runs: Vec<ReviewRun>,
    pub review_passes: u8,
    pub repair_rounds: u8,
    pub repairs_remaining: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardReviewCancelRequest {
    pub request_id: String,
    pub review_run_id: String,
}

/// Reviewer-submitted structured result. Only the recorded reviewer chat of
/// that run may submit; the engine owns every transition afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardReviewSubmitRequest {
    pub request_id: String,
    pub review_run_id: String,
    pub verdict: ReviewVerdict,
    pub basis_packet_hash: String,
    #[serde(default)]
    pub findings: Vec<ReviewFinding>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRunReceipt {
    pub request_id: String,
    pub run: ReviewRun,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigation: Option<BoardNavigationTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCleanupReceipt {
    pub review_run_id: String,
    pub state: ReviewCleanupState,
    /// Exact reviewer ids archived; read back independently.
    pub archived_task_ids: Vec<String>,
    pub closed_views: u32,
}

// ---------------------------------------------------------------------------
// Temporary authoring (Discuss with AI)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardAuthoringStartRequest {
    pub request_id: String,
    pub project_id: String,
    /// Resume a Board draft instead of starting a new session.
    #[serde(default)]
    pub resume_session_id: Option<String>,
    #[serde(default)]
    pub discussion: Option<super::AuthoringDiscussionProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardAuthoringSendRequest {
    pub session_id: String,
    /// Client-minted message id (no-replay key).
    pub message_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardAuthoringSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardAuthoringReadRequest {
    pub session_id: String,
    #[serde(default)]
    pub before: Option<u64>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// One authoring transcript page. Entries are opaque transcript JSON
/// (the same shape as session message entries).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardAuthoringPage {
    pub session: TicketAuthoringSession,
    pub entries: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<u64>,
    pub working: bool,
}

/// The assistant's draft proposal/configuration checkpoint. Creates no
/// ticket and no chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardAuthoringCheckpointRequest {
    pub session_id: String,
    pub expected_version: u64,
    pub proposal: super::AuthoringProposal,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardAuthoringListRequest {
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardAuthoringList {
    /// Resumable drafts and open sessions only (never ended/discarded).
    pub sessions: Vec<TicketAuthoringSession>,
}

// ---------------------------------------------------------------------------
// GitHub issues and dependencies (PB-2/PB-3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardIssueSourcesRequest {
    pub project_id: String,
}

/// A verified repository the project's host can read issues from. Multiple
/// remotes/forks need an explicit selection; origin is never assumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSource {
    /// Stable id: `github:{host}:{owner}/{name}`.
    pub id: String,
    pub repository: super::IssueRepository,
    pub remote_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub default: bool,
    /// Every remote that resolved to this repository (`origin`, `upstream`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_names: Vec<String>,
    /// The repository is a fork; `parent` names its upstream (`owner/name`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fork: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardIssueSources {
    pub project_id: String,
    pub host_device_id: String,
    pub sources: Vec<IssueSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<IssueSourceError>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IssueStateFilter {
    #[default]
    Open,
    Closed,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardIssueSearchRequest {
    pub project_id: String,
    pub source_id: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub state: IssueStateFilter,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// Bypass the cache (explicit refresh).
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSummary {
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_id: Option<String>,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
    pub updated_at: String,
    pub comments: u32,
    pub canonical_url: String,
    /// Existing ticket in this project (Open card by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_work_item_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueFreshness {
    pub fetched_at_ms: i64,
    pub from_cache: bool,
    pub stale: bool,
    /// The account (`gh` login) whose access produced this data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum IssueSourceError {
    AuthRequired { host: String },
    AccountChanged { host: String },
    NotFound,
    RateLimited { retry_after_ms: Option<u64> },
    Timeout,
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSearchPage {
    pub source_id: String,
    pub items: Vec<IssueSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Pagination stopped early or a page failed: disclosed, never hidden.
    pub partial: bool,
    pub freshness: IssueFreshness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<IssueSourceError>,
    /// Pull requests filtered out of the issue results.
    pub pull_requests_excluded: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardIssueDetailRequest {
    pub project_id: String,
    pub source_id: String,
    pub number: u64,
    /// Revalidate now (still conditional) instead of serving a fresh cache.
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueDetail {
    pub summary: IssueSummary,
    /// Untrusted source material (bounded).
    pub body: String,
    pub body_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_summary: Option<String>,
    pub freshness: IssueFreshness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardIssueCommentsRequest {
    pub project_id: String,
    pub source_id: String,
    pub number: u64,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueComment {
    pub id: String,
    pub author: String,
    pub created_at: String,
    pub body: String,
    pub body_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCommentPage {
    pub comments: Vec<IssueComment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub partial: bool,
}

/// Import (Add to board) or import-and-start. Duplicate imports return the
/// existing card; a second attempt is a separate deliberate action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardIssueImportRequest {
    pub request_id: String,
    pub project_id: String,
    pub source_id: String,
    pub number: u64,
    /// The user's prompt additions appended to the imported brief.
    #[serde(default)]
    pub additions: Option<String>,
    #[serde(default)]
    pub comment_ids: Vec<String>,
    #[serde(default)]
    pub start: Option<BoardImportStart>,
    /// `updatedAt` of the previewed issue: a remote edit since the preview
    /// is refused instead of silently importing different content.
    #[serde(default)]
    pub previewed_updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardImportStart {
    #[serde(default)]
    pub destination: BoardDestination,
    #[serde(default)]
    pub execution: Option<BoardExecutionProfile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardIssueImportReceipt {
    pub outcome: BoardIssueImportOutcome,
    pub mutation: BoardMutationReceipt,
    pub issue: IssueReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<BoardStartReceipt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardIssueImportOutcome {
    Created,
    /// Already linked in this project: the existing card.
    Existing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardIssueLinkRequest {
    pub request_id: String,
    pub work_item_id: String,
    pub expected_revision: u64,
    pub source_id: String,
    pub number: u64,
    #[serde(default = "default_issue_role")]
    pub role: super::IssueLinkRole,
}

fn default_issue_role() -> super::IssueLinkRole {
    super::IssueLinkRole::Related
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardIssueUnlinkRequest {
    pub request_id: String,
    pub work_item_id: String,
    pub expected_revision: u64,
    pub issue_ref_id: String,
}

/// Add (`present: true`) or remove a blocked-by relationship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardSetDependencyRequest {
    pub request_id: String,
    pub work_item_id: String,
    pub blocked_by_id: String,
    pub present: bool,
}

/// PB-3 explicit writeback. Previewed (exact account/repo/issue/payload),
/// then applied with independent remote readback; a lost response is
/// reconciled before any retry (no duplicate comment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum IssueWritebackAction {
    Comment { body: String },
    AddLabel { label: String },
    RemoveLabel { label: String },
    Close { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardIssueWritebackPreviewRequest {
    pub work_item_id: String,
    pub issue_ref_id: String,
    pub action: IssueWritebackAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueWritebackPreview {
    pub preview_id: String,
    pub expires_at_ms: i64,
    pub account: Option<String>,
    pub repository: super::IssueRepository,
    pub number: u64,
    /// The exact payload that will be sent (comments carry the hidden
    /// reconciliation marker shown here).
    pub action: IssueWritebackAction,
    #[serde(default)]
    pub canonical_url: String,
    /// The issue's remote state when previewed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardIssueWritebackRequest {
    pub request_id: String,
    pub work_item_id: String,
    pub preview_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueWritebackReceipt {
    pub request_id: String,
    pub outcome: OutcomeReference,
    /// Independent remote readback after the write.
    pub readback_verified: bool,
    pub canonical_url: String,
}

/// Bounded list of launches/outcomes for evidence surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardOutcomesRequest {
    pub work_item_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardOutcomes {
    pub outcomes: Vec<OutcomeReference>,
    pub launches: Vec<LaunchSnapshot>,
}
