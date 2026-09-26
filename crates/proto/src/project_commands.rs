//! User-owned, device-local project actions and durable terminal receipts.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAction {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub setup_on_worktree_create: bool,
    #[serde(default = "yes")]
    pub wait_for_setup: bool,
    #[serde(default)]
    pub preview_url: Option<String>,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectCommandStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Canceled,
}
impl ProjectCommandStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Running => "Running",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
            Self::Canceled => "Canceled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCommandRun {
    pub id: String,
    pub project_id: String,
    pub device_id: String,
    pub chat_id: String,
    pub cwd: String,
    pub checkout_generation: String,
    pub action: ProjectAction,
    pub action_digest: String,
    pub setup: bool,
    pub attempt: u32,
    pub status: ProjectCommandStatus,
    pub terminal_id: Option<String>,
    pub previous_terminal_ids: Vec<String>,
    pub process_group_id: Option<u32>,
    pub exit_code: Option<i32>,
    pub detail: Option<String>,
    pub updated_at_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCommandsSnapshot {
    pub project_id: String,
    pub cwd: String,
    pub actions: Vec<ProjectAction>,
    pub runs: Vec<ProjectCommandRun>,
}
