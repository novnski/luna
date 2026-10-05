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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<CrossTaskAgent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossTaskAgent {
    pub provider: crate::HarnessId,
    pub model: Option<String>,
    pub reasoning: Option<crate::ReasoningLevel>,
    pub lineage: crate::TaskLineage,
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

/// Fresh, visible same-project child. Parent authority is never a tool argument.
///
/// Every field added after the first version is optional and skipped when
/// absent, so older payloads decode and keep their durable payload hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateChildThreadRequest {
    pub prompt: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Absent: the parent's provider. Another provider starts a fresh session
    /// using its own account/defaults, never the parent's native binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<crate::HarnessId>,
    /// Absent: this provider's saved fresh-chat default, then catalog default.
    #[serde(default)]
    pub model: Option<String>,
    /// Absent: compatible saved provider reasoning, then the model default.
    #[serde(default)]
    pub reasoning: Option<crate::ReasoningLevel>,
    /// Provider-advertised option ID -> choice ID. Never internal runtime keys.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    /// Explicit existing directory. Only valid with `checkout` absent or
    /// `current`; absent means the parent's current checkout.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "child_reports_default")]
    pub report_results: bool,
    /// Explicit opt-in to queue a new parent turn after the child reports.
    #[serde(default)]
    pub continue_parent: bool,
    /// Where the child works. Absent: `cwd` when given, else the parent's
    /// current checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<ChildCheckoutRequest>,
    /// Requested access level. Absent: the parent's effective access. Access
    /// above the parent's ceiling needs an explicit user grant on the approval
    /// card and is capped unless the user picks the grant choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<crate::PermissionMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildModelCatalogRequest {
    #[serde(default)]
    pub provider: Option<crate::HarnessId>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "child_model_page_size")]
    pub limit: usize,
}
fn child_model_page_size() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildModelCatalog {
    pub provider: crate::HarnessId,
    pub models: Vec<crate::Model>,
    pub next_offset: Option<usize>,
}

/// Configured execution choice, not proof that the provider accepted startup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildExecutionSelection {
    pub provider: crate::HarnessId,
    pub model: Option<String>,
    pub reasoning: Option<crate::ReasoningLevel>,
    pub model_options: serde_json::Map<String, serde_json::Value>,
}
impl From<&crate::ChatConfig> for ChildExecutionSelection {
    fn from(config: &crate::ChatConfig) -> Self {
        Self {
            provider: config.harness,
            model: config.model.clone(),
            reasoning: config.reasoning,
            model_options: config.model_options.clone(),
        }
    }
}
fn child_reports_default() -> bool {
    true
}

/// Checkout choice for a new child chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ChildCheckoutRequest {
    /// Share the parent's current checkout (cwd, branch and worktree).
    Current,
    /// Materialize a fresh linked git worktree on a new branch for the child.
    #[serde(rename_all = "camelCase")]
    NewWorktree {
        /// New branch name. Absent: a generated `luna/child-…` branch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        /// Base ref. Absent: the parent's current branch, or its HEAD commit
        /// when the parent is detached.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildThreadReceipt {
    /// View placement is independent of durable conversation creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<serde_json::Value>,
    pub task_id: String,
    pub parent_task_id: String,
    pub command: CommandReceipt,
    pub initial_message_id: String,
    /// Durable intent/queue acceptance is not provider acceptance.
    pub startup_state: String,
    /// What was actually provisioned (checkout, branch, granted access).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioning: Option<ChildProvisioning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ChildExecutionSelection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChildCheckoutKind {
    /// The parent's checkout at creation time.
    Current,
    /// An explicit existing directory (`cwd`).
    ExistingPath,
    /// A LUNA-materialized worktree owned by this child.
    NewWorktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChildCreator {
    /// The user's own creation dialog (`CreateChildThread`).
    User,
    /// A parent agent's `create_thread` call, approved on its card.
    Agent,
}

/// Engine-recorded provisioning of one child. Never accepted from a client:
/// the engine computes it at creation and stores it on the child row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildProvisioning {
    pub created_by: ChildCreator,
    pub checkout_kind: ChildCheckoutKind,
    /// Directory the child runs in (the worktree path for `newWorktree`).
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Ref the new worktree branch was created from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// What the creator asked for; `None` = default from the parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_permission_mode: Option<crate::PermissionMode>,
    /// Access the child was created with.
    pub granted_permission_mode: crate::PermissionMode,
    /// The parent's effective access when the child was created.
    pub parent_permission_ceiling: crate::PermissionMode,
    /// The user explicitly granted access above the parent's ceiling. Such a
    /// child is not re-capped by the parent's later ceiling.
    #[serde(default)]
    pub granted_by_user: bool,
    /// Requested access exceeded the ceiling without a grant and was capped.
    #[serde(default)]
    pub escalation_denied: bool,
}

/// Engine-owned synced lineage and durable startup intent. Independent from
/// source/fork lineage and destructive lifecycle ownership.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildThread {
    /// Explicit ownership of the initial delegated message only. Legacy rows
    /// remain ancestry-only and cannot gain stop authority on upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_origin: Option<crate::WorkOrigin>,
    pub parent_task_id: String,
    pub creation_command_id: String,
    #[serde(default)]
    pub permission_ceiling: crate::PermissionMode,
    #[serde(default)]
    pub sandbox_ceiling: Option<crate::SandboxLevel>,
    pub initial_message_id: String,
    pub initial_prompt: String,
    pub report_results: bool,
    pub continue_parent: bool,
    #[serde(default)]
    pub outcomes: Vec<ChildThreadOutcome>,
    /// Transcript turns created at or before this time were reported and
    /// their delivered outcomes compacted away, keeping the synced row bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_floor_ms: Option<i64>,
    /// Absent on rows created before provisioning was recorded: those are
    /// existing-checkout children whose access is capped by the parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioning: Option<ChildProvisioning>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildThreadOutcome {
    pub id: String,
    pub turn_id: String,
    pub outcome: String,
    pub summary: String,
    pub created_at_ms: i64,
    #[serde(default)]
    pub delivered: bool,
}
