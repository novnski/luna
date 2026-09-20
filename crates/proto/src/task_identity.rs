//! Stable LUNA task identity and provider execution-binding wire records.
//!
//! A task id is app-owned. Provider thread ids, paths, machines, and worktrees
//! are observations attached through immutable binding generations; none of
//! them may become the task primary key.

use serde::{Deserialize, Serialize};

use crate::{ChatConfig, HarnessId};

pub const TASK_IDENTITY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskOrigin {
    LunaCreated,
    ProviderImported,
    ProviderAdopted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BindingOrigin {
    LunaCreated,
    ProviderImported,
    ProviderAdopted,
    ContextReset,
    ProviderSwitch,
    HandedOff,
    Forked,
    Copied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportOwnership {
    ProviderOwned,
    LunaAdopted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReconciliationState {
    Current,
    Stale,
    ProviderMissing,
    EnvironmentMissing,
    ExternalChange,
    ForeignActive,
    AmbiguousDuplicate,
    Replaced,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskLineage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_owner_task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceIdentity {
    pub schema_version: u32,
    pub workspace_id: String,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_space_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<String>,
    pub current_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    pub observed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTaskObservation {
    pub provider_task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub cwd: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default)]
    pub ephemeral: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationRecord {
    pub state: ReconciliationState,
    pub reason: String,
    pub observed_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderTaskObservation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionBinding {
    pub schema_version: u32,
    pub binding_id: String,
    pub task_id: String,
    pub generation: u32,
    pub provider_id: HarnessId,
    pub provider_instance_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_home_identity: Option<String>,
    pub provider_task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_cursor: Option<String>,
    pub environment_id: String,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    pub origin: BindingOrigin,
    pub provenance: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    pub reconciliation: ReconciliationRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by_binding_id: Option<String>,
    pub created_at_ms: i64,
    pub last_observed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LunaTaskIdentity {
    pub schema_version: u32,
    pub task_id: String,
    pub origin: TaskOrigin,
    pub lineage: TaskLineage,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_binding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_import_key: Option<String>,
    #[serde(default)]
    pub tombstoned: bool,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskIdentityView {
    pub task: LunaTaskIdentity,
    pub workspace: WorkspaceIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_binding: Option<ExecutionBinding>,
    #[serde(default)]
    pub bindings: Vec<ExecutionBinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderImportPreview {
    pub preview_id: String,
    pub expires_at_ms: i64,
    pub provider_id: HarnessId,
    pub provider_instance_id: String,
    pub provider_task: ProviderTaskObservation,
    pub ownership: ImportOwnership,
    pub workspace: WorkspaceIdentity,
    pub proposed_title: String,
    pub reconciliation_state: ReconciliationState,
    pub adoptable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptProviderTaskResult {
    pub task: TaskIdentityView,
    pub chat_config: Option<ChatConfig>,
    pub already_visible: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lineage_fields_remain_independent_on_the_wire() {
        let lineage = TaskLineage {
            parent_task_id: Some("parent".into()),
            source_task_id: Some("source".into()),
            lifecycle_owner_task_id: Some("owner".into()),
        };
        let json = serde_json::to_value(&lineage).unwrap();
        assert_eq!(json["parentTaskId"], "parent");
        assert_eq!(json["sourceTaskId"], "source");
        assert_eq!(json["lifecycleOwnerTaskId"], "owner");
    }

    #[test]
    fn reconciliation_states_use_stable_camel_case_names() {
        assert_eq!(
            serde_json::to_string(&ReconciliationState::ForeignActive).unwrap(),
            "\"foreignActive\""
        );
        assert_eq!(
            serde_json::to_string(&ReconciliationState::ProviderMissing).unwrap(),
            "\"providerMissing\""
        );
    }
}
