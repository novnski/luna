//! The ticket event journal: exactly one canonical event per accepted
//! mutation, committed in the same transaction. Replays of an idempotent
//! request and no-op repeats (an identical status report) add none.
use serde::{Deserialize, Serialize};

use super::{
    BoardActor, BoardAttentionKind, BoardColumn, BoardInstructionKind, BoardPhase, BoardScope,
    BoardStatusReportKind, OutcomeKind, ReviewRunState, ReviewVerdict, TaskLinkRole,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemEvent {
    /// Global, gap-tolerant journal cursor assigned at commit.
    pub seq: u64,
    pub id: String,
    pub work_item_id: String,
    pub project_id: String,
    /// The ticket's revision after this event (unchanged for rank-only
    /// events).
    pub revision: u64,
    pub actor: BoardActor,
    /// The normalized request that produced it.
    pub request_id: String,
    pub at_ms: i64,
    /// Shown in Activity/search. Rank-only and bookkeeping events are not.
    pub activity: bool,
    pub kind: WorkItemEventKind,
}

/// Every slice's event kinds, defined up front so the journal stays one
/// canonical stream. Unknown future kinds decode as [`Self::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WorkItemEventKind {
    // ---- store (PB-0) ----
    Created {
        key: String,
        column: BoardColumn,
    },
    /// Field-level edit. `fields` uses the camelCase field names.
    Updated {
        fields: Vec<String>,
        brief_revision: u32,
    },
    Reordered {
        column: BoardColumn,
        rank: String,
    },
    Archived,
    Unarchived,
    Deleted,
    // ---- lifecycle (PB-1) ----
    Moved {
        from: BoardColumn,
        to: BoardColumn,
        intent_revision: u64,
    },
    /// Done reached from verified current evidence (engine-owned).
    Completed {
        execution_generation: u64,
    },
    Reopened {
        from: BoardColumn,
    },
    LaunchAccepted {
        launch_snapshot_id: String,
        instruction: BoardInstructionKind,
        execution_generation: u64,
        task_id: String,
    },
    LaunchQueued {
        reason: String,
    },
    LaunchUnknown {
        launch_request_id: String,
    },
    StopRequested {
        execution_generation: u64,
    },
    StopSettled {
        execution_generation: u64,
        stopped: bool,
    },
    StatusReported {
        report: BoardStatusReportKind,
        execution_generation: u64,
    },
    BriefSent {
        brief_revision: u32,
        task_id: String,
    },
    ResultDelivered {
        recipient_task_id: String,
        execution_generation: u64,
    },
    AttentionRaised {
        reason: BoardAttentionKind,
    },
    // ---- chat model ----
    ChatLinked {
        task_id: String,
        role: TaskLinkRole,
    },
    ChatUnlinked {
        task_id: String,
        role: TaskLinkRole,
    },
    Converted {
        task_id: String,
    },
    Renamed {
        title: String,
        task_id: Option<String>,
    },
    // ---- review (PB-1R) ----
    ReviewStarted {
        review_run_id: String,
        round: u8,
    },
    ReviewStateChanged {
        review_run_id: String,
        state: ReviewRunState,
    },
    ReviewResult {
        review_run_id: String,
        verdict: ReviewVerdict,
        blocking_findings: u32,
    },
    ReviewerCleaned {
        review_run_id: String,
        archived: bool,
    },
    OutcomeRecorded {
        outcome_id: String,
        outcome: OutcomeKind,
    },
    // ---- external issues and dependencies (PB-2/PB-3) ----
    IssueLinked {
        issue_ref_id: String,
    },
    IssueUnlinked {
        issue_ref_id: String,
    },
    IssueSourceChanged {
        issue_ref_id: String,
    },
    DependencyAdded {
        blocked_by_id: String,
    },
    DependencyRemoved {
        blocked_by_id: String,
    },
    IssueWriteback {
        issue_ref_id: String,
        outcome_id: String,
    },
    /// A newer event kind this build does not know.
    #[serde(other)]
    Unknown,
}

impl WorkItemEventKind {
    /// Stable short name stored in the indexed `kind` column.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Created { .. } => "created",
            Self::Updated { .. } => "updated",
            Self::Reordered { .. } => "reordered",
            Self::Archived => "archived",
            Self::Unarchived => "unarchived",
            Self::Deleted => "deleted",
            Self::Moved { .. } => "moved",
            Self::Completed { .. } => "completed",
            Self::Reopened { .. } => "reopened",
            Self::LaunchAccepted { .. } => "launchAccepted",
            Self::LaunchQueued { .. } => "launchQueued",
            Self::LaunchUnknown { .. } => "launchUnknown",
            Self::StopRequested { .. } => "stopRequested",
            Self::StopSettled { .. } => "stopSettled",
            Self::StatusReported { .. } => "statusReported",
            Self::BriefSent { .. } => "briefSent",
            Self::ResultDelivered { .. } => "resultDelivered",
            Self::AttentionRaised { .. } => "attentionRaised",
            Self::ChatLinked { .. } => "chatLinked",
            Self::ChatUnlinked { .. } => "chatUnlinked",
            Self::Converted { .. } => "converted",
            Self::Renamed { .. } => "renamed",
            Self::ReviewStarted { .. } => "reviewStarted",
            Self::ReviewStateChanged { .. } => "reviewStateChanged",
            Self::ReviewResult { .. } => "reviewResult",
            Self::ReviewerCleaned { .. } => "reviewerCleaned",
            Self::OutcomeRecorded { .. } => "outcomeRecorded",
            Self::IssueLinked { .. } => "issueLinked",
            Self::IssueUnlinked { .. } => "issueUnlinked",
            Self::IssueSourceChanged { .. } => "issueSourceChanged",
            Self::DependencyAdded { .. } => "dependencyAdded",
            Self::DependencyRemoved { .. } => "dependencyRemoved",
            Self::IssueWriteback { .. } => "issueWriteback",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this kind belongs in Activity/search (default for the kind;
    /// the store records the decision on each event).
    pub fn is_activity(&self) -> bool {
        !matches!(self, Self::Reordered { .. } | Self::Unknown)
    }
}

/// Read the journal (bounded, oldest first after the cursor).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardEventsRequest {
    #[serde(default)]
    pub scope: BoardScope,
    #[serde(default)]
    pub work_item_id: Option<String>,
    #[serde(default)]
    pub after_seq: Option<u64>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// Only events with `activity == true`.
    #[serde(default)]
    pub activity_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardEventPage {
    pub events: Vec<WorkItemEvent>,
    /// Highest seq examined; pass as `after_seq` to continue.
    pub cursor: u64,
    pub has_more: bool,
}

/// Subscribe to board changes for a scope. The first frame always arrives
/// immediately with the current journal head.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardWatchRequest {
    #[serde(default)]
    pub scope: BoardScope,
    /// Resume after this journal cursor (typically a list's `asOfSeq`).
    #[serde(default)]
    pub after_seq: Option<u64>,
}

/// One watch frame. Clients reload affected items (or the list on `reset`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardWatchFrame {
    /// Journal head after this frame.
    pub seq: u64,
    /// New in-scope events since the previous frame (bounded).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<WorkItemEvent>,
    /// Items whose engine observation (phase/attention) changed without a
    /// journal event; reload their views.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed: Vec<String>,
    /// Too many changes (or an unknown cursor): reload the whole list.
    #[serde(default)]
    pub reset: bool,
}

/// An observation change pushed by the lifecycle/review slices (not
/// journaled).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardObservation {
    pub work_item_id: String,
    pub phase: BoardPhase,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attention: Vec<super::BoardAttentionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_reason: Option<super::BoardAdmissionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_chat: Option<super::WorkingChatSummary>,
    pub observed_at_ms: i64,
}
