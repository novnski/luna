//! CC-03 subprocess E2E through the real Claude adapter against the
//! deterministic 2.1.286-shaped peer `fixtures/claude-permissions-peer.py`.
//! The peer verifies every native control_response it receives and settles
//! the turn with `peer-verified:<scenario>`; these tests verify what reached
//! the engine's input bridge. Not live-provider proof (see the cc03
//! `live-acceptance` script). Failure cases:
//! `evidence/cc03-permissions/failure-cases.md`.
#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use luna_harness::{CancellationToken, ClaudeHarness, Harness, RunControls, SteerMessage};
use luna_proto::{
    APPROVAL_NO_EXPIRY, AgentEvent, ApprovalActionKind, DoneStatus, PermissionMode, RunRequest,
    SandboxLevel, UserInputAnswer, UserInputQuestion,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

type Parked = (
    Vec<UserInputQuestion>,
    oneshot::Sender<Vec<UserInputAnswer>>,
);

fn peer() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude-permissions-peer.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn policy(mode: PermissionMode) -> luna_proto::PermissionPolicy {
    luna_proto::PermissionPolicy {
        requested_mode: mode,
        effective_mode: mode,
        machine_name: "cc03-machine".into(),
        provider_mapping: "cc03 fixture".into(),
        ..Default::default()
    }
}

struct Run {
    events: tokio::task::JoinHandle<Vec<AgentEvent>>,
    parked: mpsc::UnboundedReceiver<Parked>,
    interrupt: CancellationToken,
    /// Held for the whole run: a closed mailbox would close the CLI's stdin.
    _steer: mpsc::Sender<SteerMessage>,
    log: PathBuf,
    scenario: String,
    asked: Vec<Vec<UserInputQuestion>>,
    _dir: tempfile::TempDir,
}

async fn start(scenario: &str, mode: PermissionMode, extra_prompt: &str) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("peer-log.json");
    let (parked_tx, parked) = mpsc::unbounded_channel::<Parked>();
    let (steer, steering) = mpsc::channel(8);
    let interrupt = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let _ = parked_tx.send((questions, tx));
            rx
        }),
        steering,
        interrupt: interrupt.clone(),
        invoke_luna_tool: None,
    };
    let request = RunRequest {
        review: None,
        input_items: Vec::new(),
        prompt: format!("scenario:{scenario} log:{} {extra_prompt}", log.display()),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: String::new(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: mode == PermissionMode::Full,
        permission_mode: mode,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: Some(policy(mode)),
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    };
    let harness = ClaudeHarness::new()
        .with_executable(peer())
        .with_graces(Duration::from_millis(800), Duration::from_millis(800));
    let stream = harness.run(request, controls).await.expect("run starts");
    let events = tokio::spawn(async move {
        stream
            .map(|event| event.expect("stream event"))
            .collect::<Vec<_>>()
            .await
    });
    Run {
        events,
        parked,
        interrupt,
        _steer: steer,
        log,
        scenario: scenario.into(),
        asked: Vec::new(),
        _dir: dir,
    }
}

impl Run {
    async fn next(&mut self) -> Parked {
        let parked = tokio::time::timeout(Duration::from_secs(10), self.parked.recv())
            .await
            .unwrap_or_else(|_| panic!("{}: no question reached the input bridge", self.scenario))
            .expect("input bridge open");
        self.asked.push(parked.0.clone());
        parked
    }

    async fn finish(mut self) -> (Vec<AgentEvent>, Value) {
        let events = tokio::time::timeout(Duration::from_secs(20), &mut self.events)
            .await
            .unwrap_or_else(|_| panic!("{}: run did not settle", self.scenario))
            .unwrap();
        // Nothing else may have been asked.
        assert!(
            self.parked.try_recv().is_err(),
            "{}: unexpected extra question",
            self.scenario
        );
        let log: Value = std::fs::read(&self.log)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null);
        if let Some(directory) = std::env::var_os("LUNA_CC03_EVIDENCE_DIR") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join(format!("{}.json", self.scenario)),
                serde_json::to_vec_pretty(&json!({
                    "provider": "deterministic claude-permissions peer (Claude Code 2.1.286 control schema)",
                    "scenario": self.scenario,
                    "questions": self.asked,
                    "events": events,
                    "peer": log,
                }))
                .unwrap(),
            )
            .unwrap();
        }
        (events, log)
    }
}

fn answer(tx: oneshot::Sender<Vec<UserInputAnswer>>, question: &UserInputQuestion, label: &str) {
    tx.send(vec![UserInputAnswer {
        question_id: question.id.clone(),
        labels: vec![label.into()],
    }])
    .expect("adapter still waiting");
}

fn assert_verified(events: &[AgentEvent], log: &Value, scenario: &str) {
    match events.last() {
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: Some(result),
            ..
        }) => assert_eq!(result, &format!("peer-verified:{scenario}"), "{log:#}"),
        other => panic!("{scenario}: peer did not verify: {other:?}\npeer log: {log:#}"),
    }
}

fn row<'a>(rows: &'a [(String, String)], label: &str) -> Option<&'a str> {
    rows.iter()
        .find(|(name, _)| name == label)
        .map(|(_, value)| value.as_str())
}

async fn closed(tx: &mut oneshot::Sender<Vec<UserInputAnswer>>) {
    tokio::time::timeout(Duration::from_secs(5), tx.closed())
        .await
        .expect("adapter dropped the withdrawn request's receiver");
}

#[tokio::test]
async fn approval_projects_native_context_and_exact_session_grant() {
    let mut run = start("approval-details", PermissionMode::Ask, "").await;

    let (questions, tx) = run.next().await;
    let [question] = questions.as_slice() else {
        panic!("one approval question: {questions:?}")
    };
    let approval = question.approval.as_ref().expect("typed approval");
    assert_eq!(question.header, "Approve command");
    assert_eq!(
        question.options,
        ["Deny", "Allow once", "Allow for session", "Cancel turn"]
    );
    assert_eq!(approval.provider, luna_proto::HarnessId::ClaudeCode);
    assert_eq!(approval.provider_request_id, "cc03-r1");
    assert_eq!(approval.action_kind, ApprovalActionKind::Command);
    assert_eq!(
        approval.expires_at_ms, APPROVAL_NO_EXPIRY,
        "waits for the user"
    );
    assert!(approval.supports_task_allowance);
    assert_eq!(
        approval.summary,
        "Claude wants to run npm test -- --watch=false"
    );
    let details = approval.details.as_ref().expect("native details");
    assert_eq!(
        details.command.as_deref(),
        Some("npm test -- --watch=false")
    );
    assert_eq!(details.cwd.as_deref(), Some("/tmp/cc03-peer"));
    assert_eq!(
        details.provider_thread_id.as_deref(),
        Some("sess-cc03-approval-details")
    );
    assert_eq!(details.item_id.as_deref(), Some("toolu_cc03_bash"));
    let reason = details.reason.as_deref().unwrap();
    assert!(
        reason.contains("Run the test suite")
            && reason.contains("This command requires approval (other)")
            && !reason.contains('\u{1b}'),
        "{reason}"
    );
    assert_eq!(
        details.proposed_exec_policy_amendment,
        Some(json!(["Bash(npm test:*)"])),
        "only the native allow rule; setMode and deny rules are never offered"
    );
    assert!(
        details.choices[2].description.contains("Bash(npm test:*)")
            && details.choices[2].scope == "session"
    );
    let rows = details.display_rows();
    assert_eq!(
        row(&rows, "Command / input"),
        Some("npm test -- --watch=false")
    );
    answer(tx, question, "Allow for session");

    let (questions, tx) = run.next().await;
    let question = &questions[0];
    let approval = question.approval.as_ref().unwrap();
    let details = approval.details.as_ref().unwrap();
    assert_eq!(question.header, "Approve file change");
    assert_eq!(details.command.as_deref(), Some("/etc/hosts"));
    assert_eq!(
        details.permissions,
        Some(json!({"fileSystem": {"blockedPath": "/etc/hosts"}}))
    );
    assert_eq!(details.grant_root.as_deref(), Some("/etc"));
    answer(tx, question, "Cancel turn");

    let (events, log) = run.finish().await;
    assert_verified(&events, &log, "approval-details");
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::InputRequested { .. } | AgentEvent::InputResolved { .. }
        )),
        "the engine bridge owns the input lifecycle"
    );
}

#[tokio::test]
async fn native_cancel_withdraws_without_reply_and_replays_are_ignored() {
    let mut run = start("cancel-replay", PermissionMode::Ask, "").await;
    let (questions, mut tx) = run.next().await;
    assert_eq!(
        questions[0].approval.as_ref().unwrap().provider_request_id,
        "cc03-r3"
    );
    closed(&mut tx).await;
    // A late Allow after the native cancel cannot reach the CLI.
    assert!(
        tx.send(vec![UserInputAnswer {
            question_id: questions[0].id.clone(),
            labels: vec!["Allow once".into()],
        }])
        .is_err()
    );
    // The replay of cc03-r3 is never rendered: the next card is cc03-r4.
    let (questions, tx) = run.next().await;
    assert_eq!(
        questions[0].approval.as_ref().unwrap().provider_request_id,
        "cc03-r4"
    );
    answer(tx, &questions[0], "Deny");
    let (events, log) = run.finish().await;
    assert_verified(&events, &log, "cancel-replay");
}

#[tokio::test]
async fn duplicate_live_request_renders_once_and_concurrent_answers_route_by_id() {
    let mut run = start("duplicate-concurrent", PermissionMode::Ask, "").await;
    let (first, first_tx) = run.next().await;
    let (second, second_tx) = run.next().await;
    let id = |q: &[UserInputQuestion]| q[0].approval.as_ref().unwrap().provider_request_id.clone();
    assert_eq!(
        (id(&first), id(&second)),
        ("cc03-r5".into(), "cc03-r6".into())
    );
    // Answer out of order.
    answer(second_tx, &second[0], "Allow once");
    answer(first_tx, &first[0], "Deny");
    let (events, log) = run.finish().await;
    assert_verified(&events, &log, "duplicate-concurrent");
}

#[tokio::test]
async fn subagent_requests_reach_the_user_and_mcp_context_is_kept() {
    let mut run = start("subagent-mcp", PermissionMode::Ask, "").await;
    let (questions, tx) = run.next().await;
    let question = &questions[0];
    let approval = question.approval.as_ref().unwrap();
    assert_eq!(question.header, "Approve command · sub-agent a1b2c3");
    assert_eq!(
        approval.summary,
        "Claude sub-agent a1b2c3 requests the Bash tool"
    );
    let details = approval.details.as_ref().unwrap();
    assert_eq!(details.subagent.as_deref(), Some("a1b2c3"));
    assert_eq!(
        row(&details.display_rows(), "Requested by sub-agent"),
        Some("a1b2c3")
    );
    assert_eq!(question.options, ["Deny", "Allow once", "Cancel turn"]);
    assert!(!approval.supports_task_allowance);
    answer(tx, question, "Allow once");

    let (questions, tx) = run.next().await;
    let question = &questions[0];
    let details = question
        .approval
        .as_ref()
        .unwrap()
        .details
        .as_ref()
        .unwrap();
    let tool = details.tool.as_ref().expect("MCP tool context");
    assert_eq!(tool.server, "linear");
    assert_eq!(tool.tool.as_deref(), Some("create_issue"));
    let mut arguments = tool.arguments.clone();
    arguments.sort();
    assert_eq!(
        arguments,
        [
            ("priority".to_string(), "2".to_string()),
            ("title".to_string(), "Bug".to_string())
        ]
    );
    assert_eq!(details.command, None);
    answer(tx, question, "Deny");
    let (events, log) = run.finish().await;
    assert_verified(&events, &log, "subagent-mcp");
}

#[tokio::test]
async fn full_mode_allows_once_but_never_plan_or_interactive_tools() {
    let mut run = start("full-mode", PermissionMode::Full, "").await;
    // Only the user's own ask rule reaches the user in Full.
    let (questions, tx) = run.next().await;
    let question = &questions[0];
    let approval = question.approval.as_ref().unwrap();
    assert_eq!(approval.provider_request_id, "cc03-r12");
    assert!(
        approval
            .details
            .as_ref()
            .unwrap()
            .reason
            .as_deref()
            .unwrap()
            .contains("Your ask rule (userSettings) requires a prompt: Bash(git push:*)")
    );
    answer(tx, question, "Allow once");
    let (events, log) = run.finish().await;
    assert_verified(&events, &log, "full-mode");
}

#[tokio::test]
async fn ask_user_question_keeps_descriptions_and_dismissal_denies() {
    let mut run = start("ask-user", PermissionMode::Ask, "").await;
    let (questions, tx) = run.next().await;
    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0].options, ["Postgres", "SQLite"]);
    assert_eq!(
        questions[0].option_description(0),
        Some("Relational, robust")
    );
    assert_eq!(questions[1].option_description(1), Some("Full text"));
    assert!(
        questions
            .iter()
            .all(|q| q.accepts_typed_answer() && q.approval.is_none())
    );
    assert!(questions[1].multi_select);
    tx.send(vec![
        UserInputAnswer {
            question_id: questions[0].id.clone(),
            labels: vec!["SQLite".into()],
        },
        UserInputAnswer {
            question_id: questions[1].id.clone(),
            labels: vec!["Auth".into(), "Search".into()],
        },
    ])
    .unwrap();
    // The second question set is dismissed: its resolver is dropped unanswered.
    let (questions, tx) = run.next().await;
    assert_eq!(questions[0].question, "Proceed?");
    drop(tx);
    let (events, log) = run.finish().await;
    assert_verified(&events, &log, "ask-user");
}

#[tokio::test]
async fn mcp_elicitation_uses_the_shared_wizard_and_answers_once() {
    let mut run = start("elicitation", PermissionMode::Ask, "").await;
    let (questions, tx) = run.next().await;
    assert_eq!(questions.len(), 3, "two fields plus the action");
    assert!(questions.iter().all(|q| q.mcp.is_some()));
    let field = |name: &str| {
        questions
            .iter()
            .find(|q| q.header == name)
            .unwrap_or_else(|| panic!("field {name}: {questions:?}"))
    };
    assert_eq!(field("visibility").options, ["public", "private"]);
    let action = questions.last().unwrap();
    assert_eq!(action.options, ["Accept", "Decline", "Cancel"]);
    tx.send(vec![
        UserInputAnswer {
            question_id: field("name").id.clone(),
            labels: vec!["luna".into()],
        },
        UserInputAnswer {
            question_id: field("visibility").id.clone(),
            labels: vec!["private".into()],
        },
        UserInputAnswer {
            question_id: action.id.clone(),
            labels: vec!["Accept".into()],
        },
    ])
    .unwrap();

    let (questions, tx) = run.next().await;
    let url = &questions[0];
    let synced = serde_json::to_string(&questions).unwrap();
    assert!(
        !synced.contains("SECRET123"),
        "the authorization URL never reaches synced questions: {synced}"
    );
    assert!(url.question.contains("https://auth.example.com"));
    answer(tx, url, "Decline");

    // The sensitive form is declined without a prompt; the next set is the
    // one the CLI withdraws.
    let (questions, mut tx) = run.next().await;
    assert_eq!(questions[0].header, "label");
    closed(&mut tx).await;
    let (events, log) = run.finish().await;
    assert_verified(&events, &log, "elicitation");
}

#[tokio::test]
async fn unknown_controls_get_an_error_and_undeclared_dialogs_no_answer() {
    let run = start("unknown-controls", PermissionMode::Ask, "").await;
    let (events, log) = run.finish().await;
    assert_verified(&events, &log, "unknown-controls");
}

#[tokio::test]
async fn stop_withdraws_pending_approval_without_a_late_grant() {
    let mut run = start("interrupt", PermissionMode::Ask, "").await;
    let (questions, mut tx) = run.next().await;
    run.interrupt.cancel();
    closed(&mut tx).await;
    assert!(
        tx.send(vec![UserInputAnswer {
            question_id: questions[0].id.clone(),
            labels: vec!["Allow once".into()],
        }])
        .is_err()
    );
    let (events, log) = run.finish().await;
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: DoneStatus::Interrupted,
                ..
            })
        ),
        "{events:?}"
    );
    assert_eq!(log["fails"], json!([]), "{log:#}");
}

#[tokio::test]
async fn weaker_native_mode_fails_closed_and_stricter_mode_is_reported() {
    let run = start(
        "mode-weaker",
        PermissionMode::Ask,
        "report-mode:bypassPermissions",
    )
    .await;
    let (events, _) = run.finish().await;
    let errors: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Error { message } => Some(message.clone()),
            _ => None,
        })
        .collect();
    assert!(
        errors
            .iter()
            .any(|message| message.contains("`bypassPermissions`")
                && message.contains("`acceptEdits`")
                && message.contains("stopped the turn")),
        "{errors:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: DoneStatus::Interrupted,
                ..
            })
        ),
        "{events:?}"
    );

    let run = start("mode-stricter", PermissionMode::Auto, "").await;
    let (events, log) = run.finish().await;
    assert_verified(&events, &log, "mode-stricter");
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message }
                if message.contains("`default`") && message.contains("at least as strict")
        )),
        "{events:?}"
    );
}
