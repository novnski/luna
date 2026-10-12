//! Pane chat (engine side): `create_thread` from a parent agent over the
//! per-process LUNA MCP endpoint, through the real Codex and Claude adapters
//! and their deterministic fake peers (`harness/tests/fixtures/fake-codex.sh`,
//! `fake-claude-luna.py`). Covers the new tool schema on both providers, the
//! approval card's explicit access grant, the default cap of a model-requested
//! escalation, denial, a forged choice index, worktree provisioning after
//! approval and the child's explicit role in the instructions each provider
//! receives. Failure cases: `evidence/pane-engine/failure-cases.md` C3–C8, F2, F3.

#![cfg(unix)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use luna_engine::sessions::LunaApprovalOutcome;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{ClaudeHarness, CodexHarness};
use luna_proto::{
    AgentEvent, ApprovalDecision, ChatConfig, ChildCheckoutKind, HarnessId, PermissionMode,
    PermissionScope, RunRequest, SandboxLevel, TaskLineage, UserInputQuestion,
};
use serde_json::{Value, json};

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
        json!({"oauthAccount": {"accountUuid": "claude-fixture", "emailAddress": "claude@example.test"}})
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

fn fixture(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures")
        .join(name);
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
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

fn init_repo(repo: &Path) {
    std::fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "pane@example.test"]);
    git(repo, &["config", "user.name", "Pane Fixture"]);
    std::fs::write(repo.join("README.md"), "main\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "init"]);
}

fn parent_config(harness: HarnessId, mode: PermissionMode) -> ChatConfig {
    ChatConfig {
        harness,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: mode,
        effective_permission_mode: mode,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn run_request(prompt: &str, cwd: &Path, mode: PermissionMode) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.display().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: mode,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: vec![],
        worktree: None,
        resume: None,
    }
}

fn new_task(core: &EngineCore, project: &str, cwd: &Path, config: &ChatConfig) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    core.workspace
        .create_chat(&id, Some(project), None, Some(config.clone()), None)
        .unwrap();
    core.task_identity
        .register_new_task(
            &id,
            Some(project),
            None,
            cwd.to_str(),
            TaskLineage::default(),
        )
        .unwrap();
    id
}

async fn next_card(
    events: &mut tokio::sync::broadcast::Receiver<luna_engine::JournaledEvent>,
) -> (String, UserInputQuestion) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let AgentEvent::InputRequested {
                request_id,
                mut questions,
            } = events.recv().await.unwrap().event
            {
                assert_eq!(questions.len(), 1);
                return (request_id, questions.remove(0));
            }
        }
    })
    .await
    .expect("LUNA approval requested")
}

/// The card advertises exactly these choices, in this order, as both the
/// option labels and the server-owned details the UI maps to an index.
fn assert_card(question: &UserInputQuestion, labels: &[&str]) {
    assert_eq!(question.options, labels, "{}", question.question);
    let approval = question.approval.as_ref().expect("approval card");
    assert!(approval.is_luna_cross_task());
    let details = approval.details.as_ref().expect("card details");
    assert_eq!(details.kind.as_deref(), Some("createChildThread"));
    assert_eq!(
        details
            .choices
            .iter()
            .map(|choice| choice.label.as_str())
            .collect::<Vec<_>>(),
        labels
    );
}

const GRANT_FULL: &str = "Allow once and grant Full access";

fn child_with_prompt(core: &EngineCore, prompt: &str) -> Option<luna_proto::Chat> {
    core.workspace
        .read_chats()
        .unwrap()
        .into_iter()
        .find(|chat| {
            chat.child_thread
                .as_ref()
                .is_some_and(|child| child.initial_prompt == prompt)
        })
}

/// `providers` is what the test registry installs: the schema offers only
/// providers that can actually run a child on this device.
fn schema_has_new_fields(schema: &Value, providers: Value) {
    assert_eq!(schema["properties"]["provider"]["enum"], providers);
    assert_eq!(
        schema["properties"]["modelOptions"]["additionalProperties"]["type"],
        "string"
    );
    let properties = &schema["properties"];
    assert!(properties["checkout"]["oneOf"].is_array(), "{schema:#}");
    assert_eq!(
        properties["permissionMode"]["enum"],
        json!(["ask", "auto", "full"]),
        "{schema:#}"
    );
    assert_eq!(schema["additionalProperties"], false);
}

fn evidence(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("PANE_ENGINE_EVIDENCE_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(Path::new(&dir).join(name), format!("{value:#}"));
    }
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    while !ready() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// Codex: the test is the MCP client, the way Codex is.
// ---------------------------------------------------------------------------

struct Held {
    url: String,
    token: String,
    thread: String,
    turn: String,
}

async fn held(dir: &Path, task: &str) -> Held {
    let file = dir.join(task);
    for _ in 0..800 {
        if let Ok(text) = std::fs::read_to_string(&file) {
            let lines: Vec<_> = text.lines().map(str::to_owned).collect();
            if lines.len() == 4 {
                return Held {
                    url: lines[0].clone(),
                    token: lines[1].clone(),
                    thread: lines[2].clone(),
                    turn: lines[3].clone(),
                };
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("fake Codex never published its endpoint for {task}");
}

async fn post(http: &reqwest::Client, held: &Held, body: &Value) -> Value {
    let response = http
        .post(&held.url)
        .bearer_auth(&held.token)
        .header("accept", "application/json, text/event-stream")
        .json(body)
        .send()
        .await
        .unwrap();
    serde_json::from_str(&response.text().await.unwrap()).unwrap_or(Value::Null)
}

fn tool_call(id: u64, args: Value, held: &Held, call_id: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":"create_thread","arguments":args,
        "_meta":{"threadId":held.thread,"callId":call_id,"sessionId":held.thread,
            "x-codex-turn-metadata":{"turn_id":held.turn,"thread_id":held.thread}}}})
}

fn tool_text(reply: &Value) -> (bool, String) {
    (
        reply["result"]["isError"].as_bool().expect("tool result"),
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_create_thread_schema_grant_cap_deny_and_child_identity() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    worktrees_root();
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(
        CodexHarness::new().with_executable(fixture("fake-codex.sh")),
    ));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&base.join("engine"), "pane-org", "pane-user"),
        registry,
        HarnessId::Codex,
        None,
        account_config(&base),
    )
    .unwrap();
    let _rpc = core.rpc_service();
    // As `luna headless --remote-port` does: the remote listener's service is
    // built after the local one. The agent's tools must not depend on that.
    let _remote = core.remote_rpc_factory().expect("remote authority");
    let repo_name = "repo-pane-codex-mcp";
    let repo = base.join(repo_name);
    init_repo(&repo);
    // Ignore the fake's per-run files so the parent checkout stays clean.
    std::fs::write(repo.join(".git/info/exclude"), ".luna-*\n").unwrap();
    std::fs::write(repo.join(".luna-fake-thread-per-task"), b"").unwrap();
    core.workspace
        .create_space(
            "project-a",
            &core.device_id,
            repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let config = parent_config(HarnessId::Codex, PermissionMode::Auto);
    let source = new_task(&core, "project-a", &repo, &config);
    let (_, mut events) = core.sessions.subscribe(&source, 0).unwrap();
    core.sessions
        .dispatch(
            &source,
            HarnessId::Codex,
            run_request("scenario:mcp-hold", &repo, PermissionMode::Auto),
            Some("pane-hold".into()),
        )
        .await
        .unwrap();
    let hold_dir = repo.join(".luna-mcp-hold");
    let a = held(&hold_dir, &source).await;
    let http = reqwest::Client::new();

    // F3: the Codex LUNA MCP endpoint lists the new create_thread schema.
    let listed = post(
        &http,
        &a,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await;
    let create = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "create_thread")
        .cloned()
        .expect("create_thread listed");
    schema_has_new_fields(&create["inputSchema"], json!(["codex"]));
    evidence("codex-create-thread-tool.json", &create);
    for name in ["get_workspace", "manage_workspace"] {
        assert!(
            listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == name),
            "{name} is not offered: {listed}"
        );
    }
    // The parent session's instructions name its role.
    let thread_request =
        std::fs::read_to_string(hold_dir.join(format!("{source}.thread"))).unwrap();
    assert!(
        thread_request.contains("Chat role: main"),
        "{thread_request}"
    );

    // C5: model-requested Full on an Auto parent, user picks the grant choice.
    let call = |id, args: Value, call_id: &str| tool_call(id, args, &a, call_id);
    let pending = {
        let (http, body, url, token) = (
            http.clone(),
            call(
                10,
                json!({"prompt":"scenario:mcp-hold","idempotencyKey":"codex-grant","title":"Granted child",
                    "permissionMode":"full","checkout":{"kind":"newWorktree","branch":"codex/granted"}}),
                "c-grant",
            ),
            a.url.clone(),
            a.token.clone(),
        );
        tokio::spawn(async move {
            post(
                &http,
                &Held {
                    url,
                    token,
                    thread: String::new(),
                    turn: String::new(),
                },
                &body,
            )
            .await
        })
    };
    let (request_id, question) = next_card(&mut events).await;
    assert_card(&question, &["Deny", "Allow once", GRANT_FULL]);
    assert!(
        question.question.contains("Full access requested"),
        "{}",
        question.question
    );
    assert!(
        question
            .question
            .contains("new worktree on branch codex/granted")
    );
    assert_eq!(
        question.approval.as_ref().unwrap().requested_scope,
        PermissionScope::Full
    );
    evidence(
        "codex-escalation-card.json",
        &serde_json::to_value(&question).unwrap(),
    );
    // Nothing is provisioned before the answer.
    assert!(!git(&repo, &["branch", "--list"]).contains("codex/granted"));
    assert_eq!(
        core.sessions.respond_luna_local_ui_approval(
            &source,
            &request_id,
            ApprovalDecision::ProviderChoice { index: 2 }
        ),
        LunaApprovalOutcome::AllowedOnce
    );
    let (failed, text) = tool_text(&pending.await.unwrap());
    assert!(!failed, "{text}");
    let receipt: Value = serde_json::from_str(&text).unwrap();
    let granted = receipt["taskId"].as_str().unwrap().to_owned();
    assert_eq!(
        receipt["provisioning"]["grantedByUser"], true,
        "{receipt:#}"
    );
    assert_eq!(receipt["provisioning"]["createdBy"], "agent");
    let row = core.workspace.chat(&granted).unwrap().unwrap();
    let provisioning = row.child_thread.clone().unwrap().provisioning.unwrap();
    assert_eq!(provisioning.granted_permission_mode, PermissionMode::Full);
    assert_eq!(provisioning.parent_permission_ceiling, PermissionMode::Auto);
    assert_eq!(provisioning.checkout_kind, ChildCheckoutKind::NewWorktree);
    assert_eq!(row.branch.as_deref(), Some("codex/granted"));
    let worktree = PathBuf::from(row.cwd.clone().unwrap());
    assert!(worktree.starts_with(worktrees_root().join(repo_name)));
    assert_eq!(
        git(&worktree, &["branch", "--show-current"]),
        "codex/granted"
    );
    evidence(
        "codex-granted-child-row.json",
        &serde_json::to_value(&row).unwrap(),
    );

    // F2 on Codex: the child's own thread request carries its identity.
    let child_hold = worktree.join(".luna-mcp-hold");
    let _child = held(&child_hold, &granted).await;
    let child_request =
        std::fs::read_to_string(child_hold.join(format!("{granted}.thread"))).unwrap();
    assert!(
        child_request.contains("Chat role: child chat"),
        "{child_request}"
    );
    assert!(
        child_request.contains("granted by the user"),
        "{child_request}"
    );
    assert!(
        child_request.contains("its own git worktree"),
        "{child_request}"
    );
    assert!(child_request.contains(&source), "{child_request}");
    std::fs::write(child_hold.join(format!("release-{granted}")), b"").unwrap();

    // C3: the same escalation answered "Allow once" is capped at Auto.
    let pending = {
        let (http, body, url, token) = (
            http.clone(),
            call(
                11,
                json!({"prompt":"Capped brief","idempotencyKey":"codex-capped","permissionMode":"full"}),
                "c-capped",
            ),
            a.url.clone(),
            a.token.clone(),
        );
        tokio::spawn(async move {
            post(
                &http,
                &Held {
                    url,
                    token,
                    thread: String::new(),
                    turn: String::new(),
                },
                &body,
            )
            .await
        })
    };
    let (request_id, question) = next_card(&mut events).await;
    assert_card(&question, &["Deny", "Allow once", GRANT_FULL]);
    assert_eq!(
        core.sessions.respond_luna_local_ui_approval(
            &source,
            &request_id,
            ApprovalDecision::AllowOnce
        ),
        LunaApprovalOutcome::AllowedOnce
    );
    let (failed, text) = tool_text(&pending.await.unwrap());
    assert!(!failed, "{text}");
    let capped = child_with_prompt(&core, "Capped brief").unwrap();
    let provisioning = capped.child_thread.clone().unwrap().provisioning.unwrap();
    assert_eq!(provisioning.granted_permission_mode, PermissionMode::Auto);
    assert!(provisioning.escalation_denied && !provisioning.granted_by_user);
    assert_eq!(provisioning.checkout_kind, ChildCheckoutKind::Current);
    assert_eq!(
        capped.config.unwrap().effective_permission_mode,
        PermissionMode::Auto
    );

    // C4: Deny creates nothing, not even the requested worktree branch.
    let before_dirs = std::fs::read_dir(worktrees_root().join(repo_name))
        .unwrap()
        .count();
    for (id, index, key, decision) in [
        (
            12,
            0u32,
            "codex-denied",
            ApprovalDecision::ProviderChoice { index: 0 },
        ),
        // C8: a forged index outside the advertised choices is a denial.
        (
            13,
            9,
            "codex-forged",
            ApprovalDecision::ProviderChoice { index: 9 },
        ),
        (14, 0, "codex-deny-legacy", ApprovalDecision::Deny),
    ] {
        let _ = index;
        let pending = {
            let (http, body, url, token) = (
                http.clone(),
                call(
                    id,
                    json!({"prompt":format!("Denied {key}"),"idempotencyKey":key,"permissionMode":"full",
                        "checkout":{"kind":"newWorktree","branch":format!("codex/{key}")}}),
                    key,
                ),
                a.url.clone(),
                a.token.clone(),
            );
            tokio::spawn(async move {
                post(
                    &http,
                    &Held {
                        url,
                        token,
                        thread: String::new(),
                        turn: String::new(),
                    },
                    &body,
                )
                .await
            })
        };
        let (request_id, _) = next_card(&mut events).await;
        assert_eq!(
            core.sessions
                .respond_luna_local_ui_approval(&source, &request_id, decision),
            LunaApprovalOutcome::Denied
        );
        let (failed, text) = tool_text(&pending.await.unwrap());
        assert!(
            text.contains("cancelled") && !text.contains("explicit user request"),
            "denied/retired approval must report cancellation: {text}"
        );
        assert!(failed, "{key} must fail");
        assert!(child_with_prompt(&core, &format!("Denied {key}")).is_none());
        assert!(!git(&repo, &["branch", "--list"]).contains(&format!("codex/{key}")));
    }
    assert_eq!(
        std::fs::read_dir(worktrees_root().join(repo_name))
            .unwrap()
            .count(),
        before_dirs
    );

    // C8: a non-escalating request is ordinary app coordination. Since
    // a08e9344 it completes without any card and keeps the requested access.
    let pending = {
        let (http, body, url, token) = (
            http.clone(),
            call(
                15,
                json!({"prompt":"Plain brief","idempotencyKey":"codex-plain","permissionMode":"ask"}),
                "c-plain",
            ),
            a.url.clone(),
            a.token.clone(),
        );
        tokio::spawn(async move {
            post(
                &http,
                &Held {
                    url,
                    token,
                    thread: String::new(),
                    turn: String::new(),
                },
                &body,
            )
            .await
        })
    };
    let reply = tokio::time::timeout(Duration::from_secs(30), pending)
        .await
        .expect("plain create_thread completes without a card")
        .unwrap();
    let (failed, text) = tool_text(&reply);
    assert!(!failed, "{text}");
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event.event, AgentEvent::InputRequested { .. }),
            "a non-escalating child must not ask for approval"
        );
    }
    let plain = child_with_prompt(&core, "Plain brief").unwrap();
    let provisioning = plain.child_thread.unwrap().provisioning.unwrap();
    assert_eq!(provisioning.granted_permission_mode, PermissionMode::Ask);
    assert!(!provisioning.granted_by_user && !provisioning.escalation_denied);

    std::fs::write(hold_dir.join(format!("release-{source}")), b"").unwrap();
    let names: HashSet<String> = core
        .workspace
        .read_chats()
        .unwrap()
        .into_iter()
        .filter_map(|chat| chat.child_thread.map(|child| child.initial_prompt))
        .collect();
    evidence("codex-children.json", &json!({"children": names}));
    core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Claude: the fake CLI is the MCP client and runs a scripted plan.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn claude_create_thread_schema_grant_cap_deny_and_child_identity() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    worktrees_root();
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(
        ClaudeHarness::new().with_executable(fixture("fake-claude-luna.py")),
    ));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&base.join("engine"), "pane-org", "pane-user"),
        registry,
        HarnessId::ClaudeCode,
        None,
        account_config(&base),
    )
    .unwrap();
    let _rpc = core.rpc_service();
    let repo_name = "repo-pane-claude-mcp";
    let repo = base.join(repo_name);
    init_repo(&repo);
    std::fs::write(repo.join(".git/info/exclude"), ".luna-*\n").unwrap();
    core.workspace
        .create_space(
            "project-a",
            &core.device_id,
            repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let config = parent_config(HarnessId::ClaudeCode, PermissionMode::Ask);
    let source = new_task(&core, "project-a", &repo, &config);
    let plan = json!([
        {"tool": "create_thread", "arguments": {
            "prompt": "Claude granted child", "idempotencyKey": "claude-grant",
            "permissionMode": "full", "checkout": {"kind": "newWorktree"}}},
        {"tool": "create_thread", "arguments": {
            "prompt": "Claude capped child", "idempotencyKey": "claude-capped",
            "permissionMode": "full"}},
        {"tool": "create_thread", "arguments": {
            "prompt": "Claude denied child", "idempotencyKey": "claude-denied",
            "permissionMode": "full", "checkout": {"kind": "newWorktree", "branch": "claude/denied"}}},
        {"tool": "create_thread", "arguments": {
            "prompt": "Claude smuggled child", "idempotencyKey": "claude-smuggle",
            "grantedByUser": true}},
    ]);
    std::fs::write(
        repo.join(".luna-claude-plan-pane.json"),
        serde_json::to_vec(&plan).unwrap(),
    )
    .unwrap();
    let (_, mut events) = core.sessions.subscribe(&source, 0).unwrap();
    core.sessions
        .dispatch(
            &source,
            HarnessId::ClaudeCode,
            run_request("scenario:luna-plan:pane", &repo, PermissionMode::Ask),
            Some("pane-plan".into()),
        )
        .await
        .unwrap();
    let mut cards = Vec::new();
    for decision in [
        ApprovalDecision::ProviderChoice { index: 2 },
        ApprovalDecision::AllowOnce,
        ApprovalDecision::Deny,
    ] {
        let (request_id, question) = next_card(&mut events).await;
        assert_card(&question, &["Deny", "Allow once", GRANT_FULL]);
        assert_eq!(
            question.approval.as_ref().unwrap().provider,
            HarnessId::ClaudeCode
        );
        cards.push(serde_json::to_value(&question).unwrap());
        let expected = if decision == ApprovalDecision::Deny {
            LunaApprovalOutcome::Denied
        } else {
            LunaApprovalOutcome::AllowedOnce
        };
        assert_eq!(
            core.sessions
                .respond_luna_local_ui_approval(&source, &request_id, decision),
            expected
        );
    }
    let file = repo.join(".luna-claude-results-pane.json");
    until("plan results", || file.exists()).await;
    let results: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    evidence("claude-plan-results.json", &results);
    evidence("claude-escalation-cards.json", &json!(cards));

    // F3: Claude's LUNA MCP entry lists the same schema; the per-server
    // timeout (QA fix F03) is still attached.
    schema_has_new_fields(
        &results["handshake"]["schemas"]["create_thread"],
        json!(["claude-code"]),
    );
    assert!(
        results["systemPrompt"]
            .as_str()
            .unwrap()
            .contains("Chat role: main"),
        "{results:#}"
    );

    // Granted: Full, own worktree; capped: Ask; denied and smuggled: nothing.
    assert_eq!(results["0"]["isError"], false, "{results:#}");
    let granted = child_with_prompt(&core, "Claude granted child").unwrap();
    let provisioning = granted.child_thread.clone().unwrap().provisioning.unwrap();
    assert!(provisioning.granted_by_user);
    assert_eq!(provisioning.granted_permission_mode, PermissionMode::Full);
    assert_eq!(provisioning.checkout_kind, ChildCheckoutKind::NewWorktree);
    let worktree = PathBuf::from(granted.cwd.clone().unwrap());
    assert!(worktree.starts_with(worktrees_root().join(repo_name)));
    assert_eq!(
        git(&worktree, &["branch", "--show-current"]),
        format!("luna/child-{}", &granted.id[..8])
    );
    assert_eq!(results["1"]["isError"], false, "{results:#}");
    let capped = child_with_prompt(&core, "Claude capped child").unwrap();
    let capped_provisioning = capped.child_thread.clone().unwrap().provisioning.unwrap();
    assert!(capped_provisioning.escalation_denied);
    assert_eq!(
        capped_provisioning.granted_permission_mode,
        PermissionMode::Ask
    );
    assert_eq!(results["2"]["isError"], true);
    assert!(child_with_prompt(&core, "Claude denied child").is_none());
    assert!(!git(&repo, &["branch", "--list"]).contains("claude/denied"));
    assert_eq!(
        results["3"]["isError"], true,
        "smuggled grant must be rejected"
    );
    assert!(child_with_prompt(&core, "Claude smuggled child").is_none());

    // F2 on Claude: each child is launched with its own role instructions.
    let granted_prompt = worktree.join(format!(".luna-claude-turn-{}.txt", granted.id));
    until("granted child turn", || granted_prompt.exists()).await;
    let text = std::fs::read_to_string(&granted_prompt).unwrap();
    assert!(text.contains("Chat role: child chat"), "{text}");
    assert!(text.contains("granted by the user"), "{text}");
    assert!(text.contains("its own git worktree"), "{text}");
    let capped_prompt = repo.join(format!(".luna-claude-turn-{}.txt", capped.id));
    until("capped child turn", || capped_prompt.exists()).await;
    let text = std::fs::read_to_string(&capped_prompt).unwrap();
    assert!(text.contains("Chat role: child chat"), "{text}");
    assert!(text.contains("capped by the parent's access"), "{text}");
    assert!(text.contains("shares the parent's checkout"), "{text}");
    evidence(
        "claude-child-instructions.json",
        &json!({"granted": std::fs::read_to_string(&granted_prompt).unwrap(),
            "capped": std::fs::read_to_string(&capped_prompt).unwrap()}),
    );
    core.shutdown().await;
}
