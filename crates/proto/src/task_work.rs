//! Owning-host projection of background work. No native identifiers or authority.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkOrigin {
    pub owner_task_id: String,
    /// Application message/launch identity, not the provider turn ID.
    pub owner_turn_id: String,
    pub launch_task_id: String,
    pub launch_turn_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskWorkKind {
    ChildChat,
    NativeChild,
    Workflow,
    Shell,
    Service,
    ResultDelivery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskWorkState {
    Starting,
    Running,
    Waiting,
    Completed,
    Failed,
    Stopped,
    Unknown,
}
impl TaskWorkState {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Stopped)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskWorkItem {
    pub id: String,
    pub task_id: String,
    pub title: String,
    pub kind: TaskWorkKind,
    pub state: TaskWorkState,
    pub origin: Option<WorkOrigin>,
    pub owned: bool,
    pub blocks_completion: bool,
    pub needs_attention: bool,
    pub can_stop: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskWorkSnapshot {
    pub schema_version: u32,
    pub task_id: String,
    pub host_device_id: String,
    pub items: Vec<TaskWorkItem>,
    pub blocking_count: usize,
    pub service_count: usize,
    pub unknown_count: usize,
    pub total_count: usize,
    pub next_offset: Option<usize>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskWorkStopPreview {
    pub task_id: String,
    pub preview_id: String,
    pub items: Vec<TaskWorkItem>,
    pub excluded: Vec<TaskWorkItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskWorkStopStatus {
    Requested,
    Stopped,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskWorkStopResult {
    pub work_id: String,
    pub status: TaskWorkStopStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskWorkStopReceipt {
    pub request_id: String,
    pub task_id: String,
    pub preview_id: String,
    pub results: Vec<TaskWorkStopResult>,
}
