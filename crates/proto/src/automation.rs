//! Host-owned schedules. Provider-native jobs are a separate observation surface.
use crate::{ChatConfig, PermissionMode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AutomationTarget {
    Existing { task_id: String },
    NewConversation { origin_task_id: String },
}
impl AutomationTarget {
    pub fn origin_task_id(&self) -> &str {
        match self {
            Self::Existing { task_id } => task_id,
            Self::NewConversation { origin_task_id } => origin_task_id,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AutomationTrigger {
    Once {
        at_ms: i64,
    },
    Interval {
        anchor_ms: i64,
        every_minutes: u32,
    },
    /// ISO weekdays 1=Monday .. 7=Sunday. Empty means every day.
    Calendar {
        hour: i8,
        minute: i8,
        weekdays: Vec<i8>,
    },
    PrWatch {
        repository: String,
        number: u64,
        poll_minutes: u32,
        wake_budget: u32,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationDefinition {
    pub name: String,
    pub prompt: String,
    pub timezone: String,
    pub target: AutomationTarget,
    pub trigger: AutomationTrigger,
    pub max_lateness_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationAuthority {
    pub origin_task_id: String,
    pub project_id: String,
    pub cwd: String,
    pub checkout_id: Option<String>,
    pub workspace_repair_revision: u64,
    pub config: ChatConfig,
    pub permission_ceiling: PermissionMode,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrWatchCursor {
    pub head_oid: String,
    pub checks_digest: String,
    pub remark_ids: Vec<String>,
    pub wake_cursor: String,
    pub wakes_used: u32,
    pub failures: u32,
    pub last_error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRecord {
    pub id: String,
    pub owner_host_id: String,
    pub revision: u64,
    pub definition: AutomationDefinition,
    pub authority: AutomationAuthority,
    pub enabled: bool,
    pub deleted: bool,
    pub next_due_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub watch: Option<PrWatchCursor>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AutomationOperation {
    Save {
        definition: AutomationDefinition,
    },
    Pause,
    Resume,
    Delete,
    /// Cancel only a claimed occurrence that has not crossed dispatch admission.
    CancelOccurrence {
        occurrence_id: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationMutationRequest {
    pub request_id: String,
    pub automation_id: String,
    pub owner_host_id: String,
    pub expected_revision: u64,
    pub operation: AutomationOperation,
    #[serde(default)]
    pub preview_id: String,
}
/// Provider arguments never carry host identity, permission grants or a preview token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationToolRequest {
    pub automation_id: String,
    pub expected_revision: u64,
    pub idempotency_key: String,
    pub operation: AutomationOperation,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationPreview {
    pub preview_id: String,
    pub expires_at_ms: i64,
    pub next_occurrences_ms: Vec<i64>,
    pub owner_host_id: String,
    pub permission_ceiling: PermissionMode,
    pub description: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationMutationReceipt {
    pub request_id: String,
    pub payload_hash: String,
    pub record: AutomationRecord,
    pub committed_at_ms: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutomationRunState {
    Claimed,
    Dispatching,
    Accepted,
    Completed,
    Failed,
    Unknown,
    Skipped,
    Cancelled,
}
impl AutomationRunState {
    pub fn blocks_next(self) -> bool {
        matches!(
            self,
            Self::Claimed | Self::Dispatching | Self::Accepted | Self::Unknown
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRun {
    pub id: String,
    pub automation_id: String,
    pub owner_host_id: String,
    pub automation_revision: u64,
    pub scheduled_at_ms: i64,
    pub claimed_at_ms: i64,
    pub updated_at_ms: i64,
    pub coalesced: u64,
    pub task_id: String,
    pub command_id: String,
    pub message_id: String,
    pub state: AutomationRunState,
    pub detail: String,
    pub boot_id: String,
}
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationReadRequest {
    #[serde(default)]
    pub automation_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub after_id: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub include_deleted: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationPage<T> {
    pub items: Vec<T>,
    pub next_after_id: Option<String>,
}
