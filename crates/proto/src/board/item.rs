//! The ticket record, its saved execution profiles and its read models.
use serde::{Deserialize, Serialize};

use super::{BoardActor, BoardChatRole, BoardColumn};
use crate::{HarnessId, PermissionMode, ReasoningLevel};

/// A board ticket. Describes **what should be achieved**; chats record the
/// work toward it. Saving one creates no chat and starts nothing.
///
/// Not to be confused with [`crate::TaskWorkItem`] (background execution of
/// one chat).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItem {
    pub schema_version: u32,
    /// Stable app-owned id (`wi-…`). Never a title, key or GitHub number.
    pub id: String,
    /// Owning LUNA project ([`crate::Space`] id); fixed at creation.
    pub project_id: String,
    /// The project's owning host; the only writer of this record.
    pub owner_host_id: String,
    /// Human key (`LUN-42`): per-project prefix plus a monotonic number,
    /// allocated once and never reused. Distinct from GitHub issue numbers.
    pub key: String,
    pub number: u64,
    pub title: String,
    /// The editable brief. Frozen copies live in [`super::LaunchSnapshot`].
    pub description: String,
    /// Increments when title, description or attachments change. Launch snapshots
    /// and instructions cite it ("brief revision 3").
    pub brief_revision: u32,
    pub column: BoardColumn,
    /// Hidden from the default board; links, history and work are kept and
    /// archiving neither stops work nor touches an external issue.
    pub archived: bool,
    /// Permanent tombstone. Keys and audit history remain reserved; ordinary
    /// reads and all live projections exclude this item.
    #[serde(default)]
    pub deleted: bool,
    /// Dense order key within (project, column): lower sorts first; ties by
    /// id. The owning store allocates bounded keys and compacts legacy ranks
    /// transactionally without changing their relative order. Reordering never changes
    /// [`Self::revision`] and has no execution effect.
    pub rank: String,
    /// Monotonic record revision (1 on create). Every content, workflow or
    /// link mutation increments it; stale writers get the current state back.
    pub revision: u64,
    /// The item revision at which each editable field last changed, for
    /// field-level conflict detection.
    #[serde(default)]
    pub field_revisions: BoardFieldRevisions,
    /// Requested implementation settings (unset fields inherit).
    #[serde(default)]
    pub implementation: BoardExecutionProfile,
    /// Requested reviewer settings (unset fields inherit; no reviewer
    /// configured anywhere falls back to the implementation model in a
    /// separate reviewer side chat).
    #[serde(default)]
    pub review: BoardReviewProfile,
    #[serde(default)]
    pub completion: BoardCompletionPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<BoardPriority>,
    /// How the ticket came to exist. Provenance, not an execution parent.
    pub origin: BoardItemOrigin,
    /// Selected durable references carried into the first launch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_refs: Vec<BoardContextRef>,
    /// Images attached to the brief: upload paths on the owning host. The
    /// first launch delivers them like a chat message's attachments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    /// `@` file mentions and `$` skills written in the brief, delivered with
    /// the first launch exactly as a chat sends them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_items: Vec<crate::ComposerInput>,
    /// Latest agent-reported progress/blocker (never engine-observed state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_report: Option<BoardStatusReportRecord>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub created_by: BoardActor,
    pub updated_by: BoardActor,
}

/// Item revision at which each editable field last changed. A field-level
/// update conflicts only when one of the fields it changes moved past the
/// caller's expected revision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BoardFieldRevisions {
    pub title: u64,
    pub description: u64,
    pub implementation: u64,
    pub review: u64,
    pub completion: u64,
    pub labels: u64,
    pub priority: u64,
    pub context_refs: u64,
    pub attachments: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardPriority {
    Urgent,
    High,
    Medium,
    Low,
}

/// Requested implementation settings saved on a ticket. `None` inherits along
/// the precedence chain (explicit Start override → this profile → compatible
/// parent settings for a same-project side chat → project defaults → saved
/// provider defaults → catalog default). Resolution happens at dispatch and is
/// frozen in the launch snapshot; nothing here proves availability.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BoardExecutionProfile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessId>,
    /// LUNA provider-account id (metadata only, never a credential).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningLevel>,
    /// Provider-advertised option id → choice id.
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    /// Requested access. Saving it is not a grant; launch admission applies
    /// existing permission/grant rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout: Option<BoardCheckoutChoice>,
    /// The user deliberately deferred choosing settings. Ready must resolve
    /// them before dispatch instead of silently picking greater access.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deferred: bool,
}

impl BoardExecutionProfile {
    pub fn is_inherited(&self) -> bool {
        self == &Self::default()
    }
}

/// Checkout preference for a new working chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardCheckoutChoice {
    /// The project's (or same-project parent's) current checkout.
    Current,
    /// A LUNA-materialized worktree on a new branch. Never created silently.
    NewWorktree {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
}

/// Requested reviewer settings. Reviewer access is always constrained to the
/// review contract by the engine, so there is no access field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BoardReviewProfile {
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
    /// Explicitly skip automatic review (only honored by a Manual acceptance
    /// completion policy; the default policy always reviews).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

/// What Done requires.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BoardCompletionPolicy {
    pub mode: BoardCompletionMode,
    /// Named checks that must pass for the current result (project commands
    /// or agent-run verification). Empty: the agent-reported checks recorded
    /// as evidence for the current result.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required_checks: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardCompletionMode {
    /// Default: current checks plus the configured reviewer side chat.
    #[default]
    AutomaticChecksAndReview,
    /// Optional: a human accepts the result explicitly.
    ManualAcceptance,
}

/// Creation provenance. A source discussion is never the execution parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardItemOrigin {
    /// Write manually on the Board.
    Manual,
    /// Created from an ordinary main/side chat ("create a ticket from this").
    Chat {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
    /// Created in place from an existing chat that became its worker.
    Converted { task_id: String },
    /// Saved by a temporary authoring session. The session id is scoped
    /// provenance only; no ordinary-chat backlink exists.
    Authoring { session_id: String },
    /// Imported from an external issue.
    Issue { issue_ref_id: String },
}

impl BoardItemOrigin {
    pub fn source_kind(&self) -> BoardSourceKind {
        match self {
            Self::Manual => BoardSourceKind::Local,
            Self::Chat { .. } | Self::Converted { .. } => BoardSourceKind::Chat,
            Self::Authoring { .. } => BoardSourceKind::Authoring,
            Self::Issue { .. } => BoardSourceKind::GitHub,
        }
    }
}

/// Source filter values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardSourceKind {
    Local,
    Chat,
    Authoring,
    GitHub,
}

/// A selected durable reference. Content is untrusted data; it never carries
/// authority, permissions or instructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardContextRef {
    /// A project-relative file or directory path.
    File {
        path: String,
    },
    Url {
        url: String,
    },
    /// A message in an ordinary chat (never a temporary authoring session).
    ChatMessage {
        task_id: String,
        message_id: String,
    },
    Issue {
        issue_ref_id: String,
    },
    /// Short quoted text the user chose to carry.
    Note {
        text: String,
    },
}

/// One agent status report. Progress cannot overwrite engine-observed phase;
/// identical repeated reports are no-ops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardStatusReportRecord {
    pub report: BoardStatusReport,
    pub actor: BoardActor,
    pub execution_generation: u64,
    pub at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardStatusReport {
    pub kind: BoardStatusReportKind,
    /// Untrusted agent prose, bounded to 2,000 characters.
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<BoardEvidenceRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardStatusReportKind {
    Progress,
    Blocked,
    Unblocked,
    /// The implementer submits its current result for checks and review.
    ReadyForReview,
    Note,
}

/// A reference to evidence (check run, artifact, PR, file). Bounded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardEvidenceRef {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_id: Option<String>,
}

/// Observed execution phase: engine evidence only, separate from the column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardPhase {
    /// No working chat has been dispatched.
    #[default]
    NotStarted,
    /// Admitted intent waiting for capacity/dependency/checkout (see reason).
    Queued,
    Starting,
    Working,
    Checking,
    Reviewing,
    Fixing,
    /// Worker exists and is idle without a completed result (e.g. after a
    /// failed or interrupted turn). Not completion.
    Idle,
    /// Ready requested while the previous generation is still stopping.
    WaitingForStop,
    StopRequested,
    Stopping,
    Stopped,
    Failed,
    Complete,
    /// Done, while exact reviewer close/archive is still pending.
    CleanupPending,
    /// The owner/runtime cannot be observed (disconnected host, reconciling
    /// an uncertain delivery). Never inferred as stopped or complete.
    Unknown,
}

impl BoardPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotStarted => "Not started",
            Self::Queued => "Queued",
            Self::Starting => "Starting",
            Self::Working => "Working",
            Self::Checking => "Checking",
            Self::Reviewing => "Reviewing",
            Self::Fixing => "Fixing",
            Self::Idle => "Idle",
            Self::WaitingForStop => "Waiting for stop",
            Self::StopRequested => "Stop requested",
            Self::Stopping => "Stopping",
            Self::Stopped => "Stopped",
            Self::Failed => "Failed",
            Self::Complete => "Complete",
            Self::CleanupPending => "Cleanup pending",
            Self::Unknown => "Unknown",
        }
    }
}

/// One reason a ticket needs attention. Routes to the existing engine-owned
/// attention item where one exists; acknowledging is not resolving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardAttentionReason {
    pub kind: BoardAttentionKind,
    pub detail: String,
    /// The engine attention item to open ([`crate::AttentionItem`] id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardAttentionKind {
    Question,
    Approval,
    ProviderFailure,
    AuthRequired,
    HostUnavailable,
    CompatibilityBlocked,
    SettingsUnresolved,
    ModelUnavailable,
    /// Blocking findings remain after the repair cap.
    ReviewBlocked,
    ReviewUnavailable,
    /// Stop could not be verified.
    StopUnknown,
    /// First delivery or a launch outcome is uncertain and reconciling.
    DeliveryUnknown,
    DependencyBlocked,
    CleanupFailed,
    /// Agent-reported blocker.
    AgentBlocked,
    Other,
}

/// Actions the caller may currently take, computed by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardAction {
    Delete,
    Edit,
    Start,
    Continue,
    Stop,
    MoveToReady,
    MoveToBacklog,
    Reopen,
    Archive,
    Unarchive,
    GoToChat,
    SendUpdate,
    Retry,
    Review,
}

/// The ticket's current primary working chat as Board shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingChatSummary {
    pub task_id: String,
    pub role: BoardChatRole,
    /// Main parent for a side worker (its fixed original parent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_parent_title: Option<String>,
    /// False when the original parent was archived/deleted/unavailable; the
    /// worker stays navigable directly.
    pub parent_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// One ticket plus its engine-derived read model. This is what Board,
/// chats and agents render; nothing here is client-computed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemView {
    pub item: WorkItem,
    pub phase: BoardPhase,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attention: Vec<BoardAttentionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_chat: Option<WorkingChatSummary>,
    /// Admission/queue reason while [`BoardPhase::Queued`] or waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_reason: Option<super::BoardAdmissionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<super::IssueReferenceSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    /// Last meaningful activity time (event or observation).
    pub last_activity_ms: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<BoardAction>,
}

/// Everything the detail drawer shows. Lists are bounded and newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardItemDetail {
    pub view: WorkItemView,
    #[serde(default)]
    pub links: Vec<super::WorkItemTaskLink>,
    #[serde(default)]
    pub issues: Vec<super::IssueReference>,
    #[serde(default)]
    pub launches: Vec<super::LaunchSnapshot>,
    #[serde(default)]
    pub outcomes: Vec<super::OutcomeReference>,
    #[serde(default)]
    pub review_runs: Vec<super::ReviewRun>,
    #[serde(default)]
    pub dependencies: Vec<super::BoardDependency>,
    /// Tickets this one blocks (the other end of `dependencies`).
    #[serde(default)]
    pub dependents: Vec<super::BoardDependency>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<super::BoardExecutionIntent>,
    #[serde(default)]
    pub recent_events: Vec<super::WorkItemEvent>,
}

/// Per-project board summary (prefix and counts) for selectors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardProjectSummary {
    pub project_id: String,
    pub owner_host_id: String,
    /// Allocated on the project's first ticket; `None` before that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_prefix: Option<String>,
    pub backlog: u64,
    pub ready: u64,
    pub done: u64,
    pub archived: u64,
}
