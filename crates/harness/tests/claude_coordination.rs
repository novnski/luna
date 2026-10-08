//! CC-13 E2E through the real `ClaudeHarness` and the fake CLI
//! (`tests/fixtures/fake-claude.sh`): native workflow/teammate task graphs,
//! inherited session-identity scrubbing, Claude's external-session listing and
//! the two-writer resume guard. Frame and row shapes come from the installed
//! 2.1.286 binary (SDK schema + emitter) and the real `claude agents --json`
//! probe in `evidence/cc13-advanced/probes/bg/`.
//!
//! Every test holds `ENV_LOCK`: one test sets process environment, and the
//! others spawn children that read it.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use luna_harness::{CancellationToken, ClaudeHarness, Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, ClaudeExternalSessionActivity, ClaudeExternalSessionKind,
    ClaudeExternalSessionState, DoneStatus, NativeTaskEndReason, NativeTaskKind,
    NativeTaskSnapshot, NativeTaskStatus, NativeWorkflowAgentState, RunRequest, SandboxLevel,
};

/// Async-aware: every test holds it across awaits.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-claude.sh");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        goal: None,
        review: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: String::new(),
        sandbox: SandboxLevel::DangerFullAccess,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

fn controls() -> RunControls {
    let (_steer_tx, steer_rx) = mpsc::channel(8);
    RunControls {
        request_input: Box::new(|_| {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Vec::new());
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    }
}

async fn run_to_end(harness: &ClaudeHarness, req: RunRequest) -> Vec<AgentEvent> {
    let stream = harness.run(req, controls()).await.expect("run starts");
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time")
}

fn native_tasks(events: &[AgentEvent]) -> Vec<NativeTaskSnapshot> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::NativeTaskChanged { task } => Some((**task).clone()),
            _ => None,
        })
        .collect()
}

/// W-1…W-11, T-1: the workflow graph through start, progress (deduped,
/// schema-less and full snapshots), pause/resume, completion enrichment and
/// late frames. A teammate is its own kind, unknown IDs are never captured,
/// and the workflow never becomes a subagent.
#[tokio::test]
async fn native_workflow_graph_survives_the_parked_wake_shape() {
    let _env = ENV_LOCK.lock().await;
    let harness = ClaudeHarness::new().with_executable(fixture_path());
    let events = run_to_end(&harness, request("scenario:workflow")).await;

    // The launching turn and the wake turn both settle; the Workflow call is
    // an ordinary tool chip, never a subagent spawn.
    let dones = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    ..
                }
            )
        })
        .count();
    assert_eq!(dones, 2, "launch turn + wake turn");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "toolu_wf"))
    );
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Subagent { parent_tool_use_id, .. } if parent_tool_use_id == "toolu_wf")),
        "a workflow must never route as a subagent"
    );

    let tasks: Vec<_> = native_tasks(&events)
        .into_iter()
        .filter(|task| task.kind != NativeTaskKind::Shell)
        .collect();
    let ids: Vec<(&str, NativeTaskStatus)> = tasks
        .iter()
        .map(|t| (t.task_id.as_str(), t.status))
        .collect();
    assert_eq!(
        ids,
        vec![
            ("wf1", NativeTaskStatus::Running),   // task_started
            ("wf1", NativeTaskStatus::Running),   // first graph (duplicate deduped)
            ("wf1", NativeTaskStatus::Running),   // schema-less progress: totals only
            ("wf1", NativeTaskStatus::Running),   // full snapshot
            ("wf1", NativeTaskStatus::Paused),    // task_updated paused
            ("wf1", NativeTaskStatus::Running),   // resumed
            ("tm1", NativeTaskStatus::Running),   // experimental teammate
            ("tm1", NativeTaskStatus::Stopped),   // worker_restart
            ("wf1", NativeTaskStatus::Completed), // task_updated completed
            ("wf1", NativeTaskStatus::Completed), // notification enrichment
        ],
        "ghost IDs and late frames produce nothing; shell lifecycle is verified separately"
    );
    let revisions: Vec<u64> = tasks.iter().map(|t| t.revision).collect();
    assert!(
        revisions.windows(2).all(|w| w[0] < w[1]),
        "revisions are monotonic"
    );

    let started = &tasks[0];
    assert_eq!(started.kind, NativeTaskKind::Workflow);
    assert_eq!(started.name.as_deref(), Some("audit-routes"));
    assert_eq!(started.description, "Audit routes");
    assert_eq!(started.tool_use_id.as_deref(), Some("toolu_wf"));
    assert_eq!(started.provider_session_id.as_deref(), Some("sess-wf"));
    assert!(!started.graph_observed);

    let first_graph = &tasks[1];
    assert!(first_graph.graph_observed);
    assert_eq!(first_graph.phases.len(), 1);
    assert_eq!(first_graph.agents.len(), 1);
    assert_eq!(
        first_graph.agents[0].state,
        NativeWorkflowAgentState::Running
    );
    assert_eq!(
        first_graph.last_activity.as_deref(),
        Some("Discover: list routes")
    );
    assert_eq!(
        first_graph.description, "Audit routes",
        "run description is kept"
    );

    let schema_less = &tasks[2];
    assert_eq!(schema_less.total_tokens, Some(1_500));
    assert_eq!(
        schema_less.agents, first_graph.agents,
        "absent graph keeps the last one"
    );

    let full = &tasks[3];
    assert_eq!(
        full.phases
            .iter()
            .map(|p| p.title.as_str())
            .collect::<Vec<_>>(),
        ["Discover", "Audit"]
    );
    assert_eq!(full.agents.len(), 3, "a snapshot replaces, never appends");
    assert_eq!(full.agents[0].state, NativeWorkflowAgentState::Done);
    assert_eq!(full.agents[0].tokens, Some(1_400));
    assert_eq!(full.agents[2].state, NativeWorkflowAgentState::Failed);
    assert!(full.agents[2].blocked);
    assert!(
        full.agents[2]
            .error
            .as_deref()
            .unwrap()
            .contains("safety classifier")
    );

    let teammate = &tasks[7];
    assert_eq!(teammate.kind, NativeTaskKind::Teammate);
    assert_eq!(
        teammate.end_reason,
        Some(NativeTaskEndReason::WorkerRestart)
    );

    let done = tasks.last().unwrap();
    assert_eq!(
        done.status,
        NativeTaskStatus::Completed,
        "late failure never flips it"
    );
    assert_eq!(
        done.output_file.as_deref(),
        Some("/tmp/claude-501/-tmp/sess-wf/tasks/wf1.output")
    );
    assert_eq!(
        done.summary.as_deref(),
        Some("Dynamic workflow \"Audit routes\" completed")
    );
    assert_eq!(
        done.total_tokens,
        Some(3_400),
        "late progress never applies"
    );
    assert_eq!(done.agents.len(), 3);

    // The document ledger applies the same rules to replays and restarts.
    let mut ledger = luna_proto::NativeTaskLedger::default();
    for task in &tasks {
        ledger.upsert(task.clone());
    }
    let mut resurrect = done.clone();
    resurrect.status = NativeTaskStatus::Running;
    assert!(
        !ledger.upsert(resurrect),
        "terminal is sticky in the ledger"
    );
    let mut flip = done.clone();
    flip.status = NativeTaskStatus::Failed;
    assert!(
        !ledger.upsert(flip),
        "a second outcome never replaces the first"
    );
    assert_eq!(ledger.tasks.len(), 2);
    assert!(!ledger.has_active());
}

/// C-3: a LUNA-launched child never inherits a parent Claude session's
/// messaging/bridge identity, and unrelated environment is kept.
#[tokio::test]
async fn launched_claude_child_drops_inherited_session_identity() {
    let _env = ENV_LOCK.lock().await;
    let names = [
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_CODE_MESSAGING_TOKEN",
        "CLAUDE_CODE_BRIDGE_SESSION_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_SESSION_ATTENDED",
        "CLAUDE_JOB_DIR",
    ];
    // SAFETY: ENV_LOCK serializes every test in this binary, so no other
    // thread reads the environment while it changes.
    unsafe {
        for name in names {
            std::env::set_var(name, "inherited-from-parent-claude-session");
        }
        std::env::set_var("LUNA_ENV_SENTINEL", "kept");
    }
    let harness = ClaudeHarness::new().with_executable(fixture_path());
    let events = run_to_end(&harness, request("scenario:env-scrub")).await;
    unsafe {
        for name in names {
            std::env::remove_var(name);
        }
        std::env::remove_var("LUNA_ENV_SENTINEL");
    }
    let done = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Done { status, error, .. } => Some((*status, error.clone())),
            _ => None,
        })
        .expect("run settles");
    assert_eq!(done, (DoneStatus::Completed, None), "child saw: {done:?}");
}

/// A wrapper CLI whose `agents --json` prints `rows` and whose other
/// invocations record a marker, then exec the shared fake.
fn wrapper(dir: &Path, rows: &str, agents_exit: i32) -> PathBuf {
    let path = dir.join("claude");
    let marker = dir.join("launched");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = agents ]; then\n  [ \"$2\" = --json ] || exit 64\n  cat <<'ROWS'\n{rows}\nROWS\n  exit {agents_exit}\nfi\n: > '{marker}'\nexec '{fake}' \"$@\"\n",
        marker = marker.display(),
        fake = fixture_path().display(),
    );
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A live process that is not this engine's child (its shell parent exits,
/// so it is reparented).
fn orphan_sleeper() -> u32 {
    let out = std::process::Command::new("sh")
        .args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn kill(pid: u32) {
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
}

/// B-4: a conversation another live Claude process holds is refused before
/// LUNA launches anything, with the native handoff command.
#[tokio::test]
async fn resume_held_by_an_external_background_session_is_refused() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let external = orphan_sleeper();
    // Row shape from the real 2.1.286 `claude agents --json` probe.
    let rows = format!(
        r#"[{{"pid":{external},"id":"7c5dcf5d","cwd":"/tmp/x","kind":"background","startedAt":1790864754191,"sessionId":"sess-held","name":"other","status":"busy","state":"working"}}]"#
    );
    let exe = wrapper(dir.path(), &rows, 0);
    let mut req = request("scenario:happy");
    req.resume = Some("sess-held".into());
    let result = ClaudeHarness::new()
        .with_executable(exe)
        .run(req, controls())
        .await;
    kill(external);
    let Err(HarnessError::RejectedBeforeAcceptance(message)) = result else {
        panic!("expected a pre-acceptance refusal");
    };
    assert!(message.contains("claude stop 7c5dcf5d"), "{message}");
    assert!(
        !dir.path().join("launched").exists(),
        "no Claude turn was launched"
    );
}

/// The configured provider home must govern both Settings readback and the
/// pre-launch writer check, rather than whichever home the engine inherited.
#[tokio::test]
async fn external_sessions_and_resume_use_the_configured_provider_home() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("provider-home");
    std::fs::create_dir(&config).unwrap();
    let external = orphan_sleeper();
    let rows = format!(
        r#"[{{"pid":{external},"id":"held-in-custom-home","kind":"background","sessionId":"sess-held","state":"working"}}]"#
    );
    let rows_path = config.join("sessions.json");
    std::fs::write(&rows_path, &rows).unwrap();
    let exe = wrapper(dir.path(), "[]", 0);
    let source = std::fs::read_to_string(&exe).unwrap();
    let listing = format!(
        "if [ \"$CLAUDE_CONFIG_DIR\" = '{config}' ]; then cat '{rows_path}'; else printf '%s\\n' '{rows}'; fi",
        config = config.display(),
        rows_path = rows_path.display(),
    );
    std::fs::write(&exe, source.replace("cat <<'ROWS'\n[]\nROWS", &listing)).unwrap();
    let harness = ClaudeHarness::new()
        .with_executable(exe)
        .with_config_dir(&config);
    let view = harness.external_sessions().await;
    let mut req = request("scenario:happy");
    req.resume = Some("sess-held".into());
    let blocked = harness.run(req.clone(), controls()).await;
    // The inherited home still reports the writer. Clearing only the pinned
    // home's listing must allow this independent provider instance to resume.
    std::fs::write(&rows_path, "[]").unwrap();
    let empty_view = harness.external_sessions().await;
    let resumed = harness.run(req, controls()).await;
    kill(external);
    assert!(view.unavailable_reason.is_none());
    assert_eq!(view.sessions.len(), 1);
    assert!(matches!(
        blocked,
        Err(HarnessError::RejectedBeforeAcceptance(_))
    ));
    assert!(empty_view.unavailable_reason.is_none());
    assert!(
        empty_view.sessions.is_empty(),
        "readback used the wrong home"
    );
    assert!(
        resumed.is_ok(),
        "resume checked another provider home's writer"
    );
    let events = tokio::time::timeout(
        Duration::from_secs(10),
        resumed
            .unwrap()
            .map(|event| event.unwrap())
            .collect::<Vec<_>>(),
    )
    .await
    .expect("resumed turn settled");
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        }
    )));
    assert!(dir.path().join("launched").exists());
}

/// The display limit is not a safety boundary: a writer after the first 200
/// rows must still be found before the resume process is launched.
#[tokio::test]
async fn resume_checks_writers_beyond_the_external_session_display_limit() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let external = orphan_sleeper();
    let mut rows = vec![serde_json::json!({"sessionId": "other", "state": "stopped"}); 201];
    rows.push(serde_json::json!({
        "pid": external, "id": "held-after-display-limit", "kind": "background",
        "sessionId": "sess-held", "state": "working",
    }));
    let exe = wrapper(dir.path(), &serde_json::to_string(&rows).unwrap(), 0);
    let harness = ClaudeHarness::new().with_executable(exe);
    let view = harness.external_sessions().await;
    let mut req = request("scenario:happy");
    req.resume = Some("sess-held".into());
    let result = harness.run(req, controls()).await;
    kill(external);
    assert!(view.truncated && view.sessions.len() == 200);
    assert!(matches!(
        result,
        Err(HarnessError::RejectedBeforeAcceptance(_))
    ));
    assert!(!dir.path().join("launched").exists());
}

/// B-4/B-5: rows that are not a live foreign writer never block — LUNA's own
/// print-mode child, a stopped background row and other conversations.
#[tokio::test]
async fn resume_ignores_luna_owned_stopped_and_unrelated_rows() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let mut own_child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let own = own_child.id();
    let external = orphan_sleeper();
    let rows = format!(
        r#"[
{{"pid":{own},"cwd":"/tmp/x","kind":"interactive","startedAt":1790864718981,"sessionId":"sess-held","name":"project-4f","status":"idle"}},
{{"id":"9bc63a21","cwd":"/tmp/x","kind":"background","startedAt":1790864753579,"sessionId":"sess-held","name":"cc13-probe","state":"stopped"}},
{{"pid":{external},"cwd":"/tmp/y","kind":"interactive","startedAt":1,"sessionId":"another-conversation","status":"busy"}}
]"#
    );
    let exe = wrapper(dir.path(), &rows, 0);
    let mut req = request("scenario:happy");
    req.resume = Some("sess-held".into());
    let events = run_to_end(&ClaudeHarness::new().with_executable(exe), req).await;
    let _ = own_child.kill();
    let _ = own_child.wait();
    kill(external);
    assert!(dir.path().join("launched").exists());
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        }
    )));
}

/// B-4 fail-open: an unreadable listing never blocks an ordinary resume.
#[tokio::test]
async fn resume_proceeds_when_the_session_listing_is_unavailable() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let exe = wrapper(dir.path(), "not json", 2);
    let mut req = request("scenario:happy");
    req.resume = Some("sess-held".into());
    let events = run_to_end(&ClaudeHarness::new().with_executable(exe), req).await;
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        }
    )));
}

/// B-1/B-2/B-5: the read-only listing maps the real row shapes, marks this
/// engine's own children, and reports an unreadable listing explicitly.
#[tokio::test]
async fn external_session_listing_maps_real_rows_and_explicit_unavailability() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let mut own_child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let own = own_child.id();
    let rows = format!(
        r#"[
{{"pid":{own},"cwd":"/tmp/x","kind":"interactive","startedAt":1790864718981,"sessionId":"11111111-2222-4333-8444-555555555555","name":"project-4f","status":"idle"}},
{{"pid":1,"id":"7c5dcf5d","cwd":"/tmp/x","kind":"background","startedAt":1790864754191,"sessionId":"9bc63a21-6fa6-420e-a00f-38b62bfbd78b","name":"cc13-probe","status":"waiting","waitingFor":"permission prompt","state":"blocked"}},
{{"id":"9bc63a21","cwd":"/tmp/x","kind":"background","startedAt":1790864753579,"sessionId":"9bc63a21-6fa6-420e-a00f-38b62bfbd78b","name":"cc13-probe","state":"stopped"}},
{{"cwd":"/tmp/z","kind":"future-kind","startedAt":2,"state":"hibernating","status":"dreaming"}}
]"#
    );
    let exe = wrapper(dir.path(), &rows, 0);
    let view = ClaudeHarness::new()
        .with_executable(exe)
        .external_sessions()
        .await;
    let _ = own_child.kill();
    let _ = own_child.wait();
    assert_eq!(view.unavailable_reason, None);
    assert_eq!(view.sessions.len(), 4);
    let own_row = &view.sessions[0];
    assert_eq!(own_row.kind, ClaudeExternalSessionKind::Interactive);
    assert!(
        own_row.process_alive && own_row.owned_by_luna,
        "this engine's own child"
    );
    let blocked = &view.sessions[1];
    assert_eq!(blocked.short_id.as_deref(), Some("7c5dcf5d"));
    assert_eq!(blocked.state, Some(ClaudeExternalSessionState::Blocked));
    assert_eq!(
        blocked.activity,
        Some(ClaudeExternalSessionActivity::Waiting)
    );
    assert_eq!(blocked.waiting_for.as_deref(), Some("permission prompt"));
    assert!(!blocked.process_alive, "pid 1 is never a session process");
    let stopped = &view.sessions[2];
    assert_eq!(stopped.state, Some(ClaudeExternalSessionState::Stopped));
    assert!(!stopped.process_alive && !stopped.owned_by_luna);
    let future = &view.sessions[3];
    assert_eq!(future.kind, ClaudeExternalSessionKind::Unknown);
    assert_eq!(
        future.state,
        Some(ClaudeExternalSessionState::Unknown),
        "never 'working'"
    );
    assert_eq!(
        future.activity,
        Some(ClaudeExternalSessionActivity::Unknown)
    );

    let broken = wrapper(dir.path(), "{\"not\":\"an array\"}", 0);
    let view = ClaudeHarness::new()
        .with_executable(broken)
        .external_sessions()
        .await;
    assert!(view.sessions.is_empty());
    assert!(
        view.unavailable_reason.is_some(),
        "unreadable is not 'no sessions'"
    );
    let failing = wrapper(dir.path(), "", 1);
    let view = ClaudeHarness::new()
        .with_executable(failing)
        .external_sessions()
        .await;
    assert!(view.unavailable_reason.unwrap().contains("agents --json"));
}

/// Lead QA only: replays a REAL 2.1.286 workflow capture (from
/// `evidence/cc13-advanced/live-acceptance.py`) through the production
/// harness. Run with `LUNA_CLAUDE_WORKFLOW_CAPTURE=<abs frames.jsonl>` and
/// `--ignored`. Prints how workflow agents' own frames were attributed.
#[tokio::test]
#[ignore = "needs a live capture from live-acceptance.py"]
async fn live_workflow_capture_replays_into_a_settled_graph() {
    let _env = ENV_LOCK.lock().await;
    let capture = std::env::var("LUNA_CLAUDE_WORKFLOW_CAPTURE").expect("capture path");
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("claude");
    std::fs::write(
        &exe,
        format!("#!/bin/sh\nread -r first\ncat '{capture}'\nsleep 1\n"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let events = run_to_end(
        &ClaudeHarness::new().with_executable(exe),
        request("live workflow replay"),
    )
    .await;
    let tasks = native_tasks(&events);
    let workflow: Vec<_> = tasks
        .iter()
        .filter(|t| t.kind == NativeTaskKind::Workflow)
        .collect();
    assert!(!workflow.is_empty(), "no local_workflow task observed");
    let last = workflow.last().unwrap();
    assert!(
        last.status.is_terminal(),
        "workflow never settled: {:?}",
        last.status
    );
    assert!(
        tasks.windows(2).all(|w| w[0].revision < w[1].revision),
        "revisions are monotonic"
    );
    let mut parents = std::collections::BTreeMap::<String, usize>::new();
    for event in &events {
        if let AgentEvent::Subagent {
            parent_tool_use_id, ..
        } = event
        {
            *parents.entry(parent_tool_use_id.clone()).or_default() += 1;
        }
    }
    println!(
        "LIVE workflow: id={} name={:?} status={:?} phases={} agents={} graph_observed={} tool_use_id={:?} subagent_events_by_parent={parents:?}",
        last.task_id,
        last.name,
        last.status,
        last.phases.len(),
        last.agents.len(),
        last.graph_observed,
        last.tool_use_id,
    );
}

/// Slice 3: a background shell remains observable after its launching tool
/// returns, without becoming a subagent or ending the parent turn.
#[tokio::test]
async fn shell_lifecycle_is_observed_without_subagent_reclassification() {
    let _guard = ENV_LOCK.lock().await;
    let harness = ClaudeHarness::new().with_executable(fixture_path());
    let events = run_to_end(&harness, request("scenario:workflow")).await;
    let shell: Vec<_> = native_tasks(&events)
        .into_iter()
        .filter(|task| task.task_id == "bg1")
        .collect();
    assert_eq!(shell.len(), 2, "running and completed shell observations");
    assert_eq!(serde_json::to_value(shell[0].kind).unwrap(), "shell");
    assert_eq!(shell[0].status, NativeTaskStatus::Running);
    assert_eq!(shell[1].status, NativeTaskStatus::Completed);
    assert!(!events.iter().any(|event| matches!(event,
        AgentEvent::Subagent { parent_tool_use_id, .. } if parent_tool_use_id == "toolu_bash")));
    if let Ok(dir) = std::env::var("LUNA_WORK_EVIDENCE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join("claude-shell-native-lifecycle.json"),
            serde_json::to_vec_pretty(&shell).unwrap(),
        )
        .unwrap();
    }
}
