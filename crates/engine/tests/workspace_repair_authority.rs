//! Failure-first lifecycle contract for a workspace association repair.
//! Exercises engine/SQLite/CRDT boundaries with synthetic provider homes.
//! Identity proof/allocation RPC coverage belongs to workspace_identity_roots.
//! These cases were authored before their new store/lifecycle interfaces.

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, ChatConfig, CommandReceipt, CommandReceiptStatus, DoneStatus, FollowUpBehavior,
    HarnessId, Model, PermissionMode, ReasoningLevel, RunRequest, SandboxLevel, SteeringMode,
};
use luna_rpc::methods;
use luna_sync::{
    LocalExecutableKind, StoredTaskRecovery, TaskCommandBeginKind, WorkspaceMaterializationFence,
    WorkspaceRepairFence,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Default)]
struct SyntheticHarness {
    calls: AtomicUsize,
    requests: Mutex<Vec<RunRequest>>,
}

#[async_trait]
impl Harness for SyntheticHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Workspace authority fixture"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(AgentEvent::SessionStarted {
                    harness: HarnessId::Mock,
                    model: "synthetic".into(),
                    tools: vec![],
                    cwd: request.cwd,
                    session_id: "synthetic-workspace-session".into(),
                    assistant_message_id: "reply".into(),
                }))
                .await;
            let _ = tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: Some("synthetic-workspace-session".into()),
                }))
                .await;
            if request.prompt == "keep warm" {
                controls.interrupt.cancelled().await;
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })
        .boxed())
    }
}

fn assemble(root: &Path, harness: Arc<SyntheticHarness>) -> EngineCore {
    let accounts = AgentAccountsConfig {
        data_dir: root.join("accounts"),
        claude_config_dir: root.join("claude"),
        claude_config_file: root.join("claude.json"),
        codex_home: root.join("codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(harness);
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("data"), "w-auth-org", "w-auth-user"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap()
}

fn db(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(root.join("data/orgs/w-auth-org/w-auth-user/docs.sqlite3")).unwrap()
}

async fn fixture() -> (tempfile::TempDir, EngineCore, Arc<SyntheticHarness>, String) {
    let root = tempfile::tempdir().unwrap();
    let harness = Arc::new(SyntheticHarness::default());
    let core = assemble(root.path(), harness.clone());
    let task = uuid::Uuid::new_v4().to_string();
    create(&core, root.path(), &task).await;
    (root, core, harness, task)
}

async fn create(core: &EngineCore, root: &Path, task: &str) {
    let config = ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some("synthetic".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: vec![],
    };
    let client = luna_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::MUTATE,
            serde_json::json!({"op":"createChat", "chatId":task,
        "deviceId":core.device_id, "cwd":root.canonicalize().unwrap(), "config":config}),
        )
        .await
        .unwrap();
    core.workspace
        .rename_chat(task, "Synthetic workspace authority QA")
        .unwrap();
}

fn request(root: &Path, prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: Some(HarnessId::Mock),
        provider_account_id: None,
        model: Some("synthetic".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: root.canonicalize().unwrap().to_string_lossy().into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: vec![],
        worktree: None,
    }
}

fn pending(core: &EngineCore, task: &str, command: &str, intent: &str) -> CommandReceipt {
    CommandReceipt {
        command_id: command.into(),
        task_id: task.into(),
        intent: intent.into(),
        payload_hash: format!("fixture-{command}"),
        status: CommandReceiptStatus::Pending,
        snapshot: core.store.task_command_snapshot(task).unwrap().snapshot,
        event_sequence: None,
        actor_id: "local-fixture".into(),
        client_id: "workspace-authority".into(),
        client_session_id: "synthetic".into(),
        execution_environment: Some(core.device_id.clone()),
        correlation_id: None,
        reason: None,
        recovery_action: None,
        accepted_at_ms: 1,
        committed_at_ms: None,
        observed_at_ms: 1,
    }
}

fn begin_repair(core: &EngineCore, task: &str, command: &str) -> WorkspaceRepairFence {
    let fence = WorkspaceRepairFence {
        expected_task_revision: core
            .store
            .task_command_snapshot(task)
            .unwrap()
            .snapshot
            .task_revision,
        expected_workspace_repair_revision: core.store.workspace_repair_revision(task).unwrap(),
    };
    let begun = core
        .doc_host
        .begin_task_command(
            pending(core, task, command, "repairWorkspaceIdentity"),
            Some(fence.expected_task_revision),
            None,
            None,
        )
        .unwrap();
    assert_eq!(begun.kind, TaskCommandBeginKind::Accepted);
    fence
}

fn commit_repair(
    core: &EngineCore,
    task: &str,
    command: &str,
    fence: &WorkspaceRepairFence,
) -> Result<CommandReceipt, luna_sync::StoreError> {
    // A deliberately isolated storage proof snapshot; actual identity evidence
    // and v2 event semantics are covered by the Workspace owner's RPC suite.
    let proof_key = format!("authority-fixture-proof:{task}");
    core.store
        .complete_task_command_with_snapshot_changes_and_workspace_repair(
            command,
            task,
            fence,
            None,
            None,
            &[(proof_key.as_str(), b"verified-association")],
        )
}

fn evidence(name: &str, value: serde_json::Value) {
    eprintln!("WORKSPACE_AUTHORITY_E2E {name} {value}");
    if let Some(dir) = std::env::var_os("LUNA_WORKSPACE_AUTHORITY_REPORT_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            PathBuf::from(dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn repair_transaction_advances_r_and_w_once_and_preserves_receipts_on_restart() {
    let (root, core, harness, task) = fixture().await;
    let original = pending(&core, &task, "proof-repair", "repairWorkspaceIdentity");
    let fence = begin_repair(&core, &task, "proof-repair");
    let receipt = commit_repair(&core, &task, "proof-repair", &fence).unwrap();
    let w = core.store.workspace_repair_revision(&task).unwrap();
    evidence(
        "atomic-repair",
        serde_json::json!({"beforeR":fence.expected_task_revision,
        "beforeW":fence.expected_workspace_repair_revision, "afterW":w, "receipt":receipt}),
    );
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
    assert_eq!(
        receipt.snapshot.task_revision,
        fence.expected_task_revision + 1
    );
    assert_eq!(w, receipt.snapshot.task_revision);
    let duplicate = core
        .doc_host
        .begin_task_command(original.clone(), None, None, None)
        .unwrap();
    assert_eq!(duplicate.kind, TaskCommandBeginKind::Duplicate);
    assert_eq!(duplicate.receipt, receipt);
    assert!(
        commit_repair(&core, &task, "proof-repair", &fence).is_err(),
        "terminal callback must not publish a newly staged in-memory projection"
    );
    core.shutdown().await;
    drop(core);
    let reopened = assemble(root.path(), harness.clone());
    let duplicate = reopened
        .doc_host
        .begin_task_command(original, None, None, None)
        .unwrap();
    assert_eq!(duplicate.receipt, receipt);
    assert_eq!(reopened.store.workspace_repair_revision(&task).unwrap(), w);
    assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
    reopened.shutdown().await;
}

#[tokio::test]
async fn repair_cas_and_sql_failure_leave_mapping_revision_and_receipt_uncommitted() {
    let (root, core, harness, task) = fixture().await;
    let fence = begin_repair(&core, &task, "repair-fault");
    let old_receipt = core.store.task_command_receipt("repair-fault").unwrap();
    let wrong = WorkspaceRepairFence {
        expected_task_revision: fence.expected_task_revision,
        expected_workspace_repair_revision: fence.expected_workspace_repair_revision + 1,
    };
    assert!(commit_repair(&core, &task, "repair-fault", &wrong).is_err());
    let wrong_r = WorkspaceRepairFence {
        expected_task_revision: fence.expected_task_revision + 1,
        expected_workspace_repair_revision: fence.expected_workspace_repair_revision,
    };
    assert!(commit_repair(&core, &task, "repair-fault", &wrong_r).is_err());
    db(root.path()).execute_batch("CREATE TRIGGER fail_repair_w BEFORE UPDATE OF workspace_repair_revision ON task_command_state BEGIN SELECT RAISE(ABORT, 'fixture repair storage failure'); END;").unwrap();
    assert!(commit_repair(&core, &task, "repair-fault", &fence).is_err());
    let after = core.store.task_command_snapshot(&task).unwrap().snapshot;
    evidence(
        "repair-sql-failure",
        serde_json::json!({"after":after,
        "w":core.store.workspace_repair_revision(&task).unwrap(),
        "receipt":core.store.task_command_receipt("repair-fault").unwrap()}),
    );
    assert_eq!(after.task_revision, fence.expected_task_revision);
    assert_eq!(
        core.store.workspace_repair_revision(&task).unwrap(),
        fence.expected_workspace_repair_revision
    );
    assert_eq!(
        core.store.task_command_receipt("repair-fault").unwrap(),
        old_receipt
    );
    db(root.path())
        .execute_batch("DROP TRIGGER fail_repair_w")
        .unwrap();
    core.shutdown().await;
    drop(core);
    let reopened = assemble(root.path(), harness);
    assert_eq!(
        reopened.store.workspace_repair_revision(&task).unwrap(),
        fence.expected_workspace_repair_revision
    );
    assert_eq!(
        reopened.store.task_command_receipt("repair-fault").unwrap(),
        old_receipt
    );
    commit_repair(&reopened, &task, "repair-fault", &fence).unwrap();
    reopened.shutdown().await;
}

#[tokio::test]
async fn identical_old_hash_and_local_edit_cannot_renew_pre_repair_authority() {
    let (_root, core, harness, task) = fixture().await;
    core.store
        .record_local_executable(
            &task,
            LocalExecutableKind::Queue,
            "old-row",
            "same-hash",
            None,
        )
        .unwrap();
    assert!(
        core.store
            .local_executable_matches(&task, LocalExecutableKind::Queue, "old-row", "same-hash")
            .unwrap()
    );
    let fence = begin_repair(&core, &task, "repair-old-grant");
    commit_repair(&core, &task, "repair-old-grant", &fence).unwrap();
    let old_matches = core
        .store
        .local_executable_matches(&task, LocalExecutableKind::Queue, "old-row", "same-hash")
        .unwrap();
    let repeated = core
        .store
        .record_local_executable(
            &task,
            LocalExecutableKind::Queue,
            "old-row",
            "same-hash",
            None,
        )
        .unwrap();
    let edited = core
        .store
        .record_local_executable(
            &task,
            LocalExecutableKind::Queue,
            "old-row",
            "edited-hash",
            Some("same-hash"),
        )
        .unwrap();
    evidence(
        "old-authority-not-renewed",
        serde_json::json!({"oldMatches":old_matches,"repeated":repeated,"edited":edited}),
    );
    assert!(!old_matches && !repeated && !edited);
    let current_w = core.store.workspace_repair_revision(&task).unwrap();
    assert!(
        core.store
            .record_local_executable(
                &task,
                LocalExecutableKind::Queue,
                "fresh-row",
                "fresh-hash",
                None
            )
            .unwrap()
    );
    core.doc_host
        .begin_task_command(
            pending(&core, &task, "unrelated-rename", "rename"),
            None,
            None,
            None,
        )
        .unwrap();
    core.doc_host
        .complete_task_command(
            "unrelated-rename",
            CommandReceiptStatus::Applied,
            true,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        core.store.workspace_repair_revision(&task).unwrap(),
        current_w
    );
    assert!(
        core.store
            .local_executable_matches(&task, LocalExecutableKind::Queue, "fresh-row", "fresh-hash")
            .unwrap(),
        "an unrelated R increment must not invalidate current W grants"
    );
    assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
    core.shutdown().await;
}

#[tokio::test]
async fn restored_old_queue_is_inert_until_a_new_explicit_current_consent() {
    let (root, core, harness, task) = fixture().await;
    let handle = core.doc_host.open(&task).unwrap();
    core.doc_host.pause_all_queues();
    core.doc_host
        .queue_message_with_id_and_execution(
            &task,
            "old-row",
            "saved prompt",
            vec![],
            true,
            FollowUpBehavior::Steer,
            None,
        )
        .unwrap();
    let old_snapshot = handle.doc().export_snapshot().unwrap();
    let old_hash = core
        .store
        .local_executable_hash(&task, LocalExecutableKind::Queue, "old-row")
        .unwrap()
        .unwrap();
    assert!(
        core.doc_host
            .remove_queued_message(&task, "old-row")
            .await
            .unwrap()
    );
    let fence = begin_repair(&core, &task, "repair-before-restored-row");
    let receipt = commit_repair(&core, &task, "repair-before-restored-row", &fence).unwrap();
    core.shutdown().await;
    drop(handle);
    drop(core);
    // Fault injection models old document/grant material returning while the
    // current repair high-water state survives. It is not full-profile rollback.
    let sql = db(root.path());
    sql.execute(
        "UPDATE snapshots SET bytes=?2 WHERE doc_id=?1",
        rusqlite::params![task, old_snapshot],
    )
    .unwrap();
    sql.execute("INSERT OR REPLACE INTO local_executable_authority(doc_id,kind,row_id,payload_hash,workspace_repair_revision) VALUES (?1,'queue','old-row',?2,0)", rusqlite::params![task, old_hash]).unwrap();
    drop(sql);
    let reopened = assemble(root.path(), harness.clone());
    let handle = reopened.doc_host.open(&task).unwrap();
    reopened.doc_host.drain_queue(&handle).await;
    assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
    assert!(
        !reopened
            .store
            .local_executable_matches(&task, LocalExecutableKind::Queue, "old-row", &old_hash)
            .unwrap()
    );
    let _ = reopened
        .doc_host
        .update_queued_message(&task, "old-row", "saved prompt edited");
    use rusqlite::OptionalExtension as _;
    let grant_w: Option<i64> = db(root.path()).query_row("SELECT workspace_repair_revision FROM local_executable_authority WHERE doc_id=?1 AND row_id='old-row'", [&task], |row| row.get(0)).optional().unwrap();
    assert!(
        grant_w.is_none_or(|w| w == 0),
        "ordinary edit must not adopt stale authority"
    );
    assert!(
        reopened
            .doc_host
            .send_queued_now(&task, "old-row")
            .await
            .unwrap(),
        "new explicit exact-row consent may adopt current authority"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while harness.calls.load(Ordering::SeqCst) != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    evidence(
        "restored-row-new-consent",
        serde_json::json!({"task":task,
        "w":reopened.store.workspace_repair_revision(&task).unwrap(),"calls":harness.calls.load(Ordering::SeqCst)}),
    );
    assert_eq!(
        reopened.store.workspace_repair_revision(&task).unwrap(),
        receipt.snapshot.task_revision
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn quiescence_blocks_pending_work_and_only_exempts_its_own_repair_receipt() {
    let (root, core, harness, task) = fixture().await;
    let sibling = uuid::Uuid::new_v4().to_string();
    create(&core, root.path(), &sibling).await;
    let fence = begin_repair(&core, &task, "current-repair");
    let _lane = core.doc_host.task_command_gate(&task).await;
    assert!(
        core.doc_host
            .prepare_workspace_identity_repair(&task, None)
            .await
            .is_err(),
        "preview does not ignore pending repair receipts"
    );
    assert!(
        core.doc_host
            .prepare_workspace_identity_repair(&task, Some("another-id"))
            .await
            .is_err()
    );
    let guard = core
        .doc_host
        .prepare_workspace_identity_repair(&task, Some("current-repair"))
        .await
        .unwrap();
    drop(guard);
    db(root.path()).execute("INSERT INTO side_chat_sends(chat_id,message_id,state,payload_hash) VALUES (?1,'pending-side-send','unknown','synthetic')", [&task]).unwrap();
    assert!(
        core.doc_host
            .prepare_workspace_identity_repair(&task, Some("current-repair"))
            .await
            .is_err(),
        "a side-panel send admitted before dispatch also blocks repair"
    );
    assert!(commit_repair(&core, &task, "current-repair", &fence).is_err());
    db(root.path())
        .execute("DELETE FROM side_chat_sends WHERE chat_id=?1", [&task])
        .unwrap();
    core.doc_host
        .begin_task_command(
            pending(&core, &task, "pending-run", "run"),
            None,
            None,
            None,
        )
        .unwrap();
    assert!(
        core.doc_host
            .prepare_workspace_identity_repair(&task, Some("current-repair"))
            .await
            .is_err()
    );
    assert!(
        commit_repair(&core, &task, "current-repair", &fence).is_err(),
        "commit rechecks durable pending work"
    );
    let sibling_guard = core
        .doc_host
        .prepare_workspace_identity_repair(&sibling, None)
        .await
        .unwrap();
    drop(sibling_guard);
    assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
    core.shutdown().await;
}

fn recovery(core: &EngineCore, task: &str, status: &str) -> StoredTaskRecovery {
    let record = serde_json::json!({"workspaceRepairRevision":0,"harness":"mock",
        "request":request(Path::new("/tmp"), "old recovery"),"originalAcceptance":"rejectedBeforeAcceptance",
        "attemptAcceptance":null,"attemptStartedAtMs":null,"attemptAutomatic":false,"providerSessionId":null,
        "view":{"chainId":"old-chain","taskId":task,"originalMessageId":"old-message",
        "originalCommandId":null,"originalAcceptance":"rejectedBeforeAcceptance","acceptance":"rejectedBeforeAcceptance",
        "failure":{"kind":"overloaded"},"action":"resendOriginal","status":status,"attemptCount":0,
        "maxAttempts":4,"automatic":false,"retryAtMs":null,"automaticDeadlineMs":i64::MAX,
        "environmentId":core.device_id,"bindingId":null,"bindingGeneration":null,
        "detail":"synthetic old recovery","createdAtMs":1,"updatedAtMs":1}});
    StoredTaskRecovery {
        task_id: task.into(),
        chain_id: "old-chain".into(),
        attempt_count: 0,
        status: status.into(),
        record_json: serde_json::to_vec(&record).unwrap(),
        updated_at: 1,
    }
}

#[tokio::test]
async fn quiescence_blocks_every_retryable_recovery_state_without_rewriting_it() {
    for status in [
        "scheduled",
        "running",
        "checkingDelivery",
        "held",
        "recoveryNeeded",
        "waitingForSignIn",
    ] {
        let (_root, core, harness, task) = fixture().await;
        let record = recovery(&core, &task, status);
        core.store.put_task_recovery(&record).unwrap();
        let _lane = core.doc_host.task_command_gate(&task).await;
        assert!(
            core.doc_host
                .prepare_workspace_identity_repair(&task, None)
                .await
                .is_err(),
            "{status}"
        );
        assert_eq!(core.store.task_recovery(&task).unwrap(), Some(record));
        assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
        core.shutdown().await;
    }
}

#[tokio::test]
async fn idle_warm_runtime_blocks_repair_and_is_not_retired_implicitly() {
    let (root, core, harness, task) = fixture().await;
    core.sessions
        .dispatch(
            &task,
            HarnessId::Mock,
            request(root.path(), "keep warm"),
            Some("warm-input".into()),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if harness.calls.load(Ordering::SeqCst) == 1
                && core.sessions.has_live_run(&task)
                && core.sessions.active_turn_id(&task).is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let _lane = core.doc_host.task_command_gate(&task).await;
    assert!(
        core.doc_host
            .prepare_workspace_identity_repair(&task, None)
            .await
            .is_err()
    );
    assert!(core.sessions.has_live_run(&task));
    assert_eq!(core.store.workspace_repair_revision(&task).unwrap(), 0);
    core.shutdown().await;
}

#[tokio::test]
async fn delayed_dispatch_keeps_original_w_and_rechecks_after_the_transition_gate() {
    let (root, core, harness, task) = fixture().await;
    let fence = begin_repair(&core, &task, "repair-ahead-of-dispatch");
    let _lane = core.doc_host.task_command_gate(&task).await;
    let guard = core
        .doc_host
        .prepare_workspace_identity_repair(&task, Some("repair-ahead-of-dispatch"))
        .await
        .unwrap();
    let sessions = core.sessions.clone();
    let target = task.clone();
    let run = request(root.path(), "delayed original authority");
    let dispatch = tokio::spawn(async move {
        sessions
            .dispatch_at_workspace_revision(
                &target,
                HarnessId::Mock,
                run,
                Some("delayed-input".into()),
                0,
            )
            .await
    });
    tokio::task::yield_now().await;
    assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
    commit_repair(&core, &task, "repair-ahead-of-dispatch", &fence).unwrap();
    drop(guard);
    let result = tokio::time::timeout(Duration::from_secs(5), dispatch)
        .await
        .unwrap()
        .unwrap();
    evidence(
        "post-gate-stale-w",
        serde_json::json!({"rejected":result.is_err(),"calls":harness.calls.load(Ordering::SeqCst)}),
    );
    assert!(result.is_err());
    assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
    core.shutdown().await;
}

#[tokio::test]
async fn old_recovery_retry_after_repair_cannot_reseal_the_frozen_request() {
    let (root, core, harness, task) = fixture().await;
    let old = recovery(&core, &task, "held");
    let fence = begin_repair(&core, &task, "repair-before-recovery-restore");
    commit_repair(&core, &task, "repair-before-recovery-restore", &fence).unwrap();
    core.shutdown().await;
    drop(core);
    let sql = db(root.path());
    sql.execute("INSERT INTO task_recovery_state(task_id,chain_id,attempt_count,status,record_json,updated_at) VALUES (?1,?2,?3,?4,?5,?6)",
        rusqlite::params![task,old.chain_id,old.attempt_count,old.status,old.record_json,old.updated_at]).unwrap();
    drop(sql);
    let reopened = assemble(root.path(), harness.clone());
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let snapshot = reopened
        .store
        .task_command_snapshot(&task)
        .unwrap()
        .snapshot;
    let result = client.call(methods::EXECUTE_TASK_COMMAND, serde_json::json!({
        "commandId":uuid::Uuid::new_v4(),"taskId":task,"actorId":"local-fixture",
        "clientId":"workspace-authority","clientSessionId":"synthetic",
        "expectedTaskRevision":snapshot.task_revision,"intent":{"kind":"retryRecovery","chainId":"old-chain"}
    })).await;
    evidence(
        "old-recovery-retry",
        serde_json::json!({"reply":format!("{result:?}"),"calls":harness.calls.load(Ordering::SeqCst)}),
    );
    assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
    let after = reopened.store.task_recovery(&task).unwrap().unwrap();
    let record: serde_json::Value = serde_json::from_slice(&after.record_json).unwrap();
    assert_eq!(record["workspaceRepairRevision"], 0);
    assert_eq!(
        after.attempt_count, 0,
        "blocked lineage is not another provider attempt"
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn initial_materialization_is_bound_to_original_pending_run_without_repair_or_receipt_renewal()
 {
    let (_root, core, harness, task) = fixture().await;
    let original = pending(&core, &task, "materializing-run", "run");
    let before_r = original.snapshot.task_revision;
    core.doc_host
        .begin_task_command(original, None, None, None)
        .unwrap();
    let before = core
        .store
        .task_command_receipt("materializing-run")
        .unwrap();
    core.store
        .record_local_executable(
            &task,
            LocalExecutableKind::Command,
            "materializing-run",
            "exact-original-command-row",
            None,
        )
        .unwrap();
    let fence = WorkspaceMaterializationFence {
        expected_workspace_repair_revision: 0,
        original_command_hash: "exact-original-command-row".into(),
    };
    let bad_hash = WorkspaceMaterializationFence {
        expected_workspace_repair_revision: 0,
        original_command_hash: "substituted-command".into(),
    };
    let bad_w = WorkspaceMaterializationFence {
        expected_workspace_repair_revision: 1,
        original_command_hash: fence.original_command_hash.clone(),
    };
    for invalid in [&bad_hash, &bad_w] {
        assert!(
            core.store
                .persist_workspace_materialization(
                    "materializing-run",
                    &task,
                    invalid,
                    &[("materialization-fixture", b"must-not-publish")]
                )
                .is_err()
        );
    }
    core.store
        .persist_workspace_materialization(
            "materializing-run",
            &task,
            &fence,
            &[("materialization-fixture", b"verified-initial-association")],
        )
        .unwrap();
    assert_eq!(
        core.store
            .task_command_receipt("materializing-run")
            .unwrap(),
        before
    );
    assert_eq!(
        core.store
            .task_command_snapshot(&task)
            .unwrap()
            .snapshot
            .task_revision,
        before_r
    );
    assert_eq!(core.store.workspace_repair_revision(&task).unwrap(), 0);
    core.doc_host
        .complete_task_command(
            "materializing-run",
            CommandReceiptStatus::Applied,
            true,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert!(
        core.store
            .persist_workspace_materialization(
                "materializing-run",
                &task,
                &fence,
                &[("materialization-fixture", b"must-not-replay")]
            )
            .is_err()
    );
    evidence(
        "pending-run-materialization",
        serde_json::json!({"beforeR":before_r,
        "afterR":core.store.task_command_snapshot(&task).unwrap().snapshot.task_revision,
        "w":core.store.workspace_repair_revision(&task).unwrap(), "calls":harness.calls.load(Ordering::SeqCst)}),
    );
    assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
    core.shutdown().await;
}
