//! Separate UUID product-path controls. Materialization has an explicit pending
//! Workspace metadata-bridge dependency; a failure is recorded before assertion.
#![cfg(unix)]
use futures::StreamExt;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_proto::{
    AgentEvent, CommandReceipt, CommandReceiptStatus, DoneStatus, HarnessId, ProjectAction,
    ProjectCommandRun, ProjectCommandStatus, TaskCommandRead, TaskIdentityView,
};
use luna_rpc::{RpcClient, methods};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

fn evidence(case: &str, value: Value) -> PathBuf {
    let root = std::env::var_os("LUNA_COMMAND_QA_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("luna-same-chat-context-evidence"));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!("{case}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    eprintln!("context evidence: {}", path.display());
    path
}
fn git(path: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Commands fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@invalid.test")
        .env("GIT_COMMITTER_NAME", "Commands fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@invalid.test")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    nested: PathBuf,
    data: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        let nested = repo.join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &["config", "core.hooksPath", "disabled-fixture-hooks"],
        );
        std::fs::write(nested.join("README"), "fixture").unwrap();
        git(&repo, &["add", "sub/README"]);
        git(
            &repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "fixture",
            ],
        );
        Self {
            data: root.join("data"),
            _temp: temp,
            root,
            repo,
            nested,
        }
    }
    fn open(&self, registry: Arc<HarnessRegistry>) -> EngineCore {
        let accounts = AgentAccountsConfig {
            data_dir: self.data.join("synthetic-providers"),
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
        .unwrap()
    }
}
fn action(command: &str) -> ProjectAction {
    ProjectAction {
        id: "shared-build".into(),
        name: "Shared ID".into(),
        command: command.into(),
        setup_on_worktree_create: false,
        wait_for_setup: true,
        preview_url: None,
    }
}
async fn state(client: &RpcClient, task: &str) -> TaskCommandRead {
    client
        .call_as(methods::GET_TASK_COMMAND_SNAPSHOT, json!({"taskId":task}))
        .await
        .unwrap()
}
async fn identity(client: &RpcClient, task: &str) -> Option<TaskIdentityView> {
    client
        .call_as(methods::GET_TASK_IDENTITY, json!({"taskId":task}))
        .await
        .unwrap()
}
async fn create(client: &RpcClient, task: &str, space: &str, cwd: &Path) -> CommandReceipt {
    client.call_as(methods::EXECUTE_TASK_COMMAND,json!({"commandId":uuid::Uuid::new_v4().to_string(),"taskId":task,"actorId":"context-fixture","clientId":"context-fixture","clientSessionId":"context-fixture","intent":{"kind":"createChat","spaceId":space,"cwd":cwd}})).await.unwrap()
}
async fn completed(core: &EngineCore, task: &str, id: &str) -> ProjectCommandRun {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let run = core
                .project_commands
                .snapshot(task)
                .await
                .unwrap()
                .runs
                .into_iter()
                .find(|run| run.id == id)
                .unwrap();
            if !run.status.is_active() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uuid_project_move_uses_current_action_and_preserves_task_cwd() {
    let fixture = Fixture::new();
    let core = fixture.open(Arc::new(HarnessRegistry::new()));
    let client = luna_rpc::memory_client(core.rpc_service());
    let source = uuid::Uuid::new_v4().to_string();
    let destination = uuid::Uuid::new_v4().to_string();
    let task = uuid::Uuid::new_v4().to_string();
    core.workspace
        .create_space(
            &source,
            &core.device_id,
            fixture.nested.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    core.workspace
        .create_space(
            &destination,
            &core.device_id,
            fixture.repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let a = action("printf source > context-marker.txt; pwd > context-cwd.txt");
    let b = action("printf destination > context-marker.txt; pwd > context-cwd.txt");
    core.project_commands
        .configure(&source, &[], vec![a])
        .unwrap();
    core.project_commands
        .configure(&destination, &[], vec![b.clone()])
        .unwrap();
    let created = create(&client, &task, &source, &fixture.nested).await;
    let created_identity = identity(&client, &task).await;
    evidence(
        "uuid-move-created",
        json!({"taskId":task,"create":created,"identity":created_identity}),
    );
    assert_eq!(created.status, CommandReceiptStatus::Applied);
    assert!(
        created_identity.is_some(),
        "UUID must enter managed identity"
    );
    let initial = core
        .project_commands
        .run(&task, "shared-build", &uuid::Uuid::new_v4().to_string())
        .await
        .unwrap();
    let initial = completed(&core, &task, &initial.id).await;
    assert_eq!(initial.status, ProjectCommandStatus::Succeeded);
    assert_eq!(
        std::fs::read_to_string(fixture.nested.join("context-marker.txt")).unwrap(),
        "source"
    );
    let before = identity(&client, &task).await.unwrap();
    let before_chat = core.workspace.chat(&task).unwrap().unwrap();
    let prior = state(&client, &task).await;
    let moved:CommandReceipt=client.call_as(methods::EXECUTE_TASK_COMMAND,json!({"commandId":uuid::Uuid::new_v4().to_string(),"taskId":task,"expectedTaskRevision":prior.snapshot.task_revision,"expectedBindingGeneration":prior.snapshot.binding_generation,"intent":{"kind":"moveProject","destinationSpaceId":destination}})).await.unwrap();
    let after = identity(&client, &task).await.unwrap();
    let after_chat = core.workspace.chat(&task).unwrap().unwrap();
    let packet = evidence(
        "uuid-project-move-preassert",
        json!({"taskId":task,"beforeIdentity":before,"afterIdentity":after,"beforeChat":before_chat,"afterChat":after_chat,"receipt":moved,"sourceProject":source,"destinationProject":destination,"priorRevision":prior.snapshot.task_revision,"repairIntentIssued":false}),
    );
    assert_eq!(
        moved.status,
        CommandReceiptStatus::Applied,
        "{}",
        packet.display()
    );
    assert_eq!(
        moved.snapshot.task_revision,
        prior.snapshot.task_revision + 1
    );
    assert_eq!(before_chat.id, after_chat.id);
    assert_eq!(before_chat.cwd, after_chat.cwd);
    assert_eq!(before_chat.device_id, after_chat.device_id);
    assert_eq!(after_chat.space_id.as_deref(), Some(destination.as_str()));
    // Workspace's own target owns W/source-space/binding invariants. Keep
    // their full diagnostics above without conflating them with Commands'
    // positive current-project/action selection proof.
    let current = core
        .project_commands
        .run(&task, "shared-build", &uuid::Uuid::new_v4().to_string())
        .await
        .unwrap();
    let current = completed(&core, &task, &current.id).await;
    let retry_rejected = core
        .project_commands
        .retry(&task, &initial.id)
        .await
        .is_err();
    let stop_rejected = core
        .project_commands
        .stop(&task, &initial.id)
        .await
        .is_err();
    evidence(
        "uuid-project-move-current-target",
        json!({"taskId":task,"sourceRun":initial,"currentRun":current,"oldRetryRejected":retry_rejected,"oldStopRejected":stop_rejected,"backendCurrentTargetIsExpected":true}),
    );
    drop(client);
    assert!(core.shutdown().await);
    assert_eq!(current.status, ProjectCommandStatus::Succeeded);
    assert_eq!(current.action.command, b.command);
    assert_eq!(current.cwd, fixture.nested.to_string_lossy());
    assert_eq!(
        std::fs::read_to_string(fixture.nested.join("context-marker.txt")).unwrap(),
        "destination"
    );
    assert!(retry_rejected && stop_rejected);
}

struct MaterializationRecorder {
    cwds: Arc<Mutex<Vec<String>>>,
}
#[async_trait::async_trait]
impl luna_harness::Harness for MaterializationRecorder {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Commands UUID materialization"
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
        futures::stream::BoxStream<'static, Result<AgentEvent, luna_harness::HarnessError>>,
        luna_harness::HarnessError,
    > {
        self.cwds.lock().unwrap().push(request.cwd.clone());
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "context-fixture".into(),
                tools: vec![],
                cwd: request.cwd,
                session_id: "synthetic-materialization".into(),
                assistant_message_id: uuid::Uuid::new_v4().to_string(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("synthetic-materialization".into()),
            }),
        ])
        .boxed())
    }
}
fn isolated_materialization_child() -> bool {
    const TEST: &str = "uuid_materialization_dependency_uses_real_run_path";
    if std::env::var("LUNA_COMMAND_CONTEXT_CHILD").ok().as_deref() == Some(TEST) {
        return false;
    }
    let temp = tempfile::tempdir().unwrap();
    let worktrees = temp.path().join("managed-worktrees");
    std::fs::create_dir(&worktrees).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env("LUNA_COMMAND_CONTEXT_CHILD", TEST)
        .env("LUNA_WORKTREES_DIR", worktrees.canonicalize().unwrap())
        .output()
        .unwrap();
    let packet = evidence(
        "uuid-materialization-child",
        json!({"status":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"dependency":"Workspace atomic pending-Run metadata bridge; failure is not UI-scope evidence"}),
    );
    assert!(
        output.status.success(),
        "materialization dependency case failed; artifact {}",
        packet.display()
    );
    true
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uuid_materialization_dependency_uses_real_run_path() {
    if isolated_materialization_child() {
        return;
    }
    let fixture = Fixture::new();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(MaterializationRecorder {
        cwds: observed.clone(),
    }));
    let core = fixture.open(registry);
    let client = luna_rpc::memory_client(core.rpc_service());
    let space = uuid::Uuid::new_v4().to_string();
    let task = uuid::Uuid::new_v4().to_string();
    let sibling = uuid::Uuid::new_v4().to_string();
    core.workspace
        .create_space(
            &space,
            &core.device_id,
            fixture.repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    core.project_commands
        .configure(
            &space,
            &[],
            vec![action(
                "printf current-checkout > context-marker.txt; pwd > context-cwd.txt",
            )],
        )
        .unwrap();
    let created = create(&client, &task, &space, &fixture.repo).await;
    let sibling_created = create(&client, &sibling, &space, &fixture.repo).await;
    let before = identity(&client, &task).await;
    let sibling_before = identity(&client, &sibling).await;
    evidence(
        "uuid-materialization-created",
        json!({"taskId":task,"siblingTaskId":sibling,"created":created,"siblingCreated":sibling_created,"before":before,"siblingBefore":sibling_before}),
    );
    assert_eq!(created.status, CommandReceiptStatus::Applied);
    assert_eq!(sibling_created.status, CommandReceiptStatus::Applied);
    assert!(
        before.is_some() && sibling_before.is_some(),
        "UUIDs must enter managed identity"
    );
    assert!(
        before
            .as_ref()
            .is_some_and(|view| view.current_binding.is_none() && view.bindings.is_empty()),
        "first materialization requires an unbound managed UUID task"
    );
    core.workspace
        .rename_chat(&task, "Pre-titled context fixture")
        .unwrap();
    let prior = state(&client, &task).await;
    let command = uuid::Uuid::new_v4().to_string();
    let submitted=client.call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND,json!({"commandId":command,"taskId":task,"expectedTaskRevision":prior.snapshot.task_revision,"expectedBindingGeneration":prior.snapshot.binding_generation,"intent":{"kind":"session","command":{"kind":"run","messageId":uuid::Uuid::new_v4().to_string(),"request":{"prompt":"fixture materialization","harness":"mock","model":"context-fixture","reasoning":null,"cwd":fixture.repo,"sandbox":"workspace-write","resume":null,"attachments":[],"worktree":{"repoPath":fixture.repo,"base":"main"}}}}})).await;
    let submission_error = submitted.as_ref().err().map(ToString::to_string);
    let initial_receipt = submitted.ok();
    let final_receipt = if let Some(receipt) = initial_receipt.clone() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut receipt = receipt;
            loop {
                if receipt.status.is_terminal() {
                    break Some(receipt);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
                match client
                    .call_as::<Option<CommandReceipt>>(
                        methods::GET_TASK_COMMAND_RECEIPT,
                        json!({"commandId":command}),
                    )
                    .await
                {
                    Ok(Some(next)) => receipt = next,
                    _ => break None,
                }
            }
        })
        .await
        .ok()
        .flatten()
    } else {
        None
    };
    if final_receipt
        .as_ref()
        .is_some_and(|receipt| receipt.status == CommandReceiptStatus::Applied)
    {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while observed.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }
    let after = identity(&client, &task).await;
    let sibling_after = identity(&client, &sibling).await;
    let chat = core.workspace.chat(&task).unwrap();
    let actual_cwds = observed.lock().unwrap().clone();
    let managed_root = PathBuf::from(
        std::env::var_os("LUNA_WORKTREES_DIR").expect("isolated child worktree root"),
    );
    let observed_git_files = actual_cwds
        .iter()
        .map(|cwd| Path::new(cwd).join(".git").is_file())
        .collect::<Vec<_>>();
    let packet = evidence(
        "uuid-materialization-preassert",
        json!({"taskId":task,"siblingTaskId":sibling,"commandId":command,"initialReceipt":initial_receipt,"finalReceipt":final_receipt,"submissionError":submission_error,"before":before,"after":after,"siblingBefore":sibling_before,"siblingAfter":sibling_after,"chat":chat,"observedHarnessCwds":actual_cwds,"observedGitFiles":observed_git_files,"repositoryRoot":fixture.repo,"managedWorktreeRoot":std::env::var("LUNA_WORKTREES_DIR").ok(),"dependency":"Workspace atomic pending-Run metadata bridge; strict expectations retained"}),
    );
    let materialized = final_receipt
        .as_ref()
        .is_some_and(|receipt| receipt.status == CommandReceiptStatus::Applied)
        && actual_cwds.len() == 1
        && actual_cwds[0] != fixture.repo.to_string_lossy()
        && Path::new(&actual_cwds[0]).starts_with(&managed_root)
        && observed_git_files[0];
    let fresh_run = if materialized {
        let run = core
            .project_commands
            .run(&task, "shared-build", &uuid::Uuid::new_v4().to_string())
            .await
            .unwrap();
        Some(completed(&core, &task, &run.id).await)
    } else {
        None
    };
    evidence(
        "uuid-materialization-current-target",
        json!({"taskId":task,"run":fresh_run,"preassertArtifact":packet}),
    );
    drop(client);
    assert!(core.shutdown().await);
    assert_eq!(
        final_receipt.as_ref().map(|receipt| receipt.status),
        Some(CommandReceiptStatus::Applied),
        "{}",
        packet.display()
    );
    assert_eq!(actual_cwds.len(), 1, "{}", packet.display());
    assert_ne!(actual_cwds[0], fixture.repo.to_string_lossy());
    assert!(Path::new(&actual_cwds[0]).starts_with(&managed_root));
    assert!(
        observed_git_files[0],
        "recorded provider cwd must be an actual linked worktree"
    );
    assert_eq!(chat.as_ref().unwrap().id, task);
    assert_eq!(
        chat.as_ref().unwrap().cwd.as_deref(),
        Some(actual_cwds[0].as_str())
    );
    let after = after.unwrap();
    let sibling_after = sibling_after.unwrap();
    assert_eq!(after.workspace.current_path, actual_cwds[0]);
    assert_eq!(
        sibling_after.workspace.current_path,
        fixture.repo.to_string_lossy()
    );
    assert_ne!(
        after.workspace.workspace_id,
        sibling_after.workspace.workspace_id
    );
    assert_eq!(
        after.current_binding.as_ref().unwrap().workspace_id,
        after.workspace.workspace_id
    );
    let run = fresh_run.unwrap();
    assert_eq!(run.status, ProjectCommandStatus::Succeeded);
    assert_eq!(run.cwd, actual_cwds[0]);
    assert_eq!(
        std::fs::read_to_string(Path::new(&run.cwd).join("context-cwd.txt"))
            .unwrap()
            .trim(),
        run.cwd
    );
}
