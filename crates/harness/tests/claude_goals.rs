//! CC-14/15 end to end through the real `ClaudeHarness` process boundary and
//! the deterministic 2.1.286 peer `tests/fixtures/fake-claude-goals.py`
//! (shapes from a real no-model probe and the installed bundle; see
//! `evidence/cc14-goals-review/failure-cases.md`, cases G*, S*, R*). No real
//! `claude`, account or model is involved.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::{mpsc, oneshot};

use luna_harness::{
    CancellationToken, ClaudeHarness, Harness, HarnessError, NativeGoalAction, NativeGoalControl,
    RunControls, SteerAcceptance, SteerDelivery, SteerMessage,
};
use luna_proto::{
    AgentEvent, DoneStatus, GoalStartRequest, GoalStatus, NativeScheduleKind, NativeScheduleState,
    PermissionMode, RunRequest, SandboxLevel, ThreadGoal,
};

fn fake() -> PathBuf {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude-goals.py");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Fixture {
    _root: tempfile::TempDir,
    config: PathBuf,
    cwd: PathBuf,
    harness: ClaudeHarness,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("claude-config");
    let cwd = root.path().join("project");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let harness = ClaudeHarness::new()
        .with_executable(fake())
        .with_config_dir(&config);
    Fixture {
        _root: root,
        config,
        cwd,
        harness,
    }
}

fn request(fixture: &Fixture, prompt: &str, goal: bool) -> RunRequest {
    RunRequest {
        review: None,
        goal: goal.then(|| GoalStartRequest {
            objective: prompt.into(),
            token_budget: None,
        }),
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: fixture.cwd.to_str().unwrap().into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let controls = RunControls {
        request_input: Box::new(|_| {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Vec::new());
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    };
    (controls, steer_tx)
}

type Stream = BoxStream<'static, Result<AgentEvent, HarnessError>>;

/// Collect events up to and including the `dones`-th root Done.
async fn until_done(stream: &mut Stream, dones: usize) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    let mut seen = 0;
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(event) = stream.next().await {
            let event = event.expect("stream event");
            let done = matches!(event, AgentEvent::Done { .. });
            events.push(event);
            if done {
                seen += 1;
                if seen == dones {
                    break;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out; events so far: {events:#?}"));
    events
}

async fn run_once(fixture: &Fixture, request: RunRequest) -> Vec<AgentEvent> {
    let (controls, steer) = controls();
    let mut stream = fixture
        .harness
        .run(request, controls)
        .await
        .expect("run starts");
    let events = until_done(&mut stream, 1).await;
    drop(steer);
    events
}

fn goals(events: &[AgentEvent]) -> Vec<(Option<ThreadGoal>, Option<String>)> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::GoalChanged { goal, turn_id, .. } => Some((goal.clone(), turn_id.clone())),
            _ => None,
        })
        .collect()
}

fn errors(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Error { message } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

fn stdin_lines(fixture: &Fixture) -> Vec<serde_json::Value> {
    std::fs::read_to_string(fixture.config.join("stdin.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("stdin line is JSON"))
        .collect()
}

fn stdin_texts(fixture: &Fixture) -> Vec<String> {
    stdin_lines(fixture)
        .iter()
        .filter_map(|line| line["message"]["content"].as_str().map(str::to_owned))
        .collect()
}

fn evidence(name: &str, events: &[AgentEvent], fixture: &Fixture) {
    let Some(dir) = std::env::var_os("LUNA_CC14_EVIDENCE_DIR") else {
        return;
    };
    let dir = Path::new(&dir).join("harness");
    std::fs::create_dir_all(&dir).unwrap();
    let transcript = std::fs::read_dir(fixture.config.join("projects"))
        .ok()
        .and_then(|mut dirs| dirs.next())
        .and_then(Result::ok)
        .and_then(|dir| std::fs::read_dir(dir.path()).ok()?.next()?.ok())
        .and_then(|file| std::fs::read_to_string(file.path()).ok());
    std::fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&serde_json::json!({
            "peer": "fake-claude-goals.py (2.1.286 shapes)",
            "events": events,
            "stdin": stdin_lines(fixture),
            "nativeTranscript": transcript,
        }))
        .unwrap(),
    )
    .unwrap();
}

const MET: &str = "scenario:goal-met DONE.txt exists in the project root";

/// G1, G2, G4, G5: Claude's own `/goal` line; set → not met (reason) → met
/// from the native record, with Claude's own duration and token spend.
#[tokio::test]
async fn goal_start_sends_claude_goal_command_and_reads_the_verdict() {
    let fixture = fixture();
    let events = run_once(&fixture, request(&fixture, MET, true)).await;
    evidence("goal-met", &events, &fixture);

    assert_eq!(stdin_texts(&fixture), vec![format!("/goal {MET}")]);
    assert!(
        stdin_lines(&fixture)
            .iter()
            .all(|line| line["type"] == "user" && !line.to_string().contains("thread/goal")),
        "only stream-json user lines reach Claude"
    );
    let changes = goals(&events);
    assert_eq!(changes.len(), 3, "{changes:#?}");
    let set = changes[0].0.as_ref().expect("active after Goal set");
    assert_eq!(set.status, GoalStatus::Active);
    assert_eq!(set.objective, MET);
    assert_eq!(set.token_budget, None);
    assert_eq!(set.evaluation.as_ref().unwrap().checks, 0);
    let checked = changes[1].0.as_ref().unwrap();
    assert_eq!(checked.status, GoalStatus::Active);
    let evaluation = checked.evaluation.as_ref().unwrap();
    assert_eq!(evaluation.checks, 1);
    assert_eq!(
        evaluation.last_reason.as_deref(),
        Some("DONE.txt does not exist yet")
    );
    let (complete, turn) = &changes[2];
    let complete = complete.as_ref().unwrap();
    assert_eq!(complete.status, GoalStatus::Complete);
    assert_eq!(complete.time_used_seconds, 41);
    assert_eq!(complete.tokens_used, 1820);
    assert_eq!(complete.evaluation.as_ref().unwrap().checks, 2);
    assert!(turn.is_some(), "an in-turn verdict names its turn");
    // The verdict precedes the turn's Done; the feedback is never a message.
    let complete_at = events
        .iter()
        .position(|e| matches!(e, AgentEvent::GoalChanged { goal: Some(g), .. } if g.status == GoalStatus::Complete))
        .unwrap();
    let done_at = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .unwrap();
    assert!(complete_at < done_at);
    assert!(matches!(
        events[done_at],
        AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        }
    ));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::UserMessage { text } if text.contains("Stop hook"))),
        "checker feedback is goal state, not a user message"
    );
}

/// G6: an impossible verdict clears the goal and keeps Claude's reason.
#[tokio::test]
async fn impossible_verdict_clears_the_goal_with_claudes_reason() {
    let fixture = fixture();
    let condition = "scenario:goal-impossible DONE.txt exists";
    let events = run_once(&fixture, request(&fixture, condition, true)).await;
    evidence("goal-impossible", &events, &fixture);
    let changes = goals(&events);
    assert!(changes.last().unwrap().0.is_none(), "{changes:#?}");
    assert!(
        !changes.iter().any(|(goal, _)| goal
            .as_ref()
            .is_some_and(|g| g.status == GoalStatus::Complete)),
        "never shown as achieved"
    );
    let errors = errors(&events);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("impossible") && e.contains("no write access")),
        "{errors:?}"
    );
}

/// G7: Claude's unrecoverable-error clear (real probe wording).
#[tokio::test]
async fn unrecoverable_error_clear_is_reported() {
    let fixture = fixture();
    let condition = "scenario:goal-error DONE.txt exists";
    let events = run_once(&fixture, request(&fixture, condition, true)).await;
    evidence("goal-error-clear", &events, &fixture);
    assert!(goals(&events).last().unwrap().0.is_none());
    assert!(
        errors(&events)
            .iter()
            .any(|e| e.starts_with("Goal cleared after an unrecoverable error")),
        "{:?}",
        errors(&events)
    );
}

/// G3: a native gate refusal is never shown as an active goal.
#[tokio::test]
async fn gate_refusal_is_never_active_and_keeps_claudes_text() {
    let fixture = fixture();
    let condition = "scenario:goal-gate DONE.txt exists";
    let events = run_once(&fixture, request(&fixture, condition, true)).await;
    evidence("goal-gate", &events, &fixture);
    let changes = goals(&events);
    assert!(
        changes.iter().all(|(goal, _)| goal.is_none()),
        "{changes:#?}"
    );
    assert!(
        errors(&events)
            .iter()
            .any(|e| e.contains("/goal can't run while hooks are restricted")),
        "{:?}",
        errors(&events)
    );
}

/// G8: a loop that ends unresolved stays Active (headless never
/// self-continues; the next message resumes evaluation).
#[tokio::test]
async fn unresolved_loop_stays_active_with_the_latest_check() {
    let fixture = fixture();
    let condition = "scenario:goal-stuck the build passes";
    let events = run_once(&fixture, request(&fixture, condition, true)).await;
    evidence("goal-stuck", &events, &fixture);
    let last = goals(&events)
        .last()
        .unwrap()
        .0
        .clone()
        .expect("still active");
    assert_eq!(last.status, GoalStatus::Active);
    let evaluation = last.evaluation.unwrap();
    assert_eq!(evaluation.checks, 2);
    assert_eq!(
        evaluation.last_reason.as_deref(),
        Some("The build has not finished")
    );
}

/// G5/G10: a verdict written after `result` (async transcript writes) is
/// read within the bounded wait instead of being guessed.
#[tokio::test]
async fn late_verdict_is_read_within_the_bounded_wait() {
    let fixture = fixture();
    let condition = "scenario:goal-late DONE.txt exists";
    let events = run_once(&fixture, request(&fixture, condition, true)).await;
    let last = goals(&events).last().unwrap().0.clone().unwrap();
    assert_eq!(last.status, GoalStatus::Complete);
}

/// G1 (warm): a goal sent into the live session is Claude's `/goal` line,
/// accepted at the stdin hand-off; Claude's own frames carry the rest.
#[tokio::test]
async fn warm_goal_start_uses_the_live_session() {
    let fixture = fixture();
    let (controls, steer) = controls();
    let mut stream = fixture
        .harness
        .run(request(&fixture, "hello", false), controls)
        .await
        .unwrap();
    until_done(&mut stream, 1).await;
    let (tx, rx) = oneshot::channel();
    steer
        .send(SteerMessage {
            prompt: MET.into(),
            attachments: Vec::new(),
            input_items: Vec::new(),
            message_id: Some("warm-goal".into()),
            delivery: SteerDelivery::AllowFollowUpTurn,
            acceptance: Some(tx),
            goal: Some(GoalStartRequest {
                objective: MET.into(),
                token_budget: None,
            }),
        })
        .await
        .unwrap();
    assert_eq!(rx.await.unwrap(), SteerAcceptance::Accepted);
    let events = until_done(&mut stream, 1).await;
    drop(steer);
    evidence("goal-warm", &events, &fixture);
    assert_eq!(
        stdin_texts(&fixture),
        vec!["hello".to_owned(), format!("/goal {MET}")]
    );
    let last = goals(&events).last().unwrap().0.clone().unwrap();
    assert_eq!(last.status, GoalStatus::Complete);
}

/// G11/G12: native read and clear outside a run. Read starts no process;
/// clear is a one-shot resumed process sending only Claude's `/goal clear`
/// (a local command: no model turn), never with a bypass flag.
#[tokio::test]
async fn native_goal_read_and_clear_use_claudes_own_record_and_command() {
    let fixture = fixture();
    let condition = "scenario:goal-stuck the build passes";
    let events = run_once(&fixture, request(&fixture, condition, true)).await;
    let session = match events
        .iter()
        .find(|e| matches!(e, AgentEvent::SessionStarted { .. }))
    {
        Some(AgentEvent::SessionStarted { session_id, .. }) => session_id.clone(),
        _ => panic!("no session"),
    };
    let control = |action| NativeGoalControl {
        session_id: session.clone(),
        cwd: fixture.cwd.to_str().unwrap().into(),
        action,
    };
    let lines_before = stdin_lines(&fixture).len();
    let read = fixture
        .harness
        .native_goal_control(control(NativeGoalAction::Read))
        .await
        .unwrap();
    assert_eq!(
        stdin_lines(&fixture).len(),
        lines_before,
        "read starts no process"
    );
    let goal = read.goal.expect("active goal read back");
    assert_eq!(goal.status, GoalStatus::Active);
    assert_eq!(goal.objective, condition);
    assert_eq!(goal.evaluation.unwrap().checks, 2);

    let cleared = fixture
        .harness
        .native_goal_control(control(NativeGoalAction::Clear))
        .await
        .unwrap();
    assert_eq!(cleared.goal, None);
    assert_eq!(
        cleared.reply.as_deref(),
        Some(format!("Goal cleared: {condition}").as_str())
    );
    let again = fixture
        .harness
        .native_goal_control(control(NativeGoalAction::Clear))
        .await
        .unwrap();
    assert_eq!(again.reply.as_deref(), Some("No goal set"));
    assert_eq!(
        stdin_texts(&fixture)[1..],
        ["/goal clear".to_owned(), "/goal clear".to_owned()]
    );
    let bad = fixture
        .harness
        .native_goal_control(NativeGoalControl {
            session_id: "../escape".into(),
            cwd: String::new(),
            action: NativeGoalAction::Read,
        })
        .await;
    assert!(matches!(
        bad,
        Err(HarnessError::RejectedBeforeAcceptance(_))
    ));
}

fn git(cwd: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "-c",
            "user.name=LUNA fixture",
            "-c",
            "user.email=fixture@luna.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn reviews(events: &[AgentEvent]) -> Vec<(String, bool, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::NativeReview {
                id,
                completed,
                text,
            } => Some((id.clone(), *completed, text.clone())),
            _ => None,
        })
        .collect()
}

/// R1, R2, R4, R8, R9: ReportFindings → the shared findings UI with
/// immutable provenance; LUNA opted in; the subagent call stays out; a
/// re-report is a new entry.
#[tokio::test]
async fn report_findings_become_shared_findings_with_provenance() {
    let fixture = fixture();
    git(&fixture.cwd, &["init", "-q"]);
    std::fs::write(fixture.cwd.join("README.md"), "one\n").unwrap();
    git(&fixture.cwd, &["add", "README.md"]);
    git(&fixture.cwd, &["commit", "-q", "-m", "fixture"]);
    std::fs::write(fixture.cwd.join("README.md"), "two\n").unwrap();
    let head = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&fixture.cwd)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();

    let events = run_once(
        &fixture,
        request(&fixture, "scenario:findings review it", false),
    )
    .await;
    evidence("findings", &events, &fixture);
    assert!(
        errors(&events).is_empty(),
        "host opt-in present: {:?}",
        errors(&events)
    );
    let reviews = reviews(&events);
    let ids: Vec<_> = reviews
        .iter()
        .map(|(id, done, _)| (id.as_str(), *done))
        .collect();
    assert_eq!(
        ids,
        [
            ("toolu_rf1", false),
            ("toolu_rf1", true),
            ("toolu_rf2", false),
            ("toolu_rf2", true)
        ],
        "subagent call excluded, re-report is its own entry"
    );
    let first = &reviews[1].2;
    for needle in [
        "1. **`src/auth.rs:42`** The token expiry check is inverted",
        "_correctness · CONFIRMED_",
        "Failure: A token that expired yesterday",
        "2. **`src/db.rs`** Connection is never returned",
        "1 malformed or extra finding(s) not shown.",
        "Claude Code 2.1.286 `ReportFindings`",
        "high effort",
        "+ uncommitted changes",
        "session `c1a0de00-",
        "call `toolu_rf1`",
        "payload sha256 `",
    ] {
        assert!(first.contains(needle), "missing {needle:?} in:\n{first}");
    }
    assert!(
        first.contains(&format!("source `{}`", &head.trim()[..12])),
        "{first}"
    );
    assert!(
        reviews[3].2.contains("_correctness · CONFIRMED · fixed_"),
        "{}",
        reviews[3].2
    );
}

/// R3: empty findings are a clean review; a failed report claims nothing.
#[tokio::test]
async fn empty_and_failed_reports_are_truthful() {
    let fixture = fixture();
    let empty = run_once(
        &fixture,
        request(&fixture, "scenario:findings-empty", false),
    )
    .await;
    let text = &reviews(&empty)[1].2;
    assert!(
        text.starts_with("Review completed with no findings"),
        "{text}"
    );
    assert!(
        text.contains("source revision unknown"),
        "not a repository: {text}"
    );

    let failed = run_once(
        &fixture,
        request(&fixture, "scenario:findings-error", false),
    )
    .await;
    evidence("findings-error", &failed, &fixture);
    let text = &reviews(&failed)[1].2;
    assert!(
        text.starts_with("Claude's findings report failed"),
        "{text}"
    );
    assert!(text.contains("InputValidationError"), "{text}");
}

/// S1, S2, S3, S5: Claude's schedules are observed with their native ids
/// and lifetimes; deletes, stops and lists settle them; LUNA writes nothing.
#[tokio::test]
async fn native_schedules_follow_claudes_own_lifecycle() {
    let fixture = fixture();
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let events = run_once(&fixture, request(&fixture, "scenario:schedules", false)).await;
    evidence("schedules", &events, &fixture);
    assert_eq!(
        stdin_texts(&fixture),
        vec!["scenario:schedules".to_owned()],
        "LUNA never schedules"
    );
    let snapshots: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::NativeSchedules { schedules, .. } => Some(schedules.clone()),
            _ => None,
        })
        .collect();
    assert!(snapshots.len() >= 5, "{snapshots:#?}");
    let last = snapshots.last().unwrap();
    assert_eq!(last.len(), 3, "{last:#?}");
    let recurring = last.iter().find(|s| s.id == "a1b2c3d4").unwrap();
    assert_eq!(recurring.kind, NativeScheduleKind::Cron);
    assert_eq!(recurring.schedule, "every 5 minutes");
    assert_eq!(recurring.cron.as_deref(), Some("*/5 * * * *"));
    assert!(recurring.recurring && !recurring.durable);
    assert_eq!(recurring.state, NativeScheduleState::Cancelled);
    let expires = recurring.expires_at_ms.unwrap() - started;
    assert!((7 * 86_400_000 - 60_000..=7 * 86_400_000 + 60_000).contains(&expires));
    let one_shot = last.iter().find(|s| s.id == "e5f6a7b8").unwrap();
    assert_eq!(one_shot.schedule, "at 15:30 on Oct 1");
    assert!(!one_shot.recurring && !one_shot.durable);
    assert_eq!(one_shot.state, NativeScheduleState::Scheduled);
    assert_eq!(one_shot.expires_at_ms, None);
    let wakeup = last
        .iter()
        .find(|s| s.kind == NativeScheduleKind::Wakeup)
        .unwrap();
    assert_eq!(wakeup.state, NativeScheduleState::Cancelled);
    assert!(
        wakeup
            .schedule
            .starts_with("in 10m — CI takes about ten minutes"),
        "{}",
        wakeup.schedule
    );
    assert_eq!(wakeup.prompt, "/loop check CI");
    assert!(
        !last.iter().any(|s| s.id == "99999999"),
        "subagent schedule excluded"
    );
}
