//! L07 behavior at the real UI-RPC -> engine -> SQLite/doc-command boundary.
//! All provider homes are synthetic and the harness is deterministic.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use zeron_doc::{MessageRole, QueueClaimMode, QueueMessageStatus};
use zeron_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, COMPOSER_DRAFT_VERSION, CommandReasonCode, CommandReceipt, CommandReceiptStatus,
    ComposerDraft, ComposerDraftRead, DoneStatus, DraftContext, DraftSubmissionState, HarnessId,
    Model, PermissionMode, ReasoningLevel, RunRequest, SandboxLevel, SteeringMode, TaskCommandRead,
};
use zeron_rpc::{RpcClient, methods};

struct CountingHarness {
    runs: Arc<AtomicUsize>,
    hold: bool,
    steering: SteeringMode,
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
                                        let _ = tx.send(zeron_harness::SteerAcceptance::Accepted);
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
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
    }
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
        hold,
        steering,
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

async fn steer_queue_case(
    steering: SteeringMode,
    with_attachment: bool,
) -> (CommandReceipt, QueueMessageStatus) {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble_with_steering(root.path(), runs, true, steering);
    let client = zeron_rpc::memory_client(core.rpc_service());
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
            zeron_proto::FollowUpBehavior::Steer,
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
    let client = zeron_rpc::memory_client(core.rpc_service());
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
    let client = zeron_rpc::memory_client(core.rpc_service());
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
    let client = zeron_rpc::memory_client(core.rpc_service());
    assert!(core.workspace.chat(&task_id).unwrap().is_some());
    assert!(!core.task_identity.view(&task_id).unwrap().task.tombstoned);
    assert!(snapshot_row_exists(&db_path, &task_id));
    let deleted = execute(&client, delete).await;
    assert_eq!(deleted.status, CommandReceiptStatus::Applied);
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

    // A process loss after the durable commit has no remaining persistent
    // cleanup step to strand: registry, identity, chat doc, and receipt agree.
    drop(client);
    drop(core);
    tokio::task::yield_now().await;
    let final_core = assemble(root.path(), runs, false);
    let final_client = zeron_rpc::memory_client(final_core.rpc_service());
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
async fn local_projection_rolls_back_with_terminal_receipt_failure_and_recovers_after_reopen() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs.clone(), false);
    let client = zeron_rpc::memory_client(core.rpc_service());
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
    let client = zeron_rpc::memory_client(reopened.rpc_service());
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
    let final_client = zeron_rpc::memory_client(final_core.rpc_service());
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
async fn durable_queue_commands_dedupe_and_revision_fence_edit_remove() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs, false);
    let client_a = zeron_rpc::memory_client(core.rpc_service());
    let client_b = zeron_rpc::memory_client(core.rpc_service());
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
    let client = zeron_rpc::memory_client(core.rpc_service());
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
    let client = zeron_rpc::memory_client(reopened.rpc_service());
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
    let client = zeron_rpc::memory_client(core.rpc_service());
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
    let client = zeron_rpc::memory_client(reopened.rpc_service());
    let row = reopened
        .doc_host
        .queued_message(&task_id, "projection-failure-row")
        .unwrap()
        .unwrap();
    assert_eq!(row.text, "original");
    assert_eq!(row.revision, 1);
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
    after_restart.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_durable_claim_never_reaches_the_provider_and_reopens_held() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let task_id = uuid::Uuid::new_v4().to_string();
    let core = assemble(root.path(), runs.clone(), true);
    let client = zeron_rpc::memory_client(core.rpc_service());
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
    assert!(matches!(row.status, zeron_doc::QueueMessageStatus::Held));

    {
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("DROP TRIGGER fail_queue_claim_snapshot")
            .unwrap();
    }
    drop(client);
    drop(core);
    let reopened = assemble(root.path(), runs.clone(), false);
    let row = reopened
        .doc_host
        .queued_message(&task_id, "claim-failure-row")
        .unwrap()
        .unwrap();
    assert!(matches!(row.status, zeron_doc::QueueMessageStatus::Held));
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
    let client = zeron_rpc::memory_client(core.rpc_service());
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
    assert_eq!(begin.kind, zeron_sync::TaskCommandBeginKind::Accepted);
    core.doc_host
        .set_task_command_correlation(command_id, message_id)
        .unwrap();
    let handle = core.doc_host.open(&task_id).unwrap();
    assert!(matches!(
        handle
            .doc()
            .claim_queued(message_id, 1, command_id, QueueClaimMode::SendNow, now)
            .unwrap(),
        zeron_doc::QueueClaimOutcome::Claimed(_)
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
    let client = zeron_rpc::memory_client(core.rpc_service());
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
    assert!(matches!(row.status, zeron_doc::QueueMessageStatus::Held));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    core.shutdown().await;
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
