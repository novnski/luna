//! L29 owner-side admission through the actual chat2 sink and live drain.
//! Every profile, provider home, command and document is synthetic.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_doc::{
    QueueHoldReason, QueuedMessage, SessionCommandEntry, SessionCommandPayload,
    SessionCommandStatus, SessionDoc,
};
use luna_engine::chat2_host::EngineChatSink;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode,
};
use luna_sync::chat_client::ChatDocSink;

const CHAT: &str = "synthetic-l29-chat";

struct CountingHarness(Arc<AtomicUsize>);

#[async_trait]
impl Harness for CountingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Synthetic L29"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
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
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(futures::stream::iter([
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "synthetic".into(),
                tools: vec![],
                cwd: request.cwd,
                session_id: "synthetic-run".into(),
                assistant_message_id: "synthetic-answer".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("synthetic-run".into()),
            }),
        ])
        .boxed())
    }
}

fn synthetic_accounts(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    }
}

fn core(root: &Path, runs: Arc<AtomicUsize>) -> EngineCore {
    let accounts = synthetic_accounts(root);
    accounts.disable_claude_keychain_fallback().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CountingHarness(runs)));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("data"), "l29-org", "l29-user"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap();
    core.workspace
        .create_chat(CHAT, None, Some(&core.device_id), None, None)
        .unwrap();
    core.workspace
        .rename_chat(CHAT, "Synthetic L29 task")
        .unwrap();
    core
}

fn run_request(text: &str) -> RunRequest {
    RunRequest {
        review: None,
        input_items: Vec::new(),
        prompt: text.into(),
        harness: Some(HarnessId::Mock),
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: String::new(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: Default::default(),
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: Vec::new(),
        worktree: None,
    }
}

async fn wait_for_runs(runs: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while runs.load(Ordering::SeqCst) < expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("synthetic harness dispatch");
}

#[tokio::test]
async fn foreign_rows_cannot_run_from_an_open_chat_or_replay() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let core = core(root.path(), runs.clone());
    let handle = core.doc_host.open(CHAT).unwrap();
    let raw = loro::LoroDoc::new();
    raw.import(&handle.doc().export_snapshot().unwrap())
        .unwrap();
    let foreign = SessionDoc::from_doc(raw);
    let before = foreign.doc().oplog_vv();
    foreign
        .queue_command(&SessionCommandEntry {
            id: "foreign-run".into(),
            payload: SessionCommandPayload::Run {
                request: run_request("foreign command"),
                message_id: "foreign-message".into(),
            },
            issued_by: core.device_id.clone(), // A CRDT author string is forgeable.
            issued_at: chrono::Utc::now().timestamp_millis(),
            based_on: None,
            expires_at: Some(i64::MAX),
            status: SessionCommandStatus::Pending,
            resolution: None,
        })
        .unwrap();
    for (id, payload) in [
        (
            "foreign-steer",
            SessionCommandPayload::Steer {
                attachments: Vec::new(),
                input_items: Vec::new(),
                prompt: "foreign steer".into(),
                message_id: None,
            },
        ),
        ("foreign-interrupt", SessionCommandPayload::Interrupt {}),
    ] {
        foreign
            .queue_command(&SessionCommandEntry {
                id: id.into(),
                payload,
                issued_by: core.device_id.clone(),
                issued_at: chrono::Utc::now().timestamp_millis(),
                based_on: None,
                expires_at: Some(i64::MAX),
                status: SessionCommandStatus::Pending,
                resolution: None,
            })
            .unwrap();
    }
    foreign
        .push_queued(&QueuedMessage::new(
            "foreign-queue",
            "foreign queued send",
            core.device_id.clone(),
        ))
        .unwrap();
    let update = foreign
        .doc()
        .export(loro::ExportMode::updates(&before))
        .unwrap();
    let sink = EngineChatSink::new(&handle.doc_arc(), core.store.clone(), CHAT);
    assert!(matches!(
        sink.apply_row(&update, 1),
        luna_sync::chat_client::RowImportOutcome::Applied
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "foreign command or queue dispatched"
    );
    let rejected = handle.doc().read_commands().unwrap();
    assert_eq!(rejected.len(), 3);
    assert!(
        rejected
            .iter()
            .all(|row| row.status == SessionCommandStatus::Rejected)
    );
    assert!(rejected.iter().all(|row| {
        core.doc_host
            .task_command_receipt(&row.id)
            .unwrap()
            .is_none()
    }));
    let queued = handle.doc().read_queue().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].hold_reason,
        Some(QueueHoldReason::RemoteAuthorityRequired)
    );
    assert!(matches!(
        sink.apply_replay_row(&update, 2),
        luna_sync::chat_client::RowImportOutcome::Applied
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 0, "replay dispatched");
    sink.apply_checkpoint(&foreign.export_snapshot().unwrap(), 3)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 0, "checkpoint dispatched");
    core.shutdown().await;
    drop(sink);
    drop(handle);
    drop(core);
    let reopened = self::core(root.path(), runs.clone());
    let reopened_handle = reopened.doc_host.open(CHAT).unwrap();
    reopened.doc_host.drain_commands(&reopened_handle).await;
    reopened.doc_host.drain_queue(&reopened_handle).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "restart dispatched foreign rows"
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn direct_relay_ingest_cannot_bypass_owner_authority() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let core = core(root.path(), runs.clone());
    let row = SessionCommandEntry {
        id: "foreign-relay-run".into(),
        payload: SessionCommandPayload::Run {
            request: run_request("foreign relay"),
            message_id: "foreign-relay-message".into(),
        },
        issued_by: core.device_id.clone(),
        issued_at: chrono::Utc::now().timestamp_millis(),
        based_on: None,
        expires_at: Some(i64::MAX),
        status: SessionCommandStatus::Pending,
        resolution: None,
    };
    assert!(
        core.doc_host
            .ingest_relayed_command(CHAT, row)
            .await
            .is_err()
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    core.shutdown().await;
}

#[tokio::test]
async fn a_foreign_edit_cannot_reuse_a_locally_authorized_queue_id() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let core = core(root.path(), runs.clone());
    let id = core
        .doc_host
        .queue_message(CHAT, "local held message", Vec::new())
        .unwrap();
    let handle = core.doc_host.open(CHAT).unwrap();
    let raw = loro::LoroDoc::new();
    raw.import(&handle.doc().export_snapshot().unwrap())
        .unwrap();
    let foreign = SessionDoc::from_doc(raw);
    let before = foreign.doc().oplog_vv();
    foreign
        .set_queued_text(
            &id,
            "foreign substituted prompt",
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    let update = foreign
        .doc()
        .export(loro::ExportMode::updates(&before))
        .unwrap();
    let sink = EngineChatSink::new(&handle.doc_arc(), core.store.clone(), CHAT);
    assert!(matches!(
        sink.apply_row(&update, 1),
        luna_sync::chat_client::RowImportOutcome::Applied
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    let row = handle.doc().read_queue().unwrap().remove(0);
    assert_eq!(row.id, id);
    assert_eq!(row.text, "foreign substituted prompt");
    assert_eq!(
        row.hold_reason,
        Some(QueueHoldReason::RemoteAuthorityRequired)
    );
    core.shutdown().await;
}

#[tokio::test]
async fn stale_viewer_queue_mutations_cannot_rewrite_the_owners_claimed_evidence() {
    let root = tempfile::tempdir().unwrap();
    let host_runs = Arc::new(AtomicUsize::new(0));
    let viewer_runs = Arc::new(AtomicUsize::new(0));
    let host = core(&root.path().join("host"), host_runs.clone());
    let viewer = core(&root.path().join("viewer"), viewer_runs.clone());
    viewer
        .workspace
        .set_chat_host(CHAT, &host.device_id)
        .unwrap();
    let host_doc = host.doc_host.open(CHAT).unwrap();
    host.doc_host.pause_all_queues();
    let id = host
        .doc_host
        .queue_message(CHAT, "frozen provider prompt", vec![])
        .unwrap();
    host.doc_host
        .queue_message(CHAT, "held sibling", vec![])
        .unwrap();

    // Both engines see Held. Disconnect their document bridge before the
    // owner claims delivery, so the viewer must not trust its stale status.
    let viewer_doc = viewer.doc_host.open(CHAT).unwrap();
    let viewer_sink = EngineChatSink::new(&viewer_doc.doc_arc(), viewer.store.clone(), CHAT);
    assert!(matches!(
        viewer_sink.apply_row(&host_doc.doc().export_snapshot().unwrap(), 1),
        luna_sync::chat_client::RowImportOutcome::Applied
    ));
    assert!(matches!(
        host_doc
            .doc()
            .claim_queued(
                &id,
                1,
                "owner-delivery",
                luna_doc::QueueClaimMode::NextTurn,
                1,
            )
            .unwrap(),
        luna_doc::QueueClaimOutcome::Claimed(_)
    ));
    let original = host_doc.doc().read_queue().unwrap();
    let client = luna_rpc::memory_client(viewer.rpc_service());
    let edit = client
        .call(
            luna_rpc::methods::UPDATE_QUEUED_MESSAGE,
            serde_json::json!({
                "chatId": CHAT, "id": id, "text": "stale viewer replacement",
            }),
        )
        .await;
    let moved = client
        .call(
            luna_rpc::methods::MOVE_QUEUED_MESSAGE,
            serde_json::json!({
                "chatId": CHAT, "id": id, "toIndex": 1,
            }),
        )
        .await;

    // Reconnect through the actual host sink. Authority must be checked at
    // the mutation boundary, before an unsafe update enters the CRDT stream.
    // Include the viewer engine's own initialization operations too; a
    // delta from after its first import would omit those causal dependencies.
    let update = viewer_doc.doc().export_snapshot().unwrap();
    let host_sink = EngineChatSink::new(&host_doc.doc_arc(), host.store.clone(), CHAT);
    assert!(matches!(
        host_sink.apply_row(&update, 1),
        luna_sync::chat_client::RowImportOutcome::Applied
    ));
    assert_eq!(
        host_doc.doc().read_queue().unwrap(),
        original,
        "stale viewer changed claimed evidence: edit={edit:?}, move={moved:?}"
    );
    assert!(
        edit.is_err() && moved.is_err(),
        "non-owner writes must fail closed"
    );
    assert_eq!(host_runs.load(Ordering::SeqCst), 0);
    assert_eq!(viewer_runs.load(Ordering::SeqCst), 0);
    host.shutdown().await;
    viewer.shutdown().await;
}

#[tokio::test]
async fn locally_accepted_command_and_queue_still_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let core = core(root.path(), runs.clone());
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: run_request("local command"),
                message_id: "local-command-message".into(),
            },
        )
        .unwrap();
    wait_for_runs(&runs, 1).await;
    core.doc_host
        .queue_message(CHAT, "local queue", Vec::new())
        .unwrap();
    wait_for_runs(&runs, 2).await;
    core.shutdown().await;
}
