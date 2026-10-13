//! PB-4A feature groups (docs/pending-tasks/devin-project-board/pb-4a/README.md).
//!
//! A feature group is a local Board record owned by one home project
//! ([`crate::Space`]) on its owning host. It names existing tickets as
//! members, including tickets of other projects on the same host (related
//! repositories) that the user linked explicitly, and carries its own
//! acceptance: shared criteria, optional integration gates and one explicit
//! Accept. Members completing never accept the group, and the group never
//! writes a member ticket, creates a chat, launch, review or worktree, or
//! grants access to another project. Every member keeps its own project,
//! checkout, worker and reviewer; the read model shows them as their owners
//! report them.
//!
//! Like the other board records, additions are `#[serde(default)]` only:
//! receipts and payload hashes are compared byte-for-byte.
use serde::{Deserialize, Serialize};

use super::{
    BoardActor, BoardAttentionKind, BoardCheckoutChoice, BoardColumn, BoardEvidenceRef, BoardPhase,
    BoardScope, BoardSourceKind, IssueReferenceSummary, ReviewRunState, ReviewVerdict,
    WorkingChatSummary,
};

/// Version stamped on every stored feature group.
pub const FEATURE_GROUP_SCHEMA_VERSION: u32 = 1;
/// Groups one host keeps (archived ones included).
pub const FEATURE_GROUPS_MAX: usize = 100;
pub const FEATURE_GROUP_TITLE_MAX_CHARS: usize = 200;
/// The shared acceptance criteria (Markdown).
pub const FEATURE_GROUP_ACCEPTANCE_MAX_BYTES: usize = 16 * 1024;
pub const FEATURE_GROUP_MEMBERS_MAX: usize = 30;
pub const FEATURE_GROUP_GATES_MAX: usize = 12;
pub const FEATURE_GROUP_GATE_LABEL_MAX_CHARS: usize = 120;
pub const FEATURE_GROUP_NOTE_MAX_CHARS: usize = 2_000;
pub const FEATURE_GROUP_EVIDENCE_MAX: usize = 8;
/// History entries one group keeps (oldest dropped first).
pub const FEATURE_GROUP_HISTORY_MAX: usize = 50;

/// One feature group. The home project's owning host is its only writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroup {
    pub schema_version: u32,
    /// Stable app-owned id (`fg-…`).
    pub id: String,
    /// The home project; fixed at creation. Never a new project identity.
    pub project_id: String,
    pub owner_host_id: String,
    pub title: String,
    /// Shared acceptance criteria (Markdown). Recorded, never executed.
    #[serde(default)]
    pub acceptance: String,
    /// Integration gates: named checks whose results the user records
    /// against the current members.
    #[serde(default)]
    pub gates: Vec<FeatureGroupGate>,
    #[serde(default)]
    pub members: Vec<FeatureGroupMember>,
    /// The explicit acceptance, when recorded. Members completing never set
    /// it; it counts only while its basis is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_record: Option<FeatureGroupAcceptance>,
    #[serde(default)]
    pub archived: bool,
    /// Monotonic record revision (1 on create); every change increments it.
    pub revision: u64,
    /// Gate ids are allocated once (`g1`, `g2`, …) and never reused.
    #[serde(default)]
    pub next_gate: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub created_by: BoardActor,
    pub updated_by: BoardActor,
    /// Bounded, oldest first.
    #[serde(default)]
    pub history: Vec<FeatureGroupHistoryEntry>,
}

/// A linked ticket. Linking writes nothing to the ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupMember {
    pub work_item_id: String,
    /// The ticket's own project (its repository). Fixed with the ticket.
    pub project_id: String,
    /// The ticket's key when linked (shown when the ticket is gone).
    pub key: String,
    pub added_by: BoardActor,
    pub added_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupGate {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<FeatureGroupGateResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeatureGroupGateState {
    Passed,
    Failed,
}

/// A recorded gate result, bound to the members' state when recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupGateResult {
    pub state: FeatureGroupGateState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<BoardEvidenceRef>,
    pub basis: Vec<FeatureGroupBasisEntry>,
    pub recorded_by: BoardActor,
    pub recorded_at_ms: i64,
}

/// One member's state in a basis. A gate result or acceptance counts only
/// while the current basis equals the recorded one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupBasisEntry {
    pub work_item_id: String,
    /// `None`: the ticket was deleted or is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<BoardColumn>,
    #[serde(default)]
    pub execution_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupAcceptance {
    pub basis: Vec<FeatureGroupBasisEntry>,
    pub accepted_by: BoardActor,
    pub accepted_at_ms: i64,
    /// The group revision this acceptance committed.
    pub group_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupHistoryEntry {
    pub revision: u64,
    pub kind: FeatureGroupHistoryKind,
    pub actor: BoardActor,
    pub at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FeatureGroupHistoryKind {
    Created,
    Updated {
        fields: Vec<String>,
    },
    MemberAdded {
        work_item_id: String,
        key: String,
        project_id: String,
    },
    MemberRemoved {
        work_item_id: String,
        key: String,
    },
    GateAdded {
        gate_id: String,
        label: String,
    },
    GateRemoved {
        gate_id: String,
    },
    GateRecorded {
        gate_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<FeatureGroupGateState>,
    },
    Accepted,
    Reopened,
    /// A change to the group's own contract cleared its acceptance.
    AcceptanceCleared {
        reason: String,
    },
    Archived,
    Unarchived,
}

// ---------------------------------------------------------------------------
// Requests and receipts
// ---------------------------------------------------------------------------

/// `BoardFeatureGroups`: groups of a scope. A project scope returns the
/// groups homed there and the groups that contain one of its tickets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardFeatureGroupsRequest {
    #[serde(default)]
    pub scope: BoardScope,
    #[serde(default)]
    pub group_id: Option<String>,
    /// Only groups that contain this ticket (id or key).
    #[serde(default)]
    pub work_item_id: Option<String>,
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardFeatureGroupPage {
    pub groups: Vec<FeatureGroupView>,
}

/// `BoardFeatureGroupMutate`: one group change with an idempotency key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardFeatureGroupMutateRequest {
    pub request_id: String,
    pub action: FeatureGroupAction,
}

/// Every action except `create` names the group and its expected revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum FeatureGroupAction {
    Create {
        project_id: String,
        title: String,
        #[serde(default)]
        acceptance: String,
        /// Gate labels.
        #[serde(default)]
        gates: Vec<String>,
        /// Ticket ids or keys to link.
        #[serde(default)]
        members: Vec<String>,
    },
    Update {
        group_id: String,
        expected_revision: u64,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        acceptance: Option<String>,
    },
    AddMember {
        group_id: String,
        expected_revision: u64,
        work_item_id: String,
    },
    RemoveMember {
        group_id: String,
        expected_revision: u64,
        work_item_id: String,
    },
    AddGate {
        group_id: String,
        expected_revision: u64,
        label: String,
    },
    RemoveGate {
        group_id: String,
        expected_revision: u64,
        gate_id: String,
    },
    /// `state: None` clears the result.
    RecordGate {
        group_id: String,
        expected_revision: u64,
        gate_id: String,
        #[serde(default)]
        state: Option<FeatureGroupGateState>,
        #[serde(default)]
        note: Option<String>,
        #[serde(default)]
        evidence: Vec<BoardEvidenceRef>,
    },
    Accept {
        group_id: String,
        expected_revision: u64,
    },
    Reopen {
        group_id: String,
        expected_revision: u64,
    },
    Archive {
        group_id: String,
        expected_revision: u64,
        archived: bool,
    },
    Delete {
        group_id: String,
        expected_revision: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeatureGroupActionKind {
    Create,
    Update,
    AddMember,
    RemoveMember,
    AddGate,
    RemoveGate,
    RecordGate,
    Accept,
    Reopen,
    Archive,
    Delete,
}

impl FeatureGroupAction {
    pub fn kind(&self) -> FeatureGroupActionKind {
        match self {
            Self::Create { .. } => FeatureGroupActionKind::Create,
            Self::Update { .. } => FeatureGroupActionKind::Update,
            Self::AddMember { .. } => FeatureGroupActionKind::AddMember,
            Self::RemoveMember { .. } => FeatureGroupActionKind::RemoveMember,
            Self::AddGate { .. } => FeatureGroupActionKind::AddGate,
            Self::RemoveGate { .. } => FeatureGroupActionKind::RemoveGate,
            Self::RecordGate { .. } => FeatureGroupActionKind::RecordGate,
            Self::Accept { .. } => FeatureGroupActionKind::Accept,
            Self::Reopen { .. } => FeatureGroupActionKind::Reopen,
            Self::Archive { .. } => FeatureGroupActionKind::Archive,
            Self::Delete { .. } => FeatureGroupActionKind::Delete,
        }
    }

    /// The target group and the caller's expected revision (`None` for
    /// `create`).
    pub fn target(&self) -> Option<(&str, u64)> {
        match self {
            Self::Create { .. } => None,
            Self::Update {
                group_id,
                expected_revision,
                ..
            }
            | Self::AddMember {
                group_id,
                expected_revision,
                ..
            }
            | Self::RemoveMember {
                group_id,
                expected_revision,
                ..
            }
            | Self::AddGate {
                group_id,
                expected_revision,
                ..
            }
            | Self::RemoveGate {
                group_id,
                expected_revision,
                ..
            }
            | Self::RecordGate {
                group_id,
                expected_revision,
                ..
            }
            | Self::Accept {
                group_id,
                expected_revision,
            }
            | Self::Reopen {
                group_id,
                expected_revision,
            }
            | Self::Archive {
                group_id,
                expected_revision,
                ..
            }
            | Self::Delete {
                group_id,
                expected_revision,
            } => Some((group_id.as_str(), *expected_revision)),
        }
    }
}

/// The durable result of one accepted group mutation. A retry with the
/// same request id and payload returns this exact receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupReceipt {
    /// Engine-normalized request id (caller scope + idempotency key).
    pub request_id: String,
    pub payload_hash: String,
    pub action: FeatureGroupActionKind,
    pub group_id: String,
    /// The group's revision after this mutation.
    pub revision: u64,
    /// `false`: the group was already in the requested state.
    pub changed: bool,
    #[serde(default)]
    pub deleted: bool,
    pub actor: BoardActor,
    pub committed_at_ms: i64,
}

/// A mutation's receipt and the group as it is now (`None` once deleted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupMutation {
    pub receipt: FeatureGroupReceipt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<FeatureGroupView>,
}

// ---------------------------------------------------------------------------
// Read model
// ---------------------------------------------------------------------------

/// One group plus its engine-derived aggregate. Nothing here is
/// client-computed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupView {
    pub group: FeatureGroup,
    /// The home project as it is now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    /// `false`: the requesting project scope sees this group because one
    /// of its tickets is a member (a related repository's view).
    pub home: bool,
    pub members: Vec<FeatureGroupMemberView>,
    pub gates: Vec<FeatureGroupGateView>,
    pub acceptance: FeatureGroupAcceptanceState,
    /// Why the group cannot be accepted now, in contract order.
    #[serde(default)]
    pub unmet: Vec<FeatureGroupUnmet>,
    pub can_accept: bool,
    pub summary: FeatureGroupSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeatureGroupAcceptanceState {
    NotAccepted,
    /// Accepted for the current members' state.
    Accepted,
    /// Accepted earlier; a member changed since (reopened, new generation,
    /// deleted). Not a current acceptance.
    Stale,
}

impl FeatureGroupAcceptanceState {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotAccepted => "Not accepted",
            Self::Accepted => "Accepted",
            Self::Stale => "Accepted · out of date",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupSummary {
    pub members: u32,
    pub done: u32,
    /// Distinct member projects.
    pub repositories: u32,
    pub gates: u32,
    pub gates_passed: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeatureGroupMemberAvailability {
    Available,
    /// Hidden from the default board; still a member.
    Archived,
    Deleted,
    Missing,
    /// Owned by another host (e.g. a restored profile).
    OtherHost,
}

impl FeatureGroupMemberAvailability {
    pub fn present(self) -> bool {
        matches!(self, Self::Available | Self::Archived)
    }
}

/// One member as its own owners report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupMemberView {
    pub work_item_id: String,
    pub key: String,
    /// The member's own project (never the group's home).
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    pub availability: FeatureGroupMemberAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<BoardColumn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<BoardPhase>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attention: Vec<BoardAttentionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<BoardSourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<IssueReferenceSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_chat: Option<WorkingChatSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<FeatureGroupCheckout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<FeatureGroupReviewSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<FeatureGroupBlocker>,
    #[serde(default)]
    pub execution_generation: u64,
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeatureGroupCheckoutSource {
    /// The ticket's working chat's checkout.
    WorkingChat,
    /// The ticket's latest launch snapshot.
    Launch,
    /// Not started: the ticket's project folder.
    Project,
}

/// Where a member's work runs, from its own records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupCheckout {
    pub source: FeatureGroupCheckoutSource,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
    /// The branch the chat or launch recorded (worktree chats).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_branch: Option<String>,
    /// The branch checked out at `cwd` when this view was read (`HEAD`
    /// when detached; `None` when not a readable Git checkout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_branch: Option<String>,
    /// The ticket's requested checkout (never created by the group).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested: Option<BoardCheckoutChoice>,
}

/// The member's latest review run, as its review owner records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupReviewSummary {
    pub review_run_id: String,
    pub state: ReviewRunState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ReviewVerdict>,
    pub execution_generation: u64,
    /// The run belongs to the ticket's current execution generation.
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupBlocker {
    pub work_item_id: String,
    pub key: String,
    /// The blocker is itself a member of this group.
    pub in_group: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeatureGroupGateStatus {
    Pending,
    Passed,
    Failed,
    /// Recorded for other members' state; needs recording again.
    Stale,
}

impl FeatureGroupGateStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Stale => "stale",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureGroupGateView {
    pub gate: FeatureGroupGate,
    pub status: FeatureGroupGateStatus,
}

/// One reason a group cannot be accepted now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FeatureGroupUnmet {
    NoMembers,
    MemberUnavailable {
        work_item_id: String,
        key: String,
        availability: FeatureGroupMemberAvailability,
    },
    MemberNotDone {
        work_item_id: String,
        key: String,
        column: BoardColumn,
        phase: BoardPhase,
    },
    MemberBlocked {
        work_item_id: String,
        key: String,
        /// Keys of its open blockers.
        blocked_by: Vec<String>,
    },
    GatePending {
        gate_id: String,
        label: String,
    },
    GateFailed {
        gate_id: String,
        label: String,
    },
    GateStale {
        gate_id: String,
        label: String,
    },
}

impl FeatureGroupUnmet {
    /// A short sentence for notices and refusals.
    pub fn label(&self) -> String {
        match self {
            Self::NoMembers => "the group has no tickets".into(),
            Self::MemberUnavailable {
                key, availability, ..
            } => match availability {
                FeatureGroupMemberAvailability::Deleted => format!("{key} was deleted"),
                FeatureGroupMemberAvailability::OtherHost => {
                    format!("{key} belongs to another host")
                }
                _ => format!("{key} is missing"),
            },
            Self::MemberNotDone {
                key, column, phase, ..
            } => format!("{key} is not Done ({} · {})", column.label(), phase.label()),
            Self::MemberBlocked {
                key, blocked_by, ..
            } => format!("{key} is blocked by {}", blocked_by.join(", ")),
            Self::GatePending { label, .. } => format!("gate “{label}” is pending"),
            Self::GateFailed { label, .. } => format!("gate “{label}” failed"),
            Self::GateStale { label, .. } => {
                format!("gate “{label}” was recorded before its tickets changed")
            }
        }
    }
}
