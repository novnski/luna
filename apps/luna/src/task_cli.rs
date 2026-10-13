//! Deliberate task commands for the running local engine (CORE-05). They use
//! the public SDK, the command boundary the app uses too: the engine admits,
//! fences and records each command, and a refusal is a durable receipt that
//! is printed like a success. Targets are exact, as for `luna inspect`.

use std::time::Duration;

use clap::{Args, Subcommand};
use luna_proto::{CommandReceipt, CommandReceiptStatus};
use serde_json::{Value, json};

use crate::inspect_cli::{self, CliError, TaskTarget};

/// The `clientId` every CLI receipt records.
const CLIENT_ID: &str = "luna-cli";
/// How long a pending receipt is polled before the CLI reports it pending.
const SETTLE: Duration = Duration::from_secs(10);

#[derive(Debug, Args)]
pub struct TaskArgs {
    /// Emit a versioned JSON object, including for failures.
    #[arg(long, global = true)]
    json: bool,
    /// Local engine IPC port (defaults to LUNA_IPC_PORT or 59778).
    #[arg(long, global = true)]
    port: Option<u16>,
    #[command(subcommand)]
    command: TaskCommand,
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    /// Rename one exact task and print its durable receipt.
    Rename {
        #[command(flatten)]
        target: TaskTarget,
        #[arg(long)]
        title: String,
        /// Idempotency key. Reuse it to retry or to read the outcome with
        /// `luna inspect receipt`; a fresh one is minted when omitted.
        #[arg(long)]
        command_id: Option<String>,
        /// Fence on this task revision instead of the current one.
        #[arg(long)]
        expected_revision: Option<u64>,
    },
}

/// A command's printed result. A receipt that is not `applied` is still
/// data, with a nonzero exit: 10 refused, 11 still pending.
struct Outcome {
    command: &'static str,
    data: Value,
    human: String,
    failure: Option<CliError>,
}

/// The `luna inspect` envelope. A failure keeps `error` and adds `command`
/// and `data` when a receipt or command id exists.
pub async fn run(args: TaskArgs) -> i32 {
    let json_mode = args.json;
    let (command, data, human, failure) = match execute(args).await {
        Ok(outcome) => (
            Some(outcome.command),
            Some(outcome.data),
            Some(outcome.human),
            outcome.failure,
        ),
        Err(error) => (None, None, None, Some(error)),
    };
    let Some(error) = failure else {
        if json_mode {
            println!(
                "{}",
                json!({ "schemaVersion": 1, "command": command, "data": data })
            );
        } else {
            println!("{}", human.unwrap_or_default());
        }
        return 0;
    };
    if json_mode {
        let mut body = json!({
            "schemaVersion": 1,
            "error": { "kind": error.kind, "message": error.message },
        });
        if let Some(command) = command {
            body["command"] = command.into();
        }
        if let Some(data) = data.or(error.data) {
            body["data"] = data;
        }
        println!("{body}");
    } else {
        if let Some(human) = human {
            println!("{human}");
        }
        eprintln!("luna task: {}", error.message);
    }
    error.code
}

async fn execute(args: TaskArgs) -> Result<Outcome, CliError> {
    match args.command {
        TaskCommand::Rename {
            target,
            title,
            command_id,
            expected_revision,
        } => {
            // The same normalization as the app's rename dialog.
            let title = title.trim().to_owned();
            if title.is_empty() {
                return Err(CliError::new(
                    "invalidArgument",
                    2,
                    "--title must not be empty",
                ));
            }
            let (client, info) = inspect_cli::connect(args.port).await?;
            inspect_cli::require_environment(&info, &target.project.environment)?;
            inspect_cli::project(&client, &target.project).await?;
            inspect_cli::find_task(&client, &target).await?;
            let mut sdk = luna_sdk::Client::new(&client).with_deadline(inspect_cli::TIMEOUT);
            sdk.negotiate().await?;
            let actor = luna_sdk::Actor::new(info.device_id.clone(), CLIENT_ID);
            let mut command = luna_sdk::TaskCommand::new(
                actor,
                target.task.clone(),
                luna_sdk::TaskIntent::Rename {
                    title: title.clone(),
                },
            );
            if let Some(command_id) = command_id {
                command = command.with_command_id(command_id);
            }
            if let Some(revision) = expected_revision {
                let mut snapshot = sdk
                    .task_command_snapshot(&target.task, None, None)
                    .await?
                    .snapshot;
                snapshot.task_revision = revision;
                command = command.with_fencing(luna_sdk::Fencing::Reviewed(snapshot));
            }
            let receipt = sdk.execute(&command).await?;
            let receipt = sdk.wait_for_terminal(receipt, None, SETTLE).await?;
            Ok(outcome(
                "task.rename",
                json!({
                    "machineId": target.project.environment.machine,
                    "environmentId": target.project.environment.environment,
                    "projectId": target.project.project,
                    "taskId": target.task,
                    "title": title,
                    "receipt": receipt,
                }),
                &receipt,
                format!("Renamed task {} to {title:?}", target.task),
            ))
        }
    }
}

fn outcome(
    command: &'static str,
    data: Value,
    receipt: &CommandReceipt,
    applied: String,
) -> Outcome {
    let summary = format!(
        "command {}  {}  revision {}",
        receipt.command_id,
        inspect_cli::wire_name(&receipt.status),
        receipt.snapshot.task_revision
    );
    let failure = match receipt.status {
        CommandReceiptStatus::Applied => None,
        CommandReceiptStatus::Pending => Some(CliError::new(
            "commandPending",
            11,
            format!(
                "command {} is still pending; read it with `luna inspect receipt --command-id {}`",
                receipt.command_id, receipt.command_id
            ),
        )),
        _ => Some(CliError::new(
            "commandRefused",
            10,
            receipt.reason.as_ref().map_or_else(
                || format!("the engine refused command {}", receipt.command_id),
                |reason| {
                    format!(
                        "{}: {}",
                        inspect_cli::wire_name(&reason.code),
                        reason.message
                    )
                },
            ),
        )),
    };
    Outcome {
        command,
        data,
        human: if failure.is_none() {
            format!("{applied}\n{summary}")
        } else {
            summary
        },
        failure,
    }
}
