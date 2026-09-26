//! Failure-first v2 workspace identity and local-only repair integration coverage.
//! The original three-case workspace_identity_roots target stays byte-for-byte frozen.
//! All provider state is synthetic and native metadata discovery is disabled.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_proto::{CommandReceipt, CommandReceiptStatus, HarnessId, TaskIdentityView};
use luna_rpc::{RpcClient, methods};
use serde_json::{Value, json};

struct Fixture {
    _temp: tempfile::TempDir,
    data: PathBuf,
    root_a: PathBuf,
    root_b: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("disposable identity fixture");
        let base = temp.path().canonicalize().unwrap();
        let root_a = base.join("root-a");
        let root_b = base.join("root-b");
        std::fs::create_dir(&root_a).unwrap();
        std::fs::create_dir(&root_b).unwrap();
        Self {
            data: base.join("data"),
            repo: base.join("repo"),
            root_a,
            root_b,
            _temp: temp,
        }
    }

    fn open(&self) -> EngineCore {
        self.open_with_registry(Arc::new(HarnessRegistry::new()))
    }

    fn open_with_registry(&self, registry: Arc<HarnessRegistry>) -> EngineCore {
        let accounts = AgentAccountsConfig {
            data_dir: self.data.join("synthetic-provider-state"),
            claude_config_dir: self.data.join("synthetic-claude"),
            claude_config_file: self.data.join("synthetic-claude.json"),
            codex_home: self.data.join("synthetic-codex"),
        };
        accounts.disable_claude_keychain_fallback().unwrap();
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            EngineProfile::local(&self.data).unwrap(),
            registry,
            HarnessId::Mock,
            None,
            accounts,
        )
        .expect("hermetic local engine")
    }

    fn init_repo(&self) {
        std::fs::create_dir(&self.repo).unwrap();
        git(&self.repo, &["init", "-q", "-b", "main"]);
        git(
            &self.repo,
            &["config", "core.hooksPath", "disabled-qa-hooks"],
        );
        std::fs::write(self.repo.join("file.txt"), "fixture\n").unwrap();
        git(&self.repo, &["add", "."]);
        git(
            &self.repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "fixture",
            ],
        );
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "LUNA Identity Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "LUNA Identity Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

async fn create(
    client: &RpcClient,
    task: &str,
    device: &str,
    space: Option<&str>,
    cwd: &Path,
) -> CommandReceipt {
    let mut intent = json!({ "kind": "createChat", "cwd": cwd });
    if let Some(space) = space {
        intent["spaceId"] = json!(space);
    } else {
        intent["deviceId"] = json!(device);
    }
    let receipt: CommandReceipt = client
        .call_as(
            methods::EXECUTE_TASK_COMMAND,
            json!({
                "commandId": uuid::Uuid::new_v4().to_string(),
                "taskId": task,
                "actorId": "identity-fixture",
                "clientId": "identity-fixture",
                "clientSessionId": "identity-fixture",
                "intent": intent,
            }),
        )
        .await
        .expect("CreateChat RPC");
    assert_eq!(receipt.status, CommandReceiptStatus::Applied, "{receipt:?}");
    receipt
}

async fn identity(client: &RpcClient, task: &str) -> TaskIdentityView {
    let view: Option<TaskIdentityView> = client
        .call_as(methods::GET_TASK_IDENTITY, json!({ "taskId": task }))
        .await
        .expect("GetTaskIdentity RPC");
    view.expect("UUID must enter managed task identity")
}

async fn close(core: EngineCore, client: RpcClient) {
    drop(client);
    assert!(
        core.shutdown().await,
        "engine shutdown must flush the identity snapshot"
    );
    drop(core);
    tokio::task::yield_now().await;
}

fn evidence(case: &str, value: Value) -> PathBuf {
    let directory = std::env::var_os("LUNA_WORKSPACE_IDENTITY_REPORT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/workspace-identity-roots")
        });
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join(format!("{case}-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&file, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    eprintln!("identity evidence: {}", file.display());
    file
}

#[path = "support/workspace_identity_repair_contract.rs"]
mod repair_contract;

fn synthetic_observation(cwd: &Path) -> luna_proto::ProviderTaskObservation {
    luna_proto::ProviderTaskObservation {
        provider_task_id: "synthetic-historical-thread".into(),
        name: Some("Historical fixture".into()),
        cwd: cwd.to_string_lossy().into_owned(),
        status: "notLoaded".into(),
        archived: Some(true),
        updated_at: Some(1),
        cli_version: Some("synthetic".into()),
        project_id: None,
        ephemeral: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_import_preview_is_display_only_and_preserves_stored_history() {
    let fixture = Fixture::new();
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let task = uuid::Uuid::new_v4().to_string();
    create(&client, &task, &core.device_id, None, &fixture.root_a).await;
    let doc = luna_doc::SessionDoc::init(&task).unwrap();
    doc.push_message(&luna_doc::SessionMessageEntry {
        id: "stored-user-message".into(),
        role: luna_doc::MessageRole::User,
        parts: vec![luna_proto::MessagePart::Text {
            id: "stored-part".into(),
            text: "Preserved synthetic history".into(),
        }],
        created_at: 1,
        device_id: core.device_id.clone(),
        status: Some(luna_doc::MessageStatus::Complete),
        continuation_of: None,
    })
    .unwrap();
    let history = doc.export_snapshot().unwrap();
    core.store.save_snapshot(&task, &history).unwrap();
    let before_identity = identity(&client, &task).await;
    let before_log = core
        .store
        .load_snapshot("luna-task-identity-events-v1")
        .unwrap();
    let missing = fixture.root_b.join("deleted-history-root");
    let preview = core
        .task_identity
        .preview_import_observation_for_tests(
            synthetic_observation(&missing),
            luna_proto::ImportOwnership::ProviderOwned,
            None,
            None,
        )
        .unwrap();
    let refused = core
        .task_identity
        .adopt_preview(&preview.preview_id, None)
        .await;
    assert!(refused.is_err(), "missing-root preview must not be adopted");
    let valid = core
        .task_identity
        .preview_import_observation_for_tests(
            synthetic_observation(&fixture.root_b),
            luna_proto::ImportOwnership::ProviderOwned,
            None,
            None,
        )
        .unwrap();
    let after_identity = identity(&client, &task).await;
    let after_log = core
        .store
        .load_snapshot("luna-task-identity-events-v1")
        .unwrap();
    let after_history = core.store.load_snapshot(&task).unwrap();
    evidence(
        "historical-import-preview",
        json!({"missing":preview,"valid":valid,
        "adoptionError":refused.err().map(|error|error.to_string()),
        "beforeIdentity":before_identity,"afterIdentity":after_identity,
        "identityLogUnchanged":before_log==after_log,"historyUnchanged":after_history.as_ref()==Some(&history)}),
    );
    close(core, client).await;
    assert_eq!(
        preview.reconciliation_state,
        luna_proto::ReconciliationState::EnvironmentMissing
    );
    assert!(!preview.adoptable);
    assert!(
        preview
            .workspace
            .workspace_id
            .starts_with("unresolved:missing:")
    );
    assert_eq!(preview.workspace.current_path, missing.to_string_lossy());
    assert_eq!(
        preview.provider_task.name.as_deref(),
        Some("Historical fixture")
    );
    assert!(valid.adoptable);
    assert!(valid.workspace.workspace_id.starts_with("workspace:v2:"));
    assert_eq!(after_identity, before_identity);
    assert_eq!(after_log, before_log);
    assert_eq!(after_history, Some(history));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_preview_does_not_mislabel_file_or_dangling_alias_as_missing_root() {
    let fixture = Fixture::new();
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let file = fixture.root_a.join("ordinary-file");
    std::fs::write(&file, b"fixture").unwrap();
    let alias = fixture.root_a.join("dangling-alias");
    std::os::unix::fs::symlink(fixture.root_b.join("absent"), &alias).unwrap();
    for path in [&file, &alias] {
        let result = core.task_identity.preview_import_observation_for_tests(
            synthetic_observation(path),
            luna_proto::ImportOwnership::ProviderOwned,
            None,
            None,
        );
        evidence(
            "historical-import-root-refusal",
            json!({"path":path,"error":result.as_ref().err().map(ToString::to_string)}),
        );
        assert!(
            result.is_err(),
            "non-directory or ambiguous alias must not be called EnvironmentMissing"
        );
    }
    close(core, client).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_import_cwd_does_not_hide_an_ambiguous_selected_project() {
    let fixture = Fixture::new();
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let alias = fixture.root_a.join("selected-project-alias");
    std::os::unix::fs::symlink(&fixture.root_b, &alias).unwrap();
    let project = uuid::Uuid::new_v4().to_string();
    client
        .call(
            methods::MUTATE,
            json!({"op":"createSpace","spaceId":project,
        "deviceId":core.device_id,"path":alias}),
        )
        .await
        .unwrap();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(fixture.root_b.join("missing-project-root"), &alias).unwrap();
    let provider_cwd = fixture.root_a.join("missing-native-root");
    let original = core
        .store
        .load_snapshot("luna-task-identity-events-v1")
        .unwrap();
    let result = core.task_identity.preview_import_observation_for_tests(
        synthetic_observation(&provider_cwd),
        luna_proto::ImportOwnership::ProviderOwned,
        Some(&project),
        None,
    );
    evidence(
        "ambiguous-import-project",
        json!({"project":project,"projectPath":alias,
        "providerCwd":provider_cwd,"error":result.as_ref().err().map(ToString::to_string)}),
    );
    assert!(
        result.is_err(),
        "dangling selected project cannot be collapsed into EnvironmentMissing"
    );
    assert_eq!(
        core.store
            .load_snapshot("luna-task-identity-events-v1")
            .unwrap(),
        original
    );
    close(core, client).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_aliases_share_only_the_same_environment_and_source_scope() {
    let fixture = Fixture::new();
    let alias = fixture.root_a.parent().unwrap().join("root-alias");
    std::os::unix::fs::symlink(&fixture.root_a, &alias).unwrap();
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let a = uuid::Uuid::new_v4().to_string();
    let b = uuid::Uuid::new_v4().to_string();
    create(&client, &a, &core.device_id, None, &fixture.root_a).await;
    create(&client, &b, &core.device_id, None, &alias).await;
    let first = identity(&client, &a).await;
    let second = identity(&client, &b).await;
    let space = uuid::Uuid::new_v4().to_string();
    client
        .call(
            methods::MUTATE,
            json!({"op":"createSpace", "spaceId":space,
        "deviceId":core.device_id, "path":fixture.root_a}),
        )
        .await
        .unwrap();
    let project_task = uuid::Uuid::new_v4().to_string();
    create(
        &client,
        &project_task,
        &core.device_id,
        Some(&space),
        &fixture.root_a,
    )
    .await;
    let project = identity(&client, &project_task).await;
    evidence(
        "canonical-root-scope",
        json!({"first":first,"alias":second,"project":project}),
    );
    close(core, client).await;
    assert_eq!(
        first.workspace.current_path,
        fixture.root_a.to_string_lossy()
    );
    assert_eq!(second.workspace.current_path, first.workspace.current_path);
    assert_eq!(second.workspace.workspace_id, first.workspace.workspace_id);
    assert!(first.workspace.workspace_id.starts_with("workspace:v2:"));
    assert_ne!(project.workspace.workspace_id, first.workspace.workspace_id);
    assert_eq!(
        project.workspace.source_space_id.as_deref(),
        Some(space.as_str())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ambiguous_relative_creation_is_refused_without_identity_side_effects() {
    let fixture = Fixture::new();
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let original = core
        .store
        .load_snapshot("luna-task-identity-events-v1")
        .unwrap();
    for cwd in ["", ".", "relative-folder"] {
        let task = uuid::Uuid::new_v4().to_string();
        let receipt: CommandReceipt = client
            .call_as(
                methods::EXECUTE_TASK_COMMAND,
                json!({
                    "commandId":uuid::Uuid::new_v4().to_string(),"taskId":task,
                    "intent":{"kind":"createChat","deviceId":core.device_id,"cwd":cwd}
                }),
            )
            .await
            .unwrap();
        let view: Option<TaskIdentityView> = client
            .call_as(methods::GET_TASK_IDENTITY, json!({"taskId":task}))
            .await
            .unwrap();
        evidence(
            "relative-root-refusal",
            json!({"cwd":cwd,"receipt":receipt,"identity":view}),
        );
        assert!(receipt.status.is_terminal() && receipt.status != CommandReceiptStatus::Applied);
        assert!(
            receipt
                .reason
                .as_ref()
                .unwrap()
                .message
                .contains("absolute")
        );
        assert!(view.is_none());
        assert!(core.workspace.chat(&task).unwrap().is_none());
        assert_eq!(
            core.store
                .load_snapshot("luna-task-identity-events-v1")
                .unwrap(),
            original
        );
    }
    close(core, client).await;
}

async fn nested_project_move(
    fixture: &Fixture,
    core: &EngineCore,
    client: &RpcClient,
    bound: bool,
) -> (String, String, String, TaskIdentityView, TaskIdentityView) {
    let nested = fixture.root_a.join("sub");
    std::fs::create_dir(&nested).unwrap();
    let origin = uuid::Uuid::new_v4().to_string();
    let parent = uuid::Uuid::new_v4().to_string();
    for (space, path) in [(&origin, &nested), (&parent, &fixture.root_a)] {
        client
            .call(
                methods::MUTATE,
                json!({"op":"createSpace","spaceId":space,
            "deviceId":core.device_id,"path":path}),
            )
            .await
            .unwrap();
    }
    let task = uuid::Uuid::new_v4().to_string();
    create(client, &task, &core.device_id, Some(&origin), &nested).await;
    if bound {
        core.task_identity
            .record_session_binding(
                &task,
                HarnessId::Mock,
                "synthetic-move-session",
                nested.to_str().unwrap(),
            )
            .unwrap();
    }
    let before = identity(client, &task).await;
    let state: luna_proto::TaskCommandRead = client
        .call_as(methods::GET_TASK_COMMAND_SNAPSHOT, json!({"taskId":task}))
        .await
        .unwrap();
    let moved: CommandReceipt = client
        .call_as(
            methods::EXECUTE_TASK_COMMAND,
            json!({
                "commandId":uuid::Uuid::new_v4().to_string(),"taskId":task,
                "expectedTaskRevision":state.snapshot.task_revision,
                "expectedBindingGeneration":state.snapshot.binding_generation,
                "intent":{"kind":"moveProject","destinationSpaceId":parent}
            }),
        )
        .await
        .unwrap();
    let after = identity(client, &task).await;
    evidence(
        "nested-project-move",
        json!({"before":before,"after":after,
        "move":moved,"chat":core.workspace.chat(&task).unwrap(),"origin":origin,"currentProject":parent}),
    );
    assert_eq!(moved.status, CommandReceiptStatus::Applied);
    assert_eq!(
        moved.snapshot.task_revision,
        state.snapshot.task_revision + 1
    );
    assert_eq!(
        core.workspace
            .chat(&task)
            .unwrap()
            .unwrap()
            .space_id
            .as_deref(),
        Some(parent.as_str())
    );
    (task, origin, parent, before, after)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_grouping_move_preserves_execution_identity_and_binding() {
    let fixture = Fixture::new();
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let (task, origin, _, before, after) =
        nested_project_move(&fixture, &core, &client, true).await;
    assert_eq!(after.workspace, before.workspace);
    assert_eq!(
        after.workspace.source_space_id.as_deref(),
        Some(origin.as_str())
    );
    let mut old_binding = serde_json::to_value(before.current_binding.as_ref().unwrap()).unwrap();
    let mut new_binding = serde_json::to_value(after.current_binding.as_ref().unwrap()).unwrap();
    for value in [&mut old_binding, &mut new_binding] {
        value.as_object_mut().unwrap().remove("reconciliation");
        value.as_object_mut().unwrap().remove("lastObservedAtMs");
    }
    assert_eq!(
        new_binding, old_binding,
        "grouping must preserve binding identity and provenance"
    );
    assert_eq!(core.store.workspace_repair_revision(&task).unwrap(), 0);
    core.task_identity
        .validate_explicit_resume(
            &task,
            HarnessId::Mock,
            fixture.root_a.join("sub").to_str().unwrap(),
            "synthetic-move-session",
        )
        .unwrap();
    close(core, client).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_unbound_repair_after_project_move_preserves_origin_and_current_group() {
    let fixture = Fixture::new();
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let (task, origin, current, _, _) = nested_project_move(&fixture, &core, &client, false).await;
    let store = core.store.clone();
    close(core, client).await;
    let mut log: Value = serde_json::from_slice(
        &store
            .load_snapshot("luna-task-identity-events-v1")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    log["schemaVersion"] = json!(1);
    for event in log["events"].as_array_mut().unwrap() {
        let field = match event["type"].as_str() {
            Some("workspaceObserved") => "workspace",
            Some("taskRegistered") => "task",
            _ => continue,
        };
        event[field]["schemaVersion"] = json!(1);
        event[field]["workspaceId"] = json!(origin);
    }
    store
        .save_snapshot(
            "luna-task-identity-events-v1",
            &serde_json::to_vec(&log).unwrap(),
        )
        .unwrap();
    drop(store);
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let state: luna_proto::TaskCommandRead = client
        .call_as(methods::GET_TASK_COMMAND_SNAPSHOT, json!({"taskId":task}))
        .await
        .unwrap();
    let plan = client
        .call(
            "PreviewWorkspaceIdentityRepair",
            json!({"taskId":task,"providerProof":"localOnly"}),
        )
        .await
        .unwrap();
    evidence(
        "legacy-moved-project-preview",
        json!({"plan":plan,"origin":origin,"currentProject":current}),
    );
    assert_eq!(plan["status"], "ready");
    let receipt:CommandReceipt=client.call_as(methods::EXECUTE_TASK_COMMAND,json!({
        "commandId":uuid::Uuid::new_v4().to_string(),"taskId":task,"expectedTaskRevision":state.snapshot.task_revision,
        "intent":{"kind":"repairWorkspaceIdentity","previewId":plan["previewId"],"evidenceDigest":plan["evidenceDigest"]}
    })).await.unwrap();
    let view = identity(&client, &task).await;
    let chat = core.workspace.chat(&task).unwrap().unwrap();
    evidence(
        "legacy-moved-project-repaired",
        json!({"receipt":receipt,"identity":view,"chat":chat}),
    );
    close(core, client).await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
    assert_eq!(
        view.workspace.source_space_id.as_deref(),
        Some(origin.as_str())
    );
    assert_eq!(chat.space_id.as_deref(), Some(current.as_str()));
    assert_eq!(
        view.workspace.current_path,
        fixture.root_a.join("sub").to_string_lossy()
    );
}

/// Construct the old v1 collision in a disposable source project, then reopen
/// the actual EngineCore/RegistryDoc from its persisted snapshots. The folder
/// variant changes the same space ID's path by a later LWW upsert; the Git
/// variant keeps the source project and uses two registered linked worktrees.
async fn legacy_project_collision(
    linked_worktrees: bool,
) -> (Fixture, EngineCore, RpcClient, String, String) {
    let fixture = Fixture::new();
    if linked_worktrees {
        fixture.init_repo();
        for (name, root) in [("qa-a", &fixture.root_a), ("qa-b", &fixture.root_b)] {
            git(
                &fixture.repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    name,
                    root.to_str().unwrap(),
                    "HEAD",
                ],
            );
        }
    }
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let space = uuid::Uuid::new_v4().to_string();
    let project_root = if linked_worktrees {
        &fixture.repo
    } else {
        &fixture.root_a
    };
    client
        .call(
            methods::MUTATE,
            json!({"op":"createSpace","spaceId":space,
        "deviceId":core.device_id,"path":project_root,"gitDetected":linked_worktrees}),
        )
        .await
        .unwrap();
    let task_a = uuid::Uuid::new_v4().to_string();
    let task_b = uuid::Uuid::new_v4().to_string();
    create(
        &client,
        &task_a,
        &core.device_id,
        Some(&space),
        &fixture.root_a,
    )
    .await;
    create(
        &client,
        &task_b,
        &core.device_id,
        Some(&space),
        &fixture.root_b,
    )
    .await;
    let store = core.store.clone();
    let device = core.device_id.clone();
    close(core, client).await;

    let mut identity: Value = serde_json::from_slice(
        &store
            .load_snapshot("luna-task-identity-events-v1")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    identity["schemaVersion"] = json!(1);
    for event in identity["events"].as_array_mut().unwrap() {
        let field = match event["type"].as_str() {
            Some("workspaceObserved") => "workspace",
            Some("taskRegistered") => "task",
            _ => continue,
        };
        event[field]["schemaVersion"] = json!(1);
        event[field]["workspaceId"] = json!(space);
    }
    store
        .save_snapshot(
            "luna-task-identity-events-v1",
            &serde_json::to_vec(&identity).unwrap(),
        )
        .unwrap();
    if !linked_worktrees {
        let raw = store
            .load_snapshot(luna_doc::REGISTRY_DOC_ID)
            .unwrap()
            .unwrap();
        let mut registry = luna_doc::RegistryDoc::from_bytes(&raw, &device).unwrap();
        let mut selected = registry.space(&space).unwrap().unwrap();
        selected.path = fixture.root_b.to_string_lossy().into_owned();
        registry.upsert_space(&selected).unwrap();
        store
            .save_snapshot(luna_doc::REGISTRY_DOC_ID, &registry.to_bytes().unwrap())
            .unwrap();
    }
    drop(store);
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    (fixture, core, client, task_a, space)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lww_space_retarget_cannot_authorize_an_unrelated_legacy_root() {
    let (fixture, core, client, task, space) = legacy_project_collision(false).await;
    let before = identity(&client, &task).await;
    let before_snapshot: luna_proto::TaskCommandRead = client
        .call_as(methods::GET_TASK_COMMAND_SNAPSHOT, json!({"taskId":task}))
        .await
        .unwrap();
    let before_w = core.store.workspace_repair_revision(&task).unwrap();
    let before_log = core
        .store
        .load_snapshot("luna-task-identity-events-v1")
        .unwrap();
    let plan = client
        .call(
            "PreviewWorkspaceIdentityRepair",
            json!({"taskId":task,"providerProof":"localOnly"}),
        )
        .await
        .unwrap();
    evidence(
        "legacy-retargeted-project-refusal",
        json!({"task":task,"space":space,
        "originalRoot":fixture.root_a,"currentProjectRoot":fixture.root_b,
        "before":before,"plan":plan,"revision":before_snapshot.snapshot.task_revision,"W":before_w}),
    );
    assert_eq!(
        plan["status"], "blocked",
        "unrelated project root cannot prove the original scope"
    );
    assert!(
        plan["blockers"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    assert!(plan["previewId"].is_null());
    assert_eq!(identity(&client, &task).await, before);
    assert_eq!(
        core.store
            .load_snapshot("luna-task-identity-events-v1")
            .unwrap(),
        before_log
    );
    assert_eq!(
        core.store.workspace_repair_revision(&task).unwrap(),
        before_w
    );
    close(core, client).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_linked_worktree_remains_a_valid_legacy_repair_root() {
    let (fixture, core, client, task, space) = legacy_project_collision(true).await;
    let before = identity(&client, &task).await;
    let plan = client
        .call(
            "PreviewWorkspaceIdentityRepair",
            json!({"taskId":task,"providerProof":"localOnly"}),
        )
        .await
        .unwrap();
    evidence(
        "legacy-linked-worktree-proof",
        json!({"task":task,"space":space,
        "sourceRepo":fixture.repo,"linkedRoot":fixture.root_a,"before":before,"plan":plan,
        "registrations":git(&fixture.repo,&["worktree","list","--porcelain"])}),
    );
    assert_eq!(
        plan["status"], "ready",
        "registered checkout of the selected repository must remain repairable"
    );
    assert!(plan["previewId"].is_string());
    close(core, client).await;
}

// Scope HOME and the materialization root to a child test process; never mutate
// process-global environment in parallel tests or create a worktree in user data.
fn isolated_child(test: &str) -> bool {
    if std::env::var("LUNA_IDENTITY_FIXTURE_CHILD").ok().as_deref() == Some(test) {
        return false;
    }
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("owner-home");
    let worktrees = temp.path().join("managed-worktrees");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&worktrees).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("LUNA_IDENTITY_FIXTURE_CHILD", test)
        .env("HOME", home.canonicalize().unwrap())
        .env("LUNA_WORKTREES_DIR", worktrees.canonicalize().unwrap())
        .output()
        .unwrap();
    let artifact = evidence(
        test,
        json!({"status":output.status.code(),
        "stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)}),
    );
    assert!(
        output.status.success(),
        "isolated child failed: {}",
        artifact.display()
    );
    true
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_home_alias_uses_owner_home_not_engine_working_directory() {
    if isolated_child("owner_home_alias_uses_owner_home_not_engine_working_directory") {
        return;
    }
    let fixture = Fixture::new();
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let home = PathBuf::from(std::env::var_os("HOME").unwrap())
        .canonicalize()
        .unwrap();
    let a = uuid::Uuid::new_v4().to_string();
    let b = uuid::Uuid::new_v4().to_string();
    create(&client, &a, &core.device_id, None, Path::new("~")).await;
    create(&client, &b, &core.device_id, None, &home).await;
    let first = identity(&client, &a).await;
    let second = identity(&client, &b).await;
    evidence(
        "owner-home-normalization",
        json!({"home":home,"first":first,"second":second}),
    );
    close(core, client).await;
    assert_eq!(first.workspace.current_path, home.to_string_lossy());
    assert_eq!(second.workspace.workspace_id, first.workspace.workspace_id);
    assert!(first.workspace.workspace_id.starts_with("workspace:v2:"));
}

struct MaterializationRecorder {
    cwds: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl luna_harness::Harness for MaterializationRecorder {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Identity materialization fixture"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> luna_proto::SteeringMode {
        luna_proto::SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[luna_proto::ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<luna_proto::Model>, luna_harness::HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: luna_proto::RunRequest,
        _: luna_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<luna_proto::AgentEvent, luna_harness::HarnessError>,
        >,
        luna_harness::HarnessError,
    > {
        use futures::StreamExt;
        self.cwds.lock().unwrap().push(request.cwd.clone());
        Ok(futures::stream::iter(vec![
            Ok(luna_proto::AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "identity-fixture".into(),
                tools: vec![],
                cwd: request.cwd,
                session_id: "synthetic-materialized-session".into(),
                assistant_message_id: uuid::Uuid::new_v4().to_string(),
            }),
            Ok(luna_proto::AgentEvent::TextDelta {
                text: "fixture complete".into(),
            }),
            Ok(luna_proto::AgentEvent::Done {
                status: luna_proto::DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("synthetic-materialized-session".into()),
            }),
        ])
        .boxed())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_worktree_materialization_keeps_sibling_root_and_initial_binding() {
    if isolated_child("new_worktree_materialization_keeps_sibling_root_and_initial_binding") {
        return;
    }
    let fixture = Fixture::new();
    fixture.init_repo();
    let cwds = Arc::new(std::sync::Mutex::new(Vec::new()));
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(MaterializationRecorder { cwds: cwds.clone() }));
    let core = fixture.open_with_registry(registry);
    let client = luna_rpc::memory_client(core.rpc_service());
    let space = uuid::Uuid::new_v4().to_string();
    client
        .call(
            methods::MUTATE,
            json!({"op":"createSpace","spaceId":space,
        "deviceId":core.device_id,"path":fixture.repo,"gitDetected":true}),
        )
        .await
        .unwrap();
    let a = uuid::Uuid::new_v4().to_string();
    let b = uuid::Uuid::new_v4().to_string();
    create(&client, &a, &core.device_id, Some(&space), &fixture.repo).await;
    create(&client, &b, &core.device_id, Some(&space), &fixture.repo).await;
    client
        .call(
            methods::MUTATE,
            json!({"op":"renameChat","chatId":a,"title":"Pre-titled identity fixture"}),
        )
        .await
        .unwrap();
    let state: luna_proto::TaskCommandRead = client
        .call_as(methods::GET_TASK_COMMAND_SNAPSHOT, json!({"taskId":a}))
        .await
        .unwrap();
    let command = uuid::Uuid::new_v4().to_string();
    let receipt:CommandReceipt=client.call_as(methods::EXECUTE_TASK_COMMAND,json!({
        "commandId":command,"taskId":a,"expectedTaskRevision":state.snapshot.task_revision,
        "intent":{"kind":"session","command":{"kind":"run","messageId":uuid::Uuid::new_v4().to_string(),
        "request":{"prompt":"fixture materialization","harness":"mock","model":"identity-fixture",
        "reasoning":null,"cwd":fixture.repo,"sandbox":"workspace-write","resume":null,"attachments":[],
        "worktree":{"repoPath":fixture.repo,"base":"main"}}}}})).await.unwrap();
    let final_receipt = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let mut receipt = receipt;
        loop {
            if receipt.status.is_terminal() {
                break receipt;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            receipt = client
                .call_as::<Option<CommandReceipt>>(
                    methods::GET_TASK_COMMAND_RECEIPT,
                    json!({"commandId":command}),
                )
                .await
                .unwrap()
                .unwrap();
        }
    })
    .await
    .unwrap();
    let first = identity(&client, &a).await;
    let sibling = identity(&client, &b).await;
    let observed = cwds.lock().unwrap().clone();
    evidence(
        "new-worktree-first-binding",
        json!({"receipt":final_receipt,"first":first,"sibling":sibling,"observedCwds":observed}),
    );
    close(core, client).await;
    assert_eq!(final_receipt.status, CommandReceiptStatus::Applied);
    assert_eq!(observed.len(), 1);
    assert_ne!(observed[0], fixture.repo.to_string_lossy());
    assert_eq!(first.workspace.current_path, observed[0]);
    assert_eq!(
        sibling.workspace.current_path,
        fixture.repo.to_string_lossy()
    );
    assert_ne!(first.workspace.workspace_id, sibling.workspace.workspace_id);
    assert_eq!(
        first.current_binding.as_ref().unwrap().workspace_id,
        first.workspace.workspace_id
    );
}
