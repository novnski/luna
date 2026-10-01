//! CC-14/15 end to end: UI task commands -> engine -> the real ClaudeHarness
//! -> the deterministic 2.1.286 peer `harness/tests/fixtures/fake-claude-goals.py`
//! (shapes from a real no-model probe and the installed bundle). No installed
//! Claude, account, credential or model is involved; the synthetic identity
//! file holds only an account id.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use luna_doc::{MessagePart, MessageRole, SessionMessageEntry};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::ClaudeHarness;
use luna_proto::{
    ChatConfig, CommandReceipt, CommandReceiptStatus, ContextControlState, GoalStartRequest,
    GoalStatus, HarnessId, NativeReviewRequest, NativeScheduleState, PermissionMode, ReviewTarget,
    RunRequest, SandboxLevel, TaskCommandRead,
};
use luna_rpc::{RpcClient, methods};

fn fake_claude() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-claude-goals.py");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Fixture {
    root: tempfile::TempDir,
    core: EngineCore,
    client: RpcClient,
}

impl Fixture {
    fn claude_dir(&self) -> PathBuf {
        self.root.path().join("synthetic-claude")
    }
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let claude_dir = root.path().join("synthetic-claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    // Account identity only (no credential): LUNA resolves Claude's native
    // account from this file before it launches the CLI.
    std::fs::write(
        root.path().join("synthetic-claude.json"),
        r#"{"oauthAccount":{"accountUuid":"claude-goal-fixture"}}"#,
    )
    .unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(
        ClaudeHarness::new()
            .with_executable(fake_claude())
            .with_config_dir(&claude_dir),
    ));
    let accounts = AgentAccountsConfig {
        data_dir: root.path().join("provider-state"),
        claude_config_dir: claude_dir,
        claude_config_file: root.path().join("synthetic-claude.json"),
        codex_home: root.path().join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.path().join("data"), "cc14-org", "cc14-user"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
        accounts,
    )
    .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    Fixture { root, core, client }
}

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::ClaudeCode,
        provider_account_id: None,
        model: Some("claude-haiku-4-5-20251001".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn request(prompt: &str, cwd: &str) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: Some(HarnessId::ClaudeCode),
        provider_account_id: None,
        model: Some("claude-haiku-4-5-20251001".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: Vec::new(),
        worktree: None,
    }
}

fn goal_request(objective: &str, cwd: &str) -> RunRequest {
    RunRequest {
        goal: Some(GoalStartRequest {
            objective: objective.into(),
            token_budget: None,
        }),
        ..request(objective, cwd)
    }
}

async fn snapshot(client: &RpcClient, task_id: &str) -> TaskCommandRead {
    client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": task_id }),
        )
        .await
        .unwrap()
}

async fn execute(
    client: &RpcClient,
    command_id: &str,
    task_id: &str,
    snapshot: Option<&TaskCommandRead>,
    intent: serde_json::Value,
) -> CommandReceipt {
    let mut value = serde_json::json!({
        "commandId": command_id,
        "actorId": "test-user",
        "clientId": "test-client",
        "clientSessionId": "test-session",
        "taskId": task_id,
        "intent": intent,
    });
    if let Some(snapshot) = snapshot {
        value["expectedTaskRevision"] = snapshot.snapshot.task_revision.into();
        if let Some(generation) = snapshot.snapshot.binding_generation {
            value["expectedBindingGeneration"] = generation.into();
        }
    }
    client
        .call_as(methods::EXECUTE_TASK_COMMAND, value)
        .await
        .unwrap()
}

/// A managed (UUID) Claude task in its own workspace folder.
async fn create_task(fixture: &Fixture, name: &str) -> (String, String) {
    let task_id = uuid::Uuid::new_v4().to_string();
    let workspace = fixture.root.path().join(name);
    std::fs::create_dir_all(&workspace).unwrap();
    let cwd = workspace.to_str().unwrap().to_owned();
    fixture
        .client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createSpace", "spaceId": format!("space-{name}"),
                "deviceId": fixture.core.device_id, "path": cwd
            }),
        )
        .await
        .unwrap();
    let receipt = execute(
        &fixture.client,
        &format!("create-{task_id}"),
        &task_id,
        None,
        serde_json::json!({
            "kind": "createChat",
            "spaceId": format!("space-{name}"),
            "cwd": cwd,
            "config": config(),
        }),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied, "{receipt:?}");
    (task_id, cwd)
}

async fn send(
    fixture: &Fixture,
    task_id: &str,
    message_id: &str,
    request: RunRequest,
) -> CommandReceipt {
    let read = snapshot(&fixture.client, task_id).await;
    execute(
        &fixture.client,
        message_id,
        task_id,
        Some(&read),
        serde_json::json!({
            "kind": "session",
            "command": {"kind": "run", "request": request, "messageId": message_id},
        }),
    )
    .await
}

async fn goal_control(
    fixture: &Fixture,
    task_id: &str,
    command_id: &str,
    operation: serde_json::Value,
) -> CommandReceipt {
    let read = snapshot(&fixture.client, task_id).await;
    execute(
        &fixture.client,
        command_id,
        task_id,
        Some(&read),
        serde_json::json!({"kind": "goal", "operation": operation}),
    )
    .await
}

fn entries(fixture: &Fixture, task_id: &str) -> Vec<SessionMessageEntry> {
    fixture
        .core
        .doc_host
        .open(task_id)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap_or_default()
}

fn text(entry: &SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn control(fixture: &Fixture, task_id: &str) -> ContextControlState {
    fixture.core.doc_host.context_control(task_id).unwrap()
}

async fn wait_until(fixture: &Fixture, task_id: &str, what: &str, done: impl Fn() -> bool) {
    let settled = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if done()
                && snapshot(&fixture.client, task_id)
                    .await
                    .snapshot
                    .active_turn_id
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        settled.is_ok(),
        "{what} never happened\ncontrol: {:?}\nentries: {:?}\nstdin: {}",
        control(fixture, task_id),
        entries(fixture, task_id)
            .iter()
            .map(|entry| (entry.role, entry.id.clone(), text(entry)))
            .collect::<Vec<_>>(),
        std::fs::read_to_string(fixture.claude_dir().join("stdin.jsonl")).unwrap_or_default()
    );
}

fn stdin_texts(fixture: &Fixture) -> Vec<String> {
    std::fs::read_to_string(fixture.claude_dir().join("stdin.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line).ok()?["message"]["content"]
                .as_str()
                .map(str::to_owned)
        })
        // LUNA's isolated title run also launches the CLI; it is not the task.
        .filter(|text| !text.starts_with("You generate session titles."))
        .collect()
}

fn starts(fixture: &Fixture) -> Vec<serde_json::Value> {
    std::fs::read_to_string(fixture.claude_dir().join("starts.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        // LUNA's isolated title run (asynchronous) is not a task process.
        .filter(|start| {
            !start["argv"].as_array().is_some_and(|argv| {
                argv.iter().any(|arg| {
                    arg.as_str()
                        .is_some_and(|arg| arg.starts_with("You generate session titles."))
                })
            })
        })
        .collect()
}

fn write_evidence(name: &str, fixture: &Fixture, task_id: &str) {
    let Some(directory) = std::env::var_os("LUNA_CC14_EVIDENCE_DIR") else {
        return;
    };
    let directory = Path::new(&directory).join("engine");
    std::fs::create_dir_all(&directory).unwrap();
    let transcript: Vec<_> = entries(fixture, task_id)
        .iter()
        .map(|entry| serde_json::json!({"id": entry.id, "role": entry.role, "parts": entry.parts}))
        .collect();
    std::fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&serde_json::json!({
            "provider": "fake-claude-goals.py (Claude Code 2.1.286 shapes)",
            "transcript": transcript,
            "contextControl": control(fixture, task_id),
            "claudeStdin": stdin_texts(fixture),
            "claudeProcesses": starts(fixture),
        }))
        .unwrap(),
    )
    .unwrap();
}

const MET: &str = "scenario:goal-met DONE.txt exists in the project root";

/// G1, G5, G9, G15: a Claude goal on a cold task is Claude's own `/goal`
/// line; Claude's met verdict marks the turn that ran the loop.
#[tokio::test]
async fn claude_goal_on_a_cold_task_runs_claudes_goal_and_marks_it_achieved() {
    let fixture = fixture();
    let (task, cwd) = create_task(&fixture, "cold-goal").await;
    let receipt = send(&fixture, &task, "goal-message", goal_request(MET, &cwd)).await;
    assert_ne!(
        receipt.status,
        CommandReceiptStatus::Rejected,
        "{receipt:?}"
    );
    wait_until(&fixture, &task, "goal achieved", || {
        !control(&fixture, &task).goal_marks.achieved.is_empty()
    })
    .await;
    write_evidence("cold-goal", &fixture, &task);

    assert_eq!(stdin_texts(&fixture), vec![format!("/goal {MET}")]);
    let entries = entries(&fixture, &task);
    let users: Vec<_> = entries
        .iter()
        .filter(|e| e.role == MessageRole::User)
        .collect();
    assert_eq!(users.len(), 1, "the goal message appears once");
    assert_eq!(users[0].id, "goal-message");
    assert_eq!(text(users[0]), MET);
    let state = control(&fixture, &task);
    assert!(state.goal_marks.is_sent("goal-message"));
    let readback = state.goal.unwrap();
    assert!(
        !readback.continuation_ready,
        "headless Claude never self-continues"
    );
    let goal = readback.goal.unwrap();
    assert_eq!(goal.status, GoalStatus::Complete);
    assert_eq!(goal.objective, MET);
    assert_eq!(goal.token_budget, None);
    let mark = &state.goal_marks.achieved[0];
    assert_eq!(mark.time_used_seconds, 41);
    let achieved = entries.iter().find(|e| e.id == mark.entry_id).unwrap();
    assert_eq!(achieved.role, MessageRole::Assistant);
    assert!(
        text(achieved).contains("Created DONE.txt."),
        "{}",
        text(achieved)
    );
}

/// G1 (warm), G11, G12: a warm Claude goal reuses the live session; Clear
/// retires it and runs Claude's own `/goal clear` in one resumed process;
/// pause/budget are refused truthfully; Get reads Claude's record.
#[tokio::test]
async fn claude_goal_warm_start_clear_and_unsupported_controls() {
    let fixture = fixture();
    let (task, cwd) = create_task(&fixture, "warm-goal").await;
    for (index, message) in ["first", "second"].into_iter().enumerate() {
        send(&fixture, &task, message, request("hello", &cwd)).await;
        wait_until(&fixture, &task, "ordinary reply", || {
            entries(&fixture, &task)
                .iter()
                .filter(|e| e.role == MessageRole::Assistant && text(e).contains("ok"))
                .count()
                > index
        })
        .await;
    }
    let processes_before_goal = starts(&fixture).len();
    let condition = "scenario:goal-stuck the build passes";
    let receipt = send(&fixture, &task, "warm-goal", goal_request(condition, &cwd)).await;
    assert_ne!(
        receipt.status,
        CommandReceiptStatus::Rejected,
        "{receipt:?}"
    );
    wait_until(&fixture, &task, "goal checked twice", || {
        control(&fixture, &task)
            .goal
            .and_then(|r| r.goal)
            .and_then(|g| g.evaluation)
            .is_some_and(|e| e.checks == 2)
    })
    .await;
    assert_eq!(
        starts(&fixture).len(),
        processes_before_goal,
        "the goal used the live Claude session"
    );
    assert!(control(&fixture, &task).goal_marks.is_sent("warm-goal"));
    let goal = control(&fixture, &task).goal.unwrap().goal.unwrap();
    assert_eq!(goal.status, GoalStatus::Active);
    assert_eq!(
        goal.evaluation.unwrap().last_reason.as_deref(),
        Some("The build has not finished")
    );

    let paused = goal_control(
        &fixture,
        &task,
        "pause",
        serde_json::json!({"action": "set", "status": "paused"}),
    )
    .await;
    assert_eq!(paused.status, CommandReceiptStatus::Rejected, "{paused:?}");
    assert!(
        paused.reason.as_ref().unwrap().message.contains("no pause"),
        "{paused:?}"
    );

    let read = goal_control(&fixture, &task, "get", serde_json::json!({"action": "get"})).await;
    assert_eq!(read.status, CommandReceiptStatus::Applied, "{read:?}");
    assert_eq!(
        control(&fixture, &task).goal.unwrap().goal.unwrap().status,
        GoalStatus::Active
    );

    let cleared = goal_control(
        &fixture,
        &task,
        "clear",
        serde_json::json!({"action": "clear"}),
    )
    .await;
    assert_eq!(cleared.status, CommandReceiptStatus::Applied, "{cleared:?}");
    assert!(control(&fixture, &task).goal.unwrap().goal.is_none());
    let processes = starts(&fixture);
    let control_process = processes.last().unwrap();
    assert!(control_process["resume"].is_string(), "{control_process}");
    let argv: Vec<&str> = control_process["argv"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a.as_str())
        .collect();
    assert!(argv.contains(&"--restricted") && !argv.contains(&"--dangerously-skip-permissions"));
    assert_eq!(
        stdin_texts(&fixture).last().map(String::as_str),
        Some("/goal clear")
    );
    assert!(
        !fixture.core.sessions.has_live_run(&task),
        "one writer: the warm session retired"
    );
    write_evidence("warm-goal-clear", &fixture, &task);
}

/// G13: a Claude goal is refused while a turn runs (Claude would run it
/// later as an unannounced prompt).
#[tokio::test]
async fn claude_goal_is_refused_while_a_turn_runs() {
    let fixture = fixture();
    let (task, cwd) = create_task(&fixture, "busy-goal").await;
    send(&fixture, &task, "slow", request("scenario:slow", &cwd)).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.core.sessions.turn_in_flight(&task) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("slow turn starts");
    let receipt = send(&fixture, &task, "busy-goal", goal_request(MET, &cwd)).await;
    assert_eq!(
        receipt.status,
        CommandReceiptStatus::Rejected,
        "{receipt:?}"
    );
    assert!(
        receipt
            .reason
            .as_ref()
            .unwrap()
            .message
            .contains("between turns"),
        "{receipt:?}"
    );
    assert!(
        !stdin_texts(&fixture)
            .iter()
            .any(|line| line.starts_with("/goal"))
    );
}

/// R1, R2, R5: the review menu on a Claude task runs Claude's own
/// `/code-review` with the mapped target, and its ReportFindings land on the
/// shared findings UI with provenance.
#[tokio::test]
async fn claude_review_runs_code_review_and_shows_shared_findings() {
    let fixture = fixture();
    let (task, cwd) = create_task(&fixture, "review").await;
    let review = RunRequest {
        review: Some(NativeReviewRequest {
            target: ReviewTarget::BaseBranch {
                branch: "main".into(),
            },
        }),
        ..request("Review changes against main", &cwd)
    };
    let receipt = send(&fixture, &task, "review-message", review).await;
    assert_ne!(
        receipt.status,
        CommandReceiptStatus::Rejected,
        "{receipt:?}"
    );
    wait_until(&fixture, &task, "findings", || {
        entries(&fixture, &task)
            .iter()
            .any(|e| text(e).contains("### Review findings"))
    })
    .await;
    write_evidence("review", &fixture, &task);
    assert_eq!(
        stdin_texts(&fixture),
        vec!["/code-review main...HEAD".to_owned()]
    );
    let entries = entries(&fixture, &task);
    let user = entries
        .iter()
        .find(|e| e.role == MessageRole::User)
        .unwrap();
    assert_eq!(text(user), "/code-review main...HEAD");
    let findings = entries
        .iter()
        .find(|e| text(e).contains("### Review findings"))
        .unwrap();
    let body = text(findings);
    assert!(body.contains("**`src/auth.rs:42`**"), "{body}");
    assert!(body.contains("call `toolu_rf1`"), "{body}");
    assert!(
        body.contains("call `toolu_rf2`"),
        "the re-report is its own entry: {body}"
    );
    assert!(
        findings
            .parts
            .iter()
            .any(|part| matches!(part, MessagePart::Tool { .. })),
        "the shared review lifecycle chip"
    );

    // A malformed ref never reaches Claude.
    let bad = RunRequest {
        review: Some(NativeReviewRequest {
            target: ReviewTarget::BaseBranch {
                branch: "--upload-pack=x".into(),
            },
        }),
        ..request("bad", &cwd)
    };
    let receipt = send(&fixture, &task, "bad-review", bad).await;
    assert_eq!(
        receipt.status,
        CommandReceiptStatus::Rejected,
        "{receipt:?}"
    );
}

/// S1–S4: Claude's own schedules persist with their native ids/states for
/// the current binding; LUNA schedules nothing.
#[tokio::test]
async fn claude_schedules_persist_for_the_binding() {
    let fixture = fixture();
    let (task, cwd) = create_task(&fixture, "schedules").await;
    send(
        &fixture,
        &task,
        "schedules",
        request("scenario:schedules", &cwd),
    )
    .await;
    wait_until(&fixture, &task, "schedules", || {
        control(&fixture, &task).native_schedules.len() == 3
    })
    .await;
    write_evidence("schedules", &fixture, &task);
    let schedules = control(&fixture, &task).native_schedules;
    let state = |id: &str| schedules.iter().find(|s| s.id == id).map(|s| s.state);
    assert_eq!(state("a1b2c3d4"), Some(NativeScheduleState::Cancelled));
    assert_eq!(state("e5f6a7b8"), Some(NativeScheduleState::Scheduled));
    assert_eq!(state("wakeup"), Some(NativeScheduleState::Cancelled));
    assert_eq!(stdin_texts(&fixture), vec!["scenario:schedules".to_owned()]);
}
