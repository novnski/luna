//! Durable task-command receipts and sequence snapshots.
//!
//! Commands themselves remain engine-owned because some payloads contain local
//! execution details. These wire records are shared by RPC/UI consumers and by
//! the SQLite journal.

use serde::{Deserialize, Serialize};

/// User-selected behavior for a follow-up submitted while a turn is active.
/// Persisted both in device-local UI settings and on each durable queue row so
/// changing the preference never rewrites work that was already accepted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FollowUpBehavior {
    #[default]
    Queue,
    Steer,
}

impl FollowUpBehavior {
    pub fn opposite(self) -> Self {
        match self {
            Self::Queue => Self::Steer,
            Self::Steer => Self::Queue,
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandReasonCode {
    CommandIdConflict,
    StaleTaskRevision,
    StaleBindingGeneration,
    StaleTurn,
    QueueRevisionConflict,
    QueueClaimed,
    QueueMissing,
    ActiveTurnConflict,
    ForeignActive,
    InvalidDestination,
    IncompatibleWorkspace,
    IncompatibleEnvironment,
    ProviderAcceptanceUnknown,
    TransactionFailed,
    UnsupportedOperation,
    OperationRejected,
    OperationFailed,
    PermissionModeRequired,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandReceiptReason {
    pub code: CommandReasonCode,
    pub message: String,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCommandEvent {
    pub sequence: u64,
    pub task_sequence: u64,
    pub command_id: String,
    pub receipt: CommandReceipt,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCommandRead {
    pub snapshot: TaskCommandSnapshot,
    #[serde(default)]
    pub events: Vec<TaskCommandEvent>,
    /// True when the requested cursor is ahead of or incompatible with the
    /// durable sequence. Consumers must replace local state with `snapshot`.
    pub reload_required: bool,
    /// Cursor to pass for the next bounded page. Absent for snapshot-only or
    /// reload-required responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_task_sequence: Option<u64>,
    /// More durable events exist after `next_task_sequence`.
    #[serde(default)]
    pub has_more: bool,
}

/// The public `ExecuteTaskCommand` envelope (CORE-01). `I` is the intent: the
/// engine uses its private intent type; the published contract uses
/// [`TaskCommandIntentEnvelope`]. The defaults keep pre-envelope clients valid.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskCommandEnvelope<I> {
    /// Caller-minted idempotency key: an identical retry returns the original
    /// receipt; a different payload under the same id is `commandIdConflict`.
    pub command_id: String,
    #[serde(default = "default_actor_id")]
    pub actor_id: String,
    #[serde(default = "default_client_id")]
    pub client_id: String,
    #[serde(default = "default_client_session_id")]
    pub client_session_id: String,
    pub task_id: String,
    #[serde(default)]
    pub expected_task_revision: Option<u64>,
    #[serde(default)]
    pub expected_binding_generation: Option<u32>,
    #[serde(default)]
    pub expected_turn_id: Option<String>,
    /// Client issue time in Unix milliseconds; defaults to the engine's receive time.
    #[serde(default = "default_issued_at_ms")]
    #[cfg_attr(feature = "schema", schemars(transform = drop_evaluated_default))]
    pub issued_at_ms: i64,
    pub intent: I,
}

/// A clock default has no stable published value; the field docs state it.
#[cfg(feature = "schema")]
fn drop_evaluated_default(schema: &mut schemars::Schema) {
    if let Some(object) = schema.as_object_mut() {
        object.remove("default");
    }
}

/// A task-command intent as published: a `kind` tag plus that kind's fields.
/// Unknown kinds are refused by the engine with a receipt, never ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TaskCommandIntentEnvelope {
    pub kind: String,
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

fn default_actor_id() -> String {
    "local-user".into()
}

fn default_client_id() -> String {
    "local-ui".into()
}

fn default_client_session_id() -> String {
    "legacy".into()
}

fn default_issued_at_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
