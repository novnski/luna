//! L07 behavior at the real UI-RPC -> engine -> SQLite/doc-command boundary.
//! All provider homes are synthetic and the harness is deterministic.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use zeron_doc::MessageRole;
use zeron_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, CommandReasonCode, CommandReceipt, CommandReceiptStatus, DoneStatus, HarnessId,
    Model, PermissionMode, ReasoningLevel, RunRequest, SandboxLevel, SteeringMode, TaskCommandRead,
};
use zeron_rpc::{RpcClient, methods};

struct CountingHarness {
    runs: Arc<AtomicUsize>,
    hold: bool,
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
        SteeringMode::StepBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(Vec::new())
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
                controls.interrupt.cancelled().await;
                DoneStatus::Interrupted
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
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
    }
}

fn assemble(root: &std::path::Path, runs: Arc<AtomicUsize>, hold: bool) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CountingHarness { runs, hold }));
    EngineCore::assemble_with_profile_and_agent_accounts_config(
        EngineProfile::development(&root.join("data"), "l07-org", "l07-user"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
        account_config(root),
    )
    .unwrap()
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

async fn snapshot(client: &RpcClient, task_id: &str) -> TaskCommandRead {
    client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": task_id }),
        )
        .await
        .unwrap()
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
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let receipt: Option<CommandReceipt> = client
                .call_as(
                    methods::GET_TASK_COMMAND_RECEIPT,
                    serde_json::json!({ "commandId": command_id }),
                )
                .await
                .unwrap();
            if let Some(receipt) = receipt
                && receipt.status == status
            {
                break receipt;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn create_task(client: &RpcClient, task_id: &str, device_id: &str) {
    let receipt = execute(
        client,
        envelope(
            "create-command",
            task_id,
            None,
            serde_json::json!({ "kind": "createChat", "deviceId": device_id }),
        ),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_send_survives_two_clients_response_loss_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs.clone(), false);
    let client_a = zeron_rpc::memory_client(core.rpc_service());
    let client_b = zeron_rpc::memory_client(core.rpc_service());
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
    let restarted_client = zeron_rpc::memory_client(reopened.rpc_service());
    let after_restart = execute(&restarted_client, command).await;
    assert_eq!(after_restart, applied);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revision_binding_turn_and_lifecycle_fences_are_visible() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs, true);
    let client = zeron_rpc::memory_client(core.rpc_service());
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
