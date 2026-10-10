//! Host-owned schedules. Provider-native jobs are a separate observation surface.
use crate::{ChatConfig, HarnessId, PermissionMode, ReasoningLevel};
use serde::{Deserialize, Serialize};

/// Invalidate cached automation definitions and history after an owner commit.
/// Initial snapshot is emitted on subscribe. Sequence resets with the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationWatchFrame {
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AutomationTarget {
    Existing {
        task_id: String,
    },
    NewConversation {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin_task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
    },
}
impl AutomationTarget {
    pub fn origin_task_id(&self) -> Option<&str> {
        match self {
            Self::Existing { task_id } => Some(task_id),
            Self::NewConversation { origin_task_id, .. } => origin_task_id.as_deref(),
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
    /// A bounded RFC 5545 calendar-rule subset; all local times use definition.timezone.
    RRule {
        rule: String,
        anchor_ms: i64,
    },
    PrWatch {
        repository: String,
        number: u64,
        poll_minutes: u32,
        wake_budget: u32,
    },
}

/// Saved execution selection. Access remains bounded by the approved authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationExecution {
    pub harness: HarnessId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_account_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning: Option<ReasoningLevel>,
    #[serde(default)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
}
impl Default for AutomationExecution {
    fn default() -> Self {
        Self {
            harness: HarnessId::Codex,
            provider_account_id: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
        }
    }
}
impl From<&ChatConfig> for AutomationExecution {
    fn from(config: &ChatConfig) -> Self {
        Self {
            harness: config.harness,
            provider_account_id: config.provider_account_id.clone(),
            model: config.model.clone(),
            reasoning: config.reasoning,
            model_options: config.model_options.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutomationNotificationPolicy {
    AllRuns,
    #[default]
    ImportantUpdates,
    Never,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<AutomationExecution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_task_id: Option<String>,
    #[serde(default)]
    pub notification_policy: AutomationNotificationPolicy,
    #[serde(default)]
    pub archive_successful_runs: bool,
}
impl Default for AutomationDefinition {
    fn default() -> Self {
        Self {
            name: String::new(),
            prompt: String::new(),
            timezone: "UTC".into(),
            target: AutomationTarget::NewConversation {
                origin_task_id: None,
                project_id: None,
            },
            trigger: AutomationTrigger::Once { at_ms: 0 },
            max_lateness_ms: 600_000,
            execution: None,
            source_task_id: None,
            notification_policy: Default::default(),
            archive_successful_runs: false,
        }
    }
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
    #[serde(default)]
    pub completed: bool,
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
    /// Acknowledge an uncertain old outcome without ever replaying its message.
    ResolveUnknown {
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
    /// Host-derived cap for agent proposals. The provider-facing tool omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_ceiling: Option<PermissionMode>,
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
    Resolved,
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
    #[serde(default)]
    pub automation_name: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub notification_policy: AutomationNotificationPolicy,
    /// Durable terminal side-effect checkpoint; never a delivery confirmation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized_at_ms: Option<i64>,
}
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationReadRequest {
    #[serde(default)]
    pub automation_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
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
