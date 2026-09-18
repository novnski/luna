//! L06 behavioral coverage: durable task identity, immutable bindings, imports,
//! tombstones, provider-home boundaries, and truthful reconciliation states.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zeron_doc::SessionCommandPayload;
use zeron_engine::{
    EngineCore, HarnessRegistry, TaskIdentityService, WorkspaceHost, WorkspaceHostConfig,
};
use zeron_harness::codex::native::CodexNativeClient;
use zeron_harness::mock::MockHarness;
use zeron_proto::{
    AgentEvent, BindingOrigin, DoneStatus, HarnessId, ImportOwnership, ReconciliationState,
    RunRequest, SandboxLevel, TaskLineage,
};
use zeron_rpc::{RpcService, methods};
use zeron_sync::DocsStore;

struct Fixture {
    _temp: tempfile::TempDir,
    store: Arc<DocsStore>,
    workspace: WorkspaceHost,
    service: TaskIdentityService,
    root: PathBuf,
    device_id: String,
    codex_home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(DocsStore::open(temp.path().join("store")).unwrap());
        let device_id = "l06-device".to_string();
        let workspace = WorkspaceHost::open(
            store.clone(),
            WorkspaceHostConfig {
                device_id: device_id.clone(),
                device_name: "L06 fixture".into(),
                platform: std::env::consts::OS.into(),
                org_id: "l06-org".into(),
                user_id: "l06-user".into(),
                edge: None,
            },
        )
        .unwrap();
        let codex_home = temp.path().join("codex-home");
        let client = CodexNativeClient::new(&codex_home).with_executable(fake_codex());
        let service = TaskIdentityService::open_with_codex_client(
            store.clone(),
            workspace.clone(),
            &device_id,
            client,
        )
        .unwrap();
        Self {
            _temp: temp,
            store,
            workspace,
            service,
            root,
            device_id,
            codex_home,
        }
    }

    fn create_space(&self) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.workspace
            .create_space(
                &id,
                &self.device_id,
                self.root.to_str().unwrap(),
                Some("L06 fixture".into()),
                false,
            )
            .unwrap();
        id
    }

    fn reopen(&self, codex_home: &Path) -> TaskIdentityService {
        TaskIdentityService::open_with_codex_client(
            self.store.clone(),
            self.workspace.clone(),
            &self.device_id,
            CodexNativeClient::new(codex_home).with_executable(fake_codex()),
        )
        .unwrap()
    }
}

fn fake_codex() -> PathBuf {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-codex.sh");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn engine_persists_the_binding_before_session_started_is_published() {
    let temp = tempfile::tempdir().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![
            AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: Vec::new(),
                cwd: "/tmp".into(),
                session_id: "provider-session-before-visible".into(),
                assistant_message_id: "assistant-1".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("provider-session-before-visible".into()),
            },
        ],
    }));
    let core = EngineCore::assemble(
        &temp.path().join("data"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let space_id = uuid::Uuid::new_v4().to_string();
    core.workspace
        .create_space(&space_id, &core.device_id, "/tmp", None, false)
        .unwrap();
    let task_id = uuid::Uuid::new_v4().to_string();
    core.rpc_service()
        .handle(
            methods::MUTATE,
            serde_json::json!({
                "op": "createChat",
                "chatId": &task_id,
                "spaceId": &space_id
            }),
        )
        .await
        .unwrap();
    core.workspace
        .rename_chat(&task_id, "Binding publication fixture")
        .unwrap();
    assert!(
        core.task_identity
            .view(&task_id)
            .unwrap()
            .current_binding
            .is_none()
    );
    let (_, mut events) = core.sessions.subscribe(&task_id, 0).unwrap();
    core.doc_host
        .queue_command(
            &task_id,
            SessionCommandPayload::Run {
                request: RunRequest {
                    prompt: "fixture".into(),
                    harness: Some(HarnessId::Mock),
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: "/tmp".into(),
                    sandbox: SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                    permission_mode: zeron_proto::PermissionMode::Ask,
                    parent_permission_ceiling: None,
                    environment_permission_ceiling: None,
                    permission_policy: None,
                    resume: None,
                    attachments: Vec::new(),
                    worktree: None,
                },
                message_id: "message-1".into(),
            },
        )
        .unwrap();
    let started = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.unwrap();
            if matches!(&event.event, AgentEvent::SessionStarted { .. }) {
                break event;
            }
        }
    })
    .await
    .expect("SessionStarted published");
    assert!(matches!(started.event, AgentEvent::SessionStarted { .. }));
    let binding = core
        .task_identity
        .view(&task_id)
        .unwrap()
        .current_binding
        .expect("binding is durable before publication");
    assert_eq!(binding.provider_task_id, "provider-session-before-visible");
    assert_eq!(binding.provider_id, HarnessId::Mock);
    core.shutdown().await;
}

#[tokio::test]
async fn restart_replays_lineage_dedup_and_immutable_binding_generations() {
    let fixture = Fixture::new();
    let space_id = fixture.create_space();
    let task_a = uuid::Uuid::new_v4().to_string();
    let task_b = uuid::Uuid::new_v4().to_string();
    let lineage = TaskLineage {
        parent_task_id: Some("parent-task".into()),
        source_task_id: Some("fork-source".into()),
        lifecycle_owner_task_id: Some("lifecycle-owner".into()),
    };
    fixture
        .service
        .register_new_task(
            &task_a,
            Some(&space_id),
            None,
            Some(fixture.root.to_str().unwrap()),
            lineage.clone(),
        )
        .unwrap();
    fixture
        .service
        .record_session_binding(
            &task_a,
            HarnessId::Codex,
            "provider-thread-1",
            fixture.root.to_str().unwrap(),
        )
        .unwrap();
    // Re-observing the same tuple updates reconciliation without adding a generation.
    fixture
        .service
        .record_session_binding(
            &task_a,
            HarnessId::Codex,
            "provider-thread-1",
            fixture.root.to_str().unwrap(),
        )
        .unwrap();
    assert_eq!(fixture.service.view(&task_a).unwrap().bindings.len(), 1);

    fixture
        .service
        .register_new_task(
            &task_b,
            Some(&space_id),
            None,
            Some(fixture.root.to_str().unwrap()),
            TaskLineage::default(),
        )
        .unwrap();
    let duplicate = fixture
        .service
        .record_session_binding(
            &task_b,
            HarnessId::Codex,
            "provider-thread-1",
            fixture.root.to_str().unwrap(),
        )
        .unwrap_err();
    assert!(duplicate.to_string().contains("already bound"));

    let replacement = fixture
        .service
        .advance_binding(
            &task_a,
            HarnessId::Codex,
            "provider-thread-2",
            BindingOrigin::HandedOff,
            "synthetic L06 handoff fixture",
        )
        .unwrap();
    assert_eq!(replacement.generation, 2);
    let before_restart = fixture.service.view(&task_a).unwrap();
    assert_eq!(before_restart.task.lineage, lineage);
    assert_eq!(before_restart.workspace.workspace_id, space_id);
    assert_eq!(
        before_restart.bindings[0].provider_task_id,
        "provider-thread-1"
    );
    assert_eq!(
        before_restart.bindings[0].reconciliation.state,
        ReconciliationState::Replaced
    );
    assert_eq!(
        before_restart.bindings[0].replaced_by_binding_id.as_deref(),
        Some(replacement.binding_id.as_str())
    );

    let replayed = fixture.reopen(&fixture.codex_home);
    assert_eq!(replayed.view(&task_a).unwrap(), before_restart);
    assert_eq!(replayed.view(&task_b).unwrap().bindings.len(), 0);

    // A different provider home is a different profile/instance even when the
    // provider task id happens to match.
    let foreign_home = fixture._temp.path().join("other-codex-home");
    let foreign = fixture.reopen(&foreign_home);
    let error = foreign
        .prepare_resume(&task_a, HarnessId::Codex, fixture.root.to_str().unwrap())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("profile/home changed"));
    assert_eq!(
        foreign
            .view(&task_a)
            .unwrap()
            .current_binding
            .unwrap()
            .reconciliation
            .state,
        ReconciliationState::ExternalChange
    );
    fixture.workspace.shutdown();
}

#[tokio::test]
async fn reconciliation_reports_missing_foreign_active_and_unsupported_truthfully() {
    let fixture = Fixture::new();
    for (provider, provider_task, expected) in [
        (
            HarnessId::Codex,
            "missing-thread",
            ReconciliationState::ProviderMissing,
        ),
        (
            HarnessId::Codex,
            "qa-active",
            ReconciliationState::ForeignActive,
        ),
        (
            HarnessId::ClaudeCode,
            "claude-native-id",
            ReconciliationState::Unsupported,
        ),
    ] {
        let task_id = uuid::Uuid::new_v4().to_string();
        fixture
            .service
            .register_new_task(
                &task_id,
                None,
                Some(&fixture.device_id),
                Some("/tmp"),
                TaskLineage::default(),
            )
            .unwrap();
        fixture
            .service
            .record_session_binding(&task_id, provider, provider_task, "/tmp")
            .unwrap();
        let view = fixture
            .service
            .reconcile_task(&task_id, false)
            .await
            .unwrap();
        assert_eq!(
            view.current_binding.unwrap().reconciliation.state,
            expected,
            "{provider:?}/{provider_task}"
        );
    }
    let task_id = uuid::Uuid::new_v4().to_string();
    let missing_workspace = fixture._temp.path().join("removed-workspace");
    fixture
        .service
        .register_new_task(
            &task_id,
            None,
            Some(&fixture.device_id),
            Some(missing_workspace.to_str().unwrap()),
            TaskLineage::default(),
        )
        .unwrap();
    fixture
        .service
        .record_session_binding(
            &task_id,
            HarnessId::Codex,
            "qa-import",
            missing_workspace.to_str().unwrap(),
        )
        .unwrap();
    let missing_environment = fixture
        .service
        .reconcile_task(&task_id, false)
        .await
        .unwrap();
    assert_eq!(
        missing_environment
            .current_binding
            .unwrap()
            .reconciliation
            .state,
        ReconciliationState::EnvironmentMissing
    );
    fixture.workspace.shutdown();
}

#[tokio::test]
async fn import_is_deduplicated_bound_before_chat_visibility_and_tombstoned() {
    let fixture = Fixture::new();
    let already_bound = uuid::Uuid::new_v4().to_string();
    fixture
        .service
        .register_new_task(
            &already_bound,
            None,
            Some(&fixture.device_id),
            Some("/tmp"),
            TaskLineage::default(),
        )
        .unwrap();
    fixture
        .service
        .record_session_binding(&already_bound, HarnessId::Codex, "qa-bound", "/tmp")
        .unwrap();
    let ambiguous = fixture
        .service
        .preview_import(
            HarnessId::Codex,
            "qa-bound",
            ImportOwnership::ProviderOwned,
            None,
            Some(&fixture.device_id),
        )
        .await
        .unwrap();
    assert!(!ambiguous.adoptable);
    assert_eq!(
        ambiguous.reconciliation_state,
        ReconciliationState::AmbiguousDuplicate
    );

    let preview = fixture
        .service
        .preview_import(
            HarnessId::Codex,
            "qa-import",
            ImportOwnership::ProviderOwned,
            None,
            Some(&fixture.device_id),
        )
        .await
        .unwrap();
    assert!(preview.adoptable, "{:?}", preview.conflict);
    let events_before = fixture.service.event_count();
    let imported = fixture
        .service
        .adopt_preview(&preview.preview_id, None)
        .await
        .unwrap();
    let task_id = imported.task.task_id.clone();
    assert!(uuid::Uuid::parse_str(&task_id).is_ok());
    assert_ne!(task_id, preview.provider_task.provider_task_id);
    assert!(fixture.service.event_count() >= events_before + 3);
    assert_eq!(imported.bindings.len(), 1);
    assert_eq!(
        imported.current_binding.as_ref().unwrap().origin,
        BindingOrigin::ProviderImported
    );
    assert!(fixture.workspace.chat(&task_id).unwrap().is_some());
    assert_eq!(fixture.workspace.chat_harness_session(&task_id), None);

    let second = fixture
        .service
        .preview_import(
            HarnessId::Codex,
            "qa-import",
            ImportOwnership::ProviderOwned,
            None,
            Some(&fixture.device_id),
        )
        .await
        .unwrap();
    assert_eq!(second.existing_task_id.as_deref(), Some(task_id.as_str()));
    let duplicate = fixture
        .service
        .adopt_preview(&second.preview_id, None)
        .await
        .unwrap();
    assert_eq!(duplicate.task.task_id, task_id);
    assert_eq!(duplicate.bindings.len(), 1);

    fixture.service.tombstone_task(&task_id).unwrap();
    let after_delete = fixture
        .service
        .preview_import(
            HarnessId::Codex,
            "qa-import",
            ImportOwnership::ProviderOwned,
            None,
            Some(&fixture.device_id),
        )
        .await
        .unwrap();
    assert!(!after_delete.adoptable);
    assert_eq!(
        after_delete.reconciliation_state,
        ReconciliationState::Replaced
    );
    assert!(after_delete.conflict.unwrap().contains("tombstone"));

    let active = fixture
        .service
        .preview_import(
            HarnessId::Codex,
            "qa-active",
            ImportOwnership::LunaAdopted,
            None,
            Some(&fixture.device_id),
        )
        .await
        .unwrap();
    assert!(!active.adoptable);
    assert_eq!(
        active.reconciliation_state,
        ReconciliationState::ForeignActive
    );
    assert!(active.conflict.unwrap().contains("currently active"));

    let replayed = fixture.reopen(&fixture.codex_home);
    assert!(replayed.view(&task_id).unwrap().task.tombstoned);
    fixture.workspace.shutdown();
}
