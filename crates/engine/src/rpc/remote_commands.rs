//! RM-04 `ExecuteRemoteTaskCommand`: a paired phone's typed task command,
//! admitted for the device session the per-connection remote guard
//! validated (`luna_rpc::remote_auth::RemotePrincipal`). The wire contract
//! is `luna_proto::remote_command`; the failure contract is
//! `docs/pending-tasks/control-this-mac/rm-04/README.md`.
//!
//! A remote command enters the one task-command pipeline local commands use
//! (`execute_task_command_with_identity`): the same durable receipt store,
//! fences, task lane, local executable grants and session-document
//! executor. It never becomes an executable synced row of its own. Admission
//! here adds what a remote principal needs before any of that runs:
//!
//! 1. the grant as it stands now (`validate_session`, `tasks-operate`) and
//!    the host environment the session belongs to;
//! 2. the typed, closed intent set and its bounds (`RemoteTaskCommand::validate`);
//! 3. the command id bound to this principal and its payload identity (the
//!    receipt's `payloadHash`, whatever the translation): a retry returns
//!    the recorded receipt (never re-validated or re-translated), a changed
//!    payload records `commandIdConflict`, another principal's id is
//!    refused, and admissions of one id are serialized so concurrent retries
//!    are never translated twice;
//! 4. the exact target: a task this host lists, or (to create one) a listed
//!    project and a fresh task id; a new worktree only for a task's first
//!    message;
//! 5. a translation into the engine intent with the principal as the
//!    receipt's actor and the phone's fences unchanged. A send to an idle
//!    task starts its next turn (a Run, as the desktop sends one); a send to
//!    a busy task (a running turn, or an earlier send still preparing its
//!    worktree and setup) waits in the durable queue, which drains it only
//!    behind that setup and holds it if the setup fails.
//!
//! Execution is detached from the caller: once admission begins it
//! completes even if the phone's socket closes or the call is cancelled, so
//! a lost connection leaves a readable receipt instead of a half-admitted
//! command. The engine's command admission guard travels with it.

use super::*;
use luna_proto::remote_command::{
    REMOTE_CLIENT_ID, RemoteFollowUp, RemoteNewWorktree, RemotePermissionMode, RemoteTaskCommand,
    RemoteTaskIntent,
};
use luna_rpc::remote_auth::{RemotePrincipal, RemoteScope, remote_command_identity};

pub(super) fn handles(method: &str) -> bool {
    method == methods::EXECUTE_REMOTE_TASK_COMMAND
}

fn denied(reason: &str) -> RpcError {
    RpcError::Failed(format!("remote authorization denied: {reason}"))
}

fn failed(error: impl std::fmt::Display) -> RpcError {
    RpcError::Failed(error.to_string())
}

/// Why a remote command is not translated: a call the host refuses outright,
/// or a refusal recorded as the command's own durable receipt (typed, so a
/// retry of the same command reads the same answer).
enum Refusal {
    Call(RpcError),
    Receipt {
        code: CommandReasonCode,
        message: String,
        recovery: &'static str,
    },
}

impl From<RpcError> for Refusal {
    fn from(error: RpcError) -> Self {
        Self::Call(error)
    }
}

/// The lock that serializes admissions of one command id in this process.
fn admission_lock(command_id: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, OnceLock, Weak};
    static LOCKS: OnceLock<std::sync::Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(command_id).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(command_id.to_owned(), Arc::downgrade(&lock));
    lock
}

/// The new worktree's base when the phone names none: the project's `HEAD`.
const DEFAULT_WORKTREE_BASE: &str = "HEAD";

/// `EngineRpc::handle_remote` for `ExecuteRemoteTaskCommand`.
pub(super) async fn admit(
    rpc: std::sync::Arc<EngineRpc>,
    principal: RemotePrincipal,
    method: String,
    params: Value,
) -> Result<RpcReply, RpcError> {
    if !handles(&method) {
        return Err(RpcError::UnknownMethod(method));
    }
    // CORE-03: a command runs only while this engine admits work.
    let admitted = rpc.admission.admit()?;
    let command: RemoteTaskCommand = parse_params(params)?;
    command.validate().map_err(RpcError::BadParams)?;
    rpc.check_remote_principal(&principal)?;
    let identity = remote_command_identity(&command);
    let actor = principal.actor_id();
    // One admission per command id at a time: a concurrent retry of the
    // same command waits, then finds the first one's receipt below, so the
    // same command is never translated (and so admitted) twice.
    let serialized = admission_lock(&command.command_id).lock_owned().await;
    if let Some(prior) = rpc
        .doc_host
        .task_command_receipt(&command.command_id)
        .map_err(failed)?
    {
        if prior.actor_id != actor {
            return Err(denied("this command id belongs to another client"));
        }
        if prior.task_id == command.task_id && prior.payload_hash == identity {
            // The same command again (a lost reply, a reconnect, a restart):
            // its receipt as recorded, whatever the task looks like now.
            return RpcReply::value(&prior);
        }
        let receipt = rpc
            .record_remote_conflict(&command, &principal, identity)
            .await?;
        return RpcReply::value(&receipt);
    }
    let params = match rpc.translate_remote_command(&command, &principal) {
        Ok(params) => params,
        Err(Refusal::Call(error)) => return Err(error),
        Err(Refusal::Receipt {
            code,
            message,
            recovery,
        }) => {
            let receipt = rpc
                .record_remote_refusal(&command, &principal, identity, code, &message, recovery)
                .await?;
            return RpcReply::value(&receipt);
        }
    };
    let stop = matches!(command.intent, RemoteTaskIntent::Stop {});
    let task_id = command.task_id.clone();
    let command_id = command.command_id.clone();
    let worker = rpc.clone();
    let execution = tokio::spawn(async move {
        let _admitted = admitted;
        let _serialized = serialized;
        let receipt = Box::pin(worker.execute_task_command_with_identity(
            params,
            false,
            None,
            None,
            Some(identity),
        ))
        .await?;
        if stop
            && matches!(
                receipt.status,
                CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
            )
        {
            // As a local Stop: a Board ticket worker's stop also stops that
            // ticket's own reviewer chats.
            worker.board_follow_chat_stop(&task_id, &command_id).await;
        }
        Ok::<_, RpcError>(receipt)
    });
    let receipt = execution.await.map_err(|error| {
        RpcError::Failed(format!("remote command admission stopped: {error}"))
    })??;
    if receipt.actor_id != actor || receipt.command_id != command.command_id {
        // Another principal's identical command took this id first.
        return Err(denied("this command id belongs to another client"));
    }
    RpcReply::value(&receipt)
}

impl EngineRpc {
    /// The grant as it stands at admission, not as the connection first saw
    /// it, for this host's own environment.
    fn check_remote_principal(&self, principal: &RemotePrincipal) -> Result<(), RpcError> {
        let auth = self
            .remote_auth
            .as_ref()
            .ok_or_else(|| denied("remote authority is unavailable"))?;
        let session = auth.validate_session(&principal.session_id)?;
        if !session.scopes.contains(&RemoteScope::TasksOperate) {
            return Err(denied("device session lacks required scope"));
        }
        if principal.environment != auth.environment()
            || principal.environment.device_id != self.engine_info.device_id
        {
            return Err(denied("target environment mismatch"));
        }
        Ok(())
    }

    /// The pending receipt a remote command would begin with.
    fn remote_pending_receipt(
        &self,
        command: &RemoteTaskCommand,
        principal: &RemotePrincipal,
        identity: String,
    ) -> Result<CommandReceipt, RpcError> {
        let snapshot = self
            .doc_host
            .task_command_snapshot(&command.task_id)
            .map_err(failed)?
            .snapshot;
        let now = crate::now_ms();
        Ok(CommandReceipt {
            command_id: command.command_id.clone(),
            task_id: command.task_id.clone(),
            intent: command.intent.kind().into(),
            payload_hash: identity,
            status: CommandReceiptStatus::Pending,
            snapshot,
            event_sequence: None,
            actor_id: principal.actor_id(),
            client_id: REMOTE_CLIENT_ID.into(),
            client_session_id: principal.session_id.clone(),
            execution_environment: Some(self.engine_info.device_id.clone()),
            correlation_id: None,
            reason: None,
            recovery_action: None,
            accepted_at_ms: now,
            committed_at_ms: None,
            observed_at_ms: now,
        })
    }

    /// The principal reused one of its command ids for another task or
    /// payload: record (once) and return the `commandIdConflict` receipt the
    /// local pipeline records for the same mistake. Nothing is translated or
    /// executed, and the original receipt is untouched.
    async fn record_remote_conflict(
        &self,
        command: &RemoteTaskCommand,
        principal: &RemotePrincipal,
        identity: String,
    ) -> Result<CommandReceipt, RpcError> {
        let _lane = self.doc_host.task_command_gate(&command.task_id).await;
        let pending = self.remote_pending_receipt(command, principal, identity)?;
        let begin = self
            .doc_host
            .begin_task_command(pending, None, None, None)
            .map_err(failed)?;
        match begin.kind {
            luna_sync::TaskCommandBeginKind::Conflict
            | luna_sync::TaskCommandBeginKind::Duplicate => {
                self.doc_host.publish_task_receipt(begin.receipt.clone());
                Ok(begin.receipt)
            }
            // The original receipt vanished between the read and this
            // write (its task was deleted): never leave an unexecuted
            // pending receipt behind.
            luna_sync::TaskCommandBeginKind::Accepted => self.finish_task_command(
                &command.command_id,
                CommandReceiptStatus::Rejected,
                false,
                CommandReasonCode::CommandIdConflict,
                "the command id changed owner while it was checked",
                "Mint a new command id and send again.",
            ),
        }
    }

    /// A command the host refuses before any provider work, recorded as its
    /// own rejected receipt with a typed reason (for example
    /// `permissionModeRequired`): the same command again reads the same
    /// answer, and nothing was created or run.
    async fn record_remote_refusal(
        &self,
        command: &RemoteTaskCommand,
        principal: &RemotePrincipal,
        identity: String,
        code: CommandReasonCode,
        message: &str,
        recovery: &str,
    ) -> Result<CommandReceipt, RpcError> {
        let _lane = self.doc_host.task_command_gate(&command.task_id).await;
        let pending = self.remote_pending_receipt(command, principal, identity)?;
        let begin = self
            .doc_host
            .begin_task_command(pending, None, None, None)
            .map_err(failed)?;
        match begin.kind {
            luna_sync::TaskCommandBeginKind::Accepted => self.finish_task_command(
                &command.command_id,
                CommandReceiptStatus::Rejected,
                false,
                code,
                message,
                recovery,
            ),
            luna_sync::TaskCommandBeginKind::Conflict
            | luna_sync::TaskCommandBeginKind::Duplicate => {
                self.doc_host.publish_task_receipt(begin.receipt.clone());
                Ok(begin.receipt)
            }
        }
    }

    /// The exact target and the engine intent, with the principal as actor.
    fn translate_remote_command(
        &self,
        command: &RemoteTaskCommand,
        principal: &RemotePrincipal,
    ) -> Result<TaskCommandParams, Refusal> {
        let projections = self.remote_projections.as_ref().ok_or_else(|| {
            RpcError::Failed("Remote task control is unavailable on this engine".into())
        })?;
        let intent = if let RemoteTaskIntent::CreateTask {
            project_id,
            permission_mode,
        } = &command.intent
        {
            let project = projections
                .listed_project(project_id)
                .ok_or_else(|| denied("the project is not on this Mac"))?;
            if self.task_identity.view(&command.task_id).is_some()
                || self
                    .workspace
                    .chat(&command.task_id)
                    .map_err(failed)?
                    .is_some()
            {
                return Err(RpcError::BadParams(
                    "a task with this id already exists; mint a new task id".into(),
                )
                .into());
            }
            let config = self.remote_task_config(&command.task_id, *permission_mode)?;
            TaskCommandIntent::CreateChat {
                side_parent_id: None,
                space_id: Some(project.id),
                device_id: None,
                config: Some(config),
                branch: None,
                cwd: None,
                lineage: TaskLineage::default(),
            }
        } else {
            // Never open (and so create) a document for a task this host
            // does not list, and never command another device's task.
            if !projections.lists_task(&command.task_id) {
                return Err(denied("the task is not on this Mac").into());
            }
            self.remote_task_intent(command, projections)?
        };
        Ok(TaskCommandParams {
            command_id: command.command_id.clone(),
            actor_id: principal.actor_id(),
            client_id: REMOTE_CLIENT_ID.into(),
            client_session_id: principal.session_id.clone(),
            task_id: command.task_id.clone(),
            expected_task_revision: command.expected_task_revision,
            expected_binding_generation: command.expected_binding_generation,
            expected_turn_id: command.expected_turn_id.clone(),
            issued_at_ms: command.issued_at_ms.unwrap_or_else(crate::now_ms),
            intent,
        })
    }

    /// A phone-created task's configuration, by the desktop's new-task rule:
    /// the host's provider for a new task, its saved default model (else its
    /// catalog's first), and a permission mode that is never legacy Ask. The
    /// phone's `full`/`auto` is used when the provider offers it; with none,
    /// Auto when the provider and model support it. Otherwise there is no
    /// safe default and the create is refused (`permissionModeRequired`)
    /// before anything is created or run.
    fn remote_task_config(
        &self,
        task_id: &str,
        requested: Option<RemotePermissionMode>,
    ) -> Result<ChatConfig, Refusal> {
        // A task that does not exist yet resolves to the engine's provider.
        let harness = self.doc_host.harness_for(task_id);
        let saved = self
            .provider_defaults
            .as_ref()
            .and_then(|defaults| defaults.get().ok())
            .and_then(|snapshot| snapshot.providers.get(&harness).cloned())
            .unwrap_or_default();
        let model = saved.model.clone().or_else(|| {
            self.model_cache
                .cached(harness, None)
                .and_then(|models| models.first().map(|model| model.id.clone()))
        });
        let capabilities = crate::provider_capabilities_for(harness, model.as_deref());
        let required = |message: String| Refusal::Receipt {
            code: CommandReasonCode::PermissionModeRequired,
            message,
            recovery: "Create the task again with Full access, or choose a provider and model that offer Auto on the Mac.",
        };
        let model_name = model.as_deref().unwrap_or("its default model");
        let mode = match requested {
            Some(RemotePermissionMode::Full) => PermissionMode::Full,
            Some(RemotePermissionMode::Auto) => PermissionMode::Auto,
            None if capabilities.supports(PermissionMode::Auto) => PermissionMode::Auto,
            None => {
                return Err(required(format!(
                    "{harness} with {model_name} has no automatic reviewer, so a new task needs an explicit permission mode"
                )));
            }
        };
        if !capabilities.supports(mode) {
            let label = if mode == PermissionMode::Full {
                "Full access"
            } else {
                "Auto"
            };
            return Err(required(format!(
                "{harness} with {model_name} does not offer {label}"
            )));
        }
        Ok(ChatConfig {
            harness,
            provider_account_id: None,
            model,
            reasoning: saved.reasoning,
            model_options: saved.model_options,
            sandbox: luna_proto::SandboxLevel::WorkspaceWrite,
            permission_mode: mode,
            effective_permission_mode: mode,
            permission_ceiling_source: None,
            permission_degradation_reason: None,
            permission_audit: Vec::new(),
        })
    }

    fn remote_task_intent(
        &self,
        command: &RemoteTaskCommand,
        projections: &crate::remote_projections::RemoteProjections,
    ) -> Result<TaskCommandIntent, RpcError> {
        Ok(match &command.intent {
            RemoteTaskIntent::CreateTask { .. } => {
                unreachable!("createTask is translated with its project")
            }
            RemoteTaskIntent::Send {
                message_id,
                text,
                follow_up,
                new_worktree: None,
            } => {
                if self.sessions.turn_in_flight(&command.task_id)
                    || self
                        .doc_host
                        .has_pending_local_send(&command.task_id)
                        .map_err(failed)?
                {
                    // Busy (a running turn, or an earlier send still
                    // preparing its turn or worktree setup): it waits in the
                    // durable queue, in order, or steers the running turn.
                    TaskCommandIntent::QueueMessage {
                        message_id: message_id.clone(),
                        text: text.clone(),
                        attachments: Vec::new(),
                        input_items: Vec::new(),
                        hold_for_turn_end: true,
                        follow_up_behavior: match follow_up {
                            RemoteFollowUp::Queue => FollowUpBehavior::Queue,
                            RemoteFollowUp::Steer => FollowUpBehavior::Steer,
                        },
                        // The task's configuration at delivery, as for an
                        // agent's queued message; pickers are RM-05's.
                        execution: None,
                    }
                } else {
                    // Idle: it starts the next turn, as a desktop send does
                    // (and, like one, thaws a queue a Stop froze). The run
                    // still waits for the checkout's setup on the host.
                    let request = self
                        .doc_host
                        .request_from_chat_row(&command.task_id, text)
                        .ok_or_else(|| {
                            RpcError::Failed("the task's configuration is unavailable".into())
                        })?;
                    TaskCommandIntent::Session {
                        command: SessionCommandPayload::Run {
                            request,
                            message_id: message_id.clone(),
                        },
                        transfers: Vec::new(),
                    }
                }
            }
            RemoteTaskIntent::Send {
                message_id,
                text,
                new_worktree: Some(worktree),
                ..
            } => self.remote_first_run(command, projections, message_id, text, worktree)?,
            RemoteTaskIntent::RemoveQueued {
                message_id,
                expected_queue_revision,
            } => TaskCommandIntent::RemoveQueuedMessage {
                message_id: message_id.clone(),
                expected_queue_revision: *expected_queue_revision,
            },
            RemoteTaskIntent::SendQueuedNow {
                message_id,
                expected_queue_revision,
            } => TaskCommandIntent::SendQueuedNow {
                message_id: message_id.clone(),
                expected_queue_revision: Some(*expected_queue_revision),
            },
            RemoteTaskIntent::SteerQueuedNow {
                message_id,
                expected_queue_revision,
            } => TaskCommandIntent::SteerQueuedNow {
                message_id: message_id.clone(),
                expected_queue_revision: Some(*expected_queue_revision),
            },
            RemoteTaskIntent::Stop {} => TaskCommandIntent::Session {
                command: SessionCommandPayload::Interrupt {},
                transfers: Vec::new(),
            },
            RemoteTaskIntent::SetArchived { archived } => TaskCommandIntent::SetArchived {
                archived: *archived,
            },
            RemoteTaskIntent::CancelCapacityWait { command_id } => {
                TaskCommandIntent::CancelCapacityWait {
                    command_id: command_id.clone(),
                }
            }
        })
    }

    /// A task's first message in a new worktree of its project: the same
    /// Run a desktop sends, configured from the task's own row. Its worktree
    /// is materialized and set up on this host before the provider starts,
    /// and later messages queue behind it (`DocHost::drain_queue`).
    fn remote_first_run(
        &self,
        command: &RemoteTaskCommand,
        projections: &crate::remote_projections::RemoteProjections,
        message_id: &str,
        text: &str,
        worktree: &RemoteNewWorktree,
    ) -> Result<TaskCommandIntent, RpcError> {
        if !self.remote_task_never_started(&command.task_id)? {
            return Err(RpcError::BadParams(
                "only a task's first message can start a new worktree".into(),
            ));
        }
        let project = self
            .workspace
            .chat(&command.task_id)
            .map_err(failed)?
            .and_then(|chat| chat.space_id)
            .and_then(|project| projections.listed_project(&project))
            .ok_or_else(|| denied("the task's project is not on this Mac"))?;
        // A folder that is not a git repository fails materialization on
        // the host with that reason on this command's receipt.
        let mut request = self
            .doc_host
            .request_from_chat_row(&command.task_id, text)
            .ok_or_else(|| RpcError::Failed("the task's configuration is unavailable".into()))?;
        request.worktree = Some(luna_proto::WorktreeSpec {
            repo_path: project.path,
            base: worktree
                .base
                .clone()
                .unwrap_or_else(|| DEFAULT_WORKTREE_BASE.into()),
        });
        Ok(TaskCommandIntent::Session {
            command: SessionCommandPayload::Run {
                request,
                message_id: message_id.to_owned(),
            },
            transfers: Vec::new(),
        })
    }

    /// No provider binding, run, transcript, queued message or command yet.
    fn remote_task_never_started(&self, task_id: &str) -> Result<bool, RpcError> {
        if self
            .task_identity
            .view(task_id)
            .is_some_and(|view| view.current_binding.is_some() || !view.bindings.is_empty())
            || self.sessions.last_request(task_id).is_some()
            || self.sessions.has_live_run(task_id)
        {
            return Ok(false);
        }
        let handle = self.doc_host.open(task_id).map_err(failed)?;
        let doc = handle.doc();
        Ok(doc.read_entries().map_err(failed)?.is_empty()
            && doc.read_queue().map_err(failed)?.is_empty()
            && doc.read_commands().map_err(failed)?.is_empty())
    }
}
