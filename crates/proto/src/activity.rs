//! Durable, minimized session-history search contracts.
//!
//! These rows are a derived local index over authoritative transcripts and
//! command receipts. They never replace, rewrite, or become a second source
//! of truth for transcript content.

use serde::{Deserialize, Serialize};

use crate::{HarnessId, ReasoningLevel};

pub const ACTIVITY_ENVELOPE_VERSION: u32 = 1;

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
