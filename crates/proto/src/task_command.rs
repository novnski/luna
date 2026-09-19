//! Durable task-command receipts and sequence snapshots.
//!
//! Commands themselves remain engine-owned because some payloads contain local
//! execution details. These wire records are shared by RPC/UI consumers and by
//! the SQLite journal.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandReceiptStatus {
    Pending,
    Applied,
    Rejected,
    Conflict,
    Superseded,
    Failed,
}

impl CommandReceiptStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandReasonCode {
    CommandIdConflict,
    StaleTaskRevision,
    StaleBindingGeneration,
    StaleTurn,
    ActiveTurnConflict,
    ForeignActive,
    ProviderAcceptanceUnknown,
    TransactionFailed,
    UnsupportedOperation,
    OperationRejected,
    OperationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandReceiptReason {
    pub code: CommandReasonCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCommandSnapshot {
    pub task_id: String,
    pub task_revision: u64,
    pub task_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_generation: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandReceipt {
    pub command_id: String,
    pub task_id: String,
    pub intent: String,
    pub payload_hash: String,
    pub status: CommandReceiptStatus,
    pub snapshot: TaskCommandSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_sequence: Option<u64>,
    pub actor_id: String,
    pub client_id: String,
    pub client_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_environment: Option<String>,
    /// Engine/provider correlation used to finalize a pending send only after
    /// the provider confirms the matching turn/message boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<CommandReceiptReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_action: Option<String>,
    pub accepted_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_at_ms: Option<i64>,
    pub observed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCommandEvent {
    pub sequence: u64,
    pub task_sequence: u64,
    pub command_id: String,
    pub receipt: CommandReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCommandRead {
    pub snapshot: TaskCommandSnapshot,
    #[serde(default)]
    pub events: Vec<TaskCommandEvent>,
    /// True when the requested cursor is ahead of or incompatible with the
    /// durable sequence. Consumers must replace local state with `snapshot`.
    pub reload_required: bool,
}
