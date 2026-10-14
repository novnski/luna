//! RM-04: the paired-phone task-command contract (`ExecuteRemoteTaskCommand`).
//!
//! A phone never writes an executable document row. It sends one typed,
//! bounded command over its authenticated socket; the host's per-connection
//! remote guard attaches the device session it validated for that very call,
//! and the engine admits the command into the same durable receipt store and
//! session-document executor that local commands use. The closed
//! [`RemoteTaskIntent`] set is everything a paired phone can ask for: rename,
//! permission and provider changes, deletion, child chats, history rewrites
//! and interaction answers are not representable here.
//!
//! Idempotency: [`RemoteTaskCommand::command_id`] is minted once on the phone
//! before the first send and kept through every retry. The host records the
//! command's payload identity (`luna_rpc::remote_auth::remote_command_identity`:
//! the task id and the intent, canonically encoded) as the receipt's
//! `payloadHash`. The same id with the same payload returns the original
//! receipt; the same id with another payload is `commandIdConflict`; the same
//! id from another paired session is refused. Fences (`expected*`) are judged
//! at first admission only, exactly as for local commands.

use serde::{Deserialize, Serialize};

/// The host admits [`RemoteTaskCommand`]s, advertised as an engine capability
/// and in every projection's host capabilities.
pub const REMOTE_TASK_CONTROL_V1: &str = "remote-task-control-v1";

/// Most bytes of a command id.
pub const MAX_COMMAND_ID_BYTES: usize = 128;
/// Most bytes of a task, project or message id (the projections' bound).
pub const MAX_TARGET_ID_BYTES: usize = crate::remote_projection::MAX_ID_BYTES;
/// Most bytes of one sent message.
pub const MAX_MESSAGE_TEXT_BYTES: usize = 64 * 1024;
/// Most bytes of a new worktree's base ref.
pub const MAX_BASE_REF_BYTES: usize = 200;

/// Every remote receipt's `clientId`.
pub const REMOTE_CLIENT_ID: &str = "luna-remote";
/// A remote receipt's `actorId` is this prefix and the device session id.
pub const REMOTE_ACTOR_PREFIX: &str = "remote-session:";

/// The receipt actor of every command a device session issues.
pub fn remote_actor_id(session_id: &str) -> String {
    format!("{REMOTE_ACTOR_PREFIX}{session_id}")
}

/// `ExecuteRemoteTaskCommand` parameters: one phone command. Unknown fields
/// (an `actorId`, a `clientId`, a raw engine intent) are refused, never
/// ignored: the principal comes only from the authenticated connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteTaskCommand {
    /// Stable idempotency key: minted once before the first send, reused
    /// for every retry and receipt read. 1-128 bytes of `[A-Za-z0-9._:-]`.
    pub command_id: String,
    /// The target task. For `createTask`, the new task's id: a UUID the
    /// phone mints, so a retried create names the same task.
    pub task_id: String,
    /// `expectedTaskRevision`: required by `setArchived`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_task_revision: Option<u64>,
    /// `expectedBindingGeneration`: required once the task has a provider
    /// binding, except for `createTask` and `cancelCapacityWait`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_binding_generation: Option<u32>,
    /// `expectedTurnId`: required by `stop` and `steerQueuedNow`, and by
    /// `sendQueuedNow` while a turn runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_turn_id: Option<String>,
    /// The phone's issue time (Unix ms); the host clamps it to its own clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at_ms: Option<i64>,
    pub intent: RemoteTaskIntent,
}

/// How a message sent while a turn runs waits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum RemoteFollowUp {
    /// Wait for the running turn to end, then start the next one.
    #[default]
    Queue,
    /// Steer the running turn when its provider takes prompts mid-turn;
    /// otherwise it stays queued until an explicit send.
    Steer,
}

/// A task's first message runs in a new git worktree of its project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteNewWorktree {
    /// The ref the worktree's new branch starts from; the project's current
    /// `HEAD` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

/// The permission mode a phone may choose for a task it creates. New work
/// has only these two: Full access never asks, and Auto lets the provider's
/// own reviewer decide. Legacy Ask is not representable (it is refused as a
/// malformed command).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum RemotePermissionMode {
    Full,
    Auto,
}

/// What a paired phone may ask its Mac to do with one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RemoteTaskIntent {
    /// Create the command's task in a project this host lists. Its folder
    /// and provider come from the host, never the phone. `permissionMode`
    /// (`full` or `auto`) is recorded in the task's configuration; without
    /// one the host uses Auto when its provider and model support it, and
    /// otherwise refuses the create (`permissionModeRequired`) before any
    /// provider work.
    #[serde(rename_all = "camelCase")]
    CreateTask {
        project_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<RemotePermissionMode>,
    },
    /// Send a message (its id becomes the transcript message id). To an
    /// idle task it starts the next turn, as a desktop send does; while the
    /// task is busy (a running turn, or an earlier send still preparing its
    /// turn or worktree) it waits in the host's durable queue (`queue`) or
    /// steers the running turn (`steer`). With `newWorktree` (only for a task
    /// that has never run) it starts the task's first turn in a fresh
    /// worktree; later messages wait behind that worktree's setup and are
    /// held if the setup fails or is cancelled.
    #[serde(rename_all = "camelCase")]
    Send {
        message_id: String,
        text: String,
        #[serde(default)]
        follow_up: RemoteFollowUp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_worktree: Option<RemoteNewWorktree>,
    },
    /// Remove a queued message, fenced by its queue revision.
    #[serde(rename_all = "camelCase")]
    RemoveQueued {
        message_id: String,
        expected_queue_revision: u64,
    },
    /// Interrupt the running turn (if any) and send this queued message now.
    #[serde(rename_all = "camelCase")]
    SendQueuedNow {
        message_id: String,
        expected_queue_revision: u64,
    },
    /// Steer this queued message into the running turn without stopping it.
    #[serde(rename_all = "camelCase")]
    SteerQueuedNow {
        message_id: String,
        expected_queue_revision: u64,
    },
    /// Stop the expected running turn. Accepted work elsewhere is untouched.
    Stop {},
    /// Archive or restore the task.
    #[serde(rename_all = "camelCase")]
    SetArchived { archived: bool },
    /// Withdraw a send that started a turn (its receipt is `run`) while it
    /// waits for a free turn on the host. A send that started is refused.
    #[serde(rename_all = "camelCase")]
    CancelCapacityWait { command_id: String },
}

impl RemoteTaskIntent {
    /// The wire `kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::CreateTask { .. } => "createTask",
            Self::Send { .. } => "send",
            Self::RemoveQueued { .. } => "removeQueued",
            Self::SendQueuedNow { .. } => "sendQueuedNow",
            Self::SteerQueuedNow { .. } => "steerQueuedNow",
            Self::Stop {} => "stop",
            Self::SetArchived { .. } => "setArchived",
            Self::CancelCapacityWait { .. } => "cancelCapacityWait",
        }
    }

    /// Controls that act on the task's current turn or queue head. A phone
    /// sends them only on a live connection with fences read at that moment,
    /// never later from an offline outbox: a stale stop or steer must not
    /// reach a newer turn.
    pub fn live_only(&self) -> bool {
        matches!(
            self,
            Self::Stop {}
                | Self::SendQueuedNow { .. }
                | Self::SteerQueuedNow { .. }
                | Self::Send {
                    follow_up: RemoteFollowUp::Steer,
                    ..
                }
        )
    }
}

impl RemoteTaskCommand {
    /// The shape every host and client enforces before anything is sent or
    /// admitted: bounded, printable ids and text, a UUID for a new task, a
    /// plain ref for a worktree base. Returns the first problem.
    pub fn validate(&self) -> Result<(), String> {
        if !command_id_ok(&self.command_id) {
            return Err(format!(
                "a command id is 1-{MAX_COMMAND_ID_BYTES} bytes of letters, digits and . _ : -"
            ));
        }
        if !target_id_ok(&self.task_id) {
            return Err(format!(
                "a task id is 1-{MAX_TARGET_ID_BYTES} printable bytes"
            ));
        }
        if self
            .expected_turn_id
            .as_deref()
            .is_some_and(|turn| !target_id_ok(turn))
        {
            return Err("the expected turn id is malformed".into());
        }
        match &self.intent {
            RemoteTaskIntent::CreateTask { project_id, .. } => {
                if uuid::Uuid::parse_str(&self.task_id).is_err() {
                    return Err("a new task's id must be a UUID".into());
                }
                if !target_id_ok(project_id) {
                    return Err("the project id is malformed".into());
                }
            }
            RemoteTaskIntent::Send {
                message_id,
                text,
                new_worktree,
                follow_up,
            } => {
                if !target_id_ok(message_id) {
                    return Err("the message id is malformed".into());
                }
                if text.trim().is_empty() {
                    return Err("a message needs text".into());
                }
                if text.len() > MAX_MESSAGE_TEXT_BYTES {
                    return Err(format!(
                        "a message is at most {MAX_MESSAGE_TEXT_BYTES} bytes"
                    ));
                }
                if let Some(worktree) = new_worktree {
                    if *follow_up != RemoteFollowUp::Queue {
                        return Err("a first message in a new worktree cannot steer".into());
                    }
                    if worktree
                        .base
                        .as_deref()
                        .is_some_and(|base| !base_ref_ok(base))
                    {
                        return Err("the worktree base is not a plain git ref".into());
                    }
                }
            }
            RemoteTaskIntent::RemoveQueued { message_id, .. }
            | RemoteTaskIntent::SendQueuedNow { message_id, .. }
            | RemoteTaskIntent::SteerQueuedNow { message_id, .. } => {
                if !target_id_ok(message_id) {
                    return Err("the message id is malformed".into());
                }
            }
            RemoteTaskIntent::CancelCapacityWait { command_id } => {
                if !command_id_ok(command_id) {
                    return Err("the withdrawn command id is malformed".into());
                }
            }
            RemoteTaskIntent::Stop {} | RemoteTaskIntent::SetArchived { .. } => {}
        }
        Ok(())
    }
}

fn command_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_COMMAND_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn target_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_TARGET_ID_BYTES
        && id.bytes().all(|byte| byte.is_ascii_graphic())
}

/// A ref git reads as a revision, never as an option or a path escape.
fn base_ref_ok(base: &str) -> bool {
    !base.is_empty()
        && base.len() <= MAX_BASE_REF_BYTES
        && !base.starts_with('-')
        && !base.starts_with('/')
        && !base.ends_with('/')
        && !base.contains("..")
        && !base.contains("//")
        && base.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'/' | b'-' | b'+' | b'@' | b'~' | b'^')
        })
}
