//! Failure-first UUID WorktreeSpec → atomic metadata → project setup → provider
//! integration. Every executable command stays in disposable local Git fixtures.
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures::{StreamExt, stream::BoxStream};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, CommandReceipt, CommandReceiptStatus, DoneStatus, HarnessId, Model, ProjectAction,
    ProjectCommandRun, ProjectCommandStatus, ReasoningLevel, RunRequest, SteeringMode,
    TaskCommandRead, TaskIdentityView, TerminalEvent,
};
use luna_rpc::{RpcClient, RpcError, methods};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const CASE: &str = "uuid_worktree_setup_fail_restart_retry_and_continue";

fn artifact(name: &str, value: Value) -> PathBuf {
    let root = std::env::var_os("LUNA_COMMAND_QA_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("luna-commands-workspace-qa"));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    eprintln!("setup evidence: {}", path.display());
    path
}
fn source_sha256() -> String {
    let digest = Sha256::digest(include_bytes!("project_commands_worktree_setup.rs"));
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Commands QA")
        .env("GIT_AUTHOR_EMAIL", "qa@invalid.test")
        .env("GIT_COMMITTER_NAME", "Commands QA")
        .env("GIT_COMMITTER_EMAIL", "qa@invalid.test")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
fn linked_worktrees(repo: &Path) -> Vec<String> {
    git(repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|line| line.strip_prefix("worktree ").map(str::to_string))
        .collect()
}
fn setup_action() -> ProjectAction {
    ProjectAction {
        id: "setup".into(),
        name: "QA setup".into(),
        command: r#"printf 'setup-attempt\n' | tee -a qa-setup-attempts; while [ ! -f qa-release-setup ]; do sleep 0.02; done; test -f qa-allow-setup"#
            .into(),
        setup_on_worktree_create: true,
        wait_for_setup: true,
        preview_url: None,
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    data: PathBuf,
    repo: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "core.hooksPath", "disabled-qa-hooks"]);
        std::fs::write(repo.join("README.md"), "isolated UUID setup fixture\n").unwrap();
        git(&repo, &["add", "README.md"]);
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
            data: base.join("data"),
            repo,
            _temp: temp,
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
struct RecordingHarness {
    cwds: Arc<Mutex<Vec<String>>>,
}
#[async_trait]
impl Harness for RecordingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "UUID setup fixture"
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
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.cwds.lock().unwrap().push(request.cwd.clone());
        let session = uuid::Uuid::new_v4().to_string();
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "fixture-mock".into(),
                tools: vec![],
                cwd: request.cwd,
                session_id: session.clone(),
                assistant_message_id: uuid::Uuid::new_v4().to_string(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(session),
            }),
        ])
        .boxed())
    }
}
fn registry(cwds: Arc<Mutex<Vec<String>>>) -> Arc<HarnessRegistry> {
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(RecordingHarness { cwds }));
    registry
}
async fn create_task(client: &RpcClient, task: &str, space: &str, cwd: &Path) -> CommandReceipt {
    client
        .call_as(
            methods::EXECUTE_TASK_COMMAND,
            json!({
                "commandId": uuid::Uuid::new_v4().to_string(), "taskId":task,
                "actorId":"setup-qa", "clientId":"setup-qa", "clientSessionId":"setup-qa",
                "intent":{"kind":"createChat","spaceId":space,"cwd":cwd}
            }),
        )
        .await
        .unwrap()
}
async fn identity(client: &RpcClient, task: &str) -> Option<TaskIdentityView> {
    client
        .call_as(methods::GET_TASK_IDENTITY, json!({"taskId":task}))
        .await
        .unwrap()
}
async fn snapshot(client: &RpcClient, task: &str) -> TaskCommandRead {
    client
        .call_as(methods::GET_TASK_COMMAND_SNAPSHOT, json!({"taskId":task}))
        .await
        .unwrap()
}
fn run_envelope(
    task: &str,
    command_id: &str,
    expected: &TaskCommandRead,
    cwd: &Path,
    repo: Option<&Path>,
) -> Value {
    let mut request = json!({
        "prompt":"fixture Run", "harness":"mock", "model":"fixture-mock",
        "reasoning":null, "cwd":cwd, "sandbox":"workspace-write",
        "resume":null, "attachments":[]
    });
    if let Some(repo) = repo {
        request["worktree"] = json!({"repoPath":repo,"base":"main"});
    }
    json!({
        "commandId":command_id, "taskId":task, "actorId":"setup-qa", "clientId":"setup-qa",
        "clientSessionId":"setup-qa", "expectedTaskRevision":expected.snapshot.task_revision,
        "expectedBindingGeneration":expected.snapshot.binding_generation,
        "intent":{"kind":"session","command":{"kind":"run",
            "messageId":uuid::Uuid::new_v4().to_string(),"request":request}}
    })
}
async fn receipt_until_terminal(
    client: &RpcClient,
    command_id: &str,
) -> (Option<CommandReceipt>, Option<String>) {
    match tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match client
                .call_as::<Option<CommandReceipt>>(
                    methods::GET_TASK_COMMAND_RECEIPT,
                    json!({"commandId":command_id}),
                )
                .await
            {
                Ok(Some(receipt)) if receipt.status.is_terminal() => break Ok(receipt),
                Ok(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                Err(error) => break Err(error.to_string()),
            }
        }
    })
    .await
    {
        Ok(Ok(receipt)) => (Some(receipt), None),
        Ok(Err(error)) => (None, Some(error)),
        Err(_) => (None, Some("receipt did not settle in 20 seconds".into())),
    }
}
fn candidate_worktree(root: &Path) -> Option<PathBuf> {
    for repo_entry in std::fs::read_dir(root).ok()?.flatten() {
        for entry in std::fs::read_dir(repo_entry.path()).ok()?.flatten() {
            if entry.path().join("qa-setup-attempts").exists() {
                return Some(entry.path());
            }
        }
    }
    None
}
async fn wait_for_setup_marker(
    client: &RpcClient,
    command_id: &str,
    root: &Path,
) -> Result<PathBuf, String> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(path) = candidate_worktree(root) {
                break Ok(path);
            }
            match client
                .call_as::<Option<CommandReceipt>>(
                    methods::GET_TASK_COMMAND_RECEIPT,
                    json!({"commandId":command_id}),
                )
                .await
            {
                Ok(Some(receipt)) if receipt.status.is_terminal() => {
                    break Err(format!(
                        "Run settled before setup shell marker: {:?} {:?}",
                        receipt.status, receipt.reason
                    ));
                }
                Ok(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                Err(error) => {
                    break Err(format!("receipt read failed before setup marker: {error}"));
                }
            }
        }
    })
    .await
    .map_err(|_| "setup shell marker did not appear in 20 seconds".to_string())?
}
async fn setup_until_terminal(core: &EngineCore, task: &str) -> Result<ProjectCommandRun, String> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let snapshot = core
                .project_commands
                .snapshot(task)
                .await
                .map_err(|e| e.to_string())?;
            if let Some(run) = snapshot.runs.into_iter().find(|run| run.setup)
                && !run.status.is_active()
            {
                break Ok(run);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| "setup did not settle in 15 seconds".to_string())?
}
async fn terminal_text(core: &EngineCore, terminal_id: &str) -> Result<String, String> {
    let mut rx = core
        .terminals
        .subscribe(terminal_id, None)
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = rx.recv().await {
            match event {
                TerminalEvent::Data { data, .. } => {
                    bytes.extend(BASE64.decode(data).map_err(|e| e.to_string())?)
                }
                TerminalEvent::Exit { .. } => break,
            }
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|_| "terminal replay timed out".to_string())?;
    read?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
async fn close(core: EngineCore, client: RpcClient) {
    drop(client);
    assert!(
        core.shutdown().await,
        "fixture engine did not shut down cleanly"
    );
    drop(core);
    tokio::task::yield_now().await;
}
fn isolated_child() -> bool {
    if std::env::var("LUNA_COMMAND_SETUP_CHILD").ok().as_deref() == Some(CASE) {
        return false;
    }
    let temp = tempfile::tempdir().unwrap();
    let worktrees = temp.path().join("managed-worktrees");
    std::fs::create_dir_all(&worktrees).unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CASE, "--nocapture", "--test-threads=1"])
        .env("LUNA_COMMAND_SETUP_CHILD", CASE)
        .env("LUNA_WORKTREES_DIR", worktrees.canonicalize().unwrap())
        .output()
        .unwrap();
    let report = artifact(
        "uuid-setup-child",
        json!({
            "sourceSha256":source_sha256(), "status":out.status.code(),
            "stdout":String::from_utf8_lossy(&out.stdout),
            "stderr":String::from_utf8_lossy(&out.stderr),
        }),
    );
    assert!(
        out.status.success(),
        "UUID setup child failed; see {}",
        report.display()
    );
    true
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uuid_worktree_setup_fail_restart_retry_and_continue() {
    if isolated_child() {
        return;
    }
    let fixture = Fixture::new();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let core = fixture.open(registry(observed.clone()));
    let client = luna_rpc::memory_client(core.rpc_service());
    let project = uuid::Uuid::new_v4().to_string();
    let task = uuid::Uuid::new_v4().to_string();
    let sibling = uuid::Uuid::new_v4().to_string();
    core.workspace
        .create_space(
            &project,
            &core.device_id,
            fixture.repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    core.project_commands
        .configure(&project, &[], vec![setup_action()])
        .unwrap();
    let target_created = create_task(&client, &task, &project, &fixture.repo).await;
    let sibling_created = create_task(&client, &sibling, &project, &fixture.repo).await;
    let target_before = identity(&client, &task).await;
    let sibling_before = identity(&client, &sibling).await;
    let created_report = artifact(
        "uuid-setup-created",
        json!({
            "sourceSha256":source_sha256(), "task":task, "sibling":sibling, "project":project,
            "targetCreate":target_created, "siblingCreate":sibling_created,
            "targetBefore":target_before, "siblingBefore":sibling_before,
        }),
    );
    assert_eq!(
        target_created.status,
        CommandReceiptStatus::Applied,
        "{}",
        created_report.display()
    );
    assert_eq!(
        sibling_created.status,
        CommandReceiptStatus::Applied,
        "{}",
        created_report.display()
    );
    let target_before = target_before.expect("UUID target must have managed identity");
    assert!(
        target_before.current_binding.is_none() && target_before.bindings.is_empty(),
        "first materialization requires an unbound UUID task"
    );
    let sibling_before = sibling_before.expect("UUID sibling must have managed identity");
    core.workspace
        .rename_chat(&task, "Pre-titled setup fixture")
        .unwrap();
    let before_run = snapshot(&client, &task).await;
    let command_id = uuid::Uuid::new_v4().to_string();
    let first_request = run_envelope(
        &task,
        &command_id,
        &before_run,
        &fixture.repo,
        Some(&fixture.repo),
    );
    let submitted: Result<CommandReceipt, RpcError> = client
        .call_as(methods::EXECUTE_TASK_COMMAND, first_request.clone())
        .await;
    let submission_error = submitted.as_ref().err().map(ToString::to_string);
    let worktrees_root =
        PathBuf::from(std::env::var_os("LUNA_WORKTREES_DIR").expect("isolated child root"));
    let marker_probe = if submitted.is_ok() {
        wait_for_setup_marker(&client, &command_id, &worktrees_root).await
    } else {
        Err(submission_error
            .clone()
            .unwrap_or_else(|| "Run was not submitted".into()))
    };
    // Read the registry/identity WHILE the setup shell is paused at its marker.
    // This proves metadata was durable before the shell could proceed. Release
    // the fixture shell before assertions so failure cannot strand a PTY.
    let at_shell_chat = core.workspace.chat(&task).unwrap();
    let at_shell_identity = identity(&client, &task).await;
    let at_shell_sibling = identity(&client, &sibling).await;
    let at_shell_receipt: Result<Option<CommandReceipt>, RpcError> = client
        .call_as(
            methods::GET_TASK_COMMAND_RECEIPT,
            json!({"commandId":command_id}),
        )
        .await;
    let at_shell_git_roots = linked_worktrees(&fixture.repo);
    let at_shell_report = artifact(
        "uuid-setup-shell-before-release",
        json!({
            "sourceSha256":source_sha256(),"task":task,"markerProbe":marker_probe,
            "chat":at_shell_chat,"identity":at_shell_identity,"sibling":at_shell_sibling,
            "receipt":at_shell_receipt.as_ref().ok(),"receiptError":at_shell_receipt.as_ref().err().map(ToString::to_string),
            "observedHarnessCwds":observed.lock().unwrap().clone(),"gitRoots":at_shell_git_roots,
            "worktreesRoot":worktrees_root,
        }),
    );
    let shell_root = marker_probe.as_ref().ok().cloned().or_else(|| {
        at_shell_git_roots
            .iter()
            .find(|root| Path::new(root.as_str()) != fixture.repo)
            .map(PathBuf::from)
    });
    if let Some(root) = shell_root.as_ref() {
        std::fs::write(root.join("qa-release-setup"), "release\n").unwrap();
    }
    assert!(marker_probe.is_ok(), "{}", at_shell_report.display());
    let at_shell_chat = at_shell_chat.expect("chat row while setup shell is paused");
    let at_shell_identity =
        at_shell_identity.expect("managed identity while setup shell is paused");
    let root = marker_probe.unwrap();
    assert_eq!(at_shell_chat.cwd.as_deref(), root.to_str());
    assert_eq!(
        at_shell_identity.workspace.current_path,
        root.to_string_lossy()
    );
    assert_eq!(
        at_shell_sibling.unwrap().workspace.current_path,
        fixture.repo.to_string_lossy()
    );
    assert!(
        observed.lock().unwrap().is_empty(),
        "provider started before setup completed"
    );
    let (settled, receipt_error) = receipt_until_terminal(&client, &command_id).await;
    let chat = core.workspace.chat(&task).unwrap();
    let target_after = identity(&client, &task).await;
    let sibling_after = identity(&client, &sibling).await;
    let setup = if chat
        .as_ref()
        .and_then(|chat| chat.cwd.as_deref())
        .is_some_and(|cwd| cwd != fixture.repo.to_string_lossy())
    {
        setup_until_terminal(&core, &task).await.ok()
    } else {
        None
    };
    let setup_output = if let Some(id) = setup.as_ref().and_then(|run| run.terminal_id.as_deref()) {
        terminal_text(&core, id).await
    } else {
        Err("setup terminal unavailable".into())
    };
    let setup_cwd = chat.as_ref().and_then(|chat| chat.cwd.clone());
    let marker_lines = setup_cwd
        .as_ref()
        .and_then(|cwd| std::fs::read_to_string(Path::new(cwd).join("qa-setup-attempts")).ok())
        .map(|text| text.lines().count());
    let git_roots = linked_worktrees(&fixture.repo);
    let first_report = artifact(
        "uuid-setup-failed-preassert",
        json!({
            "sourceSha256":source_sha256(), "task":task, "commandId":command_id,
            "submitted":submitted.as_ref().ok(), "submissionError":submission_error,
            "finalReceipt":settled, "receiptError":receipt_error,
            "chat":chat, "beforeIdentity":target_before, "afterIdentity":target_after,
            "siblingBefore":sibling_before, "siblingAfter":sibling_after,
            "setup":setup, "terminalReplay":setup_output, "setupMarkerLines":marker_lines,
            "harnessCwds":observed.lock().unwrap().clone(), "gitRoots":git_roots,
            "originalRepo":fixture.repo,
        }),
    );
    assert!(
        settled
            .as_ref()
            .is_some_and(|receipt| receipt.status != CommandReceiptStatus::Applied
                && receipt.status.is_terminal()),
        "{}",
        first_report.display()
    );
    let chat = chat.expect("target chat row after materialization");
    let setup = setup.expect("visible setup receipt after materialization");
    let target_after = target_after.expect("target identity after materialization");
    let sibling_after = sibling_after.expect("sibling identity after materialization");
    let worktree = PathBuf::from(chat.cwd.as_deref().expect("committed cwd"));
    assert_eq!(setup.status, ProjectCommandStatus::Failed);
    assert_eq!(setup.attempt, 1);
    assert_eq!(setup.action.command, setup_action().command);
    assert!(
        setup
            .detail
            .as_ref()
            .is_some_and(|detail| !detail.trim().is_empty())
    );
    assert!(
        settled
            .as_ref()
            .and_then(|receipt| receipt.reason.as_ref())
            .is_some_and(|reason| !reason.message.trim().is_empty()),
        "{}",
        first_report.display()
    );
    assert_eq!(setup.cwd, worktree.to_string_lossy());
    assert!(
        setup_output
            .as_ref()
            .is_ok_and(|text| text.contains("setup-attempt"))
    );
    assert_eq!(marker_lines, Some(1));
    assert_eq!(git_roots.len(), 2); // repository root + exactly one linked checkout
    assert!(worktree.join(".git").is_file());
    assert_eq!(
        target_after.workspace.current_path,
        worktree.to_string_lossy()
    );
    assert_eq!(
        sibling_after.workspace.current_path,
        fixture.repo.to_string_lossy()
    );
    assert_eq!(
        sibling_after.workspace.workspace_id,
        sibling_before.workspace.workspace_id
    );
    assert!(
        observed.lock().unwrap().is_empty(),
        "provider must not run behind failed setup"
    );
    close(core, client).await;

    // Restart must reconcile terminal state without replaying setup/provider work.
    let core = fixture.open(registry(observed.clone()));
    let client = luna_rpc::memory_client(core.rpc_service());
    let restored = core.project_commands.snapshot(&task).await.unwrap();
    let restored_setup = restored.runs.iter().find(|run| run.id == setup.id).unwrap();
    let restored_output =
        terminal_text(&core, restored_setup.terminal_id.as_deref().unwrap()).await;
    let restored_chat = core.workspace.chat(&task).unwrap().unwrap();
    let restored_identity = identity(&client, &task).await.unwrap();
    let retry_original: Result<CommandReceipt, RpcError> = client
        .call_as(methods::EXECUTE_TASK_COMMAND, first_request.clone())
        .await;
    let after_duplicate_roots = linked_worktrees(&fixture.repo);
    let attempts_before_retry = std::fs::read_to_string(worktree.join("qa-setup-attempts"))
        .unwrap()
        .lines()
        .count();
    let restart_report = artifact(
        "uuid-setup-restart-preassert",
        json!({
            "sourceSha256":source_sha256(), "task":task, "restoredSetup":restored_setup,
            "restoredOutput":restored_output,"restoredChat":restored_chat,
            "restoredIdentity":restored_identity, "retryOriginal":retry_original.as_ref().ok(),
            "retryOriginalError":retry_original.as_ref().err().map(ToString::to_string),
            "gitRoots":after_duplicate_roots,"markerLines":attempts_before_retry,
            "harnessCwds":observed.lock().unwrap().clone(),
        }),
    );
    assert_eq!(
        restored_setup.status,
        ProjectCommandStatus::Failed,
        "{}",
        restart_report.display()
    );
    assert!(
        restored_output
            .as_ref()
            .is_ok_and(|text| text.contains("setup-attempt"))
    );
    assert_eq!(
        restored_chat.cwd.as_deref(),
        Some(worktree.to_str().unwrap())
    );
    assert_eq!(
        restored_identity.workspace.current_path,
        worktree.to_string_lossy()
    );
    assert!(
        retry_original
            .as_ref()
            .is_ok_and(|receipt| receipt.status != CommandReceiptStatus::Applied),
        "{}",
        restart_report.display()
    );
    assert_eq!(after_duplicate_roots.len(), 2);
    assert_eq!(attempts_before_retry, 1);
    assert!(observed.lock().unwrap().is_empty());

    std::fs::write(worktree.join("qa-allow-setup"), "allow\n").unwrap();
    let retry = core.project_commands.retry(&task, &setup.id).await.unwrap();
    let retry_final = setup_until_terminal(&core, &task).await.unwrap();
    let after_retry_lines = std::fs::read_to_string(worktree.join("qa-setup-attempts"))
        .unwrap()
        .lines()
        .count();
    let retry_report = artifact(
        "uuid-setup-retry-preassert",
        json!({
            "task":task,"retryStart":retry,"retryFinal":retry_final,
            "markerLines":after_retry_lines,"gitRoots":linked_worktrees(&fixture.repo),
            "workspace":identity(&client,&task).await,"harnessCwds":observed.lock().unwrap().clone(),
        }),
    );
    assert_eq!(
        retry_final.status,
        ProjectCommandStatus::Succeeded,
        "{}",
        retry_report.display()
    );
    assert_eq!(retry_final.attempt, 2);
    assert_eq!(retry_final.action_digest, setup.action_digest);
    assert_eq!(retry_final.checkout_generation, setup.checkout_generation);
    assert_eq!(after_retry_lines, 2);
    assert!(observed.lock().unwrap().is_empty());

    let before_followup = snapshot(&client, &task).await;
    let followup_id = uuid::Uuid::new_v4().to_string();
    let followup = run_envelope(&task, &followup_id, &before_followup, &worktree, None);
    let sent: Result<CommandReceipt, RpcError> = client
        .call_as(methods::EXECUTE_TASK_COMMAND, followup)
        .await;
    let (applied, applied_error) = receipt_until_terminal(&client, &followup_id).await;
    if applied
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
    let final_chat = core.workspace.chat(&task).unwrap();
    let final_identity = identity(&client, &task).await;
    let final_sibling = identity(&client, &sibling).await;
    let final_cwds = observed.lock().unwrap().clone();
    let final_roots = linked_worktrees(&fixture.repo);
    let final_lines = std::fs::read_to_string(worktree.join("qa-setup-attempts"))
        .unwrap()
        .lines()
        .count();
    let final_report = artifact(
        "uuid-setup-provider-preassert",
        json!({
            "sourceSha256":source_sha256(),"task":task,"sibling":sibling,
            "followupSubmitted":sent.as_ref().ok(),"followupSubmitError":sent.as_ref().err().map(ToString::to_string),
            "followupReceipt":applied,"followupReceiptError":applied_error,
            "chat":final_chat,"identity":final_identity,"siblingIdentity":final_sibling,
            "harnessCwds":final_cwds,"gitRoots":final_roots,"markerLines":final_lines,
            "beforeFollowupRevision":before_followup.snapshot.task_revision,
        }),
    );
    drop(client);
    assert!(core.shutdown().await);
    assert_eq!(
        applied.as_ref().map(|receipt| receipt.status),
        Some(CommandReceiptStatus::Applied),
        "{}",
        final_report.display()
    );
    assert_eq!(final_cwds, vec![worktree.to_string_lossy().to_string()]);
    let final_chat = final_chat.unwrap();
    let final_identity = final_identity.unwrap();
    let final_sibling = final_sibling.unwrap();
    assert_eq!(final_chat.cwd.as_deref(), Some(worktree.to_str().unwrap()));
    assert_eq!(
        final_identity.workspace.current_path,
        worktree.to_string_lossy()
    );
    assert_eq!(
        final_identity
            .current_binding
            .as_ref()
            .unwrap()
            .workspace_id,
        final_identity.workspace.workspace_id
    );
    assert_eq!(
        final_sibling.workspace.current_path,
        fixture.repo.to_string_lossy()
    );
    assert_eq!(final_roots.len(), 2);
    assert_eq!(final_lines, 2);
    assert_eq!(
        applied.unwrap().snapshot.task_revision,
        before_followup.snapshot.task_revision + 1
    );
}
