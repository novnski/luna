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

/// E25 (CC-08): a 2.1.286-shaped native agent graph through the whole driver
/// path (argv opt-in → wire → normalizer → run loop). Root spawn A runs in
/// the foreground; A spawns B (seen only in A's snapshot, no `task_started`)
/// and background E (steered by A's `SendMessage`), and launches a background
/// shell job; two background root children settle after the root's eager
/// result. Failure cases: `$B/evidence/cc08-native-agents/failure-cases.md`.
#[tokio::test]
async fn native_nested_agents_keep_ownership_model_usage_and_completion() {
    let (controls, _steer, _token) = controls("A");
    // The fake CLI exits without output unless `--forward-subagent-text` is
    // on the argv (F1), so any event at all proves the opt-in.
    let events = run_to_end(&harness(), request("scenario:native-nested"), controls).await;
    assert!(!events.is_empty(), "opt-in missing from argv");

    let tagged = |spawn: &str| -> Vec<AgentEvent> {
        events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Subagent {
                    parent_tool_use_id,
                    event,
                } if parent_tool_use_id == spawn => Some(event.as_ref().clone()),
                _ => None,
            })
            .collect()
    };
    let progress = |model: Option<&str>,
                    total_tokens: Option<u64>,
                    tool_uses: Option<u64>,
                    duration_ms: Option<u64>,
                    last_tool: Option<&str>,
                    background: Option<bool>| AgentEvent::ChildProgress {
        model: model.map(str::to_owned),
        total_tokens,
        tool_uses,
        duration_ms,
        last_tool: last_tool.map(str::to_owned),
        summary: None,
        background,
    };
    let done = |status: DoneStatus, result: Option<&str>, error: Option<&str>| AgentEvent::Done {
        status,
        result: result.map(str::to_owned),
        error: error.map(str::to_owned),
        session_id: None,
    };
    let call = |events: &[AgentEvent], id: &str| {
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { id: got, .. } if got == id))
    };

    // Root feed: only the root's own text, two root Dones (eager + wake), no
    // child content, task row, hook or unknown notification (F9/F17/F18).
    let root_text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        root_text,
        "Delegating. Two watchers launched.Background agents finished."
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Done { .. }))
            .count(),
        2,
        "{events:?}"
    );
    for spawn in ["toolu_ghost", "toolu_wf", "agent-a", "ghost", "wf-1"] {
        assert!(tagged(spawn).is_empty(), "{spawn} attached: {events:?}");
    }
    let debug = format!("{events:?}");
    for leaked in [
        "ghost finished",
        "spec workflow done",
        "late duplicate",
        "again",
    ] {
        assert!(!debug.contains(leaked), "{leaked} leaked: {debug}");
    }

    // A (foreground root child): forwarded thinking and text (F2; redacted
    // thinking never becomes text), its reported model once (F13), one
    // progress row despite two ticks (F12), its nested calls and the
    // background job's completion attributed to it (F7), then usage + Done.
    let a = tagged("toolu_A");
    assert_eq!(
        a.first(),
        Some(&AgentEvent::UserMessage {
            text: "Map the parser module and delegate the lexer.".into()
        })
    );
    assert!(a.contains(&AgentEvent::ReasoningDelta {
        text: "The lexer is separable; delegate it.\n\n".into()
    }));
    assert!(a.contains(&AgentEvent::TextDelta {
        text: "Splitting the work.\n\n".into()
    }));
    assert!(!format!("{a:?}").contains("cmVkYWN0ZWQ"));
    assert_eq!(
        a.iter()
            .filter(|e| matches!(e, AgentEvent::ChildProgress { model: Some(_), .. }))
            .collect::<Vec<_>>(),
        [&progress(
            Some("claude-haiku-4-5-20251001"),
            None,
            None,
            None,
            None,
            None
        )]
    );
    assert_eq!(
        a.iter()
            .filter(|e| matches!(
                e,
                AgentEvent::ChildProgress {
                    last_tool: Some(_),
                    ..
                }
            ))
            .collect::<Vec<_>>(),
        [&progress(
            None,
            Some(1200),
            Some(1),
            Some(4000),
            Some("Agent"),
            None
        )],
        "flood guard: the second tick changes no material field"
    );
    for nested in ["toolu_B", "toolu_E", "toolu_A_bg", "toolu_A_send"] {
        assert!(call(&a, nested), "{nested} not in A's transcript: {a:?}");
    }
    assert!(a.iter().any(|e| matches!(e,
        AgentEvent::ToolResult { id, is_error: false, output: Some(output), .. }
            if id == "toolu_A_bg"
                && output.starts_with("Background job completed: Background command \"Run tests\" completed (exit code 0)")
                && output.contains("Output file: /tmp/claude/tasks/bash-a.output")
    )), "{a:?}");
    assert_eq!(
        &a[a.len() - 2..],
        [
            progress(None, Some(2400), Some(3), Some(9000), None, None),
            done(DoneStatus::Completed, Some("Parser mapped"), None),
        ]
    );

    // B: nested, seen only in A's snapshot, no task_started (F3): opening
    // prompt (F4), its OWN model (F13), content, usage and one Done (F10:
    // the late duplicate after the wake result is ignored).
    let b = tagged("toolu_B");
    assert_eq!(
        b.first(),
        Some(&AgentEvent::UserMessage {
            text: "Summarize lexer.rs states.".into()
        })
    );
    assert!(b.contains(&progress(
        Some("claude-sonnet-5"),
        None,
        None,
        None,
        None,
        None
    )));
    assert!(b.contains(&AgentEvent::TextDelta {
        text: "Lexer has three states.\n\n".into()
    }));
    assert!(call(&b, "toolu_B_read"));
    assert_eq!(
        &b[b.len() - 2..],
        [
            progress(None, Some(800), Some(1), Some(2000), None, None),
            done(DoneStatus::Completed, Some("Lexer has three states"), None),
        ]
    );

    // E: nested background child, steered by A's SendMessage (F5).
    let e = tagged("toolu_E");
    assert_eq!(
        e[..3],
        [
            AgentEvent::UserMessage {
                text: "List the token kinds.".into()
            },
            progress(None, None, None, None, None, Some(true)),
            AgentEvent::UserMessage {
                text: "Include keywords too.".into()
            },
        ]
    );
    assert_eq!(
        e.last(),
        Some(&done(
            DoneStatus::Completed,
            Some("Listed token kinds"),
            None
        ))
    );

    // C/D: background root children settling after the root's eager result
    // (F11) with observed cancellation and failure (F15). The summary that
    // only repeats the description is not an outcome.
    let c = tagged("toolu_C");
    assert_eq!(
        c.last(),
        Some(&done(
            DoneStatus::Interrupted,
            None,
            Some("Stopped: the Claude worker restarted")
        ))
    );
    let d = tagged("toolu_D");
    assert_eq!(
        d.last(),
        Some(&done(
            DoneStatus::Errored,
            None,
            Some("Docs site unreachable")
        ))
    );
    let eager = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .unwrap();
    let settled = |spawn: &str| {
        events.iter().position(|e| matches!(e,
            AgentEvent::Subagent { parent_tool_use_id, event }
                if parent_tool_use_id == spawn && matches!(event.as_ref(), AgentEvent::Done { .. })
        ))
    };
    assert!(settled("toolu_C").unwrap() > eager && settled("toolu_D").unwrap() > eager);

    // Exactly one Done per spawn; no Done for a non-spawn.
    let mut dones: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Subagent {
                parent_tool_use_id,
                event,
            } if matches!(event.as_ref(), AgentEvent::Done { .. }) => {
                Some(parent_tool_use_id.as_str())
            }
            _ => None,
        })
        .collect();
    dones.sort_unstable();
    assert_eq!(
        dones,
        ["toolu_A", "toolu_B", "toolu_C", "toolu_D", "toolu_E"]
    );
}

/// CC-08 task cancellation: `stop_native_child` writes the 2.1.286
/// `stop_task` control request only for a running native child of the live
/// run; everything else is refused locally (the fake CLI fails on any other
/// stdin line). An acknowledgement is only a request receipt (2.1.286 also
/// acknowledges unknown task ids — init probe): the child settles from its
/// own `task_updated`/`task_notification` (F15/F16).
#[tokio::test]
async fn native_child_stop_is_authority_checked_and_settles_from_task_events() {
    use luna_harness::NativeTaskStop;

    let harness = harness();
    let (controls, steer, _token) = controls("A");
    let mut stream = harness
        .run(request("scenario:native-stop"), controls)
        .await
        .expect("run starts");
    let mut events = Vec::new();
    let child_text = |event: &AgentEvent| {
        matches!(event, AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == "toolu_S" && matches!(event.as_ref(), AgentEvent::TextDelta { .. }))
    };
    assert!(next_until(&mut stream, &mut events, child_text).await);
    let session = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SessionStarted { session_id, .. } => Some(session_id.clone()),
            _ => None,
        })
        .expect("session started");

    // Refused locally: unknown spawn, another session.
    assert!(matches!(
        harness
            .stop_native_child(&session, "toolu_unknown")
            .await
            .unwrap(),
        NativeTaskStop::Rejected(_)
    ));
    assert!(matches!(
        harness
            .stop_native_child("sess-other", "toolu_S")
            .await
            .unwrap(),
        NativeTaskStop::Rejected(_)
    ));
    // The real stop: acknowledged, but the child is not settled by that.
    assert_eq!(
        harness
            .stop_native_child(&session, "toolu_S")
            .await
            .unwrap(),
        NativeTaskStop::Requested
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Subagent { event, .. }
        if matches!(event.as_ref(), AgentEvent::Done { .. })))
    );
    let child_done = |event: &AgentEvent| {
        matches!(event, AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == "toolu_S" && matches!(event.as_ref(), AgentEvent::Done { .. }))
    };
    assert!(next_until(&mut stream, &mut events, child_done).await);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Subagent {
            parent_tool_use_id: "toolu_S".into(),
            event: Box::new(AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: None,
                session_id: None,
            }),
        })
    );
    // Settled: a second stop is refused without reaching the CLI.
    assert!(matches!(
        harness
            .stop_native_child(&session, "toolu_S")
            .await
            .unwrap(),
        NativeTaskStop::Rejected(_)
    ));
    // Close the mailbox (stdin EOF); the rest of the run plays out.
    drop(steer);
    assert!(!next_until(&mut stream, &mut events, |_| false).await);
    let root: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Done { status, error, .. } => Some((*status, error.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        root,
        [(DoneStatus::Completed, None), (DoneStatus::Completed, None)],
        "a refused stop leaked to stdin, or the child's stop ended the root: {events:?}"
    );
    // The run is gone: nothing is addressable any more.
    assert!(matches!(
        harness
            .stop_native_child(&session, "toolu_S")
            .await
            .unwrap(),
        NativeTaskStop::Rejected(_)
    ));
}

/// Consume `stream` into `events` until `stop` matches (true) or it ends.
async fn next_until(
    stream: &mut futures::stream::BoxStream<'static, Result<AgentEvent, HarnessError>>,
    events: &mut Vec<AgentEvent>,
    stop: impl Fn(&AgentEvent) -> bool,
) -> bool {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = stream.next().await {
            let event = event.expect("stream event");
            let found = stop(&event);
            events.push(event);
            if found {
                return true;
            }
        }
        false
    })
    .await
    .expect("stream progressed in time")
}
