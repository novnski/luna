//! Live probe: SIGKILL the run's `opencode serve` while a permission ask (or
//! a form) is pending, then continue the same OpenCode session on a fresh
//! server — the S4X reconnect case at the harness level.
//!
//!     cargo run -p luna-harness --example opencode_server_kill_probe -- \
//!         <profile>/bin/opencode opencode/big-pickle
//!
//! Environment:
//! - `PROBE_KIND`: `approval` (default: a shell command under the ask
//!   policy) or `form` (the question tool).
//! - `PROBE_NONCE`: the marker the prompts carry (default: random).
//! - `PROBE_OUT`: where to write the JSON summary of both turns.
//!
//! Exit 0 only when:
//! - the killed turn had an input pending at the kill, ended (not Completed)
//!   and showed every error text once (Error events plus Done's error);
//! - the continuation turn (same session, new server) completed with the
//!   marker reply and carried no chip of the killed turn (no tool event for
//!   a killed call, no nameless unknown call).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use luna_harness::{CancellationToken, Harness, OpencodeHarness, RunControls};
use luna_proto::{
    AgentEvent, DoneStatus, PermissionMode, PermissionPolicy, RunRequest, SandboxLevel, ToolCall,
    UserInputAnswer, UserInputQuestion,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn request(prompt: String, model: Option<String>, cwd: &str, resume: Option<String>) -> RunRequest {
    let policy = PermissionPolicy {
        requested_mode: PermissionMode::Ask,
        effective_mode: PermissionMode::Ask,
        machine_name: "probe-machine".into(),
        ..Default::default()
    };
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt,
        harness: None,
        provider_account_id: None,
        model,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: cwd.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: Some(policy),
        attachments: Vec::new(),
        resume,
        worktree: None,
    }
}

/// The `opencode serve` processes this probe spawned (direct children: the
/// QA profile wrapper `exec`s the binary).
fn server_pids() -> Vec<(u32, String)> {
    let me = std::process::id();
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,command="])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid: u32 = fields.next()?.parse().ok()?;
            let ppid: u32 = fields.next()?.parse().ok()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            (ppid == me && command.contains(" serve")).then_some((pid, command))
        })
        .collect()
}

#[derive(Default)]
struct Turn {
    events: Vec<Value>,
    errors: Vec<String>,
    tool_ids: Vec<String>,
    text: String,
    status: Option<DoneStatus>,
    session_id: Option<String>,
}

fn record(turn: &mut Turn, event: &AgentEvent) {
    let line = match event {
        AgentEvent::TextDelta { text } => {
            turn.text.push_str(text);
            json!({"event": "text", "text": text})
        }
        AgentEvent::ReasoningDelta { .. } => return,
        AgentEvent::ContextUsage { .. } | AgentEvent::Usage { .. } => return,
        AgentEvent::ToolCall { id, call } => {
            turn.tool_ids.push(id.clone());
            let (kind, name) = match call {
                ToolCall::Unknown { name, .. } => ("unknown", name.clone()),
                ToolCall::Exec { .. } => ("exec", String::new()),
                other => ("other", format!("{other:?}").chars().take(60).collect()),
            };
            json!({"event": "toolCall", "id": id, "kind": kind, "name": name})
        }
        AgentEvent::ToolResult { id, is_error, .. } => {
            json!({"event": "toolResult", "id": id, "isError": is_error})
        }
        AgentEvent::Error { message } => {
            turn.errors.push(message.clone());
            json!({"event": "error", "message": message})
        }
        AgentEvent::Done {
            status,
            error,
            session_id,
            ..
        } => {
            turn.status = Some(*status);
            if let Some(error) = error {
                turn.errors.push(error.clone());
            }
            turn.session_id = session_id.clone().or(turn.session_id.take());
            json!({"event": "done", "status": format!("{status:?}"), "error": error})
        }
        AgentEvent::SessionStarted { session_id, .. } => {
            turn.session_id = Some(session_id.clone());
            json!({"event": "sessionStarted", "sessionId": session_id})
        }
        AgentEvent::InputRequested { request_id, .. } => {
            json!({"event": "inputRequested", "requestId": request_id})
        }
        AgentEvent::InputResolved { request_id, .. } => {
            json!({"event": "inputResolved", "requestId": request_id})
        }
        AgentEvent::ProviderEvidence {
            kind, native_id, ..
        } => json!({"event": "evidence", "kind": kind, "nativeId": native_id}),
        other => {
            let debug = format!("{other:?}");
            let name = debug
                .split([' ', '{', '('])
                .next()
                .unwrap_or_default()
                .to_owned();
            json!({"event": name})
        }
    };
    eprintln!("EV {line}");
    turn.events.push(line);
}

async fn drive(
    stream: &mut (impl futures::Stream<Item = Result<AgentEvent, luna_harness::HarnessError>> + Unpin),
    turn: &mut Turn,
    limit: Duration,
) {
    loop {
        match tokio::time::timeout(limit, stream.next()).await {
            Ok(Some(Ok(event))) => {
                let done = matches!(event, AgentEvent::Done { .. });
                record(turn, &event);
                if done {
                    break;
                }
            }
            Ok(Some(Err(error))) => eprintln!("ERR {error}"),
            Ok(None) => break,
            Err(_) => {
                eprintln!("--- timed out waiting for the turn");
                break;
            }
        }
    }
    // The adapter's teardown (lease release, SIGTERM) completes before the
    // next run starts.
    let drained = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(event) = stream.next().await {
            if let Ok(event) = event {
                record(turn, &event);
            }
        }
    })
    .await;
    eprintln!("--- stream closed after teardown: {}", drained.is_ok());
}

/// Every error text appears once across the turn's Error events and Done.
fn duplicate_errors(errors: &[String]) -> Vec<String> {
    let mut seen: Vec<&String> = Vec::new();
    let mut dup = Vec::new();
    for error in errors {
        if seen.contains(&error) {
            dup.push(error.clone());
        } else {
            seen.push(error);
        }
    }
    dup
}

#[tokio::main]
async fn main() {
    let exe = std::env::args().nth(1);
    let model = std::env::args().nth(2).filter(|m| m.contains('/'));
    let kind = env("PROBE_KIND").unwrap_or_else(|| "approval".into());
    let nonce = env("PROBE_NONCE")
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()[..8].to_uppercase());
    let workspace = tempfile::tempdir().expect("create isolated probe workspace");
    let cwd = workspace
        .path()
        .to_str()
        .expect("UTF-8 workspace")
        .to_owned();
    let first_prompt = if kind == "form" {
        format!(
            "Use your tool for asking the user a multiple-choice question (do not ask in plain \
             text). Ask me to choose exactly one color from these options: red, green, blue. \
             After I answer, reply with exactly KILL-{nonce}-<the color I chose>."
        )
    } else {
        format!(
            "Use your shell tool to run exactly this command: echo KILL-{nonce}\nThen reply \
             with only the command's output. Do not run any other command."
        )
    };
    let after_marker = format!("AFTER-{nonce}");
    let second_prompt = format!("Reply with exactly {after_marker}. Do not use tools.");
    eprintln!("--- kind={kind} nonce={nonce} cwd={cwd} model={model:?}");

    // Turn 1: the first input request kills the server; the request is
    // never answered (its sender is held, so it is not a dismissal).
    let held: Arc<Mutex<Vec<oneshot::Sender<Vec<UserInputAnswer>>>>> = Arc::default();
    let killed: Arc<Mutex<Option<Value>>> = Arc::default();
    let request_input = {
        let held = Arc::clone(&held);
        let killed = Arc::clone(&killed);
        Box::new(move |questions: Vec<UserInputQuestion>| {
            let (tx, rx) = oneshot::channel();
            let approval = questions.iter().any(|q| q.approval.is_some());
            eprintln!("INPUT {} question(s) approval={approval}", questions.len());
            held.lock().unwrap().push(tx);
            let killed = Arc::clone(&killed);
            std::thread::spawn(move || {
                // Let the card settle as the user would see it.
                std::thread::sleep(Duration::from_millis(1500));
                let mut slot = killed.lock().unwrap();
                if slot.is_some() {
                    return;
                }
                let servers = server_pids();
                for (pid, _) in &servers {
                    let _ = std::process::Command::new("kill")
                        .args(["-9", &pid.to_string()])
                        .status();
                }
                eprintln!("--- SIGKILL {servers:?} (approval pending={approval})");
                *slot = Some(json!({
                    "pendingApproval": approval,
                    "servers": servers.iter().map(|(pid, command)| json!({"pid": pid, "command": command})).collect::<Vec<_>>(),
                }));
            });
            rx
        })
    };
    let mut harness = OpencodeHarness::new();
    if let Some(exe) = &exe {
        harness = harness.with_executable(exe.clone());
    }
    let (_steer_tx, steering) = mpsc::channel(8);
    let mut stream = harness
        .run(
            request(first_prompt, model.clone(), &cwd, None),
            RunControls {
                request_input,
                steering,
                interrupt: CancellationToken::new(),
                invoke_luna_tool: None,
            },
        )
        .await
        .expect("first run starts");
    let mut killed_turn = Turn::default();
    drive(&mut stream, &mut killed_turn, Duration::from_secs(240)).await;
    drop(stream);
    let kill = killed.lock().unwrap().clone();

    // Turn 2: the same session on a fresh server.
    let mut continuation = Turn::default();
    if let Some(session) = killed_turn.session_id.clone() {
        let mut harness = OpencodeHarness::new();
        if let Some(exe) = &exe {
            harness = harness.with_executable(exe.clone());
        }
        let (_steer_tx, steering) = mpsc::channel(8);
        let mut stream = harness
            .run(
                request(second_prompt, model.clone(), &cwd, Some(session)),
                RunControls {
                    request_input: Box::new(|_| panic!("the continuation must not ask")),
                    steering,
                    interrupt: CancellationToken::new(),
                    invoke_luna_tool: None,
                },
            )
            .await
            .expect("continuation run starts");
        drive(&mut stream, &mut continuation, Duration::from_secs(240)).await;
    }
    if let Some(resolution) = luna_harness::opencode::last_opencode_resolution() {
        let version = resolution.version.as_deref().unwrap_or("unknown");
        eprintln!("RESOLVED {} version={version}", resolution.path.display());
    }

    let killed_duplicates = duplicate_errors(&killed_turn.errors);
    let stray: Vec<Value> = continuation
        .events
        .iter()
        .filter(|event| {
            let id = event.get("id").and_then(Value::as_str).unwrap_or_default();
            let nameless = event.get("kind").and_then(Value::as_str) == Some("unknown")
                && event.get("name").and_then(Value::as_str) == Some("");
            killed_turn.tool_ids.iter().any(|killed| killed == id) || nameless
        })
        .cloned()
        .collect();
    let checks = json!({
        "killedWithInputPending": kill.is_some(),
        "killedTurnEnded": matches!(killed_turn.status, Some(DoneStatus::Interrupted | DoneStatus::Errored)),
        "killedTurnErrorsOnce": killed_duplicates.is_empty() && !killed_turn.errors.is_empty(),
        "continuationCompleted": continuation.status == Some(DoneStatus::Completed),
        "continuationReply": continuation.text.contains(&after_marker),
        "continuationNoStrayChip": stray.is_empty() && continuation.status.is_some(),
    });
    let passed = checks
        .as_object()
        .is_some_and(|checks| checks.values().all(|v| v == &json!(true)));
    let summary = json!({
        "probe": "opencode_server_kill_probe",
        "kind": kind,
        "nonce": nonce,
        "model": model,
        "binary": luna_harness::opencode::last_opencode_resolution().map(|r| json!({
            "path": r.path.display().to_string(),
            "version": r.version,
        })),
        "kill": kill,
        "killedTurn": {
            "status": killed_turn.status.map(|s| format!("{s:?}")),
            "errors": killed_turn.errors,
            "duplicateErrors": killed_duplicates,
            "toolIds": killed_turn.tool_ids,
            "events": killed_turn.events,
        },
        "continuation": {
            "status": continuation.status.map(|s| format!("{s:?}")),
            "text": continuation.text,
            "strayChips": stray,
            // OpenCode's settlement of the killed call, kept as host-only
            // evidence instead of a chip (informational).
            "lateSettlements": continuation.events.iter()
                .filter(|event| event.get("kind").and_then(Value::as_str) == Some("native:tool.late_settlement"))
                .cloned()
                .collect::<Vec<_>>(),
            "events": continuation.events,
        },
        "checks": checks,
        "passed": passed,
    });
    if let Some(path) = env("PROBE_OUT") {
        std::fs::write(&path, serde_json::to_vec_pretty(&summary).unwrap())
            .expect("write PROBE_OUT");
        eprintln!("--- summary written to {path}");
    }
    eprintln!("--- checks {checks}");
    eprintln!("--- verdict: {}", if passed { "PASS" } else { "FAIL" });
    if !passed {
        std::process::exit(1);
    }
}
