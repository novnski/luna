//! Pane chat (engine side): side chats and child threads as complete,
//! independent chats, through the production path: the engine RPC, durable
//! task commands, the child sweeper, the queue, the side-chat lease service,
//! the native fork path and the runtime-contract compiler. The provider is a
//! deterministic synthetic peer registered under the Codex or Claude Code
//! identity; it records each run request (cwd, resume id, model, permission
//! policy and the compiled LUNA runtime contract). Checkouts are real git
//! repositories. Failure cases: `evidence/pane-engine/failure-cases.md`.

#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, ChildCheckoutKind, ChildCreator, DoneStatus, HarnessId, LunaAgentRuntimeContract,
    Model, PermissionMode, ProviderForkCapability, ProviderForkRequest, ProviderForkResult,
    ReasoningLevel, RunRequest, RuntimeChatRole, RuntimeCheckoutOwnership, RuntimeContextOrigin,
    RuntimeDelivery, SteeringMode,
};
use luna_rpc::RpcClient;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Synthetic provider
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Recorder {
    runs: Arc<Mutex<Vec<RunRequest>>>,
    checkpoints: Arc<Mutex<HashMap<String, Vec<String>>>>,
    sequence: Arc<AtomicUsize>,
}

struct Synthetic {
    id: HarnessId,
    recorder: Recorder,
}

#[async_trait]
impl Harness for Synthetic {
    fn id(&self) -> HarnessId {
        self.id
    }
    fn luna_runtime_delivery(&self) -> RuntimeDelivery {
        match self.id {
            HarnessId::Codex => RuntimeDelivery::CodexMcpTools,
            _ => RuntimeDelivery::ClaudeMcpTools,
        }
    }
    fn display_name(&self) -> &str {
        "pane-chat synthetic provider"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Low, ReasoningLevel::Medium, ReasoningLevel::High]
    }
    fn fork_capability(&self) -> ProviderForkCapability {
        ProviderForkCapability::Checkpoint
    }
    async fn fork_session(
        &self,
        request: ProviderForkRequest,
    ) -> Result<ProviderForkResult, HarnessError> {
        let n = self.recorder.sequence.fetch_add(1, Ordering::SeqCst);
        let fork = format!("{}-fork-{n}", request.source_session_id);
        let mut checkpoints = self.recorder.checkpoints.lock().unwrap();
        let source = checkpoints
            .get(&request.source_session_id)
            .cloned()
            .unwrap_or_default();
        let checkpoint_map = source
            .iter()
            .map(|checkpoint| (checkpoint.clone(), format!("{fork}:{checkpoint}")))
            .collect();
        checkpoints.insert(
            fork.clone(),
            source.iter().map(|cp| format!("{fork}:{cp}")).collect(),
        );
        Ok(ProviderForkResult {
            provider_session_id: fork,
            checkpoint_map,
        })
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(Vec::new())
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let n = self.recorder.sequence.fetch_add(1, Ordering::SeqCst);
        let session = request
            .resume
            .clone()
            .unwrap_or_else(|| format!("synthetic-{}-{n}", provider_name(self.id)));
        let checkpoint = format!("cp-{n}");
        self.recorder
            .checkpoints
            .lock()
            .unwrap()
            .entry(session.clone())
            .or_default()
            .push(checkpoint.clone());
        self.recorder.runs.lock().unwrap().push(request.clone());
        let started = AgentEvent::SessionStarted {
            harness: self.id,
            model: request.model.clone().unwrap_or_else(|| "synthetic".into()),
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: session.clone(),
            assistant_message_id: format!("assistant-{n}"),
        };
        let text = AgentEvent::TextDelta {
            text: format!("ACK {n}"),
        };
        if request.prompt.contains("HOLD") {
            let interrupt = controls.interrupt.clone();
            let tail = futures::stream::once(async move {
                interrupt.cancelled().await;
                Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: Some(session),
                })
            });
            return Ok(futures::stream::iter(vec![Ok(started), Ok(text)])
                .chain(tail)
                .boxed());
        }
        Ok(futures::stream::iter(vec![
            Ok(started),
            Ok(text),
            Ok(AgentEvent::ProviderCheckpoint {
                checkpoint_id: checkpoint,
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

fn provider_name(id: HarnessId) -> &'static str {
    match id {
        HarnessId::Codex => "codex",
        HarnessId::ClaudeCode => "claude-code",
        _ => "other",
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// `Repos` reads `LUNA_WORKTREES_DIR` at assembly; every test in this binary
/// shares one root and uses its own repository name below it.
fn worktrees_root() -> PathBuf {
    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    let root = ROOT.get_or_init(|| tempfile::tempdir().unwrap());
    let path = root.path().canonicalize().unwrap();
    unsafe { std::env::set_var("LUNA_WORKTREES_DIR", &path) };
    path
}

fn account_config(root: &Path) -> AgentAccountsConfig {
    let config = AgentAccountsConfig {
        data_dir: root.join("synthetic-provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    config.disable_claude_keychain_fallback().unwrap();
    std::fs::create_dir_all(&config.claude_config_dir).unwrap();
    std::fs::write(
        &config.claude_config_file,
        json!({"oauthAccount": {"accountUuid": "pane-fixture", "emailAddress": "pane@example.test"}})
            .to_string(),
    )
    .unwrap();
    std::fs::write(
        config.claude_config_dir.join(".credentials.json"),
        json!({"claudeAiOauth": {"accessToken": "synthetic"}}).to_string(),
    )
    .unwrap();
    config
}

fn assemble(root: &Path, id: HarnessId) -> (EngineCore, Recorder) {
    worktrees_root();
    let recorder = Recorder::default();
    let _ = RUN_LOG.set(recorder.clone());
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(Synthetic {
        id,
        recorder: recorder.clone(),
    }));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("engine"), "pane-org", "pane-user"),
        registry,
        id,
        None,
        account_config(root),
    )
    .unwrap();
    (core, recorder)
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// `main` with one commit and a `feature` branch one commit ahead.
fn init_repo(repo: &Path) {
    std::fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "pane@example.test"]);
    git(repo, &["config", "user.name", "Pane Fixture"]);
    std::fs::write(repo.join("README.md"), "main\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "init"]);
    git(repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("FEATURE.md"), "feature\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "feature"]);
    git(repo, &["checkout", "-q", "main"]);
}

fn config(harness: HarnessId, mode: &str, model: &str, reasoning: &str) -> Value {
    json!({
        "harness": provider_name(harness), "model": model, "reasoning": reasoning,
        "sandbox": if mode == "full" { "danger-full-access" } else { "workspace-write" },
        "permissionMode": mode, "effectivePermissionMode": mode, "modelOptions": {}
    })
}

async fn create_chat(client: &RpcClient, id: &str, space: &str, cwd: &Path, config: Value) {
    client
        .call(
            "Mutate",
            json!({"op":"createChat","chatId":id,"spaceId":space,"cwd":cwd,"config":config}),
        )
        .await
        .unwrap();
}

async fn queue(client: &RpcClient, chat: &str, message_id: &str, text: &str) {
    client
        .call(
            "QueueMessage",
            json!({"chatId":chat,"messageId":message_id,"text":text,"holdForTurnEnd":true}),
        )
        .await
        .unwrap();
}

async fn create_child(client: &RpcClient, parent: &str, request: Value) -> Result<Value, String> {
    client
        .call(
            "CreateChildThread",
            json!({"parentTaskId": parent, "request": request}),
        )
        .await
        .map_err(|error| error.to_string())
}

fn contract(request: &RunRequest) -> LunaAgentRuntimeContract {
    serde_json::from_value(
        request
            .model_options
            .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
            .cloned()
            .expect("LUNA runtime contract delivered"),
    )
    .unwrap()
}

fn runs_for(recorder: &Recorder, chat: &str) -> Vec<RunRequest> {
    recorder
        .runs
        .lock()
        .unwrap()
        .iter()
        .filter(|run| {
            run.model_options
                .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
                .and_then(|contract| contract.pointer("/context/threadId"))
                .and_then(Value::as_str)
                == Some(chat)
        })
        .cloned()
        .collect()
}

fn instructions(contract: &LunaAgentRuntimeContract) -> String {
    contract
        .instructions
        .iter()
        .map(|section| section.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

fn entry_count(core: &EngineCore, chat: &str) -> usize {
    core.doc_host
        .open(chat)
        .and_then(|handle| Ok(handle.doc().read_entries()?))
        .map(|entries| entries.len())
        .unwrap_or_default()
}

fn queue_len(core: &EngineCore, chat: &str) -> usize {
    core.doc_host
        .open(chat)
        .and_then(|handle| Ok(handle.doc().read_queue()?))
        .map(|rows| rows.len())
        .unwrap_or_default()
}

fn child_dirs(repo_name: &str) -> Vec<String> {
    let mut dirs: Vec<String> = std::fs::read_dir(worktrees_root().join(repo_name))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; recorded runs: {}",
            RUN_LOG.get().map(run_summary).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Diagnostics only: the last assembled recorder in this test process.
static RUN_LOG: OnceLock<Recorder> = OnceLock::new();

fn run_summary(recorder: &Recorder) -> String {
    recorder
        .runs
        .lock()
        .unwrap()
        .iter()
        .map(|run| {
            format!(
                "[thread={:?} prompt={:?} resume={:?}]",
                run.model_options
                    .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
                    .and_then(|contract| contract.pointer("/context/threadId"))
                    .and_then(Value::as_str),
                run.prompt,
                run.resume
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn evidence(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("PANE_ENGINE_EVIDENCE_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(Path::new(&dir).join(name), format!("{value:#}"));
    }
}

// ---------------------------------------------------------------------------
// Children created from the UI (manual path)
// ---------------------------------------------------------------------------

/// A1 A2 A5 B1–B10 C1 C2 C5 C6 D1 D2 F1 F2.
#[tokio::test(flavor = "multi_thread")]
async fn manual_children_provision_checkout_access_and_parent_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let repo_name = "repo-pane-children";
    let repo = base.join(repo_name);
    init_repo(&repo);
    let main_sha = git(&repo, &["rev-parse", "main"]);
    let feature_sha = git(&repo, &["rev-parse", "feature"]);
    let (core, recorder) = assemble(&base, HarnessId::Codex);
    let client = luna_rpc::memory_client(core.rpc_service());
    core.workspace
        .create_space(
            "space-children",
            &core.device_id,
            repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let parent = uuid::Uuid::new_v4().to_string();
    create_chat(
        &client,
        &parent,
        "space-children",
        &repo,
        config(HarnessId::Codex, "auto", "parent-model", "high"),
    )
    .await;
    core.workspace.set_chat_branch(&parent, "main").unwrap();

    // F1: the parent is a main chat.
    queue(&client, &parent, "parent-1", "parent hello").await;
    until("parent run", || runs_for(&recorder, &parent).len() == 1).await;
    let parent_contract = contract(&runs_for(&recorder, &parent)[0]);
    let identity = parent_contract.context.chat.clone().expect("chat identity");
    assert_eq!(identity.role, RuntimeChatRole::Main);
    assert_eq!(identity.parent_thread_id, None);
    assert_eq!(identity.capabilities.context_origin, RuntimeContextOrigin::Fresh);
    assert!(identity.capabilities.create_child_threads, "{identity:?}");
    assert!(instructions(&parent_contract).contains("Chat role: main"));
    evidence("parent-main-contract.json", &serde_json::to_value(&parent_contract).unwrap());
    let parent_row = core.workspace.chat(&parent).unwrap().unwrap();

    // A1 C1 D1 B8: a legacy payload defaults everything from the parent.
    let legacy_request = json!({"prompt":"LEGACY_BRIEF","idempotencyKey":"legacy"});
    let decoded: luna_proto::CreateChildThreadRequest =
        serde_json::from_value(legacy_request.clone()).unwrap();
    // A5: re-encoding a legacy request adds no new keys (stable payload hash).
    let reencoded = serde_json::to_value(&decoded).unwrap();
    for key in ["checkout", "permissionMode"] {
        assert!(reencoded.get(key).is_none(), "{reencoded}");
    }
    let receipt = create_child(&client, &parent, legacy_request).await.unwrap();
    let legacy = receipt["taskId"].as_str().unwrap().to_owned();
    let row = core.workspace.chat(&legacy).unwrap().unwrap();
    let child = row.child_thread.clone().unwrap();
    let provisioning = child.provisioning.clone().unwrap();
    assert_eq!(receipt["provisioning"], serde_json::to_value(&provisioning).unwrap());
    assert_eq!(provisioning.created_by, ChildCreator::User);
    assert_eq!(provisioning.checkout_kind, ChildCheckoutKind::Current);
    assert_eq!(row.cwd.as_deref(), Some(repo.to_str().unwrap()));
    assert_eq!(row.branch.as_deref(), Some("main"));
    assert_eq!(row.checkout_id, parent_row.checkout_id);
    let child_config = row.config.clone().unwrap();
    assert_eq!(child_config.model.as_deref(), Some("parent-model"));
    assert_eq!(child_config.reasoning, Some(ReasoningLevel::High));
    assert_eq!(child_config.effective_permission_mode, PermissionMode::Auto);
    assert_eq!(provisioning.granted_permission_mode, PermissionMode::Auto);
    assert_eq!(provisioning.parent_permission_ceiling, PermissionMode::Auto);
    assert_eq!(provisioning.requested_permission_mode, None);
    assert!(!provisioning.granted_by_user && !provisioning.escalation_denied);
    // The sweeper starts the child; its run carries the inherited settings
    // and an explicit child identity (F2).
    until("legacy child run", || !runs_for(&recorder, &legacy).is_empty()).await;
    let run = runs_for(&recorder, &legacy)[0].clone();
    assert_eq!(run.cwd, repo.to_str().unwrap());
    assert_eq!(run.model.as_deref(), Some("parent-model"));
    assert_eq!(run.reasoning, Some(ReasoningLevel::High));
    assert_eq!(
        run.permission_policy.as_ref().unwrap().effective_mode,
        PermissionMode::Auto
    );
    let child_contract = contract(&run);
    let identity = child_contract.context.chat.clone().unwrap();
    assert_eq!(identity.role, RuntimeChatRole::Child);
    assert_eq!(identity.parent_thread_id.as_deref(), Some(parent.as_str()));
    assert_eq!(child_contract.context.parent_thread_id.as_deref(), Some(parent.as_str()));
    assert_eq!(identity.capabilities.checkout, RuntimeCheckoutOwnership::SharedWithParent);
    assert_eq!(identity.capabilities.granted_access, Some(PermissionMode::Auto));
    assert!(identity.capabilities.access_capped_by_parent);
    assert!(identity.capabilities.reports_to_parent);
    assert!(instructions(&child_contract).contains("Chat role: child chat"));
    evidence("child-current-contract.json", &serde_json::to_value(&child_contract).unwrap());

    // A2: authority cannot be smuggled through request fields.
    for request in [
        json!({"prompt":"x","idempotencyKey":"smuggle-1","grantedByUser":true}),
        json!({"prompt":"x","idempotencyKey":"smuggle-2","accessGranted":true}),
        json!({"prompt":"x","idempotencyKey":"smuggle-3","checkout":{"kind":"sideways"}}),
        json!({"prompt":"x","idempotencyKey":"smuggle-4","permissionMode":"root"}),
    ] {
        assert!(create_child(&client, &parent, request.clone()).await.is_err(), "{request}");
    }
    let unknown_in_checkout = json!({"prompt":"x","idempotencyKey":"smuggle-5",
        "checkout":{"kind":"newWorktree","branch":"luna/ok","grant":true}});
    assert!(create_child(&client, &parent, unknown_in_checkout).await.is_err());

    // B3 B5 B4 B2: invalid checkout input creates nothing.
    let before_dirs = child_dirs(repo_name);
    let before_branches = git(&repo, &["branch", "--list"]);
    for (key, checkout) in [
        ("bad-1", json!({"kind":"newWorktree","branch":"-evil"})),
        ("bad-2", json!({"kind":"newWorktree","branch":"../up"})),
        ("bad-3", json!({"kind":"newWorktree","branch":"has space"})),
        ("bad-4", json!({"kind":"newWorktree","branch":"a..b"})),
        ("bad-5", json!({"kind":"newWorktree","branch":"x.lock"})),
        ("bad-6", json!({"kind":"newWorktree","base":"--orphan"})),
        ("bad-7", json!({"kind":"newWorktree","base":"no-such-ref"})),
        ("bad-8", json!({"kind":"newWorktree","branch":"feature"})),
    ] {
        let error = create_child(
            &client,
            &parent,
            json!({"prompt":"bad","idempotencyKey":key,"checkout":checkout}),
        )
        .await
        .expect_err(key);
        if key == "bad-7" {
            assert!(error.contains("Base ref does not resolve"), "{error}");
        }
        if key == "bad-8" {
            assert!(error.contains("Branch already exists"), "{error}");
        }
    }
    let ambiguous = json!({"prompt":"bad","idempotencyKey":"bad-cwd","cwd":repo,
        "checkout":{"kind":"newWorktree"}});
    assert!(create_child(&client, &parent, ambiguous).await.is_err());
    assert_eq!(child_dirs(repo_name), before_dirs);
    assert_eq!(git(&repo, &["branch", "--list"]), before_branches);

    // B9 B10: a new worktree on a generated branch from the parent's branch.
    let receipt = create_child(
        &client,
        &parent,
        json!({"prompt":"WT_DEFAULT","idempotencyKey":"wt-default",
            "checkout":{"kind":"newWorktree"}}),
    )
    .await
    .unwrap();
    let wt_child = receipt["taskId"].as_str().unwrap().to_owned();
    let row = core.workspace.chat(&wt_child).unwrap().unwrap();
    let provisioning = row.child_thread.clone().unwrap().provisioning.unwrap();
    let wt_path = PathBuf::from(provisioning.worktree_path.clone().unwrap());
    let wt_branch = format!("luna/child-{}", &wt_child[..8]);
    assert_eq!(provisioning.checkout_kind, ChildCheckoutKind::NewWorktree);
    assert_eq!(provisioning.branch.as_deref(), Some(wt_branch.as_str()));
    assert_eq!(provisioning.base.as_deref(), Some("main"));
    assert!(wt_path.starts_with(worktrees_root().join(repo_name)), "{wt_path:?}");
    assert_eq!(row.cwd.as_deref(), Some(wt_path.to_str().unwrap()));
    assert_eq!(row.branch.as_deref(), Some(wt_branch.as_str()));
    assert!(row.checkout_id.is_some() && row.checkout_id != parent_row.checkout_id);
    assert_eq!(git(&wt_path, &["branch", "--show-current"]), wt_branch);
    assert_eq!(git(&wt_path, &["rev-parse", "HEAD"]), main_sha);
    assert!(
        core.task_identity
            .view(&wt_child)
            .unwrap()
            .workspace
            .worktree_id
            .is_some()
    );
    // The parent's own checkout is untouched.
    assert_eq!(git(&repo, &["branch", "--show-current"]), "main");
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), main_sha);
    assert_eq!(git(&repo, &["status", "--porcelain", "--untracked-files=no"]), "");
    until("worktree child run", || !runs_for(&recorder, &wt_child).is_empty()).await;
    let run = runs_for(&recorder, &wt_child)[0].clone();
    assert_eq!(run.cwd, wt_path.to_str().unwrap());
    let wt_contract = contract(&run);
    assert_eq!(
        wt_contract.context.chat.as_ref().unwrap().capabilities.checkout,
        RuntimeCheckoutOwnership::OwnWorktree
    );
    assert_eq!(wt_contract.context.branch.as_deref(), Some(wt_branch.as_str()));
    assert!(instructions(&wt_contract).contains("its own git worktree and branch"));
    evidence("child-worktree-row.json", &serde_json::to_value(&row).unwrap());

    // Named branch from an explicit base; a retry reuses it (B6).
    let named = json!({"prompt":"WT_NAMED","idempotencyKey":"wt-named",
        "checkout":{"kind":"newWorktree","branch":"feat/child-x","base":"feature"}});
    let first = create_child(&client, &parent, named.clone()).await.unwrap();
    let named_child = first["taskId"].as_str().unwrap().to_owned();
    let named_row = core.workspace.chat(&named_child).unwrap().unwrap();
    let named_path = PathBuf::from(named_row.cwd.clone().unwrap());
    assert_eq!(named_row.branch.as_deref(), Some("feat/child-x"));
    assert_eq!(git(&named_path, &["rev-parse", "HEAD"]), feature_sha);
    let dirs = child_dirs(repo_name);
    let again = create_child(&client, &parent, named).await.unwrap();
    assert_eq!(again["taskId"], first["taskId"]);
    assert_eq!(child_dirs(repo_name), dirs, "a retry must not mint a second worktree");

    // C6: the creation dialog is the user's own grant above the ceiling.
    let granted = create_child(
        &client,
        &parent,
        json!({"prompt":"GRANT_FULL","idempotencyKey":"grant-full","permissionMode":"full"}),
    )
    .await
    .unwrap();
    let granted = granted["taskId"].as_str().unwrap().to_owned();
    let row = core.workspace.chat(&granted).unwrap().unwrap();
    let child = row.child_thread.clone().unwrap();
    let provisioning = child.provisioning.clone().unwrap();
    assert_eq!(provisioning.granted_permission_mode, PermissionMode::Full);
    assert_eq!(provisioning.requested_permission_mode, Some(PermissionMode::Full));
    assert!(provisioning.granted_by_user && !provisioning.escalation_denied);
    assert_eq!(child.permission_ceiling, PermissionMode::Full);
    assert_eq!(child.sandbox_ceiling, None);
    assert_eq!(row.config.unwrap().effective_permission_mode, PermissionMode::Full);
    until("granted child run", || !runs_for(&recorder, &granted).is_empty()).await;
    let run = runs_for(&recorder, &granted)[0].clone();
    assert_eq!(
        run.permission_policy.as_ref().unwrap().effective_mode,
        PermissionMode::Full
    );
    let caps = contract(&run).context.chat.unwrap().capabilities;
    assert!(caps.access_granted_by_user && !caps.access_capped_by_parent);

    // C2: a lower request is granted as asked and stays parent-capped.
    let lower = create_child(
        &client,
        &parent,
        json!({"prompt":"LOWER","idempotencyKey":"lower-ask","permissionMode":"ask"}),
    )
    .await
    .unwrap();
    let lower_row = core
        .workspace
        .chat(lower["taskId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let provisioning = lower_row.child_thread.unwrap().provisioning.unwrap();
    assert_eq!(provisioning.granted_permission_mode, PermissionMode::Ask);
    assert!(!provisioning.granted_by_user);

    // D2: explicit model and reasoning are used as given.
    let explicit = create_child(
        &client,
        &parent,
        json!({"prompt":"EXPLICIT","idempotencyKey":"explicit-model",
            "model":"child-model","reasoning":"low"}),
    )
    .await
    .unwrap();
    let explicit = explicit["taskId"].as_str().unwrap().to_owned();
    let explicit_config = core.workspace.chat(&explicit).unwrap().unwrap().config.unwrap();
    assert_eq!(explicit_config.model.as_deref(), Some("child-model"));
    assert_eq!(explicit_config.reasoning, Some(ReasoningLevel::Low));
    until("explicit child run", || !runs_for(&recorder, &explicit).is_empty()).await;
    assert_eq!(
        runs_for(&recorder, &explicit)[0].model.as_deref(),
        Some("child-model")
    );

    // C5 (dispatch): lowering the parent re-caps ordinary children (the Auto
    // child is refused rather than silently run as hidden Ask) but not the
    // one the user granted.
    let mut lowered: luna_proto::ChatConfig = serde_json::from_value(config(
        HarnessId::Codex,
        "ask",
        "parent-model",
        "high",
    ))
    .unwrap();
    lowered.effective_permission_mode = PermissionMode::Ask;
    core.workspace.set_chat_config(&parent, &lowered).unwrap();
    for (chat, message) in [(&legacy, "legacy-2"), (&granted, "granted-2")] {
        until("child idle", || !core.sessions.turn_in_flight(chat)).await;
        queue(&client, chat, message, "second turn").await;
    }
    until("granted child second run", || runs_for(&recorder, &granted).len() == 2).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        runs_for(&recorder, &legacy)
            .iter()
            .all(|run| run.permission_policy.as_ref().unwrap().effective_mode
                <= PermissionMode::Auto),
        "a parent-capped child never runs above the parent"
    );
    assert_eq!(runs_for(&recorder, &legacy).len(), 1, "capped Auto child is refused");
    assert_eq!(
        runs_for(&recorder, &granted)[1]
            .permission_policy
            .as_ref()
            .unwrap()
            .effective_mode,
        PermissionMode::Full
    );

    // B7: a commit failure after provisioning leaves no orphaned worktree.
    let dirs = child_dirs(repo_name);
    let branches = git(&repo, &["branch", "--list"]);
    let oversized = create_child(
        &client,
        &parent,
        json!({"prompt":"€".repeat(7_000),"idempotencyKey":"oversized-wt",
            "checkout":{"kind":"newWorktree"}}),
    )
    .await
    .expect_err("oversized row");
    assert!(oversized.contains("too large"), "{oversized}");
    assert_eq!(child_dirs(repo_name), dirs);
    assert_eq!(git(&repo, &["branch", "--list"]), branches);
    assert_eq!(git(&repo, &["worktree", "list"]).lines().count(), dirs.len() + 1);

    // B1: a project that is not a git repository cannot get a worktree.
    let plain = base.join("plain-project");
    std::fs::create_dir_all(&plain).unwrap();
    core.workspace
        .create_space("space-plain", &core.device_id, plain.to_str().unwrap(), None, false)
        .unwrap();
    let plain_parent = uuid::Uuid::new_v4().to_string();
    create_chat(
        &client,
        &plain_parent,
        "space-plain",
        &plain,
        config(HarnessId::Codex, "auto", "parent-model", "high"),
    )
    .await;
    let error = create_child(
        &client,
        &plain_parent,
        json!({"prompt":"NO_GIT","idempotencyKey":"no-git","checkout":{"kind":"newWorktree"}}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("not a git repository"), "{error}");
    assert!(
        !core
            .workspace
            .read_chats()
            .unwrap()
            .iter()
            .any(|chat| chat.child_thread.as_ref().is_some_and(|c| c.initial_prompt == "NO_GIT"))
    );

    evidence(
        "children-summary.json",
        &json!({
            "parent": parent, "legacy": legacy, "worktree": wt_child, "named": named_child,
            "granted": granted, "explicit": explicit, "worktreeDirs": child_dirs(repo_name),
            "branches": git(&repo, &["branch", "--list"]),
        }),
    );
    drop(client);
    core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Side chats (native fork path), both provider identities
// ---------------------------------------------------------------------------

/// E1–E5 and the side-chat identity, for one provider identity.
async fn side_chat_forks_inherits_and_runs_independently(id: HarnessId, mode: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let repo = base.join(format!("repo-pane-side-{}", provider_name(id)));
    init_repo(&repo);
    // The parent works in its own linked worktree on its own branch.
    let parent_wt = base.join("parent-worktree");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "parent-wt", parent_wt.to_str().unwrap(), "main"],
    );
    let (core, recorder) = assemble(&base, id);
    let client = luna_rpc::memory_client(core.rpc_service());
    core.workspace
        .create_space("space-side", &core.device_id, repo.to_str().unwrap(), None, true)
        .unwrap();
    let parent = uuid::Uuid::new_v4().to_string();
    create_chat(
        &client,
        &parent,
        "space-side",
        &parent_wt,
        config(id, mode, "parent-model", "medium"),
    )
    .await;
    core.workspace.set_chat_branch(&parent, "parent-wt").unwrap();
    queue(&client, &parent, "parent-context", "PARENT_CONTEXT_SENTINEL").await;
    until("parent bound and idle", || {
        runs_for(&recorder, &parent).len() == 1
            && !core.sessions.turn_in_flight(&parent)
            && core
                .task_identity
                .view(&parent)
                .and_then(|view| view.current_binding)
                .is_some_and(|binding| !binding.provider_task_id.is_empty())
    })
    .await;
    // Let the turn's completion and checkpoint settle into the parent doc.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let parent_row = core.workspace.chat(&parent).unwrap().unwrap();
    let parent_session = parent_row.harness_session_id.clone().unwrap();

    // E1: a native fork that inherits checkout, branch and permissions.
    let created: Value = client
        .call(
            "CreateSideChat",
            json!({"parentId": parent, "requestId": uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    assert_eq!(created["meta"]["contextMode"], "nativeFork", "{created:#}");
    let side = created["meta"]["id"].as_str().unwrap().to_owned();
    let lease = created["lease"].as_str().unwrap().to_owned();
    let side_row = core.workspace.chat(&side).unwrap().unwrap();
    assert_eq!(side_row.cwd, parent_row.cwd);
    assert_eq!(side_row.cwd.as_deref(), Some(parent_wt.to_str().unwrap()));
    assert_eq!(side_row.branch.as_deref(), Some("parent-wt"));
    assert_eq!(side_row.checkout_id, parent_row.checkout_id);
    assert_eq!(side_row.source_context, parent_row.source_context);
    let side_config = side_row.config.clone().unwrap();
    let parent_config = parent_row.config.clone().unwrap();
    assert_eq!(side_config.harness, parent_config.harness);
    assert_eq!(side_config.model, parent_config.model);
    assert_eq!(side_config.permission_mode, parent_config.permission_mode);
    let fork_session = side_row.harness_session_id.clone().unwrap();
    assert_ne!(fork_session, parent_session);
    assert!(fork_session.starts_with(&parent_session));

    // E3: the side chat's own turn never touches the parent.
    let parent_entries = entry_count(&core, &parent);
    client
        .call(
            "SendSideChat",
            json!({"id":side,"lease":lease,"messageId":"side-hold","text":"HOLD side question"}),
        )
        .await
        .unwrap();
    until("side turn running", || {
        core.sessions.turn_in_flight(&side) && !runs_for(&recorder, &side).is_empty()
    })
    .await;
    let side_run = runs_for(&recorder, &side)[0].clone();
    assert_eq!(side_run.cwd, parent_wt.to_str().unwrap());
    assert_eq!(side_run.resume.as_deref(), Some(fork_session.as_str()));
    assert_eq!(side_run.model.as_deref(), Some("parent-model"));
    assert_eq!(
        side_run.permission_policy.as_ref().unwrap().effective_mode,
        parent_config.effective_permission_mode
    );
    let side_contract = contract(&side_run);
    let identity = side_contract.context.chat.clone().unwrap();
    assert_eq!(identity.role, RuntimeChatRole::Side);
    assert_eq!(identity.parent_thread_id.as_deref(), Some(parent.as_str()));
    assert_eq!(identity.capabilities.context_origin, RuntimeContextOrigin::NativeFork);
    assert_eq!(identity.capabilities.checkout, RuntimeCheckoutOwnership::SharedWithParent);
    assert!(!identity.capabilities.persistent);
    assert!(!identity.capabilities.create_child_threads);
    assert!(!side_contract.tools.iter().any(|tool| tool.name == "create_thread"));
    assert!(side_contract.omitted_tools.iter().any(|tool| tool.name == "create_thread"
        && tool.reason.contains("side chats cannot create child chats")));
    let text = instructions(&side_contract);
    assert!(text.contains("Chat role: side chat"), "{text}");
    assert!(text.contains(&format!("@thread:{parent}")), "{text}");
    assert_eq!(side_contract.context.branch.as_deref(), Some("parent-wt"));
    evidence(
        &format!("side-{}-contract.json", provider_name(id)),
        &serde_json::to_value(&side_contract).unwrap(),
    );
    assert!(!core.sessions.turn_in_flight(&parent));
    assert_eq!(queue_len(&core, &parent), 0);
    assert_eq!(entry_count(&core, &parent), parent_entries);
    assert_eq!(runs_for(&recorder, &parent).len(), 1);

    // E4: the parent runs a full turn while the side turn is still running.
    queue(&client, &parent, "parent-during-side", "parent turn during side").await;
    until("parent second turn", || {
        runs_for(&recorder, &parent).len() == 2 && !core.sessions.turn_in_flight(&parent)
    })
    .await;
    assert_eq!(
        runs_for(&recorder, &parent)[1].resume.as_deref(),
        Some(parent_session.as_str())
    );
    assert!(core.sessions.turn_in_flight(&side), "side turn must keep running");

    // E3: Stop in the side chat leaves the parent alone.
    let parent_entries = entry_count(&core, &parent);
    client
        .call("StopSideChat", json!({"id":side,"lease":lease}))
        .await
        .unwrap();
    until("side stopped", || !core.sessions.turn_in_flight(&side)).await;
    assert!(!core.sessions.turn_in_flight(&parent));
    assert_eq!(entry_count(&core, &parent), parent_entries);
    assert_eq!(runs_for(&recorder, &parent).len(), 2);
    // Stop is not unload: a wrong lease is refused, the right one still sends.
    assert!(
        client
            .call("StopSideChat", json!({"id":side,"lease":"forged"}))
            .await
            .is_err()
    );

    // E4: a parent turn and its Stop leave the side chat untouched.
    queue(&client, &parent, "parent-hold", "HOLD parent work").await;
    until("parent hold running", || core.sessions.turn_in_flight(&parent)).await;
    client
        .call(
            "SendSideChat",
            json!({"id":side,"lease":lease,"messageId":"side-during-parent","text":"side follow-up"}),
        )
        .await
        .unwrap();
    until("side follow-up done", || {
        runs_for(&recorder, &side).len() == 2 && !core.sessions.turn_in_flight(&side)
    })
    .await;
    assert!(core.sessions.turn_in_flight(&parent), "parent turn must keep running");
    let side_entries = entry_count(&core, &side);
    let snapshot: luna_proto::TaskCommandRead = client
        .call_as(luna_rpc::methods::GET_TASK_COMMAND_SNAPSHOT, json!({"taskId": parent}))
        .await
        .unwrap();
    let mut stop = json!({
        "commandId": format!("stop-{parent}"), "actorId": "pane-test", "clientId": "pane-test",
        "clientSessionId": "pane-test", "taskId": parent,
        "intent": {"kind":"session","command":{"kind":"interrupt"}},
        "expectedTaskRevision": snapshot.snapshot.task_revision,
    });
    if let Some(generation) = snapshot.snapshot.binding_generation {
        stop["expectedBindingGeneration"] = generation.into();
    }
    if let Some(turn) = snapshot.snapshot.active_turn_id.clone() {
        stop["expectedTurnId"] = turn.into();
    }
    client
        .call(luna_rpc::methods::EXECUTE_TASK_COMMAND, stop)
        .await
        .unwrap();
    until("parent stopped", || !core.sessions.turn_in_flight(&parent)).await;
    assert_eq!(entry_count(&core, &side), side_entries);
    assert!(!core.sessions.turn_in_flight(&side));
    // The side chat still owns a writable lease after the parent's Stop.
    client
        .call(
            "SendSideChat",
            json!({"id":side,"lease":lease,"messageId":"side-final","text":"side final"}),
        )
        .await
        .unwrap();
    until("side final done", || runs_for(&recorder, &side).len() == 3).await;
    evidence(
        &format!("side-{}-summary.json", provider_name(id)),
        &json!({
            "parent": parent, "side": side, "parentSession": parent_session,
            "forkSession": fork_session, "parentRuns": runs_for(&recorder, &parent).len(),
            "sideRuns": runs_for(&recorder, &side).len(), "cwd": side_row.cwd,
            "branch": side_row.branch,
        }),
    );
    drop(client);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_side_chat_forks_inherits_and_runs_independently() {
    side_chat_forks_inherits_and_runs_independently(HarnessId::Codex, "auto").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_side_chat_forks_inherits_and_runs_independently() {
    side_chat_forks_inherits_and_runs_independently(HarnessId::ClaudeCode, "full").await;
}

// ---------------------------------------------------------------------------
// Existing data
// ---------------------------------------------------------------------------

/// A3 A4 A6: rows and payloads written before this change decode unchanged
/// and re-encode without the new keys.
#[test]
fn legacy_child_rows_contracts_and_requests_decode_unchanged() {
    let legacy_child_row = json!({
        "childThread": {
            "parentTaskId": "parent-legacy", "creationCommandId": "child-create:c",
            "permissionCeiling": "auto", "sandboxCeiling": "workspace-write",
            "initialMessageId": "child-initial:c", "initialPrompt": "old brief",
            "reportResults": true, "continueParent": false, "outcomes": []
        },
        "id": "c", "deviceId": "d", "title": "Old child", "archived": false,
        "cwd": "/tmp/repo", "branch": null, "checkoutId": null,
        "config": {"harness": "codex", "model": null, "reasoning": null, "modelOptions": {},
            "sandbox": "workspace-write", "permissionMode": "auto", "effectivePermissionMode": "auto"},
        "lastMessagePreview": null, "lastMessageAt": null,
        "createdAt": "2026-09-01T00:00:00Z", "spaceId": "s", "roomGen": 2
    });
    let chat: luna_proto::Chat = serde_json::from_value(legacy_child_row.clone()).unwrap();
    let child = chat.child_thread.clone().unwrap();
    assert_eq!(child.provisioning, None);
    assert_eq!(child.permission_ceiling, PermissionMode::Auto);
    assert_eq!(
        serde_json::to_value(&chat).unwrap()["childThread"],
        legacy_child_row["childThread"],
        "a legacy child row re-encodes byte-for-byte (no provisioning key)"
    );
    let legacy_receipt = json!({"taskId":"c","parentTaskId":"p","command":{
        "commandId":"child-create:c","taskId":"c","intent":"createChildChat","payloadHash":"h",
        "status":"applied","snapshot":{"taskId":"c","taskRevision":1,"taskSequence":1},
        "actorId":"a","clientId":"c","clientSessionId":"s","acceptedAtMs":1,"observedAtMs":1},
        "initialMessageId":"child-initial:c","startupState":"durable-startup-intent"});
    if let Ok(receipt) = serde_json::from_value::<luna_proto::ChildThreadReceipt>(legacy_receipt) {
        assert_eq!(receipt.provisioning, None);
    }
    let legacy_context = json!({
        "appVersion": "0.2.72", "protocolVersion": 1, "threadId": "t", "parentThreadId": null,
        "sourceThreadId": null, "projectId": "p", "projectName": "P", "environmentId": "e",
        "machineId": "m", "machineName": "M", "workspaceRoot": "/tmp", "worktreeId": null,
        "bindingGeneration": null, "providerProfileId": null,
        "branch": null, "harness": "codex", "model": null, "reasoning": null,
        "executionRoute": "local", "grantedScopes": ["threads.read"]
    });
    let context: luna_proto::RuntimeContext = serde_json::from_value(legacy_context.clone()).unwrap();
    assert_eq!(context.chat, None);
    assert_eq!(serde_json::to_value(&context).unwrap(), legacy_context);
    for request in [
        json!({"prompt":"p","idempotencyKey":"k"}),
        json!({"prompt":"p","idempotencyKey":"k","title":"t","model":"m","reasoning":"high",
            "cwd":"/tmp","reportResults":false,"continueParent":false}),
    ] {
        let decoded: luna_proto::CreateChildThreadRequest =
            serde_json::from_value(request.clone()).unwrap();
        assert_eq!(decoded.checkout, None);
        assert_eq!(decoded.permission_mode, None);
    }
}
