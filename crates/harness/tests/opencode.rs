//! Native opencode driver against a scripted in-test HTTP/SSE server.
//!
//! The fake speaks just enough of the OpenCode 2 surface (`/api/session`,
//! `/api/session/{id}/prompt`, `/api/session/{id}/interrupt`,
//! `/api/session/{id}/command`, `/api/model`, `/api/command`, `/api/event`)
//! and hands the TEST full control of bus timing via `emit()` — the
//! premature-done class is exactly about what happens between events, so the
//! fixtures must own the clock.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use luna_harness::{
    CancellationToken, Harness, HarnessError, OpencodeHarness, RunControls, SteerMessage,
};
use luna_proto::{
    AgentEvent, DoneStatus, ReasoningLevel, RunRequest, SandboxLevel, UserInputAnswer,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot};

// ---------------------------------------------------------------------------
// Fake server
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FakeOpencode {
    base: String,
    events: broadcast::Sender<(u64, String)>,
    /// Every emitted frame, sequence-stamped — replayed to late SSE
    /// subscribers so tests may emit before the driver's stream connects.
    backlog: Arc<Mutex<Vec<(u64, String)>>>,
    /// Recorded `(path, body)` of every POST and PATCH.
    posts: Arc<Mutex<Vec<(String, Value)>>>,
    /// `GET /api/model` entries.
    models: Arc<Mutex<Value>>,
    /// Whether an SSE subscriber existed when the FIRST prompt landed (the
    /// no-replay bus makes prompting before the subscription a real
    /// event-loss race — observed live on fast-failing turns).
    first_prompt_had_subscriber: Arc<Mutex<Option<bool>>>,
    /// `POST /api/session` answers 500 while this is non-zero.
    fail_session_creates: Arc<Mutex<u32>>,
}

impl FakeOpencode {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (events, _) = broadcast::channel::<(u64, String)>(256);
        let fake = Self {
            base,
            events: events.clone(),
            backlog: Arc::new(Mutex::new(Vec::new())),
            posts: Arc::new(Mutex::new(Vec::new())),
            models: Arc::new(Mutex::new(json!([]))),
            first_prompt_had_subscriber: Arc::new(Mutex::new(None)),
            fail_session_creates: Arc::new(Mutex::new(0)),
        };
        let accept = fake.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let fake = accept.clone();
                tokio::spawn(async move { fake.serve(stream).await });
            }
        });
        fake
    }

    /// Push one raw 2.x bus frame (`{id, type, data}`).
    fn emit(&self, kind: &str, data: Value) {
        let mut backlog = self.backlog.lock().unwrap();
        let seq = backlog.len() as u64;
        let framed = format!(
            "data: {}\n\n",
            json!({ "id": format!("evt_{seq}"), "created": seq, "type": kind, "data": data })
        );
        backlog.push((seq, framed.clone()));
        let _ = self.events.send((seq, framed));
    }

    fn set_models(&self, models: Value) {
        *self.models.lock().unwrap() = models;
    }

    fn posts_to(&self, path: &str) -> Vec<Value> {
        self.posts
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, b)| b.clone())
            .collect()
    }

    async fn serve(self, mut stream: tokio::net::TcpStream) {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        // Keep-alive requests re-enter here.
        loop {
            let header_end = loop {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            };
            let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let mut lines = head.lines();
            let start = lines.next().unwrap_or_default().to_owned();
            let content_length = lines
                .filter_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                })
                .next()
                .unwrap_or(0);
            while buf.len() < header_end + content_length {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let body: Value = serde_json::from_slice(&buf[header_end..header_end + content_length])
                .unwrap_or(Value::Null);
            buf.drain(..header_end + content_length);

            let mut parts = start.split_whitespace();
            let method = parts.next().unwrap_or_default().to_owned();
            let target = parts.next().unwrap_or_default().to_owned();
            let path = target.split('?').next().unwrap_or_default().to_owned();

            if method == "GET" && path == "/api/event" {
                // Subscribe FIRST, then snapshot the backlog: frames landing
                // in between arrive on both channels and dedupe by sequence.
                let mut rx = self.events.subscribe();
                let replay = self.backlog.lock().unwrap().clone();
                let mut next_seq = replay.len() as u64;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                          cache-control: no-cache\r\nconnection: close\r\n\r\n",
                    )
                    .await;
                let _ = stream
                    .write_all(b"data: {\"id\":\"evt_connected\",\"type\":\"server.connected\",\"data\":{}}\n\n")
                    .await;
                for (_, frame) in &replay {
                    if stream.write_all(frame.as_bytes()).await.is_err() {
                        return;
                    }
                }
                let _ = stream.flush().await;
                while let Ok((seq, frame)) = rx.recv().await {
                    if seq < next_seq {
                        continue;
                    }
                    next_seq = seq + 1;
                    if stream.write_all(frame.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = stream.flush().await;
                }
                return;
            }

            if method == "POST" || method == "PATCH" {
                if path.ends_with("/prompt") {
                    let mut first = self.first_prompt_had_subscriber.lock().unwrap();
                    if first.is_none() {
                        *first = Some(self.events.receiver_count() > 0);
                    }
                }
                self.posts.lock().unwrap().push((path.clone(), body));
            }
            let (status, payload) = self.route(&method, &path);
            let body = match payload {
                Some(payload) => payload.to_string(),
                None => String::new(),
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\n\r\n{body}",
                body.len()
            );
            if stream.write_all(resp.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    fn route(&self, method: &str, path: &str) -> (&'static str, Option<Value>) {
        match (method, path) {
            ("GET", "/api/info") => (
                "200 OK",
                Some(
                    json!({ "version": "2.0.22", "pid": 1, "urls": [], "paths": { "tmp": "/tmp" } }),
                ),
            ),
            ("GET", "/api/model") => (
                "200 OK",
                Some(json!({ "data": self.models.lock().unwrap().clone() })),
            ),
            ("GET", "/api/command") => (
                "200 OK",
                Some(json!({ "data": [{ "name": "init", "description": "Create AGENTS.md" }] })),
            ),
            ("POST", "/api/session") => {
                let mut fails = self.fail_session_creates.lock().unwrap();
                if *fails > 0 {
                    *fails -= 1;
                    (
                        "500 Internal Server Error",
                        Some(
                            json!({ "_tag": "UnknownError", "message": "Unexpected server error", "ref": "err_test" }),
                        ),
                    )
                } else {
                    ("200 OK", Some(json!({ "data": { "id": "ses_test" } })))
                }
            }
            ("GET", "/api/session/active") => ("200 OK", Some(json!({ "data": {} }))),
            ("GET", "/api/session/ses_resume") => {
                ("200 OK", Some(json!({ "data": { "id": "ses_resume" } })))
            }
            ("PATCH", "/api/session/ses_resume") => ("204 No Content", None),
            ("GET", "/api/session/ses_unavailable") => (
                "503 Service Unavailable",
                Some(
                    json!({ "_tag": "ServiceUnavailableError", "message": "temporarily unavailable" }),
                ),
            ),
            ("GET", p) if p.starts_with("/api/session/") => (
                "404 Not Found",
                Some(json!({ "_tag": "SessionNotFoundError", "message": "Session not found" })),
            ),
            ("POST", p) if p.ends_with("/model") => ("204 No Content", None),
            ("POST", p) if p.ends_with("/prompt") => (
                "200 OK",
                Some(
                    json!({ "data": { "id": "msg_inbox", "sessionID": "ses_test", "type": "user", "delivery": "steer" } }),
                ),
            ),
            ("POST", "/api/session/ses_test/command") => ("204 No Content", None),
            ("POST", p) if p.ends_with("/interrupt") => {
                ("200 OK", Some(json!({ "interrupted": true })))
            }
            ("POST", p) if p.contains("/permission/") => ("204 No Content", None),
            _ => ("404 Not Found", Some(json!({ "missing": path }))),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::DangerFullAccess,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    }
}

#[allow(clippy::type_complexity)]
fn controls() -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (steer_tx, steering) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: q.options.first().cloned().into_iter().collect(),
                })
                .collect();
            let _ = tx.send(answers);
            rx
        }),
        steering,
        interrupt: token.clone(),
        invoke_luna_tool: None,
    };
    (controls, steer_tx, token)
}

fn harness(fake: &FakeOpencode) -> OpencodeHarness {
    OpencodeHarness::new().with_base_url(fake.base.clone())
}

/// Emit the standard opening frames of an assistant step.
fn assistant_message(fake: &FakeOpencode, session: &str, message: &str) {
    fake.emit("session.execution.started", json!({ "sessionID": session }));
    fake.emit(
        "session.step.started",
        json!({ "sessionID": session, "assistantMessageID": message }),
    );
}

/// A complete text item (snapshot) in `message`.
fn text(fake: &FakeOpencode, session: &str, message: &str, ordinal: u64, body: &str) {
    fake.emit(
        "session.text.ended",
        json!({ "sessionID": session, "assistantMessageID": message, "ordinal": ordinal, "text": body }),
    );
}

fn idle(fake: &FakeOpencode, session: &str) {
    fake.emit(
        "session.execution.succeeded",
        json!({ "sessionID": session }),
    );
}

/// The next transcript-relevant event. Host-only receipt evidence
/// (`ProviderEvidence`, journal-only) is skipped.
async fn next_event(
    stream: &mut (impl futures::Stream<Item = Result<AgentEvent, HarnessError>> + Unpin),
) -> AgentEvent {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("event within budget")
            .expect("stream open")
            .expect("ok event");
        if !matches!(event, AgentEvent::ProviderEvidence { .. }) {
            return event;
        }
    }
}

/// `Err` (elapsed) when nothing but host-only receipt evidence arrives
/// within `ms`.
async fn quiet_for(
    stream: &mut (impl futures::Stream<Item = Result<AgentEvent, HarnessError>> + Unpin),
    ms: u64,
) -> Result<Option<Result<AgentEvent, HarnessError>>, tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_millis(ms), async {
        loop {
            match stream.next().await {
                Some(Ok(AgentEvent::ProviderEvidence { .. })) => continue,
                other => return other,
            }
        }
    })
    .await
}

/// Poll until `path` has received `n` POSTs; returns their bodies.
async fn wait_posts(fake: &FakeOpencode, path: &str, n: usize) -> Vec<Value> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let posts = fake.posts_to(path);
            if posts.len() >= n {
                return posts;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{path} never saw {n} posts"))
}

/// Drain until a Done arrives; returns everything seen (Done last).
async fn drain_to_done(
    stream: &mut (impl futures::Stream<Item = Result<AgentEvent, HarnessError>> + Unpin),
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    loop {
        let ev = next_event(stream).await;
        let done = matches!(&ev, AgentEvent::Done { .. });
        events.push(ev);
        if done {
            return events;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn thinking_streams_and_the_turn_settles_only_on_idle() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");

    let started = next_event(&mut stream).await;
    assert!(matches!(
        &started,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "ses_test"
    ));
    // AvailableCommands from /api/command.
    let commands = next_event(&mut stream).await;
    assert!(matches!(
        &commands,
        AgentEvent::AvailableCommands { commands } if commands.len() == 1
    ));

    assistant_message(&fake, "ses_test", "msg_1");
    // Reasoning item: open snapshot → delta → closing snapshot (full text,
    // must dedup to nothing).
    fake.emit(
        "session.reasoning.started",
        json!({ "sessionID": "ses_test", "assistantMessageID": "msg_1", "ordinal": 0 }),
    );
    fake.emit(
        "session.reasoning.delta",
        json!({ "sessionID": "ses_test", "assistantMessageID": "msg_1", "ordinal": 0, "delta": "let me think" }),
    );
    let thinking = next_event(&mut stream).await;
    assert!(matches!(
        &thinking,
        AgentEvent::ReasoningDelta { text } if text == "let me think"
    ));
    fake.emit(
        "session.reasoning.ended",
        json!({ "sessionID": "ses_test", "assistantMessageID": "msg_1", "ordinal": 0, "text": "let me think" }),
    );

    // Text streams; the turn must NOT settle during the quiet gap after it —
    // only the execution terminal ends the turn (the premature-done
    // regression).
    text(&fake, "ses_test", "msg_1", 0, "Hello");
    let delta = next_event(&mut stream).await;
    assert!(matches!(&delta, AgentEvent::TextDelta { text } if text == "Hello"));
    let quiet = quiet_for(&mut stream, 600).await;
    assert!(quiet.is_err(), "nothing may settle a quiet-but-live turn");

    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.first(),
        Some(AgentEvent::AssistantMessageCompleted { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            session_id: Some(sid),
            ..
        }) if sid == "ses_test"
    ));
}

#[tokio::test]
async fn foreign_session_idle_never_settles_our_turn() {
    // The exact bug in opencode's own ACP layer: the first idle observed —
    // any session's — settled the turn.
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await; // SessionStarted
    let _ = next_event(&mut stream).await; // AvailableCommands

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_OTHER");
    let quiet = quiet_for(&mut stream, 600).await;
    assert!(quiet.is_err(), "a foreign session's idle settled our turn");

    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn model_and_advertised_variant_ride_the_session() {
    let fake = FakeOpencode::start().await;
    fake.set_models(json!([{
        "providerID": "anthropic",
        "id": "opus",
        "name": "Opus",
        "variants": [{ "id": "high" }, { "id": "max" }],
        "enabled": true,
    }]));
    let (controls, _steer, _token) = controls();
    let mut req = request("hi");
    req.model = Some("anthropic/opus".into());
    req.reasoning = Some(ReasoningLevel::XHigh);
    let mut stream = harness(&fake).run(req, controls).await.expect("run starts");
    let _ = next_event(&mut stream).await;
    let _ = next_event(&mut stream).await;

    let selected = wait_posts(&fake, "/api/session/ses_test/model", 1).await;
    // XHigh isn't advertised: the ladder clamps to "high".
    assert_eq!(
        selected[0],
        json!({ "model": { "providerID": "anthropic", "id": "opus", "variant": "high" } })
    );
    let prompts = wait_posts(&fake, "/api/session/ses_test/prompt", 1).await;
    // A caller id (`msg_…`, the inbox id) and an explicit turn-boundary
    // delivery ride every prompt (OC-07).
    assert_eq!(prompts[0]["text"], "hi");
    assert_eq!(prompts[0]["files"], json!([]));
    assert_eq!(prompts[0]["delivery"], "queue");
    assert!(
        prompts[0]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("msg_")),
        "{:?}",
        prompts[0]
    );

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn steer_queues_mid_turn_and_delivers_at_idle() {
    let fake = FakeOpencode::start().await;
    let (controls, steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await;
    let _ = next_event(&mut stream).await;

    assistant_message(&fake, "ses_test", "msg_1");
    text(&fake, "ses_test", "msg_1", 0, "working");
    let _ = next_event(&mut stream).await; // TextDelta

    steer
        .send(SteerMessage {
            input_items: Vec::new(),
            attachments: Vec::new(),
            prompt: "also do this".into(),
            message_id: None,
            delivery: luna_harness::SteerDelivery::AllowFollowUpTurn,
            acceptance: None,
            goal: None,
        })
        .await
        .unwrap();
    // Give the steer time to land in the queue, then end turn 1.
    tokio::time::sleep(Duration::from_millis(100)).await;
    idle(&fake, "ses_test");

    let ev = next_event(&mut stream).await;
    assert!(
        matches!(&ev, AgentEvent::Steered { .. }),
        "queued steer must continue the run at the turn boundary, got {ev:?}"
    );
    // The steer went out as a second prompt on the SAME session.
    let prompts = wait_posts(&fake, "/api/session/ses_test/prompt", 2).await;
    assert_eq!(prompts[1]["text"], "also do this");

    // Turn 2 settles normally.
    assistant_message(&fake, "ses_test", "msg_2");
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn interrupt_aborts_and_settles_interrupted() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await;
    let _ = next_event(&mut stream).await;

    assistant_message(&fake, "ses_test", "msg_1");
    token.cancel();
    let interrupts = wait_posts(&fake, "/api/session/ses_test/interrupt", 1).await;
    assert_eq!(interrupts[0], Value::Null, "interrupt carries no body");
    fake.emit(
        "session.execution.interrupted",
        json!({ "sessionID": "ses_test", "reason": "user" }),
    );
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Interrupted,
            ..
        })
    ));
}

#[tokio::test]
async fn execution_failure_with_no_content_settles_errored() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await;
    let _ = next_event(&mut stream).await;

    fake.emit(
        "session.execution.started",
        json!({ "sessionID": "ses_test" }),
    );
    fake.emit(
        "session.execution.failed",
        json!({ "sessionID": "ses_test", "error": {
            "type": "provider.auth",
            "message": "no credentials for anthropic",
        }}),
    );
    let ev = next_event(&mut stream).await;
    assert!(matches!(
        &ev,
        AgentEvent::Error { message } if message.contains("no credentials")
    ));
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(e),
            ..
        }) if e.contains("no credentials")
    ));
}

#[tokio::test]
async fn resume_reuses_the_durable_session() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut req = request("continue");
    req.resume = Some("ses_resume".into());
    let mut stream = harness(&fake).run(req, controls).await.expect("run starts");
    let started = next_event(&mut stream).await;
    assert!(matches!(
        &started,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "ses_resume"
    ));
    let _ = next_event(&mut stream).await;
    let patches = wait_posts(&fake, "/api/session/ses_resume", 1).await;
    assert!(patches[0]["permissions"].is_array(), "{:?}", patches[0]);
    wait_posts(&fake, "/api/session/ses_resume/prompt", 1).await;

    assistant_message(&fake, "ses_resume", "msg_1");
    idle(&fake, "ses_resume");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn failed_resume_never_silently_starts_a_fresh_session() {
    // Both an unavailable server and a missing durable session must leave the
    // user's history binding intact. Neither authorizes a new conversation.
    for session in ["ses_missing", "ses_unavailable"] {
        let fake = FakeOpencode::start().await;
        let (controls, _steer, _token) = controls();
        let mut req = request("continue existing work");
        req.resume = Some(session.into());
        let mut stream = harness(&fake)
            .run(req, controls)
            .await
            .expect("stream starts");
        let first = next_event(&mut stream).await;
        assert!(
            !matches!(&first, AgentEvent::SessionStarted { .. }),
            "failed resume must not silently publish a new binding: {first:?}"
        );
        let mut events = vec![first];
        if !matches!(events.last(), Some(AgentEvent::Done { .. })) {
            events.extend(drain_to_done(&mut stream).await);
        }
        assert!(
            matches!(
                events.last(),
                Some(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    ..
                })
            ),
            "{events:?}"
        );
        assert!(
            fake.posts_to("/api/session").is_empty(),
            "failed resume must not create a session"
        );
        assert!(fake.posts_to("/api/session/ses_test/prompt").is_empty());
    }
}

#[tokio::test]
async fn slash_command_routes_through_the_command_endpoint() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("/init the repo"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await;
    let _ = next_event(&mut stream).await;

    let commands = wait_posts(&fake, "/api/session/ses_test/command", 1).await;
    assert_eq!(commands[0], json!({ "name": "init", "text": "the repo" }));
    assert!(fake.posts_to("/api/session/ses_test/prompt").is_empty());

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn first_prompt_waits_for_the_live_event_subscription() {
    // The bus has no replay: a fast-failing turn (bad model id) runs its
    // whole lifecycle within ~200ms of the prompt. Prompting before the SSE
    // stream exists loses the whole turn — the driver must gate the first
    // prompt on the connection.
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("hi"), controls)
        .await
        .expect("run starts");
    let _ = next_event(&mut stream).await;
    let _ = next_event(&mut stream).await;
    wait_posts(&fake, "/api/session/ses_test/prompt", 1).await;
    assert_eq!(
        *fake.first_prompt_had_subscriber.lock().unwrap(),
        Some(true),
        "prompt must not be posted before the /api/event subscription exists"
    );

    // And the fast-failure lifecycle settles promptly (all frames in one
    // burst), not via the stall watchdog.
    fake.emit(
        "session.execution.started",
        json!({ "sessionID": "ses_test" }),
    );
    fake.emit(
        "session.execution.failed",
        json!({ "sessionID": "ses_test", "error": {
            "type": "unknown",
            "message": "Model not found: opencode/gone-model",
        }}),
    );
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(e),
            ..
        }) if e.contains("Model not found")
    ));
}

#[tokio::test]
async fn models_discover_from_the_model_list() {
    let fake = FakeOpencode::start().await;
    fake.set_models(json!([
        { "providerID": "opencode", "id": "big-pickle", "name": "Big Pickle", "enabled": true },
        { "providerID": "catalog-only", "id": "locked", "name": "Locked", "enabled": false },
    ]));
    let harness = harness(&fake);
    let models = harness.models().await.expect("models");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "opencode/big-pickle");
    // Commands were primed off the same probe.
    let commands = harness.commands().await.expect("commands");
    assert_eq!(commands[0].name, "init");
}

#[tokio::test]
async fn models_refresh_large_catalogs_and_never_return_a_stale_one() {
    let fake = FakeOpencode::start().await;
    let harness = harness(&fake);
    let models: Vec<Value> = (0..512)
        .map(|i| json!({ "providerID": "provider", "id": format!("model-{i}"), "name": "x".repeat(2048) }))
        .collect();
    fake.set_models(Value::Array(models));
    assert_eq!(harness.models().await.unwrap().len(), 512);

    // An empty catalog (after the boot-race polling) is an error, not the
    // old account's list.
    fake.set_models(json!([]));
    assert!(
        harness.models().await.is_err(),
        "must not return the old account's catalog"
    );

    fake.set_models(json!([{ "providerID": "new-account", "id": "fresh", "name": "Fresh" }]));
    let refreshed = harness.models().await.unwrap();
    assert_eq!(refreshed.len(), 1);
    assert_eq!(refreshed[0].id, "new-account/fresh");
}

#[tokio::test]
async fn session_create_failure_is_not_retried() {
    // A blind retry after a 5xx could mint a duplicate session (OCF-74).
    let fake = FakeOpencode::start().await;
    *fake.fail_session_creates.lock().unwrap() = 10;
    let (controls, _, _) = controls();
    let mut stream = harness(&fake).run(request("hi"), controls).await.unwrap();
    let events = drain_to_done(&mut stream).await;
    assert_eq!(fake.posts_to("/api/session").len(), 1);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionStarted { .. }))
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(e),
            ..
        }) if e.contains("UnknownError: Unexpected server error")
    ));
}
