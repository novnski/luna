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
    /// `(provider session, steer prompt)` for every accepted native steer.
    steers: Arc<Mutex<Vec<(String, String)>>>,
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
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
        ]
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
        Ok(["parent-model", "child-model", "cross-provider-model"]
            .into_iter()
            .map(|id| Model {
                id: id.into(),
                label: id.into(),
                description: None,
                reasoning_levels: vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High,
                ],
                options: vec![luna_proto::ModelOption {
                    id: "fastMode".into(),
                    label: "Fast mode".into(),
                    default_choice: "off".into(),
                    choices: vec![
                        luna_proto::ModelOptionChoice {
                            id: "off".into(),
                            label: "Off".into(),
                        },
                        luna_proto::ModelOptionChoice {
                            id: "on".into(),
                            label: "On".into(),
                        },
                    ],
                }],
            })
            .collect())
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
            // Holds the turn open: native steers are accepted and recorded,
            // Stop ends it as interrupted.
            let RunControls {
                interrupt,
                mut steering,
                ..
            } = controls;
            let steers = self.recorder.steers.clone();
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            tokio::spawn(async move {
                let _ = tx.send(Ok(started)).await;
                let _ = tx.send(Ok(text)).await;
                let status = loop {
                    tokio::select! {
                        _ = interrupt.cancelled() => break DoneStatus::Interrupted,
                        steer = steering.recv() => match steer {
                            Some(mut steer) => {
                                if let Some(ack) = steer.acceptance.take() {
                                    let _ = ack.send(luna_harness::SteerAcceptance::Accepted);
                                }
                                steers.lock().unwrap().push((session.clone(), steer.prompt.clone()));
                                let _ = tx.send(Ok(AgentEvent::Steered {
                                    user_message_id: steer.message_id.clone(),
                                    assistant_message_id: None,
                                    next_assistant_message_id: Some(uuid::Uuid::new_v4().to_string()),
                                })).await;
                                let _ = tx.send(Ok(AgentEvent::TextDelta {
                                    text: format!("STEERED {}", steer.prompt),
                                })).await;
                            }
                            None => break DoneStatus::Completed,
                        },
                    }
                };
                let _ = tx
                    .send(Ok(AgentEvent::Done {
                        status,
                        result: None,
                        error: None,
                        session_id: Some(session),
                    }))
                    .await;
            });
            return Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            })
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

async fn create_child(core: &EngineCore, parent: &str, request: Value) -> Result<Value, String> {
    let request = serde_json::from_value(request).map_err(|error| error.to_string())?;
    let receipt = core
        .rpc_service()
        .seed_child_for_qa(parent.into(), request)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::to_value(receipt).map_err(|error| error.to_string())
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
    assert_eq!(
        identity.capabilities.context_origin,
        RuntimeContextOrigin::Fresh
    );
    assert!(identity.capabilities.create_child_threads, "{identity:?}");
    assert!(instructions(&parent_contract).contains("Chat role: main"));
    evidence(
        "parent-main-contract.json",
        &serde_json::to_value(&parent_contract).unwrap(),
    );
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
    let receipt = create_child(&core, &parent, legacy_request).await.unwrap();
    let legacy = receipt["taskId"].as_str().unwrap().to_owned();
    let row = core.workspace.chat(&legacy).unwrap().unwrap();
    let child = row.child_thread.clone().unwrap();
    let provisioning = child.provisioning.clone().unwrap();
    assert_eq!(
        receipt["provisioning"],
        serde_json::to_value(&provisioning).unwrap()
    );
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
    until("legacy child run", || {
        !runs_for(&recorder, &legacy).is_empty()
    })
    .await;
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
    assert_eq!(
        child_contract.context.parent_thread_id.as_deref(),
        Some(parent.as_str())
    );
    assert_eq!(
        identity.capabilities.checkout,
        RuntimeCheckoutOwnership::SharedWithParent
    );
    assert_eq!(
        identity.capabilities.granted_access,
        Some(PermissionMode::Auto)
    );
    assert!(identity.capabilities.access_capped_by_parent);
    assert!(identity.capabilities.reports_to_parent);
    assert!(instructions(&child_contract).contains("Chat role: child chat"));
    evidence(
        "child-current-contract.json",
        &serde_json::to_value(&child_contract).unwrap(),
    );

    // A2: authority cannot be smuggled through request fields.
    for request in [
        json!({"prompt":"x","idempotencyKey":"smuggle-1","grantedByUser":true}),
        json!({"prompt":"x","idempotencyKey":"smuggle-2","accessGranted":true}),
        json!({"prompt":"x","idempotencyKey":"smuggle-3","checkout":{"kind":"sideways"}}),
        json!({"prompt":"x","idempotencyKey":"smuggle-4","permissionMode":"root"}),
    ] {
        assert!(
            create_child(&core, &parent, request.clone()).await.is_err(),
            "{request}"
        );
    }
    let unknown_in_checkout = json!({"prompt":"x","idempotencyKey":"smuggle-5",
        "checkout":{"kind":"newWorktree","branch":"luna/ok","grant":true}});
    assert!(
        create_child(&core, &parent, unknown_in_checkout)
            .await
            .is_err()
    );

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
            &core,
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
    assert!(create_child(&core, &parent, ambiguous).await.is_err());
    assert_eq!(child_dirs(repo_name), before_dirs);
    assert_eq!(git(&repo, &["branch", "--list"]), before_branches);

    // B9 B10: a new worktree on a generated branch from the parent's branch.
    let receipt = create_child(
        &core,
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
    assert!(
        wt_path.starts_with(worktrees_root().join(repo_name)),
        "{wt_path:?}"
    );
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
    assert_eq!(
        git(&repo, &["status", "--porcelain", "--untracked-files=no"]),
        ""
    );
    until("worktree child run", || {
        !runs_for(&recorder, &wt_child).is_empty()
    })
    .await;
    let run = runs_for(&recorder, &wt_child)[0].clone();
    assert_eq!(run.cwd, wt_path.to_str().unwrap());
    let wt_contract = contract(&run);
    assert_eq!(
        wt_contract
            .context
            .chat
            .as_ref()
            .unwrap()
            .capabilities
            .checkout,
        RuntimeCheckoutOwnership::OwnWorktree
    );
    assert_eq!(
        wt_contract.context.branch.as_deref(),
        Some(wt_branch.as_str())
    );
    assert!(instructions(&wt_contract).contains("its own git worktree and branch"));
    evidence(
        "child-worktree-row.json",
        &serde_json::to_value(&row).unwrap(),
    );

    // Named branch from an explicit base; a retry reuses it (B6).
    let named = json!({"prompt":"WT_NAMED","idempotencyKey":"wt-named",
        "checkout":{"kind":"newWorktree","branch":"feat/child-x","base":"feature"}});
    let first = create_child(&core, &parent, named.clone()).await.unwrap();
    let named_child = first["taskId"].as_str().unwrap().to_owned();
    let named_row = core.workspace.chat(&named_child).unwrap().unwrap();
    let named_path = PathBuf::from(named_row.cwd.clone().unwrap());
    assert_eq!(named_row.branch.as_deref(), Some("feat/child-x"));
    assert_eq!(git(&named_path, &["rev-parse", "HEAD"]), feature_sha);
    let dirs = child_dirs(repo_name);
    let again = create_child(&core, &parent, named).await.unwrap();
    assert_eq!(again["taskId"], first["taskId"]);
    assert_eq!(
        child_dirs(repo_name),
        dirs,
        "a retry must not mint a second worktree"
    );

    // C6: the creation dialog is the user's own grant above the ceiling.
    let granted = create_child(
        &core,
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
    assert_eq!(
        provisioning.requested_permission_mode,
        Some(PermissionMode::Full)
    );
    assert!(provisioning.granted_by_user && !provisioning.escalation_denied);
    assert_eq!(child.permission_ceiling, PermissionMode::Full);
    assert_eq!(child.sandbox_ceiling, None);
    assert_eq!(
        row.config.unwrap().effective_permission_mode,
        PermissionMode::Full
    );
    until("granted child run", || {
        !runs_for(&recorder, &granted).is_empty()
    })
    .await;
    let run = runs_for(&recorder, &granted)[0].clone();
    assert_eq!(
        run.permission_policy.as_ref().unwrap().effective_mode,
        PermissionMode::Full
    );
    let caps = contract(&run).context.chat.unwrap().capabilities;
    assert!(caps.access_granted_by_user && !caps.access_capped_by_parent);

    // C2: a lower request is granted as asked and stays parent-capped.
    let lower = create_child(
        &core,
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
        &core,
        &parent,
        json!({"prompt":"EXPLICIT","idempotencyKey":"explicit-model",
            "model":"child-model","reasoning":"low"}),
    )
    .await
    .unwrap();
    let explicit = explicit["taskId"].as_str().unwrap().to_owned();
    let explicit_config = core
        .workspace
        .chat(&explicit)
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    assert_eq!(explicit_config.model.as_deref(), Some("child-model"));
    assert_eq!(explicit_config.reasoning, Some(ReasoningLevel::Low));
    until("explicit child run", || {
        !runs_for(&recorder, &explicit).is_empty()
    })
    .await;
    assert_eq!(
        runs_for(&recorder, &explicit)[0].model.as_deref(),
        Some("child-model")
    );

    // C5 (dispatch): lowering the parent re-caps ordinary children (the Auto
    // child is refused rather than silently run as hidden Ask) but not the
    // one the user granted.
    let mut lowered: luna_proto::ChatConfig =
        serde_json::from_value(config(HarnessId::Codex, "ask", "parent-model", "high")).unwrap();
    lowered.effective_permission_mode = PermissionMode::Ask;
    core.workspace.set_chat_config(&parent, &lowered).unwrap();
    for (chat, message) in [(&legacy, "legacy-2"), (&granted, "granted-2")] {
        until("child idle", || !core.sessions.turn_in_flight(chat)).await;
        queue(&client, chat, message, "second turn").await;
    }
    until("granted child second run", || {
        runs_for(&recorder, &granted).len() == 2
    })
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        runs_for(&recorder, &legacy).iter().all(|run| run
            .permission_policy
            .as_ref()
            .unwrap()
            .effective_mode
            <= PermissionMode::Auto),
        "a parent-capped child never runs above the parent"
    );
    assert_eq!(
        runs_for(&recorder, &legacy).len(),
        1,
        "capped Auto child is refused"
    );
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
        &core,
        &parent,
        json!({"prompt":"€".repeat(7_000),"idempotencyKey":"oversized-wt",
            "checkout":{"kind":"newWorktree"}}),
    )
    .await
    .expect_err("oversized row");
    assert!(oversized.contains("too large"), "{oversized}");
    assert_eq!(child_dirs(repo_name), dirs);
    assert_eq!(git(&repo, &["branch", "--list"]), branches);
    assert_eq!(
        git(&repo, &["worktree", "list"]).lines().count(),
        dirs.len() + 1
    );

    // B1: a project that is not a git repository cannot get a worktree.
    let plain = base.join("plain-project");
    std::fs::create_dir_all(&plain).unwrap();
    core.workspace
        .create_space(
            "space-plain",
            &core.device_id,
            plain.to_str().unwrap(),
            None,
            false,
        )
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
        &core,
        &plain_parent,
        json!({"prompt":"NO_GIT","idempotencyKey":"no-git","checkout":{"kind":"newWorktree"}}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("not a git repository"), "{error}");
    assert!(!core.workspace.read_chats().unwrap().iter().any(|chat| {
        chat.child_thread
            .as_ref()
            .is_some_and(|c| c.initial_prompt == "NO_GIT")
    }));

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

/// A rejected first send must not consume or echo the seed. Goals and native
/// reviews carry the same frozen parent context as ordinary sends, without
/// changing the objective, review target, or visible user message.
#[tokio::test(flavor = "multi_thread")]
async fn snapshot_side_context_survives_goals_reviews_and_rejected_first_send() {
    for provider in [HarnessId::Codex, HarnessId::ClaudeCode] {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repo = base.join("snapshot-side");
        init_repo(&repo);
        let parent_wt = base.join("snapshot-parent-wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "snapshot-parent",
                parent_wt.to_str().unwrap(),
                "main",
            ],
        );
        let (core, recorder) = assemble(&base, provider);
        let client = luna_rpc::memory_client(core.rpc_service());
        core.workspace
            .create_space(
                "snapshot-space",
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
            "snapshot-space",
            &parent_wt,
            config(provider, "ask", "parent-model", "low"),
        )
        .await;
        core.doc_host
            .open(&parent)
            .unwrap()
            .write_user_message("snapshot-origin", "SNAPSHOT_PARENT_927", 1)
            .unwrap();
        core.workspace
            .set_chat_branch(&parent, "snapshot-parent")
            .unwrap();
        queue(&client, &parent, "parent-bound", "BIND_PARENT").await;
        until("snapshot parent idle", || {
            runs_for(&recorder, &parent).len() == 1 && !core.sessions.turn_in_flight(&parent)
        })
        .await;
        let parent_row = core.workspace.chat(&parent).unwrap().unwrap();
        assert!(parent_row.checkout_id.is_some() && parent_row.source_context.is_some());
        for review in [false, true] {
            let created: Value = client.call("CreateSideChat", json!({
                "parentId":parent, "requestId":uuid::Uuid::new_v4().to_string(),
                "forceSnapshot":true, "originMessageId":"snapshot-origin", "anchorText":"SELECTED_QUOTE_813",
            })).await.unwrap();
            let side = created["meta"]["id"].as_str().unwrap();
            let side_row = core.workspace.chat(side).unwrap().unwrap();
            assert_eq!(side_row.cwd, parent_row.cwd);
            assert_eq!(side_row.branch, parent_row.branch);
            assert_eq!(
                side_row.checkout_id, parent_row.checkout_id,
                "snapshot retains checkout before any send"
            );
            assert_eq!(side_row.source_context, parent_row.source_context);
            let make_request = |objective: &str| RunRequest {
                prompt: "visible first input".into(),
                harness: Some(provider),
                provider_account_id: side_row
                    .config
                    .as_ref()
                    .unwrap()
                    .provider_account_id
                    .clone(),
                model: Some("parent-model".into()),
                reasoning: Some(ReasoningLevel::Low),
                model_options: Default::default(),
                cwd: parent_wt.to_str().unwrap().into(),
                sandbox: luna_proto::SandboxLevel::WorkspaceWrite,
                auto_approve: false,
                permission_mode: PermissionMode::Ask,
                parent_permission_ceiling: None,
                environment_permission_ceiling: None,
                permission_policy: None,
                resume: None,
                attachments: vec![],
                worktree: None,
                input_items: vec![],
                review: None,
                goal: Some(luna_proto::GoalStartRequest {
                    objective: objective.into(),
                    token_budget: None,
                }),
            };
            let rejected = core
                .sessions
                .dispatch(
                    side,
                    provider,
                    make_request(""),
                    Some("rejected-first".into()),
                )
                .await;
            assert!(rejected.is_err());
            assert_eq!(
                entry_count(&core, side),
                0,
                "invalid first goal must not leave an echo"
            );
            let mut request = make_request("USE_SELECTED_CONTEXT");
            if review {
                request.goal = None;
                request.review = Some(luna_proto::NativeReviewRequest {
                    target: luna_proto::ReviewTarget::Custom {
                        instructions: "REVIEW_SELECTED_CONTEXT".into(),
                    },
                });
            }
            core.sessions
                .dispatch(
                    side,
                    provider,
                    request.clone(),
                    Some("accepted-first".into()),
                )
                .await
                .unwrap();
            until("seeded side run", || {
                !runs_for(&recorder, side).is_empty() && !core.sessions.turn_in_flight(side)
            })
            .await;
            let run = runs_for(&recorder, side)[0].clone();
            let seeded = contract(&run);
            let section = seeded
                .instructions
                .iter()
                .find(|section| section.name == "Side chat parent context")
                .expect("parent context delivered outside rewritten prompt");
            assert!(
                section.text.contains("SNAPSHOT_PARENT_927")
                    && section.text.contains("SELECTED_QUOTE_813")
            );
            if !review {
                assert_eq!(run.prompt, "USE_SELECTED_CONTEXT");
                assert_eq!(run.goal, request.goal);
            } else if provider == HarnessId::Codex {
                assert_eq!(run.review, request.review);
            } else {
                assert_eq!(run.prompt, "REVIEW_SELECTED_CONTEXT");
            }
            queue(&client, side, "follow-up", "FOLLOW_UP").await;
            until("stable side contract", || {
                runs_for(&recorder, side).len() == 2 && !core.sessions.turn_in_flight(side)
            })
            .await;
            let follow_up = contract(&runs_for(&recorder, side)[1]);
            evidence(
                "snapshot-context-contracts.json",
                &json!({"first":seeded,"followUp":follow_up}),
            );
            assert_eq!(
                Some(section),
                follow_up
                    .instructions
                    .iter()
                    .find(|section| section.name == "Side chat parent context"),
                "frozen seed is stable after binding publication"
            );
            let mut held = runs_for(&recorder, side)[1].clone();
            held.prompt = "HOLD_WITH_CONTEXT".into();
            core.sessions
                .dispatch(
                    side,
                    provider,
                    held.clone(),
                    Some("held-with-context".into()),
                )
                .await
                .unwrap();
            until("warm seeded side", || {
                runs_for(&recorder, side).len() == 3 && core.sessions.turn_in_flight(side)
            })
            .await;
            held.prompt = "STEER_WITH_CONTEXT".into();
            core.sessions
                .dispatch(side, provider, held, Some("steer-with-context".into()))
                .await
                .unwrap();
            until("seeded steer accepted", || {
                recorder
                    .steers
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(_, prompt)| prompt == "STEER_WITH_CONTEXT")
            })
            .await;
            core.sessions.interrupt_for_stop(side).await.unwrap();
            until("seeded side stopped", || {
                !core.sessions.turn_in_flight(side)
            })
            .await;
        }
        drop(client);
        core.shutdown().await;
    }
}

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
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "parent-wt",
            parent_wt.to_str().unwrap(),
            "main",
        ],
    );
    let (core, recorder) = assemble(&base, id);
    let client = luna_rpc::memory_client(core.rpc_service());
    core.workspace
        .create_space(
            "space-side",
            &core.device_id,
            repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let parent = uuid::Uuid::new_v4().to_string();
    let mut parent_config = config(id, mode, "parent-model", "medium");
    // The product Full picker changes mode, not this historical field.
    // Native parent runs already derive DangerFullAccess from effective Full.
    if mode == "full" {
        parent_config["sandbox"] = json!("workspace-write");
    }
    create_chat(&client, &parent, "space-side", &parent_wt, parent_config).await;
    core.workspace
        .set_chat_branch(&parent, "parent-wt")
        .unwrap();
    queue(
        &client,
        &parent,
        "parent-context",
        "PARENT_CONTEXT_SENTINEL",
    )
    .await;
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
    let lease = created["lease"].as_str().unwrap_or_default().to_owned();
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
    assert_eq!(
        identity.capabilities.context_origin,
        RuntimeContextOrigin::NativeFork
    );
    assert_eq!(
        identity.capabilities.checkout,
        RuntimeCheckoutOwnership::SharedWithParent
    );
    assert!(identity.capabilities.persistent);
    assert!(identity.capabilities.create_child_threads);
    assert!(!identity.capabilities.access_capped_by_parent);
    assert!(
        side_contract
            .tools
            .iter()
            .any(|tool| tool.name == "create_thread")
    );
    assert!(
        !side_contract
            .omitted_tools
            .iter()
            .any(|tool| tool.name == "create_thread"
                && tool.reason.contains("side chats cannot create child chats"))
    );
    let text = instructions(&side_contract);
    assert!(text.contains("Chat role: fork thread"), "{text}");
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
    queue(
        &client,
        &parent,
        "parent-during-side",
        "parent turn during side",
    )
    .await;
    until("parent second turn", || {
        runs_for(&recorder, &parent).len() == 2 && !core.sessions.turn_in_flight(&parent)
    })
    .await;
    assert_eq!(
        runs_for(&recorder, &parent)[1].resume.as_deref(),
        Some(parent_session.as_str())
    );
    assert!(
        core.sessions.turn_in_flight(&side),
        "side turn must keep running"
    );

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
    // Stop does not end the chat and no view token is required.
    assert!(
        client
            .call("StopSideChat", json!({"id":side}))
            .await
            .is_ok()
    );

    // E4: a parent turn and its Stop leave the side chat untouched.
    queue(&client, &parent, "parent-hold", "HOLD parent work").await;
    until("parent hold running", || {
        core.sessions.turn_in_flight(&parent)
    })
    .await;
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
    assert!(
        core.sessions.turn_in_flight(&parent),
        "parent turn must keep running"
    );
    let side_entries = entry_count(&core, &side);
    let snapshot: luna_proto::TaskCommandRead = client
        .call_as(
            luna_rpc::methods::GET_TASK_COMMAND_SNAPSHOT,
            json!({"taskId": parent}),
        )
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
async fn codex_full_side_chat_inherits_effective_access_with_legacy_sandbox() {
    side_chat_forks_inherits_and_runs_independently(HarnessId::Codex, "full").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_side_chat_forks_inherits_and_runs_independently() {
    side_chat_forks_inherits_and_runs_independently(HarnessId::ClaudeCode, "full").await;
}

// ---------------------------------------------------------------------------
// Side chats on the ordinary composer path (task commands and per-chat RPCs)
// ---------------------------------------------------------------------------

/// A valid 1×1 PNG: native image admission sniffs the real format.
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x64, 0x60, 0xF8, 0x5F,
    0x0F, 0x00, 0x02, 0x87, 0x01, 0x80, 0xEB, 0x47, 0xBA, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// An ordinary task command, fenced on the task's current snapshot the way the
/// composer's `task_commands::execute` does.
async fn command(client: &RpcClient, task: &str, id: &str, intent: Value) -> Result<Value, String> {
    let snapshot: luna_proto::TaskCommandRead = client
        .call_as(
            luna_rpc::methods::GET_TASK_COMMAND_SNAPSHOT,
            json!({"taskId": task}),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut envelope = json!({
        "commandId": id, "actorId": "pane-test", "clientId": "pane-test",
        "clientSessionId": "pane-test", "taskId": task, "intent": intent,
        "expectedTaskRevision": snapshot.snapshot.task_revision,
    });
    if let Some(generation) = snapshot.snapshot.binding_generation {
        envelope["expectedBindingGeneration"] = generation.into();
    }
    if let Some(turn) = snapshot.snapshot.active_turn_id {
        envelope["expectedTurnId"] = turn.into();
    }
    client
        .call(luna_rpc::methods::EXECUTE_TASK_COMMAND, envelope)
        .await
        .map_err(|error| error.to_string())
}

async fn queue_with(client: &RpcClient, chat: &str, params: Value) -> Result<Value, String> {
    let mut params = params;
    params["chatId"] = chat.into();
    client
        .call("QueueMessage", params)
        .await
        .map_err(|error| error.to_string())
}

/// Failure context for one chat: queue rows, transcript and session status.
fn chat_diag(core: &EngineCore, recorder: &Recorder, chat: &str) -> String {
    let mut out = format!(
        "status {:?}; in flight {}\n",
        core.sessions.session_status(chat).map(|s| s.status),
        core.sessions.turn_in_flight(chat)
    );
    if let Ok(handle) = core.doc_host.open(chat) {
        for row in handle.doc().read_queue().unwrap_or_default() {
            out.push_str(&format!(
                "  queue {} {:?} hold={:?} gate={:?}\n",
                row.id, row.status, row.hold_reason, row.delivery_gate
            ));
        }
        for entry in handle.doc().read_entries().unwrap_or_default() {
            out.push_str(&format!(
                "  entry {:?} {} {:?}\n",
                entry.role, entry.id, entry.parts
            ));
        }
    }
    out.push_str(&run_summary(recorder));
    out
}

async fn until_diag(what: &str, mut ready: impl FnMut() -> bool, diag: impl Fn() -> String) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}\n{}",
            diag()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn until_diag_side(
    core: &EngineCore,
    recorder: &Recorder,
    side: &str,
    what: &str,
    ready: impl FnMut() -> bool,
) {
    until_diag(what, ready, || chat_diag(core, recorder, side)).await;
}

fn queued_ids(core: &EngineCore, chat: &str) -> Vec<String> {
    core.doc_host
        .open(chat)
        .and_then(|handle| Ok(handle.doc().read_queue()?))
        .map(|rows| rows.into_iter().map(|row| row.id).collect())
        .unwrap_or_default()
}

/// G1–G7, G10 for one provider identity: queue, steer, attachments, the
/// ordinary Stop, model/effort and access changes on a leased side chat,
/// while the parent's run, queue, config and transcript stay untouched.
async fn side_chat_ordinary_commands(id: HarnessId, mode: &str) {
    use base64::Engine as _;
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let repo = base.join(format!("repo-pane-side-ordinary-{}", provider_name(id)));
    init_repo(&repo);
    let (core, recorder) = assemble(&base, id);
    let client = luna_rpc::memory_client(core.rpc_service());
    core.workspace
        .create_space(
            "space-ordinary",
            &core.device_id,
            repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let parent = uuid::Uuid::new_v4().to_string();
    let mut parent_config = config(id, mode, "parent-model", "medium");
    if mode == "full" {
        parent_config["sandbox"] = json!("workspace-write");
    }
    create_chat(&client, &parent, "space-ordinary", &repo, parent_config).await;
    core.workspace.set_chat_branch(&parent, "main").unwrap();
    queue(&client, &parent, "parent-context", "PARENT_CONTEXT").await;
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
    tokio::time::sleep(Duration::from_millis(300)).await;
    let parent_before = core.workspace.chat(&parent).unwrap().unwrap();
    let parent_entries = entry_count(&core, &parent);
    let parent_untouched =
        |core: &EngineCore, recorder: &Recorder, step: &str| {
            assert_eq!(runs_for(recorder, &parent).len(), 1, "{step}: parent ran");
            assert!(
                !core.sessions.turn_in_flight(&parent),
                "{step}: parent busy"
            );
            assert_eq!(queue_len(core, &parent), 0, "{step}: parent queue");
            assert_eq!(
                entry_count(core, &parent),
                parent_entries,
                "{step}: parent transcript"
            );
            assert_eq!(
                core.workspace.chat(&parent).unwrap().unwrap().config,
                parent_before.config,
                "{step}: parent config"
            );
            assert!(
                recorder.steers.lock().unwrap().iter().all(|(session, _)| {
                    Some(session) != parent_before.harness_session_id.as_ref()
                }),
                "{step}: parent steered"
            );
        };
    let created: Value = client
        .call(
            "CreateSideChat",
            json!({"parentId": parent, "requestId": uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    let side = created["meta"]["id"].as_str().unwrap().to_owned();
    let lease = created["lease"].as_str().unwrap_or_default().to_owned();
    let fork_session = core
        .workspace
        .chat(&side)
        .unwrap()
        .unwrap()
        .harness_session_id
        .unwrap();

    let nested_request = uuid::Uuid::new_v4().to_string();
    let nested_fork = client.call("CreateSideChat", json!({"parentId":side,"requestId":nested_request})).await.unwrap_or_else(|error| {
        let receipt = core.doc_host.task_command_receipt(&format!("side-chat-create:{nested_request}")).unwrap();
        evidence("nested-fork-error.json", &json!({"error":error.to_string(),"receipt":receipt,"entries":core.doc_host.open(&side).unwrap().doc().read_entries().unwrap(),"checkpoints":recorder.checkpoints.lock().unwrap().clone()}));
        panic!("nested native fork failed: {error}; receipt={receipt:?}")
    });
    assert_eq!(nested_fork["meta"]["contextMode"], "nativeFork");

    // pane-ui gate: the capability is advertised and the live side chat is
    // published in WatchChats with ordinary project membership and provenance.
    let info: Value = client
        .call(luna_rpc::methods::ENGINE_INFO, json!({}))
        .await
        .unwrap();
    assert!(
        info["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|capability| capability == "side-chat-task-commands-v1"),
        "{info}"
    );
    let mut chats = client
        .subscribe(luna_rpc::methods::WATCH_CHATS, json!({}))
        .await
        .unwrap();
    let rows = tokio::time::timeout(Duration::from_secs(5), chats.recv())
        .await
        .unwrap()
        .unwrap();
    let side_published = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == side.as_str())
        .cloned()
        .expect("live side chat row published");
    assert_eq!(
        side_published["spaceId"], "space-ordinary",
        "{side_published}"
    );
    assert_eq!(side_published["sideChatParentId"], parent.as_str());
    assert_eq!(side_published["config"]["model"], "parent-model");
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == parent.as_str() && row["spaceId"].is_string())
    );
    drop(chats);
    // The per-chat reads the full composer binds accept the side id.
    for method in [
        luna_rpc::methods::WATCH_DOC_MESSAGES,
        luna_rpc::methods::WATCH_QUEUE,
        luna_rpc::methods::WATCH_NATIVE_TASKS,
    ] {
        let stream = client
            .subscribe_checked(method, json!({"chatId": side}))
            .await;
        assert!(stream.is_ok(), "{method}: {:?}", stream.err());
    }
    client
        .call(
            luna_rpc::methods::GET_COMPOSER_DRAFT,
            json!({"taskId": side}),
        )
        .await
        .unwrap();

    // G3: an attachment through the ordinary upload + QueueMessage path.
    client
        .call(
            luna_rpc::methods::UPLOAD_CHUNK,
            json!({"uploadId":"att-side","seq":0,
                "data": base64::engine::general_purpose::STANDARD.encode(PNG_1X1)}),
        )
        .await
        .unwrap();
    client
        .call(
            luna_rpc::methods::UPLOAD_COMMIT,
            json!({"uploadId":"att-side","fileName":"pic.png"}),
        )
        .await
        .unwrap();
    queue_with(
        &client,
        &side,
        json!({"messageId":"side-q1","holdForTurnEnd":true,
            "text":"HOLD side work\n\nAttached images (local files — open them to view):\n- pending://att-side/pic.png",
            "attachments":["pending://att-side/pic.png"]}),
    )
    .await
    .unwrap();
    until_diag(
        "side ordinary run",
        || core.sessions.turn_in_flight(&side) && !runs_for(&recorder, &side).is_empty(),
        || chat_diag(&core, &recorder, &side),
    )
    .await;
    let run = runs_for(&recorder, &side)[0].clone();
    assert!(run.prompt.contains("HOLD side work"), "{}", run.prompt);
    assert_eq!(run.attachments.len(), 1, "{:?}", run.attachments);
    assert!(
        run.attachments[0].ends_with("att-side-pic.png"),
        "{:?}",
        run.attachments
    );
    assert_eq!(run.resume.as_deref(), Some(fork_session.as_str()));
    assert_eq!(
        contract(&run).context.chat.unwrap().role,
        RuntimeChatRole::Side
    );
    parent_untouched(&core, &recorder, "attachment run");

    // G4: steer reaches the running side turn only.
    queue_with(
        &client,
        &side,
        json!({"messageId":"side-steer","text":"STEER into side","holdForTurnEnd":true,
            "followUpBehavior":"steer"}),
    )
    .await
    .unwrap();
    until_diag_side(&core, &recorder, &side, "side steer", || {
        recorder
            .steers
            .lock()
            .unwrap()
            .iter()
            .any(|(session, prompt)| session == &fork_session && prompt == "STEER into side")
    })
    .await;
    assert!(core.sessions.turn_in_flight(&side));
    parent_untouched(&core, &recorder, "steer");

    // G4: queue while the side turn runs holds in the side queue only.
    queue_with(
        &client,
        &side,
        json!({"messageId":"side-q2","text":"QUEUED after hold","holdForTurnEnd":true}),
    )
    .await
    .unwrap();
    until_diag_side(&core, &recorder, &side, "side queue holds", || {
        queued_ids(&core, &side).contains(&"side-q2".to_string())
    })
    .await;
    parent_untouched(&core, &recorder, "queue");

    // G5: the ordinary Stop task command stops only the side turn.
    command(
        &client,
        &side,
        "side-stop",
        json!({"kind":"session","command":{"kind":"interrupt"}}),
    )
    .await
    .unwrap();
    until_diag_side(&core, &recorder, &side, "side stopped", || {
        !core.sessions.turn_in_flight(&side)
    })
    .await;
    parent_untouched(&core, &recorder, "stop");
    // The held follow-up still belongs to the side chat; send it now if the
    // queue was paused by Stop.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline
        && !runs_for(&recorder, &side)
            .iter()
            .any(|run| run.prompt.contains("QUEUED after hold"))
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !runs_for(&recorder, &side)
        .iter()
        .any(|run| run.prompt.contains("QUEUED after hold"))
    {
        command(
            &client,
            &side,
            "side-send-q2",
            json!({"kind":"sendQueuedNow","messageId":"side-q2"}),
        )
        .await
        .unwrap();
    }
    until_diag_side(&core, &recorder, &side, "queued side follow-up ran", || {
        runs_for(&recorder, &side)
            .iter()
            .any(|run| run.prompt.contains("QUEUED after hold"))
            && !core.sessions.turn_in_flight(&side)
    })
    .await;

    // Like a main chat, Stop freezes the side queue until a fresh
    // user-authored turn: the composer's idle send is a session `run`.
    let side_row = core.workspace.chat(&side).unwrap().unwrap();
    let side_config_now = side_row.config.clone().unwrap();
    let fresh = RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: "fresh turn after stop".into(),
        harness: Some(id),
        provider_account_id: side_config_now.provider_account_id.clone(),
        model: side_config_now.model.clone(),
        reasoning: side_config_now.reasoning,
        model_options: Default::default(),
        cwd: side_row.cwd.clone().unwrap(),
        sandbox: side_config_now.sandbox,
        auto_approve: false,
        permission_mode: side_config_now.permission_mode,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    };
    command(
        &client,
        &side,
        "side-fresh",
        json!({"kind":"session","command":{"kind":"run","messageId":"side-fresh","request": fresh}}),
    )
    .await
    .unwrap();
    until_diag_side(&core, &recorder, &side, "fresh side turn", || {
        runs_for(&recorder, &side)
            .iter()
            .any(|run| run.prompt == "fresh turn after stop")
            && !core.sessions.turn_in_flight(&side)
    })
    .await;
    parent_untouched(&core, &recorder, "fresh turn");

    // G6: model/effort change on the side chat applies to its next turn only.
    let mut side_config = core.workspace.chat(&side).unwrap().unwrap().config.unwrap();
    side_config.model = Some("side-model".into());
    side_config.reasoning = Some(ReasoningLevel::Low);
    command(
        &client,
        &side,
        "side-model",
        json!({"kind":"setModel","config": side_config}),
    )
    .await
    .unwrap();
    let before = runs_for(&recorder, &side).len();
    queue_with(
        &client,
        &side,
        json!({"messageId":"side-q3","text":"after model change","holdForTurnEnd":true}),
    )
    .await
    .unwrap();
    until_diag_side(&core, &recorder, &side, "side model run", || {
        runs_for(&recorder, &side).len() == before + 1 && !core.sessions.turn_in_flight(&side)
    })
    .await;
    let run = runs_for(&recorder, &side)[before].clone();
    assert_eq!(run.model.as_deref(), Some("side-model"));
    assert_eq!(run.reasoning, Some(ReasoningLevel::Low));
    parent_untouched(&core, &recorder, "model change");

    // Explicit user access changes affect the fork independently of its source.
    command(
        &client,
        &side,
        "side-ask",
        json!({"kind":"setPermission","mode":"ask"}),
    )
    .await
    .unwrap();
    let before = runs_for(&recorder, &side).len();
    queue_with(
        &client,
        &side,
        json!({"messageId":"side-q4","text":"after access change","holdForTurnEnd":true}),
    )
    .await
    .unwrap();
    until_diag_side(&core, &recorder, &side, "side ask run", || {
        runs_for(&recorder, &side).len() == before + 1
    })
    .await;
    assert_eq!(
        runs_for(&recorder, &side)[before]
            .permission_policy
            .as_ref()
            .unwrap()
            .effective_mode,
        PermissionMode::Ask
    );
    until_diag_side(&core, &recorder, &side, "side idle", || {
        !core.sessions.turn_in_flight(&side)
    })
    .await;
    if mode != "full" {
        command(
            &client,
            &side,
            "side-full",
            json!({"kind":"setPermission","mode":"full"}),
        )
        .await
        .unwrap();
        let before = runs_for(&recorder, &side).len();
        queue_with(
            &client,
            &side,
            json!({"messageId":"side-q5","text":"above parent","holdForTurnEnd":true}),
        )
        .await
        .unwrap();
        until_diag_side(&core, &recorder, &side, "side capped run", || {
            runs_for(&recorder, &side).len() == before + 1
        })
        .await;
        let policy = runs_for(&recorder, &side)[before]
            .permission_policy
            .clone()
            .unwrap();
        assert_eq!(
            policy.effective_mode,
            PermissionMode::Full,
            "the user grants this ordinary fork its own access"
        );
        until_diag_side(&core, &recorder, &side, "side idle", || {
            !core.sessions.turn_in_flight(&side)
        })
        .await;
    }
    parent_untouched(&core, &recorder, "access change");

    // Organization uses the normal task-command receipts. Invalid account
    // choices fail through provider validation, not a side-role ban.
    for (key, intent) in [("fork-pin", json!({"kind":"setPinned","pinned":true}))] {
        let receipt = command(&client, &side, key, intent).await.unwrap();
        assert_eq!(receipt["status"], "applied", "{receipt}");
    }
    let refused = command(
        &client,
        &side,
        "fork-invalid-account",
        json!({"kind":"setProviderAccount","accountId":"other"}),
    )
    .await
    .unwrap();
    assert_ne!(refused["status"], "applied");
    assert!(!refused.to_string().contains("keeps its parent's provider"));

    // G10: the legacy side-panel send still works alongside.
    client
        .call(
            "SendSideChat",
            json!({"id":side,"lease":lease,"messageId":"side-legacy","text":"legacy send"}),
        )
        .await
        .unwrap();
    until_diag_side(&core, &recorder, &side, "legacy side send", || {
        runs_for(&recorder, &side)
            .iter()
            .any(|run| run.prompt == "legacy send")
    })
    .await;
    until_diag_side(&core, &recorder, &side, "side idle", || {
        !core.sessions.turn_in_flight(&side)
    })
    .await;
    // The legacy send now honours the side chat's own model change.
    assert_eq!(
        runs_for(&recorder, &side)
            .iter()
            .find(|run| run.prompt == "legacy send")
            .unwrap()
            .model
            .as_deref(),
        Some("side-model")
    );

    // RenewSideChat keeps the lease without a history read.
    let renewed: Value = client
        .call("RenewSideChat", json!({"id":side,"lease":lease}))
        .await
        .unwrap();
    assert_eq!(renewed["state"], "active");

    // An old client's view close is compatible and has no lifecycle effect.
    let binding_before_close = core
        .task_identity
        .view(&side)
        .unwrap()
        .current_binding
        .unwrap();
    client
        .call("UnloadSideChat", json!({"id":side,"lease":lease}))
        .await
        .unwrap();
    queue_with(
        &client,
        &side,
        json!({"messageId":"side-late","text":"after closing view","holdForTurnEnd":true}),
    )
    .await
    .unwrap();
    until_diag_side(&core, &recorder, &side, "send after view close", || {
        runs_for(&recorder, &side)
            .iter()
            .any(|run| run.prompt == "after closing view")
            && !core.sessions.turn_in_flight(&side)
    })
    .await;
    let binding_after_close = core
        .task_identity
        .view(&side)
        .unwrap()
        .current_binding
        .unwrap();
    assert_eq!(
        binding_after_close.binding_id,
        binding_before_close.binding_id
    );
    assert_eq!(
        binding_after_close.generation,
        binding_before_close.generation
    );
    assert_eq!(binding_after_close.provider_task_id, fork_session);
    assert!(
        client
            .subscribe_checked(
                luna_rpc::methods::WATCH_DOC_MESSAGES,
                json!({"chatId":side})
            )
            .await
            .is_ok()
    );
    client
        .call("GetChatWorkbench", json!({"mainChatId":side}))
        .await
        .unwrap();
    let blank_child = client.call("CreateChildTab", json!({"parentTaskId":side,"idempotencyKey":"fork-blank-child","title":"Child from fork","provider":id,"model":"child-model","reasoning":"medium"})).await.unwrap();
    assert_eq!(blank_child["startupState"], "awaiting-first-message");
    assert!(runs_for(&recorder, blank_child["taskId"].as_str().unwrap()).is_empty());
    // Steered display segments without a checkpoint must fail before any
    // provider mutation; the native-fork alternative remains explicit.
    let rejected_request = uuid::Uuid::new_v4().to_string();
    let provider_operations = recorder.sequence.load(Ordering::SeqCst);
    assert!(
        client
            .call(
                "CreateSideChat",
                json!({"parentId":side,"requestId":rejected_request})
            )
            .await
            .is_err()
    );
    let rejected = core
        .doc_host
        .task_command_receipt(&format!("side-chat-create:{rejected_request}"))
        .unwrap()
        .unwrap();
    assert_ne!(rejected.status, luna_proto::CommandReceiptStatus::Pending);
    assert_eq!(
        recorder.sequence.load(Ordering::SeqCst),
        provider_operations,
        "unmapped history is rejected before native effects"
    );
    parent_untouched(&core, &recorder, "after view close and nested creation");
    let deleted = command(
        &client,
        &parent,
        "delete-fork-source",
        json!({"kind":"delete"}),
    )
    .await
    .unwrap();
    assert_eq!(deleted["status"], "applied", "{deleted}");
    assert!(core.workspace.chat(&side).unwrap().is_some());
    queue_with(&client, &side, json!({"messageId":"side-source-gone","text":"after source deletion","holdForTurnEnd":true})).await.unwrap();
    until_diag_side(
        &core,
        &recorder,
        &side,
        "send after source deletion",
        || {
            runs_for(&recorder, &side)
                .iter()
                .any(|run| run.prompt == "after source deletion")
                && !core.sessions.turn_in_flight(&side)
        },
    )
    .await;
    assert_eq!(
        runs_for(&recorder, &side).last().unwrap().resume.as_deref(),
        Some(fork_session.as_str())
    );
    // Mismatched native identity is held before any direct send effect.
    // ClearContext then starts a fresh generation for the first legacy send.
    let nested_id = nested_fork["meta"]["id"].as_str().unwrap();
    let nested_binding = core
        .task_identity
        .view(nested_id)
        .unwrap()
        .current_binding
        .unwrap();
    let nested_cwd = core
        .workspace
        .chat(nested_id)
        .unwrap()
        .unwrap()
        .cwd
        .unwrap();
    core.workspace
        .set_chat_harness_session(nested_id, "mismatched-native-session", &nested_cwd);
    let blocked = client
        .call(
            "SendSideChat",
            json!({"id":nested_id,"messageId":"mismatch-held","text":"must not run"}),
        )
        .await
        .unwrap_err();
    assert!(
        blocked.to_string().contains("managed binding disagree"),
        "{blocked}"
    );
    assert!(runs_for(&recorder, nested_id).is_empty());
    core.workspace.set_chat_harness_session(
        nested_id,
        &nested_binding.provider_task_id,
        &nested_cwd,
    );
    let cleared = command(
        &client,
        nested_id,
        "nested-clear",
        json!({"kind":"clearContext"}),
    )
    .await
    .unwrap();
    assert_eq!(cleared["status"], "applied", "{cleared}");
    client
        .call(
            "SendSideChat",
            json!({"id":nested_id,"messageId":"after-clear","text":"fresh nested context"}),
        )
        .await
        .unwrap();
    until_diag_side(
        &core,
        &recorder,
        nested_id,
        "fresh direct send after clear",
        || runs_for(&recorder, nested_id).len() == 1 && !core.sessions.turn_in_flight(nested_id),
    )
    .await;
    let fresh_run = runs_for(&recorder, nested_id)[0].clone();
    assert!(fresh_run.resume.is_none());
    assert_eq!(
        contract(&fresh_run)
            .context
            .chat
            .unwrap()
            .capabilities
            .context_origin,
        RuntimeContextOrigin::Fresh
    );
    assert!(
        !instructions(&contract(&fresh_run)).contains("earlier context is already in this history")
    );
    let fresh_binding = core
        .task_identity
        .view(nested_id)
        .unwrap()
        .current_binding
        .unwrap();
    assert_eq!(fresh_binding.generation, nested_binding.generation + 1);
    assert_ne!(
        fresh_binding.provider_task_id,
        nested_binding.provider_task_id
    );
    evidence(
        &format!("side-ordinary-{}-summary.json", provider_name(id)),
        &json!({
            "parent": parent, "side": side, "forkSession": fork_session,
            "blankChild": blank_child, "nestedFork": nested_fork, "sourceDeleted":true,
            "sideRuns": runs_for(&recorder, &side).iter().map(|run| json!({
                "prompt": run.prompt, "model": run.model, "reasoning": run.reasoning,
                "attachments": run.attachments, "resume": run.resume,
                "effectiveMode": run.permission_policy.as_ref().map(|p| p.effective_mode),
            })).collect::<Vec<_>>(),
            "steers": recorder.steers.lock().unwrap().clone(),
            "parentRuns": runs_for(&recorder, &parent).len(),
        }),
    );
    drop(client);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_side_chat_ordinary_queue_steer_stop_attachments_and_model() {
    side_chat_ordinary_commands(HarnessId::Codex, "auto").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_side_chat_ordinary_queue_steer_stop_attachments_and_model() {
    side_chat_ordinary_commands(HarnessId::ClaudeCode, "full").await;
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
    let context: luna_proto::RuntimeContext =
        serde_json::from_value(legacy_context.clone()).unwrap();
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

// Placement/model follow-up: failure cases were recorded before implementation
// in docs/reference/chat-placement-and-child-models.md. Real engine/document/
// startup integration with two recording native-provider boundaries.
fn assemble_multi_provider(root: &Path) -> (EngineCore, Recorder) {
    worktrees_root();
    let recorder = Recorder::default();
    let registry = Arc::new(HarnessRegistry::new());
    for id in [HarnessId::Codex, HarnessId::ClaudeCode] {
        registry.register(Arc::new(Synthetic {
            id,
            recorder: recorder.clone(),
        }));
    }
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("engine"), "execution-org", "execution-user"),
        registry,
        HarnessId::Codex,
        None,
        account_config(root),
    )
    .unwrap();
    (core, recorder)
}

#[tokio::test(flavor = "multi_thread")]
async fn child_execution_selects_provider_model_options_and_keeps_fresh_history_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("execution-project");
    init_repo(&project);
    let (core, recorder) = assemble_multi_provider(root.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    core.workspace
        .create_space(
            "execution-space",
            &core.device_id,
            project.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let codex_parent = uuid::Uuid::new_v4().to_string();
    let claude_parent = uuid::Uuid::new_v4().to_string();
    for (id, provider) in [
        (&codex_parent, HarnessId::Codex),
        (&claude_parent, HarnessId::ClaudeCode),
    ] {
        create_chat(
            &client,
            id,
            "execution-space",
            &project,
            config(provider, "auto", "parent-model", "high"),
        )
        .await;
        core.doc_host
            .open(id)
            .unwrap()
            .write_user_message("parent-private-context", "PARENT_HISTORY_DO_NOT_COPY", 1)
            .unwrap();
    }
    let request = json!({"prompt":"CHILD_ONLY_EXECUTION_PROBE","idempotencyKey":"cross-claude","provider":"claude-code",
        "model":"cross-provider-model","reasoning":"low","modelOptions":{"fastMode":"off"},"reportResults":false});
    let receipt = create_child(&core, &codex_parent, request.clone())
        .await
        .unwrap();
    let id = receipt["taskId"].as_str().unwrap().to_owned();
    assert_eq!(receipt["execution"]["provider"], "claude-code");
    let row = core.workspace.chat(&id).unwrap().unwrap();
    let selected = row.config.as_ref().unwrap();
    assert_eq!(selected.harness, HarnessId::ClaudeCode);
    assert_eq!(selected.model.as_deref(), Some("cross-provider-model"));
    assert_eq!(selected.reasoning, Some(ReasoningLevel::Low));
    assert_eq!(selected.model_options.get("fastMode"), Some(&json!("off")));
    assert_eq!(
        row.child_thread.as_ref().unwrap().parent_task_id,
        codex_parent
    );
    assert!(
        row.harness_session_id
            .as_deref()
            .is_none_or(|session| !session.contains("codex"))
    );
    let retry = create_child(&core, &codex_parent, request.clone())
        .await
        .unwrap();
    assert_eq!(retry["taskId"], id);
    let mut conflict = request.clone();
    conflict["modelOptions"]["fastMode"] = json!("on");
    assert!(create_child(&core, &codex_parent, conflict).await.is_err());
    tokio::time::sleep(Duration::from_secs(1)).await;
    evidence(
        "child-execution-dispatch-state.json",
        &json!({"receipt":receipt,"row":core.workspace.chat(&id).unwrap(),"queue":core.doc_host.open(&id).unwrap().doc().read_queue().unwrap(),"entries":core.doc_host.open(&id).unwrap().doc().read_entries().unwrap(),"status":core.sessions.session_status(&id)}),
    );
    until("cross-provider child dispatch", || {
        recorder
            .runs
            .lock()
            .unwrap()
            .iter()
            .any(|run| run.prompt.contains("CHILD_ONLY_EXECUTION_PROBE"))
    })
    .await;
    let runs = recorder.runs.lock().unwrap();
    let run = runs
        .iter()
        .find(|run| run.prompt.contains("CHILD_ONLY_EXECUTION_PROBE"))
        .unwrap();
    assert_eq!(run.harness, Some(HarnessId::ClaudeCode));
    assert!(run.resume.is_none());
    assert!(
        !serde_json::to_string(run)
            .unwrap()
            .contains("PARENT_HISTORY_DO_NOT_COPY")
    );
    drop(runs);
    let inverse = create_child(&core, &claude_parent, json!({"prompt":"INVERSE_ONLY","idempotencyKey":"cross-codex",
        "provider":"codex","model":"child-model","reasoning":"high","modelOptions":{"fastMode":"on"},"reportResults":false})).await.unwrap();
    assert_eq!(
        core.workspace
            .chat(inverse["taskId"].as_str().unwrap())
            .unwrap()
            .unwrap()
            .config
            .unwrap()
            .harness,
        HarnessId::Codex
    );
    let defaults = create_child(&core, &codex_parent, json!({"prompt":"DEFAULTS_ONLY","idempotencyKey":"cross-defaults","provider":"claude-code","reportResults":false})).await.unwrap();
    let default_config = core
        .workspace
        .chat(defaults["taskId"].as_str().unwrap())
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    assert_eq!(default_config.harness, HarnessId::ClaudeCode);
    assert!(
        default_config.model.is_none()
            && default_config.reasoning.is_none()
            && default_config.model_options.is_empty()
    );
    evidence(
        "child-execution-before-restart.json",
        &json!({"receipt":receipt,"row":row,"inverse":inverse,"defaults":defaults}),
    );
    drop(client);
    core.shutdown().await;
    drop(core);
    let (reopened, _) = assemble_multi_provider(root.path());
    let restored = reopened.workspace.chat(&id).unwrap().unwrap();
    assert_eq!(
        luna_proto::ChildExecutionSelection::from(restored.config.as_ref().unwrap()),
        luna_proto::ChildExecutionSelection::from(row.config.as_ref().unwrap())
    );
    assert_eq!(
        restored.config.as_ref().unwrap().provider_account_id,
        row.config.as_ref().unwrap().provider_account_id
    );
    assert_eq!(
        restored.config.as_ref().unwrap().effective_permission_mode,
        PermissionMode::Ask
    );
    assert_eq!(
        restored.child_thread.as_ref().unwrap().parent_task_id,
        codex_parent
    );
    assert_eq!(
        create_child(&reopened, &codex_parent, request)
            .await
            .unwrap()["taskId"],
        id
    );
    evidence(
        "child-execution-after-restart.json",
        &serde_json::to_value(restored).unwrap(),
    );
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn child_execution_rejects_invalid_choices_before_identity_or_worktree_creation() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("invalid-execution-project");
    init_repo(&project);
    let (core, _) = assemble_multi_provider(root.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    core.workspace
        .create_space(
            "execution-space",
            &core.device_id,
            project.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let parent = uuid::Uuid::new_v4().to_string();
    create_chat(
        &client,
        &parent,
        "execution-space",
        &project,
        config(HarnessId::Codex, "auto", "parent-model", "high"),
    )
    .await;
    use luna_engine::cross_task_tools::{CrossTaskAuthority, CrossTaskBackend};
    let authority = CrossTaskAuthority {
        caller_task_id: parent.clone(),
        local_environment_id: core.device_id.clone(),
        visible_project_ids: ["execution-space".into()].into_iter().collect(),
        allow_projectless: false,
        granted_scopes: ["threads.read".into()].into_iter().collect(),
        explicit_user_request: false,
        delivery: RuntimeDelivery::CodexMcpTools,
    };
    let catalog = core
        .rpc_service()
        .child_model_catalog(
            serde_json::from_value(json!({"provider":"claude-code","limit":1})).unwrap(),
            &authority,
        )
        .await
        .unwrap();
    assert_eq!(catalog.models.len(), 1);
    assert_eq!(catalog.next_offset, Some(1));
    evidence(
        "child-model-catalog.json",
        &serde_json::to_value(catalog).unwrap(),
    );
    let cases = [
        json!({"provider":"cursor"}),
        json!({"provider":"mock"}),
        json!({"provider":"claude-code","model":"missing-model"}),
        json!({"provider":"claude-code","model":"child-model","reasoning":"ultra"}),
        json!({"provider":"claude-code","model":"child-model","modelOptions":{"fastMode":"invalid"}}),
        json!({"provider":"claude-code","model":"child-model","modelOptions":{"lunaRuntimeContract":"forged"}}),
        json!({"provider":"claude-code","modelOptions":{"fastMode":"on"}}),
        json!({"provider":"claude-code","model":"child-model","modelOptions":{"fastMode":true}}),
        json!({"provider":"claude-code","model":"child-model","permissionMode":"auto"}),
    ];
    let mut errors = Vec::new();
    for (index, fields) in cases.into_iter().enumerate() {
        let mut request = json!({"prompt":"never run","idempotencyKey":format!("bad-{index}"),"checkout":{"kind":"newWorktree","branch":format!("invalid-execution-{index}")}});
        request
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        errors.push(create_child(&core, &parent, request).await.unwrap_err());
    }
    assert!(errors[2].contains("missing-model"), "{:?}", errors);
    assert!(errors[3].contains("reasoning"), "{:?}", errors);
    assert!(errors[4].contains("fastMode"), "{:?}", errors);
    assert_eq!(core.workspace.read_chats().unwrap().len(), 1);
    assert!(!project.join(".git/worktrees").exists());
    let decoded: luna_proto::CreateChildThreadRequest =
        serde_json::from_value(json!({"prompt":"legacy","idempotencyKey":"legacy"})).unwrap();
    let encoded = serde_json::to_value(decoded).unwrap();
    assert!(encoded.get("provider").is_none() && encoded.get("modelOptions").is_none());
    evidence(
        "child-execution-invalid.json",
        &json!({"errors":errors,"chats":1,"worktreesCreated":false}),
    );
    core.shutdown().await;
}

// Central workbench failure cases were recorded before implementation in
// docs/design/chat-workbench/failure-cases.md. This uses real registry/docs,
// queue dispatch, child maintenance, side forks, persistence and restart.
#[tokio::test(flavor = "multi_thread")]
async fn blank_child_tabs_preserve_drafts_and_running_sessions_across_view_changes() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("blank-child-project");
    init_repo(&project);
    let (core, recorder) = assemble_multi_provider(root.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    core.workspace
        .create_space(
            "blank-space",
            &core.device_id,
            project.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let codex = uuid::Uuid::new_v4().to_string();
    let claude = uuid::Uuid::new_v4().to_string();
    for (parent, provider) in [(&codex, HarnessId::Codex), (&claude, HarnessId::ClaudeCode)] {
        create_chat(
            &client,
            parent,
            "blank-space",
            &project,
            config(provider, "ask", "parent-model", "high"),
        )
        .await;
        core.doc_host
            .open(parent)
            .unwrap()
            .write_user_message("private-parent", "PARENT_PRIVATE_CONTEXT", 1)
            .unwrap();
    }
    let requests = [
        json!({"parentTaskId":codex,"idempotencyKey":"user-claude-tab","title":"Blank Claude child","provider":"claude-code","model":"child-model","reasoning":"high","modelOptions":{"fastMode":"off"}}),
        json!({"parentTaskId":claude,"idempotencyKey":"user-codex-tab","title":"Blank Codex child","provider":"codex","model":"cross-provider-model","reasoning":"low","modelOptions":{"fastMode":"on"}}),
    ];
    let mut receipts = Vec::new();
    let mut children = Vec::new();
    for request in &requests {
        let before = core
            .workspace
            .chat(request["parentTaskId"].as_str().unwrap())
            .unwrap();
        let receipt = client
            .call("CreateChildTab", request.clone())
            .await
            .unwrap_or_else(|error| {
                evidence("blank-child-preflight-failure.json", &json!({"error":error.to_string(),"before":before,"after":core.workspace.chat(request["parentTaskId"].as_str().unwrap()).unwrap()}));
                panic!("blank child creation failed: {error}")
            });
        assert_eq!(receipt["startupState"], "awaiting-first-message");
        assert_eq!(receipt["provisioning"]["createdBy"], "user");
        let child = receipt["taskId"].as_str().unwrap().to_string();
        let row = core.workspace.chat(&child).unwrap().unwrap();
        let lineage = row.child_thread.as_ref().unwrap();
        assert!(lineage.initial_prompt.is_empty());
        assert!(!lineage.report_results);
        assert!(!lineage.continue_parent);
        assert_eq!(
            lineage.parent_task_id,
            request["parentTaskId"].as_str().unwrap()
        );
        assert!(row.source_context.is_none());
        assert!(row.harness_session_id.is_none());
        assert!(
            core.task_identity
                .view(&child)
                .unwrap()
                .current_binding
                .is_none()
        );
        assert_eq!(entry_count(&core, &child), 0);
        assert_eq!(queue_len(&core, &child), 0);
        receipts.push(receipt);
        children.push(child);
    }
    // At least one real periodic child sweep must run while both tabs are blank.
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert!(recorder.runs.lock().unwrap().is_empty());
    for child in &children {
        assert_eq!(entry_count(&core, child), 0);
        assert_eq!(queue_len(&core, child), 0);
    }
    assert_eq!(
        entry_count(&core, &codex),
        1,
        "blank creation must not inject a notice/prompt into Main"
    );
    let first = &children[0];
    let draft = json!({"version":1,"draftId":first,"taskId":first,"revision":1,"text":"UNSENT_DRAFT_SURVIVES_TAB_CLOSE","updatedAtMs":100});
    client
        .call("SaveComposerDraft", json!({"draft":draft}))
        .await
        .unwrap();
    async fn layout(client: &RpcClient, main: &str, command: Value) -> Value {
        let info = client.call("EngineInfo", json!({})).await.unwrap();
        let current = client
            .call("GetChatWorkbench", json!({"mainChatId":main}))
            .await
            .unwrap();
        client.call("MutateChatWorkbench",json!({"mainChatId":main,"profileId":info["profileId"],"expectedRevision":current["revision"],"commandId":uuid::Uuid::new_v4().to_string(),"command":command})).await.unwrap()
    }
    layout(
        &client,
        &codex,
        json!({"op":"openTab","tab":{"chatId":first}}),
    )
    .await;
    layout(&client, &codex, json!({"op":"closeTab","chatId":first})).await;
    let saved_draft = client
        .call("GetComposerDraft", json!({"taskId":first}))
        .await
        .unwrap();
    assert!(
        saved_draft
            .to_string()
            .contains("UNSENT_DRAFT_SURVIVES_TAB_CLOSE")
    );
    let replay = client
        .call("CreateChildTab", requests[0].clone())
        .await
        .unwrap();
    assert_eq!(replay["taskId"], *first);
    let mut conflict = requests[0].clone();
    conflict["model"] = json!("cross-provider-model");
    assert!(client.call("CreateChildTab", conflict).await.is_err());
    for (key, value) in [
        ("prompt", json!("must not execute")),
        ("permissionMode", json!("full")),
        ("checkout", json!({"kind":"newWorktree"})),
        ("reportResults", json!(true)),
    ] {
        let mut forged = requests[0].clone();
        forged[key] = value;
        assert!(client.call("CreateChildTab", forged).await.is_err());
    }
    assert_eq!(core.workspace.read_chats().unwrap().len(), 4);
    let blank_rows = children
        .iter()
        .map(|id| core.workspace.chat(id).unwrap().unwrap())
        .collect::<Vec<_>>();
    drop(client);
    core.shutdown().await;
    drop(core);
    let (core, recorder) = assemble_multi_provider(root.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(recorder.runs.lock().unwrap().is_empty());
    assert_eq!(
        client
            .call("GetComposerDraft", json!({"taskId":first}))
            .await
            .unwrap(),
        saved_draft
    );
    for child in &children {
        assert_eq!(entry_count(&core, child), 0);
        assert_eq!(queue_len(&core, child), 0);
    }
    layout(&client, &codex, json!({"op":"reopenClosed"})).await;
    queue(
        &client,
        first,
        "first-real-user-message",
        "HOLD FIRST_UI_CHILD_PROMPT",
    )
    .await;
    until("manual child running", || {
        runs_for(&recorder, first).len() == 1
            && core.sessions.turn_in_flight(first)
            && core
                .task_identity
                .view(first)
                .is_some_and(|view| view.current_binding.is_some())
    })
    .await;
    let native = core
        .task_identity
        .view(first)
        .unwrap()
        .current_binding
        .unwrap();
    assert_eq!(
        contract(&runs_for(&recorder, first)[0])
            .context
            .parent_thread_id
            .as_deref(),
        Some(codex.as_str())
    );
    assert_eq!(
        runs_for(&recorder, first)[0].prompt,
        "HOLD FIRST_UI_CHILD_PROMPT"
    );
    layout(&client,&codex,json!({"op":"moveTab","chatId":first,"destination":{"kind":"split","nodeId":"main-pane","edge":"right","newPaneId":"running-child","splitId":"running-split"}})).await;
    layout(
        &client,
        &codex,
        json!({"op":"closePane","paneId":"running-child"}),
    )
    .await;
    assert!(
        core.sessions.turn_in_flight(first),
        "closing the view must not stop the turn"
    );
    layout(
        &client,
        &codex,
        json!({"op":"openTab","tab":{"chatId":first}}),
    )
    .await;
    assert_eq!(runs_for(&recorder, first).len(), 1);
    assert!(runs_for(&recorder, &children[1]).is_empty());
    assert_eq!(
        core.task_identity
            .view(first)
            .unwrap()
            .current_binding
            .unwrap(),
        native
    );
    let entries = core
        .doc_host
        .open(first)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    let users = entries
        .iter()
        .filter(|entry| entry.role == luna_doc::MessageRole::User)
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 1);
    assert!(
        !serde_json::to_string(&entries)
            .unwrap()
            .contains("PARENT_PRIVATE_CONTEXT")
    );
    command(
        &client,
        first,
        "explicit-stop-child",
        json!({"kind":"session","command":{"kind":"interrupt"}}),
    )
    .await
    .unwrap();
    until("explicit child stop", || {
        !core.sessions.turn_in_flight(first)
    })
    .await;
    assert_eq!(
        entry_count(&core, &codex),
        1,
        "manual child must not report/wake Main"
    );
    // A real native-fork side chat remains active while its view closes/reopens.
    queue(&client, &codex, "parent-for-side", "PARENT_SIDE_CONTEXT").await;
    until("parent ready to fork", || {
        runs_for(&recorder, &codex).len() == 1
            && !core.sessions.turn_in_flight(&codex)
            && core
                .task_identity
                .view(&codex)
                .and_then(|v| v.current_binding)
                .is_some()
    })
    .await;
    let side = client
        .call(
            "CreateSideChat",
            json!({"parentId":codex,"requestId":uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    let side_id = side["meta"]["id"].as_str().unwrap();
    let side_binding = core.task_identity.view(side_id).unwrap().current_binding;
    layout(
        &client,
        &codex,
        json!({"op":"openTab","tab":{"chatId":side_id,"sideParentId":codex}}),
    )
    .await;
    layout(&client, &codex, json!({"op":"closeTab","chatId":side_id})).await;
    let side_history = client
        .call("ListSideChats", json!({"parentId":codex}))
        .await
        .unwrap();
    assert!(
        side_history["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == side_id && row["state"] == "active")
    );
    layout(&client, &codex, json!({"op":"reopenClosed"})).await;
    assert_eq!(
        core.task_identity.view(side_id).unwrap().current_binding,
        side_binding
    );
    assert!(runs_for(&recorder, side_id).is_empty());
    assert!(
        client
            .call("GetChatWorkbench", json!({"mainChatId":side_id}))
            .await
            .is_ok()
    );
    let switched = command(&client, side_id, "fork-switch-provider", json!({
        "kind":"switchProvider", "config":config(HarnessId::ClaudeCode, "ask", "child-model", "medium"),
    })).await.unwrap();
    assert_eq!(switched["status"], "applied", "{switched}");
    client.call("SendSideChat", json!({"id":side_id,"messageId":"first-direct-after-switch","text":"new provider after explicit switch"})).await.unwrap();
    until_diag_side(&core, &recorder, side_id, "fork switched provider", || {
        runs_for(&recorder, side_id).len() == 1 && !core.sessions.turn_in_flight(side_id)
    })
    .await;
    let switched_run = runs_for(&recorder, side_id)[0].clone();
    assert_eq!(switched_run.harness, Some(HarnessId::ClaudeCode));
    assert!(switched_run.resume.is_none());
    assert_eq!(
        contract(&switched_run)
            .context
            .chat
            .unwrap()
            .capabilities
            .context_origin,
        RuntimeContextOrigin::Fresh
    );
    let switched_binding = core
        .task_identity
        .view(side_id)
        .unwrap()
        .current_binding
        .unwrap();
    assert_eq!(switched_binding.provider_id, HarnessId::ClaudeCode);
    assert_eq!(
        switched_binding.generation,
        side_binding.as_ref().unwrap().generation + 1
    );
    assert_ne!(
        switched_binding.provider_task_id,
        side_binding.as_ref().unwrap().provider_task_id
    );
    evidence(
        "fork-explicit-provider-switch.json",
        &json!({"originalBinding":side_binding,"newBinding":switched_binding,"request":switched_run,"receipt":switched}),
    );
    let final_layout = client
        .call("GetChatWorkbench", json!({"mainChatId":codex}))
        .await
        .unwrap();
    evidence(
        "blank-child-workbench-lifecycle.json",
        &json!({"receipts":receipts,"blankRows":blank_rows,"draft":saved_draft,"firstUserMessages":users,"nativeBinding":native,"sideHistory":side_history,"layout":final_layout,"noBlankStartupAfterRestart":true,"viewCloseDidNotStop":true}),
    );
    drop(client);
    core.shutdown().await;
}
