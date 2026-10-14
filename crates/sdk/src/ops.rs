//! The typed operations this SDK version calls. Each names its wire method,
//! whose catalog row (`luna_rpc::operations`) supplies the stable id, version,
//! kind and reply shape, plus the Rust types that are its runtime validation
//! and the source of the published JSON Schemas and TypeScript types.

use luna_proto::remote_command::RemoteTaskCommand;
use luna_proto::remote_projection::{
    RemoteTaskItem, RemoteTaskParams, RemoteTasksItem, RemoteTasksParams,
};
use luna_proto::{CommandReceipt, EngineInfo, TaskCommandRead};
use luna_rpc::methods;
use luna_rpc::operations::{ApiDescription, OperationSpec};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// One typed operation of the public contract.
pub trait Operation {
    /// The operation's name in every SDK language binding.
    const NAME: &'static str;
    /// The wire method (the catalog row's `method`).
    const METHOD: &'static str;
    type Params: Serialize + Send + Sync;
    /// The unary result, or one stream item for a watch.
    type Output: DeserializeOwned + Send;
}

/// The catalog row an operation is served under. Every SDK operation has
/// one; the SDK manifest generation refuses an uncatalogued method.
pub fn spec<O: Operation>() -> Option<&'static OperationSpec> {
    luna_rpc::operations::spec(O::METHOD)
}

/// Every operation this SDK calls, as `(name, wire method)`.
pub const OPERATIONS: &[(&str, &str)] = &[
    (DescribeApi::NAME, DescribeApi::METHOD),
    (GetEngineInfo::NAME, GetEngineInfo::METHOD),
    (GetTaskCommandSnapshot::NAME, GetTaskCommandSnapshot::METHOD),
    (GetTaskCommandReceipt::NAME, GetTaskCommandReceipt::METHOD),
    (ExecuteTaskCommand::NAME, ExecuteTaskCommand::METHOD),
    (WatchTaskCommands::NAME, WatchTaskCommands::METHOD),
    (WatchRemoteTasks::NAME, WatchRemoteTasks::METHOD),
    (WatchRemoteTask::NAME, WatchRemoteTask::METHOD),
    (
        ExecuteRemoteTaskCommand::NAME,
        ExecuteRemoteTaskCommand::METHOD,
    ),
];

/// Parameters of an operation that takes none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EmptyParams {}

/// Parameters of a task's command snapshot read or watch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskCommandSnapshotParams {
    pub task_id: String,
    /// With a cursor, the durable events after it (a bounded page); without,
    /// the current snapshot only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_task_sequence: Option<u64>,
    /// Page size; the engine clamps it to 1..=100 (default 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
    /// The task's execution host, when it is not the connected engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

/// Parameters of a durable receipt lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskCommandReceiptParams {
    pub command_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

/// The published task-command intents. Every other intent the engine accepts
/// is first-party and may change without an API version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum TaskIntent {
    /// Rename a task. Fenced by its revision and provider binding generation.
    Rename { title: String },
}

/// Which snapshot values an intent's command must carry as fences. The
/// engine refuses a fenced intent sent without them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fences {
    /// `expectedTaskRevision`.
    pub task_revision: bool,
    /// `expectedBindingGeneration`, when the task has a provider binding.
    pub binding_generation: bool,
    /// `expectedTurnId`, when the task has an active turn.
    pub turn: bool,
}

impl Fences {
    pub fn any(self) -> bool {
        self.task_revision || self.binding_generation || self.turn
    }
}

impl TaskIntent {
    /// Each published intent kind and its fences.
    pub const KINDS: &'static [(&'static str, Fences)] = &[(
        "rename",
        Fences {
            task_revision: true,
            binding_generation: true,
            turn: false,
        },
    )];
}

/// What the SDK needs to send an intent: its wire form, its `kind` tag and
/// the fences the engine requires for it. [`TaskIntent`] is the published
/// set; a first-party client may implement this for its own intents.
pub trait CommandIntent: Serialize + Send + Sync {
    fn kind(&self) -> &str;
    fn fences(&self) -> Fences;
}

impl CommandIntent for TaskIntent {
    fn kind(&self) -> &str {
        match self {
            Self::Rename { .. } => "rename",
        }
    }

    fn fences(&self) -> Fences {
        let kind = self.kind();
        Self::KINDS
            .iter()
            .find(|(published, _)| *published == kind)
            .map(|(_, fences)| *fences)
            .unwrap_or_default()
    }
}

/// The `ExecuteTaskCommand` envelope as the SDK sends it. Absent optional
/// fields are omitted, so the engine applies its documented defaults.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(rename = "ExecuteTaskCommandParams"))]
#[serde(rename_all = "camelCase")]
pub struct ExecuteTaskCommandParams<I> {
    /// Stable idempotency key, minted once before the first send.
    pub command_id: String,
    pub actor_id: String,
    pub client_id: String,
    pub client_session_id: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_task_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_binding_generation: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_turn_id: Option<String>,
    /// Client issue time in Unix milliseconds; omitted, the engine's receive time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at_ms: Option<i64>,
    /// The task's execution host, when it is not the connected engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
    pub intent: I,
}

/// One `WatchTaskCommands` stream item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TaskCommandWatchItem {
    /// First item: the snapshot and, after a cursor, a bounded event page.
    Snapshot { read: TaskCommandRead },
    /// A receipt recorded for the watched task after the subscription began.
    /// Boxed for size only: serde and the schema treat it as the receipt.
    Receipt { receipt: Box<CommandReceipt> },
}

/// The engine's API version, build and served operations.
pub struct DescribeApi;
impl Operation for DescribeApi {
    const NAME: &'static str = "describeApi";
    const METHOD: &'static str = methods::DESCRIBE_API;
    type Params = EmptyParams;
    type Output = ApiDescription;
}

/// The connected engine's device, workspace scope and capabilities.
pub struct GetEngineInfo;
impl Operation for GetEngineInfo {
    const NAME: &'static str = "engineInfo";
    const METHOD: &'static str = methods::ENGINE_INFO;
    type Params = EmptyParams;
    type Output = EngineInfo;
}

/// A task's command snapshot, or a bounded page of its durable events.
pub struct GetTaskCommandSnapshot;
impl Operation for GetTaskCommandSnapshot {
    const NAME: &'static str = "getTaskCommandSnapshot";
    const METHOD: &'static str = methods::GET_TASK_COMMAND_SNAPSHOT;
    type Params = TaskCommandSnapshotParams;
    type Output = TaskCommandRead;
}

/// One command's durable receipt, or `null` when the engine has none.
pub struct GetTaskCommandReceipt;
impl Operation for GetTaskCommandReceipt {
    const NAME: &'static str = "getTaskCommandReceipt";
    const METHOD: &'static str = methods::GET_TASK_COMMAND_RECEIPT;
    type Params = TaskCommandReceiptParams;
    type Output = Option<CommandReceipt>;
}

/// Admit one task command; the reply is its durable receipt.
pub struct ExecuteTaskCommand;
impl Operation for ExecuteTaskCommand {
    const NAME: &'static str = "executeTaskCommand";
    const METHOD: &'static str = methods::EXECUTE_TASK_COMMAND;
    type Params = ExecuteTaskCommandParams<TaskIntent>;
    type Output = CommandReceipt;
}

/// A task's command snapshot, then each receipt recorded for it.
pub struct WatchTaskCommands;
impl Operation for WatchTaskCommands {
    const NAME: &'static str = "watchTaskCommands";
    const METHOD: &'static str = methods::WATCH_TASK_COMMANDS;
    type Params = TaskCommandSnapshotParams;
    type Output = TaskCommandWatchItem;
}

/// RM-03: the connected host's projects and tasks (its cheap registry): a
/// snapshot, or `current` for the client's cursor, then each change.
pub struct WatchRemoteTasks;
impl Operation for WatchRemoteTasks {
    const NAME: &'static str = "watchRemoteTasks";
    const METHOD: &'static str = methods::WATCH_REMOTE_TASKS;
    type Params = RemoteTasksParams;
    type Output = RemoteTasksItem;
}

/// RM-03: one task in detail (its row, command position and a bounded
/// transcript tail): a snapshot or `current`, then deltas, or `removed`.
pub struct WatchRemoteTask;
impl Operation for WatchRemoteTask {
    const NAME: &'static str = "watchRemoteTask";
    const METHOD: &'static str = methods::WATCH_REMOTE_TASK;
    type Params = RemoteTaskParams;
    type Output = RemoteTaskItem;
}

/// RM-04: a paired device session's typed task command (`tasks-operate`).
/// The reply is its durable receipt, whose `actorId` is that session and
/// whose `payloadHash` is the command's payload identity
/// (`luna_rpc::remote_auth::remote_command_identity`).
pub struct ExecuteRemoteTaskCommand;
impl Operation for ExecuteRemoteTaskCommand {
    const NAME: &'static str = "executeRemoteTaskCommand";
    const METHOD: &'static str = methods::EXECUTE_REMOTE_TASK_COMMAND;
    type Params = RemoteTaskCommand;
    type Output = CommandReceipt;
}
