use super::history::*;
use super::*;

fn ignore_fork_progress(_: ProviderForkProgress) -> Result<(), HarnessError> {
    Ok(())
}

/// Two settled v2 turns: model switch, user, assistant, idle; user,
/// assistant, idle.
fn two_turns() -> Value {
    json!([
        {"id":"msg_m0","type":"model-switched","time":{"created":1},"model":{"id":"muse","providerID":"opencode"}},
        {"id":"msg_u1","type":"user","time":{"created":2},"text":"one"},
        {"id":"msg_a1","type":"assistant","time":{"created":3,"completed":4},"agent":"build","content":[{"type":"text","text":"answer"}]},
        {"id":"msg_i1","type":"idle","time":{"created":5},"outcome":"succeeded"},
        {"id":"msg_u2","type":"user","time":{"created":6},"text":"later"},
        {"id":"msg_a2","type":"assistant","time":{"created":7,"completed":8},"agent":"build","content":[{"type":"text","text":"second"}]},
        {"id":"msg_i2","type":"idle","time":{"created":9},"outcome":"succeeded"}
    ])
}

/// `items` with every id replaced the way a fork regenerates them.
fn forked_copy(items: &[Value]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|item| {
                let mut item = item.clone();
                let id = item["id"].as_str().unwrap().replace("msg_", "msg_fork_");
                item["id"] = json!(id);
                item
            })
            .collect(),
    )
}

async fn fork_wire(
    fork_messages: Value,
) -> (
    String,
    Arc<std::sync::Mutex<Vec<(String, String, Value)>>>,
    tokio::task::JoinHandle<()>,
) {
    fork_wire_with(two_turns(), fork_messages, false).await
}

/// A 2.x HTTP fake for the fork path. Records `(method, path, body)`;
/// message pages are served whole (no cursor), and the fork answer echoes
/// the boundary it was asked for.
async fn fork_wire_with(
    source_messages: Value,
    fork_messages: Value,
    delay_fork_readback: bool,
) -> (
    String,
    Arc<std::sync::Mutex<Vec<(String, String, Value)>>>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            let source_messages = source_messages.clone();
            let fork_messages = fork_messages.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                let header_end = loop {
                    let count = socket.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let header = String::from_utf8_lossy(&request[..header_end]);
                let mut request_line = header.lines().next().unwrap().split_whitespace();
                let method = request_line.next().unwrap().to_owned();
                let target = request_line.next().unwrap().to_owned();
                let path = target.split('?').next().unwrap().to_owned();
                let length = header
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + length {
                    let count = socket.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..count]);
                }
                let body = serde_json::from_slice(&request[header_end..header_end + length])
                    .unwrap_or(Value::Null);
                recorded
                    .lock()
                    .unwrap()
                    .push((method.clone(), path.clone(), body.clone()));
                if delay_fork_readback && path == "/api/session/forked" && method == "GET" {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                let response = match path.as_str() {
                    "/api/session/source" => json!({"data":{"id":"source"}}),
                    "/api/session/source/message" => {
                        json!({"data": source_messages, "cursor": {"next": null}})
                    }
                    "/api/session/source/fork" => {
                        let boundary = match body.get("before") {
                            Some(before) => json!({"type":"before","messageID":before}),
                            None => json!({"type":"through","messageID":"msg_last"}),
                        };
                        json!({"data":{"id":"forked","fork":{"sessionID":"source","boundary":boundary}}})
                    }
                    "/api/session/forked" => json!({"data":{"id":"forked"}}),
                    "/api/session/forked/message" => {
                        json!({"data": fork_messages, "cursor": {"next": null}})
                    }
                    _ => json!({}),
                };
                let encoded = response.to_string();
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{encoded}",
                            encoded.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });
        }
    });
    (base, requests, server)
}

#[tokio::test]
async fn native_fork_adapter_remaps_regenerated_ids_and_rejects_leakage() {
    let source = two_turns();
    let first_turn = &source.as_array().unwrap()[..4];
    let good = forked_copy(first_turn);
    let (base, requests, server_task) = fork_wire(good).await;
    let result = Server::attached(base)
        .fork_session_staged(
            "source",
            None,
            &ProviderForkBoundary::ThroughCheckpoint {
                checkpoint_id: "msg_u1".into(),
            },
            &ignore_fork_progress,
        )
        .await
        .unwrap();
    assert_eq!(result.provider_session_id, "forked");
    assert_eq!(
        result.checkpoint_map.get("msg_u1").map(String::as_str),
        Some("msg_fork_u1")
    );
    assert!(requests.lock().unwrap().iter().any(|(method, path, body)| {
        method == "POST"
            && path == "/api/session/source/fork"
            && body == &json!({"before":"msg_u2"})
    }));
    server_task.abort();

    // A fork holding more than the kept prefix fails, and the unverified
    // fork is deleted rather than left orphaned.
    let leaked = forked_copy(&source.as_array().unwrap()[..5]);
    let (base, requests, server_task) = fork_wire(leaked).await;
    let error = Server::attached(base)
        .fork_session_staged(
            "source",
            None,
            &ProviderForkBoundary::ThroughCheckpoint {
                checkpoint_id: "msg_u1".into(),
            },
            &ignore_fork_progress,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("history length mismatch"));
    assert!(error.to_string().contains("was deleted"));
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .any(|(method, path, _)| method == "DELETE" && path == "/api/session/forked")
    );
    server_task.abort();
}

#[tokio::test]
async fn native_fork_progress_distinguishes_preflight_acknowledgement_and_readback_timeout() {
    let mut mismatched = forked_copy(&two_turns().as_array().unwrap()[..4]);
    mismatched[1]["text"] = json!("changed");
    let (base, requests, server_task) = fork_wire(mismatched).await;
    let progress = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = progress.clone();
    let report = move |event| {
        observed.lock().unwrap().push(event);
        Ok(())
    };
    let error = Server::attached(base)
        .fork_session_staged(
            "source",
            None,
            &ProviderForkBoundary::ThroughCheckpoint {
                checkpoint_id: "msg_u1".into(),
            },
            &report,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("history diverged"));
    assert_eq!(
        *progress.lock().unwrap(),
        vec![
            ProviderForkProgress::MutationStarted,
            ProviderForkProgress::Acknowledged {
                provider_session_id: "forked".into()
            }
        ]
    );
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .any(|(method, path, _)| method == "DELETE" && path == "/api/session/forked")
    );
    server_task.abort();

    let (base, _, server_task) = fork_wire_with(
        json!([
            {"id":"msg_u1","type":"user","time":{"created":1},"text":"one"}
        ]),
        json!([]),
        true,
    )
    .await;
    let progress = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = progress.clone();
    let report = move |event| {
        observed.lock().unwrap().push(event);
        Ok(())
    };
    let server = Server::attached(base);
    let timed_out = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        server.fork_session_staged("source", None, &ProviderForkBoundary::Tip, &report),
    )
    .await;
    assert!(timed_out.is_err());
    assert_eq!(
        progress.lock().unwrap().last(),
        Some(&ProviderForkProgress::Acknowledged {
            provider_session_id: "forked".into()
        })
    );
    server_task.abort();

    let (base, requests, server_task) =
        fork_wire_with(json!({"error":"preflight failed"}), json!([]), false).await;
    let progress = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = progress.clone();
    let report = move |event| {
        observed.lock().unwrap().push(event);
        Ok(())
    };
    let error = Server::attached(base)
        .fork_session_staged("source", None, &ProviderForkBoundary::Tip, &report)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("messages response was not an array")
    );
    assert!(progress.lock().unwrap().is_empty());
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, path, _)| path != "/api/session/source/fork")
    );
    server_task.abort();
}

#[test]
fn fork_cutoff_includes_checkpoint_by_excluding_the_next_message() {
    let messages = two_turns().as_array().unwrap().clone();
    // Through a turn: everything up to its closing idle, cut before the
    // next turn's first item.
    let (prefix, cutoff) = opencode_fork_plan(
        &messages,
        None,
        &ProviderForkBoundary::ThroughCheckpoint {
            checkpoint_id: "msg_u1".into(),
        },
    )
    .unwrap();
    assert_eq!(prefix.len(), 4);
    assert_eq!(cutoff.as_deref(), Some("msg_u2"));
    // Through the last turn: the whole history, no `before`.
    assert_eq!(
        opencode_fork_plan(
            &messages,
            None,
            &ProviderForkBoundary::ThroughCheckpoint {
                checkpoint_id: "msg_u2".into()
            }
        )
        .unwrap()
        .1,
        None
    );
    // Before a turn: exclusive, at the turn region's first item.
    let (before_prefix, before) = opencode_fork_plan(
        &messages,
        None,
        &ProviderForkBoundary::BeforeCheckpoint {
            checkpoint_id: "msg_u2".into(),
        },
    )
    .unwrap();
    assert_eq!(before_prefix, prefix);
    assert_eq!(before.as_deref(), Some("msg_u2"));
    // Nothing precedes the first turn's region; assistant ids and unknown
    // ids are not turn checkpoints.
    for checkpoint in ["msg_u1", "msg_a1", "missing"] {
        assert!(
            opencode_fork_plan(
                &messages,
                None,
                &ProviderForkBoundary::BeforeCheckpoint {
                    checkpoint_id: checkpoint.into()
                }
            )
            .is_err()
        );
    }
    // A staged revert ends the active history: a tip fork stops there.
    assert_eq!(
        opencode_fork_plan(&prefix, Some("msg_u2"), &ProviderForkBoundary::Tip)
            .unwrap()
            .1
            .as_deref(),
        Some("msg_u2")
    );
    // An unsettled assistant step is not copied by OpenCode, so it is not
    // expected either.
    let mut running = messages.clone();
    running[5]["time"] = json!({"created": 7});
    let (expected, _) = opencode_fork_plan(&running, None, &ProviderForkBoundary::Tip).unwrap();
    assert_eq!(expected.len(), messages.len() - 1);

    let fork = forked_copy(&prefix);
    let map = map_opencode_fork_history(&prefix, fork.as_array().unwrap()).unwrap();
    assert_eq!(map.get("msg_a1").map(String::as_str), Some("msg_fork_a1"));
    assert!(map_opencode_fork_history(&prefix, &messages).is_err());

    let malformed = vec![messages[1].clone(), json!({"type":"assistant"})];
    assert!(opencode_fork_plan(&malformed, None, &ProviderForkBoundary::Tip).is_err());
}

#[test]
fn fork_readback_ignores_regenerated_ids_but_preserves_semantic_nested_fields() {
    let source = vec![
        json!({"id":"msg_u1","type":"user","time":{"created":1},"text":"one"}),
        json!({
            "id":"msg_a1","type":"assistant","time":{"created":2,"completed":3},
            "content":[{"type":"tool","id":"call_1","name":"read","state":{"status":"completed","input":{"id":"semantic-id","time":17},"content":[{"type":"text","text":"x"}]}}]
        }),
        json!({"id":"msg_c1","type":"compaction","status":"completed","reason":"manual","summary":"s","recent":"r","time":{"created":4}}),
    ];
    let fork = forked_copy(&source).as_array().unwrap().clone();
    let map = map_opencode_fork_history(&source, &fork).unwrap();
    assert_eq!(map.get("msg_c1").map(String::as_str), Some("msg_fork_c1"));

    let mut semantic_mismatch = fork;
    semantic_mismatch[1]["content"][0]["state"]["input"]["id"] = "changed".into();
    assert!(map_opencode_fork_history(&source, &semantic_mismatch).is_err());
    let mut time_mismatch = semantic_mismatch;
    time_mismatch[1]["content"][0]["state"]["input"]["id"] = "semantic-id".into();
    time_mismatch[1]["content"][0]["state"]["input"]["time"] = 18.into();
    assert!(map_opencode_fork_history(&source, &time_mismatch).is_err());
}

#[test]
fn compact_posts_an_object_body_on_the_v2_route() {
    // The caller names the inbox item so `compaction.started.inputID`
    // correlates the outcome.
    assert_eq!(
        compact_http_request("session-2", "msg_1"),
        (
            "/api/session/session-2/compact".into(),
            json!({"id": "msg_1"})
        )
    );
}

#[test]
fn permission_modes_compile_to_session_scoped_rules() {
    let ask = opencode_permission_rules(luna_proto::PermissionMode::Ask);
    let rules = ask.as_array().unwrap();
    let move_deny = json!({ "action": "opencode_session_move", "resource": "*", "effect": "deny" });
    // v2 rule shape only: {action, resource, effect}.
    assert!(rules.iter().all(|rule| {
        let rule = rule.as_object().unwrap();
        rule.len() == 3
            && rule.contains_key("action")
            && rule.contains_key("resource")
            && rule.contains_key("effect")
    }));
    for action in ["shell", "edit", "external_directory", "webfetch"] {
        assert!(
            rules
                .iter()
                .any(|rule| rule["action"] == action && rule["effect"] == "ask"),
            "{action} must ask"
        );
    }
    assert_eq!(
        rules[0],
        json!({ "action": "*", "resource": "*", "effect": "ask" })
    );
    assert_eq!(
        &rules[rules.len() - 4..],
        &[
            json!({ "action": "read", "resource": "*.env", "effect": "ask" }),
            json!({ "action": "read", "resource": "*.env.*", "effect": "ask" }),
            json!({ "action": "read", "resource": "*.env.example", "effect": "allow" }),
            // Contained (PD-14): the chat's directory stays LUNA-owned.
            move_deny.clone(),
        ]
    );
    let full = opencode_permission_rules(luna_proto::PermissionMode::Full);
    assert_eq!(
        full,
        json!([{ "action": "*", "resource": "*", "effect": "allow" }, move_deny])
    );
}

#[test]
fn session_allowance_is_a_session_rule_and_replies_use_decision() {
    let labels = |grant: &[Rule]| -> Vec<String> {
        opencode_approval_choices(grant)
            .into_iter()
            .map(|choice| choice.label)
            .collect()
    };
    assert_eq!(labels(&[]), ["Deny", "Allow once", "Cancel turn"]);
    let grant = session_grant_rules(&json!({"action":"shell","save":["python3 *"]}));
    assert_eq!(
        labels(&grant),
        ["Deny", "Allow once", "Allow for session", "Cancel turn"]
    );
    assert_eq!(
        opencode_permission_reply_body(Some("Allow once"), false),
        json!({"decision":"once"})
    );
    // "Allow for session" is `once` plus LUNA's own session rule, never
    // OpenCode's project-wide `always`.
    assert_eq!(
        opencode_permission_reply_body(Some("Allow for session"), true),
        json!({"decision":"once"})
    );
    // Deny (and anything unrecognized, unanswered, or a session allowance
    // the card never offered) rejects WITH feedback so the model continues;
    // Cancel turn rejects without it and stops.
    for (answer, offered) in [
        (Some("Allow for task"), true),
        (Some("Allow for session"), false),
        (Some("Deny"), true),
        (None, true),
    ] {
        let body = opencode_permission_reply_body(answer, offered);
        assert_eq!(body["decision"], "reject");
        assert!(body["message"].as_str().is_some_and(|m| !m.is_empty()));
    }
    assert_eq!(
        opencode_permission_reply_body(Some("Cancel turn"), true),
        json!({"decision":"reject"})
    );
}

/// Real HTTP/SSE transport with explicitly ordered turn events. No provider or
/// installed CLI is involved, so duplicate completion frames are reproducible.
struct TurnWire {
    bus: mpsc::UnboundedSender<Value>,
    responses: Arc<std::sync::Mutex<HashMap<String, Value>>>,
    posts: Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    requests: mpsc::UnboundedReceiver<String>,
    events: mpsc::Receiver<Result<AgentEvent, HarnessError>>,
    interrupt: tokio_util::sync::CancellationToken,
    server: tokio::task::JoinHandle<()>,
    run: tokio::task::JoinHandle<()>,
}

impl Drop for TurnWire {
    fn drop(&mut self) {
        self.interrupt.cancel();
        self.run.abort();
        self.server.abort();
    }
}

impl TurnWire {
    /// Serves the 2.x wire (`/api/*` routes, `{data}` wrappers).
    async fn start(queued: bool) -> Self {
        Self::start_policy(queued, true, None).await
    }

    async fn start_policy(queued: bool, auto_approve: bool, answer: Option<&'static str>) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (bus, bus_rx) = mpsc::unbounded_channel::<Value>();
        let bus_rx = Arc::new(tokio::sync::Mutex::new(bus_rx));
        let responses = Arc::new(std::sync::Mutex::new(HashMap::<String, Value>::new()));
        let response_overrides = responses.clone();
        let (request_tx, requests) = mpsc::unbounded_channel();
        let posts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = posts.clone();
        let server = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let bus_rx = bus_rx.clone();
                let request_tx = request_tx.clone();
                let recorded = recorded.clone();
                let responses = response_overrides.clone();
                connections.spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0; 4096];
                    let header_end = loop {
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 { return; }
                        request.extend_from_slice(&buf[..n]);
                        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            break end + 4;
                        }
                    };
                    let header = String::from_utf8_lossy(&request[..header_end]);
                    let is_post = header.starts_with("POST ");
                    let path = header.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();
                    let length = header.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                    }).unwrap_or(0);
                    while request.len() < header_end + length {
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 { return; }
                        request.extend_from_slice(&buf[..n]);
                    }
                    if is_post { recorded.lock().unwrap().push((path.clone(), serde_json::from_slice(&request[header_end..header_end+length]).unwrap_or(Value::Null))); }
                    if path == "/api/event" {
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n: connected\n\n").await.unwrap();
                        let mut events = bus_rx.lock().await;
                        while let Some(event) = events.recv().await {
                            // A null frame closes this subscription; a retry
                            // resumes reading the same ordered event channel.
                            if event.is_null() { break; }
                            if socket.write_all(format!("data: {event}\n\n").as_bytes()).await.is_err() { break; }
                        }
                        return;
                    }
                    if path.starts_with("/api/experimental/session/fixture/log") {
                        let body = "data: {\"type\":\"log.synced\",\"seq\":100}\n\n";
                        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                        return;
                    }
                    let session_record;
                    let body = match path.as_str() {
                        "/api/info" => r#"{"version":"2.0.22","pid":1,"urls":[],"paths":{"tmp":"/tmp"}}"#,
                        "/api/session" => r#"{"data":{"id":"fixture"}}"#,
                        // Rules readback: the session holds what it was created with.
                        "/api/session/fixture" if !is_post => {
                            let permissions = recorded
                                .lock()
                                .unwrap()
                                .iter()
                                .rev()
                                .find(|(p, _)| p == "/api/session")
                                .map(|(_, body)| body["permissions"].clone())
                                .unwrap_or(Value::Null);
                            session_record = json!({"data": {"id": "fixture", "permissions": permissions}}).to_string();
                            session_record.as_str()
                        }
                        "/api/command" => r#"{"data":[]}"#,
                        // Non-empty: the catalog-sync retry loop must not stall tests.
                        "/api/model" => r#"{"data":[{"providerID":"opencode","id":"muse","name":"Muse","limit":{"context":1000},"variants":[{"id":"low"}],"enabled":true}]}"#,
                        _ => "{}",
                    };
                    let override_body = responses.lock().unwrap().get(path.split('?').next().unwrap()).map(Value::to_string);
                    let body = override_body.as_deref().unwrap_or(body);
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    if path.ends_with("/prompt")
                        || path.ends_with("/interrupt")
                        || path == "/api/model"
                    {
                        let _ = request_tx.send(path);
                    }
                });
            }
        });
        let (event_tx, events) = mpsc::channel(64);
        let (steer_tx, steering) = mpsc::channel(4);
        if queued {
            steer_tx
                .send(crate::SteerMessage {
                    input_items: Vec::new(),
                    attachments: Vec::new(),
                    prompt: "second".into(),
                    message_id: None,
                    delivery: crate::SteerDelivery::AllowFollowUpTurn,
                    acceptance: None,
                    goal: None,
                })
                .await
                .unwrap();
        }
        drop(steer_tx);
        let interrupt = tokio_util::sync::CancellationToken::new();
        let run = tokio::spawn(run_session(Session {
            server: Server::attached(base),
            event_tx,
            controls: RunControls {
                request_input: Box::new(move |questions| {
                    let answer = answer.expect("fixture must not ask for input");
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = tx.send(
                        questions
                            .into_iter()
                            .map(|q| UserInputAnswer {
                                question_id: q.id,
                                labels: vec![answer.into()],
                            })
                            .collect(),
                    );
                    rx
                }),
                steering,
                interrupt: interrupt.clone(),
                invoke_luna_tool: None,
            },
            request: serde_json::from_value(
                json!({"prompt":"first", "cwd":"", "sandbox":"workspace-write", "autoApprove": auto_approve, "model": "opencode/muse", "reasoning": "low"}),
            )
            .unwrap(),
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_millis(50),
            known_commands: Some(vec![]),
            live_runs: LiveRuns::default(),
            live: LiveServers::default(),
            luna: None,
            grants: SessionGrants::default(),
        }));
        Self {
            bus,
            responses,
            posts,
            requests,
            events,
            interrupt,
            server,
            run,
        }
    }

    async fn request(&mut self, suffix: &str) {
        let path = tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(path.ends_with(suffix), "unexpected request: {path}");
    }

    /// Push one raw 2.x `/api/event` frame (normalized in the real
    /// stream_bus, not here).
    fn v2(&self, kind: &str, data: Value) {
        self.bus
            .send(json!({"id": format!("evt_{kind}"), "type": kind, "data": data}))
            .unwrap();
    }

    async fn done(&mut self) -> (DoneStatus, String) {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut text = String::new();
            loop {
                match self.events.recv().await.unwrap().unwrap() {
                    AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                    AgentEvent::Done { status, .. } => return (status, text),
                    _ => {}
                }
            }
        })
        .await
        .unwrap()
    }
}

/// Failure case: a reconnect readback recovers the completed item before
/// live bytes already waiting on the socket. Those bytes must not append
/// again after the completed snapshot, including for reasoning.
#[tokio::test]
async fn completed_stream_snapshots_ignore_buffered_deltas() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    wire.v2(
        "session.step.started",
        json!({"sessionID": "fixture", "assistantMessageID": "msg_recovered"}),
    );
    for kind in ["text", "reasoning"] {
        let data = json!({
            "sessionID": "fixture", "assistantMessageID": "msg_recovered", "ordinal": 0,
        });
        wire.v2(&format!("session.{kind}.started"), data.clone());
        let mut delta = data.clone();
        delta["delta"] = json!("first");
        wire.v2(&format!("session.{kind}.delta"), delta.clone());
        let mut ended = data.clone();
        ended["text"] = json!("first recovered");
        wire.v2(&format!("session.{kind}.ended"), ended.clone());
        // The subscription's buffered opening frame cannot reopen it either.
        wire.v2(&format!("session.{kind}.started"), data);
        delta["delta"] = json!(" recovered");
        wire.v2(&format!("session.{kind}.delta"), delta);
        wire.v2(&format!("session.{kind}.ended"), ended);
    }
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID": "fixture"}),
    );
    let (status, text, reasoning) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut text = String::new();
        let mut reasoning = String::new();
        loop {
            match wire.events.recv().await.unwrap().unwrap() {
                AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                AgentEvent::ReasoningDelta { text: delta } => reasoning.push_str(&delta),
                AgentEvent::Done { status, .. } => return (status, text, reasoning),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(status, DoneStatus::Completed);
    assert_eq!(text, "first recovered");
    assert_eq!(reasoning, "first recovered");
}

/// Failure case: HTTP 200 without SSE bytes is not a live subscription.
/// The first retry with bytes must still unlock prompt/compaction admission.
#[tokio::test]
async fn empty_sse_attempt_does_not_consume_initial_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut events = 0;
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut bytes = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut bytes).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&bytes[..n]);
            }
            if request.starts_with(b"GET /api/event ") {
                events += 1;
                if events == 1 {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await
                        .unwrap();
                    continue;
                }
                connections.spawn(async move {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n: connected\n\n").await.unwrap();
                    std::future::pending::<()>().await;
                });
            } else {
                let body = "data: {\"type\":\"log.synced\",\"seq\":0}\n\n";
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        }
    });
    let (tx, mut rx) = mpsc::channel(16);
    let bus = tokio::spawn(bus_task(
        BusConfig {
            base,
            auth: None,
            session_id: "fixture".into(),
            directory: None,
        },
        tx,
    ));
    let first = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;
    bus.abort();
    server.abort();
    assert!(
        matches!(first, Ok(Some(BusMsg::Connected))),
        "first live SSE subscription must announce Connected"
    );
}

#[tokio::test]
async fn empty_sse_attempts_exhaust_reconnect_budget() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut bytes = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut bytes).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&bytes[..n]);
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        }
    });
    let (tx, mut rx) = mpsc::channel(16);
    let bus = tokio::spawn(bus_task(
        BusConfig {
            base,
            auth: None,
            session_id: "fixture".into(),
            directory: None,
        },
        tx,
    ));
    let deadline = BUS_RECONNECT_DELAY * (BUS_RECONNECT_ATTEMPTS + 1) + Duration::from_secs(5);
    let outcome = tokio::time::timeout(deadline, rx.recv()).await;
    bus.abort();
    server.abort();
    assert!(
        matches!(outcome, Ok(Some(BusMsg::Disconnected))),
        "empty successes must not reset the retry budget"
    );
}

/// A dropped settlement and another session's pending asks must not leave
/// this turn permanently exempt from its silence watchdog. An unreadable
/// list still provides no evidence that a previously pending ask settled.
#[tokio::test]
async fn reconnect_reconciles_pending_request_ownership_and_settlement() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.responses.lock().unwrap().extend([
        ("/api/session/active".into(), json!({"data":{"fixture":{}}})),
        (
            "/api/form".into(),
            json!({"data":[{"id":"frm_foreign","sessionID":"foreign"}]}),
        ),
        // A malformed response is an unknown readback, not an empty list.
        ("/api/permission/request".into(), json!({})),
    ]);
    wire.v2("session.execution.started", json!({"sessionID":"fixture"}));
    wire.v2(
        "permission.asked",
        json!({
            "id":"per_stale", "sessionID":"fixture", "action":"shell", "resources":["echo safe"],
        }),
    );
    for expected in [1, 0] {
        if expected == 0 {
            wire.responses
                .lock()
                .unwrap()
                .insert("/api/permission/request".into(), json!({"data":[]}));
        }
        wire.bus.send(Value::Null).unwrap();
        let pending = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let AgentEvent::ProviderEvidence { kind, payload, .. } =
                    wire.events.recv().await.unwrap().unwrap()
                    && kind == "native:readback.requests"
                {
                    break payload["pending"].as_u64().unwrap();
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(pending, expected);
    }
}

/// The root already sent Done and is parked. Only history, not the live
/// socket, contains the child's completion and the root's wake response.
#[tokio::test]
async fn reconnect_recovers_parked_child_completion_without_crossing_owners() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    let prompt_id = wire
        .posts
        .lock()
        .unwrap()
        .iter()
        .find(|(path, _)| path.ends_with("/prompt"))
        .unwrap()
        .1["id"]
        .clone();
    wire.v2("session.execution.started", json!({"sessionID":"fixture"}));
    wire.v2(
        "session.inbox.delivered",
        json!({"sessionID":"fixture","inboxID":prompt_id}),
    );
    wire.v2(
        "session.step.started",
        json!({"sessionID":"fixture","assistantMessageID":"msg_parent"}),
    );
    for (call, child) in [("one", "child_one"), ("two", "child_two")] {
        wire.v2("session.tool.input.started", json!({"sessionID":"fixture","assistantMessageID":"msg_parent","id":call,"name":"subagent"}));
        wire.v2("session.tool.called", json!({"sessionID":"fixture","assistantMessageID":"msg_parent","id":call,"input":{"description":call,"prompt":"work","background":true}}));
        wire.v2(
            "session.created",
            json!({"sessionID":child,"parentID":"fixture","title":call}),
        );
        wire.v2("session.tool.success", json!({"sessionID":"fixture","assistantMessageID":"msg_parent","id":call,"content":[{"type":"text","text":"started"}],"metadata":{"sessionID":child,"status":"running"}}));
    }
    wire.v2("session.text.ended", json!({"sessionID":"fixture","assistantMessageID":"msg_parent","ordinal":0,"text":"original"}));
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID":"fixture"}),
    );
    assert_eq!(
        wire.done().await,
        (DoneStatus::Completed, "original".into())
    );

    let history = json!([
        {"id":prompt_id,"type":"user","text":"first"},
        {"id":"msg_parent","type":"assistant","content":[{"type":"text","text":"original"}]},
        {"id":"msg_idle","type":"idle","outcome":"succeeded"},
        {"id":"msg_foreign","type":"synthetic","text":"foreign","metadata":{"source":"subagent","childID":"foreign_child","state":"completed"}},
        {"id":"msg_completion","type":"synthetic","text":"complete","metadata":{"source":"subagent","childID":"child_one","state":"completed"}},
        {"id":"msg_wake","type":"assistant","content":[{"type":"text","text":"wake response"}]},
        {"id":"msg_wake_idle","type":"idle","outcome":"succeeded"},
    ]);
    let mut newest_first = history.as_array().unwrap().clone();
    newest_first.reverse();
    wire.responses.lock().unwrap().extend([
        (
            "/api/session/fixture/message".into(),
            json!({"data":newest_first}),
        ),
        ("/api/permission/request".into(), json!({"data":[]})),
        ("/api/form".into(), json!({"data":[]})),
    ]);
    wire.bus.send(Value::Null).unwrap();
    let (children, text, turns) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut children = Vec::new();
        let mut text = String::new();
        let mut turns = 0;
        loop {
            match wire.events.recv().await.unwrap().unwrap() {
                AgentEvent::Subagent {
                    parent_tool_use_id,
                    event,
                } if matches!(*event, AgentEvent::Done { .. }) => children.push(parent_tool_use_id),
                AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                AgentEvent::ProviderTurnStarted { .. } => turns += 1,
                AgentEvent::Done { status, .. } => {
                    assert_eq!(status, DoneStatus::Completed);
                    return (children, text, turns);
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(children, ["fixture:msg_parent:one"]);
    assert_eq!(text, "wake response");
    assert_eq!(turns, 1);
}

#[tokio::test]
async fn queued_turn_ignores_previous_turn_duplicate_terminal() {
    let mut wire = TurnWire::start(true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    // A repeated terminal for the first turn: the first submits the queued
    // prompt; the duplicate must not finish that new turn.
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID": "fixture"}),
    );
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID": "fixture"}),
    );
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    wire.v2(
        "session.step.started",
        json!({"sessionID":"fixture","assistantMessageID":"answer"}),
    );
    wire.v2(
        "session.text.ended",
        json!({"sessionID":"fixture","assistantMessageID":"answer","ordinal":0,"text":"SECOND_OK"}),
    );
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID": "fixture"}),
    );
    let (status, text) = wire.done().await;
    assert_eq!(status, DoneStatus::Completed);
    assert_eq!(
        text, "SECOND_OK",
        "queued turn was completed before its response"
    );
}

#[tokio::test]
async fn v2_wire_streams_text_and_settles_on_execution_success() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    wire.v2(
        "session.step.started",
        json!({
            "sessionID": "fixture",
            "assistantMessageID": "msg_a",
            "model": {"id": "muse", "providerID": "opencode", "variant": "low"},
        }),
    );
    wire.v2(
        "session.text.started",
        json!({
            "sessionID": "fixture", "assistantMessageID": "msg_a", "ordinal": 0
        }),
    );
    wire.v2(
        "session.text.delta",
        json!({
            "sessionID": "fixture", "assistantMessageID": "msg_a", "ordinal": 0,
            "delta": "PONG"
        }),
    );
    wire.v2(
        "session.text.ended",
        json!({
            "sessionID": "fixture", "assistantMessageID": "msg_a", "ordinal": 0,
            "text": "PONG"
        }),
    );
    // Per-step usage (turn tokens and context) and the running totals.
    wire.v2(
        "session.step.ended",
        json!({
            "sessionID": "fixture", "assistantMessageID": "msg_a",
            "finish": "stop", "cost": 0,
            "tokens": {"input": 10, "output": 2, "reasoning": 0,
                       "cache": {"read": 0, "write": 0}}
        }),
    );
    wire.v2(
        "session.usage.updated",
        json!({
            "sessionID": "fixture", "cost": 0,
            "tokens": {"input": 10, "output": 2, "reasoning": 0,
                       "cache": {"read": 0, "write": 0}}
        }),
    );
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID": "fixture"}),
    );

    let (status, text, usage) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut text = String::new();
        let mut usage = None;
        loop {
            match wire.events.recv().await.unwrap().unwrap() {
                AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    usage = Some((input_tokens, output_tokens));
                }
                AgentEvent::Done { status, .. } => return (status, text, usage),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(status, DoneStatus::Completed);
    assert_eq!(text, "PONG");
    assert_eq!(usage, Some((10, 2)));
}

#[tokio::test]
async fn v2_wire_tool_frames_open_and_resolve_chips() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    wire.v2(
        "session.step.started",
        json!({
            "sessionID": "fixture", "assistantMessageID": "msg_a"
        }),
    );
    wire.v2(
        "session.tool.input.started",
        json!({
            "sessionID": "fixture", "assistantMessageID": "msg_a",
            "id": "call_1", "name": "read"
        }),
    );
    wire.v2(
        "session.tool.called",
        json!({
            "sessionID": "fixture", "assistantMessageID": "msg_a",
            "id": "call_1", "input": {"path": "/tmp/oc2x-probe/note.txt"}
        }),
    );
    wire.v2(
        "session.tool.success",
        json!({
            "sessionID": "fixture", "assistantMessageID": "msg_a",
            "id": "call_1",
            "content": [{"type": "text", "text": "1: The secret word is BANANA42"}]
        }),
    );
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID": "fixture"}),
    );

    let (status, calls, results) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut calls = Vec::new();
        let mut results = Vec::new();
        loop {
            match wire.events.recv().await.unwrap().unwrap() {
                AgentEvent::ToolCall { id, call } => calls.push((id, call)),
                AgentEvent::ToolResult { id, output, .. } => results.push((id, output)),
                AgentEvent::Done { status, .. } => return (status, calls, results),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(status, DoneStatus::Completed);
    assert_eq!(
        calls,
        vec![(
            "fixture:msg_a:call_1".to_owned(),
            ToolCall::ReadFile {
                path: "/tmp/oc2x-probe/note.txt".to_owned()
            }
        )]
    );
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "fixture:msg_a:call_1");
    let payload = envelope(results[0].1.as_deref().expect("output"));
    assert_eq!(payload["tool"], "read");
    assert_eq!(
        payload["content"],
        json!([{"type": "text", "text": "1: The secret word is BANANA42"}])
    );
}

#[tokio::test]
async fn v2_execution_failure_and_interrupt_settle_the_turn() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    wire.v2(
        "session.execution.failed",
        json!({"sessionID": "fixture", "error": {"type": "provider.auth", "message": ""}}),
    );
    let (status, error) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut error = None;
        loop {
            match wire.events.recv().await.unwrap().unwrap() {
                AgentEvent::Error { message } => error = Some(message),
                AgentEvent::Done {
                    status, error: e, ..
                } => return (status, e.or(error)),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(status, DoneStatus::Errored);
    assert!(error.unwrap().contains("provider.auth"));

    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    wire.interrupt.cancel();
    wire.request("/interrupt").await;
    wire.v2(
        "session.execution.interrupted",
        json!({"sessionID": "fixture", "reason": "user"}),
    );
    assert_eq!(wire.done().await.0, DoneStatus::Interrupted);
}

/// Replays the native S9Q failure ordering: one failed step, one failed
/// execution. Each Error and Done.error becomes a separate transcript part,
/// so only Done may carry the terminal reason needed by crash recovery.
#[tokio::test]
async fn v2_terminal_failures_emit_one_persistent_error() {
    for (step_failure, partial_content, deleted) in [
        (true, false, false),
        (false, false, false),
        (true, true, false),
        (false, true, true),
    ] {
        let mut wire = TurnWire::start(false).await;
        wire.request("/api/model").await;
        wire.request("/prompt").await;
        wire.v2("session.execution.started", json!({"sessionID":"fixture"}));
        wire.v2(
            "session.step.started",
            json!({"sessionID":"fixture","assistantMessageID":"msg_failed"}),
        );
        if partial_content {
            wire.v2("session.text.ended", json!({"sessionID":"fixture","assistantMessageID":"msg_failed","ordinal":0,"text":"partial answer"}));
        }
        let error = json!({
            "type":"provider.quota", "message":"Rate limit exceeded. Please try again later.", "status":429,
            "response":{"body":"{\"type\":\"error\",\"error\":{\"type\":\"FreeUsageLimitError\",\"message\":\"Rate limit exceeded. Please try again later.\"},\"metadata\":{}}"},
        });
        if step_failure {
            wire.v2(
                "session.step.failed",
                json!({"sessionID":"fixture","assistantMessageID":"msg_failed","error":error}),
            );
        }
        wire.v2(
            if deleted {
                "session.deleted"
            } else {
                "session.execution.failed"
            },
            json!({"sessionID":"fixture","error":error}),
        );
        let (status, terminal, errors, warnings) =
            tokio::time::timeout(Duration::from_secs(5), async {
                let mut errors = Vec::new();
                let mut warnings = 0;
                loop {
                    match wire.events.recv().await.unwrap().unwrap() {
                        AgentEvent::Error { message } => errors.push(message),
                        AgentEvent::ProviderNotice { notice }
                            if notice.kind == luna_proto::ProviderNoticeKind::Warning =>
                        {
                            warnings += 1
                        }
                        AgentEvent::Done { status, error, .. } => {
                            return (status, error, errors, warnings);
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
        assert_eq!(status, DoneStatus::Errored);
        assert!(
            errors.is_empty(),
            "terminal reason must only appear on Done: {errors:?}"
        );
        let terminal = terminal.expect("crash recovery requires the terminal reason");
        assert!(
            terminal.contains(if deleted {
                "no longer exists"
            } else {
                "provider.quota"
            }),
            "{terminal}"
        );
        assert_eq!(warnings, usize::from(step_failure));
    }
}

#[tokio::test]
async fn v2_recovered_step_failure_is_a_transient_warning() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID":"fixture"}));
    let failure = json!({"sessionID":"foreign","error":{"type":"provider.quota","message":"foreign failure"}});
    wire.v2("session.step.failed", failure);
    wire.v2(
        "session.step.failed",
        json!({"sessionID":"fixture","error":{"type":"provider.rate_limit","message":"Retrying"}}),
    );
    wire.v2(
        "session.step.started",
        json!({"sessionID":"fixture","assistantMessageID":"msg_recovered"}),
    );
    wire.v2("session.text.ended", json!({"sessionID":"fixture","assistantMessageID":"msg_recovered","ordinal":0,"text":"recovered"}));
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID":"fixture"}),
    );
    let (status, terminal, errors, warnings, text) =
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut errors = Vec::new();
            let mut warnings = Vec::new();
            let mut text = String::new();
            loop {
                match wire.events.recv().await.unwrap().unwrap() {
                    AgentEvent::Error { message } => errors.push(message),
                    AgentEvent::ProviderNotice { notice }
                        if notice.kind == luna_proto::ProviderNoticeKind::Warning =>
                    {
                        warnings.push(notice.message)
                    }
                    AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                    AgentEvent::Done { status, error, .. } => {
                        return (status, error, errors, warnings, text);
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(status, DoneStatus::Completed);
    assert!(terminal.is_none());
    assert!(errors.is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("Retrying"));
    assert_eq!(text, "recovered");
}

#[tokio::test]
async fn catalog_decodes_fragmented_http_without_retaining_unused_fields() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = json!({
        "all": [{"id":"test", "models":{"model":{"name":"模型", "variants":{"high":{"unused":"x".repeat(128 * 1024)}}}}}],
        "connected":["test"]
    }).to_string();
    let expected: ProviderCatalog = serde_json::from_str(&body).unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(socket.read(&mut [0; 4096]).await.unwrap() > 0);
        socket
            .write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
            )
            .await
            .unwrap();
        for chunk in body.as_bytes().chunks(257) {
            socket.write_all(chunk).await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    let response = reqwest::get(format!("http://{address}")).await.unwrap();
    let catalog: ProviderCatalog = decode_json_response(response).await.unwrap();
    assert_eq!(
        models_from_providers(&catalog),
        models_from_providers(&expected)
    );
    server.await.unwrap();
}

#[tokio::test]
async fn cancelled_catalog_decode_releases_a_stalled_http_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(socket.read(&mut [0; 4096]).await.unwrap() > 0);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n{")
            .await
            .unwrap();
        let result = socket.read(&mut [0; 1]).await;
        let _ = closed_tx.send(result);
    });
    let response = reqwest::get(format!("http://{address}")).await.unwrap();
    let decode = tokio::spawn(decode_json_response::<ProviderCatalog>(response));
    tokio::time::sleep(Duration::from_millis(20)).await;
    decode.abort();
    assert!(decode.await.unwrap_err().is_cancelled());
    let result = tokio::time::timeout(Duration::from_secs(2), closed_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(result, Ok(0) | Err(_)),
        "cancel must close the body reader"
    );
    server.await.unwrap();
}
use serde_json::json;

#[test]
fn provider_discovery_ignores_metadata_and_accepts_null_optional_fields() {
    let catalog: ProviderCatalog = serde_json::from_str(
        r#"{
        "all": [{
            "id": "local", "name": null,
            "models": {
                "small": {"name": null, "variants": null},
                "thinking": {
                    "variants": {"high": {"nested": [{"unused": "configuration"}]}},
                    "capabilities": {"large": [1, 2, 3]},
                    "cost": {"input": 1}, "limit": {"context": 200000}
                }
            },
            "options": {"unused": [true, false, null]}
        }, {"id": "empty", "models": null}],
        "connected": null,
        "default": {"unused": "model"}
    }"#,
    )
    .unwrap();
    let models = models_from_providers(&catalog);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "local/small");
    assert_eq!(models[0].description.as_deref(), Some("local"));
    assert!(models[0].reasoning_levels.is_empty());
    assert_eq!(models[1].reasoning_levels, vec![ReasoningLevel::High]);
    assert_eq!(
        pick_variant(&catalog, "local", "thinking", Some(ReasoningLevel::High)).as_deref(),
        Some("high")
    );
}

#[test]
fn models_map_provider_catalog_with_variant_ladders() {
    let providers: ProviderCatalog = serde_json::from_value(json!({
        "all": [
            {
                "id": "anthropic",
                "name": "Anthropic",
                "models": {
                    "claude-opus-5": {
                        "name": "Claude Opus 5",
                        "variants": {"low": {}, "medium": {}, "high": {}, "max": {}},
                    },
                    "claude-haiku-4-5": {"name": "Claude Haiku 4.5"},
                }
            },
            {
                "id": "opencode",
                "name": "OpenCode Zen",
                "models": {"big-pickle": {"name": "Big Pickle"}}
            }
        ],
        "default": {},
        "connected": ["anthropic"],
    }))
    .unwrap();
    let models = models_from_providers(&providers);
    // `connected` filters: the full catalog is 194 providers / 7k models of
    // which the user can run almost none (v0.2.21 field report).
    assert_eq!(models.len(), 2);
    let opus = models
        .iter()
        .find(|m| m.id == "anthropic/claude-opus-5")
        .expect("opus");
    assert_eq!(opus.label, "Claude Opus 5");
    assert_eq!(opus.description.as_deref(), Some("Anthropic"));
    assert_eq!(
        opus.reasoning_levels,
        vec![
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::Max
        ]
    );
    let haiku = models
        .iter()
        .find(|m| m.id == "anthropic/claude-haiku-4-5")
        .expect("haiku");
    assert!(haiku.reasoning_levels.is_empty());
    assert!(
        !models.iter().any(|m| m.id == "opencode/big-pickle"),
        "unconnected providers stay out of the picker"
    );
}

#[test]
fn missing_connected_list_falls_back_to_the_full_catalog() {
    let providers: ProviderCatalog = serde_json::from_value(json!({
        "all": [
            {"id": "a", "models": {"m1": {}}},
            {"id": "b", "models": {"m2": {}}},
        ],
    }))
    .unwrap();
    assert_eq!(models_from_providers(&providers).len(), 2);
    let providers: ProviderCatalog = serde_json::from_value(json!({
        "all": [
            {"id": "a", "models": {"m1": {}}},
            {"id": "b", "models": {"m2": {}}},
        ],
        "connected": [],
    }))
    .unwrap();
    assert_eq!(models_from_providers(&providers).len(), 2);
}

#[test]
fn variants_only_ride_models_that_advertise_them() {
    let providers: ProviderCatalog = serde_json::from_value(json!({
        "all": [{
            "id": "anthropic",
            "models": {
                "opus": {"variants": {"high": {}, "max": {}}},
                "haiku": {},
            }
        }]
    }))
    .unwrap();
    assert_eq!(
        pick_variant(&providers, "anthropic", "opus", Some(ReasoningLevel::High)).as_deref(),
        Some("high")
    );
    // XHigh clamps down the candidate ladder to an advertised id.
    assert_eq!(
        pick_variant(&providers, "anthropic", "opus", Some(ReasoningLevel::XHigh)).as_deref(),
        Some("high")
    );
    assert_eq!(
        pick_variant(&providers, "anthropic", "haiku", Some(ReasoningLevel::High)),
        None
    );
    assert_eq!(pick_variant(&providers, "anthropic", "opus", None), None);
    assert_eq!(
        pick_variant(&providers, "missing", "opus", Some(ReasoningLevel::Low)),
        None
    );
}

fn feed_with_assistant(message: &str) -> SessionFeed {
    let mut feed = SessionFeed::default();
    feed.assistant_messages.insert(message.into(), true);
    feed
}

#[test]
fn reasoning_parts_stream_as_reasoning_deltas() {
    let mut feed = feed_with_assistant("msg_a");
    // Opening snapshot: empty reasoning part fixes the kind.
    let open = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "",
    });
    assert!(part_snapshot_events(&mut feed, &open, true).is_empty());
    // Deltas append as ReasoningDelta, not text.
    let props = json!({"sessionID": "ses_1", "messageID": "msg_a", "partID": "prt_r"});
    let events = part_delta_events(&mut feed, &props, "prt_r", "thinking hard");
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ReasoningDelta { text }] if text == "thinking hard"
    ));
    // The closing full snapshot re-sends everything: dedup emits nothing.
    let close = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "thinking hard",
    });
    assert!(part_snapshot_events(&mut feed, &close, true).is_empty());
    // A longer snapshot emits only the suffix.
    let more = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "thinking hard about it",
    });
    let events = part_snapshot_events(&mut feed, &more, true);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ReasoningDelta { text }] if text == " about it"
    ));
}

#[test]
fn reasoning_ahead_of_its_message_role_is_held_and_replayed() {
    let mut feed = SessionFeed::default();
    let part = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "early thought",
    });
    assert!(part_snapshot_events(&mut feed, &part, true).is_empty());
    assert_eq!(feed.pending_parts.len(), 1);
    // The role lands; replay drains the held part.
    feed.assistant_messages.insert("msg_a".into(), true);
    let mut turn = TurnState::begin(None);
    let events = replay_pending(&mut feed, "msg_a", true, &mut turn);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ReasoningDelta { text }] if text == "early thought"
    ));
    assert!(turn.saw_content);
}

#[test]
fn main_feed_user_text_is_the_prompt_echo_and_never_renders() {
    let mut feed = SessionFeed::default();
    feed.assistant_messages.insert("msg_u".into(), false);
    let part = json!({
        "id": "prt_u", "messageID": "msg_u", "sessionID": "ses_1",
        "type": "text", "text": "the prompt",
    });
    assert!(part_snapshot_events(&mut feed, &part, true).is_empty());
    // On a CHILD feed the same shape is the message INTO the child.
    let mut child = SessionFeed::default();
    child.assistant_messages.insert("msg_u".into(), false);
    let events = part_snapshot_events(&mut child, &part, false);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::UserMessage { text }] if text == "the prompt"
    ));
    // Re-delivered snapshots don't double the entry.
    assert!(part_snapshot_events(&mut child, &part, false).is_empty());
}

/// The `native` payload of an OpenCode tool-result envelope.
fn envelope(output: &str) -> Value {
    let value: Value = serde_json::from_str(output).expect("envelope JSON");
    let payload = value["lunaToolOutput"].clone();
    assert_eq!(payload["kind"], "native");
    assert_eq!(payload["provider"], "opencode");
    payload
}

#[test]
fn tool_parts_open_and_resolve_once() {
    let mut feed = feed_with_assistant("msg_a");
    let running = json!({
        "id": "prt_t", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "shell", "callID": "call-1",
        "state": {"status": "running", "input": {"command": "echo ok"}},
    });
    let events = part_snapshot_events(&mut feed, &running, true);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ToolCall { id, call: ToolCall::Exec { command } }]
            if id == "call-1" && command == "echo ok"
    ));
    assert!(part_snapshot_events(&mut feed, &running, true).is_empty());
    let done = json!({
        "id": "prt_t", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "shell", "callID": "call-1",
        "state": {"status": "completed", "output": "ok\n",
                  "content": [{"type": "text", "text": "ok\n"}],
                  "metadata": {"truncated": false, "exit": 0}},
    });
    let events = part_snapshot_events(&mut feed, &done, true);
    let [
        AgentEvent::ToolResult {
            id,
            is_error: false,
            output: Some(o),
            ..
        },
    ] = events.as_slice()
    else {
        panic!("one result: {events:?}");
    };
    assert_eq!(id, "call-1");
    let payload = envelope(o);
    assert_eq!(payload["content"][0]["text"], "ok\n");
    assert_eq!(payload["metadata"]["exit"], 0);
    assert_eq!(payload["input"]["command"], "echo ok");
    // A replayed terminal frame never resolves the chip twice.
    assert!(part_snapshot_events(&mut feed, &done, true).is_empty());
}

/// Feed raw v2 frames through the normalizer into one child tree; returns
/// the observation events (spawn chip ToolCalls come from the part path).
fn observe_v2(
    tree: &mut ChildTree,
    names: &mut HashMap<V2ToolKey, String>,
    root: &str,
    kind: &str,
    data: Value,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    for payload in normalize_v2_frame(json!({"type": kind, "data": data}), names) {
        let props = payload.get("properties").cloned().unwrap_or(Value::Null);
        match payload.get("type").and_then(Value::as_str) {
            Some(OBSERVE) => events.extend(tree.observe(root, &props, true).events),
            Some("session.created") => {
                let info = &props["info"];
                events.extend(tree.on_session_created(
                    root,
                    info["id"].as_str().unwrap_or(""),
                    info["parentID"].as_str().unwrap_or(""),
                    info["title"].as_str().unwrap_or(""),
                ));
            }
            _ => {}
        }
    }
    events
}

#[test]
fn task_spawn_registers_child_by_metadata_and_completion_settles() {
    let mut tree = ChildTree::default();
    let mut names = HashMap::new();
    let tool = |extra: Value| {
        let mut data =
            json!({"sessionID": "ses_parent", "assistantMessageID": "msg_a", "id": "call_1"});
        data.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        data
    };
    observe_v2(
        &mut tree,
        &mut names,
        "ses_parent",
        "session.tool.input.started",
        tool(json!({"name": "subagent"})),
    );
    observe_v2(
        &mut tree,
        &mut names,
        "ses_parent",
        "session.tool.called",
        tool(
            json!({"input": {"agent": "general", "description": "Scan crates", "prompt": "scan"}, "executed": true}),
        ),
    );
    // Genus-gated spawn naming; the chip is the v2 call id.
    let chip = "ses_parent:msg_a:call_1";
    assert!(matches!(
        oc_tool_call("subagent", &json!({"description": "Scan crates"}), &[]),
        ToolCall::Unknown { name, .. } if name == "Agent: Scan crates"
    ));
    // The tool's own metadata binds the child and seeds its opening prompt.
    let events = observe_v2(
        &mut tree,
        &mut names,
        "ses_parent",
        "session.tool.progress",
        tool(json!({"metadata": {"sessionID": "ses_child", "status": "running"}})),
    );
    assert_eq!(
        tree.runs
            .get("ses_child")
            .expect("bound child")
            .parent_tool_use_id,
        chip
    );
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::Subagent { parent_tool_use_id, event }]
            if parent_tool_use_id == chip
                && matches!(event.as_ref(), AgentEvent::UserMessage { text } if text == "scan")
    ));
    // The foreground result settles the chip, once.
    let completed = tool(json!({
        "content": [{"type": "text", "text": "<subagent>done</subagent>"}],
        "metadata": {"sessionID": "ses_child", "status": "completed"},
        "executed": true,
    }));
    let events = observe_v2(
        &mut tree,
        &mut names,
        "ses_parent",
        "session.tool.success",
        completed.clone(),
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == chip
                && matches!(event.as_ref(), AgentEvent::Done { status: DoneStatus::Completed, .. })
    )));
    assert!(
        observe_v2(
            &mut tree,
            &mut names,
            "ses_parent",
            "session.tool.success",
            completed
        )
        .is_empty()
    );
}

#[test]
fn child_binding_falls_back_to_title_match() {
    let mut tree = ChildTree::default();
    let mut names = HashMap::new();
    for (call, description) in [("call_1", "Scan crates"), ("call_2", "Write docs")] {
        let data = json!({"sessionID": "ses_root", "assistantMessageID": "msg_a", "id": call});
        let mut started = data.clone();
        started["name"] = json!("subagent");
        observe_v2(
            &mut tree,
            &mut names,
            "ses_root",
            "session.tool.input.started",
            started,
        );
        let mut called = data;
        called["input"] = json!({"agent": "general", "description": description, "prompt": "p"});
        observe_v2(
            &mut tree,
            &mut names,
            "ses_root",
            "session.tool.called",
            called,
        );
    }
    let created = |id: &str, parent: &str, title: &str| json!({"sessionID": id, "parentID": parent, "title": title, "slug": "s", "version": "2"});
    observe_v2(
        &mut tree,
        &mut names,
        "ses_root",
        "session.created",
        created("ses_b", "ses_root", "Write docs"),
    );
    assert_eq!(
        tree.runs.get("ses_b").unwrap().parent_tool_use_id,
        "ses_root:msg_a:call_2"
    );
    // Unmatched title binds FIFO.
    observe_v2(
        &mut tree,
        &mut names,
        "ses_root",
        "session.created",
        created("ses_a", "ses_root", "mystery"),
    );
    assert_eq!(
        tree.runs.get("ses_a").unwrap().parent_tool_use_id,
        "ses_root:msg_a:call_1"
    );
    // Nothing pending: no bind, and a foreign parent never joins the tree.
    observe_v2(
        &mut tree,
        &mut names,
        "ses_root",
        "session.created",
        created("ses_c", "ses_root", "anything"),
    );
    assert!(!tree.runs.contains_key("ses_c"));
    observe_v2(
        &mut tree,
        &mut names,
        "ses_root",
        "session.created",
        created("ses_x", "ses_foreign", "Scan crates"),
    );
    assert!(!tree.owns("ses_root", "ses_x"));
}

#[test]
fn tool_names_type_the_common_calls() {
    let call = oc_tool_call("shell", &json!({"command": "ls -la", "workdir": "/w"}), &[]);
    assert_eq!(
        call,
        ToolCall::Exec {
            command: "ls -la".into()
        }
    );
    let call = oc_tool_call(
        "edit",
        &json!({"path": "/w/a.rs", "oldString": "a", "newString": "b"}),
        &[],
    );
    assert_eq!(
        call,
        ToolCall::EditFile {
            path: "/w/a.rs".into(),
            old_string: Some("a".into()),
            new_string: Some("b".into()),
        }
    );
    let call = oc_tool_call("grep", &json!({"pattern": "fn", "include": "*.rs"}), &[]);
    assert_eq!(
        call,
        ToolCall::Search {
            pattern: "fn".into(),
            path: None,
        }
    );
    let call = oc_tool_call(
        "subagent",
        &json!({"agent": "general", "description": "Scan crates", "prompt": "scan"}),
        &[],
    );
    assert!(matches!(&call, ToolCall::Unknown { name, .. } if name == "Agent: Scan crates"));
    assert!(call.is_subagent_spawn());
    let call = oc_tool_call("task", &json!({"description": "Scan crates"}), &[]);
    assert!(call.is_subagent_spawn());
    // MCP `<server>_<tool>`: the longest configured server name wins.
    let servers = vec!["my".to_owned(), "my_srv".to_owned()];
    let call = oc_tool_call("my_srv_fetch_page", &json!({"u": 1}), &servers);
    assert!(matches!(
        &call,
        ToolCall::Mcp { server, tool, input: Some(_) } if server == "my_srv" && tool == "fetch_page"
    ));
    // 1.x names render generic, input preserved.
    let call = oc_tool_call("bash", &json!({"command": "ls"}), &[]);
    assert!(matches!(&call, ToolCall::Unknown { name, input: Some(_) } if name == "bash"));
    let call = oc_tool_call("mystery", &json!({"x": 1}), &[]);
    assert!(matches!(&call, ToolCall::Unknown { name, input: Some(_) } if name == "mystery"));
    assert!(!call.is_subagent_spawn());
}

#[test]
fn commands_map_from_wire() {
    let wire = json!([
        {"name": "init", "description": "Create AGENTS.md"},
        {"name": "share"},
        {"description": "nameless is dropped"},
    ]);
    let commands = commands_from_wire(&wire);
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].name, "init");
    assert_eq!(commands[0].description, "Create AGENTS.md");
    assert_eq!(commands[1].name, "share");
}

#[test]
fn stall_env_and_startup_env_parse() {
    // Defaults (no env in test runner): bounded stall, 300s startup.
    assert_eq!(stall_bound(), Some(DEFAULT_STALL_BOUND));
    assert_eq!(startup_timeout(), DEFAULT_STARTUP_TIMEOUT);
}

#[test]
fn directory_header_percent_encodes() {
    assert_eq!(
        encode_directory("/home/u/my project"),
        "/home/u/my%20project"
    );
    assert_eq!(encode_directory("/plain/path"), "/plain/path");
}

/// The family payloads of one v2 frame, without the child-tree observation
/// that rides beside them.
fn family_frames(event: Value, tools: &mut HashMap<V2ToolKey, String>) -> Vec<Value> {
    normalize_v2_frame(event, tools)
        .into_iter()
        .filter(|payload| payload["type"] != OBSERVE)
        .collect()
}

#[test]
fn v2_frames_normalize_to_v1_payloads() {
    let mut tools = HashMap::new();
    // Lifecycle: busy on start, idle on the terminal frames.
    let out = family_frames(
        json!({"id":"evt_1","type":"session.execution.started","data":{"sessionID":"ses_1"}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![json!({"type":"session.status","properties":{
            "sessionID":"ses_1","status":{"type":"busy"}}})]
    );
    for kind in [
        "session.execution.succeeded",
        "session.execution.interrupted",
    ] {
        let out = family_frames(
            json!({"id":"evt_2","type":kind,"data":{"sessionID":"ses_1"}}),
            &mut tools,
        );
        // The turn's accounting rides ahead of its terminal frame.
        assert_eq!(
            out,
            vec![
                json!({"type":"luna.turn.ended","properties":{"sessionID":"ses_1"}}),
                json!({"type":if kind == "session.execution.interrupted" { "session.interrupted" } else { "session.idle" },"properties":{"sessionID":"ses_1"}})
            ]
        );
    }
    // Terminal failure = error chip + idle (both shapes captured live).
    let out = family_frames(
        json!({"id":"evt_3","type":"session.execution.failed","data":{
            "sessionID":"ses_1","error":{"type":"provider.auth","message":""}}}),
        &mut tools,
    );
    assert_eq!(out.len(), 3);
    assert_eq!(out[0]["type"], "luna.turn.ended");
    // Empty message (live shape) falls back to the error type.
    assert_eq!(
        out[1],
        json!({"type":"session.error","properties":{
            "sessionID":"ses_1",
            "error":{"name":"provider.auth","data":{"message":"provider.auth"}}}}),
    );
    // The interrupt's step-level echo is NOT a provider error.
    assert!(
        family_frames(
            json!({"id":"evt_4","type":"session.step.failed","data":{
            "sessionID":"ses_1","error":{"type":"aborted","message":"Step interrupted"}}}),
            &mut tools,
        )
        .is_empty()
    );
    // Streaming: the step registers its assistant message, then text flows
    // as a part delta keyed by message + kind + ordinal.
    let out = family_frames(
        json!({"id":"evt_5","type":"session.step.started","data":{
            "sessionID":"ses_1","assistantMessageID":"msg_a",
            "model":{"providerID":"opencode","id":"muse","variant":"low"}}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![
            json!({"type":"luna.step.started","properties":{
                "sessionID":"ses_1","messageID":"msg_a","model":"opencode/muse","variant":"low"}}),
            json!({"type":"message.updated","properties":{
                "info":{"sessionID":"ses_1","id":"msg_a","role":"assistant"}}})
        ]
    );
    let out = family_frames(
        json!({"id":"evt_6","type":"session.text.delta","data":{
            "sessionID":"ses_1","assistantMessageID":"msg_a","ordinal":0,"delta":"hi"}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![json!({"type":"message.part.delta","properties":{
            "sessionID":"ses_1","messageID":"msg_a","partID":"msg_a:t0",
            "field":"text","delta":"hi"}})]
    );
    // A tool: the NAME rides input.started; later frames carry only ids.
    family_frames(
        json!({"id":"evt_7","type":"session.tool.input.started","data":{
            "sessionID":"ses_1","assistantMessageID":"msg_a","id":"call_1","name":"read"}}),
        &mut tools,
    );
    let out = family_frames(
        json!({"id":"evt_8","type":"session.tool.called","data":{
            "sessionID":"ses_1","assistantMessageID":"msg_a","id":"call_1",
            "input":{"path":"/tmp/x"}}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![json!({"type":"message.part.updated","properties":{"part":{
            "sessionID":"ses_1","messageID":"msg_a","id":"ses_1:msg_a:call_1","callID":"ses_1:msg_a:call_1",
            "type":"tool","tool":"read",
            "state":{"status":"running","input":{"path":"/tmp/x"}}}}})]
    );
    // Usage totals are running totals for accounting, never a synthetic
    // assistant message (which used to become a fork checkpoint).
    let out = family_frames(
        json!({"id":"evt_9","type":"session.usage.updated","data":{
            "sessionID":"ses_1","cost":0,
            "tokens":{"input":10,"output":2,"reasoning":0,"cache":{"read":0,"write":0}}}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![json!({"type":"luna.usage.totals","properties":{
            "sessionID":"ses_1","cost":0,
            "tokens":{"input":10,"output":2,"reasoning":0,
                      "cache":{"read":0,"write":0}}}})]
    );
    // The permission ask keeps its 1.x name on 2.x (observed live when a
    // tool reaches outside the workspace); the permission bridge replies.
    let out = family_frames(
        json!({"id":"evt_11","type":"permission.asked","data":{
            "id":"per_1","sessionID":"ses_1","action":"external_directory",
            "resources":["/tmp/*"]}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![json!({"type":"permission.asked","properties":{
            "id":"per_1","sessionID":"ses_1","action":"external_directory","resources":["/tmp/*"]}})]
    );
    // Boilerplate frames (catalog sync etc.) drop.
    assert!(
        family_frames(
            json!({"id":"evt_10","type":"catalog.updated","data":{}}),
            &mut tools,
        )
        .is_empty()
    );
}

#[test]
fn v2_model_list_folds_into_provider_catalog() {
    let list: V2ModelList = serde_json::from_value(json!({
        "location": {"directory": "/w"},
        "data": [
            {"providerID": "opencode", "id": "muse-spark", "name": "Muse Spark",
             "limit": {"context": 1000000},
             // Live shape: variants are `{id, settings}` objects.
             "variants": [{"id": "low", "settings": {"x": 1}}, {"id": "high"}],
             "enabled": true},
            {"providerID": "opencode", "id": "plain", "enabled": true},
            {"providerID": "dead", "id": "off", "enabled": false},
        ]
    }))
    .unwrap();
    let catalog = catalog_from_v2_models(list.data);
    let models = models_from_providers(&catalog);
    assert_eq!(models.len(), 2);
    let muse = models
        .iter()
        .find(|m| m.id == "opencode/muse-spark")
        .unwrap();
    assert_eq!(muse.label, "Muse Spark");
    assert_eq!(
        muse.reasoning_levels,
        vec![ReasoningLevel::Low, ReasoningLevel::High]
    );
    // The folded catalog feeds variant picking exactly like 1.x's.
    assert_eq!(
        pick_variant(
            &catalog,
            "opencode",
            "muse-spark",
            Some(ReasoningLevel::High)
        )
        .as_deref(),
        Some("high")
    );
    assert!(models.iter().all(|m| m.id != "dead/off"));
}

#[test]
fn prompt_body_v2_carries_text_and_files_only() {
    let body = prompt_body_v2(
        "hello",
        &["/tmp/shot.png".to_owned()],
        &[],
        "msg_luna_1",
        PromptDelivery::Steer,
    );
    assert_eq!(body["id"], "msg_luna_1");
    assert_eq!(body["delivery"], "steer");
    assert_eq!(body["text"], "hello");
    assert_eq!(body["files"][0]["uri"], "file:///tmp/shot.png");
    assert_eq!(body["files"][0]["name"], "shot.png");
    assert!(
        body.get("model").is_none(),
        "model rides the session on 2.x"
    );
    assert!(body.get("parts").is_none());
}

#[tokio::test]
async fn permissions_stay_session_scoped_and_never_persist_grants() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    let ask = |owner: Option<&str>, id: &str| {
        let mut data = json!({"id":id, "action":"external_directory", "resources":["/private/*"]});
        if let Some(owner) = owner {
            data["sessionID"] = json!(owner);
        }
        json!({"type":"permission.asked", "data":data})
    };
    wire.bus.send(ask(Some("foreign"), "foreign")).unwrap();
    wire.bus.send(ask(None, "missing")).unwrap();
    // Establish an owned child before its first permission request.
    wire.bus
        .send(json!({"type":"session.created","data":{"sessionID":"child","parentID":"fixture"}}))
        .unwrap();
    wire.bus.send(ask(Some("child"), "child")).unwrap();
    wire.bus.send(ask(Some("fixture"), "own")).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if wire
                .posts
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.contains("permission"))
                .count()
                >= 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let posts = wire.posts.lock().unwrap();
    let approvals: Vec<_> = posts
        .iter()
        .filter(|(p, _)| p.contains("permission"))
        .collect();
    assert_eq!(
        approvals.len(),
        2,
        "foreign or ownerless permission was answered"
    );
    for (path, body) in approvals {
        assert_eq!(body, &json!({"decision":"once"}));
        assert!(!path.contains("foreign") && !path.contains("missing"));
    }
}

#[tokio::test]
async fn permissions_without_auto_approve_require_an_explicit_answer() {
    for accept in [false, true] {
        let answer = if accept { "Allow once" } else { "Deny" };
        let mut wire = TurnWire::start_policy(false, false, Some(answer)).await;
        wire.request("/api/model").await;
        wire.request("/prompt").await;
        wire.v2(
            "permission.asked",
            json!({"id":"approval", "sessionID":"fixture"}),
        );
        let body = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some((_, body)) = wire
                    .posts
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(p, _)| p.contains("permission"))
                {
                    break body.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(body["decision"], if accept { "once" } else { "reject" });
    }
}

#[tokio::test]
async fn task_allowance_never_persists_through_v2() {
    let mut wire = TurnWire::start_policy(false, false, Some("Allow for task")).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2(
        "permission.asked",
        json!({"id":"approval", "sessionID":"fixture"}),
    );
    let body = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some((_, body)) = wire
                .posts
                .lock()
                .unwrap()
                .iter()
                .find(|(path, _)| path.contains("permission"))
            {
                break body.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // A forged or stale task allowance fails closed: never `once`/`always`.
    assert_eq!(body["decision"], "reject");
}

#[test]
fn v2_tools_are_scoped_and_retired_and_real_failures_are_visible() {
    let mut tools = HashMap::new();
    let event = |kind, session, message, name| {
        json!({"type":kind,"data":{
            "sessionID":session,"assistantMessageID":message,"id":"same","name":name,
            "error":{"type":"permission.denied","message":"Denied"}
        }})
    };
    for (session, message, name) in [
        ("a", "m1", "read"),
        ("b", "m1", "bash"),
        ("a", "m2", "write"),
    ] {
        normalize_v2_frame(
            event("session.tool.input.started", session, message, name),
            &mut tools,
        );
    }
    for (session, message, name) in [
        ("a", "m1", "read"),
        ("b", "m1", "bash"),
        ("a", "m2", "write"),
    ] {
        let out = normalize_v2_frame(
            event("session.tool.failed", session, message, ""),
            &mut tools,
        );
        assert_eq!(out[0].pointer("/properties/part/tool").unwrap(), name);
        assert_eq!(
            out[0].pointer("/properties/part/state/status").unwrap(),
            "error"
        );
        assert_eq!(
            out[0].pointer("/properties/part/callID").unwrap(),
            &json!(format!("{session}:{message}:same"))
        );
    }
    assert!(tools.is_empty());
    normalize_v2_frame(
        event("session.tool.input.started", "a", "m1", "read"),
        &mut tools,
    );
    normalize_v2_frame(
        event("session.execution.interrupted", "a", "m1", ""),
        &mut tools,
    );
    assert!(tools.is_empty());
}

#[tokio::test]
async fn v2_reasoning_deltas_and_failed_tools_reach_the_feed() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID":"fixture"}));
    wire.v2(
        "session.step.started",
        json!({"sessionID":"fixture","assistantMessageID":"m"}),
    );
    wire.v2(
        "session.reasoning.started",
        json!({"sessionID":"fixture","assistantMessageID":"m","ordinal":0}),
    );
    wire.v2(
        "session.reasoning.delta",
        json!({"sessionID":"fixture","assistantMessageID":"m","ordinal":0,"delta":"Thinking"}),
    );
    wire.v2(
        "session.tool.input.started",
        json!({"sessionID":"fixture","assistantMessageID":"m","id":"c","name":"read"}),
    );
    wire.v2("session.tool.failed",json!({"sessionID":"fixture","assistantMessageID":"m","id":"c","error":{"message":"Denied"}}));
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID":"fixture"}),
    );
    let mut reasoning = String::new();
    let mut failed = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(3), wire.events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            AgentEvent::ReasoningDelta { text } => reasoning.push_str(&text),
            AgentEvent::ToolResult { is_error, .. } => failed |= is_error,
            AgentEvent::Done { .. } => break,
            _ => {}
        }
    }
    assert_eq!(reasoning, "Thinking");
    assert!(failed);
}

#[tokio::test]
async fn prompt_error_and_interrupt_before_busy_still_settle() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2(
        "session.execution.failed",
        json!({"sessionID":"fixture","error":{"type":"provider.invalid-request","message":"bad model"}}),
    );
    assert_eq!(wire.done().await.0, DoneStatus::Errored);

    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.interrupt.cancel();
    wire.request("/interrupt").await;
    wire.v2(
        "session.execution.interrupted",
        json!({"sessionID":"fixture","reason":"user"}),
    );
    assert_eq!(wire.done().await.0, DoneStatus::Interrupted);
    assert!(
        wire.posts
            .lock()
            .unwrap()
            .iter()
            .any(|(path, body)| path == "/api/session/fixture/interrupt" && body.is_null()),
        "interrupt carries no body"
    );
}

#[tokio::test]
async fn v2_model_selection_and_prompt_use_the_documented_bodies() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    let posts = wire.posts.lock().unwrap();
    assert_eq!(
        posts
            .iter()
            .find(|(p, _)| p == "/api/session/fixture/model")
            .unwrap()
            .1,
        json!({"model":{"providerID":"opencode","id":"muse","variant":"low"}})
    );
    let prompt = &posts
        .iter()
        .find(|(p, _)| p == "/api/session/fixture/prompt")
        .unwrap()
        .1;
    assert_eq!(prompt["text"], "first");
    assert_eq!(prompt["files"], json!([]));
    assert_eq!(prompt["delivery"], "queue");
    assert!(
        prompt["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("msg_luna_")),
        "{prompt:?}"
    );
}

#[tokio::test]
async fn v2_pending_tool_overflow_fails_the_run_instead_of_growing_forever() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    for i in 0..=MAX_PENDING_V2_TOOLS {
        wire.v2("session.tool.input.started",json!({"sessionID":"other","assistantMessageID":"m","id":format!("c{i}"),"name":"read"}));
    }
    assert_eq!(wire.done().await.0, DoneStatus::Errored);
}

#[tokio::test]
async fn v2_external_interrupt_is_not_reported_as_success() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2(
        "session.execution.interrupted",
        json!({"sessionID":"fixture","reason":"shutdown"}),
    );
    assert_eq!(wire.done().await.0, DoneStatus::Interrupted);
}

#[tokio::test]
async fn v2_recovered_step_failure_does_not_poison_successful_execution() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID":"fixture"}));
    wire.v2(
        "session.step.failed",
        json!({"sessionID":"fixture","error":{"type":"provider.rate_limit","message":"Retrying"}}),
    );
    wire.v2(
        "session.step.started",
        json!({"sessionID":"fixture","assistantMessageID":"recovered"}),
    );
    wire.v2("session.text.ended",json!({"sessionID":"fixture","assistantMessageID":"recovered","ordinal":0,"text":"Recovered"}));
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID":"fixture"}),
    );
    let (status, text) = wire.done().await;
    assert_eq!(status, DoneStatus::Completed);
    assert_eq!(text, "Recovered");
}
