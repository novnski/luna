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
    let refused: Vec<&str> = ledger["inbound_requests"]
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
    let asked = Arc::new(Mutex::new(0usize));
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let counter = Arc::clone(&asked);
    let controls = RunControls {
        // Answer "Allow once" only after the CLI has already withdrawn the
        // prompt: that answer must never reach the CLI.
        request_input: Box::new(move |questions: Vec<UserInputQuestion>| {
            *counter.lock().unwrap() += 1;
            let (tx, rx) = oneshot::channel();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
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
    let events = run_to_end(
        &harness(),
        request("scenario:cc01-cancel-approval"),
        controls,
    )
    .await;
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
