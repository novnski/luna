//! Provider-backed conversation branching and earlier-message edit contracts.
//!
//! These types describe provider history operations only. They intentionally
//! carry no filesystem checkpoint or worktree semantics.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderForkCapability {
    None,
    Tip,
    Checkpoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProviderForkBoundary {
    Tip,
    BeforeCheckpoint { checkpoint_id: String },
    ThroughCheckpoint { checkpoint_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderForkRequest {
    pub source_session_id: String,
    pub boundary: ProviderForkBoundary,
    pub cwd: String,
    pub model: Option<String>,
    /// Engine-resolved provider home. Never accepted from UI/remote callers.
    pub provider_home: Option<String>,
    /// Engine-owned account isolation flag for providers whose CLI supports a
    /// file-only authentication backend.
    pub force_file_auth: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderForkResult {
    pub provider_session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConversationForkBoundaryKind {
    Tip,
    BeforeUserMessage,
    AfterCompletedTurn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecoveryView {
    pub recovery_id: String,
    pub task_id: String,
    pub source_binding_id: String,
    pub source_provider_session_id: String,
    pub cutoff_message_id: String,
    pub created_at_ms: i64,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCapabilityView {
    pub task_id: String,
    pub provider: Option<crate::HarnessId>,
    pub capability: ProviderForkCapability,
    pub unavailable_reason: Option<String>,
}
