//! Read-only CLI adapter for the running local engine. All data comes through
//! the same typed RPC service as the UI; this module never opens the data store.

use std::time::Duration;

use clap::{Args, Subcommand};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use zeron_proto::{Chat, Device, EngineInfo, Space, TaskCommandRead, TaskIdentityView};
use zeron_rpc::{RpcClient, RpcError, methods};

const TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_PORT: u16 = zeron_engine::DEFAULT_LUNA_IPC_PORT;
const MAX_LIMIT: u16 = 100;

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Emit a versioned JSON object, including for failures.
    #[arg(long, global = true)]
    json: bool,
    /// Local engine IPC port (defaults to LUNA_IPC_PORT or 59778).
    #[arg(long, global = true)]
    port: Option<u16>,
    #[command(subcommand)]
    command: InspectCommand,
}

#[derive(Debug, Subcommand)]
enum InspectCommand {
    /// Show the connected engine identity, scope, readiness and capabilities.
    Status {
        /// Require this exact machine ID; omit to discover the local ID.
        #[arg(long)]
        machine: Option<String>,
    },
    /// Check the local read path without touching credentials or providers.
    Doctor {
        #[arg(long)]
        machine: Option<String>,
    },
    /// Show CLI/engine version information and advertised capabilities.
    Version {
        #[arg(long)]
        machine: Option<String>,
    },
    /// List projects owned by the exact local machine/environment.
    Projects {
        #[command(flatten)]
        target: EnvironmentTarget,
        #[arg(long, default_value_t = 20)]
        limit: u16,
        /// Continue after this project ID from the previous page.
        #[arg(long)]
        after: Option<String>,
    },
    /// List tasks in one exact project.
    Tasks {
        #[command(flatten)]
        target: ProjectTarget,
        #[arg(long, default_value_t = 20)]
        limit: u16,
        /// Continue after this task ID from the previous page.
        #[arg(long)]
        after: Option<String>,
    },
    /// Read one task's metadata, identity and a bounded command receipt page.
    Read {
        #[command(flatten)]
        target: TaskTarget,
        #[arg(long, default_value_t = 20)]
        limit: u16,
        /// Continue command receipts after this task sequence (defaults to 0).
        #[arg(long)]
        after_task_sequence: Option<u64>,
    },
}

#[derive(Debug, Args)]
struct EnvironmentTarget {
    #[arg(long)]
    machine: String,
    #[arg(long)]
    environment: String,
}

#[derive(Debug, Args)]
struct ProjectTarget {
    #[command(flatten)]
    environment: EnvironmentTarget,
    #[arg(long)]
    project: String,
}

#[derive(Debug, Args)]
struct TaskTarget {
    #[command(flatten)]
    project: ProjectTarget,
    #[arg(long)]
    task: String,
}

#[derive(Debug)]
struct CliError {
    kind: &'static str,
    code: i32,
    message: String,
}

impl CliError {
    fn new(kind: &'static str, code: i32, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
        }
    }

    fn offline(message: impl Into<String>) -> Self {
        Self::new("offline", 3, message)
    }

    fn mismatch(message: impl Into<String>) -> Self {
        Self::new("targetMismatch", 4, message)
    }

    fn missing(message: impl Into<String>) -> Self {
        Self::new("notFound", 5, message)
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self::new("malformedResponse", 7, message)
    }
}

impl From<RpcError> for CliError {
    fn from(error: RpcError) -> Self {
        match error {
            RpcError::Transport(_) | RpcError::Closed => Self::offline(error.to_string()),
            RpcError::UnknownMethod(_) => Self::new("unavailable", 6, error.to_string()),
            RpcError::BadParams(_) => Self::protocol(error.to_string()),
            RpcError::Failed(_) => Self::new("engineError", 8, error.to_string()),
        }
    }
}

struct Output {
    command: &'static str,
    data: Value,
    human: String,
}

/// Clap runs before `InspectArgs` exists. Keep its parse failures on the same
/// JSON envelope as runtime errors when this exact subcommand requests JSON.
pub fn print_parse_error(error: &clap::Error) -> i32 {
    println!(
        "{}",
        json!({
            "schemaVersion": 1,
            "error": {
                "kind": "invalidArgument",
                "message": error.to_string().trim(),
            },
        })
    );
    2
}

pub async fn run(args: InspectArgs) -> i32 {
    let json_mode = args.json;
    match execute(args).await {
        Ok(output) => {
            if json_mode {
                println!(
                    "{}",
                    json!({
                        "schemaVersion": 1,
                        "command": output.command,
                        "data": output.data,
                    })
                );
            } else {
                println!("{}", output.human);
            }
            0
        }
        Err(error) => {
            if json_mode {
                println!(
                    "{}",
                    json!({
                        "schemaVersion": 1,
                        "error": { "kind": error.kind, "message": error.message },
                    })
                );
            } else {
                eprintln!("luna inspect: {}", error.message);
            }
            error.code
        }
    }
}

async fn execute(args: InspectArgs) -> Result<Output, CliError> {
    // Validate bounds before dialing, including while the engine is offline.
    let limit = match &args.command {
        InspectCommand::Projects { limit, .. }
        | InspectCommand::Tasks { limit, .. }
        | InspectCommand::Read { limit, .. } => Some(checked_limit(*limit)?),
        _ => None,
    };
    let port = args.port.unwrap_or_else(|| {
        std::env::var("LUNA_IPC_PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_PORT)
    });
    let client = zeron_rpc::connect_ws(&format!("ws://127.0.0.1:{port}"))
        .await
        .map_err(|error| {
            CliError::offline(format!(
                "local engine unavailable on 127.0.0.1:{port}: {error}"
            ))
        })?;
    let info: EngineInfo = call(&client, methods::ENGINE_INFO, json!({})).await?;
    if !info.supports(zeron_proto::capabilities::LUNA_OWNER_IPC_V1) {
        return Err(CliError::protocol(
            "local listener is not a compatible LUNA engine",
        ));
    }
    if args.port.is_none() {
        crate::paths::verify_local_owner(&client, &crate::paths::data_dir())
            .await
            .map_err(|error| CliError::protocol(error.to_string()))?;
    }
    match args.command {
        InspectCommand::Status { machine } => {
            require_machine(&info, machine.as_deref())?;
            let ready = engine_ready(&client).await?;
            let data = json!({
                "machineId": info.device_id,
                "workspaceScope": info.workspace_scope,
                "ready": ready,
                "capabilities": info.capabilities,
            });
            Ok(Output {
                command: "status",
                human: format!(
                    "Machine {} — {} — {}\nCapabilities: {}",
                    data["machineId"].as_str().unwrap_or("?"),
                    data["workspaceScope"].as_str().unwrap_or("?"),
                    if ready { "ready" } else { "not ready" },
                    info.capabilities.join(", ")
                ),
                data,
            })
        }
        InspectCommand::Doctor { machine } => {
            require_machine(&info, machine.as_deref())?;
            let ready = engine_ready(&client).await?;
            let task_identity = info.supports(zeron_proto::capabilities::TASK_IDENTITY_V1);
            let history_search =
                info.supports(zeron_proto::capabilities::SESSION_HISTORY_SEARCH_V1);
            let data = json!({
                "machineId": info.device_id,
                "healthy": ready,
                "checks": {
                    "localRpc": true,
                    "engineReady": ready,
                    "taskIdentityAdvertised": task_identity,
                    "sessionHistorySearchAdvertised": history_search,
                },
                "writeCommands": "unavailable: cross-task send requires in-process local UI approval",
            });
            Ok(Output {
                command: "doctor",
                human: format!(
                    "Local RPC: connected\nEngine: {}\nTask identity: {}\nHistory search: {}\nWrites: unavailable (local UI approval required)",
                    if ready { "ready" } else { "not ready" },
                    if task_identity {
                        "advertised"
                    } else {
                        "not advertised"
                    },
                    if history_search {
                        "advertised"
                    } else {
                        "not advertised"
                    },
                ),
                data,
            })
        }
        InspectCommand::Version { machine } => {
            require_machine(&info, machine.as_deref())?;
            let devices: Vec<Device> = first_snapshot(&client, methods::WATCH_DEVICES).await?;
            let published = devices
                .iter()
                .find(|device| device.id == info.device_id)
                .and_then(|device| device.version.clone());
            let data = json!({
                "cliVersion": env!("CARGO_PKG_VERSION"),
                "enginePublishedVersion": published,
                "machineId": info.device_id,
                "capabilities": info.capabilities,
            });
            Ok(Output {
                command: "version",
                human: format!(
                    "CLI {}\nEngine published version: {}\nMachine: {}\nCapabilities: {}",
                    env!("CARGO_PKG_VERSION"),
                    published.as_deref().unwrap_or("unavailable"),
                    info.device_id,
                    info.capabilities.join(", ")
                ),
                data,
            })
        }
        InspectCommand::Projects { target, after, .. } => {
            require_environment(&info, &target)?;
            let spaces: Vec<Space> = first_snapshot(&client, methods::WATCH_SPACES).await?;
            let mut projects: Vec<_> = spaces
                .into_iter()
                .filter(|space| space.device_id == target.machine)
                .collect();
            projects.sort_by(|a, b| a.id.cmp(&b.id));
            let total = projects.len();
            projects.retain(|space| after.as_ref().is_none_or(|cursor| space.id > *cursor));
            let truncated = projects.len() > limit.expect("validated list limit");
            projects.truncate(limit.expect("validated list limit"));
            let next_cursor = truncated.then(|| projects.last().expect("nonempty page").id.clone());
            let mut human = if projects.is_empty() {
                "No projects on this environment.".to_string()
            } else {
                projects
                    .iter()
                    .map(|space| {
                        format!(
                            "{}  {}  {}",
                            space.id,
                            one_line(space.display_name(), 120),
                            one_line(&space.path, 200)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if let Some(cursor) = &next_cursor {
                human.push_str(&format!("\nMore: --after {cursor}"));
            }
            Ok(Output {
                command: "projects",
                data: json!({
                    "machineId": target.machine,
                    "environmentId": target.environment,
                    "projects": projects,
                    "total": total,
                    "truncated": truncated,
                    "nextCursor": next_cursor,
                }),
                human,
            })
        }
        InspectCommand::Tasks { target, after, .. } => {
            require_environment(&info, &target.environment)?;
            let project = project(&client, &target).await?;
            let chats: Vec<Chat> = first_snapshot(&client, methods::WATCH_CHATS).await?;
            let mut tasks: Vec<_> = chats
                .into_iter()
                .filter(|chat| {
                    chat.device_id == target.environment.machine
                        && chat.space_id.as_deref() == Some(&target.project)
                })
                .collect();
            tasks.sort_by(|a, b| a.id.cmp(&b.id));
            let total = tasks.len();
            tasks.retain(|chat| after.as_ref().is_none_or(|cursor| chat.id > *cursor));
            let truncated = tasks.len() > limit.expect("validated list limit");
            tasks.truncate(limit.expect("validated list limit"));
            let next_cursor = truncated.then(|| tasks.last().expect("nonempty page").id.clone());
            let mut human = if tasks.is_empty() {
                format!("No tasks in project {}.", project.id)
            } else {
                tasks
                    .iter()
                    .map(|chat| {
                        format!(
                            "{}  {}{}",
                            chat.id,
                            one_line(chat.title.as_deref().unwrap_or("Untitled"), 160),
                            if chat.archived { " [archived]" } else { "" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if let Some(cursor) = &next_cursor {
                human.push_str(&format!("\nMore: --after {cursor}"));
            }
            Ok(Output {
                command: "tasks",
                data: json!({
                    "machineId": target.environment.machine,
                    "environmentId": target.environment.environment,
                    "projectId": target.project,
                    "tasks": tasks,
                    "total": total,
                    "truncated": truncated,
                    "nextCursor": next_cursor,
                }),
                human,
            })
        }
        InspectCommand::Read {
            target,
            after_task_sequence,
            ..
        } => {
            require_environment(&info, &target.project.environment)?;
            project(&client, &target.project).await?;
            let chats: Vec<Chat> = first_snapshot(&client, methods::WATCH_CHATS).await?;
            let chat = chats
                .into_iter()
                .find(|chat| {
                    chat.id == target.task
                        && chat.device_id == target.project.environment.machine
                        && chat.space_id.as_deref() == Some(&target.project.project)
                })
                .ok_or_else(|| {
                    CliError::missing(format!(
                        "task {} was not found in project {}",
                        target.task, target.project.project
                    ))
                })?;
            let identity: Option<TaskIdentityView> = call(
                &client,
                methods::GET_TASK_IDENTITY,
                json!({ "taskId": target.task }),
            )
            .await?;
            if let Some(identity) = &identity {
                validate_identity(&target, identity)?;
            }
            let commands: TaskCommandRead = call(
                &client,
                methods::GET_TASK_COMMAND_SNAPSHOT,
                json!({ "taskId": target.task, "afterTaskSequence": after_task_sequence.unwrap_or(0), "limit": limit.expect("validated read limit") }),
            )
            .await?;
            validate_command_page(&target, &commands, limit.expect("validated read limit"))?;
            if let Some(identity) = &identity {
                validate_receipt_bindings(identity, &commands)?;
            }
            if let (Some(identity), Some(snapshot_binding)) =
                (&identity, commands.snapshot.binding_id.as_deref())
                && identity.task.current_binding_id.as_deref() != Some(snapshot_binding)
            {
                return Err(CliError::mismatch(
                    "task binding changed during read; refresh the task and retry",
                ));
            }
            let identity_summary = identity.as_ref().map(|view| {
                json!({
                    "taskId": view.task.task_id,
                    "origin": view.task.origin,
                    "workspaceId": view.workspace.workspace_id,
                    "environmentId": view.workspace.environment_id,
                    "currentPath": view.workspace.current_path,
                    "bindingCount": view.bindings.len(),
                    "currentBinding": view.current_binding.as_ref().map(|binding| json!({
                        "bindingId": binding.binding_id,
                        "generation": binding.generation,
                        "providerId": binding.provider_id,
                        "reconciliation": binding.reconciliation.state,
                    })),
                })
            });
            let human = format!(
                "{}  {}\nProject: {}\nMachine/environment: {}\nIdentity: {}\nLast activity: {}\nCommand receipts: {}{}\nTranscript: unavailable (current RPC has only a full watch)",
                chat.id,
                one_line(chat.title.as_deref().unwrap_or("Untitled"), 160),
                target.project.project,
                target.project.environment.machine,
                if identity.is_some() {
                    "available"
                } else {
                    "unavailable"
                },
                one_line(chat.last_message_preview.as_deref().unwrap_or("none"), 200),
                commands.events.len(),
                if commands.has_more {
                    " (more available)"
                } else {
                    ""
                },
            );
            Ok(Output {
                command: "read",
                data: json!({
                    "machineId": target.project.environment.machine,
                    "environmentId": target.project.environment.environment,
                    "projectId": target.project.project,
                    "taskId": target.task,
                    "task": chat,
                    "identity": identity_summary,
                    "commands": commands,
                    "transcript": { "available": false, "reason": "current RPC exposes an unbounded transcript watch" },
                }),
                human,
            })
        }
    }
}

fn checked_limit(limit: u16) -> Result<usize, CliError> {
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(CliError::new(
            "invalidArgument",
            2,
            format!("--limit must be between 1 and {MAX_LIMIT}"),
        ));
    }
    Ok(usize::from(limit))
}

fn validate_identity(target: &TaskTarget, view: &TaskIdentityView) -> Result<(), CliError> {
    if view.task.task_id != target.task || view.task.workspace_id != view.workspace.workspace_id {
        return Err(CliError::protocol(
            "GetTaskIdentity returned an identity for another task or workspace",
        ));
    }
    if view.workspace.environment_id != target.project.environment.environment
        || view.workspace.source_space_id.as_deref() != Some(&target.project.project)
    {
        return Err(CliError::mismatch(
            "task identity no longer matches the requested environment/project; refresh the list",
        ));
    }
    if view
        .bindings
        .iter()
        .any(|binding| binding.task_id != target.task)
    {
        return Err(CliError::protocol(
            "GetTaskIdentity contains a binding for another task",
        ));
    }
    if let Some(binding) = &view.current_binding {
        if binding.task_id != target.task
            || view.task.current_binding_id.as_deref() != Some(&binding.binding_id)
        {
            return Err(CliError::protocol(
                "GetTaskIdentity returned a current binding for another task",
            ));
        }
        if binding.environment_id != target.project.environment.environment
            || binding.workspace_id != view.workspace.workspace_id
        {
            return Err(CliError::mismatch(
                "current task binding no longer matches the requested environment/project; refresh the task",
            ));
        }
    }
    Ok(())
}

fn validate_command_page(
    target: &TaskTarget,
    page: &TaskCommandRead,
    limit: usize,
) -> Result<(), CliError> {
    if page.snapshot.task_id != target.task || page.events.len() > limit {
        return Err(CliError::protocol(
            "GetTaskCommandSnapshot returned another task or exceeded the requested limit",
        ));
    }
    if page.events.iter().any(|event| {
        event.receipt.task_id != target.task
            || event.receipt.snapshot.task_id != target.task
            || event.command_id != event.receipt.command_id
    }) {
        return Err(CliError::protocol(
            "GetTaskCommandSnapshot contains a receipt for another task or command",
        ));
    }
    // A receipt's execution_environment records where that historical command
    // ran; it may legitimately differ after a task handoff.
    Ok(())
}

fn validate_receipt_bindings(
    identity: &TaskIdentityView,
    page: &TaskCommandRead,
) -> Result<(), CliError> {
    for event in &page.events {
        let snapshot = &event.receipt.snapshot;
        let Some(binding_id) = snapshot.binding_id.as_deref() else {
            continue;
        };
        let binding = identity
            .bindings
            .iter()
            .chain(identity.current_binding.iter())
            .find(|binding| binding.binding_id == binding_id)
            .ok_or_else(|| {
                CliError::protocol("command receipt references a binding outside this task")
            })?;
        if snapshot
            .binding_generation
            .is_some_and(|generation| generation != binding.generation)
        {
            return Err(CliError::protocol(
                "command receipt binding generation does not match task identity",
            ));
        }
    }
    Ok(())
}

fn one_line(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .take(max_chars)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn require_machine(info: &EngineInfo, expected: Option<&str>) -> Result<(), CliError> {
    if expected.is_some_and(|expected| expected != info.device_id) {
        return Err(CliError::mismatch(format!(
            "connected to machine {}; requested {}. Remote CLI routing is unavailable.",
            info.device_id,
            expected.unwrap_or_default()
        )));
    }
    Ok(())
}

fn require_environment(info: &EngineInfo, target: &EnvironmentTarget) -> Result<(), CliError> {
    require_machine(info, Some(&target.machine))?;
    // Today the accepted local task environment is the owning device ID. L29
    // must supply authenticated remote environment selection before this grows.
    if target.environment != info.device_id {
        return Err(CliError::mismatch(format!(
            "environment {} is unavailable on connected machine {}; use the local machine ID for --environment",
            target.environment, info.device_id
        )));
    }
    Ok(())
}

async fn project(client: &RpcClient, target: &ProjectTarget) -> Result<Space, CliError> {
    let spaces: Vec<Space> = first_snapshot(client, methods::WATCH_SPACES).await?;
    spaces
        .into_iter()
        .find(|space| space.id == target.project && space.device_id == target.environment.machine)
        .ok_or_else(|| {
            CliError::missing(format!(
                "project {} was not found on machine {}",
                target.project, target.environment.machine
            ))
        })
}

async fn engine_ready(client: &RpcClient) -> Result<bool, CliError> {
    let value: Value = call(client, methods::ENGINE_READY, json!({})).await?;
    value["ready"]
        .as_bool()
        .ok_or_else(|| CliError::protocol("EngineReady response has no boolean ready field"))
}

async fn call<T: DeserializeOwned>(
    client: &RpcClient,
    method: &str,
    params: Value,
) -> Result<T, CliError> {
    tokio::time::timeout(TIMEOUT, client.call_as(method, params))
        .await
        .map_err(|_| CliError::offline(format!("{method} timed out")))?
        .map_err(CliError::from)
}

async fn first_snapshot<T: DeserializeOwned>(
    client: &RpcClient,
    method: &str,
) -> Result<T, CliError> {
    let mut stream = tokio::time::timeout(TIMEOUT, client.subscribe_checked(method, json!({})))
        .await
        .map_err(|_| CliError::offline(format!("{method} did not start")))?
        .map_err(CliError::from)?;
    let value = tokio::time::timeout(TIMEOUT, stream.recv())
        .await
        .map_err(|_| CliError::offline(format!("{method} snapshot timed out")))?
        .ok_or_else(|| CliError::offline(format!("{method} stream closed before its snapshot")))?;
    serde_json::from_value(value)
        .map_err(|error| CliError::protocol(format!("{method} snapshot is malformed: {error}")))
}
