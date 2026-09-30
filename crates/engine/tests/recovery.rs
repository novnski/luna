//! L10 recovery through the production task-command, session, journal, and
//! session-watch boundaries. Provider homes and effects are synthetic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use luna_doc::{
    MessagePart, MessageRole, SessionCommandEntry, SessionCommandPayload, SessionCommandStatus,
};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry, RunJournal};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, ChatConfig, CommandReceipt, CommandReceiptStatus, DoneStatus, HarnessId, Model,
    PermissionMode, ReasoningLevel, RecoveryAcceptance, RecoveryAction, RecoveryStatus, RunRequest,
    SandboxLevel, SteeringMode, TaskCommandRead,
};
use luna_rpc::{RpcClient, methods};
use luna_sync::{DocsStore, TaskRecoveryExpectation, TaskRecoveryTransition};

const TASK: &str = "l10-recovery-task";
const MESSAGE: &str = "l10-original-message";

#[derive(Clone, Copy)]
enum FirstOutcome {
    RejectedOverload,
    AcceptedOverload,
    AcceptedOverloadThenPending,
    AcceptedThenContinuationRejected,
    AcceptedThenContinuationUnknown,
    OriginalPending,
    UnknownTransport,
}

struct RecoveryHarness {
    outcome: FirstOutcome,
    runs: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<RunRequest>>>,
}

#[async_trait]
impl Harness for RecoveryHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }

    fn display_name(&self) -> &str {
        "L10 synthetic"
    }

    fn supports_steering(&self) -> bool {
        false
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }

    fn supports_safe_recovery_continuation(&self) -> bool {
        true
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(Vec::new())
    }

    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.requests.lock().unwrap().push(request.clone());
        let run = self.runs.fetch_add(1, Ordering::SeqCst);
        if run == 0 && matches!(self.outcome, FirstOutcome::RejectedOverload) {
            return Err(HarnessError::RejectedBeforeAcceptance(
                "synthetic provider overloaded".into(),
            ));
        }
        if run == 0 && matches!(self.outcome, FirstOutcome::UnknownTransport) {
            return Ok(futures::stream::iter(vec![Err(HarnessError::Protocol(
                "synthetic transport timeout".into(),
            ))])
            .boxed());
        }
        if run == 1 && matches!(self.outcome, FirstOutcome::AcceptedThenContinuationRejected) {
            return Err(HarnessError::RejectedBeforeAcceptance(
                "synthetic continuation rejected before acceptance: provider overloaded".into(),
            ));
        }
        if run == 1 && matches!(self.outcome, FirstOutcome::AcceptedThenContinuationUnknown) {
            return Ok(futures::stream::iter(vec![Err(HarnessError::Protocol(
                "synthetic continuation transport timeout".into(),
            ))])
            .boxed());
        }

        let session_id = request
            .resume
            .clone()
            .unwrap_or_else(|| "l10-provider-session".into());
        if run == 0 && matches!(self.outcome, FirstOutcome::OriginalPending) {
            return Ok(futures::stream::iter(vec![
                Ok(AgentEvent::SessionStarted {
                    harness: HarnessId::Mock,
                    model: request.model.unwrap_or_else(|| "mock-1".into()),
                    tools: Vec::new(),
                    cwd: request.cwd,
                    session_id,
                    assistant_message_id: "assistant-stopped".into(),
                }),
                Ok(AgentEvent::TextDelta {
                    text: "partial output before explicit Stop".into(),
                }),
            ])
            .chain(futures::stream::pending())
            .boxed());
        }
        if run == 1 && matches!(self.outcome, FirstOutcome::AcceptedOverloadThenPending) {
            return Ok(futures::stream::once(async move {
                Ok(AgentEvent::SessionStarted {
                    harness: HarnessId::Mock,
                    model: request.model.unwrap_or_else(|| "mock-1".into()),
                    tools: Vec::new(),
                    cwd: request.cwd,
                    session_id,
                    assistant_message_id: "assistant-pending".into(),
                })
            })
            .chain(futures::stream::pending())
            .boxed());
        }
        let mut events = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: request.model.clone().unwrap_or_else(|| "mock-1".into()),
                tools: Vec::new(),
                cwd: request.cwd.clone(),
                session_id: session_id.clone(),
                assistant_message_id: format!("assistant-{run}"),
            }),
            Ok(AgentEvent::TextDelta {
                text: if run == 0 {
                    "partial output before the overload".into()
                } else {
                    "continued only the unfinished work".into()
                },
            }),
        ];
        if run == 0
            && matches!(
                self.outcome,
                FirstOutcome::AcceptedOverload
                    | FirstOutcome::AcceptedOverloadThenPending
                    | FirstOutcome::AcceptedThenContinuationRejected
                    | FirstOutcome::AcceptedThenContinuationUnknown
            )
        {
            events.extend([
                Ok(AgentEvent::ToolCall {
                    id: "completed-effect".into(),
                    call: luna_proto::ToolCall::Exec {
                        command: "synthetic-effect".into(),
                    },
                }),
                Ok(AgentEvent::ToolResult {
                    id: "completed-effect".into(),
                    is_error: false,
                    output: Some("effect completed once".into()),
                    diff: None,
                }),
                Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some("synthetic provider overloaded".into()),
                    session_id: Some(session_id),
                }),
            ]);
        } else {
            events.push(Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(session_id),
            }));
        }
        Ok(futures::stream::iter(events).boxed())
    }
}

fn account_config(root: &std::path::Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    }
}

fn store_root(root: &std::path::Path) -> std::path::PathBuf {
    root.join("data/orgs/l10-org/l10-user")
}

fn assemble(
    root: &std::path::Path,
    outcome: FirstOutcome,
    runs: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<RunRequest>>>,
) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(RecoveryHarness {
        outcome,
        runs,
        requests,
    }));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config(
        EngineProfile::development(&root.join("data"), "l10-org", "l10-user"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
        account_config(root),
    )
    .unwrap();
    if core.workspace.chat(TASK).unwrap().is_none() {
        core.workspace
            .create_chat(
                TASK,
                None,
                Some(&core.device_id),
                None,
                Some("/tmp".to_string()),
            )
            .unwrap();
    }
    core
}

fn request(attachment: &str) -> RunRequest {
    let mut model_options = serde_json::Map::new();
    model_options.insert("fixtureChoice".into(), "frozen".into());
    RunRequest {
        input_items: Vec::new(),
        prompt: "perform the original synthetic request".into(),
        harness: Some(HarnessId::Mock),
        provider_account_id: None,
        model: Some("mock-1".into()),
        reasoning: Some(ReasoningLevel::Medium),
        model_options,
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: Some(PermissionMode::Ask),
        environment_permission_ceiling: Some(PermissionMode::Ask),
        permission_policy: None,
        attachments: vec![attachment.into()],
        resume: None,
        worktree: None,
    }
}

async fn snapshot(client: &RpcClient) -> TaskCommandRead {
    client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": TASK }),
        )
        .await
        .unwrap()
}

async fn command(client: &RpcClient, id: &str, intent: serde_json::Value) -> CommandReceipt {
    let snapshot = snapshot(client).await;
    let requires_revision = matches!(
        intent.get("kind").and_then(serde_json::Value::as_str),
        Some(
            "rename"
                | "setArchived"
                | "delete"
                | "setBranch"
                | "setCwd"
                | "setHost"
                | "setConfig"
                | "setPermission"
                | "setProviderAccount"
        )
    );
    let requires_turn = intent.get("kind").and_then(serde_json::Value::as_str) == Some("session")
        && intent
            .get("command")
            .and_then(|command| command.get("kind"))
            .and_then(serde_json::Value::as_str)
            == Some("interrupt");
    let mut value = serde_json::json!({
        "commandId":id,
        "actorId":"l10-test",
        "clientId":"l10-test",
        "clientSessionId":"isolated",
        "taskId":TASK,
        "intent":intent,
    });
    if let Some(generation) = snapshot.snapshot.binding_generation {
        value["expectedBindingGeneration"] = generation.into();
    }
    if requires_revision {
        value["expectedTaskRevision"] = snapshot.snapshot.task_revision.into();
    }
    if requires_turn && let Some(turn_id) = snapshot.snapshot.active_turn_id {
        value["expectedTurnId"] = turn_id.into();
    }
    client
        .call_as(methods::EXECUTE_TASK_COMMAND, value)
        .await
        .unwrap()
}

async fn send_original(client: &RpcClient, request: RunRequest) -> CommandReceipt {
    command(
        client,
        "l10-original-command",
        serde_json::json!({
            "kind":"session",
            "command": { "kind":"run", "request":request, "messageId":MESSAGE },
            "transfers":[]
        }),
    )
    .await
}

async fn wait_for_recovery(core: &EngineCore, status: RecoveryStatus) -> luna_proto::TaskRecovery {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(recovery) = core
            .sessions
            .session_status(TASK)
            .and_then(|session| session.recovery)
            .filter(|recovery| recovery.status == status)
        {
            return recovery;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "recovery status timeout"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_active_turn(core: &EngineCore) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(turn_id) = core.sessions.active_turn_id(TASK) {
            return turn_id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "active turn timeout"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_receipt(
    core: &EngineCore,
    command_id: &str,
    status: CommandReceiptStatus,
) -> CommandReceipt {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(receipt) = core.doc_host.task_command_receipt(command_id).unwrap()
            && receipt.status == status
        {
            return receipt;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task receipt timeout"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn claim_recovery_without_dispatch(root: &std::path::Path) {
    let store = DocsStore::open(store_root(root)).unwrap();
    let current = store.task_recovery(TASK).unwrap().unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&current.record_json).unwrap();
    value["view"]["status"] = "running".into();
    value["view"]["automatic"] = false.into();
    value["view"]["retryAtMs"] = serde_json::Value::Null;
    value["view"]["attemptCount"] = 1.into();
    value["view"]["updatedAtMs"] = 1_789_860_000_000_i64.into();
    value["attemptAcceptance"] = serde_json::Value::Null;
    value["attemptStartedAtMs"] = 1_789_860_000_000_i64.into();
    let next = luna_sync::StoredTaskRecovery {
        task_id: current.task_id.clone(),
        chain_id: current.chain_id.clone(),
        attempt_count: 1,
        status: "running".into(),
        record_json: serde_json::to_vec(&value).unwrap(),
        updated_at: 1_789_860_000_000,
    };
    assert!(
        store
            .transition_task_recovery(&TaskRecoveryTransition {
                expected: Some(TaskRecoveryExpectation {
                    chain_id: current.chain_id,
                    attempt_count: current.attempt_count,
                    status: current.status,
                }),
                next,
            })
            .unwrap()
    );
}

fn set_recovery_retry_at(root: &std::path::Path, retry_at_ms: i64) {
    let store = DocsStore::open(store_root(root)).unwrap();
    let current = store.task_recovery(TASK).unwrap().unwrap();
    assert_eq!(current.status, "scheduled");
    let mut value: serde_json::Value = serde_json::from_slice(&current.record_json).unwrap();
    value["view"]["retryAtMs"] = retry_at_ms.into();
    value["view"]["updatedAtMs"] = retry_at_ms.into();
    let next = luna_sync::StoredTaskRecovery {
        task_id: current.task_id.clone(),
        chain_id: current.chain_id.clone(),
        attempt_count: current.attempt_count,
        status: current.status.clone(),
        record_json: serde_json::to_vec(&value).unwrap(),
        updated_at: retry_at_ms,
    };
    assert!(
        store
            .transition_task_recovery(&TaskRecoveryTransition {
                expected: Some(TaskRecoveryExpectation {
                    chain_id: current.chain_id,
                    attempt_count: current.attempt_count,
                    status: current.status,
                }),
                next,
            })
            .unwrap()
    );
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn changed_config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some("mock-2".into()),
        reasoning: Some(ReasoningLevel::Medium),
        model_options: serde_json::Map::new(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn mutate_recovery_record(
    root: &std::path::Path,
    mutate: impl FnOnce(&mut serde_json::Value, &mut luna_sync::StoredTaskRecovery),
) {
    let store = DocsStore::open(store_root(root)).unwrap();
    let current = store.task_recovery(TASK).unwrap().unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&current.record_json).unwrap();
    let mut next = current.clone();
    mutate(&mut value, &mut next);
    next.record_json = serde_json::to_vec(&value).unwrap();
    assert!(
        store
            .transition_task_recovery(&TaskRecoveryTransition {
                expected: Some(TaskRecoveryExpectation {
                    chain_id: current.chain_id,
                    attempt_count: current.attempt_count,
                    status: current.status,
                }),
                next,
            })
            .unwrap()
    );
}

fn persist_synthetic_run(
    core: &EngineCore,
    command_id: &str,
    message_id: &str,
    request: RunRequest,
) {
    let handle = core.doc_host.open(TASK).unwrap();
    handle
        .write_user_message(message_id, &request.prompt, now_ms())
        .unwrap();
    handle
        .doc()
        .queue_command(&SessionCommandEntry {
            id: command_id.into(),
            payload: SessionCommandPayload::Run {
                request,
                message_id: message_id.into(),
            },
            issued_by: core.device_id.clone(),
            issued_at: now_ms(),
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Applied,
            resolution: None,
        })
        .unwrap();
    core.doc_host.flush_all();
}

fn persist_pending_receipt(core: &EngineCore, command_id: &str, message_id: &str) {
    let now = now_ms();
    let snapshot = core.doc_host.task_command_snapshot(TASK).unwrap().snapshot;
    core.doc_host
        .begin_task_command(
            CommandReceipt {
                command_id: command_id.into(),
                task_id: TASK.into(),
                intent: "run".into(),
                payload_hash: format!("synthetic-{command_id}"),
                status: CommandReceiptStatus::Pending,
                snapshot,
                event_sequence: None,
                actor_id: "l10-test".into(),
                client_id: "l10-test".into(),
                client_session_id: "isolated".into(),
                execution_environment: Some(core.device_id.clone()),
                correlation_id: None,
                reason: None,
                recovery_action: None,
                accepted_at_ms: now,
                committed_at_ms: None,
                observed_at_ms: now,
            },
            None,
            None,
            None,
        )
        .unwrap();
    core.doc_host
        .set_task_command_correlation(command_id, message_id)
        .unwrap();
}

fn persist_accepted_receipt(core: &EngineCore, command_id: &str, message_id: &str) {
    persist_pending_receipt(core, command_id, message_id);
    assert_eq!(
        core.doc_host
            .confirm_task_command_correlation(TASK, message_id, "terminal-window-turn", None, None,)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn confirmed_rejection_resends_the_frozen_request_once() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let recovery = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    assert_eq!(
        recovery.acceptance,
        RecoveryAcceptance::RejectedBeforeAcceptance
    );
    core.sessions
        .hold_recovery(
            TASK,
            "Recovery held by Stop. Queued work and partial output were preserved.",
        )
        .unwrap();
    let held = wait_for_recovery(&core, RecoveryStatus::Held).await;
    assert_eq!(held.chain_id, recovery.chain_id);
    assert_eq!(
        command(
            &client,
            "l10-manual-retry",
            serde_json::json!({"kind":"retryRecovery","chainId":recovery.chain_id})
        )
        .await
        .status,
        CommandReceiptStatus::Applied
    );
    let completed = wait_for_recovery(&core, RecoveryStatus::Completed).await;
    assert_eq!(completed.attempt_count, 1);
    {
        let logged = requests.lock().unwrap();
        assert_eq!(logged.len(), 2);
        assert_eq!(logged[0].prompt, logged[1].prompt);
        assert_eq!(logged[0].attachments, logged[1].attachments);
        assert_eq!(logged[0].model_options, logged[1].model_options);
        assert_eq!(logged[0].sandbox, logged[1].sandbox);
    }
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    core.shutdown().await;
}

#[tokio::test]
async fn accepted_failure_continues_same_session_without_repeating_effects() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::AcceptedOverload,
        runs,
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let recovery = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    assert_eq!(recovery.acceptance, RecoveryAcceptance::Accepted);
    command(
        &client,
        "l10-manual-continuation",
        serde_json::json!({"kind":"retryRecovery","chainId":recovery.chain_id}),
    )
    .await;
    wait_for_recovery(&core, RecoveryStatus::Completed).await;
    {
        let logged = requests.lock().unwrap();
        assert_eq!(logged.len(), 2);
        assert_eq!(logged[1].resume.as_deref(), Some("l10-provider-session"));
        assert!(
            logged[1]
                .prompt
                .starts_with("Continue from the preserved provider session")
        );
        assert!(logged[1].attachments.is_empty());
        assert_eq!(logged[0].model, logged[1].model);
        assert_eq!(logged[0].model_options, logged[1].model_options);
    }
    let entries = core
        .doc_host
        .open(TASK)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.role == MessageRole::User)
            .count(),
        1
    );
    assert_eq!(
        entries
            .iter()
            .flat_map(|entry| &entry.parts)
            .filter(|part| matches!(part, MessagePart::Tool { id, .. } if id == "completed-effect"))
            .count(),
        1
    );
    assert!(entries.iter().flat_map(|entry| &entry.parts).any(
        |part| matches!(part, MessagePart::Text { text, .. } if text.contains("partial output"))
    ));
    core.shutdown().await;
}

#[tokio::test]
async fn explicit_stop_survives_restart_without_recovery_or_queue_release() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("stopped.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::OriginalPending,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    wait_for_active_turn(&core).await;
    wait_for_receipt(&core, "l10-original-command", CommandReceiptStatus::Applied).await;

    let queued = command(
        &client,
        "l10-queue-before-stop",
        serde_json::json!({
            "kind":"queueMessage",
            "messageId":"l10-held-after-stop",
            "text":"preserve this queued follow-up",
            "attachments":[]
        }),
    )
    .await;
    assert_eq!(queued.status, CommandReceiptStatus::Applied);
    command(
        &client,
        "l10-stop-original",
        serde_json::json!({
            "kind":"session",
            "command":{"kind":"interrupt"},
            "transfers":[]
        }),
    )
    .await;
    wait_for_receipt(&core, "l10-stop-original", CommandReceiptStatus::Applied).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while core.sessions.active_turn_id(TASK).is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "Stop did not settle"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        core.sessions
            .session_status(TASK)
            .and_then(|session| session.recovery)
            .is_none()
    );
    let entries = core
        .doc_host
        .open(TASK)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert!(entries.iter().flat_map(|entry| &entry.parts).any(
        |part| matches!(part, MessagePart::Text { text, .. } if text.contains("partial output before explicit Stop"))
    ));
    assert!(matches!(
        core.doc_host
            .open(TASK)
            .unwrap()
            .doc()
            .read_queue()
            .unwrap()[0]
            .status,
        luna_doc::QueueMessageStatus::Held
    ));
    core.shutdown().await;
    drop(client);
    drop(core);

    let reopened = assemble(
        root.path(),
        FirstOutcome::OriginalPending,
        runs.clone(),
        requests,
    );
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "Stop must never become auto-retry"
    );
    assert!(
        reopened
            .sessions
            .session_status(TASK)
            .and_then(|session| session.recovery)
            .is_none()
    );
    let handle = reopened.doc_host.open(TASK).unwrap();
    let queue = handle.doc().read_queue().unwrap();
    assert_eq!(queue.len(), 1);
    assert!(matches!(
        queue[0].status,
        luna_doc::QueueMessageStatus::Held
    ));
    let entries = handle.doc().read_entries().unwrap();
    assert!(entries.iter().flat_map(|entry| &entry.parts).any(
        |part| matches!(part, MessagePart::Text { text, .. } if text.contains("partial output before explicit Stop"))
    ));
    reopened.shutdown().await;
}

#[tokio::test]
async fn explicit_new_original_after_stop_is_not_suppressed() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("stopped-then-new.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::OriginalPending,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    wait_for_active_turn(&core).await;
    command(
        &client,
        "l10-stop-before-new-original",
        serde_json::json!({
            "kind":"session",
            "command":{"kind":"interrupt"},
            "transfers":[]
        }),
    )
    .await;
    wait_for_receipt(
        &core,
        "l10-stop-before-new-original",
        CommandReceiptStatus::Applied,
    )
    .await;

    let mut newer = request(attachment.to_str().unwrap());
    newer.prompt = "explicitly authorized after Stop".into();
    command(
        &client,
        "l10-new-original-after-stop",
        serde_json::json!({
            "kind":"session",
            "command":{
                "kind":"run",
                "request":newer,
                "messageId":"l10-new-message-after-stop"
            },
            "transfers":[]
        }),
    )
    .await;
    wait_for_receipt(
        &core,
        "l10-new-original-after-stop",
        CommandReceiptStatus::Applied,
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while runs.load(Ordering::SeqCst) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "new original did not dispatch"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        requests.lock().unwrap()[1].prompt,
        "explicitly authorized after Stop"
    );
    core.shutdown().await;
    drop(client);
    drop(core);

    let reopened = assemble(
        root.path(),
        FirstOutcome::OriginalPending,
        runs.clone(),
        requests,
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    reopened.shutdown().await;
}

#[tokio::test]
async fn accepted_original_is_never_downgraded_when_continuation_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::AcceptedThenContinuationRejected,
        runs,
        requests,
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let first = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    command(
        &client,
        "l10-rejected-continuation",
        serde_json::json!({"kind":"retryRecovery","chainId":first.chain_id}),
    )
    .await;
    let retry = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    assert_eq!(retry.attempt_count, 1);
    assert_eq!(retry.original_acceptance, RecoveryAcceptance::Accepted);
    assert_eq!(retry.acceptance, RecoveryAcceptance::Accepted);
    assert_eq!(retry.action, RecoveryAction::ContinueSession);

    let stored = DocsStore::open(store_root(root.path()))
        .unwrap()
        .task_recovery(TASK)
        .unwrap()
        .unwrap();
    let private: serde_json::Value = serde_json::from_slice(&stored.record_json).unwrap();
    assert_eq!(
        private["request"]["prompt"], "perform the original synthetic request",
        "a failed continuation must not replace the frozen original request"
    );
    assert_eq!(
        private["request"]["attachments"].as_array().unwrap().len(),
        1
    );
    core.shutdown().await;
}

#[tokio::test]
async fn ambiguous_continuation_start_checks_delivery_and_cannot_resend_original() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::AcceptedThenContinuationUnknown,
        runs.clone(),
        requests,
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let first = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    command(
        &client,
        "l10-unknown-continuation",
        serde_json::json!({"kind":"retryRecovery","chainId":first.chain_id}),
    )
    .await;
    let recovery = wait_for_recovery(&core, RecoveryStatus::CheckingDelivery).await;
    assert_eq!(recovery.original_acceptance, RecoveryAcceptance::Accepted);
    assert_eq!(recovery.acceptance, RecoveryAcceptance::Unknown);
    assert_eq!(recovery.action, RecoveryAction::ObserveOnly);
    let unsafe_retry = command(
        &client,
        "l10-replay-after-ambiguous-continuation",
        serde_json::json!({"kind":"retryRecovery","chainId":recovery.chain_id}),
    )
    .await;
    assert_eq!(unsafe_retry.status, CommandReceiptStatus::Rejected);
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    core.shutdown().await;
}

#[tokio::test]
async fn unknown_delivery_is_visible_and_never_replayed() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::UnknownTransport,
        runs.clone(),
        requests,
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let recovery = wait_for_recovery(&core, RecoveryStatus::CheckingDelivery).await;
    assert_eq!(recovery.acceptance, RecoveryAcceptance::Unknown);
    let receipt = command(
        &client,
        "l10-unsafe-retry",
        serde_json::json!({"kind":"retryRecovery","chainId":recovery.chain_id}),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Rejected);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let cancelled = command(
        &client,
        "l10-cancel-unknown",
        serde_json::json!({"kind":"cancelRecovery","chainId":recovery.chain_id}),
    )
    .await;
    assert_eq!(cancelled.status, CommandReceiptStatus::Applied);
    wait_for_recovery(&core, RecoveryStatus::Cancelled).await;
    core.shutdown().await;
}

#[tokio::test]
async fn cancellation_and_chain_identity_survive_restart_without_replay() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::UnknownTransport,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let recovery = wait_for_recovery(&core, RecoveryStatus::CheckingDelivery).await;
    command(
        &client,
        "l10-cancel-before-restart",
        serde_json::json!({"kind":"cancelRecovery","chainId":recovery.chain_id}),
    )
    .await;
    let cancelled = wait_for_recovery(&core, RecoveryStatus::Cancelled).await;
    let chain_id = cancelled.chain_id.clone();
    assert_eq!(cancelled.attempt_count, 0);
    core.shutdown().await;
    drop(client);
    drop(core);

    let reopened = assemble(
        root.path(),
        FirstOutcome::UnknownTransport,
        runs.clone(),
        requests,
    );
    let restored = wait_for_recovery(&reopened, RecoveryStatus::Cancelled).await;
    assert_eq!(restored.chain_id, chain_id);
    assert_eq!(restored.attempt_count, 0);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "restart must not replay a cancelled chain"
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn terminal_old_chain_does_not_mask_a_newer_interrupted_original_on_restart() {
    for accepted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let old_attachment = root.path().join("old.png");
        let new_attachment = root.path().join("new.png");
        std::fs::write(&old_attachment, b"old").unwrap();
        std::fs::write(&new_attachment, b"new").unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let core = assemble(
            root.path(),
            FirstOutcome::RejectedOverload,
            runs.clone(),
            requests.clone(),
        );
        let client = luna_rpc::memory_client(core.rpc_service());
        send_original(&client, request(old_attachment.to_str().unwrap())).await;
        let old = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
        command(
            &client,
            "l10-cancel-old-original",
            serde_json::json!({"kind":"cancelRecovery","chainId":old.chain_id.clone()}),
        )
        .await;
        let old = wait_for_recovery(&core, RecoveryStatus::Cancelled).await;

        let mut newer = request(new_attachment.to_str().unwrap());
        newer.prompt = if accepted {
            "newer accepted interrupted original".into()
        } else {
            "newer unknown interrupted original".into()
        };
        newer
            .model_options
            .insert("fixtureChoice".into(), "newer".into());
        let message_id = if accepted {
            "l10-newer-accepted-message"
        } else {
            "l10-newer-unknown-message"
        };
        let command_id = if accepted {
            "l10-newer-accepted-command"
        } else {
            "l10-newer-unknown-command"
        };
        persist_synthetic_run(&core, command_id, message_id, newer.clone());
        if accepted {
            persist_accepted_receipt(&core, command_id, message_id);
        } else {
            persist_pending_receipt(&core, command_id, message_id);
        }
        let journal = RunJournal::open(store_root(root.path()).join("journals")).unwrap();
        if accepted {
            journal
                .append(
                    TASK,
                    &AgentEvent::SessionStarted {
                        harness: HarnessId::Mock,
                        model: "mock-1".into(),
                        tools: Vec::new(),
                        cwd: "/tmp".into(),
                        session_id: "newer-provider-session".into(),
                        assistant_message_id: "newer-assistant".into(),
                    },
                )
                .unwrap();
        } else {
            journal
                .append(
                    TASK,
                    &AgentEvent::TextDelta {
                        text: "delivery boundary unknown".into(),
                    },
                )
                .unwrap();
        }
        std::fs::write(
            root.path().join("data/ui-settings.json"),
            br#"{"automaticRetriesEnabled":false}"#,
        )
        .unwrap();
        core.shutdown().await;
        drop(client);
        drop(core);

        let reopened = assemble(
            root.path(),
            FirstOutcome::RejectedOverload,
            runs.clone(),
            requests,
        );
        let recovered = wait_for_recovery(
            &reopened,
            if accepted {
                RecoveryStatus::RecoveryNeeded
            } else {
                RecoveryStatus::CheckingDelivery
            },
        )
        .await;
        assert_ne!(recovered.chain_id, old.chain_id);
        assert_eq!(recovered.original_message_id, message_id);
        assert_eq!(recovered.original_command_id.as_deref(), Some(command_id));
        assert_eq!(
            recovered.original_acceptance,
            if accepted {
                RecoveryAcceptance::Accepted
            } else {
                RecoveryAcceptance::Unknown
            }
        );
        assert_eq!(recovered.attempt_count, 0);
        let stored = DocsStore::open(store_root(root.path()))
            .unwrap()
            .task_recovery(TASK)
            .unwrap()
            .unwrap();
        let private: serde_json::Value = serde_json::from_slice(&stored.record_json).unwrap();
        assert_eq!(private["request"]["prompt"], newer.prompt);
        assert_eq!(
            private["request"]["attachments"][0],
            new_attachment.to_str().unwrap()
        );
        assert_eq!(private["request"]["modelOptions"]["fixtureChoice"], "newer");
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "restart must not replay either original"
        );
        reopened.shutdown().await;
    }
}

#[tokio::test]
async fn terminal_journal_failure_without_a_recovery_row_is_visible_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("terminal-window.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::AcceptedOverload,
        runs.clone(),
        requests.clone(),
    );
    let mut frozen = request(attachment.to_str().unwrap());
    frozen.prompt = "terminal journal before recovery row".into();
    frozen
        .model_options
        .insert("terminalWindow".into(), true.into());
    persist_synthetic_run(
        &core,
        "l10-terminal-window-command",
        "l10-terminal-window-message",
        frozen.clone(),
    );
    persist_accepted_receipt(
        &core,
        "l10-terminal-window-command",
        "l10-terminal-window-message",
    );
    let journal = RunJournal::open(store_root(root.path()).join("journals")).unwrap();
    journal
        .append(
            TASK,
            &AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: Vec::new(),
                cwd: "/tmp".into(),
                session_id: "terminal-window-session".into(),
                assistant_message_id: "terminal-window-assistant".into(),
            },
        )
        .unwrap();
    journal
        .append(
            TASK,
            &AgentEvent::Done {
                status: DoneStatus::Errored,
                result: None,
                error: Some("synthetic provider overloaded".into()),
                session_id: Some("terminal-window-session".into()),
            },
        )
        .unwrap();
    std::fs::write(
        root.path().join("data/ui-settings.json"),
        br#"{"automaticRetriesEnabled":false}"#,
    )
    .unwrap();
    core.shutdown().await;
    drop(core);

    let reopened = assemble(
        root.path(),
        FirstOutcome::AcceptedOverload,
        runs.clone(),
        requests,
    );
    let recovered = wait_for_recovery(&reopened, RecoveryStatus::RecoveryNeeded).await;
    assert_eq!(recovered.original_message_id, "l10-terminal-window-message");
    assert_eq!(recovered.original_acceptance, RecoveryAcceptance::Accepted);
    assert_eq!(recovered.action, RecoveryAction::ContinueSession);
    let stored = DocsStore::open(store_root(root.path()))
        .unwrap()
        .task_recovery(TASK)
        .unwrap()
        .unwrap();
    let private: serde_json::Value = serde_json::from_slice(&stored.record_json).unwrap();
    assert_eq!(private["request"]["prompt"], frozen.prompt);
    assert_eq!(
        private["request"]["attachments"].as_array().unwrap().len(),
        1
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    reopened.shutdown().await;
}

#[tokio::test]
async fn terminal_sweep_does_not_borrow_an_older_segment_for_a_newer_pending_run() {
    let root = tempfile::tempdir().unwrap();
    let old_attachment = root.path().join("older-segment.png");
    let new_attachment = root.path().join("newer-pending.png");
    std::fs::write(&old_attachment, b"old").unwrap();
    std::fs::write(&new_attachment, b"new").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::AcceptedOverload,
        runs.clone(),
        requests.clone(),
    );
    let old = request(old_attachment.to_str().unwrap());
    persist_synthetic_run(
        &core,
        "l10-older-terminal-command",
        "l10-older-terminal-message",
        old,
    );
    persist_accepted_receipt(
        &core,
        "l10-older-terminal-command",
        "l10-older-terminal-message",
    );
    let journal = RunJournal::open(store_root(root.path()).join("journals")).unwrap();
    journal
        .append(
            TASK,
            &AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: Vec::new(),
                cwd: "/tmp".into(),
                session_id: "older-provider-session".into(),
                assistant_message_id: "older-assistant".into(),
            },
        )
        .unwrap();
    journal
        .append(
            TASK,
            &AgentEvent::Done {
                status: DoneStatus::Errored,
                result: None,
                error: Some("synthetic provider overloaded".into()),
                session_id: Some("older-provider-session".into()),
            },
        )
        .unwrap();

    let mut newer = request(new_attachment.to_str().unwrap());
    newer.prompt = "newer pending before first journal event".into();
    newer
        .model_options
        .insert("segmentOwner".into(), "newer".into());
    persist_synthetic_run(
        &core,
        "l10-newer-pending-command",
        "l10-newer-pending-message",
        newer.clone(),
    );
    persist_pending_receipt(
        &core,
        "l10-newer-pending-command",
        "l10-newer-pending-message",
    );
    core.shutdown().await;
    drop(core);

    let reopened = assemble(
        root.path(),
        FirstOutcome::AcceptedOverload,
        runs.clone(),
        requests,
    );
    let recovered = wait_for_recovery(&reopened, RecoveryStatus::CheckingDelivery).await;
    assert_eq!(recovered.original_message_id, "l10-newer-pending-message");
    assert_eq!(
        recovered.original_command_id.as_deref(),
        Some("l10-newer-pending-command")
    );
    assert_eq!(recovered.original_acceptance, RecoveryAcceptance::Unknown);
    assert_eq!(recovered.action, RecoveryAction::ObserveOnly);
    let stored = DocsStore::open(store_root(root.path()))
        .unwrap()
        .task_recovery(TASK)
        .unwrap()
        .unwrap();
    let private: serde_json::Value = serde_json::from_slice(&stored.record_json).unwrap();
    assert_eq!(private["request"]["prompt"], newer.prompt);
    assert_eq!(private["request"]["modelOptions"]["segmentOwner"], "newer");
    assert!(private.get("providerSessionId").is_none());
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    reopened.shutdown().await;
}

#[tokio::test]
async fn restart_after_earliest_attempt_claim_preserves_chain_budget_and_does_not_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::AcceptedOverload,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let original = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    let chain_id = original.chain_id.clone();
    let deadline = original.automatic_deadline_ms;
    core.shutdown().await;
    drop(client);
    drop(core);

    // This is the exact durable state after the receipt/attempt claim commits
    // but before the provider dispatch emits its first journal event.
    claim_recovery_without_dispatch(root.path());
    let reopened = assemble(
        root.path(),
        FirstOutcome::AcceptedOverload,
        runs.clone(),
        requests.clone(),
    );
    let reconciled = wait_for_recovery(&reopened, RecoveryStatus::CheckingDelivery).await;
    assert_eq!(reconciled.chain_id, chain_id);
    assert_eq!(reconciled.attempt_count, 1);
    assert_eq!(reconciled.automatic_deadline_ms, deadline);
    assert_eq!(reconciled.original_acceptance, RecoveryAcceptance::Accepted);
    assert_eq!(reconciled.acceptance, RecoveryAcceptance::Unknown);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "restart must not dispatch the claim"
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
    let entries = reopened
        .doc_host
        .open(TASK)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.role == MessageRole::User)
            .count(),
        1,
        "restart reconciliation must not duplicate the original input"
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn restart_during_accepted_attempt_continues_same_chain_without_replaying_input() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::AcceptedOverload,
        runs.clone(),
        requests,
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let original = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    let chain_id = original.chain_id.clone();
    let deadline = original.automatic_deadline_ms;
    core.shutdown().await;
    drop(client);
    drop(core);
    claim_recovery_without_dispatch(root.path());
    let journal = RunJournal::open(store_root(root.path()).join("journals")).unwrap();
    journal
        .append(
            TASK,
            &AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: Vec::new(),
                cwd: "/tmp".into(),
                session_id: "accepted-recovery-session".into(),
                assistant_message_id: "accepted-recovery-assistant".into(),
            },
        )
        .unwrap();
    std::fs::write(
        root.path().join("data/ui-settings.json"),
        br#"{"automaticRetriesEnabled":false}"#,
    )
    .unwrap();

    let reopened = assemble(
        root.path(),
        FirstOutcome::AcceptedOverload,
        runs.clone(),
        Arc::new(Mutex::new(Vec::new())),
    );
    let reconciled = wait_for_recovery(&reopened, RecoveryStatus::RecoveryNeeded).await;
    assert_eq!(reconciled.chain_id, chain_id);
    assert_eq!(reconciled.attempt_count, 1);
    assert_eq!(reconciled.automatic_deadline_ms, deadline);
    assert_eq!(reconciled.original_acceptance, RecoveryAcceptance::Accepted);
    assert_eq!(reconciled.acceptance, RecoveryAcceptance::Accepted);
    assert_eq!(reconciled.action, RecoveryAction::ContinueSession);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    reopened.shutdown().await;
}

#[tokio::test]
async fn newer_user_activity_supersedes_the_scheduled_chain() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let recovery = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    let mut newer = request(attachment.to_str().unwrap());
    newer.prompt = "newer explicit user request".into();
    let receipt = command(
        &client,
        "l10-newer-command",
        serde_json::json!({
            "kind":"session",
            "command":{"kind":"run","request":newer,"messageId":"l10-newer-message"},
            "transfers":[]
        }),
    )
    .await;
    assert!(matches!(
        receipt.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));
    let superseded = wait_for_recovery(&core, RecoveryStatus::Superseded).await;
    assert_eq!(superseded.chain_id, recovery.chain_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while runs.load(Ordering::SeqCst) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "newer run did not complete"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "the obsolete retry timer must be cancelled"
    );
    assert_eq!(
        requests.lock().unwrap()[1].prompt,
        "newer explicit user request"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn two_clients_cannot_claim_the_same_recovery_attempt() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::AcceptedOverload,
        runs.clone(),
        requests,
    );
    let first = luna_rpc::memory_client(core.rpc_service());
    let second = luna_rpc::memory_client(core.rpc_service());
    send_original(&first, request(attachment.to_str().unwrap())).await;
    let recovery = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    let (a, b) = tokio::join!(
        command(
            &first,
            "l10-racing-retry-a",
            serde_json::json!({"kind":"retryRecovery","chainId":recovery.chain_id})
        ),
        command(
            &second,
            "l10-racing-retry-b",
            serde_json::json!({"kind":"retryRecovery","chainId":recovery.chain_id})
        )
    );
    let statuses = [a.status, b.status];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == CommandReceiptStatus::Applied)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == CommandReceiptStatus::Rejected)
            .count(),
        1
    );
    wait_for_recovery(&core, RecoveryStatus::Completed).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "only one recovery provider call"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn duplicate_retry_after_lost_response_returns_the_receipt_without_another_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::AcceptedOverload,
        runs.clone(),
        requests,
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let recovery = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    let chain_id = recovery.chain_id.clone();
    let first = command(
        &client,
        "l10-lost-retry-response",
        serde_json::json!({"kind":"retryRecovery","chainId":chain_id}),
    )
    .await;
    assert_eq!(first.status, CommandReceiptStatus::Applied);
    wait_for_recovery(&core, RecoveryStatus::Completed).await;

    // Simulate a client that lost the first response and retries the exact
    // command id and payload after observing no reply.
    let duplicate = command(
        &client,
        "l10-lost-retry-response",
        serde_json::json!({"kind":"retryRecovery","chainId":recovery.chain_id}),
    )
    .await;
    assert_eq!(duplicate.status, CommandReceiptStatus::Applied);
    assert_eq!(duplicate.command_id, first.command_id);
    assert_eq!(duplicate.committed_at_ms, first.committed_at_ms);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "duplicate receipt must not dispatch"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn timer_claim_fences_a_late_cancellation_without_a_second_retry() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::AcceptedOverloadThenPending,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let scheduled = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    let chain_id = scheduled.chain_id.clone();
    core.shutdown().await;
    drop(client);
    drop(core);

    set_recovery_retry_at(root.path(), now_ms());
    let reopened = assemble(
        root.path(),
        FirstOutcome::AcceptedOverloadThenPending,
        runs.clone(),
        requests,
    );
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let running = wait_for_recovery(&reopened, RecoveryStatus::Running).await;
    assert_eq!(running.chain_id, chain_id);
    assert_eq!(running.attempt_count, 1);
    let cancelled = command(
        &client,
        "l10-cancel-after-timer-claim",
        serde_json::json!({"kind":"cancelRecovery","chainId":chain_id}),
    )
    .await;
    assert_eq!(cancelled.status, CommandReceiptStatus::Rejected);
    let final_state = wait_for_recovery(&reopened, RecoveryStatus::Running).await;
    assert_eq!(final_state.attempt_count, 1);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "one claimed retry may run, never two"
    );
    reopened.shutdown().await;
    let held = wait_for_recovery(&reopened, RecoveryStatus::Held).await;
    assert_eq!(held.attempt_count, 1);
}

#[tokio::test]
async fn configuration_commit_supersedes_a_pending_timer_before_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let scheduled = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    let chain_id = scheduled.chain_id.clone();
    core.shutdown().await;
    drop(client);
    drop(core);

    set_recovery_retry_at(root.path(), now_ms() + 250);
    let reopened = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests,
    );
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let changed = changed_config();
    let receipt = command(
        &client,
        "l10-config-before-timer",
        serde_json::json!({"kind":"setConfig","config":changed}),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
    let superseded = wait_for_recovery(&reopened, RecoveryStatus::Superseded).await;
    assert_eq!(superseded.chain_id, chain_id);
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "superseded timer must not dispatch"
    );
    assert_eq!(
        reopened
            .workspace
            .chat(TASK)
            .unwrap()
            .unwrap()
            .config
            .unwrap()
            .model
            .as_deref(),
        Some("mock-2")
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn permission_grant_supersedes_a_pending_timer_before_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let scheduled = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    let chain_id = scheduled.chain_id.clone();
    core.shutdown().await;
    drop(client);
    drop(core);

    set_recovery_retry_at(root.path(), now_ms() + 250);
    let reopened = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests,
    );
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let receipt = command(
        &client,
        "l10-permission-before-timer",
        serde_json::json!({"kind":"setPermission","mode":"full","actor":"l10-test"}),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
    let superseded = wait_for_recovery(&reopened, RecoveryStatus::Superseded).await;
    assert_eq!(superseded.chain_id, chain_id);
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "a permission grant committed first must fence the pending timer"
    );
    let config = reopened
        .workspace
        .chat(TASK)
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    assert_eq!(config.permission_mode, PermissionMode::Full);
    assert_eq!(config.effective_permission_mode, PermissionMode::Full);
    reopened.shutdown().await;
}

#[tokio::test]
async fn overdue_in_process_timer_expires_without_dispatching() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("overdue.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests,
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let scheduled = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    assert_eq!(scheduled.attempt_count, 0);
    mutate_recovery_record(root.path(), |value, next| {
        let updated = now_ms();
        value["view"]["automaticDeadlineMs"] = (updated - 1).into();
        value["view"]["updatedAtMs"] = updated.into();
        next.updated_at = updated;
    });

    let wait_ms = scheduled
        .retry_at_ms
        .unwrap()
        .saturating_sub(now_ms())
        .max(0) as u64
        + 500;
    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
    let expired = core
        .sessions
        .session_status(TASK)
        .and_then(|session| session.recovery)
        .unwrap();
    assert_eq!(expired.status, RecoveryStatus::RecoveryNeeded);
    assert_eq!(expired.chain_id, scheduled.chain_id);
    assert_eq!(expired.attempt_count, 0);
    assert!(!expired.automatic);
    assert!(expired.retry_at_ms.is_none());
    assert!(expired.detail.contains("expired before dispatch"));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    core.shutdown().await;
}

#[tokio::test]
async fn stale_timer_snapshot_cannot_claim_a_rescheduled_attempt() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("stale-timer.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests,
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let scheduled = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    let old_retry_at = scheduled.retry_at_ms.unwrap();
    mutate_recovery_record(root.path(), |value, next| {
        let updated = now_ms();
        let replacement_retry = updated + 60_000;
        value["view"]["attemptCount"] = 1.into();
        value["view"]["retryAtMs"] = replacement_retry.into();
        value["view"]["updatedAtMs"] = updated.into();
        next.attempt_count = 1;
        next.updated_at = updated;
    });

    let wait_ms = old_retry_at.saturating_sub(now_ms()).max(0) as u64 + 250;
    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
    let stored = DocsStore::open(store_root(root.path()))
        .unwrap()
        .task_recovery(TASK)
        .unwrap()
        .unwrap();
    assert_eq!(stored.chain_id, scheduled.chain_id);
    assert_eq!(stored.attempt_count, 1);
    assert_eq!(stored.status, "scheduled");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    core.shutdown().await;
}

#[tokio::test]
async fn recovery_transition_storage_failure_rolls_back_receipt_and_reopens_safely() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let recovery = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    let db_path = store_root(root.path()).join("docs.sqlite3");
    let injection = rusqlite::Connection::open(&db_path).unwrap();
    injection
        .execute_batch(
            "CREATE TRIGGER fail_recovery_transition
             BEFORE UPDATE ON task_recovery_state
             BEGIN SELECT RAISE(ABORT, 'forced recovery transition failure'); END;",
        )
        .unwrap();
    let value = serde_json::json!({
        "commandId":"l10-cancel-storage-failure",
        "actorId":"l10-test",
        "clientId":"l10-test",
        "clientSessionId":"isolated",
        "taskId":TASK,
        "intent":{"kind":"cancelRecovery","chainId":recovery.chain_id},
    });
    let failed = client
        .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, value)
        .await;
    assert!(failed.is_err(), "injected SQLite failure must surface");
    assert_eq!(
        wait_for_recovery(&core, RecoveryStatus::Scheduled)
            .await
            .chain_id,
        recovery.chain_id
    );
    assert_eq!(
        core.doc_host
            .task_command_receipt("l10-cancel-storage-failure")
            .unwrap()
            .unwrap()
            .status,
        CommandReceiptStatus::Pending,
        "terminal receipt must roll back with recovery state"
    );
    injection
        .execute_batch("DROP TRIGGER fail_recovery_transition")
        .unwrap();
    let applied = command(
        &client,
        "l10-cancel-storage-failure",
        serde_json::json!({"kind":"cancelRecovery","chainId":recovery.chain_id}),
    )
    .await;
    assert_eq!(applied.status, CommandReceiptStatus::Applied);
    wait_for_recovery(&core, RecoveryStatus::Cancelled).await;
    core.shutdown().await;
    drop(client);
    drop(core);

    let reopened = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests,
    );
    wait_for_recovery(&reopened, RecoveryStatus::Cancelled).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    reopened.shutdown().await;
}

#[tokio::test]
async fn stop_recovery_storage_failure_never_acknowledges_without_a_durable_hold() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("stop-storage-failure.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::AcceptedOverloadThenPending,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let scheduled = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    command(
        &client,
        "l10-running-recovery-before-stop-fault",
        serde_json::json!({"kind":"retryRecovery","chainId":scheduled.chain_id}),
    )
    .await;
    wait_for_active_turn(&core).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if core
            .sessions
            .session_status(TASK)
            .and_then(|session| session.recovery)
            .is_some_and(|recovery| {
                recovery.status == RecoveryStatus::Running
                    && recovery.acceptance == RecoveryAcceptance::Accepted
            })
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "recovery attempt acceptance timeout"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let db_path = store_root(root.path()).join("docs.sqlite3");
    let injection = rusqlite::Connection::open(&db_path).unwrap();
    injection
        .execute_batch(
            "CREATE TRIGGER fail_stop_recovery_hold
             BEFORE UPDATE ON task_recovery_state
             BEGIN SELECT RAISE(ABORT, 'forced Stop recovery hold failure'); END;",
        )
        .unwrap();
    let stop = command(
        &client,
        "l10-stop-storage-failure",
        serde_json::json!({
            "kind":"session",
            "command":{"kind":"interrupt"},
            "transfers":[]
        }),
    )
    .await;
    assert_eq!(stop.status, CommandReceiptStatus::Pending);
    let settle_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while core.sessions.active_turn_id(TASK).is_some() {
        assert!(
            tokio::time::Instant::now() < settle_deadline,
            "faulted Stop did not settle"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        core.doc_host
            .task_command_receipt("l10-stop-storage-failure")
            .unwrap()
            .unwrap()
            .status,
        CommandReceiptStatus::Pending,
        "Stop must not be acknowledged when its recovery hold rolls back"
    );
    assert_eq!(
        DocsStore::open(store_root(root.path()))
            .unwrap()
            .task_recovery(TASK)
            .unwrap()
            .unwrap()
            .status,
        "running"
    );
    injection
        .execute_batch("DROP TRIGGER fail_stop_recovery_hold")
        .unwrap();
    core.shutdown().await;
    drop(client);
    drop(core);

    let reopened = assemble(
        root.path(),
        FirstOutcome::AcceptedOverloadThenPending,
        runs.clone(),
        requests,
    );
    let held = wait_for_recovery(&reopened, RecoveryStatus::Held).await;
    assert_eq!(held.attempt_count, 1);
    assert!(held.detail.contains("held by Stop"));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_ne!(
        reopened
            .doc_host
            .task_command_receipt("l10-stop-storage-failure")
            .unwrap()
            .unwrap()
            .status,
        CommandReceiptStatus::Applied
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn recovery_storage_failure_rolls_back_registry_projection_and_receipt_together() {
    let root = tempfile::tempdir().unwrap();
    let attachment = root.path().join("frozen.png");
    std::fs::write(&attachment, b"synthetic").unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests.clone(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let recovery = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    let before_config = core.workspace.chat(TASK).unwrap().unwrap().config;

    let db_path = store_root(root.path()).join("docs.sqlite3");
    let injection = rusqlite::Connection::open(&db_path).unwrap();
    injection
        .execute_batch(
            "CREATE TRIGGER fail_recovery_projection
             BEFORE UPDATE ON task_recovery_state
             BEGIN SELECT RAISE(ABORT, 'forced recovery projection failure'); END;",
        )
        .unwrap();
    let value = serde_json::json!({
        "commandId":"l10-config-storage-failure",
        "actorId":"l10-test",
        "clientId":"l10-test",
        "clientSessionId":"isolated",
        "taskId":TASK,
        "expectedTaskRevision":snapshot(&client).await.snapshot.task_revision,
        "intent":{"kind":"setConfig","config":changed_config()},
    });
    let failed = client
        .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, value)
        .await;
    assert!(failed.is_err(), "injected SQLite failure must surface");
    assert_eq!(
        core.workspace.chat(TASK).unwrap().unwrap().config,
        before_config,
        "the in-memory registry candidate must roll back"
    );
    assert_eq!(
        wait_for_recovery(&core, RecoveryStatus::Scheduled)
            .await
            .chain_id,
        recovery.chain_id
    );
    assert_eq!(
        core.doc_host
            .task_command_receipt("l10-config-storage-failure")
            .unwrap()
            .unwrap()
            .status,
        CommandReceiptStatus::Pending
    );

    injection
        .execute_batch("DROP TRIGGER fail_recovery_projection")
        .unwrap();
    let applied = command(
        &client,
        "l10-config-storage-failure",
        serde_json::json!({"kind":"setConfig","config":changed_config()}),
    )
    .await;
    assert_eq!(applied.status, CommandReceiptStatus::Applied);
    wait_for_recovery(&core, RecoveryStatus::Superseded).await;
    assert_eq!(
        core.workspace
            .chat(TASK)
            .unwrap()
            .unwrap()
            .config
            .unwrap()
            .model
            .as_deref(),
        Some("mock-2")
    );
    core.shutdown().await;
    drop(client);
    drop(core);

    let reopened = assemble(
        root.path(),
        FirstOutcome::RejectedOverload,
        runs.clone(),
        requests,
    );
    wait_for_recovery(&reopened, RecoveryStatus::Superseded).await;
    assert_eq!(
        reopened
            .workspace
            .chat(TASK)
            .unwrap()
            .unwrap()
            .config
            .unwrap()
            .model
            .as_deref(),
        Some("mock-2")
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    reopened.shutdown().await;
}
