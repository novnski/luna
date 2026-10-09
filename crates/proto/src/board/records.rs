//! Durable records linked to a ticket: chat links, external issues, launch
//! snapshots, outcomes, execution intent, dependencies, review runs and
//! temporary authoring sessions. Each is one SQLite row with a JSON record.
use serde::{Deserialize, Serialize};

use super::{
    BoardActor, BoardAttentionKind, BoardChatRole, BoardColumn, BoardContextMode, BoardContextRef,
    BoardDestination, BoardEvidenceRef, BoardExecutionProfile, BoardFollowUp, BoardPhase,
    BoardReviewProfile,
};
use crate::{CommandReceipt, HarnessId, PermissionMode, ReasoningLevel};

// ---------------------------------------------------------------------------
// Chat links
// ---------------------------------------------------------------------------

/// How a chat relates to a ticket. Only `Working` grants the ticket badge;
/// one active Working link per ticket and one per chat (enforced by unique
/// indexes, not by UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskLinkRole {
    /// The current primary working chat.
    Working,
    /// A historical or deliberately added attempt (same original container).
    Attempt,
    /// The discussion the ticket was created from. Provenance only.
    Source,
    /// An ordinary mention/related chat. Never a worker.
    Reference,
    /// A temporary reviewer side chat (see [`ReviewRun`]).
    Reviewer,
}

impl TaskLinkRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Attempt => "attempt",
            Self::Source => "source",
            Self::Reference => "reference",
            Self::Reviewer => "reviewer",
        }
    }
}

/// The container a working chat was first created in or converted from.
/// Frozen forever: no UI, agent, RPC or engine path may reattach a ticket
/// worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OriginalContainer {
    /// An independent main sidebar chat.
    MainChat,
    /// A side chat displayed in this main chat's workbench.
    SideOf { main_task_id: String },
}

/// Once-only result routing requested by the starter. Provenance is not a
/// subscription; this is only set when explicitly requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSubscription {
    pub recipient_task_id: String,
    pub follow_up: BoardFollowUp,
    /// Set once delivery is durably recorded; restart never redelivers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemTaskLink {
    pub id: String,
    pub work_item_id: String,
    /// Stable LUNA task id (never a provider session id).
    pub task_id: String,
    pub role: TaskLinkRole,
    pub active: bool,
    /// Chat role when linked; the canonical role is always re-derived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_role: Option<BoardChatRole>,
    /// Working/attempt links only: frozen at first creation or conversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_container: Option<OriginalContainer>,
    /// Who asked for this link (creator provenance, distinct from parent).
    pub linked_by: BoardActor,
    pub linked_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlinked_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_refs: Vec<BoardContextRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub result_subscriptions: Vec<ResultSubscription>,
}

// ---------------------------------------------------------------------------
// External issues (PB-2/PB-3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IssueProvider {
    GitHub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IssueLinkRole {
    /// The issue this ticket was imported from / implements.
    Source,
    Related,
}

/// Repository identity resolved from a verified remote. Names are display and
/// navigation fields; `api_id` is the stable dedupe identity when available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRepository {
    pub host: String,
    pub owner: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueReference {
    pub id: String,
    pub work_item_id: String,
    pub project_id: String,
    pub provider: IssueProvider,
    pub repository: IssueRepository,
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_api_id: Option<String>,
    pub canonical_url: String,
    /// Selected account reference (e.g. `gh` login), never a token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub role: IssueLinkRole,
    /// Remote state is separate from the LUNA workflow state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_updated_at: Option<String>,
    pub fetched_at_ms: i64,
    pub active: bool,
    pub linked_by: BoardActor,
    pub linked_at_ms: i64,
    /// Stable numeric GitHub account id behind `account` (cache and write
    /// scoping); never a token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Exactly the issue context imported into the brief at import time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_snapshot: Option<IssueContextSnapshot>,
    /// Hash of the import request that created this reference, so a retry of
    /// the same key replays instead of conflicting on changed remote content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_request_hash: Option<String>,
    /// When a remote edit or closure after import was first observed. The
    /// local brief is never rewritten and nothing is sent to a chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_changed_at_ms: Option<i64>,
    /// Stable id of the source this reference was read through
    /// (`github:{host}:{owner}/{name}`), for refresh and writeback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
}

impl IssueReference {
    /// One imported issue maps to one ticket per project (unique index on
    /// `(project_id, dedupe_key)` among active references).
    pub fn dedupe_key(&self) -> String {
        let repository = self.repository.api_id.clone().unwrap_or_else(|| {
            format!(
                "{}/{}",
                self.repository.owner.to_lowercase(),
                self.repository.name.to_lowercase()
            )
        });
        let issue = self
            .issue_api_id
            .clone()
            .unwrap_or_else(|| self.number.to_string());
        format!(
            "github:{}:{repository}#{issue}",
            self.repository.host.to_lowercase()
        )
    }

    pub fn summary(&self) -> IssueReferenceSummary {
        IssueReferenceSummary {
            id: self.id.clone(),
            provider: self.provider,
            display: format!(
                "{}/{}#{}",
                self.repository.owner, self.repository.name, self.number
            ),
            canonical_url: self.canonical_url.clone(),
            remote_state: self.remote_state.clone(),
            source_changed: self.source_changed_at_ms.is_some(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueReferenceSummary {
    pub id: String,
    pub provider: IssueProvider,
    /// `owner/repo#123`.
    pub display: String,
    pub canonical_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_state: Option<String>,
    /// The issue was edited or closed remotely after import.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_changed: bool,
}

/// Exactly the issue context frozen into one launch (body plus selected
/// comments, with truncation disclosed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueContextSnapshot {
    pub issue_ref_id: String,
    pub remote_updated_at: Option<String>,
    pub body: String,
    pub body_truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comment_ids: Vec<String>,
    pub omitted_summary: Option<String>,
    pub content_hash: String,
}

// ---------------------------------------------------------------------------
// Launch snapshots
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardInstructionKind {
    /// First dispatch of a never-dispatched ticket: the brief is sent once.
    FirstStart,
    /// Backlog → Ready after work started (or a converted chat's first board
    /// dispatch): resume from the checkpoint, never replay the brief.
    Continuation,
    /// Explicit "Send update to chat" with a brief revision delta.
    BriefUpdate,
    /// Recovery under the same visible chat after a lost provider session.
    Recovery,
    /// A deliberate, user-authorized retry (another attempt or more rounds).
    Retry,
    /// Review findings returned to the implementer.
    RepairRequest,
}

/// Fully resolved settings frozen before queueing. Later default changes
/// never alter an accepted launch. A receipt proves admission/persistence,
/// not that the provider honored every option ([`Self`] vs observed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedExecution {
    pub harness: HarnessId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningLevel>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    pub permission_mode: PermissionMode,
    pub host_device_id: String,
    pub project_id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Where each resolved field came from, for honest display.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<BoardSettingSource>,
}

/// Provenance of one resolved setting along the precedence chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardSettingSource {
    /// `harness`, `model`, `reasoning`, `permissionMode`, `checkout`, …
    pub field: String,
    pub source: BoardSettingOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardSettingOrigin {
    StartOverride,
    Ticket,
    Parent,
    Project,
    ProviderDefault,
    CatalogDefault,
    /// Reviewer only: no reviewer configured, so the implementation model.
    ImplementationFallback,
    /// An existing working chat keeps its own configuration.
    ExistingWorker,
}

/// Repository basis frozen before a ticket's first execution. Continuations
/// and repairs retain it so committing work cannot remove it from review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardReviewBase {
    pub checkout_id: String,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSnapshot {
    pub id: String,
    pub work_item_id: String,
    /// Stable launch request identity, persisted before sending; a timeout
    /// reconciles by this key and never retries blindly.
    pub launch_request_id: String,
    pub execution_generation: u64,
    pub intent_revision: u64,
    pub brief_revision: u32,
    pub kind: BoardInstructionKind,
    /// Exact brief text frozen at dispatch (immutable).
    pub sent_title: String,
    pub sent_description: String,
    /// The exact app-generated instruction sent through the ordinary task
    /// command path (quoted brief, trusted workflow metadata separate).
    pub instruction_text: String,
    pub instruction_hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_refs: Vec<BoardContextRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_context: Option<IssueContextSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_base: Option<BoardReviewBase>,
    pub requested: BoardExecutionProfile,
    pub resolved: ResolvedExecution,
    pub destination: BoardDestination,
    pub context_mode: BoardContextMode,
    /// The working chat this launch targets.
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_parent_id: Option<String>,
    pub command_id: String,
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_receipt: Option<CommandReceipt>,
    pub follow_up: BoardFollowUp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_recipient_task_id: Option<String>,
    pub created_by: BoardActor,
    pub created_at_ms: i64,
}

// ---------------------------------------------------------------------------
// Outcomes and evidence (PB-1R/PB-3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutcomeKind {
    Diff,
    PullRequest,
    Check,
    Artifact,
    /// A non-code deliverable (document, analysis).
    Document,
    ReviewResult,
    Summary,
    /// Explicit external writeback receipt with readback (PB-3).
    IssueWriteback,
}

impl OutcomeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::PullRequest => "pullRequest",
            Self::Check => "check",
            Self::Artifact => "artifact",
            Self::Document => "document",
            Self::ReviewResult => "reviewResult",
            Self::Summary => "summary",
            Self::IssueWriteback => "issueWriteback",
        }
    }
}

/// A reference to an existing result, artifact, PR or check with the
/// revision/time it was observed. Never a copy of large artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeReference {
    pub id: String,
    pub work_item_id: String,
    pub kind: OutcomeKind,
    pub execution_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub label: String,
    /// Diff hash, head SHA or other basis this evidence describes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `passed`, `failed`, `open`, `merged`, … as observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Bounded structured detail (≤ 16 KiB serialized).
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub detail: serde_json::Value,
    /// Superseded by a newer result; kept as history.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub historical: bool,
    pub observed_by: BoardActor,
    pub observed_at_ms: i64,
}

// ---------------------------------------------------------------------------
// Execution intent (desired vs observed) — owned by the lifecycle slice
// ---------------------------------------------------------------------------

/// Delivery state of the first execution instruction, tracked separately
/// from chat creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FirstPromptDelivery {
    /// Nothing sent yet (including a chat allocated before delivery).
    NotSent,
    /// Persisted command; provider acceptance not yet observed.
    Pending {
        command_id: String,
        message_id: String,
    },
    Delivered {
        command_id: String,
        message_id: String,
        at_ms: i64,
    },
    /// Outcome uncertain: reconcile the original command/message first.
    Unknown {
        command_id: String,
        message_id: String,
        detail: String,
    },
    /// A converted existing chat never receives a first-start brief.
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardStopState {
    Requested,
    Stopping,
    Stopped,
    /// Termination could not be verified; stays visible.
    Unknown,
    Rejected,
}

/// Stop tied to one exact admitted execution generation. A late
/// acknowledgement for an older generation can never apply to a newer run;
/// Ready waits for this barrier to clear before sending Continue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardStopBarrier {
    pub execution_generation: u64,
    pub request_id: String,
    pub state: BoardStopState,
    /// Exact owned work included (working chat turn, owned children, owned
    /// reviewer chats).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ids: Vec<String>,
    /// `TaskWorkStopReceipt` request ids for readback.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_receipt_ids: Vec<String>,
    pub requested_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Why Ready work is not running yet. The ticket stays in Ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardAdmissionReason {
    ProjectCapacity { limit: u32 },
    HostCapacity { limit: u32 },
    ProviderCapacity { harness: HarnessId, limit: u32 },
    Budget { detail: String },
    Dependency { blocked_by: Vec<String> },
    CheckoutBusy { task_id: String },
    WaitingForStop { execution_generation: u64 },
    HostUnavailable { host_device_id: String },
    SettingsUnresolved { fields: Vec<String> },
}

/// Desired state persisted before any effect, separate from observation.
/// One row per ticket; CAS on `version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardExecutionIntent {
    pub work_item_id: String,
    /// Row CAS version (every write increments).
    pub version: u64,
    /// Latest desired column (Backlog/Ready). Done is engine-owned.
    pub desired: BoardColumn,
    /// Monotonic; increments on every accepted move. Callbacks carrying an
    /// older revision are ignored.
    pub intent_revision: u64,
    /// Increments on every admitted execution (first start, continuation,
    /// retry). Stop, review and completion are tied to one generation.
    pub execution_generation: u64,
    /// The transition request that produced this revision (dedupes repeats
    /// from two windows).
    pub last_request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_command_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<String>,
    pub first_prompt: FirstPromptDelivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<BoardStopBarrier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<BoardAdmissionReason>,
    /// Review/repair accounting for the current cycle; survives restart and
    /// Backlog/Ready so Stop/Continue cannot reset the cap.
    #[serde(default)]
    pub review_passes: u8,
    #[serde(default)]
    pub repair_rounds: u8,
    /// Last engine-observed phase (cache for restart readback; observations
    /// are re-derived before acting).
    pub observed_phase: BoardPhase,
    /// Set when restart must query receipts/runtime before acting.
    #[serde(default)]
    pub needs_reconcile: bool,
    pub updated_at_ms: i64,
    /// The `intent_revision` last served by an admitted launch (or by reusing
    /// an already-active worker). Ready with a newer revision still needs
    /// admission; an equal one is never dispatched again.
    #[serde(default)]
    pub served_intent_revision: u64,
    /// Delivery of the current generation's launch (`launch_snapshot_id`).
    #[serde(default)]
    pub launch_state: BoardLaunchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_detail: Option<String>,
    /// Engine boot that sent the current launch: a pending receipt from an
    /// earlier boot can no longer prove acceptance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_boot_id: Option<String>,
    /// Why the latest dispatch attempt could not run (shown as Needs
    /// attention until a new transition clears it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_failure: Option<super::BoardAttentionReason>,
    /// The start request waiting for admission (destination, overrides and
    /// the settings frozen when it was accepted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_start: Option<BoardPendingStart>,
    /// The generation that reached Done (from verified current evidence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_generation: Option<u64>,
    /// Fingerprint of the last published Needs-attention overlay, so a
    /// change that leaves the phase alone (an approval appearing on a
    /// running worker) still wakes Board watchers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_attention: Option<String>,
}

/// Delivery of one launch instruction through the ordinary task command
/// path. Only an applied receipt correlated with the exact message proves
/// provider acceptance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardLaunchState {
    /// No launch for the current generation.
    #[default]
    None,
    /// Frozen and persisted; the command has not been issued yet.
    Unsent,
    /// Command admitted; provider acceptance not yet observed.
    Pending,
    Delivered,
    /// Acceptance can no longer be proven; reconcile by the original
    /// command/message id before any new send.
    Unknown,
    /// The command was refused before reaching the provider.
    Failed,
    /// Cancelled before it was sent.
    Cancelled,
}

/// A Start/Continue request accepted into the intent and waiting for (or
/// served by) admission. The caller is recorded as provenance; authority was
/// checked when the request was accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardPendingStart {
    pub request_id: String,
    pub actor: BoardActor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_task_id: Option<String>,
    /// As requested, and as resolved for this caller (concrete form).
    pub destination: BoardDestination,
    pub resolved_destination: BoardDestination,
    #[serde(default)]
    pub context: BoardContextMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<BoardExecutionProfile>,
    #[serde(default)]
    pub follow_up: BoardFollowUp,
    /// Settings frozen when accepted (new workers only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedExecution>,
    /// Deferred settings that must be chosen before dispatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
    /// A deliberate user-authorized retry.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retry: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub adopt_latest_brief: bool,
    pub accepted_at_ms: i64,
}

// ---------------------------------------------------------------------------
// Dependencies (PB-2 blocked-by)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DependencySource {
    Manual,
    Issue { issue_ref_id: String },
}

/// `work_item_id` is blocked by `blocked_by_id`. Visible only: clearing a
/// dependency never authorizes or launches work. Cycles are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardDependency {
    pub work_item_id: String,
    pub blocked_by_id: String,
    pub source: DependencySource,
    pub created_by: BoardActor,
    pub created_at_ms: i64,
}

// ---------------------------------------------------------------------------
// Review runs (PB-1R)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewRunState {
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    /// Launch or delivery uncertain; recover by the original key.
    OutcomeUnknown,
    /// The reviewed basis changed; cannot complete the ticket.
    Stale,
}

impl ReviewRunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::OutcomeUnknown => "outcomeUnknown",
            Self::Stale => "stale",
        }
    }

    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Stale
        )
    }
}

/// Main ticket → reviewer side chat directly under it; side ticket → fresh
/// sibling under the same main parent. Never nested, never a main chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewerPlacement {
    UnderTicket,
    SiblingUnderMain,
}

/// Requested, resolved and observed reviewer settings, kept distinct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewProfileResolution {
    pub requested: BoardReviewProfile,
    pub resolved: ResolvedExecution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_harness: Option<HarnessId>,
    /// No reviewer configured: the implementation model was used.
    #[serde(default)]
    pub implementation_fallback: bool,
}

/// The engine-assembled, size-bounded review basis. The implementer may add
/// a summary but never controls the only evidence the reviewer sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewBasis {
    pub packet_version: u32,
    pub brief_revision: u32,
    pub checkout_id: Option<String>,
    pub cwd: String,
    pub base_revision: Option<String>,
    pub head_revision: Option<String>,
    pub diff_hash: String,
    /// Hash of the included uncommitted-file manifest (commit id alone is
    /// insufficient with uncommitted changes).
    pub uncommitted_manifest_hash: Option<String>,
    pub packet_hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_outcome_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omissions: Vec<String>,
    pub captured_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewVerdict {
    Passed,
    ChangesRequested,
    Blocked,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewSeverity {
    Blocker,
    Major,
    Minor,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFinding {
    pub id: String,
    pub severity: ReviewSeverity,
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub explanation: String,
    pub required_action: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// Same finding persisted from an earlier round (no-progress detection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeats_finding_id: Option<String>,
}

/// Structured reviewer result. "Looks good" prose, a provider exit or an
/// empty result can never satisfy Done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewResult {
    pub verdict: ReviewVerdict,
    /// Must equal the run's [`ReviewBasis::packet_hash`].
    pub basis_packet_hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<ReviewFinding>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_model: Option<String>,
    pub submitted_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewCleanupState {
    NotStarted,
    /// Eligible, journaled; archive pending or retrying the same identity.
    Pending,
    Archived,
    /// Archived and every view closed (independent readback).
    Closed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCleanup {
    pub state: ReviewCleanupState,
    pub cleanup_id: String,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRun {
    /// Distinct from ticket, chat and provider-session ids.
    pub id: String,
    pub work_item_id: String,
    pub version: u64,
    pub execution_generation: u64,
    /// 0 = initial review; 1..=2 = review after each repair round.
    pub round: u8,
    pub primary_task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_task_id: Option<String>,
    /// Display parent (the ticket for a main ticket, its main parent for a
    /// side ticket). The workflow owner is always the ticket.
    pub display_main_parent_id: String,
    pub placement: ReviewerPlacement,
    pub launch_request_id: String,
    pub profile: ReviewProfileResolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<ReviewBasis>,
    pub state: ReviewRunState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ReviewResult>,
    /// Once-only return of findings/result to the ticket chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_request_id: Option<String>,
    pub cleanup: ReviewCleanup,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Why the run is unresolved, failed, stale or cancelled (shown with
    /// Needs attention; never a success claim).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<BoardAttentionKind>,
    /// Who asked for this pass (the implementer's run, the user, or the
    /// engine's automatic re-review). Never the reviewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<BoardActor>,
    /// Normalized request id of the accepted `review_submit` (once-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_request_id: Option<String>,
    /// The result arrived after the run was cancelled, went stale or its
    /// generation ended: kept as history, never applied.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub late_result: bool,
    /// How the engine applied a completed result (once-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ReviewRunOutcome>,
    /// Implementer-contributed summary (untrusted; never the only evidence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementer_summary: Option<String>,
    /// Evidence the implementer referenced (PR link, artifacts, documents).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<BoardEvidenceRef>,
    /// `OutcomeReference` ids of the checks reported for this pass; only
    /// those whose basis equals the reviewed diff hash count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reported_check_ids: Vec<String>,
    /// The run this pass replaces (stale basis, or a reviewer turn that
    /// ended without a structured result).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    /// Automatic relaunches already spent on this chain (bounded).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub relaunches: u8,
}

fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

/// How a completed review result was applied. Recorded once; a restart
/// retries only the missing delivery, never the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ReviewRunOutcome {
    /// Findings returned to the implementer as a durable repair request.
    RepairRequested {
        launch_snapshot_id: String,
        repair_round: u8,
    },
    /// Current checks and this review passed: the ticket moved to Done.
    Completed { execution_generation: u64 },
    /// Passed, but the completion policy needs a human acceptance.
    AwaitingAcceptance,
    /// Blockers remain or the contract cannot be met automatically.
    NeedsAttention { reason: BoardAttentionKind },
    /// The ticket's intent or generation changed: kept as history only.
    Historical,
}

/// One check the implementer ran for its current result. Agent-reported:
/// the engine ties it to the snapshot captured when it was reported, and the
/// reviewer verifies it. A check from another snapshot never counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCheckReport {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Bounded output excerpt or evidence pointer (untrusted text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Temporary ticket-authoring sessions
// ---------------------------------------------------------------------------

/// Discuss with AI lives here, never in the chat registry: no sidebar,
/// side-chat list, Settled, Archive, search, sync projection or import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthoringState {
    /// Runtime may be live; the composer is open.
    Active,
    /// Closed unfinished: a resumable Board draft only. Runtime stopped.
    Draft,
    /// A ticket was saved; the session is still open.
    Saved,
    /// Saved then closed: ended and cleaned up.
    Ended,
    /// Explicitly discarded: draft removed.
    Discarded,
}

impl AuthoringState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draft => "draft",
            Self::Saved => "saved",
            Self::Ended => "ended",
            Self::Discarded => "discarded",
        }
    }
}

/// The discussion assistant's own settings: independent of the future
/// implementation/review profiles; neither broadens the other.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AuthoringDiscussionProfile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningLevel>,
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

/// The assistant's internal candidate. Rendered read-only; revised only
/// through conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AuthoringProposal {
    pub title: String,
    pub description: String,
    pub implementation: BoardExecutionProfile,
    pub review: BoardReviewProfile,
    pub completion: super::BoardCompletionPolicy,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context_refs: Vec<BoardContextRef>,
    /// Settings the assistant still has to ask about.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketAuthoringSession {
    /// Scoped purpose/actor id (`auth-…`), never a chat id.
    pub id: String,
    pub version: u64,
    pub project_id: String,
    pub owner_host_id: String,
    pub state: AuthoringState,
    pub discussion: AuthoringDiscussionProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<AuthoringProposal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_item_ids: Vec<String>,
    pub message_count: u64,
    /// Opaque private runtime binding (provider session id), never exposed
    /// to ordinary history discovery; kept to block re-import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_binding: Option<String>,
    /// Earlier provider bindings of this session (a provider that replaced
    /// its session id). Exclusion metadata only, like `runtime_binding`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_bindings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at_ms: Option<i64>,
}

/// Convenience used in receipts and lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardWorkerPlacement {
    pub task_id: String,
    pub role: BoardChatRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_parent_id: Option<String>,
    pub container: OriginalContainer,
}

impl BoardExecutionIntent {
    /// The starting point for a ticket that has never been moved.
    pub fn initial(work_item_id: &str, now_ms: i64) -> Self {
        Self {
            work_item_id: work_item_id.into(),
            version: 0,
            desired: BoardColumn::Backlog,
            intent_revision: 0,
            execution_generation: 0,
            last_request_id: String::new(),
            working_task_id: None,
            active_command_id: None,
            active_message_id: None,
            active_turn_id: None,
            first_prompt: FirstPromptDelivery::NotSent,
            launch_snapshot_id: None,
            stop: None,
            queue: None,
            review_passes: 0,
            repair_rounds: 0,
            observed_phase: BoardPhase::NotStarted,
            needs_reconcile: false,
            updated_at_ms: now_ms,
            served_intent_revision: 0,
            launch_state: BoardLaunchState::None,
            launch_detail: None,
            launch_boot_id: None,
            launch_failure: None,
            pending_start: None,
            completed_generation: None,
            observed_attention: None,
        }
    }
}
