//! ClaudeHarness integration tests against the fake CLI in
//! `tests/fixtures/fake-claude.sh` (no real `claude` binary involved).
//! A live smoke test against the real CLI lives at the bottom, `#[ignore]`d.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use luna_harness::{
    CancellationToken, ClaudeHarness, Harness, HarnessError, RunControls, SteerAcceptance,
    SteerMessage,
};
use luna_proto::{
    AgentEvent, DoneStatus, HarnessId, ReasoningLevel, RunRequest, SandboxLevel, ToolCall,
    UserInputAnswer, UserInputQuestion,
};

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-claude.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    path
}

fn harness() -> ClaudeHarness {
    ClaudeHarness::new().with_executable(fixture_path())
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
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

/// Controls whose `request_input` answers every question with `answer_label`.
fn controls(
    answer_label: &'static str,
) -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec![answer_label.into()],
                })
                .collect();
            let _ = tx.send(answers);
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
        invoke_luna_tool: None,
    };
    (controls, steer_tx, token)
}

async fn run_to_end(
    harness: &ClaudeHarness,
    req: RunRequest,
    controls: RunControls,
) -> Vec<AgentEvent> {
    let stream = harness.run(req, controls).await.expect("run starts");
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time")
}

#[tokio::test]
async fn synthetic_luna_instructions_append_without_replacing_provider_prompt() {
    let root = tempfile::tempdir().unwrap();
    let mut req = request("scenario:runtime-instructions");
    req.cwd = root.path().to_str().unwrap().into();
    req.model_options.insert(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(), serde_json::json!({
        "version": 1,
        "contentHash": "synthetic",
        "environment": {"LUNA_THREAD_ID":"thread-a","LUNA_ENVIRONMENT_ID":"environment-a","LUNA_MACHINE_ID":"machine-a"},
        "localEndpoints": [],
        "context": {
            "appVersion":"test", "protocolVersion":1, "threadId":"thread-a",
            "parentThreadId":null, "sourceThreadId":null, "projectId":null, "projectName":null,
            "environmentId":"environment-a", "machineId":"machine-a", "machineName":"machine-a",
            "workspaceRoot":"/synthetic", "worktreeId":null, "branch":null,
            "harness":"claude-code", "model":null, "reasoning":null,
            "executionRoute":"local", "grantedScopes":[]
        },
        "instructions":[{"name":"LUNA core","source":"lunaCore","provenance":"synthetic","text":"Run inside LUNA."}],
        "tools":[], "omittedTools":[], "delivery":"claudeInstructionsOnly"
    }));
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), req, controls).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn happy_path_normalizes_events_and_tags_subagents() {
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness(), request("scenario:happy"), controls).await;

    // One SessionStarted despite the re-emitted init frame.
    let starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SessionStarted {
                harness,
                model,
                tools,
                session_id,
                ..
            } => Some((harness, model, tools, session_id)),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 1, "init must be deduped: {events:?}");
    let (h, model, tools, session_id) = starts[0];
    assert_eq!(*h, HarnessId::ClaudeCode);
    assert_eq!(model, "claude-fable-5");
    assert_eq!(tools, &vec!["Bash".to_string(), "Read".to_string()]);
    assert_eq!(session_id, "sess-1");

    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "pondering".into()
    }));
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Hello".into()
    }));

    // Subagent frames (parent_tool_use_id set) arrive TAGGED — never as bare
    // parent-feed events.
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::TextDelta { text } if text.contains("SUBAGENT")
        )),
        "subagent delta leaked into the parent feed: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCall { id, .. } | AgentEvent::ToolResult { id, .. } if id == "sub-tool"
        )),
        "subagent tool frames leaked into the parent feed: {events:?}"
    );
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "sub-1".into(),
        event: Box::new(AgentEvent::TextDelta {
            text: "SUBAGENT".into()
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "sub-1".into(),
        event: Box::new(AgentEvent::ToolCall {
            id: "sub-tool".into(),
            call: ToolCall::Exec {
                command: "echo sub".into()
            },
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "sub-1".into(),
        event: Box::new(AgentEvent::ToolResult {
            id: "sub-tool".into(),
            is_error: false,
            output: None,
            diff: None,
        }),
    }));

    // Typed tool decoding: Bash -> Exec, mcp__server__tool -> Mcp.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "tool-1".into(),
        call: ToolCall::Exec {
            command: "ls -la".into()
        },
    }));
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "tool-2".into(),
        call: ToolCall::Mcp {
            server: "linear".into(),
            tool: "search".into(),
            input: Some(serde_json::json!({"q": "bug"})),
        },
    }));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::AssistantMessageCompleted { .. }))
    );
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "tool-1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "tool-2".into(),
        is_error: true,
        output: None,
        diff: None,
    }));

    // Informational rate-limit frames stay quiet.
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Error { .. })));

    assert!(events.contains(&AgentEvent::Usage {
        input_tokens: 10,
        output_tokens: 20
    }));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: Some("done!".into()),
            error: None,
            session_id: Some("sess-1".into()),
        })
    );
}

#[tokio::test]
async fn eager_done_forwards_wake_turn_as_second_done() {
    // The background-subagent shape (live-verified 2.1.228): result #1 is
    // eager — the run must NOT hold the turn for the subagent — and the wake
    // turn's frames flow through the SAME stream, settling with result #2.
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness(), request("scenario:wake"), controls).await;

    let done_positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, AgentEvent::Done { .. }).then_some(i))
        .collect();
    assert_eq!(
        done_positions.len(),
        2,
        "eager done + wake done: {events:?}"
    );

    // One SessionStarted total — the wake init is deduped.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SessionStarted { .. }))
            .count(),
        1
    );

    // The subagent's interior streams tagged BETWEEN the two dones.
    let tagged_position = events
        .iter()
        .position(|e| {
            matches!(e, AgentEvent::Subagent { parent_tool_use_id, .. } if parent_tool_use_id == "toolu_agent")
        })
        .expect("tagged subagent traffic present");
    assert!(
        done_positions[0] < tagged_position && tagged_position < done_positions[1],
        "subagent interior must stream between the eager done and the wake done: {events:?}"
    );

    // The wake turn's own (untagged) output precedes the second done.
    let wake_text = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "subagent finished"))
        .expect("wake-turn delta present");
    assert!(done_positions[0] < wake_text && wake_text < done_positions[1]);

    // Both dones settle Completed with the same session id.
    for i in done_positions {
        assert!(matches!(
            &events[i],
            AgentEvent::Done {
                status: DoneStatus::Completed,
                session_id: Some(id),
                ..
            } if id == "sess-wake"
        ));
    }
}

#[tokio::test]
async fn ask_user_question_round_trips_through_the_control_channel() {
    // The questions must reach the ENGINE's input bridge (`request_input`) —
    // and the harness must NOT emit its own `InputRequested`/`InputResolved`
    // twins: the bridge owns that lifecycle (it mints the request id the
    // resolver is parked under; a harness-emitted copy folded an unanswerable
    // duplicate chip into the doc).
    let asked: Arc<Mutex<Vec<UserInputQuestion>>> = Arc::new(Mutex::new(Vec::new()));
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let _steer = steer_tx;
    let token = CancellationToken::new();
    let seen = asked.clone();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            seen.lock().unwrap().extend(questions.iter().cloned());
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec!["B".into()],
                })
                .collect();
            let _ = tx.send(answers);
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
        invoke_luna_tool: None,
    };
    let mut req = request("scenario:askuser");
    req.permission_mode = luna_proto::PermissionMode::Full;
    req.auto_approve = true;
    req.permission_policy = Some(luna_proto::PermissionPolicy {
        requested_mode: luna_proto::PermissionMode::Full,
        effective_mode: luna_proto::PermissionMode::Full,
        scope: luna_proto::PermissionScope::Full,
        reviewer: luna_proto::ApprovalReviewer::None,
        escalation: luna_proto::PermissionEscalation::None,
        machine_name: "test-machine".into(),
        provider_mapping: "test full".into(),
        ..Default::default()
    });
    let events = run_to_end(&harness(), req, controls).await;

    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].header, "Choice");
    assert_eq!(asked[0].question, "Pick one");
    assert_eq!(asked[0].options, vec!["A".to_string(), "B".to_string()]);
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::InputRequested { .. } | AgentEvent::InputResolved { .. }
        )),
        "harness must not emit input lifecycle events itself: {events:?}"
    );

    // "answered" proves both control round-trips: the plain Bash can_use_tool
    // was auto-allowed AND the answers reached the CLI as updatedInput.answers
    // keyed by question text.
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: Some("answered".into()),
            error: None,
            session_id: Some("sess-ask".into()),
        })
    );
}

#[tokio::test]
async fn steering_lines_are_written_to_stdin_mid_run() {
    let (controls, steer, _token) = controls("A");
    steer
        .send(SteerMessage {
            input_items: Vec::new(),
            attachments: Vec::new(),
            prompt: "redirect please".into(),
            message_id: None,
            delivery: luna_harness::SteerDelivery::AllowFollowUpTurn,
            acceptance: None,
        })
        .await
        .expect("steer queued");
    let events = run_to_end(&harness(), request("scenario:steer"), controls).await;

    let steered = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Steered {
                assistant_message_id,
                next_assistant_message_id,
                ..
            } => Some((
                assistant_message_id.clone(),
                next_assistant_message_id.clone(),
            )),
            _ => None,
        })
        .expect("Steered emitted");
    assert!(steered.0.is_some() && steered.1.is_some());
    assert_ne!(steered.0, steered.1);

    // The fake CLI echoes the steer line's content back as a delta.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "steered:redirect please".into()
    }));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn explicit_steer_does_not_claim_incorporation_from_a_text_echo() {
    let (controls, steer, _token) = controls("A");
    let (acceptance_tx, acceptance_rx) = oneshot::channel();
    steer
        .send(SteerMessage {
            input_items: Vec::new(),
            attachments: Vec::new(),
            prompt: "redirect please".into(),
            message_id: Some("queued-steer".into()),
            delivery: luna_harness::SteerDelivery::RequireActiveTurn,
            acceptance: Some(acceptance_tx),
        })
        .await
        .expect("steer queued");
    let events = run_to_end(&harness(), request("scenario:steer"), controls).await;

    assert_eq!(
        acceptance_rx.await.expect("acceptance response"),
        SteerAcceptance::Accepted
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Steered { .. })),
        "Claude root user echoes carry no durable steer correlation id: {events:?}"
    );
}

#[tokio::test]
async fn explicit_claude_write_ack_without_echo_is_not_incorporated() {
    let (controls, steer, _token) = controls("A");
    let (acceptance_tx, acceptance_rx) = oneshot::channel();
    steer
        .send(SteerMessage {
            input_items: Vec::new(),
            attachments: Vec::new(),
            prompt: "redirect please".into(),
            message_id: Some("queued-steer".into()),
            delivery: luna_harness::SteerDelivery::RequireActiveTurn,
            acceptance: Some(acceptance_tx),
        })
        .await
        .expect("steer queued");
    let events = run_to_end(
        &harness(),
        request("scenario:steer-accepted-only"),
        controls,
    )
    .await;

    assert_eq!(
        acceptance_rx.await.expect("acceptance response"),
        SteerAcceptance::Accepted
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Steered { .. })),
        "stdin write success alone must not synthesize incorporation: {events:?}"
    );
}

#[tokio::test]
async fn interrupt_escalates_to_sigterm_and_ends_with_interrupted_done() {
    let harness = ClaudeHarness::new()
        .with_executable(fixture_path())
        .with_graces(Duration::from_millis(100), Duration::from_millis(500));
    let (controls, _steer, token) = controls("A");
    let mut stream = harness
        .run(request("scenario:interrupt"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(ev, AgentEvent::SessionStarted { .. }) {
                token.cancel(); // interrupt as soon as the session is up
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("interrupt completed in time");

    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Interrupted,
            result: None,
            error: None,
            session_id: Some("sess-int".into()),
        })
    );
}

#[tokio::test]
async fn error_codes_map_to_readable_messages() {
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness(), request("scenario:error"), controls).await;

    let errors: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Error { message } => Some(message.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        errors.contains(&"Claude usage limit reached — try again after the limit resets."),
        "assistant error code not mapped: {errors:?}"
    );
    assert!(
        errors.contains(
            &"Claude 5-hour limit reached — the turn was blocked. Try again after it resets."
        ),
        "rejected rate_limit_event not mapped: {errors:?}"
    );

    // Empty `errors` array on the result falls back to subtype wording.
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Errored,
            result: None,
            error: Some("The run hit the maximum number of turns.".into()),
            session_id: Some("sess-err".into()),
        })
    );
}

#[tokio::test]
async fn missing_binary_is_not_installed() {
    let harness = ClaudeHarness::new().with_executable("/nonexistent/claude-nowhere");
    let (controls, _steer, _token) = controls("A");
    let err = harness
        .run(request("scenario:happy"), controls)
        .await
        .err()
        .expect("spawn fails");
    assert!(matches!(err, HarnessError::NotInstalled(_)), "{err:?}");
}

#[tokio::test]
async fn captured_live_background_subagent_frames_replay_correctly() {
    // Frames captured VERBATIM from claude 2.1.228 (2026-08-17): a turn that
    // spawns a background Agent subagent — eager result while it runs, tagged
    // subagent traffic, then the wake turn (second init, same session id,
    // second result). Replayed through the fake-CLI transport so the whole
    // driver path (wire parse → normalize → run loop) is exercised, not just
    // the normalizer.
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude")
        .join("live-2.1.228-background-subagent.jsonl");
    let script = std::fs::read_to_string(&fixture).expect("fixture readable");
    // A one-off cat-style fake CLI: reads the prompt line, plays the capture.
    // The capture contains one can_use_tool control_request; the driver
    // auto-allows it on stdin, which this replayer ignores.
    let dir = std::env::temp_dir().join(format!("claude-replay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let frames = dir.join("frames.jsonl");
    std::fs::write(&frames, &script).expect("frames written");
    let cli = dir.join("replay.sh");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nread -r _first || exit 1\ncat '{}'\n",
            frames.display()
        ),
    )
    .expect("replayer written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let harness = ClaudeHarness::new().with_executable(&cli);
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness, request("replay"), controls).await;

    // One SessionStarted (the wake init dedupes), two Dones (eager + wake).
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SessionStarted { .. }))
            .count(),
        1,
        "{events:?}"
    );
    let dones: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, AgentEvent::Done { .. }).then_some(i))
        .collect();
    assert_eq!(dones.len(), 2, "eager done + wake done: {events:?}");

    // The parent-feed Agent spawn is a plain tool call; the subagent's own
    // Bash call arrives tagged with the spawning tool-use id, between the
    // two dones, and never as a bare parent event.
    let spawn_id = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCall { id, call } => match call {
                ToolCall::Unknown { name, .. } if name.starts_with("Agent") => Some(id.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("Agent spawn tool call in the parent feed");
    // The synthesized opening user message rides WITH the spawn (before the
    // eager done); the child's own interior streams between the two dones.
    let opening: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if *parent_tool_use_id == spawn_id
                        && matches!(event.as_ref(), AgentEvent::UserMessage { .. })
            )
            .then_some(i)
        })
        .collect();
    assert_eq!(opening.len(), 1, "one seeded opening prompt: {events:?}");
    assert!(opening[0] < dones[0], "opening rides with the spawn");
    let tagged: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if *parent_tool_use_id == spawn_id
                        && !matches!(event.as_ref(), AgentEvent::UserMessage { .. })
            )
            .then_some(i)
        })
        .collect();
    assert!(!tagged.is_empty(), "tagged subagent traffic: {events:?}");
    assert!(
        tagged.iter().all(|i| dones[0] < *i && *i < dones[1]),
        "subagent interior streams between the eager and wake dones"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Subagent { event, .. }
                if matches!(event.as_ref(), AgentEvent::ToolCall { call: ToolCall::Exec { .. }, .. })
        )),
        "subagent Bash call arrives tagged: {events:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Live smoke against the REAL claude CLI (2.1.x, must be installed + authed):
/// one trivial turn through the stdio permission channel, ending on the
/// result frame. `cargo test -p luna-harness --test claude -- --ignored`.
#[tokio::test]
#[ignore = "spawns the real claude CLI; needs install + auth + network"]
async fn live_real_cli_single_turn() {
    let harness = ClaudeHarness::new();
    let mut req = request("Reply with exactly the word: pong");
    req.model = Some("haiku".into());
    req.cwd = std::env::temp_dir().display().to_string();
    req.auto_approve = false; // exercise --permission-prompt-tool stdio
    let (controls, _steer, _token) = controls("A");
    let mut stream = harness.run(req, controls).await.expect("run starts");
    // The session PARKS after the turn (steering mailbox still open, the CLI
    // waits for more stdin) — collect up to the first Done, not stream end.
    let events = tokio::time::timeout(Duration::from_secs(120), async {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            let done = matches!(ev, AgentEvent::Done { .. });
            events.push(ev);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("live turn finished in time");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionStarted { .. })),
        "{events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

// ---------------------------------------------------------------------------
// Slash-command discovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn commands_come_from_the_initialize_control_request() {
    let h = harness();
    let commands = h.commands().await.expect("discovery succeeds");
    assert_eq!(
        commands.len(),
        2,
        "nameless entries are dropped: {commands:?}"
    );
    assert_eq!(commands[0].name, "review");
    assert_eq!(commands[0].description, "Review a pull request");
    assert_eq!(commands[0].input_hint.as_deref(), Some("[pr number]"));
    assert_eq!(commands[1].name, "compact");
    assert_eq!(commands[1].input_hint, None, "empty hint reads as None");

    // Cached: the second call reuses the first probe's result (the fake has
    // exited; a re-probe against a dead binary path would still work here,
    // but object identity of the cached list is the cheap assertion).
    let again = h.commands().await.expect("cache hit");
    assert_eq!(again, commands);
}

#[tokio::test]
async fn models_come_from_the_initialize_control_request() {
    let models = harness().live_models().await.expect("live CLI catalog");
    assert!(!models.iter().any(|model| model.id == "default"));
    let fresh = models
        .iter()
        .find(|model| model.id == "claude-fresh-7")
        .expect("a newly advertised model appears without a bundled entry");
    assert_eq!(fresh.label, "Fresh 7");
    assert!(fresh.reasoning_levels.contains(&ReasoningLevel::High));
    let long_context = models
        .iter()
        .find(|model| model.id == "claude-fable-5[1m]")
        .expect("live long-context model");
    assert_eq!(long_context.label, "Fable 5");
    assert_eq!(
        long_context
            .options
            .iter()
            .find(|option| option.id == "contextWindow")
            .map(|option| option.default_choice.as_str()),
        Some("1m")
    );
}

/// Read-only live CLI smoke: no user message or model turn is sent.
#[tokio::test]
#[ignore]
async fn live_models_discovery() {
    let models = ClaudeHarness::new()
        .live_models()
        .await
        .expect("live models");
    assert!(models.iter().any(|model| model.id.starts_with("claude-")));
    eprintln!("{} Claude model rows discovered", models.len());
}

/// Live smoke against the real CLI: `cargo test -p luna-harness --test
/// claude -- --ignored live_commands`. No model turn, no API cost.
#[tokio::test]
#[ignore]
async fn live_commands_discovery() {
    let h = ClaudeHarness::new();
    let commands = h.commands().await.expect("live discovery");
    assert!(!commands.is_empty());
    eprintln!("{} commands, first: {:?}", commands.len(), commands.first());
}

#[tokio::test]
async fn title_run_disables_tools_and_denies_unexpected_permissions() {
    let (controls, _steer, token) = controls("Yes");
    let mut stream = harness()
        .run_title(request("scenario:title"), controls)
        .await
        .unwrap();
    let events = tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            let done = matches!(event, AgentEvent::Done { .. });
            events.push(event);
            if done {
                break;
            }
        }
        events
    })
    .await
    .unwrap();
    token.cancel();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text == "Fix Login Flow")),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        )),
        "{events:?}"
    );
}

// ---------------------------------------------------------------------------
// cc01 transport/control (CLI 2.1.286 shapes). Failure cases:
// $B/evidence/cc01-transport/failure-cases.md. Frames come from the installed
// binary's SDK schema and initialization-only handshakes; no model turn.
// ---------------------------------------------------------------------------

fn compat_ledger() -> serde_json::Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/compat-2.1.286.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("ledger fixture")).expect("ledger")
}

fn explicit_steer(id: &str) -> (SteerMessage, oneshot::Receiver<SteerAcceptance>) {
    let (acceptance_tx, acceptance_rx) = oneshot::channel();
    (
        SteerMessage {
            input_items: Vec::new(),
            attachments: Vec::new(),
            prompt: "redirect please".into(),
            message_id: Some(id.into()),
            delivery: luna_harness::SteerDelivery::RequireActiveTurn,
            acceptance: Some(acceptance_tx),
        },
        acceptance_rx,
    )
}

/// Drive a run; once the CLI's init is normalized (so its capabilities are
/// known), queue `steer`, and optionally interrupt `stop_after` later.
async fn run_steering_after_start(
    harness: &ClaudeHarness,
    req: RunRequest,
    controls: RunControls,
    steer_tx: mpsc::Sender<SteerMessage>,
    steer: SteerMessage,
    stop: Option<(CancellationToken, Duration)>,
) -> Vec<AgentEvent> {
    let mut stream = harness.run(req, controls).await.expect("run starts");
    let mut steer = Some(steer);
    tokio::time::timeout(Duration::from_secs(15), async move {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.expect("stream event");
            if matches!(event, AgentEvent::SessionStarted { .. })
                && let Some(steer) = steer.take()
            {
                steer_tx.send(steer).await.expect("steer queued");
                if let Some((token, after)) = stop.clone() {
                    tokio::spawn(async move {
                        tokio::time::sleep(after).await;
                        token.cancel();
                    });
                }
            }
            events.push(event);
        }
        events
    })
    .await
    .expect("run finished in time")
}

fn assert_completed(events: &[AgentEvent], result: &str) {
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Done { status: DoneStatus::Completed, result: Some(text), error: None, .. }
                if text == result
        )),
        "expected Completed({result}): {events:?}"
    );
}

#[tokio::test]
async fn cc01_every_unsupported_inbound_control_gets_one_explicit_error() {
    let ledger = compat_ledger();
    let refused: Vec<&str> = ledger["transport"]["inbound_requests"]
        .as_array()
        .expect("inbound_requests")
        .iter()
        .filter(|row| row["luna"] == "unsupported_error")
        .map(|row| row["subtype"].as_str().expect("subtype"))
        .collect();
    assert!(refused.contains(&"elicitation") && refused.contains(&"luna_future_subtype"));
    let prompt = format!("scenario:cc01-unknown-controls {}", refused.join(" "));
    let (controls, _steer, _token) = controls("Allow once");
    let events = run_to_end(&harness(), request(&prompt), controls).await;
    // The fake ends Completed only if every subtype got exactly one error
    // reply with its id and reason, and the duplicate id, the id-less request,
    // the stray response, the unknown withdrawal and the heartbeat got none.
    assert_completed(&events, "controls-ok");
}

#[tokio::test]
async fn cc01_withdrawn_approval_is_never_answered_late() {
    let root = tempfile::tempdir().unwrap();
    let surfaced = root.path().join("surfaced");
    let asked = Arc::new(Mutex::new(0usize));
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let counter = Arc::clone(&asked);
    let controls = RunControls {
        // Surfacing drops a marker; the fake then withdraws the prompt, and
        // "Allow once" arrives later still: it must never reach the CLI.
        request_input: Box::new(move |questions: Vec<UserInputQuestion>| {
            *counter.lock().unwrap() += 1;
            std::fs::write(&surfaced, b"1").unwrap();
            let (tx, rx) = oneshot::channel();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let _ = tx.send(
                    questions
                        .iter()
                        .map(|q| UserInputAnswer {
                            question_id: q.id.clone(),
                            labels: vec!["Allow once".into()],
                        })
                        .collect(),
                );
            });
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
        invoke_luna_tool: None,
    };
    let mut req = request("scenario:cc01-cancel-approval");
    req.cwd = root.path().to_str().unwrap().into();
    let events = run_to_end(&harness(), req, controls).await;
    drop(steer_tx);
    assert_completed(&events, "cancel-ok");
    assert_eq!(*asked.lock().unwrap(), 1, "the approval was surfaced once");
}

#[tokio::test]
async fn cc01_explicit_steer_settles_only_on_its_exact_lifecycle_receipt() {
    let (controls, steer_tx, _token) = controls("A");
    let (steer, acceptance) = explicit_steer("queued-steer");
    let events = run_steering_after_start(
        &harness(),
        request("scenario:cc01-lifecycle-steer"),
        controls,
        steer_tx,
        steer,
        None,
    )
    .await;
    assert_eq!(
        acceptance.await.expect("acceptance"),
        SteerAcceptance::Accepted
    );
    let steered: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| matches!(event, AgentEvent::Steered { .. }).then_some(at))
        .collect();
    assert_eq!(
        steered.len(),
        1,
        "one exact receipt, duplicates and foreign uuids ignored: {events:?}"
    );
    let AgentEvent::Steered {
        user_message_id,
        assistant_message_id,
        next_assistant_message_id,
    } = &events[steered[0]]
    else {
        unreachable!()
    };
    assert_eq!(user_message_id.as_deref(), Some("queued-steer"));
    assert!(assistant_message_id.is_some() && next_assistant_message_id.is_some());
    assert_ne!(assistant_message_id, next_assistant_message_id);
    let position = |text: &str| {
        events
            .iter()
            .position(|e| matches!(e, AgentEvent::TextDelta { text: t } if t == text))
            .unwrap_or_else(|| panic!("missing delta {text}: {events:?}"))
    };
    assert!(position("first") < steered[0] && steered[0] < position("steered"));
    assert_completed(&events, "lifecycle-ok");
}

#[tokio::test]
async fn cc01_refused_steer_is_rejected_not_accepted() {
    let (controls, steer_tx, _token) = controls("A");
    let (steer, acceptance) = explicit_steer("refused-steer");
    let events = run_steering_after_start(
        &harness(),
        request("scenario:cc01-lifecycle-refused"),
        controls,
        steer_tx,
        steer,
        None,
    )
    .await;
    assert!(
        matches!(acceptance.await.expect("acceptance"), SteerAcceptance::Rejected(reason) if reason.contains("refused")),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Steered { .. })),
        "{events:?}"
    );
    assert_completed(&events, "refused-ok");
}

#[tokio::test]
async fn cc01_silent_lifecycle_falls_back_to_the_write_acknowledgement() {
    let (controls, steer_tx, _token) = controls("A");
    let (steer, acceptance) = explicit_steer("silent-steer");
    let started = std::time::Instant::now();
    let run = tokio::spawn(async move {
        run_steering_after_start(
            &harness(),
            request("scenario:cc01-lifecycle-silent"),
            controls,
            steer_tx,
            steer,
            None,
        )
        .await
    });
    assert_eq!(
        acceptance.await.expect("acceptance"),
        SteerAcceptance::Accepted
    );
    assert!(
        started.elapsed() >= Duration::from_millis(1500),
        "acceptance waited for a receipt before falling back"
    );
    let events = run.await.unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Steered { .. })),
        "{events:?}"
    );
    assert_completed(&events, "silent-ok");
}

#[tokio::test]
async fn cc01_stop_rejects_an_unreceipted_steer_the_cli_cancelled() {
    let harness = ClaudeHarness::new()
        .with_executable(fixture_path())
        .with_graces(Duration::from_secs(5), Duration::from_secs(2));
    let (controls, steer_tx, token) = controls("A");
    let (steer, acceptance) = explicit_steer("stopped-steer");
    let events = run_steering_after_start(
        &harness,
        request("scenario:cc01-stop-cancels-queued"),
        controls,
        steer_tx,
        steer,
        Some((token, Duration::from_millis(300))),
    )
    .await;
    assert!(
        matches!(acceptance.await.expect("acceptance"), SteerAcceptance::Rejected(reason) if reason.contains("stopped")),
        "the interrupt receipt's cancelled list must reject the queued steer: {events:?}"
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
}

#[tokio::test]
async fn cc01_stop_sends_a_unique_cancel_queued_interrupt_and_drains_before_signals() {
    let root = tempfile::tempdir().unwrap();
    let harness = ClaudeHarness::new()
        .with_executable(fixture_path())
        .with_graces(Duration::from_secs(5), Duration::from_secs(2));
    let (controls, _steer, token) = controls("A");
    let mut req = request("scenario:cc01-interrupt-receipt");
    req.cwd = root.path().to_str().unwrap().into();
    let mut stream = harness.run(req, controls).await.expect("run starts");
    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.expect("stream event");
            if matches!(&event, AgentEvent::TextDelta { text } if text == "working") {
                token.cancel();
            }
            events.push(event);
        }
        events
    })
    .await
    .expect("stop completed in time");
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: DoneStatus::Interrupted,
                error: None,
                ..
            })
        ),
        "{events:?}"
    );
    let request: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.path().join("interrupt-request.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(request["type"], "control_request");
    assert_eq!(request["request"]["subtype"], "interrupt");
    assert_eq!(request["request"]["cancel_queued"], true);
    assert_eq!(request["request_id"], "luna-interrupt-1");
    assert!(
        root.path().join("drained-marker").exists(),
        "stdin was closed after the interrupted result"
    );
    assert!(
        !root.path().join("sigterm-marker").exists(),
        "the CLI exited on its own before any signal"
    );
}

#[tokio::test]
async fn cc01_auth_and_limit_variants_end_with_readable_errors() {
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness(), request("scenario:cc01-auth-variants"), controls).await;
    assert!(
        events.contains(&AgentEvent::Error {
            message: "Claude needs you to verify your account — sign in again.".into()
        }),
        "{events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::Done { status: DoneStatus::Errored, error: Some(error), .. })
                if error == "The run ended with an error."
        ),
        "an unknown future result subtype still ends the turn visibly: {events:?}"
    );
}

// ── CC-02: native turn identity (E24) and event/storage decoding ─────────
// Frame shapes follow the SDK schema embedded in CLI 2.1.286; scenarios live
// in `fixtures/fake-claude.sh` (`scenario:e24-*`, `scenario:cc02-lifecycle`).
// Failure cases: `$B/evidence/cc02-events/failure-cases.md`.

fn evidence<'a>(events: &'a [AgentEvent], kind: &str) -> Vec<&'a serde_json::Value> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ProviderEvidence {
                provider,
                kind: found,
                payload,
                ..
            } if found == kind => {
                assert_eq!(*provider, HarnessId::ClaudeCode);
                Some(payload)
            }
            _ => None,
        })
        .collect()
}

fn text_at(events: &[AgentEvent], text: &str) -> usize {
    events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text: found } if found == text))
        .unwrap_or_else(|| panic!("text {text} missing: {events:?}"))
}

fn done_positions(events: &[AgentEvent]) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, AgentEvent::Done { .. }).then_some(i))
        .collect()
}

fn steer_positions(events: &[AgentEvent]) -> Vec<(usize, Option<String>)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            AgentEvent::Steered {
                user_message_id, ..
            } => Some((i, user_message_id.clone())),
            _ => None,
        })
        .collect()
}

/// Run `prompt`; after the first `Done`, send each follow-up as an ordinary
/// warm send (`AllowFollowUpTurn`) — the shape the engine's warm route uses.
async fn run_with_follow_ups(prompt: &str, follow_ups: &[(&str, Option<&str>)]) -> Vec<AgentEvent> {
    let (controls, steer, _token) = controls("A");
    let mut stream = harness()
        .run(request(prompt), controls)
        .await
        .expect("run starts");
    let mut pending: Vec<SteerMessage> = follow_ups
        .iter()
        .map(|(text, id)| SteerMessage {
            input_items: Vec::new(),
            attachments: Vec::new(),
            prompt: (*text).into(),
            message_id: id.map(Into::into),
            delivery: luna_harness::SteerDelivery::AllowFollowUpTurn,
            acceptance: None,
        })
        .collect();
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.expect("stream event");
            let done = matches!(event, AgentEvent::Done { .. });
            events.push(event);
            if done {
                for message in pending.drain(..) {
                    steer.send(message).await.expect("follow-up queued");
                }
            }
        }
        events
    })
    .await
    .expect("run finished in time")
}

fn result_records(events: &[AgentEvent]) -> Vec<&serde_json::Value> {
    evidence(events, "result")
}

/// E24 / B1, B3, B7, B9, B10: a resumed session reports a leftover background
/// task (origin only) and a peer turn (foreign echo) BEFORE it runs the human
/// prompt. Neither foreign result may complete the human turn; usage still
/// flows; the human turn's first stamped frame opens a fresh segment; a
/// replayed and a historical result change nothing.
#[tokio::test]
async fn e24_foreign_results_never_complete_the_pending_human_turn() {
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness(), request("scenario:e24-resume"), controls).await;

    let dones = done_positions(&events);
    assert_eq!(dones.len(), 1, "only the human result settles: {events:?}");
    assert!(matches!(
        &events[dones[0]],
        AgentEvent::Done { status: DoneStatus::Completed, session_id: Some(id), .. } if id == "sess-e24"
    ));
    for (input, output) in [(3, 4), (2, 2), (5, 6)] {
        assert!(
            events.contains(&AgentEvent::Usage {
                input_tokens: input,
                output_tokens: output
            }),
            "usage {input}/{output} must still flow: {events:?}"
        );
    }

    let records = result_records(&events);
    let owners: Vec<(&str, bool)> = records
        .iter()
        .map(|r| {
            (
                r["owner"].as_str().unwrap(),
                r["withheld"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        owners,
        [
            ("foreign", true),
            ("foreign", true),
            ("own", false),
            ("duplicate", true),
            ("duplicate", true)
        ],
        "{records:#?}"
    );
    assert_eq!(records[0]["stamp"]["origin"]["kind"], "task-notification");
    assert_eq!(records[1]["stamp"]["origin"]["kind"], "peer");
    let own = records[2];
    let initial_uuid = own["stamp"]["userMessageUuid"].as_str().unwrap().to_owned();
    assert_eq!(own["consumed"][0]["kind"], "initial");
    assert_eq!(own["consumed"][0]["nativeUuid"], initial_uuid.as_str());
    assert!(uuid::Uuid::parse_str(&initial_uuid).is_ok());

    let steers = steer_positions(&events);
    assert_eq!(
        steers.len(),
        1,
        "one boundary for the human turn: {events:?}"
    );
    let (steer_at, correlation) = &steers[0];
    assert_eq!(correlation.as_deref(), Some(initial_uuid.as_str()));
    assert!(text_at(&events, "FOREIGN-WAKE") < *steer_at);
    assert!(text_at(&events, "PEER-TURN") < *steer_at);
    assert!(*steer_at < text_at(&events, "HUMAN-ANSWER"));
    assert!(text_at(&events, "HUMAN-ANSWER") < dones[0]);

    let init = evidence(&events, "system/init");
    assert_eq!(init.len(), 1, "{events:?}");
    assert_eq!(init[0]["claudeCodeVersion"], "2.1.286");
    assert_eq!(init[0]["ledger"]["echoCapable"], true);
    // The human result's evidence precedes its Done in the stream.
    let own_at = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ProviderEvidence { native_id: Some(id), .. } if id == "res-human"))
        .unwrap();
    assert!(own_at < dones[0]);
}

/// E24 / B2: a warm follow-up written while Claude starts a wake turn waits in
/// the CLI queue. Its `Steered` carries the LUNA message id and lands only
/// when the CLI starts it; the wake result completes nothing.
#[tokio::test]
async fn e24_follow_up_queued_behind_a_wake_turn_settles_only_its_own_receipt() {
    let events =
        run_with_follow_ups("scenario:e24-followup", &[("again", Some("luna-msg-2"))]).await;

    let dones = done_positions(&events);
    assert_eq!(dones.len(), 2, "first answer + follow-up only: {events:?}");
    let steers = steer_positions(&events);
    assert_eq!(steers.len(), 1, "{events:?}");
    let (steer_at, correlation) = &steers[0];
    assert_eq!(correlation.as_deref(), Some("luna-msg-2"));
    assert!(dones[0] < text_at(&events, "WAKE-TURN"));
    assert!(text_at(&events, "WAKE-TURN") < *steer_at);
    assert!(*steer_at < text_at(&events, "SECOND-ANSWER"));
    assert!(text_at(&events, "SECOND-ANSWER") < dones[1]);

    let records = result_records(&events);
    assert_eq!(records.len(), 3, "{records:#?}");
    assert_eq!(records[1]["owner"], "foreign");
    assert_eq!(records[1]["withheld"], true);
    assert_eq!(records[1]["queuedTurnCount"], 1);
    assert_eq!(records[2]["owner"], "own");
    assert_eq!(records[2]["consumed"][0]["lunaId"], "luna-msg-2");
    assert_eq!(records[2]["consumed"][0]["kind"], "followUp");
}

/// E24 / B4: without echo capability the legacy contract stays: the warm
/// follow-up's boundary is the stdin write and every root result settles.
#[tokio::test]
async fn e24_older_runtime_keeps_legacy_completion() {
    let events = run_with_follow_ups("scenario:e24-legacy", &[("again", Some("luna-msg-2"))]).await;

    assert_eq!(done_positions(&events).len(), 3, "{events:?}");
    let steers = steer_positions(&events);
    assert_eq!(steers.len(), 1, "{events:?}");
    assert_eq!(steers[0].1, None, "legacy boundary keeps FIFO correlation");
    assert!(
        steers[0].0 < text_at(&events, "WAKE-TURN"),
        "boundary at write"
    );
    let records = result_records(&events);
    assert_eq!(records[1]["owner"], "foreign");
    assert_eq!(records[1]["withheld"], false);
    assert_eq!(records[1]["ledger"]["echoCapable"], false);
}

/// E24 / B6: the CLI folds the queued follow-up into its own wake turn. The
/// result echoes it, so it is OWN and settles — no wedged turn.
#[tokio::test]
async fn e24_follow_up_folded_into_a_wake_turn_settles_without_waiting() {
    let events = run_with_follow_ups("scenario:e24-fold", &[("again", Some("luna-msg-2"))]).await;

    let dones = done_positions(&events);
    assert_eq!(dones.len(), 2, "{events:?}");
    let steers = steer_positions(&events);
    assert_eq!(steers.len(), 1, "{events:?}");
    assert_eq!(steers[0].1.as_deref(), Some("luna-msg-2"));
    assert!(text_at(&events, "WAKE-PART") < steers[0].0);
    assert!(steers[0].0 < text_at(&events, "FOLDED-ANSWER"));
    let records = result_records(&events);
    assert_eq!(records[1]["owner"], "own");
    assert_eq!(records[1]["stamp"]["origin"]["kind"], "task-notification");
}

/// E24 / B5, B8: two follow-ups merged into one turn both get their exact
/// boundary (in send order) and one result consumes both.
#[tokio::test]
async fn e24_batched_follow_ups_settle_together() {
    let events = run_with_follow_ups(
        "scenario:e24-batch",
        &[("two", Some("luna-2")), ("three", Some("luna-3"))],
    )
    .await;

    assert_eq!(done_positions(&events).len(), 2, "{events:?}");
    let steers = steer_positions(&events);
    let ids: Vec<_> = steers.iter().map(|(_, id)| id.as_deref()).collect();
    assert_eq!(ids, [Some("luna-2"), Some("luna-3")], "{events:?}");
    assert!(steers[1].0 < text_at(&events, "BATCH-ANSWER"));
    let records = result_records(&events);
    let consumed: Vec<_> = records[1]["consumed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["lunaId"].as_str().unwrap())
        .collect();
    assert_eq!(consumed, ["luna-2", "luna-3"]);
}

/// CC-02 A–E: task/hook/compaction/reset/lifecycle decoding, bounded and
/// redacted unknown retention with exact counts, invalid frames, tool-result
/// bodies, and a tolerant result that still settles the turn.
#[tokio::test]
async fn cc02_lifecycle_unknown_and_tool_bodies_are_decoded_and_retained() {
    let (controls, _steer, _token) = controls("A");
    let events = run_to_end(&harness(), request("scenario:cc02-lifecycle"), controls).await;
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(!serialized.contains("SECRET-SENTINEL"), "secrets leaked");

    // Tasks (C1, C2).
    let started = evidence(&events, "system/task_started");
    assert_eq!(started[0]["family"], "task");
    assert_eq!(started[0]["phase"], "started");
    assert_eq!(started[0]["subagentType"], "general-purpose");
    assert_eq!(started[0]["promptBytes"], 11);
    assert_eq!(
        evidence(&events, "system/task_progress")[0]["usage"]["totalTokens"],
        120
    );
    let updated = evidence(&events, "system/task_updated");
    assert_eq!(updated[0]["status"], "killed");
    assert_eq!(updated[0]["error"], "stopped by user");
    assert_eq!(
        evidence(&events, "system/background_tasks_changed")[0]["tasks"][0]["taskId"],
        "agent-1"
    );
    assert_eq!(
        evidence(&events, "system/task_notification")[0]["status"],
        "stopped"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == "toolu_spawn"
                && matches!(event.as_ref(), AgentEvent::Done { status: DoneStatus::Interrupted, .. })
    )));

    // Hooks (C3): bounded output.
    assert_eq!(
        evidence(&events, "system/hook_started")[0]["hookEvent"],
        "PreToolUse"
    );
    assert_eq!(
        evidence(&events, "system/hook_progress")[0]["stdout"],
        "checking"
    );
    let response = evidence(&events, "system/hook_response")[0];
    assert_eq!(response["outcome"], "success");
    assert_eq!(response["exitCode"], 0);
    let stdout = response["stdout"].as_str().unwrap();
    assert!(
        stdout.contains("[truncated") && stdout.len() < 4300,
        "{}",
        stdout.len()
    );

    // Compaction and reset (C4, C5).
    let status = evidence(&events, "system/status");
    assert_eq!(status[0]["phase"], "started");
    assert_eq!(status[1]["phase"], "finished");
    assert_eq!(status[1]["result"], "failed");
    assert_eq!(status[1]["error"], "summary too long");
    let boundary = evidence(&events, "system/compact_boundary")[0];
    assert_eq!(boundary["family"], "compaction");
    assert_eq!(boundary["trigger"], "manual");
    assert_eq!(boundary["preTokens"], 959489);
    assert_eq!(boundary["postTokens"], 10107);
    let reset = evidence(&events, "conversation_reset")[0];
    assert_eq!(reset["family"], "conversationReset");
    assert_eq!(
        reset["newConversationId"],
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    );
    assert_eq!(reset["trigger"], "clear");
    // `command_lifecycle` is transport-owned (cc01 consumes it before
    // normalization and forwards receipts to the turn ledger); it must never
    // surface as an unknown frame.
    assert!(evidence(&events, "unknown:command_lifecycle").is_empty());
    assert!(evidence(&events, "command_lifecycle").is_empty());

    // Unprojected, unknown, redaction, bounds, flood caps (D1–D6).
    assert_eq!(
        evidence(&events, "native:system/permission_denied").len(),
        1
    );
    assert_eq!(evidence(&events, "native:prompt_suggestion").len(), 1);
    let widget = evidence(&events, "unknown:system/future_widget")[0];
    assert_eq!(widget["sample"]["api_key"], "[redacted]");
    assert_eq!(widget["sample"]["authorization"], "[redacted]");
    assert_eq!(widget["sample"]["input_tokens"], 5);
    assert_eq!(widget["sample"]["note"], "kept");
    assert_eq!(widget["sha256"].as_str().unwrap().len(), 64);
    let big = evidence(&events, "unknown:future_frame")[0];
    assert_eq!(big["truncated"], true);
    assert!(big["bytes"].as_u64().unwrap() > 20_000);
    assert!(big["sample"]["blob"].as_str().unwrap().len() < 700);
    assert_eq!(
        evidence(&events, "unknown:future_flood").len(),
        8,
        "per-kind cap"
    );
    assert_eq!(evidence(&events, "invalid:non_json").len(), 1);
    assert_eq!(evidence(&events, "invalid:assistant").len(), 1);
    assert!(evidence(&events, "system/thinking_tokens").is_empty());
    assert!(evidence(&events, "system/session_state_changed").is_empty());

    // Tool-result bodies (E1–E3), root and subagent.
    let output = |id: &str| {
        events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolResult {
                    id: found, output, ..
                } if found == id => Some(output.clone()),
                AgentEvent::Subagent { event, .. } => match event.as_ref() {
                    AgentEvent::ToolResult {
                        id: found, output, ..
                    } if found == id => Some(output.clone()),
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or_else(|| panic!("tool result {id} missing"))
    };
    // Content checks tolerate CC-05's richer `lunaToolOutput` envelope, which
    // supersedes this plain-text projection at integration; received-empty
    // versus unavailable stays exact.
    let has = |id: &str, needle: &str| output(id).is_some_and(|text| text.contains(needle));
    assert!(has("t-str", "hi"), "{:?}", output("t-str"));
    assert!(
        has("t-arr", "line one") && has("t-arr", "image/png"),
        "{:?}",
        output("t-arr")
    );
    assert!(
        output("t-empty").is_some(),
        "received-but-empty is not unavailable"
    );
    assert_eq!(output("t-none"), None, "absent content stays unavailable");
    assert!(
        has("sub-t", "(Bash completed with no output)"),
        "{:?}",
        output("sub-t")
    );

    // Tolerant result (A1–A4): malformed fields, success + is_error (529).
    let record = result_records(&events)[0];
    let invalid: Vec<_> = record["invalidFields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(invalid, ["errors", "usage"]);
    assert_eq!(record["owner"], "unattributed");
    assert_eq!(record["totalCostUsd"], 0.0123);
    assert_eq!(record["localCommand"], "compact");
    assert_eq!(record["permissionDenials"], 1);
    assert_eq!(record["apiErrorStatus"], 529);
    let counts = &record["frameCounts"];
    assert_eq!(counts["unknown:future_flood"], 12, "{counts}");
    assert_eq!(counts["system/thinking_tokens"], 2, "{counts}");
    assert_eq!(counts["system/session_state_changed"], 1, "{counts}");
    assert!(events.contains(&AgentEvent::Error {
        message: "Claude API is overloaded (529). Try again shortly.".into()
    }));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("sess-life".into()),
        })
    );
}
