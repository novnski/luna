//! Live probe (OC-11): a turn that spawns native subagents through the native
//! OpenCode 2 driver against a real binary, printing the normalized event
//! stream and a summary of the nested transcripts it would produce.
//! Arg 1 = executable (default: resolution), arg 2 = model
//! (`provider/model`), arg 3 = prompt.
//!
//!     cargo run -p luna-harness --example opencode_subagent_probe -- \
//!         ~/.opencode/bin/opencode opencode/big-pickle \
//!         "Use the subagent tool to launch one general subagent …"
//!
//! Nested subagents need `experimental.subagent_depth` ≥ 2 in the (isolated)
//! OpenCode config. Controls:
//! - `PROBE_STOP_CHILD_AFTER_MS=<ms>`: once the first child streams, wait
//!   the delay, then call `Harness::stop_native_child` for its chip (the
//!   per-child Stop) and print the answer.
//! - `PROBE_INTERRUPT_AFTER_MS=<ms>`: press the root Stop after the delay.
//! - `PROBE_INTERRUPT_AFTER_SPAWN_MS=<ms>`: press the root Stop that long
//!   after the first root spawn chip appears (model latency independent).
//!
//! The probe drains until the adapter closes the stream, so a parked run
//! (background work after the root Done) and its wake turns are observed.
//! Exit 0 when a spawn chip received tagged traffic and a tagged Done, the
//! root settled, and the stream closed.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use futures::StreamExt;
use luna_harness::{CancellationToken, Harness, OpencodeHarness, RunControls};
use luna_proto::{AgentEvent, RunRequest, SandboxLevel, UserInputAnswer};
use tokio::sync::{mpsc, oneshot};

#[derive(Default, Debug)]
struct Chip {
    /// The chip that owns this spawn call (None = the root transcript).
    owner: Option<String>,
    name: String,
    tagged: u32,
    tools: u32,
    text_bytes: usize,
    statuses: Vec<String>,
    progress: Vec<String>,
    done: Option<String>,
}

fn short(event: &AgentEvent) -> String {
    match event {
        AgentEvent::TextDelta { text } => format!("TextDelta({:?})", text.trim_end()),
        AgentEvent::ReasoningDelta { text } => format!("ReasoningDelta({}B)", text.len()),
        AgentEvent::ToolCall { id, call } => format!("ToolCall({id}, {call:?})"),
        AgentEvent::ToolResult {
            id,
            is_error,
            output,
            ..
        } => format!(
            "ToolResult({id}, error={is_error}, {}B)",
            output.as_deref().map_or(0, str::len)
        ),
        other => format!("{other:?}"),
    }
}

#[tokio::main]
async fn main() {
    let exe = std::env::args().nth(1);
    let model = std::env::args().nth(2).filter(|m| m.contains('/'));
    let prompt = std::env::args().nth(3).unwrap_or_else(|| {
        "Use the subagent tool to launch ONE general subagent with description \
         'Probe child' and prompt: 'Run the shell command `echo CHILD-A` and reply \
         with its output.'. Then reply with the subagent's result."
            .into()
    });
    let workspace = tempfile::tempdir().expect("create isolated probe workspace");
    let cwd = workspace.path().to_str().expect("UTF-8 workspace path");
    let delay = |key: &str| {
        std::env::var(key)
            .ok()
            .and_then(|ms| ms.parse::<u64>().ok())
            .map(std::time::Duration::from_millis)
    };
    let stop_child_after = delay("PROBE_STOP_CHILD_AFTER_MS");
    let interrupt_after = delay("PROBE_INTERRUPT_AFTER_MS");
    let interrupt_after_spawn = delay("PROBE_INTERRUPT_AFTER_SPAWN_MS");
    let (_steer_tx, steering) = mpsc::channel(8);
    let request = RunRequest {
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
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    };
    let interrupt = CancellationToken::new();
    let root_stop = interrupt.clone();
    let mut root_stop_armed = false;
    if let Some(after) = interrupt_after {
        let interrupt = interrupt.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            eprintln!("--- pressing root Stop");
            interrupt.cancel();
        });
    }
    let mut harness = OpencodeHarness::new();
    if let Some(exe) = exe {
        harness = harness.with_executable(exe);
    }
    let harness = Arc::new(harness);
    let started = std::time::Instant::now();
    let mut stream = harness
        .run(
            request,
            RunControls {
                request_input: Box::new(|questions| {
                    // Auto-approve is on; answer anything else with its first
                    // option so the probe never blocks.
                    let (tx, rx) = oneshot::channel();
                    let _ = tx.send(
                        questions
                            .iter()
                            .map(|q| UserInputAnswer {
                                question_id: q.id.clone(),
                                labels: q.options.first().cloned().into_iter().collect(),
                            })
                            .collect(),
                    );
                    rx
                }),
                steering,
                interrupt,
                invoke_luna_tool: None,
            },
        )
        .await
        .expect("run starts");

    let mut root_session: Option<String> = None;
    let mut chips: BTreeMap<String, Chip> = BTreeMap::new();
    let mut root_dones: Vec<String> = Vec::new();
    let mut provider_turns = 0u32;
    let mut native_tasks: Vec<String> = Vec::new();
    let mut stop_sent: HashSet<String> = HashSet::new();
    let (stop_tx, mut stop_rx) = mpsc::unbounded_channel::<String>();
    let closed = loop {
        let next = tokio::select! {
            next = stream.next() => next,
            Some(line) = stop_rx.recv() => {
                eprintln!("{line}");
                continue;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(240)) => {
                eprintln!("--- timed out after 240 s of silence");
                break false;
            }
        };
        let Some(event) = next else { break true };
        let t = started.elapsed().as_millis();
        match event {
            Ok(AgentEvent::SessionStarted { session_id, .. }) => {
                eprintln!("[{t:>6}] ROOT SessionStarted({session_id})");
                root_session = Some(session_id);
            }
            Ok(AgentEvent::Subagent {
                parent_tool_use_id,
                event,
            }) => {
                eprintln!("[{t:>6}] SUB[{parent_tool_use_id}] {}", short(&event));
                // A spawn inside a child: its chip belongs to that child.
                if let AgentEvent::ToolCall { id, call } = event.as_ref()
                    && call.is_subagent_spawn()
                {
                    let chip = chips.entry(id.clone()).or_default();
                    chip.owner = Some(parent_tool_use_id.clone());
                    chip.name = format!("{call:?}");
                }
                let chip = chips.entry(parent_tool_use_id.clone()).or_default();
                chip.tagged += 1;
                match event.as_ref() {
                    AgentEvent::ToolCall { .. } => chip.tools += 1,
                    AgentEvent::TextDelta { text } => chip.text_bytes += text.len(),
                    AgentEvent::ChildStatus { status } => chip.statuses.push(format!("{status:?}")),
                    AgentEvent::ChildProgress { .. } => chip.progress.push(short(&event)),
                    AgentEvent::Done { status, error, .. } => {
                        chip.done = Some(format!("{status:?} {error:?}"))
                    }
                    _ => {}
                }
                if let (Some(after), Some(session)) = (stop_child_after, root_session.clone())
                    && stop_sent.is_empty()
                    && chip.done.is_none()
                {
                    stop_sent.insert(parent_tool_use_id.clone());
                    let harness = harness.clone();
                    let spawn = parent_tool_use_id.clone();
                    let report = stop_tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(after).await;
                        let _ = report.send(format!("--- stop_native_child({spawn}) requested"));
                        let answer = harness.stop_native_child(&session, &spawn).await;
                        let _ =
                            report.send(format!("--- stop_native_child({spawn}) -> {answer:?}"));
                        // A second stop of the same chip after it settles must be refused.
                        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                        let again = harness.stop_native_child(&session, &spawn).await;
                        let _ = report
                            .send(format!("--- stop_native_child({spawn}) again -> {again:?}"));
                        let bogus = harness.stop_native_child(&session, "not-a-spawn").await;
                        let _ =
                            report.send(format!("--- stop_native_child(not-a-spawn) -> {bogus:?}"));
                    });
                }
            }
            Ok(AgentEvent::ToolCall { id, call }) => {
                eprintln!("[{t:>6}] ROOT ToolCall({id}, {call:?})");
                if call.is_subagent_spawn() {
                    chips.entry(id.clone()).or_default().name = format!("{call:?}");
                    if let Some(after) = interrupt_after_spawn
                        && !root_stop_armed
                    {
                        root_stop_armed = true;
                        let root_stop = root_stop.clone();
                        let report = stop_tx.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(after).await;
                            let _ = report.send("--- pressing root Stop (after spawn)".into());
                            root_stop.cancel();
                        });
                    }
                }
            }
            Ok(AgentEvent::Done { status, error, .. }) => {
                eprintln!("[{t:>6}] ROOT Done({status:?}, {error:?})");
                root_dones.push(format!("{status:?}"));
            }
            Ok(AgentEvent::ProviderTurnStarted { thread_id, turn_id }) => {
                provider_turns += 1;
                eprintln!("[{t:>6}] ROOT ProviderTurnStarted({thread_id}, {turn_id})");
            }
            Ok(AgentEvent::NativeTaskChanged { task }) => {
                eprintln!(
                    "[{t:>6}] ROOT NativeTaskChanged({:?} {} {:?} tool={:?} {:?})",
                    task.kind, task.task_id, task.status, task.tool_use_id, task.description
                );
                native_tasks.push(format!("{}:{:?}", task.task_id, task.status));
            }
            Ok(other) => eprintln!("[{t:>6}] ROOT {}", short(&other)),
            Err(e) => eprintln!("[{t:>6}] ERR {e}"),
        }
    };
    while let Ok(line) = stop_rx.try_recv() {
        eprintln!("{line}");
    }
    eprintln!("=== summary ({:?})", started.elapsed());
    eprintln!(
        "root Done: {root_dones:?}; provider turns: {provider_turns}; stream closed: {closed}"
    );
    eprintln!("native tasks: {native_tasks:?}");
    for (id, chip) in &chips {
        eprintln!(
            "chip {id}\n  owner={:?} name={}\n  tagged={} tools={} text={}B statuses={:?}\n  progress={:?}\n  done={:?}",
            chip.owner,
            chip.name,
            chip.tagged,
            chip.tools,
            chip.text_bytes,
            chip.statuses,
            chip.progress,
            chip.done
        );
    }
    // A bound, settled child, or (background-shell probes) a settled row.
    let bound = chips.values().any(|c| c.tagged > 1 && c.done.is_some());
    let settled_task = native_tasks.iter().any(|t| !t.ends_with(":Running"));
    if !((bound || settled_task) && !root_dones.is_empty() && closed) {
        std::process::exit(1);
    }
}
