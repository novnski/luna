//! CC-00 conformance + G1/E23 effective configuration, against the deterministic
//! Claude Code 2.1.286 peer `tests/fixtures/fake-claude-config.py`.
//!
//! The peer is first checked against every case of the recorded real-CLI matrix
//! (`fixtures/claude/compat-2.1.286.json`), so the E23 assertions below run on
//! recorded native semantics, not invented ones. Failure cases:
//! `$B/evidence/cc00-baseline/failure-cases.md` (C-*, G-*).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use luna_harness::claude::profile::{ASK_AUTO_TOOL_GRANT, RECORDED_CLI_VERSION};
use luna_harness::{
    CancellationToken, ClaudeHarness, CommandScope, Harness, RunControls, SteerMessage,
};
use luna_proto::{
    AgentEvent, DoneStatus, PermissionMode, PermissionPolicy, RunRequest, SandboxLevel,
    UserInputAnswer,
};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn fake() -> PathBuf {
    let path = fixtures().join("fake-claude-config.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn record() -> Value {
    let text = std::fs::read_to_string(fixtures().join("claude/compat-2.1.286.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The disposable layout the real probes used (see the record's `fixture_layout`).
fn layout(root: &Path, followup: bool) {
    let project = root.join("project");
    std::fs::create_dir_all(project.join(".claude/commands")).unwrap();
    std::fs::create_dir_all(root.join("config/commands")).unwrap();
    std::fs::create_dir_all(root.join("elsewhere")).unwrap();
    std::fs::write(
        project.join(".claude/settings.json"),
        r#"{"outputStyle":"Explanatory"}"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".claude/commands/luna-project-canary.md"),
        "---\ndescription: project canary\n---\nNever invoked.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("config/commands/luna-user-canary.md"),
        "---\ndescription: user canary\n---\nNever invoked.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("explicit-settings.json"),
        r#"{"outputStyle":"Learning"}"#,
    )
    .unwrap();
    if followup {
        std::fs::write(
            project.join(".claude/settings.local.json"),
            r#"{"outputStyle":"Concise"}"#,
        )
        .unwrap();
        let hooks = root.join("hook-project/.claude");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join("settings.json"),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"true"}]}]}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("config-2")).unwrap();
    }
}

fn log_entries(home: &Path) -> Vec<Value> {
    std::fs::read_to_string(home.join("fake-claude-log.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn kind_count(home: &Path, kind: &str) -> usize {
    log_entries(home)
        .iter()
        .filter(|entry| entry["kind"] == kind)
        .count()
}

/// Policy-affecting flags (value flags carry their value).
fn policy_flags(argv: &Value) -> Vec<String> {
    const SWITCHES: &[&str] = &["--restricted", "--dangerously-skip-permissions"];
    const VALUED: &[&str] = &[
        "--permission-mode",
        "--tools",
        "--permission-prompt-tool",
        "--setting-sources",
    ];
    let argv: Vec<&str> = argv
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        if SWITCHES.contains(&argv[i]) {
            out.push(argv[i].to_owned());
        } else if VALUED.contains(&argv[i]) && i + 1 < argv.len() {
            out.push(format!("{}={}", argv[i], argv[i + 1]));
            i += 1;
        }
        i += 1;
    }
    out
}

fn count_flag(argv: &Value, flag: &str) -> usize {
    argv.as_array()
        .unwrap()
        .iter()
        .filter(|v| v.as_str() == Some(flag))
        .count()
}

fn request(cwd: &Path, mode: PermissionMode) -> RunRequest {
    RunRequest {
        goal: None,
        review: None,
        input_items: Vec::new(),
        prompt: "effective configuration".into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: cwd.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: mode,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: Some(PermissionPolicy {
            requested_mode: mode,
            effective_mode: mode,
            ..Default::default()
        }),
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>) {
    let (steer_tx, steer_rx) = mpsc::channel(1);
    let controls = RunControls {
        request_input: Box::new(|questions| {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(
                questions
                    .iter()
                    .map(|q| UserInputAnswer {
                        question_id: q.id.clone(),
                        labels: vec!["Deny".into()],
                    })
                    .collect(),
            );
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    };
    (controls, steer_tx)
}

async fn run_once(harness: &ClaudeHarness, req: RunRequest, title: bool) -> Vec<AgentEvent> {
    let (controls, _steer) = controls();
    let stream = if title {
        harness
            .run_title(req, controls)
            .await
            .expect("title starts")
    } else {
        harness.run(req, controls).await.expect("run starts")
    };
    tokio::time::timeout(
        Duration::from_secs(15),
        stream.map(|r| r.expect("event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished")
}

fn scope(cwd: &Path, mode: PermissionMode, account: Option<&str>) -> CommandScope {
    CommandScope {
        cwd: Some(cwd.to_path_buf()),
        permission_mode: mode,
        auth_instance: account.map(str::to_owned),
    }
}

fn names(commands: &[luna_proto::SlashCommand]) -> Vec<String> {
    commands.iter().map(|c| c.name.clone()).collect()
}

/// C-1/C-2/P-5: the peer reproduces every recorded 2.1.286 observation, its
/// initialize response has the recorded keys, and the grant pins this version.
#[test]
fn fake_peer_reproduces_recorded_2_1_286_matrix() {
    let record = record();
    assert!(
        record["cli"]["version"]
            .as_str()
            .unwrap()
            .starts_with(RECORDED_CLI_VERSION)
    );
    let grant: Vec<&str> = record["policy"]["ask_auto_grant"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(grant, ASK_AUTO_TOOL_GRANT, "grant drifted from the record");
    let cases = record["probe_cases"].as_array().unwrap();
    assert!(cases.len() >= 20, "matrix too small: {}", cases.len());
    for case in cases {
        let root = tempfile::tempdir().unwrap();
        layout(root.path(), case["fixture"] == "followup");
        let root_text = root.path().to_string_lossy().into_owned();
        let argv: Vec<String> = case["argv_after_cli"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().replace("<fixture>", &root_text))
            .collect();
        let cwd = root.path().join(match case["cwd"].as_str().unwrap() {
            "other" => "elsewhere",
            other => other,
        });
        let home = root.path().join(case["home"].as_str().unwrap());
        let mut input =
            json!({"type":"control_request","request_id":"conf","request":{"subtype":"initialize"}})
                .to_string() + "\n";
        let wants_init = !case["system_init_tools"].is_null();
        if wants_init {
            input += &(json!({"type":"user","message":{"role":"user","content":"x"}}).to_string()
                + "\n");
        }
        let mut child = std::process::Command::new(fake())
            .args(&argv)
            .current_dir(&cwd)
            .env("CLAUDE_CONFIG_DIR", &home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let frames: Vec<Value> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let name = case["case"].as_str().unwrap();
        let body = &frames[0]["response"]["response"];
        let keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut expected_keys: Vec<&str> = record["initialize"]["response_keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        expected_keys.sort_unstable();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, expected_keys, "{name}: initialize keys");
        let command_names: Vec<&str> = body["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            command_names.contains(&"luna-project-canary"),
            case["project_canary"].as_bool().unwrap(),
            "{name}: project canary"
        );
        assert_eq!(
            command_names.contains(&"luna-user-canary"),
            case["user_canary"].as_bool().unwrap(),
            "{name}: user canary"
        );
        assert_eq!(body["output_style"], case["output_style"], "{name}: style");
        assert_eq!(
            body["current_permission_mode"], case["current_permission_mode"],
            "{name}: mode"
        );
        if let Some(ran) = case.get("session_start_hook_ran") {
            assert_eq!(&log_entries(&home)[0]["hooks_ran"], ran, "{name}: hooks");
        }
        if wants_init {
            assert_eq!(
                frames[1]["tools"], case["system_init_tools"],
                "{name}: tools"
            );
        }
    }
}

/// G-1, G-8, G-9, G-10, G-11, G-12: for Ask, Auto and Full, discovery and the
/// run use the same binary, home, cwd and policy flags, so the catalog LUNA
/// shows is exactly what the run loads, and the run's tools match the mode.
#[tokio::test]
async fn discovery_and_launch_agree_for_every_mode() {
    for mode in [
        PermissionMode::Ask,
        PermissionMode::Auto,
        PermissionMode::Full,
    ] {
        let root = tempfile::tempdir().unwrap();
        layout(root.path(), false);
        let project = root.path().join("project");
        let home = root.path().join("config");
        let harness = ClaudeHarness::new()
            .with_executable(fake())
            .with_config_dir(&home);
        let discovered = harness
            .commands_in(&scope(&project, mode, Some("claude-auth:a")))
            .await
            .expect("discovery");
        let mut req = request(&project, mode);
        req.model_options.insert("fastMode".into(), json!("on"));
        req.model_options.insert("thinking".into(), json!(true));
        let events = run_once(&harness, req, false).await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    ..
                }
            )),
            "{mode:?}: {events:?}"
        );

        let log = log_entries(&home);
        let init = log.iter().find(|e| e["kind"] == "initialize").unwrap();
        let run = log.iter().find(|e| e["kind"] == "run").unwrap();
        assert_eq!(log.len(), 2, "{mode:?}: one probe + one run: {log:?}");
        assert_eq!(init["exe"], run["exe"], "{mode:?}: same binary");
        assert_eq!(init["cwd"], run["cwd"], "{mode:?}: same cwd");
        assert_eq!(
            init["config_home"], run["config_home"],
            "{mode:?}: same home"
        );
        assert_eq!(
            policy_flags(&init["argv"]),
            policy_flags(&run["argv"]),
            "{mode:?}: same policy flags"
        );
        assert_eq!(run["initialized_first"], false, "run is not a probe");
        // Discovery never runs hooks and writes no user line (G-12).
        assert_eq!(init["flag_settings"]["disableAllHooks"], true);
        assert_eq!(init["hooks_ran"], false);
        // One merged settings layer per process (G-10).
        assert_eq!(count_flag(&init["argv"], "--settings"), 1);
        assert_eq!(count_flag(&run["argv"], "--settings"), 1);
        assert_eq!(run["flag_settings"]["fastMode"], true);
        assert_eq!(run["flag_settings"]["alwaysThinkingEnabled"], true);
        // The agreement itself.
        assert_eq!(
            json!(names(&discovered)),
            run["commands"],
            "{mode:?}: catalog == loaded commands"
        );
        assert_eq!(init["output_style"], run["output_style"]);
        let tools: Vec<&str> = run["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        match mode {
            PermissionMode::Ask | PermissionMode::Auto => {
                assert_eq!(count_flag(&run["argv"], "--restricted"), 1, "boundary kept");
                assert_eq!(
                    count_flag(&run["argv"], "--dangerously-skip-permissions"),
                    0
                );
                assert!(!discovered.iter().any(|c| c.name == "luna-project-canary"));
                assert!(!discovered.iter().any(|c| c.name == "luna-user-canary"));
                assert_eq!(run["output_style"], "default");
                for tool in ["Bash", "WebFetch", "Read", "Edit", "Glob", "Grep"] {
                    assert!(tools.contains(&tool), "{mode:?}: {tool} granted: {tools:?}");
                }
                for tool in ["Workflow", "CronCreate"] {
                    assert!(!tools.contains(&tool), "{mode:?}: {tool} not granted");
                }
            }
            PermissionMode::Full => {
                assert_eq!(count_flag(&run["argv"], "--restricted"), 0);
                assert!(discovered.iter().any(|c| c.name == "luna-project-canary"));
                assert!(discovered.iter().any(|c| c.name == "luna-user-canary"));
                assert_eq!(run["output_style"], "Explanatory");
                assert!(tools.contains(&"Bash") && tools.contains(&"Workflow"));
            }
        }
    }
}

/// G-2, G-4, G-5, G-6, G-14: the cache is per project, policy, account and
/// configuration state; unchanged scopes reuse it, concurrent scopes don't mix.
#[tokio::test]
async fn discovery_cache_tracks_project_policy_account_and_config_changes() {
    let root = tempfile::tempdir().unwrap();
    layout(root.path(), false);
    let a = root.path().join("project");
    let b = root.path().join("elsewhere");
    let home = root.path().join("config");
    let harness = ClaudeHarness::new()
        .with_executable(fake())
        .with_config_dir(&home);
    let full = PermissionMode::Full;

    let (scope_a, scope_b) = (
        scope(&a, full, Some("acct-1")),
        scope(&b, full, Some("acct-1")),
    );
    let (in_a, in_b) = tokio::join!(harness.commands_in(&scope_a), harness.commands_in(&scope_b));
    let (in_a, in_b) = (in_a.unwrap(), in_b.unwrap());
    assert!(names(&in_a).contains(&"luna-project-canary".to_owned()));
    assert!(!names(&in_b).contains(&"luna-project-canary".to_owned()));
    assert_eq!(kind_count(&home, "initialize"), 2);

    // Unchanged scope: cached.
    let again = harness
        .commands_in(&scope(&a, full, Some("acct-1")))
        .await
        .unwrap();
    assert_eq!(again, in_a);
    assert_eq!(kind_count(&home, "initialize"), 2, "cache hit");

    // Policy switch on the same project: restricted catalog.
    let ask = harness
        .commands_in(&scope(&a, PermissionMode::Ask, Some("acct-1")))
        .await
        .unwrap();
    assert!(!names(&ask).contains(&"luna-project-canary".to_owned()));
    assert_eq!(kind_count(&home, "initialize"), 3);

    // Account switch: re-probe.
    harness
        .commands_in(&scope(&a, full, Some("acct-2")))
        .await
        .unwrap();
    assert_eq!(kind_count(&home, "initialize"), 4);

    // A new project command invalidates exactly that scope.
    std::fs::write(
        a.join(".claude/commands/luna-added.md"),
        "---\ndescription: added later\n---\nx\n",
    )
    .unwrap();
    let added = harness
        .commands_in(&scope(&a, full, Some("acct-1")))
        .await
        .unwrap();
    assert!(names(&added).contains(&"luna-added".to_owned()));
    assert_eq!(kind_count(&home, "initialize"), 5);

    // Settings edit (style) invalidates too.
    std::fs::write(
        a.join(".claude/settings.json"),
        r#"{"outputStyle":"Learning"}"#,
    )
    .unwrap();
    harness
        .commands_in(&scope(&a, full, Some("acct-1")))
        .await
        .unwrap();
    assert_eq!(kind_count(&home, "initialize"), 6);
    let last = log_entries(&home).pop().unwrap();
    assert_eq!(last["output_style"], "Learning");

    // A different provider home is a different user catalog.
    let other_home = root.path().join("config-other");
    std::fs::create_dir_all(&other_home).unwrap();
    let other = ClaudeHarness::new()
        .with_executable(fake())
        .with_config_dir(&other_home);
    let other_a = other
        .commands_in(&scope(&a, full, Some("acct-1")))
        .await
        .unwrap();
    assert!(!names(&other_a).contains(&"luna-user-canary".to_owned()));
    assert!(names(&added).contains(&"luna-user-canary".to_owned()));
}

/// G-7, C-3, G-15: a failed probe surfaces the native error and is retried;
/// the legacy unscoped call still answers (Ask profile, no workspace).
#[tokio::test]
async fn failed_discovery_is_not_cached_and_legacy_call_answers() {
    let root = tempfile::tempdir().unwrap();
    layout(root.path(), false);
    let project = root.path().join("project");
    let home = root.path().join("config");
    let harness = ClaudeHarness::new()
        .with_executable(fake())
        .with_config_dir(&home);
    std::fs::write(project.join(".fake-init-error"), "").unwrap();
    let error = harness
        .commands_in(&scope(&project, PermissionMode::Full, None))
        .await
        .expect_err("initialize error surfaces");
    assert!(
        error.to_string().contains("fixture initialize failure"),
        "{error}"
    );
    std::fs::remove_file(project.join(".fake-init-error")).unwrap();
    let ok = harness
        .commands_in(&scope(&project, PermissionMode::Full, None))
        .await
        .expect("retried");
    assert!(names(&ok).contains(&"luna-project-canary".to_owned()));
    assert_eq!(kind_count(&home, "initialize-error"), 1);
    assert_eq!(kind_count(&home, "initialize"), 1);

    let legacy = harness.commands().await.expect("legacy discovery");
    assert!(!names(&legacy).contains(&"luna-user-canary".to_owned()));
    let entry = log_entries(&home).pop().unwrap();
    assert_eq!(entry["restricted"], true, "unscoped = default Ask profile");
}

/// G-13: a title run keeps exactly one `--tools ""` and never gets the grant.
#[tokio::test]
async fn title_run_keeps_tools_disabled() {
    let root = tempfile::tempdir().unwrap();
    layout(root.path(), false);
    let project = root.path().join("project");
    let home = root.path().join("config");
    let harness = ClaudeHarness::new()
        .with_executable(fake())
        .with_config_dir(&home);
    run_once(&harness, request(&project, PermissionMode::Ask), true).await;
    let run = log_entries(&home).pop().unwrap();
    assert_eq!(count_flag(&run["argv"], "--tools"), 1, "{run}");
    assert_eq!(run["tools"], json!([]));
    assert_eq!(
        count_flag(&run["argv"], "--dangerously-skip-permissions"),
        0
    );
}

/// LIVE E23 acceptance for the lead (real CLI, the user's normal Claude login,
/// network, ONE `claude-haiku-4-5-20251001` turn). Agents never run it.
/// `scripts/cargo.sh test --locked -p luna-harness --test claude_effective_config
///  -- --ignored --nocapture live_e23_effective_configuration`
/// Expected: Ask discovery omits the project canary and Full shows it (both
/// initialize-only); the Ask run has Bash (any escalation is answered
/// "Allow once" through LUNA's approval bridge) and returns `luna-e23-ask`.
#[tokio::test]
#[ignore = "real claude CLI + login + network + one haiku turn (lead QA only)"]
async fn live_e23_effective_configuration() {
    let root = tempfile::Builder::new()
        .prefix("luna-e23-")
        .tempdir()
        .unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".claude/commands")).unwrap();
    std::fs::write(
        project.join(".claude/settings.json"),
        r#"{"outputStyle":"Explanatory"}"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".claude/commands/luna-e23-canary.md"),
        "---\ndescription: E23 canary\n---\nReply with: canary.\n",
    )
    .unwrap();
    let harness = ClaudeHarness::new();
    let ask = harness
        .commands_in(&scope(&project, PermissionMode::Ask, None))
        .await
        .expect("Ask discovery");
    let full = harness
        .commands_in(&scope(&project, PermissionMode::Full, None))
        .await
        .expect("Full discovery");
    eprintln!(
        "E23 discovery: ask={} commands (canary={}), full={} commands (canary={})",
        ask.len(),
        names(&ask).contains(&"luna-e23-canary".to_owned()),
        full.len(),
        names(&full).contains(&"luna-e23-canary".to_owned())
    );
    assert!(!names(&ask).contains(&"luna-e23-canary".to_owned()));
    assert!(names(&full).contains(&"luna-e23-canary".to_owned()));

    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = asked.clone();
    let (steer_tx, steer_rx) = mpsc::channel(1);
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            seen.lock()
                .unwrap()
                .extend(questions.iter().map(|q| q.question.clone()));
            let _ = tx.send(
                questions
                    .iter()
                    .map(|q| UserInputAnswer {
                        question_id: q.id.clone(),
                        labels: vec!["Allow once".into()],
                    })
                    .collect(),
            );
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    };
    let mut req = request(&project, PermissionMode::Ask);
    req.model = Some("claude-haiku-4-5-20251001".into());
    req.prompt =
        "Use the Bash tool to run exactly `echo luna-e23-ask`, then reply with its output only."
            .into();
    let mut stream = harness.run(req, controls).await.expect("live run starts");
    let events = tokio::time::timeout(Duration::from_secs(180), async {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.expect("event");
            let done = matches!(event, AgentEvent::Done { .. });
            events.push(event);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("live turn finished");
    drop(steer_tx);
    let bash = events.iter().any(|e| {
        matches!(e, AgentEvent::ToolCall { call: luna_proto::ToolCall::Exec { command }, .. }
            if command.contains("luna-e23-ask"))
    });
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    eprintln!(
        "E23 Ask run: bash_call={bash} approvals={:?} text={text:?} last={:?}",
        asked.lock().unwrap(),
        events.last()
    );
    // Whether `echo` escalates depends on the CLI's read-only allowlist; the
    // approval list is printed for the QA record, not asserted.
    assert!(bash, "Ask must expose Bash: {events:?}");
    assert!(text.contains("luna-e23-ask"), "{text:?}");
}
