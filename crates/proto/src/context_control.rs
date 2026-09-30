//! Harness-owned context controls and their durable task-local projection.
//!
//! LUNA coordinates provider operations, but never manufactures a replacement
//! summary. The chat document keeps only operation/boundary provenance and a
//! provider summary when the provider explicitly exposes one.

use serde::{Deserialize, Serialize};

use crate::HarnessId;

pub const CONTEXT_CONTROL_SCHEMA_VERSION: u32 = 1;
pub const MAX_CONTEXT_HANDOFF_BYTES: usize = 12 * 1024;
pub const MAX_CONTEXT_HANDOFF_MESSAGES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompactMechanism {
    NativeOperation,
    ProviderCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextOperationStatus {
    Idle,
    Running,
    Completed,
    Failed,
    Unknown,
}

impl Default for ContextOperationStatus {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactOperation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub status: ContextOperationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mechanism: Option<CompactMechanism>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_generation: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CompactOperation {
    pub fn blocks_new_turn(&self) -> bool {
        matches!(
            self.status,
            ContextOperationStatus::Running | ContextOperationStatus::Unknown
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextBoundaryKind {
    Cleared,
    ProviderSwitched,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextHandoffKind {
    RecentVisibleHistory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextHandoff {
    pub kind: ContextHandoffKind,
    /// Explicit user-selected, visible transcript text only. Tool output and
    /// hidden reasoning are never copied into this field.
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextBoundary {
    pub boundary_id: String,
    pub kind: ContextBoundaryKind,
    pub generation: u32,
    pub from_provider: HarnessId,
    pub to_provider: HarnessId,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_kind: Option<ContextHandoffKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextControlState {
    pub schema_version: u32,
    /// Native goal snapshot; transported with task-local provider controls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<crate::GoalReadback>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_generation: Option<u32>,
    #[serde(default)]
    pub compact: CompactOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_boundary: Option<ContextBoundary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_handoff: Option<ContextHandoff>,
    /// Last explicit visible-history handoff, retained after provider
    /// acceptance for audit without causing another delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_handoff: Option<ContextHandoff>,
    /// Only populated from a provider's readable summary exposure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

impl Default for ContextControlState {
    fn default() -> Self {
        Self {
            schema_version: CONTEXT_CONTROL_SCHEMA_VERSION,
            goal: None,
            binding_generation: None,
            compact: CompactOperation::default(),
            last_boundary: None,
            pending_handoff: None,
            last_handoff: None,
            provider_summary: None,
            notice: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextControlCapabilities {
    pub context_usage: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact: Option<CompactMechanism>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_unavailable_reason: Option<String>,
    pub clear_context: bool,
    pub model_change: bool,
    pub provider_switch: bool,
    pub model_change_note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_and_unknown_compaction_block_new_turns() {
        for status in [
            ContextOperationStatus::Running,
            ContextOperationStatus::Unknown,
        ] {
            assert!(
                CompactOperation {
                    status,
                    ..Default::default()
                }
                .blocks_new_turn()
            );
        }
        assert!(!CompactOperation::default().blocks_new_turn());
    }
}
