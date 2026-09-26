//! L07 behavior at the real UI-RPC -> engine -> SQLite/doc-command boundary.
//! All provider homes are synthetic and the harness is deterministic.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use luna_doc::{
    MessageRole, QueueClaimMode, QueueDeliveryGate, QueueHoldReason, QueueMessageStatus,
};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{
    ContextCompactRequest, Harness, HarnessError, ProviderForkProgress,
    ProviderForkProgressReporter, RunControls,
};
use luna_proto::{
    AgentEvent, BindingOrigin, COMPOSER_DRAFT_VERSION, ChatConfig, CommandReasonCode,
    CommandReceipt, CommandReceiptStatus, ComposerDraft, ComposerDraftRead, ContextControlState,
    ContextHandoff, ContextHandoffKind, ContextOperationStatus, DoneStatus, DraftContext,
    DraftSubmissionState, FollowUpBehavior, HarnessId, HistoryRecoveryView, Model, PermissionMode,
    ProviderForkCapability, ProviderForkRequest, ProviderForkResult, ReasoningLevel, RunRequest,
    SandboxLevel, SteeringMode, TaskCommandRead,
};
use luna_rpc::{RpcClient, methods};
use tokio::sync::Notify;

struct CountingHarness {
    runs: Arc<AtomicUsize>,
    compactions: Arc<AtomicUsize>,
    compact_error: bool,
    hold: bool,
    steering: SteeringMode,
}

#[derive(Clone, Copy)]
enum CodexContextOutcome {
    Completed,
    Missing,
}

struct CodexContextHarness {
    runs: Arc<AtomicUsize>,
    release_context: Arc<Notify>,
    release_done: Arc<Notify>,
    outcome: CodexContextOutcome,
}

struct HistoryHarness {
    provider: HarnessId,
    forks: Arc<Mutex<Vec<ProviderForkRequest>>>,
    fork_sequence: Arc<AtomicUsize>,
    behavior: HistoryForkBehavior,
}

#[derive(Clone, Copy)]
enum HistoryForkBehavior {
    Success,
    FailPreflight,
    FailAfterAcknowledgement,
}

#[async_trait]
impl Harness for HistoryHarness {
    fn id(&self) -> HarnessId {
        self.provider
    }

    fn display_name(&self) -> &str {
        "L13 synthetic provider history"
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

    fn fork_capability(&self) -> ProviderForkCapability {
        ProviderForkCapability::Checkpoint
    }

    async fn fork_session(
        &self,
        request: ProviderForkRequest,
    ) -> Result<ProviderForkResult, HarnessError> {
        let source_checkpoint = match &request.boundary {
            luna_proto::ProviderForkBoundary::BeforeCheckpoint { checkpoint_id }
            | luna_proto::ProviderForkBoundary::ThroughCheckpoint { checkpoint_id } => {
                checkpoint_id.clone()
            }
            luna_proto::ProviderForkBoundary::Tip => {
                if request.source_session_id == "synthetic-history-source" {
                    "provider-message-1".into()
                } else {
                    let suffix = request
                        .source_session_id
                        .strip_prefix("synthetic-history-fork-")
                        .unwrap_or("1");
                    format!("fork-provider-message-{suffix}")
                }
            }
        };
        self.forks.lock().unwrap().push(request);
        let sequence = self.fork_sequence.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(ProviderForkResult {
            provider_session_id: format!("synthetic-history-fork-{sequence}"),
            checkpoint_map: std::collections::BTreeMap::from([(
                source_checkpoint,
                format!("fork-provider-message-{sequence}"),
            )]),
        })
    }

    async fn fork_session_staged(
        &self,
        request: ProviderForkRequest,
        progress: &ProviderForkProgressReporter,
    ) -> Result<ProviderForkResult, HarnessError> {
        if matches!(self.behavior, HistoryForkBehavior::FailPreflight) {
            return Err(HarnessError::Protocol(
                "synthetic fork preflight failed before mutation".into(),
            ));
        }
        progress(ProviderForkProgress::MutationStarted)?;
        let result = self.fork_session(request).await?;
        progress(ProviderForkProgress::Acknowledged {
            provider_session_id: result.provider_session_id.clone(),
        })?;
        if matches!(self.behavior, HistoryForkBehavior::FailAfterAcknowledgement) {
            return Err(HarnessError::Protocol(
                "synthetic fork readback failed after acknowledgement".into(),
            ));
        }
        Ok(result)
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(Vec::new())
    }

    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let session_id = request
            .resume
            .unwrap_or_else(|| "synthetic-history-source".into());
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: self.provider,
                model: "synthetic-history".into(),
                tools: Vec::new(),
                cwd: request.cwd,
                session_id: session_id.clone(),
                assistant_message_id: "assistant-history".into(),
            }),
            Ok(AgentEvent::TextDelta {
                text: "synthetic completed answer".into(),
            }),
            Ok(AgentEvent::ProviderCheckpoint {
                checkpoint_id: "provider-message-1".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(session_id),
            }),
        ])
        .boxed())
    }
}

#[async_trait]
impl Harness for CodexContextHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Codex
    }

    fn display_name(&self) -> &str {
        "Synthetic Codex context lifecycle"
    }

    fn supports_steering(&self) -> bool {
        false
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }

    fn deterministic_turn_end(&self) -> bool {
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
        let run_index = self.runs.fetch_add(1, Ordering::SeqCst);
        let release_context = self.release_context.clone();
        let release_done = self.release_done.clone();
        let outcome = self.outcome;
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let thread_id = request
                .resume
                .unwrap_or_else(|| "synthetic-codex-thread".into());
            let _ = tx
                .send(Ok(AgentEvent::SessionStarted {
                    harness: HarnessId::Codex,
                    model: "synthetic-codex".into(),
                    tools: Vec::new(),
                    cwd: request.cwd,
                    session_id: thread_id.clone(),
                    assistant_message_id: format!("synthetic-codex-assistant-{run_index}"),
                }))
                .await;
            if run_index == 0 {
                let operation_id =
                    format!("codex-auto:{thread_id}:synthetic-turn:synthetic-compact");
                let _ = tx
                    .send(Ok(AgentEvent::ContextCompaction {
                        operation_id: operation_id.clone(),
                        status: ContextOperationStatus::Running,
                        thread_id: thread_id.clone(),
                        turn_id: "synthetic-turn".into(),
                    }))
                    .await;
                release_context.notified().await;
                if matches!(outcome, CodexContextOutcome::Completed) {
                    let _ = tx
                        .send(Ok(AgentEvent::ContextCompaction {
                            operation_id,
                            status: ContextOperationStatus::Completed,
                            thread_id: thread_id.clone(),
                            turn_id: "synthetic-turn".into(),
                        }))
                        .await;
                }
                release_done.notified().await;
            }
            let _ = tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: Some(thread_id),
                }))
                .await;
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

#[async_trait]
impl Harness for CountingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }

    fn display_name(&self) -> &str {
        "L07 synthetic"
    }

    fn supports_steering(&self) -> bool {
        true
    }

    fn steering_mode(&self) -> SteeringMode {
        self.steering
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(Vec::new())
    }

    async fn compact_context(&self, _request: ContextCompactRequest) -> Result<(), HarnessError> {
        self.compactions.fetch_add(1, Ordering::SeqCst);
        if self.compact_error {
            Err(HarnessError::Protocol(
                "synthetic compact completion lost".into(),
            ))
        } else {
            Ok(())
        }
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let hold = self.hold;
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(AgentEvent::SessionStarted {
                    harness: HarnessId::Mock,
                    model: "synthetic".into(),
                    tools: Vec::new(),
                    cwd: request.cwd,
                    session_id: "synthetic-provider-session".into(),
                    assistant_message_id: "assistant-1".into(),
                }))
                .await;
            let status = if hold {
                let interrupt = controls.interrupt;
                let mut steering = controls.steering;
                loop {
                    tokio::select! {
                        _ = interrupt.cancelled() => break DoneStatus::Interrupted,
                        steer = steering.recv() => {
                            match steer {
                                Some(mut steer) => {
                                    if let Some(tx) = steer.acceptance.take() {
                                        let _ = tx.send(luna_harness::SteerAcceptance::Accepted);
                                    }
                                }
                                None => break DoneStatus::Completed,
                            }
                        }
                    }
                }
            } else {
                DoneStatus::Completed
            };
            let _ = tx
                .send(Ok(AgentEvent::Done {
                    status,
                    result: None,
                    error: None,
                    session_id: Some("synthetic-provider-session".into()),
                }))
                .await;
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

fn account_config(root: &std::path::Path) -> AgentAccountsConfig {
    let accounts = AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    accounts
        .disable_claude_keychain_fallback()
        .expect("synthetic accounts");
    accounts
}

fn assemble(root: &std::path::Path, runs: Arc<AtomicUsize>, hold: bool) -> EngineCore {
    assemble_with_steering(root, runs, hold, SteeringMode::StepBoundary)
}

fn assemble_with_steering(
    root: &std::path::Path,
    runs: Arc<AtomicUsize>,
    hold: bool,
    steering: SteeringMode,
) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CountingHarness {
        runs,
        compactions: Arc::new(AtomicUsize::new(0)),
        compact_error: false,
        hold,
        steering,
    }));
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("data"), "l07-org", "l07-user"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
        account_config(root),
    )
    .unwrap()
}

fn assemble_context(
    root: &std::path::Path,
    runs: Arc<AtomicUsize>,
    compactions: Arc<AtomicUsize>,
    hold: bool,
    compact_error: bool,
) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CountingHarness {
        runs,
        compactions,
        compact_error,
        hold,
        steering: SteeringMode::StepBoundary,
    }));
    EngineCore::assemble_with_profile_and_agent_accounts_config(
        EngineProfile::development(&root.join("data"), "l07-org", "l07-user"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
        account_config(root),
    )
    .unwrap()
}

fn assemble_codex_context(
    root: &std::path::Path,
    runs: Arc<AtomicUsize>,
    release_context: Arc<Notify>,
    release_done: Arc<Notify>,
    outcome: CodexContextOutcome,
) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CodexContextHarness {
        runs,
        release_context,
        release_done,
        outcome,
    }));
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("data"), "l07-org", "l07-user"),
        Arc::new(registry),
        HarnessId::Codex,
        None,
        account_config(root),
    )
    .unwrap()
}

fn assemble_history(
    root: &std::path::Path,
    forks: Arc<Mutex<Vec<ProviderForkRequest>>>,
    provider: HarnessId,
) -> EngineCore {
    assemble_history_with_behavior(root, forks, provider, HistoryForkBehavior::Success)
}

fn assemble_history_with_behavior(
    root: &std::path::Path,
    forks: Arc<Mutex<Vec<ProviderForkRequest>>>,
    provider: HarnessId,
    behavior: HistoryForkBehavior,
) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(HistoryHarness {
        provider,
        forks,
        fork_sequence: Arc::new(AtomicUsize::new(0)),
        behavior,
    }));
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("data"), "l13-org", "l13-user"),
        Arc::new(registry),
        provider,
        None,
        account_config(root),
    )
    .unwrap()
}

fn task_config(model: &str) -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some(model.into()),
        reasoning: Some(ReasoningLevel::Medium),
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: Some(HarnessId::Mock),
        provider_account_id: None,
        model: Some("synthetic".into()),
        reasoning: Some(ReasoningLevel::Medium),
        model_options: Default::default(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: Vec::new(),
        worktree: None,
    }
}

fn codex_task_config() -> ChatConfig {
    let mut config = task_config("synthetic-codex");
    config.harness = HarnessId::Codex;
    config
}

fn history_task_config(provider: HarnessId) -> ChatConfig {
    let mut config = task_config("synthetic-history");
    config.harness = provider;
    config
}

fn history_request(prompt: &str, provider: HarnessId) -> RunRequest {
    let mut request = request(prompt);
    request.harness = Some(provider);
    request.model = Some("synthetic-history".into());
    request
}

fn codex_request(prompt: &str) -> RunRequest {
    let mut request = request(prompt);
    request.harness = Some(HarnessId::Codex);
    request.model = Some("synthetic-codex".into());
    request
}

fn composer_draft(task_id: &str, text: &str) -> ComposerDraft {
    ComposerDraft {
        version: COMPOSER_DRAFT_VERSION,
        draft_id: format!("task:{task_id}"),
        task_id: Some(task_id.to_string()),
        revision: 1,
        text: text.to_string(),
        attachments: Vec::new(),
        appshots: Vec::new(),
        context: DraftContext::default(),
        execution: serde_json::Value::Null,
        updated_at_ms: 1,
    }
}

async fn snapshot(client: &RpcClient, task_id: &str) -> TaskCommandRead {
    client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": task_id }),
        )
        .await
        .unwrap()
}

fn history_review(
    core: &EngineCore,
    snapshot: &TaskCommandRead,
    task_id: &str,
    message_id: Option<&str>,
    message_text: Option<&str>,
) -> serde_json::Value {
    let chat = core.workspace.chat(task_id).unwrap().unwrap();
    serde_json::json!({
        "taskRevision": snapshot.snapshot.task_revision,
        "bindingGeneration": snapshot.snapshot.binding_generation.unwrap(),
        "providerSessionId": chat.harness_session_id.unwrap(),
        "providerAccountId": chat.config.as_ref().and_then(|config| config.provider_account_id.clone()),
        "cwd": chat.cwd.unwrap_or_else(|| ".".into()),
        "messageId": message_id,
        "messageText": message_text,
    })
}

async fn create_materialized_history_task(
    core: &EngineCore,
    client: &RpcClient,
    task_id: &str,
    provider: HarnessId,
) {
    assert_eq!(
        execute(
            client,
            envelope(
                &format!("history-create-{task_id}"),
                task_id,
                None,
                serde_json::json!({
                    "kind": "createChat",
                    "deviceId": core.device_id,
                    "cwd": "/tmp",
                    "config": history_task_config(provider),
                }),
            ),
        )
        .await
        .status,
        CommandReceiptStatus::Applied
    );
    execute(
        client,
        envelope(
            &format!("history-materialize-{task_id}"),
            task_id,
            Some(&snapshot(client, task_id).await),
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": history_request("source prompt", provider),
                    "messageId": format!("history-user-{task_id}"),
                }
            }),
        ),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let materialized = core
                .task_identity
                .view(task_id)
                .and_then(|view| view.current_binding)
                .is_some_and(|binding| !binding.provider_task_id.is_empty());
            if materialized
                && snapshot(client, task_id)
                    .await
                    .snapshot
                    .active_turn_id
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

fn envelope(
    command_id: &str,
    task_id: &str,
    snapshot: Option<&TaskCommandRead>,
    intent: serde_json::Value,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "commandId": command_id,
        "actorId": "test-user",
        "clientId": "test-client",
        "clientSessionId": "test-session",
        "taskId": task_id,
        "intent": intent,
    });
    if let Some(snapshot) = snapshot {
        value["expectedTaskRevision"] = snapshot.snapshot.task_revision.into();
        if let Some(generation) = snapshot.snapshot.binding_generation {
            value["expectedBindingGeneration"] = generation.into();
        }
    }
    value
}

async fn execute(client: &RpcClient, value: serde_json::Value) -> CommandReceipt {
    client
        .call_as(methods::EXECUTE_TASK_COMMAND, value)
        .await
        .unwrap()
}

async fn wait_for_receipt(
    client: &RpcClient,
    command_id: &str,
    status: CommandReceiptStatus,
) -> CommandReceipt {
    let last = Mutex::<Option<CommandReceipt>>::new(None);
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let receipt: Option<CommandReceipt> = client
                .call_as(
                    methods::GET_TASK_COMMAND_RECEIPT,
                    serde_json::json!({ "commandId": command_id }),
                )
                .await
                .unwrap();
            if let Some(receipt) = receipt {
                if receipt.status == status {
                    break receipt;
                }
                *last.lock().unwrap() = Some(receipt);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    outcome.unwrap_or_else(|_| {
        let observed = last
            .lock()
            .unwrap()
            .as_ref()
            .map(|receipt| {
                let reason = receipt
                    .reason
                    .as_ref()
                    .map(|reason| {
                        format!(
                            "{:?}: {}",
                            reason.code,
                            reason
                                .message
                                .replace('\n', " ")
                                .chars()
                                .take(160)
                                .collect::<String>()
                        )
                    })
                    .unwrap_or_else(|| "none".into());
                format!("status={:?} reason={reason}", receipt.status)
            })
            .unwrap_or_else(|| "no receipt observed".into());
        panic!("command {command_id} did not reach {status:?} within 5s; last {observed}");
    })
}

async fn create_task(client: &RpcClient, task_id: &str, device_id: &str) {
    let receipt = execute(
        client,
        envelope(
            &format!("create-command-{task_id}"),
            task_id,
            None,
            serde_json::json!({ "kind": "createChat", "deviceId": device_id }),
        ),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
}

async fn create_task_in_space(
    client: &RpcClient,
    task_id: &str,
    space_id: &str,
    cwd: &std::path::Path,
) {
    let receipt = execute(
        client,
        envelope(
            &format!("create-{task_id}"),
            task_id,
            None,
            serde_json::json!({
                "kind": "createChat",
                "spaceId": space_id,
                "cwd": cwd,
                "config": task_config("synthetic")
            }),
        ),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
}

async fn create_configured_task(client: &RpcClient, task_id: &str, device_id: &str) {
    let receipt = execute(
        client,
        envelope(
            "create-context-task",
            task_id,
            None,
            serde_json::json!({
                "kind": "createChat",
                "deviceId": device_id,
                "cwd": "/tmp",
                "config": task_config("synthetic")
            }),
        ),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
}

async fn create_codex_context_task(client: &RpcClient, task_id: &str, device_id: &str) {
    let receipt = execute(
        client,
        envelope(
            "create-codex-context-task",
            task_id,
            None,
            serde_json::json!({
                "kind": "createChat",
                "deviceId": device_id,
                "cwd": "/tmp",
                "config": codex_task_config()
            }),
        ),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
}

async fn start_codex_context_run(client: &RpcClient, task_id: &str) {
    let receipt = execute(
        client,
        envelope(
            "codex-context-run",
            task_id,
            Some(&snapshot(client, task_id).await),
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": codex_request("exercise automatic context lifecycle"),
                    "messageId": "codex-context-run-message"
                }
            }),
        ),
    )
    .await;
    assert!(matches!(
        receipt.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));
}

async fn wait_for_context_status(core: &EngineCore, task_id: &str, status: ContextOperationStatus) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if core
                .doc_host
                .context_control(task_id)
                .is_ok_and(|context| context.compact.status == status)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_for_run_count(runs: &AtomicUsize, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while runs.load(Ordering::SeqCst) < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn run_once(client: &RpcClient, task_id: &str, command_id: &str, prompt: &str) {
    let receipt = execute(
        client,
        envelope(
            command_id,
            task_id,
            Some(&snapshot(client, task_id).await),
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": request(prompt),
                    "messageId": format!("{command_id}-message")
                }
            }),
        ),
    )
    .await;
    assert!(matches!(
        receipt.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));
    wait_for_receipt(client, command_id, CommandReceiptStatus::Applied).await;
}

async fn wait_for_no_live_run(core: &EngineCore, task_id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while core.sessions.has_live_run(task_id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn steer_queue_case(
    steering: SteeringMode,
    with_attachment: bool,
) -> (CommandReceipt, QueueMessageStatus) {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble_with_steering(root.path(), runs, true, steering);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task(&client, &task_id, &core.device_id).await;

    let run = execute(
        &client,
        envelope(
            "steer-caller-run",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": request("hold active turn"),
                    "messageId": "steer-caller-run-message"
                }
            }),
        ),
    )
    .await;
    assert!(matches!(
        run.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));
    wait_for_receipt(&client, "steer-caller-run", CommandReceiptStatus::Applied).await;
    let _active = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let read = snapshot(&client, &task_id).await;
            if read.snapshot.active_turn_id.is_some() {
                break read;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let attachments = if with_attachment {
        vec!["/synthetic/queued.png".to_string()]
    } else {
        Vec::new()
    };
    core.doc_host
        .queue_message_with_id_and_execution(
            &task_id,
            "steer-caller-row",
            "steer this row",
            attachments,
            true,
            luna_proto::FollowUpBehavior::Steer,
            None,
        )
        .unwrap();
    let row = core
        .doc_host
        .queued_message(&task_id, "steer-caller-row")
        .unwrap()
        .unwrap();
    assert!(matches!(row.status, QueueMessageStatus::Held));

    let current = snapshot(&client, &task_id).await;
    let mut steer = envelope(
        "steer-caller-deliver",
        &task_id,
        Some(&current),
        serde_json::json!({
            "kind": "steerQueuedNow",
            "messageId": "steer-caller-row",
            "expectedQueueRevision": row.revision
        }),
    );
    steer["expectedTurnId"] = current.snapshot.active_turn_id.unwrap().into();
    let receipt = execute(&client, steer).await;
    let status = core
        .doc_host
        .queued_message(&task_id, "steer-caller-row")
        .unwrap()
        .expect("steer keeps the row until provider correlation")
        .status;
    core.shutdown().await;
    (receipt, status)
}

fn snapshot_row_exists(db_path: &std::path::Path, doc_id: &str) -> bool {
    rusqlite::Connection::open(db_path)
        .unwrap()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM snapshots WHERE doc_id=?1)",
            [doc_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_delete_identity_and_local_cleanup_share_the_receipt_transaction() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let db_path = root.path().join("data/orgs/l07-org/l07-user/docs.sqlite3");
    let core = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(core.rpc_service());
    let initial = snapshot(&client, &task_id).await;
    let create = envelope(
        "atomic-create-lifecycle",
        &task_id,
        Some(&initial),
        serde_json::json!({ "kind": "createChat", "deviceId": core.device_id }),
    );
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_create_lifecycle BEFORE INSERT ON task_command_events
             WHEN NEW.command_id='atomic-create-lifecycle'
             BEGIN SELECT RAISE(ABORT, 'forced create lifecycle failure'); END;",
        )
        .unwrap();
    }
    assert!(
        client
            .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, create.clone())
            .await
            .is_err()
    );
    assert!(core.workspace.chat(&task_id).unwrap().is_none());
    assert!(
        core.task_identity.view(&task_id).is_none(),
        "failed create must not pre-register a task identity"
    );
    let create_pending: Option<CommandReceipt> = client
        .call_as(
            methods::GET_TASK_COMMAND_RECEIPT,
            serde_json::json!({ "commandId": "atomic-create-lifecycle" }),
        )
        .await
        .unwrap();
    assert_eq!(
        create_pending.unwrap().status,
        CommandReceiptStatus::Pending
    );
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_create_lifecycle")
            .unwrap();
    }
    drop(client);
    drop(core);
    tokio::task::yield_now().await;

    let core = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(core.rpc_service());
    assert!(core.workspace.chat(&task_id).unwrap().is_none());
    assert!(core.task_identity.view(&task_id).is_none());
    let created = execute(&client, create).await;
    assert_eq!(created.status, CommandReceiptStatus::Applied);
    assert!(core.workspace.chat(&task_id).unwrap().is_some());
    assert!(!core.task_identity.view(&task_id).unwrap().task.tombstoned);
    let handle = core.doc_host.open(&task_id).unwrap();
    core.doc_host.flush_all();
    drop(handle);
    assert!(snapshot_row_exists(&db_path, &task_id));
    let blob = core.uploads.dir().join("l18-private-fixture.png");
    std::fs::create_dir_all(core.uploads.dir()).unwrap();
    std::fs::write(&blob, b"synthetic").unwrap();
    let mut local_draft = composer_draft(&task_id, "unsent fixture");
    local_draft.attachments.push(luna_proto::DraftBlob {
        attachment_id: "l18-private-fixture".into(),
        name: "l18-private-fixture.png".into(),
        path: Some(blob.to_string_lossy().to_string()),
        sha256: None,
        size: Some(9),
        state: luna_proto::DraftBlobState::Saved,
        error: None,
    });
    core.store
        .save_composer_draft(&core.device_id, &local_draft)
        .unwrap();
    core.store
        .prepare_task_cleanup(&core.device_id, &task_id, core.cleanup_boot_id_for_tests())
        .unwrap();
    let second_service = luna_rpc::memory_client(core.rpc_service());
    assert_eq!(core.cleanup_worker_count_for_tests(), 1);
    assert_eq!(
        core.store.task_cleanup_state(&task_id).unwrap().unwrap().0,
        "pending",
        "another RPC service must not abort an in-flight delete preparation"
    );
    drop(second_service);

    let before_delete = snapshot(&client, &task_id).await;
    let delete = envelope(
        "atomic-delete-lifecycle",
        &task_id,
        Some(&before_delete),
        serde_json::json!({ "kind": "delete" }),
    );
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_delete_lifecycle BEFORE INSERT ON task_command_events
             WHEN NEW.command_id='atomic-delete-lifecycle'
             BEGIN SELECT RAISE(ABORT, 'forced delete lifecycle failure'); END;",
        )
        .unwrap();
    }
    assert!(
        client
            .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, delete.clone())
            .await
            .is_err()
    );
    assert!(core.workspace.chat(&task_id).unwrap().is_some());
    assert!(
        !core.task_identity.view(&task_id).unwrap().task.tombstoned,
        "failed delete must not tombstone a surviving task"
    );
    assert!(snapshot_row_exists(&db_path, &task_id));
    assert!(blob.exists(), "failed delete retains an unsent blob");
    assert!(
        core.store.task_cleanup_state(&task_id).unwrap().is_none(),
        "failed registry commit reopens the surviving task for draft writes"
    );
    let failed_delete_read = snapshot(&client, &task_id).await;
    assert_eq!(
        failed_delete_read.snapshot.task_revision,
        before_delete.snapshot.task_revision
    );
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_delete_lifecycle")
            .unwrap();
    }
    drop(client);
    drop(core);
    tokio::task::yield_now().await;

    let core = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(core.rpc_service());
    assert!(core.workspace.chat(&task_id).unwrap().is_some());
    assert!(!core.task_identity.view(&task_id).unwrap().task.tombstoned);
    assert!(snapshot_row_exists(&db_path, &task_id));
    let deleted = execute(&client, delete).await;
    assert_eq!(
        deleted.status,
        CommandReceiptStatus::Applied,
        "{:?}",
        deleted.reason
    );
    assert_eq!(
        deleted.snapshot.task_revision,
        before_delete.snapshot.task_revision + 1
    );
    assert!(deleted.snapshot.binding_id.is_none());
    assert!(deleted.snapshot.binding_generation.is_none());
    assert!(core.workspace.chat(&task_id).unwrap().is_none());
    assert!(core.task_identity.view(&task_id).unwrap().task.tombstoned);
    assert!(
        !snapshot_row_exists(&db_path, &task_id),
        "local chat snapshot cleanup commits before the applied receipt returns"
    );
    assert!(!blob.exists(), "unshared LUNA-owned draft blob is removed");
    let cleanup: serde_json::Value = client
        .call_as(
            methods::GET_TASK_CLEANUP,
            serde_json::json!({ "taskId": task_id }),
        )
        .await
        .unwrap();
    assert_eq!(cleanup["state"], "completed");

    // A process loss after the durable commit has no remaining persistent
    // cleanup step to strand: registry, identity, chat doc, and receipt agree.
    drop(client);
    drop(core);
    tokio::task::yield_now().await;
    let final_core = assemble(root.path(), runs, false);
    let final_client = luna_rpc::memory_client(final_core.rpc_service());
    assert!(final_core.workspace.chat(&task_id).unwrap().is_none());
    assert!(
        final_core
            .task_identity
            .view(&task_id)
            .unwrap()
            .task
            .tombstoned
    );
    assert!(!snapshot_row_exists(&db_path, &task_id));
    let final_receipt: Option<CommandReceipt> = final_client
        .call_as(
            methods::GET_TASK_COMMAND_RECEIPT,
            serde_json::json!({ "commandId": "atomic-delete-lifecycle" }),
        )
        .await
        .unwrap();
    assert_eq!(final_receipt.unwrap(), deleted);
    let final_read = snapshot(&final_client, &task_id).await;
    assert_eq!(
        final_read.snapshot.task_revision,
        deleted.snapshot.task_revision
    );
    assert!(final_read.snapshot.binding_id.is_none());
    final_core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_delete_requires_exact_children_and_preserves_workspace_files() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let unrelated = root.path().join("unrelated");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&unrelated).unwrap();
    let live_file = project.join("keep.txt");
    std::fs::write(&live_file, b"workspace-owned").unwrap();
    let core = assemble(root.path(), Arc::new(AtomicUsize::new(0)), false);
    core.workspace
        .create_space(
            "project-a",
            &core.device_id,
            project.to_str().unwrap(),
            Some("Project A".into()),
            false,
        )
        .unwrap();
    core.workspace
        .create_space(
            "project-b",
            &core.device_id,
            unrelated.to_str().unwrap(),
            Some("Project B".into()),
            false,
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task_in_space(&client, "delete-a", "project-a", &project).await;
    create_task_in_space(&client, "keep-b", "project-b", &unrelated).await;
    core.workspace.set_chat_archived("delete-a", true).unwrap();
    core.store
        .save_activity_project_policy(&luna_proto::HistoryProjectPolicy {
            project_id: "project-a".into(),
            excluded_path_prefixes: vec!["private".into()],
            excluded_terms: Vec::new(),
            index_terminal_output: false,
            index_browser_fields: false,
            index_accessibility_text: false,
            index_screenshot_text: false,
        })
        .unwrap();

    let stale = client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "deleteSpace", "spaceId": "project-a", "expectedChatIds": []
            }),
        )
        .await;
    assert!(stale.is_err());
    assert!(core.workspace.space("project-a").unwrap().is_some());
    client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "deleteSpace", "spaceId": "project-a", "expectedChatIds": ["delete-a"]
            }),
        )
        .await
        .unwrap();
    assert!(core.workspace.space("project-a").unwrap().is_none());
    assert!(core.workspace.chat("delete-a").unwrap().is_none());
    assert!(core.workspace.chat("keep-b").unwrap().is_some());
    assert!(
        live_file.exists(),
        "live project file must never be removed"
    );
    let cleanup: serde_json::Value = client
        .call_as(
            methods::GET_TASK_CLEANUP,
            serde_json::json!({ "taskId": "delete-a" }),
        )
        .await
        .unwrap();
    assert_eq!(cleanup["state"], "completed");
    let project_cleanup: serde_json::Value = client
        .call_as(
            methods::GET_PROJECT_CLEANUP,
            serde_json::json!({ "projectId": "project-a" }),
        )
        .await
        .unwrap();
    assert_eq!(project_cleanup["state"], "completed");
    assert!(
        core.store
            .activity_project_policy("project-a")
            .unwrap()
            .is_none()
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_cleanup_failure_resumes_from_durable_child_list() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let core = assemble(root.path(), runs.clone(), false);
    core.workspace
        .create_space(
            "recover-project",
            &core.device_id,
            project.to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task_in_space(&client, "recover-project-task", "recover-project", &project).await;
    let candidate = core.uploads.dir().join("project-retry.png");
    std::fs::create_dir_all(&candidate).unwrap();
    let mut draft = composer_draft("recover-project-task", "unsent");
    draft.attachments.push(luna_proto::DraftBlob {
        attachment_id: "project-retry".into(),
        name: "project-retry.png".into(),
        path: Some(candidate.to_string_lossy().to_string()),
        sha256: None,
        size: None,
        state: luna_proto::DraftBlobState::Saved,
        error: None,
    });
    core.store
        .save_composer_draft(&core.device_id, &draft)
        .unwrap();
    client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "deleteSpace", "spaceId": "recover-project",
                "expectedChatIds": ["recover-project-task"]
            }),
        )
        .await
        .unwrap();
    let failed: serde_json::Value = client
        .call_as(
            methods::GET_PROJECT_CLEANUP,
            serde_json::json!({ "projectId": "recover-project" }),
        )
        .await
        .unwrap();
    assert_eq!(failed["state"], "failed");
    drop(client);
    drop(core);
    tokio::task::yield_now().await;
    std::fs::remove_dir(&candidate).unwrap();
    std::fs::write(&candidate, b"synthetic").unwrap();

    let reopened = assemble(root.path(), runs, false);
    assert!(
        reopened
            .workspace
            .space("recover-project")
            .unwrap()
            .is_none(),
        "committed project tombstone must survive reopening"
    );
    assert!(
        reopened.workspace.registry_synced(),
        "registry must be authoritative in this fixture"
    );
    assert!(
        reopened
            .uploads
            .owned_blob_path(candidate.to_str().unwrap())
            .is_some(),
        "upload root {:?}, candidate {:?}",
        reopened.uploads.dir(),
        candidate
    );
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let completed: serde_json::Value = client
        .call_as(
            methods::GET_PROJECT_CLEANUP,
            serde_json::json!({ "projectId": "recover-project" }),
        )
        .await
        .unwrap();
    assert_eq!(completed["state"], "completed", "{completed}");
    assert!(!candidate.exists());
    assert!(project.exists());
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_recovery_waits_for_durable_registry_delete() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let db_path = root.path().join("data/orgs/l07-org/l07-user/docs.sqlite3");
    let core = assemble(root.path(), runs.clone(), false);
    core.workspace
        .create_space(
            "flush-failure-project",
            &core.device_id,
            project.to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task_in_space(&client, &task_id, "flush-failure-project", &project).await;
    assert!(core.task_identity.view(&task_id).is_some());
    let handle = core.doc_host.open(&task_id).unwrap();
    core.doc_host.flush_all();
    drop(handle);
    assert!(snapshot_row_exists(&db_path, &task_id));
    let blob = core.uploads.dir().join("registry-failure-fixture.png");
    std::fs::create_dir_all(core.uploads.dir()).unwrap();
    std::fs::write(&blob, b"synthetic").unwrap();
    let mut draft = composer_draft(&task_id, "keep until durable");
    draft.attachments.push(luna_proto::DraftBlob {
        attachment_id: "registry-failure-fixture".into(),
        name: "registry-failure-fixture.png".into(),
        path: Some(blob.to_string_lossy().to_string()),
        sha256: None,
        size: Some(9),
        state: luna_proto::DraftBlobState::Saved,
        error: None,
    });
    core.store
        .save_composer_draft(&core.device_id, &draft)
        .unwrap();
    core.workspace.flush(); // persist the project/chat before injecting failure
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TRIGGER reject_l18_registry_delete BEFORE UPDATE ON snapshots
             WHEN NEW.doc_id='registry1'
             BEGIN SELECT RAISE(ABORT, 'synthetic registry write failure'); END;",
        )
        .unwrap();
    }
    let attempted = client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "deleteSpace", "spaceId": "flush-failure-project",
                "expectedChatIds": [task_id]
            }),
        )
        .await;
    assert!(attempted.is_err());
    let rpc = core.rpc_service();
    rpc.run_project_cleanup_maintenance_for_tests();
    let status: serde_json::Value = client
        .call_as(
            methods::GET_PROJECT_CLEANUP,
            serde_json::json!({ "projectId": "flush-failure-project" }),
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "failed");
    assert!(!core.task_identity.view(&task_id).unwrap().task.tombstoned);
    assert!(
        core.store
            .read_composer_draft(&core.device_id, Some(&task_id), None)
            .unwrap()
            .draft
            .is_some()
    );
    assert!(snapshot_row_exists(&db_path, &task_id));
    assert!(blob.exists());
    let persisted = core.store.load_snapshot("registry1").unwrap().unwrap();
    let persisted = luna_doc::RegistryDoc::from_bytes(&persisted, &core.device_id).unwrap();
    assert!(persisted.space("flush-failure-project").unwrap().is_some());
    assert!(persisted.chat(&task_id).unwrap().is_some());

    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER reject_l18_registry_delete")
            .unwrap();
    }
    rpc.run_project_cleanup_maintenance_for_tests();
    let status: serde_json::Value = client
        .call_as(
            methods::GET_PROJECT_CLEANUP,
            serde_json::json!({ "projectId": "flush-failure-project" }),
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "completed", "{status}");
    let persisted = core.store.load_snapshot("registry1").unwrap().unwrap();
    let persisted = luna_doc::RegistryDoc::from_bytes(&persisted, &core.device_id).unwrap();
    assert!(persisted.space("flush-failure-project").unwrap().is_none());
    assert!(persisted.chat(&task_id).unwrap().is_none());
    assert!(core.task_identity.view(&task_id).unwrap().task.tombstoned);
    assert!(!snapshot_row_exists(&db_path, &task_id));
    assert!(!blob.exists());
    drop(client);
    drop(rpc);
    drop(core);
    tokio::task::yield_now().await;
    let reopened = assemble(root.path(), runs, false);
    assert!(
        reopened
            .workspace
            .space("flush-failure-project")
            .unwrap()
            .is_none()
    );
    assert!(reopened.workspace.chat(&task_id).unwrap().is_none());
    assert!(
        reopened
            .task_identity
            .view(&task_id)
            .unwrap()
            .task
            .tombstoned
    );
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deletion_settles_a_held_synthetic_run_before_tombstoning() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path(), Arc::new(AtomicUsize::new(0)), true);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_configured_task(&client, "held-delete", &core.device_id).await;
    let start = execute(
        &client,
        envelope(
            "held-delete-run",
            "held-delete",
            Some(&snapshot(&client, "held-delete").await),
            serde_json::json!({ "kind": "session", "command": {
                "kind": "run", "request": request("hold for delete"),
                "messageId": "held-delete-message"
            }}),
        ),
    )
    .await;
    assert!(matches!(
        start.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));
    wait_for_receipt(&client, "held-delete-run", CommandReceiptStatus::Applied).await;
    assert!(core.sessions.has_live_run("held-delete"));
    let deleted = execute(
        &client,
        envelope(
            "held-delete-command",
            "held-delete",
            Some(&snapshot(&client, "held-delete").await),
            serde_json::json!({ "kind": "delete" }),
        ),
    )
    .await;
    assert_eq!(
        deleted.status,
        CommandReceiptStatus::Applied,
        "{:?}",
        deleted.reason
    );
    assert!(!core.sessions.has_live_run("held-delete"));
    assert!(core.workspace.chat("held-delete").unwrap().is_none());
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deletion_cleanup_failure_is_reported_and_resumes_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let core = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_configured_task(&client, "retry-delete", &core.device_id).await;
    let candidate = core.uploads.dir().join("retry-fixture.png");
    std::fs::create_dir_all(&candidate).unwrap(); // cannot be unlinked as a regular blob
    let mut draft = composer_draft("retry-delete", "unsent fixture");
    draft.attachments.push(luna_proto::DraftBlob {
        attachment_id: "retry-fixture".into(),
        name: "retry-fixture.png".into(),
        path: Some(candidate.to_string_lossy().to_string()),
        sha256: None,
        size: None,
        state: luna_proto::DraftBlobState::Saved,
        error: None,
    });
    core.store
        .save_composer_draft(&core.device_id, &draft)
        .unwrap();
    let receipt = execute(
        &client,
        envelope(
            "retry-delete-command",
            "retry-delete",
            Some(&snapshot(&client, "retry-delete").await),
            serde_json::json!({ "kind": "delete" }),
        ),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
    let failed: serde_json::Value = client
        .call_as(
            methods::GET_TASK_CLEANUP,
            serde_json::json!({ "taskId": "retry-delete" }),
        )
        .await
        .unwrap();
    assert_eq!(failed["state"], "failed");
    let summary: serde_json::Value = client
        .call_as(methods::GET_DELETION_CLEANUP_SUMMARY, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(summary["failed"], 1);
    assert!(candidate.is_dir());
    drop(client);
    drop(core);
    tokio::task::yield_now().await;
    std::fs::remove_dir(&candidate).unwrap();
    std::fs::write(&candidate, b"synthetic").unwrap();

    let reopened = assemble(root.path(), runs, false);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let completed: serde_json::Value = client
        .call_as(
            methods::GET_TASK_CLEANUP,
            serde_json::json!({ "taskId": "retry-delete" }),
        )
        .await
        .unwrap();
    assert_eq!(completed["state"], "completed");
    let summary: serde_json::Value = client
        .call_as(methods::GET_DELETION_CLEANUP_SUMMARY, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(summary["failed"], 0);
    assert!(!candidate.exists());
    assert!(reopened.workspace.chat("retry-delete").unwrap().is_none());
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandoned_task_preparation_reopens_drafts_without_deleting_data() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_configured_task(&client, &task_id, &core.device_id).await;
    let blob = core.uploads.dir().join("abandoned-preparation.png");
    std::fs::create_dir_all(core.uploads.dir()).unwrap();
    std::fs::write(&blob, b"synthetic").unwrap();
    let mut draft = composer_draft(&task_id, "before crash");
    draft.attachments.push(luna_proto::DraftBlob {
        attachment_id: "abandoned-preparation".into(),
        name: "abandoned-preparation.png".into(),
        path: Some(blob.to_string_lossy().to_string()),
        sha256: None,
        size: Some(9),
        state: luna_proto::DraftBlobState::Saved,
        error: None,
    });
    core.store
        .save_composer_draft(&core.device_id, &draft)
        .unwrap();
    core.store
        .prepare_task_cleanup(&core.device_id, &task_id, core.cleanup_boot_id_for_tests())
        .unwrap();
    drop(client);
    drop(core);
    tokio::task::yield_now().await;

    let reopened = assemble(root.path(), runs, false);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    assert!(reopened.workspace.chat(&task_id).unwrap().is_some());
    assert!(
        !reopened
            .task_identity
            .view(&task_id)
            .unwrap()
            .task
            .tombstoned
    );
    assert!(blob.exists());
    draft.revision += 1;
    draft.text = "after reopen".into();
    reopened
        .store
        .save_composer_draft(&reopened.device_id, &draft)
        .unwrap();
    assert!(
        reopened
            .store
            .task_cleanup_state(&task_id)
            .unwrap()
            .is_none()
    );
    drop(client);
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_project_preparation_never_deletes_a_moved_survivor() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let nested = repo.join("nested");
    init_git_repo(&repo);
    std::fs::create_dir_all(&nested).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs.clone(), false);
    core.workspace
        .create_space(
            "old-project",
            &core.device_id,
            nested.to_str().unwrap(),
            Some("Old".into()),
            true,
        )
        .unwrap();
    core.workspace
        .create_space(
            "new-project",
            &core.device_id,
            repo.to_str().unwrap(),
            Some("New".into()),
            true,
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task_in_space(&client, &task_id, "old-project", &nested).await;
    assert!(core.task_identity.view(&task_id).is_some());
    let handle = core.doc_host.open(&task_id).unwrap();
    core.doc_host.flush_all();
    drop(handle);
    let blob = core.uploads.dir().join("moved-survivor.png");
    std::fs::create_dir_all(core.uploads.dir()).unwrap();
    std::fs::write(&blob, b"synthetic").unwrap();
    let mut draft = composer_draft(&task_id, "keep me");
    draft.attachments.push(luna_proto::DraftBlob {
        attachment_id: "moved-survivor".into(),
        name: "moved-survivor.png".into(),
        path: Some(blob.to_string_lossy().to_string()),
        sha256: None,
        size: Some(9),
        state: luna_proto::DraftBlobState::Saved,
        error: None,
    });
    core.store
        .save_composer_draft(&core.device_id, &draft)
        .unwrap();
    core.store
        .prepare_project_cleanup(
            "old-project",
            std::slice::from_ref(&task_id),
            core.cleanup_boot_id_for_tests(),
        )
        .unwrap();
    core.store
        .prepare_task_cleanup(&core.device_id, &task_id, core.cleanup_boot_id_for_tests())
        .unwrap();
    let moved = execute(
        &client,
        envelope(
            "move-after-prepare",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({"kind":"moveProject","destinationSpaceId":"new-project"}),
        ),
    )
    .await;
    assert_eq!(
        moved.status,
        CommandReceiptStatus::Applied,
        "{:?}",
        moved.reason
    );
    core.workspace.delete_space("old-project").unwrap();
    core.workspace.flush();
    drop(client);
    drop(core);
    tokio::task::yield_now().await;

    let reopened = assemble(root.path(), runs, false);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let survivor = reopened.workspace.chat(&task_id).unwrap().unwrap();
    assert_eq!(survivor.space_id.as_deref(), Some("new-project"));
    assert!(
        !reopened
            .task_identity
            .view(&task_id)
            .unwrap()
            .task
            .tombstoned
    );
    assert!(reopened.store.has_snapshot(&task_id).unwrap());
    assert!(blob.exists());
    draft.revision += 1;
    reopened
        .store
        .save_composer_draft(&reopened.device_id, &draft)
        .unwrap();
    drop(client);
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cleanup_worker_advances_beyond_one_batch_without_touching_live_tasks() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path(), Arc::new(AtomicUsize::new(0)), false);
    for index in 0..21 {
        let task = format!("pending-live-{index:02}");
        core.workspace
            .create_chat(&task, None, Some(&core.device_id), None, None)
            .unwrap();
        core.store
            .prepare_task_cleanup(&core.device_id, &task, "prior-boot")
            .unwrap();
    }
    let _first = luna_rpc::memory_client(core.rpc_service());
    let _second = luna_rpc::memory_client(core.rpc_service());
    assert_eq!(core.cleanup_worker_count_for_tests(), 1);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if core.store.pending_task_cleanups(100).unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for index in 0..21 {
        assert!(
            core.workspace
                .chat(&format!("pending-live-{index:02}"))
                .unwrap()
                .is_some()
        );
    }
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_projection_rolls_back_with_terminal_receipt_failure_and_recovers_after_reopen() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task(&client, &task_id, &core.device_id).await;
    let before = snapshot(&client, &task_id).await;
    assert!(
        before.events.is_empty(),
        "ordinary snapshots do not scan history"
    );
    let mut chats = core.workspace.watch_chats();
    assert!(
        chats
            .borrow()
            .iter()
            .any(|chat| chat.id == task_id && chat.title.is_none())
    );

    let db_path = root.path().join("data/orgs/l07-org/l07-user/docs.sqlite3");
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_atomic_projection BEFORE INSERT ON task_command_events
             WHEN NEW.command_id='atomic-rename' BEGIN SELECT RAISE(ABORT, 'forced'); END;",
        )
        .unwrap();
    }
    let command = envelope(
        "atomic-rename",
        &task_id,
        Some(&before),
        serde_json::json!({ "kind": "rename", "title": "durable title" }),
    );
    let failed = client
        .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, command.clone())
        .await;
    assert!(
        failed.is_err(),
        "faulted terminal transaction must not acknowledge"
    );
    assert_eq!(
        core.workspace.chat(&task_id).unwrap().unwrap().title,
        None,
        "candidate registry state is rolled back before readers can observe it"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(150), chats.changed())
            .await
            .is_err(),
        "rolled-back projection is never published to workspace watchers"
    );
    let failed_read = snapshot(&client, &task_id).await;
    assert_eq!(
        failed_read.snapshot.task_revision,
        before.snapshot.task_revision
    );
    assert_eq!(
        failed_read.snapshot.task_sequence,
        before.snapshot.task_sequence
    );
    let pending: Option<CommandReceipt> = client
        .call_as(
            methods::GET_TASK_COMMAND_RECEIPT,
            serde_json::json!({ "commandId": "atomic-rename" }),
        )
        .await
        .unwrap();
    assert_eq!(pending.unwrap().status, CommandReceiptStatus::Pending);
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_atomic_projection")
            .unwrap();
    }

    // Drop without the orderly flush path to model process loss immediately
    // after the failed terminal transaction.
    drop(chats);
    drop(client);
    drop(core);
    tokio::task::yield_now().await;
    let reopened = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    assert_eq!(
        reopened.workspace.chat(&task_id).unwrap().unwrap().title,
        None,
        "a crash/reopen boundary retains the rolled-back task value"
    );
    let reopened_read = snapshot(&client, &task_id).await;
    assert_eq!(
        reopened_read.snapshot.task_revision,
        before.snapshot.task_revision
    );
    let reopened_pending: Option<CommandReceipt> = client
        .call_as(
            methods::GET_TASK_COMMAND_RECEIPT,
            serde_json::json!({ "commandId": "atomic-rename" }),
        )
        .await
        .unwrap();
    assert_eq!(
        reopened_pending.unwrap().status,
        CommandReceiptStatus::Pending
    );
    let applied = execute(&client, command).await;
    assert_eq!(applied.status, CommandReceiptStatus::Applied);
    assert_eq!(
        applied.snapshot.task_revision,
        before.snapshot.task_revision + 1
    );
    assert_eq!(
        reopened
            .workspace
            .chat(&task_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("durable title")
    );
    reopened.shutdown().await;
    drop(reopened);

    let final_core = assemble(root.path(), runs, false);
    let final_client = luna_rpc::memory_client(final_core.rpc_service());
    assert_eq!(
        final_core
            .workspace
            .chat(&task_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("durable title")
    );
    let final_receipt: Option<CommandReceipt> = final_client
        .call_as(
            methods::GET_TASK_COMMAND_RECEIPT,
            serde_json::json!({ "commandId": "atomic-rename" }),
        )
        .await
        .unwrap();
    assert_eq!(final_receipt.unwrap(), applied);
    let final_read = snapshot(&final_client, &task_id).await;
    assert_eq!(
        final_read.snapshot.task_revision,
        applied.snapshot.task_revision
    );
    assert_eq!(
        final_read.snapshot.task_sequence,
        applied.snapshot.task_sequence
    );
    final_core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_send_survives_two_clients_response_loss_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs.clone(), false);
    let client_a = luna_rpc::memory_client(core.rpc_service());
    let client_b = luna_rpc::memory_client(core.rpc_service());
    create_task(&client_a, &task_id, &core.device_id).await;
    core.workspace.rename_chat(&task_id, "Pre-titled").unwrap();
    let before = snapshot(&client_a, &task_id).await;
    let mut published = client_a
        .subscribe(
            methods::WATCH_TASK_COMMANDS,
            serde_json::json!({
                "taskId": task_id,
                "afterTaskSequence": before.snapshot.task_sequence,
            }),
        )
        .await
        .unwrap();
    assert_eq!(published.recv().await.unwrap()["kind"], "snapshot");
    let command = envelope(
        "send-once",
        &task_id,
        Some(&before),
        serde_json::json!({
            "kind": "session",
            "command": {
                "kind": "run",
                "request": request("only once"),
                "messageId": "message-once"
            }
        }),
    );

    let (first, concurrent_retry) = tokio::join!(
        execute(&client_a, command.clone()),
        execute(&client_b, command.clone())
    );
    assert!(matches!(
        first.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));
    assert!(matches!(
        concurrent_retry.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));
    let applied = wait_for_receipt(&client_a, "send-once", CommandReceiptStatus::Applied).await;
    let published_applied: CommandReceipt = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = published.recv().await.unwrap();
            if value["kind"] == "receipt" && value["receipt"]["status"] == "applied" {
                break serde_json::from_value(value["receipt"].clone()).unwrap();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        published_applied, applied,
        "publication occurs only after the durable receipt is readable"
    );
    assert_eq!(applied.correlation_id.as_deref(), Some("message-once"));
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    // Simulate a caller that lost the applied response and retransmits the
    // original serialized envelope: it receives the same receipt, no effect.
    let after_loss = execute(&client_b, command.clone()).await;
    assert_eq!(after_loss, applied);
    let entries = core
        .doc_host
        .open(&task_id)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.role == MessageRole::User && entry.id == "message-once")
            .count(),
        1
    );

    let conflict = execute(
        &client_b,
        envelope(
            "send-once",
            "different-task",
            None,
            serde_json::json!({ "kind": "markSeen" }),
        ),
    )
    .await;
    assert_eq!(conflict.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        conflict.reason.unwrap().code,
        CommandReasonCode::CommandIdConflict
    );

    core.shutdown().await;
    drop(client_a);
    drop(client_b);
    drop(core);
    let reopened = assemble(root.path(), runs.clone(), false);
    let restarted_client = luna_rpc::memory_client(reopened.rpc_service());
    let after_restart = execute(&restarted_client, command).await;
    assert_eq!(after_restart, applied);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn durable_queue_commands_dedupe_and_revision_fence_edit_remove() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs, false);
    let client_a = luna_rpc::memory_client(core.rpc_service());
    let client_b = luna_rpc::memory_client(core.rpc_service());
    create_task(&client_a, &task_id, &core.device_id).await;
    core.doc_host.open(&task_id).unwrap();
    core.doc_host.pause_all_queues();

    let before = snapshot(&client_a, &task_id).await;
    let enqueue = envelope(
        "queue-command-1",
        &task_id,
        Some(&before),
        serde_json::json!({
            "kind": "queueMessage",
            "messageId": "queue-message-1",
            "text": "original queue text",
            "holdForTurnEnd": true,
            "execution": {
                "harness": "mock",
                "providerAccountId": null,
                "model": "frozen-model",
                "reasoning": "medium",
                "modelOptions": {},
                "sandbox": "workspace-write",
                "permissionMode": "ask",
                "effectivePermissionMode": "ask"
            }
        }),
    );
    let (first, duplicate) = tokio::join!(
        execute(&client_a, enqueue.clone()),
        execute(&client_b, enqueue.clone())
    );
    assert_eq!(first.status, CommandReceiptStatus::Applied);
    assert_eq!(duplicate, first);
    let queue = core
        .doc_host
        .open(&task_id)
        .unwrap()
        .doc()
        .read_queue()
        .unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].revision, 1);
    assert_eq!(
        queue[0]
            .execution
            .as_ref()
            .and_then(|config| config.model.as_deref()),
        Some("frozen-model")
    );

    let lease = client_a
        .call(
            methods::BEGIN_QUEUED_MESSAGE_EDIT,
            serde_json::json!({
                "chatId": task_id,
                "id": "queue-message-1",
                "editorDeviceId": core.device_id,
                "editorInstanceId": "test-window"
            }),
        )
        .await
        .unwrap();
    assert_eq!(lease["outcome"], "acquired");
    assert_eq!(lease["revision"], 1);
    let after_enqueue = snapshot(&client_a, &task_id).await;
    let edit_command = envelope(
        "queue-edit-1",
        &task_id,
        Some(&after_enqueue),
        serde_json::json!({
            "kind": "editQueuedMessage",
            "messageId": "queue-message-1",
            "expectedQueueRevision": 1,
            "leaseId": lease["leaseId"],
            "text": "edited queue text",
            "expectedTextHash": lease["baseTextHash"],
            "attachments": []
        }),
    );
    let db_path = root.path().join("data/orgs/l07-org/l07-user/docs.sqlite3");
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_queue_edit_receipt BEFORE INSERT ON task_command_events
             WHEN NEW.command_id='queue-edit-1'
             BEGIN SELECT RAISE(ABORT, 'forced queue edit receipt loss'); END;",
        )
        .unwrap();
    }
    assert!(
        client_a
            .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, edit_command.clone())
            .await
            .is_err(),
        "the atomic queue projection/receipt transaction must surface the failure"
    );
    let unchanged = core
        .doc_host
        .queued_message(&task_id, "queue-message-1")
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.text, "original queue text");
    assert_eq!(unchanged.revision, 1);
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_queue_edit_receipt")
            .unwrap();
    }
    let edit = execute(&client_a, edit_command).await;
    assert_eq!(edit.status, CommandReceiptStatus::Applied);
    assert_eq!(
        core.doc_host
            .queued_message(&task_id, "queue-message-1")
            .unwrap()
            .unwrap()
            .revision,
        2,
        "the exact retry applies the edit once after durable storage recovers"
    );

    let after_edit = snapshot(&client_a, &task_id).await;
    let stale_remove = execute(
        &client_a,
        envelope(
            "queue-remove-stale",
            &task_id,
            Some(&after_edit),
            serde_json::json!({
                "kind": "removeQueuedMessage",
                "messageId": "queue-message-1",
                "expectedQueueRevision": 1
            }),
        ),
    )
    .await;
    assert_eq!(stale_remove.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        stale_remove.reason.unwrap().code,
        CommandReasonCode::QueueRevisionConflict
    );

    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(&format!(
            "CREATE TRIGGER fail_queue_remove_snapshot BEFORE UPDATE ON snapshots
             WHEN OLD.doc_id='{task_id}'
             BEGIN SELECT RAISE(ABORT, 'forced queue remove snapshot failure'); END;"
        ))
        .unwrap();
    }
    let failed_remove = execute(
        &client_a,
        envelope(
            "queue-remove-storage-failure",
            &task_id,
            Some(&snapshot(&client_a, &task_id).await),
            serde_json::json!({
                "kind": "removeQueuedMessage",
                "messageId": "queue-message-1",
                "expectedQueueRevision": 2
            }),
        ),
    )
    .await;
    assert_ne!(failed_remove.status, CommandReceiptStatus::Applied);
    assert_eq!(
        core.doc_host
            .queued_message(&task_id, "queue-message-1")
            .unwrap()
            .unwrap()
            .revision,
        2,
        "failed durable removal must leave the live queue projection intact"
    );
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_queue_remove_snapshot")
            .unwrap();
    }

    let remove = envelope(
        "queue-remove-current",
        &task_id,
        Some(&snapshot(&client_a, &task_id).await),
        serde_json::json!({
            "kind": "removeQueuedMessage",
            "messageId": "queue-message-1",
            "expectedQueueRevision": 2
        }),
    );
    let removed = execute(&client_a, remove.clone()).await;
    assert_eq!(removed.status, CommandReceiptStatus::Applied);
    assert!(
        core.doc_host
            .queued_message(&task_id, "queue-message-1")
            .unwrap()
            .is_none()
    );
    assert_eq!(execute(&client_b, remove).await, removed);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_queue_snapshot_never_applies_or_clears_the_draft_and_retry_reopens_once() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let message_id = "durable-enqueue-row";
    let failed_command_id = "durable-enqueue-failed";
    let core = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task(&client, &task_id, &core.device_id).await;
    core.doc_host.open(&task_id).unwrap();
    core.doc_host.pause_all_queues();

    let draft = composer_draft(&task_id, "draft survives failed queue persistence");
    client
        .call(
            methods::SAVE_COMPOSER_DRAFT,
            serde_json::json!({ "draft": draft }),
        )
        .await
        .unwrap();
    client
        .call(
            methods::BEGIN_DRAFT_SUBMISSION,
            serde_json::json!({
                "commandId": failed_command_id,
                "taskId": task_id,
                "snapshot": draft,
            }),
        )
        .await
        .unwrap();

    let db_path = root.path().join("data/orgs/l07-org/l07-user/docs.sqlite3");
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(&format!(
            "CREATE TRIGGER fail_queue_snapshot BEFORE UPDATE ON snapshots
             WHEN OLD.doc_id='{task_id}'
             BEGIN SELECT RAISE(ABORT, 'forced queue snapshot failure'); END;"
        ))
        .unwrap();
    }
    let failed = execute(
        &client,
        envelope(
            failed_command_id,
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "queueMessage",
                "messageId": message_id,
                "text": "persist exactly once",
                "holdForTurnEnd": true,
                "execution": null,
            }),
        ),
    )
    .await;
    assert_ne!(failed.status, CommandReceiptStatus::Applied);
    assert!(
        core.doc_host
            .queued_message(&task_id, message_id)
            .unwrap()
            .is_none(),
        "a failed projection transaction must not mutate the watched queue"
    );
    let draft_read: ComposerDraftRead = client
        .call_as(
            methods::GET_COMPOSER_DRAFT,
            serde_json::json!({ "taskId": task_id }),
        )
        .await
        .unwrap();
    assert_eq!(
        draft_read.draft.as_ref().map(|draft| draft.text.as_str()),
        Some("draft survives failed queue persistence")
    );
    assert!(draft_read.submissions.iter().any(|submission| {
        submission.command_id == failed_command_id
            && submission.state == DraftSubmissionState::Pending
    }));

    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_queue_snapshot")
            .unwrap();
    }
    drop(client);
    drop(core);

    let reopened = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    reopened.doc_host.open(&task_id).unwrap();
    reopened.doc_host.pause_all_queues();
    assert!(
        reopened
            .doc_host
            .queued_message(&task_id, message_id)
            .unwrap()
            .is_none()
    );
    let retry_command_id = "durable-enqueue-retry";
    client
        .call(
            methods::BEGIN_DRAFT_SUBMISSION,
            serde_json::json!({
                "commandId": retry_command_id,
                "taskId": task_id,
                "snapshot": draft,
            }),
        )
        .await
        .unwrap();
    let retried = execute(
        &client,
        envelope(
            retry_command_id,
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "queueMessage",
                "messageId": message_id,
                "text": "persist exactly once",
                "holdForTurnEnd": true,
                "execution": null,
            }),
        ),
    )
    .await;
    assert_eq!(retried.status, CommandReceiptStatus::Applied);
    client
        .call(
            methods::SETTLE_DRAFT_SUBMISSION,
            serde_json::json!({
                "commandId": retry_command_id,
                "outcome": "applied",
                "queueId": message_id,
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        reopened
            .doc_host
            .open(&task_id)
            .unwrap()
            .doc()
            .read_queue()
            .unwrap()
            .iter()
            .filter(|item| item.id == message_id)
            .count(),
        1
    );
    drop(client);
    drop(reopened);

    let after_restart = assemble(root.path(), runs.clone(), false);
    after_restart.doc_host.open(&task_id).unwrap();
    assert_eq!(
        after_restart
            .doc_host
            .open(&task_id)
            .unwrap()
            .doc()
            .read_queue()
            .unwrap()
            .iter()
            .filter(|item| item.id == message_id)
            .count(),
        1
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    after_restart.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_edit_and_remove_projections_stay_absent_after_abrupt_reopen() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task(&client, &task_id, &core.device_id).await;
    core.doc_host.open(&task_id).unwrap();
    core.doc_host.pause_all_queues();
    assert_eq!(
        execute(
            &client,
            envelope(
                "projection-failure-enqueue",
                &task_id,
                Some(&snapshot(&client, &task_id).await),
                serde_json::json!({
                    "kind": "queueMessage",
                    "messageId": "projection-failure-row",
                    "text": "original",
                    "holdForTurnEnd": true
                }),
            ),
        )
        .await
        .status,
        CommandReceiptStatus::Applied
    );
    let lease = client
        .call(
            methods::BEGIN_QUEUED_MESSAGE_EDIT,
            serde_json::json!({
                "chatId": task_id,
                "id": "projection-failure-row",
                "editorDeviceId": core.device_id,
                "editorInstanceId": "failure-test"
            }),
        )
        .await
        .unwrap();
    let db_path = root.path().join("data/orgs/l07-org/l07-user/docs.sqlite3");
    let original_authority: String = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row(
            "SELECT payload_hash FROM local_executable_authority WHERE doc_id=?1 AND kind='queue' AND row_id='projection-failure-row'",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(&format!(
            "CREATE TRIGGER fail_edit_projection BEFORE UPDATE ON snapshots
             WHEN OLD.doc_id='{task_id}'
             BEGIN SELECT RAISE(ABORT, 'forced edit projection failure'); END;"
        ))
        .unwrap();
    }
    let failed_edit = execute(
        &client,
        envelope(
            "projection-failure-edit",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "editQueuedMessage",
                "messageId": "projection-failure-row",
                "expectedQueueRevision": 1,
                "leaseId": lease["leaseId"],
                "text": "must not persist",
                "expectedTextHash": lease["baseTextHash"],
                "attachments": []
            }),
        ),
    )
    .await;
    assert_ne!(failed_edit.status, CommandReceiptStatus::Applied);
    assert_eq!(
        core.doc_host
            .queued_message(&task_id, "projection-failure-row")
            .unwrap()
            .unwrap()
            .text,
        "original"
    );
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_edit_projection")
            .unwrap();
    }
    drop(client);
    drop(core);

    let reopened = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let row = reopened
        .doc_host
        .queued_message(&task_id, "projection-failure-row")
        .unwrap()
        .unwrap();
    assert_eq!(row.text, "original");
    assert_eq!(row.revision, 1);
    let after_failed_edit: String = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row(
            "SELECT payload_hash FROM local_executable_authority WHERE doc_id=?1 AND kind='queue' AND row_id='projection-failure-row'",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after_failed_edit, original_authority);
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(&format!(
            "CREATE TRIGGER fail_remove_projection BEFORE UPDATE ON snapshots
             WHEN OLD.doc_id='{task_id}'
             BEGIN SELECT RAISE(ABORT, 'forced remove projection failure'); END;"
        ))
        .unwrap();
    }
    let failed_remove = execute(
        &client,
        envelope(
            "projection-failure-remove",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "removeQueuedMessage",
                "messageId": "projection-failure-row",
                "expectedQueueRevision": 1
            }),
        ),
    )
    .await;
    assert_ne!(failed_remove.status, CommandReceiptStatus::Applied);
    assert!(
        reopened
            .doc_host
            .queued_message(&task_id, "projection-failure-row")
            .unwrap()
            .is_some()
    );
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_remove_projection")
            .unwrap();
    }
    drop(client);
    drop(reopened);

    let after_restart = assemble(root.path(), runs, false);
    let row = after_restart
        .doc_host
        .queued_message(&task_id, "projection-failure-row")
        .unwrap()
        .unwrap();
    assert_eq!(row.text, "original");
    assert_eq!(row.revision, 1);
    let after_failed_remove: String = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row(
            "SELECT payload_hash FROM local_executable_authority WHERE doc_id=?1 AND kind='queue' AND row_id='projection-failure-row'",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after_failed_remove, original_authority);
    after_restart.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_durable_claim_never_reaches_the_provider_and_reopens_held() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs.clone(), true);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task(&client, &task_id, &core.device_id).await;

    let run = execute(
        &client,
        envelope(
            "claim-failure-run",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": request("hold the provider"),
                    "messageId": "claim-failure-run-message"
                }
            }),
        ),
    )
    .await;
    assert!(matches!(
        run.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));
    wait_for_receipt(&client, "claim-failure-run", CommandReceiptStatus::Applied).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    let queued = execute(
        &client,
        envelope(
            "claim-failure-enqueue",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "queueMessage",
                "messageId": "claim-failure-row",
                "text": "must stay held",
                "holdForTurnEnd": true
            }),
        ),
    )
    .await;
    assert_eq!(queued.status, CommandReceiptStatus::Applied);

    let db_path = root.path().join("data/orgs/l07-org/l07-user/docs.sqlite3");
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(&format!(
            "CREATE TRIGGER fail_queue_claim_snapshot BEFORE UPDATE ON snapshots
             WHEN OLD.doc_id='{task_id}'
             BEGIN SELECT RAISE(ABORT, 'forced queue claim snapshot failure'); END;"
        ))
        .unwrap();
    }
    let send = execute(
        &client,
        envelope(
            "claim-failure-send",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "sendQueuedNow",
                "messageId": "claim-failure-row",
                "expectedQueueRevision": 1
            }),
        ),
    )
    .await;
    assert_ne!(send.status, CommandReceiptStatus::Applied);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "claim persistence must precede provider dispatch"
    );
    let row = core
        .doc_host
        .queued_message(&task_id, "claim-failure-row")
        .unwrap()
        .unwrap();
    assert!(matches!(row.status, luna_doc::QueueMessageStatus::Held));

    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_queue_claim_snapshot")
            .unwrap();
    }
    core.doc_host.pause_all_queues();
    core.shutdown().await;
    drop(client);
    drop(core);
    let reopened = assemble(root.path(), runs.clone(), false);
    let row = reopened
        .doc_host
        .queued_message(&task_id, "claim-failure-row")
        .unwrap()
        .unwrap();
    assert!(matches!(row.status, luna_doc::QueueMessageStatus::Held));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_correlation_commit_keeps_receipt_pending_and_claim_visible_on_reopen() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let command_id = "correlation-storage-failure";
    let message_id = "correlation-storage-row";
    let core = assemble(root.path(), runs.clone(), false);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task(&client, &task_id, &core.device_id).await;
    core.doc_host.open(&task_id).unwrap();
    core.doc_host.pause_all_queues();
    assert_eq!(
        execute(
            &client,
            envelope(
                "correlation-storage-enqueue",
                &task_id,
                Some(&snapshot(&client, &task_id).await),
                serde_json::json!({
                    "kind": "queueMessage",
                    "messageId": message_id,
                    "text": "claimed until correlation is durable",
                    "holdForTurnEnd": true
                }),
            ),
        )
        .await
        .status,
        CommandReceiptStatus::Applied
    );

    let task_snapshot = snapshot(&client, &task_id).await.snapshot;
    let now = chrono::Utc::now().timestamp_millis();
    let begin = core
        .doc_host
        .begin_task_command(
            CommandReceipt {
                command_id: command_id.into(),
                task_id: task_id.clone(),
                intent: "sendQueuedNow".into(),
                payload_hash: "correlation-storage-payload".into(),
                status: CommandReceiptStatus::Pending,
                snapshot: task_snapshot,
                event_sequence: None,
                actor_id: core.device_id.clone(),
                client_id: "test".into(),
                client_session_id: "test".into(),
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
    assert_eq!(begin.kind, luna_sync::TaskCommandBeginKind::Accepted);
    core.doc_host
        .set_task_command_correlation(command_id, message_id)
        .unwrap();
    let handle = core.doc_host.open(&task_id).unwrap();
    assert!(matches!(
        handle
            .doc()
            .claim_queued(message_id, 1, command_id, QueueClaimMode::SendNow, now)
            .unwrap(),
        luna_doc::QueueClaimOutcome::Claimed(_)
    ));
    core.doc_host.flush_all();

    let db_path = root.path().join("data/orgs/l07-org/l07-user/docs.sqlite3");
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(&format!(
            "CREATE TRIGGER fail_correlation_snapshot BEFORE UPDATE ON snapshots
             WHEN OLD.doc_id='{task_id}'
             BEGIN SELECT RAISE(ABORT, 'forced correlation snapshot failure'); END;"
        ))
        .unwrap();
    }
    assert!(
        core.doc_host
            .confirm_task_command_correlation(&task_id, message_id, "synthetic-turn", None, None,)
            .is_err()
    );
    assert_eq!(
        core.doc_host
            .task_command_receipt(command_id)
            .unwrap()
            .unwrap()
            .status,
        CommandReceiptStatus::Pending
    );
    assert!(matches!(
        core.doc_host
            .queued_message(&task_id, message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueMessageStatus::Claimed { .. }
    ));
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_correlation_snapshot")
            .unwrap();
    }
    drop(client);
    drop(handle);
    drop(core);

    let reopened = assemble(root.path(), runs, false);
    assert!(matches!(
        reopened
            .doc_host
            .queued_message(&task_id, message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueMessageStatus::DeliveryUnknown { .. }
    ));
    assert_ne!(
        reopened
            .doc_host
            .task_command_receipt(command_id)
            .unwrap()
            .unwrap()
            .status,
        CommandReceiptStatus::Applied
    );
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_steer_caller_applies_only_for_supported_attachment_free_held_rows() {
    let (supported, supported_row) = steer_queue_case(SteeringMode::StepBoundary, false).await;
    assert_eq!(supported.status, CommandReceiptStatus::Pending);
    assert!(matches!(
        supported_row,
        QueueMessageStatus::SteeringAccepted { .. }
    ));

    let (unsupported_harness, harness_row) =
        steer_queue_case(SteeringMode::TurnBoundary, false).await;
    assert_eq!(unsupported_harness.status, CommandReceiptStatus::Rejected);
    assert_eq!(
        unsupported_harness.reason.unwrap().code,
        CommandReasonCode::UnsupportedOperation
    );
    assert!(matches!(harness_row, QueueMessageStatus::Held));

    let (unsupported_attachment, attachment_row) =
        steer_queue_case(SteeringMode::StepBoundary, true).await;
    assert_eq!(
        unsupported_attachment.status,
        CommandReceiptStatus::Rejected
    );
    assert_eq!(
        unsupported_attachment.reason.unwrap().code,
        CommandReasonCode::UnsupportedOperation
    );
    assert!(matches!(attachment_row, QueueMessageStatus::Held));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_stop_fences_send_now_and_holds_the_queue() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs.clone(), true);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task(&client, &task_id, &core.device_id).await;
    core.workspace.rename_chat(&task_id, "Pre-titled").unwrap();
    let before_run = snapshot(&client, &task_id).await;
    let run = execute(
        &client,
        envelope(
            "stop-race-run",
            &task_id,
            Some(&before_run),
            serde_json::json!({
                "kind":"session",
                "command": {
                    "kind":"run",
                    "request": request("hold active turn"),
                    "messageId":"stop-race-run-message"
                }
            }),
        ),
    )
    .await;
    assert!(matches!(
        run.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));
    wait_for_receipt(&client, "stop-race-run", CommandReceiptStatus::Applied).await;
    let active = snapshot(&client, &task_id).await;
    let turn_id = active.snapshot.active_turn_id.clone().expect("active turn");
    let queued = execute(
        &client,
        envelope(
            "stop-race-enqueue",
            &task_id,
            Some(&active),
            serde_json::json!({
                "kind":"queueMessage",
                "messageId":"stop-race-queued",
                "text":"must remain held",
                "holdForTurnEnd":true
            }),
        ),
    )
    .await;
    assert_eq!(queued.status, CommandReceiptStatus::Applied);

    let after_queue = snapshot(&client, &task_id).await;
    let mut stop = envelope(
        "stop-race-stop",
        &task_id,
        Some(&after_queue),
        serde_json::json!({
            "kind":"session",
            "command":{"kind":"interrupt"}
        }),
    );
    stop["expectedTurnId"] = turn_id.clone().into();
    let stop_receipt = execute(&client, stop).await;
    assert!(matches!(
        stop_receipt.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));

    let mut send_now = envelope(
        "stop-race-send-now",
        &task_id,
        Some(&snapshot(&client, &task_id).await),
        serde_json::json!({
            "kind":"sendQueuedNow",
            "messageId":"stop-race-queued",
            "expectedQueueRevision":1
        }),
    );
    send_now["expectedTurnId"] = turn_id.into();
    let send_receipt = execute(&client, send_now).await;
    assert_eq!(send_receipt.status, CommandReceiptStatus::Conflict);
    tokio::time::timeout(Duration::from_secs(5), async {
        while core.sessions.turn_in_flight(&task_id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let row = core
        .doc_host
        .queued_message(&task_id, "stop-race-queued")
        .unwrap()
        .expect("stop holds the pending queue row");
    assert!(matches!(row.status, luna_doc::QueueMessageStatus::Held));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revision_binding_turn_and_lifecycle_fences_are_visible() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs, true);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task(&client, &task_id, &core.device_id).await;
    core.workspace.rename_chat(&task_id, "Pre-titled").unwrap();
    let before = snapshot(&client, &task_id).await;
    execute(
        &client,
        envelope(
            "held-send",
            &task_id,
            Some(&before),
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": request("hold"),
                    "messageId": "held-message"
                }
            }),
        ),
    )
    .await;
    wait_for_receipt(&client, "held-send", CommandReceiptStatus::Applied).await;
    let active = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let read = snapshot(&client, &task_id).await;
            if read.snapshot.active_turn_id.is_some() && read.snapshot.binding_generation.is_some()
            {
                break read;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let archive = execute(
        &client,
        envelope(
            "archive-active",
            &task_id,
            Some(&active),
            serde_json::json!({ "kind": "setArchived", "archived": true }),
        ),
    )
    .await;
    assert_eq!(archive.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        archive.reason.unwrap().code,
        CommandReasonCode::ActiveTurnConflict
    );

    let mut wrong_stop = envelope(
        "wrong-stop",
        &task_id,
        Some(&snapshot(&client, &task_id).await),
        serde_json::json!({ "kind": "session", "command": { "kind": "interrupt" } }),
    );
    wrong_stop["expectedTurnId"] = "replaced-turn".into();
    let wrong_stop = execute(&client, wrong_stop).await;
    assert_eq!(wrong_stop.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        wrong_stop.reason.unwrap().code,
        CommandReasonCode::StaleTurn
    );
    assert!(core.sessions.has_live_run(&task_id));

    let before_stop = snapshot(&client, &task_id).await;
    let old_turn = before_stop.snapshot.active_turn_id.clone().unwrap();
    let mut stop = envelope(
        "correct-stop",
        &task_id,
        Some(&before_stop),
        serde_json::json!({ "kind": "session", "command": { "kind": "interrupt" } }),
    );
    stop["expectedTurnId"] = old_turn.clone().into();
    execute(&client, stop).await;
    wait_for_receipt(&client, "correct-stop", CommandReceiptStatus::Applied).await;
    assert!(!core.sessions.has_live_run(&task_id));

    let after_stop = snapshot(&client, &task_id).await;
    let mut late_steer = envelope(
        "late-steer",
        &task_id,
        Some(&after_stop),
        serde_json::json!({
            "kind": "session",
            "command": { "kind": "steer", "prompt": "late", "messageId": "late-message" }
        }),
    );
    late_steer["expectedTurnId"] = old_turn.into();
    let late_steer = execute(&client, late_steer).await;
    assert_eq!(late_steer.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        late_steer.reason.unwrap().code,
        CommandReasonCode::StaleTurn
    );

    let current = snapshot(&client, &task_id).await;
    let mut stale_binding = envelope(
        "stale-binding",
        &task_id,
        Some(&current),
        serde_json::json!({ "kind": "rename", "title": "must not apply" }),
    );
    stale_binding["expectedBindingGeneration"] = 0.into();
    let stale_binding = execute(&client, stale_binding).await;
    assert_eq!(stale_binding.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        stale_binding.reason.unwrap().code,
        CommandReasonCode::StaleBindingGeneration
    );

    let revision = snapshot(&client, &task_id).await;
    let rename = execute(
        &client,
        envelope(
            "rename-current",
            &task_id,
            Some(&revision),
            serde_json::json!({ "kind": "rename", "title": "current title" }),
        ),
    )
    .await;
    assert_eq!(rename.status, CommandReceiptStatus::Applied);
    let stale_rename = execute(
        &client,
        envelope(
            "rename-stale",
            &task_id,
            Some(&revision),
            serde_json::json!({ "kind": "rename", "title": "lost update" }),
        ),
    )
    .await;
    assert_eq!(stale_rename.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        stale_rename.reason.unwrap().code,
        CommandReasonCode::StaleTaskRevision
    );
    assert_eq!(
        core.workspace
            .chat(&task_id)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("current title")
    );

    let first_page: TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({
                "taskId": task_id,
                "afterTaskSequence": 0,
                "limit": 2,
            }),
        )
        .await
        .unwrap();
    assert_eq!(first_page.events.len(), 2);
    assert!(first_page.has_more);
    let next = first_page.next_task_sequence.unwrap();
    let second_page: TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({
                "taskId": task_id,
                "afterTaskSequence": next,
                "limit": 2,
            }),
        )
        .await
        .unwrap();
    assert!(
        second_page
            .events
            .iter()
            .all(|event| event.task_sequence > next)
    );

    let gap: TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": task_id, "afterTaskSequence": 999999 }),
        )
        .await
        .unwrap();
    assert!(gap.reload_required);
    assert!(gap.events.is_empty());
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn organization_commands_are_durable_ordered_and_revision_fenced() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_ids = ["org-a", "org-b", "org-c"];
    let core = assemble(root.path(), runs, false);
    let client = luna_rpc::memory_client(core.rpc_service());
    for task_id in task_ids {
        let receipt = execute(
            &client,
            envelope(
                &format!("create-{task_id}"),
                task_id,
                None,
                serde_json::json!({ "kind": "createChat", "deviceId": core.device_id }),
            ),
        )
        .await;
        assert_eq!(receipt.status, CommandReceiptStatus::Applied);
        let read = snapshot(&client, task_id).await;
        let receipt = execute(
            &client,
            envelope(
                &format!("pin-{task_id}"),
                task_id,
                Some(&read),
                serde_json::json!({ "kind": "setPinned", "pinned": true }),
            ),
        )
        .await;
        assert_eq!(receipt.status, CommandReceiptStatus::Applied);
    }

    let before_reorder = snapshot(&client, "org-c").await;
    let reorder = execute(
        &client,
        envelope(
            "reorder-org-c",
            "org-c",
            Some(&before_reorder),
            serde_json::json!({
                "kind": "setPinned",
                "pinned": true,
                "beforeTaskId": "org-a"
            }),
        ),
    )
    .await;
    assert_eq!(reorder.status, CommandReceiptStatus::Applied);
    let duplicate = execute(
        &client,
        envelope(
            "reorder-org-c",
            "org-c",
            Some(&before_reorder),
            serde_json::json!({
                "kind": "setPinned",
                "pinned": true,
                "beforeTaskId": "org-a"
            }),
        ),
    )
    .await;
    assert_eq!(duplicate, reorder);

    let mut pinned = core.workspace.read_chats().unwrap();
    pinned.sort_by(|left, right| {
        luna_proto::compare_pin_order(
            left.pinned_order.as_deref().expect("filtered pin key"),
            right.pinned_order.as_deref().expect("filtered pin key"),
        )
        .then_with(|| left.id.cmp(&right.id))
    });
    assert_eq!(
        pinned
            .iter()
            .map(|chat| chat.id.as_str())
            .collect::<Vec<_>>(),
        ["org-c", "org-a", "org-b"]
    );

    let unread_snapshot = snapshot(&client, "org-a").await;
    let unread = execute(
        &client,
        envelope(
            "org-a-unread",
            "org-a",
            Some(&unread_snapshot),
            serde_json::json!({ "kind": "setRead", "read": false }),
        ),
    )
    .await;
    assert_eq!(unread.status, CommandReceiptStatus::Applied);
    let stale_read = execute(
        &client,
        envelope(
            "org-a-stale-read",
            "org-a",
            Some(&unread_snapshot),
            serde_json::json!({ "kind": "setRead", "read": true }),
        ),
    )
    .await;
    assert_eq!(stale_read.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        stale_read.reason.unwrap().code,
        CommandReasonCode::StaleTaskRevision
    );

    let archive = execute(
        &client,
        envelope(
            "org-a-archive",
            "org-a",
            Some(&snapshot(&client, "org-a").await),
            serde_json::json!({ "kind": "setArchived", "archived": true }),
        ),
    )
    .await;
    assert_eq!(archive.status, CommandReceiptStatus::Applied);
    let archived = core.workspace.chat("org-a").unwrap().unwrap();
    assert!(archived.archived);
    assert_eq!(archived.pinned_order, None, "archive clears active pin");
    assert!(archived.manually_unread);
    let unarchive = execute(
        &client,
        envelope(
            "org-a-unarchive",
            "org-a",
            Some(&snapshot(&client, "org-a").await),
            serde_json::json!({ "kind": "setArchived", "archived": false }),
        ),
    )
    .await;
    assert_eq!(unarchive.status, CommandReceiptStatus::Applied);
    assert_eq!(
        core.workspace.chat("org-a").unwrap().unwrap().pinned_order,
        None,
        "unarchive returns unpinned"
    );

    core.shutdown().await;
    drop(core);
    let reopened = assemble(root.path(), Arc::new(AtomicUsize::new(0)), false);
    let persisted = reopened.workspace.chat("org-a").unwrap().unwrap();
    assert!(!persisted.archived);
    assert!(persisted.manually_unread);
    assert_eq!(persisted.pinned_order, None);
    let mut persisted_pins: Vec<_> = reopened
        .workspace
        .read_chats()
        .unwrap()
        .into_iter()
        .filter(|chat| chat.pinned_order.is_some())
        .collect();
    persisted_pins.sort_by(|left, right| {
        luna_proto::compare_pin_order(
            left.pinned_order.as_deref().unwrap(),
            right.pinned_order.as_deref().unwrap(),
        )
    });
    assert_eq!(
        persisted_pins
            .iter()
            .map(|chat| chat.id.as_str())
            .collect::<Vec<_>>(),
        ["org-c", "org-b"]
    );
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dense_pin_insertion_changes_only_requested_tasks_and_rejects_stale_neighbors() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path(), Arc::new(AtomicUsize::new(0)), false);
    let client = luna_rpc::memory_client(core.rpc_service());
    for task_id in ["pin-left", "pin-right"] {
        create_task(&client, task_id, &core.device_id).await;
        let receipt = execute(
            &client,
            envelope(
                &format!("pin-{task_id}"),
                task_id,
                Some(&snapshot(&client, task_id).await),
                serde_json::json!({ "kind": "setPinned", "pinned": true }),
            ),
        )
        .await;
        assert_eq!(receipt.status, CommandReceiptStatus::Applied);
    }
    let left_revision = snapshot(&client, "pin-left").await.snapshot.task_revision;
    let right_revision = snapshot(&client, "pin-right").await.snapshot.task_revision;
    let left_key = core
        .workspace
        .chat("pin-left")
        .unwrap()
        .unwrap()
        .pinned_order
        .unwrap();
    let right_key = core
        .workspace
        .chat("pin-right")
        .unwrap()
        .unwrap()
        .pinned_order
        .unwrap();

    let mut next = "pin-right".to_string();
    let mut replay = None;
    for index in 0..16 {
        let task_id = format!("pin-dense-{index}");
        create_task(&client, &task_id, &core.device_id).await;
        let command_id = format!("pin-dense-command-{index}");
        let read = snapshot(&client, &task_id).await;
        let receipt = execute(
            &client,
            envelope(
                &command_id,
                &task_id,
                Some(&read),
                serde_json::json!({
                    "kind": "setPinned",
                    "pinned": true,
                    "afterTaskId": "pin-left",
                    "beforeTaskId": next,
                }),
            ),
        )
        .await;
        assert_eq!(receipt.status, CommandReceiptStatus::Applied);
        if index == 15 {
            let duplicate = execute(
                &client,
                envelope(
                    &command_id,
                    &task_id,
                    Some(&read),
                    serde_json::json!({
                        "kind": "setPinned",
                        "pinned": true,
                        "afterTaskId": "pin-left",
                        "beforeTaskId": next,
                    }),
                ),
            )
            .await;
            assert_eq!(duplicate, receipt);
            replay = Some(task_id.clone());
        }
        next = task_id;
    }

    assert_eq!(
        snapshot(&client, "pin-left").await.snapshot.task_revision,
        left_revision,
        "inserting between pins must not revise the left neighbor"
    );
    assert_eq!(
        snapshot(&client, "pin-right").await.snapshot.task_revision,
        right_revision,
        "inserting between pins must not revise the right neighbor"
    );
    assert_eq!(
        core.workspace
            .chat("pin-left")
            .unwrap()
            .unwrap()
            .pinned_order
            .as_deref(),
        Some(left_key.as_str())
    );
    assert_eq!(
        core.workspace
            .chat("pin-right")
            .unwrap()
            .unwrap()
            .pinned_order
            .as_deref(),
        Some(right_key.as_str())
    );
    let replay_key = core
        .workspace
        .chat(replay.as_deref().unwrap())
        .unwrap()
        .unwrap()
        .pinned_order
        .unwrap();
    assert!(replay_key.len() > 10, "dense insertions extend the key");

    create_task(&client, "pin-stale", &core.device_id).await;
    let stale_read = snapshot(&client, "pin-stale").await;
    create_task(&client, "pin-neighbor", &core.device_id).await;
    let changed = execute(
        &client,
        envelope(
            "pin-new-neighbor",
            "pin-neighbor",
            Some(&snapshot(&client, "pin-neighbor").await),
            serde_json::json!({
                "kind": "setPinned",
                "pinned": true,
                "afterTaskId": "pin-left",
                "beforeTaskId": next,
            }),
        ),
    )
    .await;
    assert_eq!(changed.status, CommandReceiptStatus::Applied);
    let stale = execute(
        &client,
        envelope(
            "pin-stale-neighbors",
            "pin-stale",
            Some(&stale_read),
            serde_json::json!({
                "kind": "setPinned",
                "pinned": true,
                "afterTaskId": "pin-left",
                "beforeTaskId": next,
            }),
        ),
    )
    .await;
    assert_eq!(stale.status, CommandReceiptStatus::Failed);
    assert!(
        core.workspace
            .chat("pin-stale")
            .unwrap()
            .unwrap()
            .pinned_order
            .is_none()
    );
    core.shutdown().await;
}

fn init_git_repo(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    let status = std::process::Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg(path)
        .status()
        .expect("run git init");
    assert!(status.success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_workspace_project_move_is_atomic_and_preserves_task_state() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let nested = repo.join("packages/app");
    init_git_repo(&repo);
    std::fs::create_dir_all(&nested).unwrap();
    let other = root.path().join("other");
    init_git_repo(&other);

    let core = assemble(root.path(), Arc::new(AtomicUsize::new(0)), false);
    core.workspace
        .create_space(
            "source-project",
            &core.device_id,
            nested.to_str().unwrap(),
            Some("App".into()),
            true,
        )
        .unwrap();
    core.workspace
        .create_space(
            "root-project",
            &core.device_id,
            repo.to_str().unwrap(),
            Some("Repository".into()),
            true,
        )
        .unwrap();
    core.workspace
        .create_space(
            "other-project",
            &core.device_id,
            other.to_str().unwrap(),
            Some("Other".into()),
            true,
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task_in_space(&client, "move-task", "source-project", &nested).await;
    core.workspace
        .rename_chat("move-task", "Keep this task")
        .unwrap();
    core.workspace.set_chat_harness_session(
        "move-task",
        "provider-session-1",
        nested.to_str().unwrap(),
    );

    let pin = execute(
        &client,
        envelope(
            "move-pin",
            "move-task",
            Some(&snapshot(&client, "move-task").await),
            serde_json::json!({ "kind": "setPinned", "pinned": true }),
        ),
    )
    .await;
    assert_eq!(pin.status, CommandReceiptStatus::Applied);
    let unread = execute(
        &client,
        envelope(
            "move-unread",
            "move-task",
            Some(&snapshot(&client, "move-task").await),
            serde_json::json!({ "kind": "setRead", "read": false }),
        ),
    )
    .await;
    assert_eq!(unread.status, CommandReceiptStatus::Applied);
    let before = core.workspace.chat("move-task").unwrap().unwrap();
    let binding_before = core
        .task_identity
        .view("move-task")
        .and_then(|view| view.current_binding);

    let move_receipt = execute(
        &client,
        envelope(
            "move-same-workspace",
            "move-task",
            Some(&snapshot(&client, "move-task").await),
            serde_json::json!({
                "kind": "moveProject",
                "destinationSpaceId": "root-project"
            }),
        ),
    )
    .await;
    assert_eq!(move_receipt.status, CommandReceiptStatus::Applied);
    let after = core.workspace.chat("move-task").unwrap().unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(after.title, before.title);
    assert_eq!(after.cwd, before.cwd);
    assert_eq!(after.config, before.config);
    assert_eq!(after.pinned_order, before.pinned_order);
    assert_eq!(after.manually_unread, before.manually_unread);
    assert_eq!(after.harness_session_id, before.harness_session_id);
    assert_eq!(after.harness_session_cwd, before.harness_session_cwd);
    assert_eq!(after.created_at, before.created_at);
    assert_eq!(after.space_id.as_deref(), Some("root-project"));
    assert_eq!(
        core.task_identity
            .view("move-task")
            .and_then(|view| view.current_binding),
        binding_before,
        "binding/provider continuity is preserved"
    );

    let rejected = execute(
        &client,
        envelope(
            "move-cross-workspace",
            "move-task",
            Some(&snapshot(&client, "move-task").await),
            serde_json::json!({
                "kind": "moveProject",
                "destinationSpaceId": "other-project"
            }),
        ),
    )
    .await;
    assert_eq!(rejected.status, CommandReceiptStatus::Rejected);
    assert_eq!(
        rejected.reason.unwrap().code,
        CommandReasonCode::IncompatibleWorkspace
    );
    assert_eq!(
        core.workspace
            .chat("move-task")
            .unwrap()
            .unwrap()
            .space_id
            .as_deref(),
        Some("root-project"),
        "failed move leaves original ownership untouched"
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exact_non_git_workspace_move_succeeds_and_different_root_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("notes");
    let other = root.path().join("other-notes");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let core = assemble(root.path(), Arc::new(AtomicUsize::new(0)), false);
    core.workspace
        .create_space(
            "notes-project",
            &core.device_id,
            workspace.to_str().unwrap(),
            Some("Notes".into()),
            false,
        )
        .unwrap();
    core.workspace
        .create_space(
            "other-project",
            &core.device_id,
            other.to_str().unwrap(),
            Some("Other".into()),
            false,
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    let created = execute(
        &client,
        envelope(
            "create-personal-notes",
            "non-git-task",
            None,
            serde_json::json!({
                "kind": "createChat",
                "deviceId": core.device_id,
                "cwd": workspace,
                "config": task_config("synthetic")
            }),
        ),
    )
    .await;
    assert_eq!(created.status, CommandReceiptStatus::Applied);
    let moved = execute(
        &client,
        envelope(
            "move-non-git-same-root",
            "non-git-task",
            Some(&snapshot(&client, "non-git-task").await),
            serde_json::json!({
                "kind": "moveProject",
                "destinationSpaceId": "notes-project"
            }),
        ),
    )
    .await;
    assert_eq!(moved.status, CommandReceiptStatus::Applied);
    assert_eq!(
        core.workspace
            .chat("non-git-task")
            .unwrap()
            .unwrap()
            .space_id
            .as_deref(),
        Some("notes-project")
    );

    let rejected = execute(
        &client,
        envelope(
            "move-non-git-other-root",
            "non-git-task",
            Some(&snapshot(&client, "non-git-task").await),
            serde_json::json!({
                "kind": "moveProject",
                "destinationSpaceId": "other-project"
            }),
        ),
    )
    .await;
    assert_eq!(rejected.status, CommandReceiptStatus::Rejected);
    assert_eq!(
        rejected.reason.unwrap().code,
        CommandReasonCode::IncompatibleWorkspace
    );
    assert_eq!(
        core.workspace
            .chat("non-git-task")
            .unwrap()
            .unwrap()
            .space_id
            .as_deref(),
        Some("notes-project")
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn active_task_cannot_move_under_execution() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let nested = repo.join("nested");
    init_git_repo(&repo);
    std::fs::create_dir_all(&nested).unwrap();
    let core = assemble(root.path(), Arc::new(AtomicUsize::new(0)), true);
    core.workspace
        .create_space(
            "source",
            &core.device_id,
            nested.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    core.workspace
        .create_space(
            "destination",
            &core.device_id,
            repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task_in_space(&client, "active-move", "source", &nested).await;
    let run = execute(
        &client,
        envelope(
            "active-move-run",
            "active-move",
            Some(&snapshot(&client, "active-move").await),
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": request("hold"),
                    "messageId": "active-move-message"
                }
            }),
        ),
    )
    .await;
    assert!(matches!(
        run.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));
    wait_for_receipt(&client, "active-move-run", CommandReceiptStatus::Applied).await;
    let active = snapshot(&client, "active-move").await;
    let move_receipt = execute(
        &client,
        envelope(
            "active-move-command",
            "active-move",
            Some(&active),
            serde_json::json!({
                "kind": "moveProject",
                "destinationSpaceId": "destination"
            }),
        ),
    )
    .await;
    assert_eq!(move_receipt.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        move_receipt.reason.unwrap().code,
        CommandReasonCode::ActiveTurnConflict
    );
    assert_eq!(
        core.workspace
            .chat("active-move")
            .unwrap()
            .unwrap()
            .space_id
            .as_deref(),
        Some("source")
    );
    let turn = active.snapshot.active_turn_id.unwrap();
    let mut stop = envelope(
        "active-move-stop",
        "active-move",
        Some(&snapshot(&client, "active-move").await),
        serde_json::json!({ "kind": "session", "command": { "kind": "interrupt" } }),
    );
    stop["expectedTurnId"] = turn.into();
    execute(&client, stop).await;
    wait_for_receipt(&client, "active-move-stop", CommandReceiptStatus::Applied).await;
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clear_context_atomically_advances_binding_and_retains_local_state() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let compactions = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble_context(root.path(), runs.clone(), compactions, false, false);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_configured_task(&client, &task_id, &core.device_id).await;
    core.doc_host.open(&task_id).unwrap();
    run_once(&client, &task_id, "clear-context-run", "remember this").await;
    wait_for_no_live_run(&core, &task_id).await;
    core.doc_host.pause_all_queues();

    let before_binding = core
        .task_identity
        .view(&task_id)
        .unwrap()
        .current_binding
        .unwrap();
    let before_entries = core
        .doc_host
        .open(&task_id)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert!(!before_entries.is_empty());
    core.doc_host
        .open(&task_id)
        .unwrap()
        .doc()
        .update_context_usage(Some(321), Some(1_000))
        .unwrap();

    let draft = composer_draft(&task_id, "draft stays local");
    client
        .call(
            methods::SAVE_COMPOSER_DRAFT,
            serde_json::json!({ "draft": draft }),
        )
        .await
        .unwrap();
    let queue = execute(
        &client,
        envelope(
            "clear-context-queue",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "queueMessage",
                "messageId": "clear-context-row",
                "text": "queued content stays",
                "attachments": ["/tmp/synthetic-attachment.txt"],
                "holdForTurnEnd": true,
                "execution": task_config("synthetic")
            }),
        ),
    )
    .await;
    assert_eq!(queue.status, CommandReceiptStatus::Applied);

    let clear = envelope(
        "clear-context",
        &task_id,
        Some(&snapshot(&client, &task_id).await),
        serde_json::json!({ "kind": "clearContext" }),
    );
    let db_path = root.path().join("data/orgs/l07-org/l07-user/docs.sqlite3");
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_context_boundary BEFORE INSERT ON task_command_events
             WHEN NEW.command_id='clear-context'
             BEGIN SELECT RAISE(ABORT, 'forced context boundary failure'); END;",
        )
        .unwrap();
    }
    assert!(
        client
            .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, clear.clone())
            .await
            .is_err()
    );
    assert_eq!(
        core.task_identity
            .view(&task_id)
            .unwrap()
            .current_binding
            .unwrap()
            .binding_id,
        before_binding.binding_id
    );
    assert!(
        core.doc_host
            .context_control(&task_id)
            .unwrap()
            .last_boundary
            .is_none()
    );
    assert_eq!(
        core.doc_host
            .open(&task_id)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
            .len(),
        before_entries.len()
    );
    assert_eq!(
        core.workspace
            .chat(&task_id)
            .unwrap()
            .unwrap()
            .harness_session_id
            .as_deref(),
        Some("synthetic-provider-session")
    );
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_context_boundary")
            .unwrap();
    }
    let applied = execute(&client, clear.clone()).await;
    assert_eq!(applied.status, CommandReceiptStatus::Applied);
    assert_eq!(execute(&client, clear).await, applied);

    let after_binding = core
        .task_identity
        .view(&task_id)
        .unwrap()
        .current_binding
        .unwrap();
    assert_eq!(after_binding.generation, before_binding.generation + 1);
    assert_eq!(after_binding.origin, BindingOrigin::ContextReset);
    assert!(after_binding.provider_task_id.is_empty());
    let chat = core.workspace.chat(&task_id).unwrap().unwrap();
    assert_eq!(chat.config.unwrap().harness, HarnessId::Mock);
    assert_eq!(chat.harness_session_id.as_deref(), Some(""));
    let handle = core.doc_host.open(&task_id).unwrap();
    assert!(handle.doc().context_usage().is_none());
    let state = handle.doc().context_control();
    assert_eq!(state.binding_generation, Some(after_binding.generation));
    assert_eq!(
        state.last_boundary.as_ref().map(|boundary| boundary.kind),
        Some(luna_proto::ContextBoundaryKind::Cleared)
    );
    let entries = handle.doc().read_entries().unwrap();
    assert!(entries.len() > before_entries.len());
    assert!(entries.iter().any(|entry| {
        entry.role == MessageRole::System && entry.id == "context-boundary:clear-context"
    }));
    let row = core
        .doc_host
        .queued_message(&task_id, "clear-context-row")
        .unwrap()
        .unwrap();
    assert_eq!(row.text, "queued content stays");
    assert_eq!(row.attachments, vec!["/tmp/synthetic-attachment.txt"]);
    assert_eq!(row.hold_reason, Some(QueueHoldReason::ContextChanged));
    assert!(matches!(
        row.delivery_gate,
        Some(QueueDeliveryGate::ReviewRequired { .. })
    ));
    let draft_read: ComposerDraftRead = client
        .call_as(
            methods::GET_COMPOSER_DRAFT,
            serde_json::json!({ "taskId": task_id }),
        )
        .await
        .unwrap();
    assert_eq!(
        draft_read.draft.as_ref().map(|draft| draft.text.as_str()),
        Some("draft stays local")
    );

    core.doc_host.flush_all();
    core.shutdown().await;
    drop(client);
    drop(core);
    let reopened = assemble_context(
        root.path(),
        runs,
        Arc::new(AtomicUsize::new(0)),
        false,
        false,
    );
    let reopened_binding = reopened
        .task_identity
        .view(&task_id)
        .unwrap()
        .current_binding
        .unwrap();
    assert_eq!(reopened_binding.binding_id, after_binding.binding_id);
    assert!(reopened_binding.provider_task_id.is_empty());
    assert_eq!(
        reopened
            .doc_host
            .open(&task_id)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
            .len(),
        entries.len()
    );
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_switch_creates_fresh_binding_and_holds_incompatible_queue() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble_context(
        root.path(),
        runs,
        Arc::new(AtomicUsize::new(0)),
        false,
        false,
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    create_configured_task(&client, &task_id, &core.device_id).await;
    core.doc_host.open(&task_id).unwrap();
    run_once(
        &client,
        &task_id,
        "switch-provider-run",
        "old provider history",
    )
    .await;
    wait_for_no_live_run(&core, &task_id).await;
    core.doc_host.pause_all_queues();
    let before = core
        .task_identity
        .view(&task_id)
        .unwrap()
        .current_binding
        .unwrap();

    let queued = execute(
        &client,
        envelope(
            "switch-provider-queue",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "queueMessage",
                "messageId": "switch-provider-row",
                "text": "review me before delivery",
                "attachments": ["/tmp/retained.png"],
                "holdForTurnEnd": true,
                "execution": task_config("synthetic")
            }),
        ),
    )
    .await;
    assert_eq!(queued.status, CommandReceiptStatus::Applied);

    let mut target = task_config("gpt-5.1-codex");
    target.harness = HarnessId::Codex;
    let requested_handoff = ContextHandoff {
        kind: ContextHandoffKind::RecentVisibleHistory,
        text: "invented hidden context must not cross the boundary".into(),
        message_ids: vec!["switch-provider-run-message".into()],
    };
    let expected_handoff = ContextHandoff {
        kind: ContextHandoffKind::RecentVisibleHistory,
        text: "User: old provider history".into(),
        message_ids: vec!["switch-provider-run-message".into()],
    };
    let switched = execute(
        &client,
        envelope(
            "switch-provider",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "switchProvider",
                "config": target,
                "handoff": requested_handoff
            }),
        ),
    )
    .await;
    assert_eq!(switched.status, CommandReceiptStatus::Applied);

    let after = core
        .task_identity
        .view(&task_id)
        .unwrap()
        .current_binding
        .unwrap();
    assert_eq!(after.generation, before.generation + 1);
    assert_eq!(after.origin, BindingOrigin::ProviderSwitch);
    assert_eq!(after.provider_id, HarnessId::Codex);
    assert!(after.provider_task_id.is_empty());
    let chat = core.workspace.chat(&task_id).unwrap().unwrap();
    assert_eq!(chat.config.unwrap().harness, HarnessId::Codex);
    assert_eq!(chat.harness_session_id.as_deref(), Some(""));
    let state = core.doc_host.context_control(&task_id).unwrap();
    assert_eq!(state.pending_handoff.as_ref(), Some(&expected_handoff));
    assert_eq!(state.last_handoff.as_ref(), Some(&expected_handoff));
    assert_eq!(
        state.last_boundary.as_ref().map(|boundary| boundary.kind),
        Some(luna_proto::ContextBoundaryKind::ProviderSwitched)
    );
    let row = core
        .doc_host
        .queued_message(&task_id, "switch-provider-row")
        .unwrap()
        .unwrap();
    assert_eq!(row.attachments, vec!["/tmp/retained.png"]);
    assert_eq!(row.hold_reason, Some(QueueHoldReason::ContextChanged));
    assert!(matches!(
        row.delivery_gate,
        Some(QueueDeliveryGate::ReviewRequired { .. })
    ));
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compact_is_idempotent_and_restart_unknown_is_never_replayed() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let compactions = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble_context(root.path(), runs.clone(), compactions.clone(), false, false);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_configured_task(&client, &task_id, &core.device_id).await;
    run_once(&client, &task_id, "compact-run", "fill provider context").await;
    wait_for_no_live_run(&core, &task_id).await;

    let compact = envelope(
        "compact-once",
        &task_id,
        Some(&snapshot(&client, &task_id).await),
        serde_json::json!({ "kind": "compactContext" }),
    );
    let applied = execute(&client, compact.clone()).await;
    assert_eq!(applied.status, CommandReceiptStatus::Applied);
    assert_eq!(execute(&client, compact).await, applied);
    assert_eq!(compactions.load(Ordering::SeqCst), 1);
    assert_eq!(
        core.doc_host
            .context_control(&task_id)
            .unwrap()
            .compact
            .status,
        ContextOperationStatus::Completed
    );

    let binding_generation = core
        .task_identity
        .view(&task_id)
        .unwrap()
        .current_binding
        .unwrap()
        .generation;
    let mut automatic = ContextControlState::default();
    automatic.binding_generation = Some(binding_generation);
    automatic.compact.operation_id =
        Some("codex-auto:synthetic-thread:synthetic-turn:compact-item".into());
    automatic.compact.status = ContextOperationStatus::Running;
    automatic.compact.binding_generation = Some(binding_generation);
    automatic.compact.started_at_ms = Some(1);
    let handle = core.doc_host.open(&task_id).unwrap();
    handle
        .doc()
        .update_context_usage(Some(900), Some(1_000))
        .unwrap();
    core.doc_host
        .persist_provider_context_control(
            &task_id,
            "provider-context:automatic-running",
            &automatic,
            false,
        )
        .unwrap();
    assert_eq!(handle.doc().context_usage().unwrap().tokens, Some(900));
    let mut completed = automatic.clone();
    completed.compact.status = ContextOperationStatus::Completed;
    completed.compact.finished_at_ms = Some(2);
    core.doc_host
        .persist_provider_context_control(
            &task_id,
            "provider-context:automatic-completed",
            &completed,
            true,
        )
        .unwrap();
    assert!(handle.doc().context_usage().is_none());
    assert!(
        core.doc_host
            .task_command_receipt("provider-context:automatic-running")
            .unwrap()
            .is_none(),
        "unsolicited provider lifecycle must not manufacture a manual command receipt"
    );
    assert!(
        core.doc_host
            .task_command_receipt("provider-context:automatic-completed")
            .unwrap()
            .is_none()
    );
    let mut interrupted = completed;
    interrupted.compact.operation_id =
        Some("codex-auto:synthetic-thread:next-turn:interrupted-item".into());
    interrupted.compact.status = ContextOperationStatus::Running;
    interrupted.compact.started_at_ms = Some(3);
    interrupted.compact.finished_at_ms = None;
    core.doc_host
        .persist_provider_context_control(
            &task_id,
            "provider-context:automatic-interrupted",
            &interrupted,
            false,
        )
        .unwrap();
    core.doc_host
        .queue_message_with_id_and_execution(
            &task_id,
            "automatic-compact-queue",
            "must remain held",
            Vec::new(),
            true,
            FollowUpBehavior::Queue,
            None,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        core.doc_host
            .queued_message(&task_id, "automatic-compact-queue")
            .unwrap()
            .unwrap()
            .status,
        QueueMessageStatus::Held
    ));
    core.doc_host.flush_all();
    core.shutdown().await;
    drop(client);
    drop(core);

    let reopened = assemble_context(root.path(), runs, compactions.clone(), false, false);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let restarted_state = reopened.doc_host.context_control(&task_id).unwrap();
    assert_eq!(
        restarted_state.compact.status,
        ContextOperationStatus::Unknown
    );
    assert!(
        restarted_state
            .compact
            .detail
            .as_deref()
            .unwrap()
            .contains("will not replay")
    );
    let retry = envelope(
        "compact-after-restart",
        &task_id,
        Some(&snapshot(&client, &task_id).await),
        serde_json::json!({ "kind": "compactContext" }),
    );
    let rejected = execute(&client, retry.clone()).await;
    assert_eq!(rejected.status, CommandReceiptStatus::Rejected);
    assert_eq!(
        rejected.reason.as_ref().map(|reason| reason.code),
        Some(CommandReasonCode::ProviderAcceptanceUnknown)
    );
    assert_eq!(execute(&client, retry).await, rejected);
    assert_eq!(compactions.load(Ordering::SeqCst), 1);
    let state = reopened.doc_host.context_control(&task_id).unwrap();
    assert_eq!(state.compact.status, ContextOperationStatus::Unknown);
    assert!(state.compact.detail.unwrap().contains("will not replay"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        reopened
            .doc_host
            .queued_message(&task_id, "automatic-compact-queue")
            .unwrap()
            .unwrap()
            .status,
        QueueMessageStatus::Held
    ));
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn automatic_codex_context_events_gate_real_queue_drain_and_restart() {
    let completed_root = tempfile::tempdir().unwrap();
    let completed_runs = Arc::new(AtomicUsize::new(0));
    let completed_context = Arc::new(Notify::new());
    let completed_done = Arc::new(Notify::new());
    let completed_task = uuid::Uuid::new_v4().to_string();
    let completed_core = assemble_codex_context(
        completed_root.path(),
        completed_runs.clone(),
        completed_context.clone(),
        completed_done.clone(),
        CodexContextOutcome::Completed,
    );
    let completed_client = luna_rpc::memory_client(completed_core.rpc_service());
    create_codex_context_task(
        &completed_client,
        &completed_task,
        &completed_core.device_id,
    )
    .await;
    start_codex_context_run(&completed_client, &completed_task).await;
    wait_for_context_status(
        &completed_core,
        &completed_task,
        ContextOperationStatus::Running,
    )
    .await;
    completed_core
        .doc_host
        .queue_message_with_id_and_execution(
            &completed_task,
            "eligible-after-completed",
            "dispatch only after the real turn ends",
            Vec::new(),
            true,
            FollowUpBehavior::Queue,
            None,
        )
        .unwrap();
    completed_context.notify_one();
    wait_for_context_status(
        &completed_core,
        &completed_task,
        ContextOperationStatus::Completed,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(completed_runs.load(Ordering::SeqCst), 1);
    assert!(matches!(
        completed_core
            .doc_host
            .queued_message(&completed_task, "eligible-after-completed")
            .unwrap()
            .unwrap()
            .status,
        QueueMessageStatus::Held
    ));
    let completed_operation = "codex-auto:synthetic-codex-thread:synthetic-turn:synthetic-compact";
    for status in ["Running", "Completed"] {
        assert!(
            completed_core
                .doc_host
                .task_command_receipt(&format!("provider-context:{completed_operation}:{status}"))
                .unwrap()
                .is_none(),
            "automatic provider lifecycle must not create a manual receipt"
        );
    }
    completed_done.notify_one();
    wait_for_run_count(&completed_runs, 2).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while completed_core
            .doc_host
            .queued_message(&completed_task, "eligible-after-completed")
            .unwrap()
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    wait_for_no_live_run(&completed_core, &completed_task).await;
    completed_core.shutdown().await;

    let unknown_root = tempfile::tempdir().unwrap();
    let unknown_runs = Arc::new(AtomicUsize::new(0));
    let unknown_context = Arc::new(Notify::new());
    let unknown_done = Arc::new(Notify::new());
    let unknown_task = uuid::Uuid::new_v4().to_string();
    let unknown_core = assemble_codex_context(
        unknown_root.path(),
        unknown_runs.clone(),
        unknown_context.clone(),
        unknown_done.clone(),
        CodexContextOutcome::Missing,
    );
    let unknown_client = luna_rpc::memory_client(unknown_core.rpc_service());
    create_codex_context_task(&unknown_client, &unknown_task, &unknown_core.device_id).await;
    start_codex_context_run(&unknown_client, &unknown_task).await;
    wait_for_context_status(
        &unknown_core,
        &unknown_task,
        ContextOperationStatus::Running,
    )
    .await;
    unknown_core
        .doc_host
        .queue_message_with_id_and_execution(
            &unknown_task,
            "eligible-after-unknown",
            "must remain held without replay",
            Vec::new(),
            true,
            FollowUpBehavior::Queue,
            None,
        )
        .unwrap();
    unknown_context.notify_one();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        unknown_core
            .doc_host
            .context_control(&unknown_task)
            .unwrap()
            .compact
            .status,
        ContextOperationStatus::Running,
        "missing provider completion must remain Running until the real terminal boundary"
    );
    unknown_done.notify_one();
    wait_for_context_status(
        &unknown_core,
        &unknown_task,
        ContextOperationStatus::Unknown,
    )
    .await;
    wait_for_no_live_run(&unknown_core, &unknown_task).await;
    assert_eq!(unknown_runs.load(Ordering::SeqCst), 1);
    assert!(matches!(
        unknown_core
            .doc_host
            .queued_message(&unknown_task, "eligible-after-unknown")
            .unwrap()
            .unwrap()
            .status,
        QueueMessageStatus::Held
    ));
    for suffix in ["Running", "Unknown"] {
        assert!(
            unknown_core
                .doc_host
                .task_command_receipt(&format!("provider-context:{completed_operation}:{suffix}"))
                .unwrap()
                .is_none()
        );
    }
    unknown_core.doc_host.flush_all();
    unknown_core.shutdown().await;
    drop(unknown_client);
    drop(unknown_core);

    let reopened = assemble_codex_context(
        unknown_root.path(),
        unknown_runs.clone(),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        CodexContextOutcome::Missing,
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        reopened
            .doc_host
            .context_control(&unknown_task)
            .unwrap()
            .compact
            .status,
        ContextOperationStatus::Unknown
    );
    assert!(matches!(
        reopened
            .doc_host
            .queued_message(&unknown_task, "eligible-after-unknown")
            .unwrap()
            .unwrap()
            .status,
        QueueMessageStatus::Held
    ));
    assert_eq!(unknown_runs.load(Ordering::SeqCst), 1);
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_change_waits_for_safe_turn_and_keeps_binding_generation() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble_context(
        root.path(),
        runs,
        Arc::new(AtomicUsize::new(0)),
        true,
        false,
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    create_configured_task(&client, &task_id, &core.device_id).await;
    let before_first_turn = execute(
        &client,
        envelope(
            "model-change-before-first-turn",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "setModel",
                "config": task_config("synthetic-prestart")
            }),
        ),
    )
    .await;
    assert_eq!(before_first_turn.status, CommandReceiptStatus::Applied);
    assert!(
        core.task_identity
            .view(&task_id)
            .unwrap()
            .current_binding
            .is_none()
    );
    assert_eq!(
        core.workspace
            .chat(&task_id)
            .unwrap()
            .unwrap()
            .config
            .unwrap()
            .model
            .as_deref(),
        Some("synthetic-prestart")
    );
    let run = execute(
        &client,
        envelope(
            "model-change-run",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": request("hold the active turn"),
                    "messageId": "model-change-run-message"
                }
            }),
        ),
    )
    .await;
    assert_eq!(run.status, CommandReceiptStatus::Pending);
    // Binding publication precedes the applied receipt and its final task
    // revision. Wait for admission to settle before testing the active-turn
    // fence; otherwise a legitimate stale-revision conflict wins the race.
    wait_for_receipt(&client, "model-change-run", CommandReceiptStatus::Applied).await;
    let active = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = snapshot(&client, &task_id).await;
            if current.snapshot.active_turn_id.is_some()
                && current.snapshot.binding_generation.is_some()
            {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let generation = active.snapshot.binding_generation.unwrap();
    let conflict = execute(
        &client,
        envelope(
            "model-change-active",
            &task_id,
            Some(&active),
            serde_json::json!({
                "kind": "setModel",
                "config": task_config("synthetic-next")
            }),
        ),
    )
    .await;
    assert_eq!(conflict.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        conflict.reason.unwrap().code,
        CommandReasonCode::ActiveTurnConflict
    );

    let before_stop = snapshot(&client, &task_id).await;
    let mut stop = envelope(
        "model-change-stop",
        &task_id,
        Some(&before_stop),
        serde_json::json!({ "kind": "session", "command": { "kind": "interrupt" } }),
    );
    stop["expectedTurnId"] = before_stop.snapshot.active_turn_id.unwrap().into();
    execute(&client, stop).await;
    wait_for_receipt(&client, "model-change-stop", CommandReceiptStatus::Applied).await;

    let mut requested_model = task_config("synthetic-next");
    requested_model.permission_mode = PermissionMode::Full;
    requested_model.provider_account_id = Some("must-not-change-through-set-model".into());
    let changed = execute(
        &client,
        envelope(
            "model-change-safe",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "setModel",
                "config": requested_model
            }),
        ),
    )
    .await;
    assert_eq!(changed.status, CommandReceiptStatus::Applied);
    let after = core
        .task_identity
        .view(&task_id)
        .unwrap()
        .current_binding
        .unwrap();
    assert_eq!(after.generation, generation);
    let config = core
        .workspace
        .chat(&task_id)
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    assert_eq!(config.model.as_deref(), Some("synthetic-next"));
    assert_eq!(config.permission_mode, PermissionMode::Ask);
    assert!(config.provider_account_id.is_none());
    core.shutdown().await;
}

#[tokio::test]
async fn staged_history_fork_persists_acknowledged_id_and_replay_never_mutates_twice() {
    let dir = tempfile::tempdir().unwrap();
    let forks = Arc::new(Mutex::new(Vec::new()));
    let source_task_id = uuid::Uuid::new_v4().to_string();
    let target_task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble_history_with_behavior(
        dir.path(),
        forks.clone(),
        HarnessId::Opencode,
        HistoryForkBehavior::FailAfterAcknowledgement,
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    create_materialized_history_task(&core, &client, &source_task_id, HarnessId::Opencode).await;
    let fork_snapshot = snapshot(&client, &source_task_id).await;
    let command = envelope(
        "history-staged-ack-failure",
        &source_task_id,
        Some(&fork_snapshot),
        serde_json::json!({
            "kind": "forkTask",
            "newTaskId": target_task_id.clone(),
            "boundary": "tip",
            "sourceLink": format!("luna://open/chat/{source_task_id}?workspace=fixture"),
            "review": history_review(&core, &fork_snapshot, &source_task_id, None, None),
        }),
    );
    let first = execute(&client, command.clone()).await;
    assert_eq!(first.status, CommandReceiptStatus::Pending);
    assert_eq!(
        first.reason.as_ref().map(|reason| reason.code),
        Some(CommandReasonCode::ProviderAcceptanceUnknown)
    );
    assert_eq!(
        first.correlation_id.as_deref(),
        Some("synthetic-history-fork-1")
    );
    assert_eq!(forks.lock().unwrap().len(), 1);
    assert!(core.task_identity.view(&target_task_id).is_none());
    assert!(core.workspace.chat(&target_task_id).unwrap().is_none());
    core.shutdown().await;
    drop(client);
    drop(core);

    let reopened = assemble_history_with_behavior(
        dir.path(),
        forks.clone(),
        HarnessId::Opencode,
        HistoryForkBehavior::FailAfterAcknowledgement,
    );
    let reopened_client = luna_rpc::memory_client(reopened.rpc_service());
    let replay = execute(&reopened_client, command).await;
    assert_eq!(replay.status, CommandReceiptStatus::Pending);
    assert_eq!(
        replay.correlation_id.as_deref(),
        Some("synthetic-history-fork-1")
    );
    assert_eq!(forks.lock().unwrap().len(), 1);
    reopened.shutdown().await;

    let preflight_dir = tempfile::tempdir().unwrap();
    let preflight_forks = Arc::new(Mutex::new(Vec::new()));
    let preflight_core = assemble_history_with_behavior(
        preflight_dir.path(),
        preflight_forks.clone(),
        HarnessId::Opencode,
        HistoryForkBehavior::FailPreflight,
    );
    let preflight_client = luna_rpc::memory_client(preflight_core.rpc_service());
    let preflight_source = uuid::Uuid::new_v4().to_string();
    create_materialized_history_task(
        &preflight_core,
        &preflight_client,
        &preflight_source,
        HarnessId::Opencode,
    )
    .await;
    let preflight_snapshot = snapshot(&preflight_client, &preflight_source).await;
    let failed = execute(
        &preflight_client,
        envelope(
            "history-staged-preflight-failure",
            &preflight_source,
            Some(&preflight_snapshot),
            serde_json::json!({
                "kind": "forkTask",
                "newTaskId": uuid::Uuid::new_v4().to_string(),
                "boundary": "tip",
                "sourceLink": format!("luna://open/chat/{preflight_source}?workspace=fixture"),
                "review": history_review(
                    &preflight_core,
                    &preflight_snapshot,
                    &preflight_source,
                    None,
                    None,
                ),
            }),
        ),
    )
    .await;
    assert_eq!(failed.status, CommandReceiptStatus::Failed);
    assert!(failed.correlation_id.is_none());
    assert!(preflight_forks.lock().unwrap().is_empty());
    preflight_core.shutdown().await;
}

#[tokio::test]
async fn fork_initial_prompt_grant_commits_with_its_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let forks = Arc::new(Mutex::new(Vec::new()));
    let core = assemble_history(dir.path(), forks, HarnessId::Opencode);
    let client = luna_rpc::memory_client(core.rpc_service());
    let source_id = uuid::Uuid::new_v4().to_string();
    create_materialized_history_task(&core, &client, &source_id, HarnessId::Opencode).await;
    core.doc_host.flush_all();

    let failed_target = uuid::Uuid::new_v4().to_string();
    let db_path = dir.path().join("data/orgs/l13-org/l13-user/docs.sqlite3");
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(&format!(
            "CREATE TRIGGER fail_fork_prompt_snapshot BEFORE INSERT ON snapshots
             WHEN NEW.doc_id='{failed_target}'
             BEGIN SELECT RAISE(ABORT, 'forced fork prompt snapshot failure'); END;"
        ))
        .unwrap();
    }
    let before = snapshot(&client, &source_id).await;
    let failed_command_id = "fork-prompt-failed";
    let failed = execute(
        &client,
        envelope(
            failed_command_id,
            &source_id,
            Some(&before),
            serde_json::json!({
                "kind": "forkTask",
                "newTaskId": failed_target,
                "boundary": "tip",
                "initialPrompt": "must not dispatch",
                "sourceLink": format!("luna://open/chat/{source_id}?workspace=fixture"),
                "review": history_review(&core, &before, &source_id, None, None),
            }),
        ),
    )
    .await;
    assert_ne!(failed.status, CommandReceiptStatus::Applied);
    assert!(
        core.store
            .local_executable_hash(
                &failed_target,
                luna_sync::LocalExecutableKind::Command,
                &format!("fork-run:{failed_command_id}"),
            )
            .unwrap()
            .is_none()
    );
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_fork_prompt_snapshot")
            .unwrap();
    }

    let target_id = uuid::Uuid::new_v4().to_string();
    let before = snapshot(&client, &source_id).await;
    let command_id = "fork-prompt-applied";
    let applied = execute(
        &client,
        envelope(
            command_id,
            &source_id,
            Some(&before),
            serde_json::json!({
                "kind": "forkTask",
                "newTaskId": target_id,
                "boundary": "tip",
                "initialPrompt": "locally reviewed fork prompt",
                "sourceLink": format!("luna://open/chat/{source_id}?workspace=fixture"),
                "review": history_review(&core, &before, &source_id, None, None),
            }),
        ),
    )
    .await;
    assert_eq!(applied.status, CommandReceiptStatus::Applied);
    let run_id = format!("fork-run:{command_id}");
    assert!(
        core.store
            .local_executable_hash(&target_id, luna_sync::LocalExecutableKind::Command, &run_id)
            .unwrap()
            .is_some()
    );
    let handle = core.doc_host.open(&target_id).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if handle.doc().read_commands().unwrap().iter().any(|row| {
                row.id == run_id && row.status == luna_doc::SessionCommandStatus::Applied
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    core.shutdown().await;
}

#[tokio::test]
async fn native_tip_fork_creates_independent_task_binding_and_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let forks = Arc::new(Mutex::new(Vec::new()));
    let core = assemble_history(dir.path(), forks.clone(), HarnessId::Opencode);
    let client = luna_rpc::memory_client(core.rpc_service());
    let source_task_id = uuid::Uuid::new_v4().to_string();
    let target_task_id = uuid::Uuid::new_v4().to_string();

    let created = execute(
        &client,
        envelope(
            "history-create-source",
            &source_task_id,
            None,
            serde_json::json!({
                "kind": "createChat",
                "deviceId": core.device_id,
                "cwd": "/tmp",
                "config": history_task_config(HarnessId::Opencode),
            }),
        ),
    )
    .await;
    assert_eq!(created.status, CommandReceiptStatus::Applied);

    let started = execute(
        &client,
        envelope(
            "history-materialize-source",
            &source_task_id,
            Some(&snapshot(&client, &source_task_id).await),
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": history_request("source prompt", HarnessId::Opencode),
                    "messageId": "source-user-1",
                }
            }),
        ),
    )
    .await;
    assert!(matches!(
        started.status,
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
    ));

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let materialized = core
                .task_identity
                .view(&source_task_id)
                .and_then(|view| view.current_binding)
                .is_some_and(|binding| !binding.provider_task_id.is_empty());
            if materialized
                && snapshot(&client, &source_task_id)
                    .await
                    .snapshot
                    .active_turn_id
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let fork_snapshot = snapshot(&client, &source_task_id).await;
    let fork_review = history_review(&core, &fork_snapshot, &source_task_id, None, None);
    let invalid_target = execute(
        &client,
        envelope(
            "history-fork-invalid-target",
            &source_task_id,
            Some(&fork_snapshot),
            serde_json::json!({
                "kind": "forkTask",
                "newTaskId": "not-a-uuid",
                "boundary": "tip",
                "sourceLink": format!("luna://open/chat/{source_task_id}?workspace=fixture"),
                "review": fork_review.clone(),
            }),
        ),
    )
    .await;
    assert_eq!(invalid_target.status, CommandReceiptStatus::Failed);
    assert!(forks.lock().unwrap().is_empty());

    let failed_target_task_id = uuid::Uuid::new_v4().to_string();
    core.doc_host.flush_all();
    let db_path = dir.path().join("data/orgs/l13-org/l13-user/docs.sqlite3");
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(&format!(
            "CREATE TRIGGER fail_history_target_snapshot BEFORE INSERT ON snapshots
             WHEN NEW.doc_id='{failed_target_task_id}'
             BEGIN SELECT RAISE(ABORT, 'forced history target snapshot failure'); END;"
        ))
        .unwrap();
    }
    let post_provider_failure = execute(
        &client,
        envelope(
            "history-fork-post-provider-failure",
            &source_task_id,
            Some(&fork_snapshot),
            serde_json::json!({
                "kind": "forkTask",
                "newTaskId": failed_target_task_id,
                "boundary": "tip",
                "sourceLink": format!("luna://open/chat/{source_task_id}?workspace=fixture"),
                "review": fork_review,
            }),
        ),
    )
    .await;
    assert_eq!(post_provider_failure.status, CommandReceiptStatus::Pending);
    assert_eq!(
        post_provider_failure
            .reason
            .as_ref()
            .map(|reason| reason.code),
        Some(CommandReasonCode::ProviderAcceptanceUnknown)
    );
    assert_eq!(
        post_provider_failure.correlation_id.as_deref(),
        Some("synthetic-history-fork-1")
    );
    assert!(core.task_identity.view(&failed_target_task_id).is_none());
    assert!(
        core.workspace
            .chat(&failed_target_task_id)
            .unwrap()
            .is_none()
    );
    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_history_target_snapshot")
            .unwrap();
    }

    let fork_snapshot = snapshot(&client, &source_task_id).await;
    let fork_review = history_review(&core, &fork_snapshot, &source_task_id, None, None);
    let mut requested_full = history_task_config(HarnessId::Opencode);
    requested_full.permission_mode = PermissionMode::Full;
    requested_full.effective_permission_mode = PermissionMode::Full;
    let forked = execute(
        &client,
        envelope(
            "history-fork-tip",
            &source_task_id,
            Some(&fork_snapshot),
            serde_json::json!({
                "kind": "forkTask",
                "newTaskId": target_task_id,
                "boundary": "tip",
                "title": "Independent fork",
                "config": requested_full,
                "sourceLink": format!("luna://open/chat/{source_task_id}?workspace=fixture"),
                "review": fork_review,
            }),
        ),
    )
    .await;
    assert_eq!(forked.status, CommandReceiptStatus::Applied);
    assert_eq!(
        forked.correlation_id.as_deref(),
        Some("synthetic-history-fork-2")
    );

    let source = core.task_identity.view(&source_task_id).unwrap();
    let target = core.task_identity.view(&target_task_id).unwrap();
    assert_eq!(
        target.task.lineage.source_task_id.as_deref(),
        Some(source_task_id.as_str())
    );
    assert_eq!(
        target.task.lineage.lifecycle_owner_task_id.as_deref(),
        Some(target_task_id.as_str())
    );
    let source_binding = source.current_binding.unwrap();
    let target_binding = target.current_binding.unwrap();
    assert_eq!(target_binding.origin, BindingOrigin::Forked);
    assert_ne!(
        source_binding.provider_task_id,
        target_binding.provider_task_id
    );
    assert_eq!(target_binding.provider_task_id, "synthetic-history-fork-2");
    let target_config = core
        .workspace
        .chat(&target_task_id)
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    assert_eq!(target_config.permission_mode, PermissionMode::Ask);
    assert_eq!(target_config.effective_permission_mode, PermissionMode::Ask);
    let descendant_task_id = uuid::Uuid::new_v4().to_string();
    let descendant_snapshot = snapshot(&client, &target_task_id).await;
    let descendant_review =
        history_review(&core, &descendant_snapshot, &target_task_id, None, None);
    let descendant = execute(
        &client,
        envelope(
            "history-fork-descendant",
            &target_task_id,
            Some(&descendant_snapshot),
            serde_json::json!({
                "kind": "forkTask",
                "newTaskId": descendant_task_id,
                "boundary": "tip",
                "sourceLink": format!("luna://open/chat/{target_task_id}?workspace=fixture"),
                "review": descendant_review,
            }),
        ),
    )
    .await;
    assert_eq!(descendant.status, CommandReceiptStatus::Applied);
    let calls = forks.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert!(matches!(
        calls[0].boundary,
        luna_proto::ProviderForkBoundary::Tip
    ));
    assert!(matches!(
        calls[1].boundary,
        luna_proto::ProviderForkBoundary::Tip
    ));
    assert_eq!(calls[2].source_session_id, "synthetic-history-fork-2");
    drop(calls);
    core.shutdown().await;
}

#[tokio::test]
async fn earlier_message_edit_rebinds_to_native_fork_and_keeps_recovery_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let forks = Arc::new(Mutex::new(Vec::new()));
    let core = assemble_history(dir.path(), forks.clone(), HarnessId::Codex);
    let client = luna_rpc::memory_client(core.rpc_service());
    let task_id = uuid::Uuid::new_v4().to_string();

    assert_eq!(
        execute(
            &client,
            envelope(
                "history-edit-create",
                &task_id,
                None,
                serde_json::json!({
                    "kind": "createChat",
                    "deviceId": core.device_id,
                    "cwd": "/tmp",
                    "config": history_task_config(HarnessId::Codex),
                }),
            ),
        )
        .await
        .status,
        CommandReceiptStatus::Applied
    );
    execute(
        &client,
        envelope(
            "history-edit-materialize",
            &task_id,
            Some(&snapshot(&client, &task_id).await),
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": history_request("source prompt", HarnessId::Codex),
                    "messageId": "edit-source-user",
                }
            }),
        ),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ready = core
                .task_identity
                .view(&task_id)
                .and_then(|view| view.current_binding)
                .is_some_and(|binding| !binding.provider_task_id.is_empty());
            let checkpoint = core.doc_host.open(&task_id).ok().and_then(|handle| {
                handle
                    .doc()
                    .read_entries()
                    .ok()?
                    .into_iter()
                    .find(|entry| entry.role == luna_doc::MessageRole::Assistant)
                    .and_then(|entry| handle.doc().provider_checkpoint(&entry.id))
            });
            if ready
                && checkpoint.is_some()
                && snapshot(&client, &task_id)
                    .await
                    .snapshot
                    .active_turn_id
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let unchanged_snapshot = snapshot(&client, &task_id).await;
    let unchanged_review = history_review(
        &core,
        &unchanged_snapshot,
        &task_id,
        Some("edit-source-user"),
        Some("source prompt"),
    );
    let unchanged = execute(
        &client,
        envelope(
            "history-edit-unchanged",
            &task_id,
            Some(&unchanged_snapshot),
            serde_json::json!({
                "kind": "editEarlierMessage",
                "messageId": "edit-source-user",
                "text": "source prompt",
                "review": unchanged_review,
            }),
        ),
    )
    .await;
    assert_eq!(unchanged.status, CommandReceiptStatus::Failed);
    assert_eq!(forks.lock().unwrap().len(), 0);

    let stale_snapshot = snapshot(&client, &task_id).await;
    let stale_review = history_review(
        &core,
        &stale_snapshot,
        &task_id,
        Some("edit-source-user"),
        Some("source prompt"),
    );
    let changed = execute(
        &client,
        envelope(
            "history-review-source-change",
            &task_id,
            Some(&stale_snapshot),
            serde_json::json!({ "kind": "setRead", "read": false }),
        ),
    )
    .await;
    assert_eq!(changed.status, CommandReceiptStatus::Applied);
    let fresh_snapshot = snapshot(&client, &task_id).await;
    let stale = execute(
        &client,
        envelope(
            "history-edit-stale-review",
            &task_id,
            Some(&fresh_snapshot),
            serde_json::json!({
                "kind": "editEarlierMessage",
                "messageId": "edit-source-user",
                "text": "stale overwrite",
                "review": stale_review,
            }),
        ),
    )
    .await;
    assert_eq!(stale.status, CommandReceiptStatus::Failed);
    assert_eq!(forks.lock().unwrap().len(), 0);

    let fingerprint_snapshot = snapshot(&client, &task_id).await;
    let mut reused_id_review = history_review(
        &core,
        &fingerprint_snapshot,
        &task_id,
        Some("edit-source-user"),
        Some("source prompt"),
    );
    reused_id_review["messageText"] = "different text under the same message id".into();
    let reused_id = execute(
        &client,
        envelope(
            "history-edit-reused-id",
            &task_id,
            Some(&fingerprint_snapshot),
            serde_json::json!({
                "kind": "editEarlierMessage",
                "messageId": "edit-source-user",
                "text": "replacement after reused id",
                "review": reused_id_review,
            }),
        ),
    )
    .await;
    assert_eq!(reused_id.status, CommandReceiptStatus::Failed);
    assert_eq!(forks.lock().unwrap().len(), 0);

    let before_binding = core
        .task_identity
        .view(&task_id)
        .unwrap()
        .current_binding
        .unwrap();
    let edit_snapshot = snapshot(&client, &task_id).await;
    let edit_review = history_review(
        &core,
        &edit_snapshot,
        &task_id,
        Some("edit-source-user"),
        Some("source prompt"),
    );
    let edited = execute(
        &client,
        envelope(
            "history-edit-command",
            &task_id,
            Some(&edit_snapshot),
            serde_json::json!({
                "kind": "editEarlierMessage",
                "messageId": "edit-source-user",
                "text": "rewritten source prompt",
                "review": edit_review,
            }),
        ),
    )
    .await;
    assert_eq!(edited.status, CommandReceiptStatus::Applied);
    let internal_run_id = "history-edit-run:history-edit-command";
    assert!(
        core.store
            .local_executable_hash(
                &task_id,
                luna_sync::LocalExecutableKind::Command,
                internal_run_id,
            )
            .unwrap()
            .is_some()
    );

    let after = core.task_identity.view(&task_id).unwrap();
    let after_binding = after.current_binding.unwrap();
    assert_eq!(after_binding.generation, before_binding.generation + 1);
    assert_eq!(after_binding.origin, BindingOrigin::Forked);
    assert_ne!(
        after_binding.provider_task_id,
        before_binding.provider_task_id
    );
    assert_eq!(
        after
            .bindings
            .iter()
            .find(|binding| binding.binding_id == before_binding.binding_id)
            .and_then(|binding| binding.replaced_by_binding_id.as_deref()),
        Some(after_binding.binding_id.as_str())
    );
    let edit_context = core.doc_host.context_control(&task_id).unwrap();
    assert_eq!(
        edit_context.binding_generation,
        Some(after_binding.generation)
    );
    assert!(edit_context.pending_handoff.is_none());
    assert!(edit_context.last_handoff.is_none());
    let recoveries: Vec<HistoryRecoveryView> = client
        .call_as(
            methods::LIST_HISTORY_RECOVERIES,
            serde_json::json!({ "taskId": task_id }),
        )
        .await
        .unwrap();
    assert_eq!(recoveries.len(), 1);
    assert_eq!(recoveries[0].cutoff_message_id, "edit-source-user");
    assert!(
        core.store
            .load_snapshot(&format!(
                "luna-history-recovery:{}",
                recoveries[0].recovery_id
            ))
            .unwrap()
            .is_some()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let command_settled = core
                .doc_host
                .open(&task_id)
                .unwrap()
                .doc()
                .read_commands()
                .unwrap()
                .iter()
                .all(|command| command.status != luna_doc::SessionCommandStatus::Pending);
            if command_settled
                && snapshot(&client, &task_id)
                    .await
                    .snapshot
                    .active_turn_id
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        core.doc_host
            .open(&task_id)
            .unwrap()
            .doc()
            .read_commands()
            .unwrap()
            .iter()
            .any(|command| {
                command.id == internal_run_id
                    && command.status == luna_doc::SessionCommandStatus::Applied
            })
    );
    let restore_snapshot = snapshot(&client, &task_id).await;
    let mut restore_review = history_review(&core, &restore_snapshot, &task_id, None, None);
    restore_review["messageText"] = recoveries[0].source_provider_session_id.clone().into();
    let restored = execute(
        &client,
        envelope(
            "history-restore-command",
            &task_id,
            Some(&restore_snapshot),
            serde_json::json!({
                "kind": "restoreHistoryRecovery",
                "recoveryId": recoveries[0].recovery_id,
                "review": restore_review,
            }),
        ),
    )
    .await;
    assert_eq!(
        restored.status,
        CommandReceiptStatus::Applied,
        "{:?}",
        restored.reason
    );
    let restored_binding = core
        .task_identity
        .view(&task_id)
        .unwrap()
        .current_binding
        .unwrap();
    assert_eq!(restored_binding.generation, after_binding.generation + 1);
    let restored_context = core.doc_host.context_control(&task_id).unwrap();
    assert_eq!(
        restored_context.binding_generation,
        Some(restored_binding.generation)
    );
    assert!(restored_context.pending_handoff.is_none());
    assert!(restored_context.last_handoff.is_none());
    let recoveries_after: Vec<HistoryRecoveryView> = client
        .call_as(
            methods::LIST_HISTORY_RECOVERIES,
            serde_json::json!({ "taskId": task_id }),
        )
        .await
        .unwrap();
    assert_eq!(recoveries_after.len(), 2);
    assert!(
        recoveries_after
            .iter()
            .any(|recovery| recovery.recovery_id == "restore:history-restore-command")
    );
    let calls = forks.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(matches!(
        &calls[0].boundary,
        luna_proto::ProviderForkBoundary::BeforeCheckpoint { checkpoint_id }
            if checkpoint_id == "provider-message-1"
    ));
    assert_eq!(calls[1].source_session_id, before_binding.provider_task_id);
    assert!(matches!(
        &calls[1].boundary,
        luna_proto::ProviderForkBoundary::ThroughCheckpoint { checkpoint_id }
            if checkpoint_id == "provider-message-1"
    ));
    drop(calls);
    core.shutdown().await;
}

/// Side chats use the same native-fork acknowledgement/receipt boundary while
/// retaining only their new exchanges in the side transcript.
#[tokio::test(flavor = "multi_thread")]
async fn side_chat_native_fork_preserves_binding_and_never_replays_unknown_creation() {
    for behavior in [
        HistoryForkBehavior::Success,
        HistoryForkBehavior::FailAfterAcknowledgement,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let forks = Arc::new(Mutex::new(Vec::new()));
        let core = assemble_history_with_behavior(
            dir.path(),
            forks.clone(),
            HarnessId::Opencode,
            behavior,
        );
        let client = luna_rpc::memory_client(core.rpc_service());
        let parent = uuid::Uuid::new_v4().to_string();
        create_materialized_history_task(&core, &client, &parent, HarnessId::Opencode).await;
        let before = core
            .doc_host
            .open(&parent)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap();
        let request =
            serde_json::json!({"parentId":parent,"requestId":uuid::Uuid::new_v4().to_string()});
        let first = client.call("CreateSideChat", request.clone()).await;
        let second = client.call("CreateSideChat", request).await;
        assert_eq!(
            forks.lock().unwrap().len(),
            1,
            "creation cannot replay a native fork"
        );
        if matches!(behavior, HistoryForkBehavior::Success) {
            let created = first.unwrap();
            assert_eq!(created["meta"]["contextMode"], "nativeFork");
            assert_eq!(created["meta"]["id"], second.unwrap()["meta"]["id"]);
            let child = created["meta"]["id"].as_str().unwrap();
            assert!(
                core.doc_host
                    .open(child)
                    .unwrap()
                    .doc()
                    .read_entries()
                    .unwrap()
                    .is_empty()
            );
            assert_ne!(
                core.task_identity
                    .view(child)
                    .unwrap()
                    .current_binding
                    .unwrap()
                    .provider_task_id,
                core.task_identity
                    .view(&parent)
                    .unwrap()
                    .current_binding
                    .unwrap()
                    .provider_task_id
            );
            client
                .call(
                    "UnloadSideChat",
                    serde_json::json!({"id":child,"lease":created["lease"]}),
                )
                .await
                .unwrap();
            assert!(
                client
                    .call(
                        "QueueMessage",
                        serde_json::json!({"chatId":child,"text":"must not resume"})
                    )
                    .await
                    .is_err()
            );
        } else {
            assert!(first.is_err());
            assert!(second.is_err());
        }
        assert_eq!(
            before,
            core.doc_host
                .open(&parent)
                .unwrap()
                .doc()
                .read_entries()
                .unwrap()
        );
        core.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn side_chat_context_snapshot_is_an_explicit_native_fork_alternative() {
    let dir = tempfile::tempdir().unwrap();
    let forks = Arc::new(Mutex::new(Vec::new()));
    let core = assemble_history(dir.path(), forks.clone(), HarnessId::Opencode);
    let client = luna_rpc::memory_client(core.rpc_service());
    let parent = uuid::Uuid::new_v4().to_string();
    create_materialized_history_task(&core, &client, &parent, HarnessId::Opencode).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let request =
        serde_json::json!({"parentId":parent,"requestId":request_id,"forceSnapshot":true});
    let created = client.call("CreateSideChat", request).await.unwrap();
    assert_eq!(created["meta"]["contextMode"], "contextSnapshot");
    assert!(
        forks.lock().unwrap().is_empty(),
        "explicit snapshot must not attempt native fork"
    );
    assert!(
        client
            .call(
                "CreateSideChat",
                serde_json::json!({"parentId":parent,"requestId":request_id,"forceSnapshot":false})
            )
            .await
            .is_err(),
        "request dedupe includes the explicit context choice"
    );
    core.shutdown().await;
}
