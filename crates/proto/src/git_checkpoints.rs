//! Local Git checkpoints. These describe observed file trees, not provider history.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCheckpoint {
    pub turn_id: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub outcome: Option<String>,
    pub before_available: bool,
    pub after_available: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCheckpointList {
    pub checkout_id: String,
    pub cwd: String,
    pub checkpoints: Vec<GitCheckpoint>,
    pub restore_available: bool,
    pub restore_unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCheckpointDiff {
    pub turn_id: String,
    pub patch: String,
    pub paths: Vec<String>,
    pub truncated: bool,
    /// Changes in a shared checkout are observed, not attributed to this task.
    pub attribution: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCheckpointRestorePreview {
    pub checkout_id: String,
    pub cwd: String,
    pub turn_id: String,
    pub point: String,
    pub paths: Vec<String>,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCheckpointRestoreRequest {
    pub chat_id: String,
    pub cwd: String,
    pub turn_id: String,
    pub point: String,
    pub expected_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCheckpointRestoreReceipt {
    pub restored: bool,
    pub recovery_ref: String,
    pub paths: Vec<String>,
}
