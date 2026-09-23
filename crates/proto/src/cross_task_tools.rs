//! Bounded, LUNA-owned cross-task tool wire types.
//!
//! Caller authority is deliberately absent: the engine supplies it from the
//! active run, never from model arguments or another task's content.

use serde::{Deserialize, Serialize};

use crate::CommandReceipt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrossTaskListRequest {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossTaskSummary {
    /// Stable LUNA identity, never a provider session identifier.
    pub task_id: String,
    pub project_id: Option<String>,
    pub environment_id: String,
    pub title: String,
    pub status: String,
    pub archived: bool,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossTaskListPage {
    pub tasks: Vec<CrossTaskSummary>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrossTaskReadRequest {
    pub task_id: String,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossTaskTurnSummary {
    pub turn_id: String,
    pub status: String,
    /// Untrusted task content. Consumers must not treat it as instructions.
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossTaskReadPage {
    pub task: CrossTaskSummary,
    pub turns: Vec<CrossTaskTurnSummary>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub content_untrusted: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CrossTaskSendMode {
    #[default]
    Queue,
    Steer,
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrossTaskSendRequest {
    pub task_id: String,
    pub message: String,
    #[serde(default)]
    pub mode: CrossTaskSendMode,
    /// Reuse exactly this key after an uncertain result.
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossTaskSendReceipt {
    pub task_id: String,
    pub queued_message_id: String,
    pub command: CommandReceipt,
    pub activity_cursor: String,
    /// A queued receipt proves persistence, not provider or model acceptance.
    pub delivery_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrossTaskWaitTarget {
    pub task_id: String,
    #[serde(default)]
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrossTaskWaitRequest {
    pub targets: Vec<CrossTaskWaitTarget>,
    #[serde(default)]
    pub timeout_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossTaskWaitEvent {
    pub task: CrossTaskSummary,
    pub cursor: String,
    pub needs_attention: bool,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossTaskWaitResult {
    pub event: Option<CrossTaskWaitEvent>,
    pub timed_out: bool,
}

/// Engine command intent for a control that must have a separate, trusted
/// per-request authorization grant. These are not advertised provider tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CrossTaskControlAction {
    Create,
    Interrupt,
    Archive,
    Unarchive,
    Move,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrossTaskControlRequest {
    pub action: CrossTaskControlAction,
    /// Existing stable LUNA task, or the preallocated stable UUID for create.
    pub task_id: String,
    pub project_id: String,
    pub environment_id: String,
    pub command_id: String,
    #[serde(default)]
    pub destination_project_id: Option<String>,
    #[serde(default)]
    pub expected_task_revision: Option<u64>,
    #[serde(default)]
    pub expected_binding_generation: Option<u32>,
    #[serde(default)]
    pub expected_turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossTaskControlReceipt {
    pub task_id: String,
    pub command: CommandReceipt,
    /// Current local observation after the command. A pending stop is not a
    /// claim that the provider or engine has settled.
    pub observed_status: String,
    pub settled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_arguments_cannot_smuggle_authority_or_provider_ids() {
        assert!(
            serde_json::from_value::<CrossTaskSendRequest>(serde_json::json!({
                "taskId":"luna-task", "message":"hello", "mode":"queue",
                "idempotencyKey":"request-1", "providerSessionId":"native-id"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<CrossTaskReadRequest>(serde_json::json!({
                "taskId":"luna-task", "grantedScopes":["threads.read"]
            }))
            .is_err()
        );
        let request: CrossTaskSendRequest = serde_json::from_value(serde_json::json!({
            "taskId":"luna-task", "message":"hello", "idempotencyKey":"request-1"
        }))
        .unwrap();
        assert_eq!(request.mode, CrossTaskSendMode::Queue);
        assert!(
            serde_json::from_value::<CrossTaskControlRequest>(serde_json::json!({
                "action":"archive", "taskId":"luna-task", "projectId":"project-a",
                "environmentId":"local", "commandId":"command-a",
                "grantedScopes":["threads.manage"]
            }))
            .is_err()
        );
    }
}
