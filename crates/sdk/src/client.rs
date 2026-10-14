//! The typed client: discovery and version negotiation, runtime-validated
//! calls and the task-command contract (stable id, exact target, fences,
//! unknown outcome read back by receipt).

use std::sync::Arc;
use std::time::Duration;

use luna_proto::remote_command::RemoteTaskCommand;
use luna_proto::remote_projection::{
    ProjectionCursor, RemoteTaskItem, RemoteTaskParams, RemoteTasksItem, RemoteTasksParams,
};
use luna_proto::{CommandReceipt, EngineInfo, TaskCommandRead, TaskCommandSnapshot};
use luna_rpc::RpcClient;
use luna_rpc::operations::ApiDescription;
use serde::de::DeserializeOwned;

use crate::SdkError;
use crate::ops::{
    self, CommandIntent, DescribeApi, EmptyParams, ExecuteRemoteTaskCommand, ExecuteTaskCommand,
    ExecuteTaskCommandParams, GetEngineInfo, GetTaskCommandReceipt, GetTaskCommandSnapshot,
    Operation, TaskCommandReceiptParams, TaskCommandSnapshotParams, TaskIntent,
};
use crate::projection::ProjectionStream;
use crate::watch::TaskCommandWatch;

/// A connected transport the client can borrow: an owned [`RpcClient`], a
/// reference to one, or an app handle that owns one.
pub trait Transport: Send + Sync {
    fn rpc(&self) -> &RpcClient;
}

impl Transport for RpcClient {
    fn rpc(&self) -> &RpcClient {
        self
    }
}

impl<T: Transport + ?Sized> Transport for &T {
    fn rpc(&self) -> &RpcClient {
        (**self).rpc()
    }
}

impl<T: Transport + ?Sized> Transport for Arc<T> {
    fn rpc(&self) -> &RpcClient {
        (**self).rpc()
    }
}

/// Who issues a command. Every receipt records these three values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// The principal; the local owner uses its device id.
    pub actor_id: String,
    /// The client kind, such as `luna-cli`.
    pub client_id: String,
    /// One id per client process or session.
    pub client_session_id: String,
}

impl Actor {
    /// An actor with a fresh client session id.
    pub fn new(actor_id: impl Into<String>, client_id: impl Into<String>) -> Self {
        Self {
            actor_id: actor_id.into(),
            client_id: client_id.into(),
            client_session_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

/// Where a command's fence values come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fencing {
    /// Read the task's current snapshot when the intent needs fences.
    Current,
    /// Use exactly this reviewed snapshot; its absent values are not sent.
    Reviewed(TaskCommandSnapshot),
}

/// One task command. The command id is fixed before the first send, so a
/// retry or a receipt read always names the same command.
#[derive(Debug, Clone)]
pub struct TaskCommand<I> {
    pub command_id: String,
    pub task_id: String,
    /// The task's execution host when it is not the connected engine. Never
    /// inferred: a missing value addresses the connected engine.
    pub target_device_id: Option<String>,
    pub actor: Actor,
    pub fencing: Fencing,
    pub issued_at_ms: Option<i64>,
    pub intent: I,
}

impl<I> TaskCommand<I> {
    /// A command with a fresh id, fenced from the task's current snapshot.
    pub fn new(actor: Actor, task_id: impl Into<String>, intent: I) -> Self {
        Self {
            command_id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            target_device_id: None,
            actor,
            fencing: Fencing::Current,
            issued_at_ms: None,
            intent,
        }
    }

    pub fn with_command_id(mut self, command_id: impl Into<String>) -> Self {
        self.command_id = command_id.into();
        self
    }

    pub fn with_target_device(mut self, device_id: impl Into<String>) -> Self {
        self.target_device_id = Some(device_id.into());
        self
    }

    pub fn with_fencing(mut self, fencing: Fencing) -> Self {
        self.fencing = fencing;
        self
    }
}

/// Typed access to one engine connection.
pub struct Client<T> {
    transport: T,
    deadline: Option<Duration>,
    api: Option<ApiDescription>,
}

impl Client<RpcClient> {
    /// Dial a local engine's owner IPC (`ws://127.0.0.1:{port}`), require a
    /// LUNA owner engine and negotiate the API. A caller attaching to the
    /// default port should also verify the data-root claim, as the CLI does.
    pub async fn connect_local(
        port: u16,
        deadline: Duration,
    ) -> Result<(Self, EngineInfo), SdkError> {
        let rpc = luna_rpc::connect_ws(&format!("ws://127.0.0.1:{port}"))
            .await
            .map_err(SdkError::Offline)?;
        let mut client = Client::new(rpc).with_deadline(deadline);
        let info = client.engine_info().await?;
        if !info.supports(luna_proto::capabilities::LUNA_OWNER_IPC_V1) {
            return Err(SdkError::Incompatible {
                detail: "local listener is not a compatible LUNA engine".into(),
            });
        }
        client.negotiate().await?;
        Ok((client, info))
    }
}

impl<T: Transport> Client<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            deadline: None,
            api: None,
        }
    }

    /// Bound every call. Without a deadline a call waits for its reply or
    /// the connection's end, which is what an in-app client wants.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }

    pub fn rpc(&self) -> &RpcClient {
        self.transport.rpc()
    }

    /// The negotiated engine API, if [`Client::negotiate`] found one.
    pub fn api(&self) -> Option<&ApiDescription> {
        self.api.as_ref()
    }

    /// Read the engine's API and refuse a different contract version. An
    /// engine older than `DescribeApi` is accepted as API 1 without a
    /// description; its calls then rely on the engine's own `unknown method`.
    pub async fn negotiate(&mut self) -> Result<Option<&ApiDescription>, SdkError> {
        let api = match self.describe_api().await {
            Ok(api) => api,
            Err(SdkError::Unsupported { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        if api.api_version != crate::API_VERSION {
            return Err(SdkError::Incompatible {
                detail: format!(
                    "engine {} speaks API {}; this SDK ({}) speaks API {}",
                    api.engine_version,
                    api.api_version,
                    crate::SDK_VERSION,
                    crate::API_VERSION
                ),
            });
        }
        self.api = Some(api);
        Ok(self.api.as_ref())
    }

    /// Refuse an operation the negotiated engine does not serve at this
    /// SDK's version, before anything is sent.
    fn require<O: Operation>(&self) -> Result<(), SdkError> {
        let (Some(api), Some(spec)) = (&self.api, ops::spec::<O>()) else {
            return Ok(());
        };
        match api.operation(spec.id) {
            Some(served) if served.version == spec.version => Ok(()),
            Some(served) => Err(SdkError::Incompatible {
                detail: format!(
                    "engine serves {}@{}; this SDK calls @{}",
                    spec.id, served.version, spec.version
                ),
            }),
            None => Err(SdkError::Unsupported {
                operation: O::NAME,
                method: O::METHOD.into(),
            }),
        }
    }

    async fn send(
        &self,
        operation: &'static str,
        method: &'static str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, SdkError> {
        let call = self.transport.rpc().call(method, params);
        let result = match self.deadline {
            Some(after) => tokio::time::timeout(after, call)
                .await
                .map_err(|_| SdkError::Timeout { operation, after })?,
            None => call.await,
        };
        result.map_err(|error| SdkError::from_rpc(operation, error))
    }

    /// Call a unary operation and validate its reply against its type.
    pub async fn call<O: Operation>(&self, params: &O::Params) -> Result<O::Output, SdkError> {
        self.require::<O>()?;
        let params = serde_json::to_value(params)
            .map_err(|error| SdkError::Engine(format!("serialize {}: {error}", O::NAME)))?;
        let value = self.send(O::NAME, O::METHOD, params).await?;
        decode::<O::Output>(O::NAME, value)
    }

    pub async fn describe_api(&self) -> Result<ApiDescription, SdkError> {
        self.call::<DescribeApi>(&EmptyParams {}).await
    }

    pub async fn engine_info(&self) -> Result<EngineInfo, SdkError> {
        self.call::<GetEngineInfo>(&EmptyParams {}).await
    }

    /// The task's current snapshot, or with `after` a bounded page of its
    /// durable events. Refuses a reply for another task or out of order.
    pub async fn task_command_snapshot(
        &self,
        task_id: &str,
        after: Option<u64>,
        target_device_id: Option<&str>,
    ) -> Result<TaskCommandRead, SdkError> {
        let read = self
            .call::<GetTaskCommandSnapshot>(&TaskCommandSnapshotParams {
                task_id: task_id.into(),
                after_task_sequence: after,
                limit: after.map(|_| 100),
                target_device_id: target_device_id.map(str::to_owned),
            })
            .await?;
        validate_read(GetTaskCommandSnapshot::NAME, task_id, after, &read)?;
        Ok(read)
    }

    /// A command's durable receipt; `None` means the engine never admitted it.
    pub async fn task_command_receipt(
        &self,
        command_id: &str,
        target_device_id: Option<&str>,
    ) -> Result<Option<CommandReceipt>, SdkError> {
        let receipt = self
            .call::<GetTaskCommandReceipt>(&TaskCommandReceiptParams {
                command_id: command_id.into(),
                target_device_id: target_device_id.map(str::to_owned),
            })
            .await?;
        if let Some(receipt) = &receipt
            && receipt.command_id != command_id
        {
            return Err(SdkError::invalid(
                GetTaskCommandReceipt::NAME,
                format!("asked for command {command_id}, got {}", receipt.command_id),
            ));
        }
        Ok(receipt)
    }

    /// Admit one command and return its durable receipt.
    ///
    /// A refusal is a receipt (`conflict`, `rejected`, ...), not an error, and
    /// `pending` is not success: see [`Client::wait_for_terminal`]. When the
    /// reply is lost the receipt is read back by the same command id; only
    /// when no receipt exists is the identical envelope sent once more, which
    /// the engine deduplicates. If the receipt cannot be read either, the
    /// result is [`SdkError::UnknownOutcome`].
    pub async fn execute<I: CommandIntent>(
        &self,
        command: &TaskCommand<I>,
    ) -> Result<CommandReceipt, SdkError> {
        self.require::<ExecuteTaskCommand>()?;
        let fences = command.intent.fences();
        let target = command.target_device_id.as_deref();
        // Every command reads the current snapshot first, as the app always
        // has, so a failed read stops it before anything is sent.
        let snapshot = match &command.fencing {
            Fencing::Reviewed(snapshot) => snapshot.clone(),
            Fencing::Current => {
                self.task_command_snapshot(&command.task_id, None, target)
                    .await?
                    .snapshot
            }
        };
        let params = ExecuteTaskCommandParams {
            command_id: command.command_id.clone(),
            actor_id: command.actor.actor_id.clone(),
            client_id: command.actor.client_id.clone(),
            client_session_id: command.actor.client_session_id.clone(),
            task_id: command.task_id.clone(),
            expected_task_revision: fences.task_revision.then_some(snapshot.task_revision),
            expected_binding_generation: snapshot
                .binding_generation
                .filter(|_| fences.binding_generation),
            expected_turn_id: snapshot.active_turn_id.filter(|_| fences.turn),
            issued_at_ms: command.issued_at_ms,
            target_device_id: command.target_device_id.clone(),
            intent: &command.intent,
        };
        let params = serde_json::to_value(&params).map_err(|error| {
            SdkError::Engine(format!("serialize {}: {error}", ExecuteTaskCommand::NAME))
        })?;
        let first = match self.submit(command, params.clone()).await {
            Ok(receipt) => return Ok(receipt),
            Err(error) => error,
        };
        match self.read_back(command).await? {
            Some(receipt) => Ok(receipt),
            None => match self.submit(command, params).await {
                Ok(receipt) => Ok(receipt),
                // No receipt after a second failure means it was not admitted.
                Err(_) => self.read_back(command).await?.ok_or(first),
            },
        }
    }

    async fn submit<I>(
        &self,
        command: &TaskCommand<I>,
        params: serde_json::Value,
    ) -> Result<CommandReceipt, SdkError> {
        let value = self
            .send(ExecuteTaskCommand::NAME, ExecuteTaskCommand::METHOD, params)
            .await?;
        let receipt: CommandReceipt = decode(ExecuteTaskCommand::NAME, value)?;
        validate_receipt(ExecuteTaskCommand::NAME, command, &receipt)?;
        Ok(receipt)
    }

    /// The command's receipt after a lost or unreadable reply. A failed read
    /// leaves the outcome unknown.
    async fn read_back<I>(
        &self,
        command: &TaskCommand<I>,
    ) -> Result<Option<CommandReceipt>, SdkError> {
        let receipt = self
            .task_command_receipt(&command.command_id, command.target_device_id.as_deref())
            .await
            .map_err(|error| SdkError::UnknownOutcome {
                command_id: command.command_id.clone(),
                source: Box::new(error),
            })?;
        if let Some(receipt) = &receipt {
            validate_receipt(GetTaskCommandReceipt::NAME, command, receipt).map_err(|error| {
                SdkError::UnknownOutcome {
                    command_id: command.command_id.clone(),
                    source: Box::new(error),
                }
            })?;
        }
        Ok(receipt)
    }

    /// RM-04: admit one paired-device command and return its durable receipt.
    ///
    /// `actor_id` is the receipt actor this connection's device session
    /// produces (`luna_proto::remote_command::remote_actor_id`). The host
    /// binds the command id to that actor and to the command's payload
    /// identity; a receipt naming another actor, task or payload is never
    /// taken for this command's. As with [`Client::execute`], a refusal is a
    /// receipt and `pending` is not success. When the reply is lost the
    /// receipt is read back by the same id first; only when the host has
    /// none is the identical command sent once more (the host deduplicates
    /// it). If the receipt cannot be read either, the result is
    /// [`SdkError::UnknownOutcome`]. A refusal the host answered (denied,
    /// bad params, unsupported) is returned as it is.
    pub async fn execute_remote(
        &self,
        command: &RemoteTaskCommand,
        actor_id: &str,
    ) -> Result<CommandReceipt, SdkError> {
        self.require::<ExecuteRemoteTaskCommand>()?;
        command
            .validate()
            .map_err(|message| SdkError::InvalidRequest {
                operation: ExecuteRemoteTaskCommand::NAME,
                message,
            })?;
        let first = match self.submit_remote(command, actor_id).await {
            Ok(receipt) => return Ok(receipt),
            Err(error) if !error.may_have_been_admitted() => return Err(error),
            Err(error) => error,
        };
        match self.remote_receipt(command, actor_id).await? {
            Some(receipt) => Ok(receipt),
            None => match self.submit_remote(command, actor_id).await {
                Ok(receipt) => Ok(receipt),
                Err(error) if !error.may_have_been_admitted() => Err(error),
                // No receipt after a second failure means it was not admitted.
                Err(_) => self.remote_receipt(command, actor_id).await?.ok_or(first),
            },
        }
    }

    /// RM-04: the durable receipt of a paired-device command, read by its id:
    /// `None` when the host never admitted it. A read that fails leaves the
    /// outcome unknown. A receipt under this id for another actor, task or
    /// payload is not this command's and is refused, never returned.
    pub async fn remote_receipt(
        &self,
        command: &RemoteTaskCommand,
        actor_id: &str,
    ) -> Result<Option<CommandReceipt>, SdkError> {
        let unknown = |error: SdkError| SdkError::UnknownOutcome {
            command_id: command.command_id.clone(),
            source: Box::new(error),
        };
        let receipt = self
            .task_command_receipt(&command.command_id, None)
            .await
            .map_err(unknown)?;
        if let Some(receipt) = &receipt {
            // Not this command's: a definite answer, not an unknown one.
            validate_remote_receipt(GetTaskCommandReceipt::NAME, command, actor_id, receipt)?;
        }
        Ok(receipt)
    }

    async fn submit_remote(
        &self,
        command: &RemoteTaskCommand,
        actor_id: &str,
    ) -> Result<CommandReceipt, SdkError> {
        let params = serde_json::to_value(command).map_err(|error| {
            SdkError::Engine(format!(
                "serialize {}: {error}",
                ExecuteRemoteTaskCommand::NAME
            ))
        })?;
        let value = self
            .send(
                ExecuteRemoteTaskCommand::NAME,
                ExecuteRemoteTaskCommand::METHOD,
                params,
            )
            .await?;
        let receipt: CommandReceipt = decode(ExecuteRemoteTaskCommand::NAME, value)?;
        validate_remote_receipt(ExecuteRemoteTaskCommand::NAME, command, actor_id, &receipt)?;
        Ok(receipt)
    }

    /// Rename one task through the published intent, fenced from its
    /// current snapshot.
    pub async fn rename_task(
        &self,
        actor: Actor,
        task_id: &str,
        title: &str,
    ) -> Result<CommandReceipt, SdkError> {
        let command = TaskCommand::new(
            actor,
            task_id,
            TaskIntent::Rename {
                title: title.into(),
            },
        );
        self.execute(&command).await
    }

    /// Poll a command's receipt until it is terminal or `timeout` passes;
    /// returns the last receipt read, which is still `pending` on timeout.
    /// Needs a Tokio runtime for its timer.
    pub async fn wait_for_terminal(
        &self,
        receipt: CommandReceipt,
        target_device_id: Option<&str>,
        timeout: Duration,
    ) -> Result<CommandReceipt, SdkError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut current = receipt;
        while !current.status.is_terminal() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if let Some(next) = self
                .task_command_receipt(&current.command_id, target_device_id)
                .await?
            {
                current = next;
            }
        }
        Ok(current)
    }

    /// Watch a task's commands from `after` (or from its current snapshot).
    /// Dropping the watch cancels the engine's stream.
    pub async fn watch_task_commands(
        &self,
        task_id: &str,
        after: Option<u64>,
        target_device_id: Option<&str>,
    ) -> Result<TaskCommandWatch<'_, T>, SdkError> {
        TaskCommandWatch::open(self, task_id, after, target_device_id).await
    }

    /// RM-03: follow the connected host's task registry from `after` (the
    /// cursor a client holds), or from a snapshot. Dropping the stream
    /// cancels the engine's stream. Apply its items to a
    /// [`crate::ProjectionStore`], which refuses anything out of order.
    pub async fn watch_remote_tasks(
        &self,
        after: Option<ProjectionCursor>,
    ) -> Result<ProjectionStream<RemoteTasksItem>, SdkError> {
        let subscription = self
            .subscribe::<ops::WatchRemoteTasks>(&RemoteTasksParams { after })
            .await?;
        Ok(ProjectionStream::new(
            ops::WatchRemoteTasks::NAME,
            subscription,
        ))
    }

    /// RM-03: follow one task in detail from `after`, or from a snapshot.
    pub async fn watch_remote_task(
        &self,
        task_id: &str,
        after: Option<ProjectionCursor>,
    ) -> Result<ProjectionStream<RemoteTaskItem>, SdkError> {
        let subscription = self
            .subscribe::<ops::WatchRemoteTask>(&RemoteTaskParams {
                task_id: task_id.into(),
                after,
            })
            .await?;
        Ok(ProjectionStream::new(
            ops::WatchRemoteTask::NAME,
            subscription,
        ))
    }

    /// Open a watch operation's stream, bounded by the client's deadline
    /// until the engine accepts it.
    async fn subscribe<O: Operation>(
        &self,
        params: &O::Params,
    ) -> Result<luna_rpc::RpcSubscription, SdkError> {
        self.require::<O>()?;
        let params = serde_json::to_value(params)
            .map_err(|error| SdkError::Engine(format!("serialize {}: {error}", O::NAME)))?;
        let subscribe = self.transport.rpc().subscribe_checked(O::METHOD, params);
        match self.deadline {
            Some(after) => {
                tokio::time::timeout(after, subscribe)
                    .await
                    .map_err(|_| SdkError::Timeout {
                        operation: O::NAME,
                        after,
                    })?
            }
            None => subscribe.await,
        }
        .map_err(|error| SdkError::from_rpc(O::NAME, error))
    }

    pub(crate) fn deadline(&self) -> Option<Duration> {
        self.deadline
    }
}

pub(crate) fn decode<T: DeserializeOwned>(
    operation: &'static str,
    value: serde_json::Value,
) -> Result<T, SdkError> {
    serde_json::from_value(value).map_err(|error| SdkError::invalid(operation, error.to_string()))
}

fn validate_receipt<I>(
    operation: &'static str,
    command: &TaskCommand<I>,
    receipt: &CommandReceipt,
) -> Result<(), SdkError> {
    if receipt.command_id != command.command_id || receipt.task_id != command.task_id {
        return Err(SdkError::invalid(
            operation,
            format!(
                "receipt is for command {} on task {}, not {} on {}",
                receipt.command_id, receipt.task_id, command.command_id, command.task_id
            ),
        ));
    }
    Ok(())
}

/// RM-04: a receipt is this paired-device command's: its id and task, the
/// session's actor and the command's payload identity (also on a
/// `commandIdConflict` receipt, which records the conflicting payload).
pub(crate) fn validate_remote_receipt(
    operation: &'static str,
    command: &RemoteTaskCommand,
    actor_id: &str,
    receipt: &CommandReceipt,
) -> Result<(), SdkError> {
    if receipt.command_id != command.command_id || receipt.task_id != command.task_id {
        return Err(SdkError::invalid(
            operation,
            format!(
                "receipt is for command {} on task {}, not {} on {}",
                receipt.command_id, receipt.task_id, command.command_id, command.task_id
            ),
        ));
    }
    if receipt.actor_id != actor_id {
        return Err(SdkError::invalid(
            operation,
            format!("command {} belongs to another client", command.command_id),
        ));
    }
    if receipt.payload_hash != luna_rpc::remote_auth::remote_command_identity(command) {
        return Err(SdkError::invalid(
            operation,
            format!(
                "command {} names another payload on the host",
                command.command_id
            ),
        ));
    }
    Ok(())
}

/// A page is for the requested task, in increasing order, after its cursor.
pub(crate) fn validate_read(
    operation: &'static str,
    task_id: &str,
    after: Option<u64>,
    read: &TaskCommandRead,
) -> Result<(), SdkError> {
    if read.snapshot.task_id != task_id {
        return Err(SdkError::invalid(
            operation,
            format!("snapshot is for task {}", read.snapshot.task_id),
        ));
    }
    let mut previous = after.unwrap_or(0);
    for event in &read.events {
        if event.receipt.task_id != task_id
            || event.command_id != event.receipt.command_id
            || event.task_sequence <= previous
        {
            return Err(SdkError::invalid(
                operation,
                format!(
                    "event {} (task sequence {}) is out of order or for another task",
                    event.command_id, event.task_sequence
                ),
            ));
        }
        previous = event.task_sequence;
    }
    Ok(())
}
