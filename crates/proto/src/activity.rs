//! Durable, minimized session-history search contracts.
//!
//! These rows are a derived local index over authoritative transcripts and
//! command receipts. They never replace, rewrite, or become a second source
//! of truth for transcript content.

use serde::{Deserialize, Serialize};

use crate::{HarnessId, ReasoningLevel};

pub const ACTIVITY_ENVELOPE_VERSION: u32 = 1;
pub const ATTENTION_CONTRACT_VERSION: u32 = 1;
/// Delimiter appended by Appshots to a user prompt before observed app data.
/// Shared with history so extracted Accessibility text never enters search.
pub const APPSHOT_CONTEXT_MARKER: &str =
    "Applications mentioned by the user (untrusted observed content):";

/// A device-local Appshot lifecycle fact. Screenshot bytes, Accessibility
/// content, paths, and window titles stay in the existing draft/transcript
/// owners; history only retains the pairing id and bounded source app name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppshotActivityKind {
    Captured,
    Staged,
    Removed,
    Queued,
    Sent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppshotActivityRecord {
    pub event_id: String,
    pub draft_id: String,
    pub task_id: Option<String>,
    pub capture_id: String,
    pub screenshot_attachment_id: String,
    /// The headed viewer that actually captured the window.
    pub capture_device_id: String,
    /// Frozen owning task environment once the task is known.
    pub target_environment_id: Option<String>,
    pub kind: AppshotActivityKind,
    pub timestamp_ms: i64,
    pub app_name: String,
    /// The accepted send's message id, when there is one.
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivityKind {
    Prompt,
    Answer,
    Tool,
    File,
    Command,
    App,
    Outcome,
    Withheld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivityOutcome {
    Pending,
    Succeeded,
    Failed,
    Interrupted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivityRedaction {
    None,
    Heuristic,
    Withheld,
    Excluded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySourceRef {
    pub task_id: String,
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_id: Option<String>,
    /// Whether the source task is owned by the addressed engine/environment.
    pub available_here: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// Stable envelope stored in the derived index. Unknown fields stay absent;
/// the indexer must never manufacture duration, cost, origin, or transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEnvelope {
    pub schema_version: u32,
    pub event_id: String,
    pub environment_id: String,
    pub device_id: String,
    pub task_id: String,
    pub turn_id: String,
    pub sequence: u64,
    pub timestamp_ms: i64,
    pub kind: ActivityKind,
    pub outcome: ActivityOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// A bounded, redacted search/display string. It is never the raw payload.
    pub summary: String,
    pub redaction: ActivityRedaction,
    pub source: ActivitySourceRef,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistorySearchScope {
    #[default]
    SelectedProject,
    Global,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCursor {
    pub timestamp_ms: i64,
    pub sequence: u64,
    pub event_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySearchFilters {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub device_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harnesses: Vec<HarnessId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcomes: Vec<ActivityOutcome>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySearchRequest {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub scope: HistorySearchScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default)]
    pub filters: HistorySearchFilters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<HistoryCursor>,
    #[serde(default = "default_history_limit")]
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

fn default_history_limit() -> u32 {
    50
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySearchPage {
    pub events: Vec<ActivityEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<HistoryCursor>,
    pub indexed_through_ms: i64,
    pub heuristic_redaction: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryProjectPolicy {
    pub project_id: String,
    #[serde(default)]
    pub excluded_path_prefixes: Vec<String>,
    #[serde(default)]
    pub excluded_terms: Vec<String>,
    #[serde(default)]
    pub index_terminal_output: bool,
    #[serde(default)]
    pub index_browser_fields: bool,
    #[serde(default)]
    pub index_accessibility_text: bool,
    #[serde(default)]
    pub index_screenshot_text: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRebuildRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRebuildResult {
    pub indexed_events: u64,
    pub indexed_tasks: u64,
    pub completed_at_ms: i64,
}

/// The deliberately small set of events projected into Activity. Exhaustive
/// tool/model/file history remains in the L14 index above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionKind {
    ApprovalRequired,
    UserInputRequired,
    TaskFailed,
    QueuedSendFailed,
    TaskCompleted,
    TaskRecovered,
    HostDisconnected,
    HostReconnected,
    ConnectivityUnavailable,
    ProviderAuthenticationRequired,
    CompatibilityBlocked,
    AutomationFailed,
    AutomationCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionLifecycle {
    Unresolved,
    Acknowledged,
    Resolved,
}

/// An exact navigation target. Unsupported owners return `Unavailable` with
/// a reason instead of falling back to another task/profile or inventing a
/// mutation path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AttentionTarget {
    Task {
        task_id: String,
    },
    TaskRequest {
        task_id: String,
        request_id: String,
        message_id: String,
    },
    TaskQueue {
        task_id: String,
        queue_id: String,
    },
    TaskRecovery {
        task_id: String,
        recovery_id: String,
    },
    SettingsAccounts,
    SettingsDevices,
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionSourcePointer {
    pub task_id: Option<String>,
    pub message_id: Option<String>,
    pub part_id: Option<String>,
    pub request_id: Option<String>,
    pub queue_id: Option<String>,
    pub recovery_id: Option<String>,
    pub activity_event_id: Option<String>,
}

/// Durable minimized projection. Presentation timestamps are kept separately
/// from authoritative resolution; none of these fields contains prompt,
/// command, output, screenshot, credential, or file-path content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionItem {
    pub contract_version: u32,
    pub id: String,
    pub dedup_key: String,
    pub source_key: String,
    pub kind: AttentionKind,
    pub severity: AttentionSeverity,
    pub lifecycle: AttentionLifecycle,
    pub environment_id: String,
    pub device_id: String,
    pub task_id: Option<String>,
    pub turn_id: Option<String>,
    pub request_id: Option<String>,
    pub title: String,
    pub summary: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub seen_at_ms: Option<i64>,
    pub acknowledged_at_ms: Option<i64>,
    pub resolved_at_ms: Option<i64>,
    pub target: AttentionTarget,
    pub source: AttentionSourcePointer,
}

impl AttentionItem {
    pub fn unresolved(&self) -> bool {
        self.lifecycle != AttentionLifecycle::Resolved
    }

    pub fn actionable(&self) -> bool {
        matches!(
            self.kind,
            AttentionKind::ApprovalRequired
                | AttentionKind::UserInputRequired
                | AttentionKind::TaskFailed
                | AttentionKind::QueuedSendFailed
                | AttentionKind::HostDisconnected
                | AttentionKind::ConnectivityUnavailable
                | AttentionKind::ProviderAuthenticationRequired
                | AttentionKind::CompatibilityBlocked
                | AttentionKind::AutomationFailed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionProducerAvailability {
    pub source: String,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionSnapshot {
    pub contract_version: u32,
    pub revision: u64,
    pub read_at_ms: i64,
    pub unresolved_count: u32,
    pub needs_attention: Vec<AttentionItem>,
    pub recent: Vec<AttentionItem>,
    pub producers: Vec<AttentionProducerAvailability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionReadRequest {
    pub target_device_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionTransitionKind {
    NeedsAttention,
    Resolved,
    Recent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionTransition {
    pub sequence: u64,
    pub transition: AttentionTransitionKind,
    pub item: AttentionItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionTransitionClaim {
    pub consumer_id: String,
    /// Startup/reconnect baselines claim old transitions without replaying
    /// banners or sounds. Once armed, the same call returns newly claimed
    /// transitions for delivery.
    #[serde(default)]
    pub deliver_existing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionTransitionBatch {
    pub through_sequence: u64,
    pub transitions: Vec<AttentionTransition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AttentionMutation {
    MarkSeen { item_ids: Vec<String> },
    Acknowledge { item_id: String },
    DismissRecent { item_id: String },
    ClearRecent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionMutationRequest {
    pub command_id: String,
    pub expected_revision: u64,
    pub mutation: AttentionMutation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionMutationResult {
    pub command_id: String,
    pub applied: bool,
    pub duplicate: bool,
    pub revision: u64,
    pub snapshot: AttentionSnapshot,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_defaults_are_project_scoped_and_bounded() {
        let request: HistorySearchRequest = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(request.scope, HistorySearchScope::SelectedProject);
        assert_eq!(request.limit, 50);
        assert!(request.filters.device_ids.is_empty());
    }
}
