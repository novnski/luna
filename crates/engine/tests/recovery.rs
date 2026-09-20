//! L10 recovery through the production task-command, session, journal, and
//! session-watch boundaries. Provider homes and effects are synthetic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use zeron_doc::{MessagePart, MessageRole};
use zeron_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, CommandReceipt, CommandReceiptStatus, DoneStatus, HarnessId, Model, PermissionMode,
    ReasoningLevel, RecoveryAcceptance, RecoveryStatus, RunRequest, SandboxLevel, SteeringMode,
    TaskCommandRead,
};
use zeron_rpc::{RpcClient, methods};

const TASK: &str = "l10-recovery-task";
const MESSAGE: &str = "l10-original-message";

#[derive(Clone, Copy)]
enum FirstOutcome {
    RejectedOverload,
    AcceptedOverload,
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

        let session_id = request
            .resume
            .clone()
            .unwrap_or_else(|| "l10-provider-session".into());
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
        if run == 0 && matches!(self.outcome, FirstOutcome::AcceptedOverload) {
            events.extend([
                Ok(AgentEvent::ToolCall {
                    id: "completed-effect".into(),
                    call: zeron_proto::ToolCall::Exec {
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
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
    }
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

async fn wait_for_recovery(core: &EngineCore, status: RecoveryStatus) -> zeron_proto::TaskRecovery {
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
    let client = zeron_rpc::memory_client(core.rpc_service());
    send_original(&client, request(attachment.to_str().unwrap())).await;
    let recovery = wait_for_recovery(&core, RecoveryStatus::Scheduled).await;
    assert_eq!(
        recovery.acceptance,
        RecoveryAcceptance::RejectedBeforeAcceptance
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(
            root.path()
                .join("data/orgs/l10-org/l10-user/recovery/l10-recovery-task.json")
        )
        .unwrap()
        .permissions()
        .mode()
            & 0o777,
        0o600,
        "the frozen recovery request ledger must be private to the local user"
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
    let client = zeron_rpc::memory_client(core.rpc_service());
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
    let client = zeron_rpc::memory_client(core.rpc_service());
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
    let client = zeron_rpc::memory_client(core.rpc_service());
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
    let client = zeron_rpc::memory_client(core.rpc_service());
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
