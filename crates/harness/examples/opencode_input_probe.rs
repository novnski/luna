//! Live probe: one turn through the native driver with LUNA's input bridge
//! answered by a script — permission asks (the shared approval card) and
//! forms (the shared question panel / MCP form contract). Prints every
//! prompt LUNA would show, the scripted answer, and the event stream.
//!
//!     cargo run -p luna-harness --example opencode_input_probe -- \
//!         ~/.opencode/bin/opencode opencode/big-pickle \
//!         "Run the shell command: echo LUNA_PERMISSION_OK"
//!
//! Environment:
//! - `PROBE_MODE`: `ask` (default) or `full` — the LUNA permission mode.
//! - `PROBE_APPROVAL`: the approval label to answer (`Allow once`
//!   (default), `Allow for session`, `Deny`, `Cancel turn`).
//!   `PROBE_APPROVALS`: comma-separated labels answered in order (the last
//!   one repeats), e.g. `Allow for session,Allow once`.
//! - `PROBE_MAX_APPROVALS`: maximum approval cards the first run may show.
//! - `PROBE_RESUME_PROMPT`: a second run on the same harness that resumes
//!   the first run's session (a later turn of the same chat);
//!   `PROBE_RESUME_MAX_APPROVALS` bounds its approval cards and
//!   `PROBE_RESUME_MODE` (`ask`/`full`) sets its permission mode.
//!   `PROBE_RESUME2_PROMPT` / `_MAX_APPROVALS` / `_MIN_APPROVALS` / `_MODE`:
//!   a third run, after the second.
//! - `PROBE_CHOICE`: the option index (0-based) chosen on question panels
//!   (default 0); `PROBE_TYPED=<text>` answers with typed text instead;
//!   `PROBE_DISMISS=1` dismisses (empty answers).
//! - `PROBE_FIELDS`: a JSON object answering typed (MCP contract) fields by
//!   name, e.g. `{"age":"42","tags":["A","B"]}`; `PROBE_MCP_ACTION`: the
//!   form action (`Accept` (default), `Decline`, `Cancel`).
//! - `PROBE_EXPECT`: `completed` (default; needs non-empty text),
//!   `interrupted`, or `any`.
//! - `PROBE_EXPECT_TEXT`: a substring the final text must contain.
//! - `PROBE_MIN_APPROVALS` / `PROBE_MIN_QUESTIONS`: minimum prompts seen.
//! - `PROBE_CWD`: the run's workspace (default: a fresh temp dir).
//! - `PROBE_FOLLOWUP`: a second prompt queued as a follow-up turn of the
//!   same run (delivered when the first turn ends).
//! - `PROBE_ANSWER_DELAY_MS`: answer every prompt this long after it was
//!   shown (a user who reads before answering; e.g. 7000 lets LUNA's
//!   pooled connection pass the server's 6 s keep-alive close).
//! - `PROBE_IDLE_TIMEOUT_SECS`: fail (exit 2) when the run emits nothing for
//!   this long (default 180): a turn left waiting on a prompt nobody sees.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use luna_harness::{
    CancellationToken, Harness, OpencodeHarness, RunControls, SteerDelivery, SteerMessage,
};
use luna_proto::{
    AgentEvent, PermissionMode, PermissionPolicy, RunRequest, SandboxLevel, UserInputAnswer,
    UserInputQuestion,
};
use tokio::sync::{mpsc, oneshot};

#[derive(Default)]
struct Seen {
    approvals: u32,
    questions: u32,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn describe(question: &UserInputQuestion) {
    eprintln!("  [{}] {}", question.id, question.header);
    for line in question.question.lines() {
        eprintln!("    | {line}");
    }
    if !question.options.is_empty() {
        eprintln!(
            "    options: {:?}{}",
            question.options,
            if question.multi_select {
                " (multi)"
            } else {
                ""
            }
        );
    }
    if let Some(input) = &question.input {
        eprintln!(
            "    input: allow_other={} descriptions={:?}",
            input.allow_other, input.option_descriptions
        );
    }
    if let Some(mcp) = &question.mcp {
        eprintln!(
            "    mcp: server={} request={} thread={} kind={:?} launch_url_held={}",
            mcp.server_name,
            mcp.provider_request_id,
            mcp.thread_id,
            mcp.kind,
            mcp.launch_url.is_some()
        );
    }
    if let Some(approval) = &question.approval {
        eprintln!(
            "    approval: kind={:?} summary={:?} expires={:?} task_allowance={}",
            approval.action_kind,
            approval.summary,
            approval.expires(),
            approval.supports_task_allowance
        );
        if let Some(details) = &approval.details {
            for (label, value) in details.display_rows() {
                eprintln!("    detail: {label}: {}", value.replace('\n', " / "));
            }
            let choices: Vec<_> = details.choices.iter().map(|c| c.label.as_str()).collect();
            eprintln!("    choices: {choices:?}");
        }
    }
}

#[tokio::main]
async fn main() {
    let exe = std::env::args().nth(1);
    let model = std::env::args().nth(2).filter(|m| m.contains('/'));
    let prompt = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "Run the shell command: echo LUNA_PERMISSION_OK".into());
    let temp = tempfile::tempdir().expect("create isolated probe workspace");
    let cwd = env("PROBE_CWD").unwrap_or_else(|| temp.path().to_str().unwrap().to_owned());
    let mode = match env("PROBE_MODE").as_deref() {
        Some("full") => PermissionMode::Full,
        _ => PermissionMode::Ask,
    };
    let approval_label = env("PROBE_APPROVAL").unwrap_or_else(|| "Allow once".into());
    let approval_labels: Vec<String> = env("PROBE_APPROVALS")
        .map(|labels| labels.split(',').map(|l| l.trim().to_owned()).collect())
        .unwrap_or_else(|| vec![approval_label.clone()]);
    let max_approvals: Option<u32> = env("PROBE_MAX_APPROVALS").and_then(|n| n.parse().ok());
    // Later runs resuming the first run's session: (prompt, mode, min, max).
    let resumes: Vec<(String, PermissionMode, u32, Option<u32>)> = ["RESUME", "RESUME2"]
        .iter()
        .map_while(|key| {
            let prompt = env(&format!("PROBE_{key}_PROMPT"))?;
            let mode = match env(&format!("PROBE_{key}_MODE")).as_deref() {
                Some("full") => PermissionMode::Full,
                Some("ask") => PermissionMode::Ask,
                _ => mode,
            };
            let min = env(&format!("PROBE_{key}_MIN_APPROVALS"))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let max = env(&format!("PROBE_{key}_MAX_APPROVALS")).and_then(|n| n.parse().ok());
            Some((prompt, mode, min, max))
        })
        .collect();
    let choice: usize = env("PROBE_CHOICE")
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let typed = env("PROBE_TYPED");
    let dismiss = env("PROBE_DISMISS").is_some();
    let fields: Option<serde_json::Map<String, serde_json::Value>> = env("PROBE_FIELDS")
        .map(|json| serde_json::from_str(&json).expect("PROBE_FIELDS is a JSON object"));
    let mcp_action = env("PROBE_MCP_ACTION").unwrap_or_else(|| "Accept".into());
    let expect = env("PROBE_EXPECT").unwrap_or_else(|| "completed".into());
    let expect_text = env("PROBE_EXPECT_TEXT");
    let min_approvals: u32 = env("PROBE_MIN_APPROVALS")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let min_questions: u32 = env("PROBE_MIN_QUESTIONS")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let answer_delay = std::time::Duration::from_millis(
        env("PROBE_ANSWER_DELAY_MS")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0),
    );
    let idle_timeout = std::time::Duration::from_secs(
        env("PROBE_IDLE_TIMEOUT_SECS")
            .and_then(|n| n.parse().ok())
            .unwrap_or(180),
    );

    let policy = PermissionPolicy {
        requested_mode: mode,
        effective_mode: mode,
        machine_name: "probe-machine".into(),
        ..Default::default()
    };
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
        cwd: cwd.clone(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: mode == PermissionMode::Full,
        permission_mode: mode,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: Some(policy),
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    };
    eprintln!(
        "--- mode={mode:?} cwd={cwd} approvals={approval_labels:?} choice={choice} typed={typed:?} dismiss={dismiss}"
    );

    let seen = Arc::new(Mutex::new(Seen::default()));
    let seen_input = Arc::clone(&seen);
    let answer_input = Arc::new(move |questions: Vec<UserInputQuestion>| {
        let (tx, rx) = oneshot::channel();
        eprintln!("INPUT {} question(s)", questions.len());
        let mut answers = Vec::new();
        for question in &questions {
            describe(question);
            let labels: Vec<String> = if question.approval.is_some() {
                let mut seen = seen_input.lock().unwrap();
                let label = approval_labels
                    .get(seen.approvals as usize)
                    .or(approval_labels.last())
                    .cloned()
                    .unwrap_or_default();
                seen.approvals += 1;
                vec![label]
            } else if let Some(mcp) = &question.mcp {
                seen_input.lock().unwrap().questions += 1;
                // MCP forms: PROBE_FIELDS[name] (a string or a list of
                // labels), else the indexed option / typed text; the action
                // question answers PROBE_MCP_ACTION (default Accept).
                match &mcp.kind {
                    luna_proto::McpInputKind::Field { name, .. } => {
                        match fields.as_ref().and_then(|f| f.get(name)) {
                            Some(serde_json::Value::Array(items)) => items
                                .iter()
                                .filter_map(|item| item.as_str().map(str::to_owned))
                                .collect(),
                            Some(serde_json::Value::String(text)) => vec![text.clone()],
                            Some(_) => Vec::new(),
                            None => typed
                                .clone()
                                .or_else(|| question.options.get(choice).cloned())
                                .into_iter()
                                .collect(),
                        }
                    }
                    _ => vec![mcp_action.clone()],
                }
            } else {
                seen_input.lock().unwrap().questions += 1;
                typed
                    .clone()
                    .or_else(|| question.options.get(choice).cloned())
                    .into_iter()
                    .collect()
            };
            if dismiss && question.approval.is_none() {
                continue;
            }
            if !labels.is_empty() {
                eprintln!("    -> answer {labels:?}");
                answers.push(UserInputAnswer {
                    question_id: question.id.clone(),
                    labels,
                });
            }
        }
        if dismiss {
            eprintln!("    -> dismissed (empty answers)");
        }
        if answer_delay.is_zero() {
            let _ = tx.send(answers);
        } else {
            eprintln!("    -> answering in {answer_delay:?}");
            tokio::spawn(async move {
                tokio::time::sleep(answer_delay).await;
                eprintln!("    -> answered after {answer_delay:?}");
                let _ = tx.send(answers);
            });
        }
        rx
    });
    let input_for_resume = Arc::clone(&answer_input);
    let request_input = Box::new(move |questions| (answer_input)(questions));

    let (steer_tx, steering) = mpsc::channel(8);
    // A follow-up turn in the same run (a fresh execution, so tools that
    // registered after the first prompt — late MCP servers — are offered).
    if let Some(prompt) = env("PROBE_FOLLOWUP") {
        steer_tx
            .send(SteerMessage {
                prompt,
                attachments: Vec::new(),
                input_items: Vec::new(),
                message_id: None,
                delivery: SteerDelivery::AllowFollowUpTurn,
                acceptance: None,
                goal: None,
            })
            .await
            .expect("queue the follow-up turn");
    }
    let started = std::time::Instant::now();
    let mut harness = OpencodeHarness::new();
    if let Some(exe) = exe {
        harness = harness.with_executable(exe);
    }
    let mut stream = harness
        .run(
            request.clone(),
            RunControls {
                request_input,
                steering,
                interrupt: CancellationToken::new(),
                invoke_luna_tool: None,
            },
        )
        .await
        .expect("run starts");
    let mut text = String::new();
    let mut tools = 0u32;
    let mut subagent_events = 0u32;
    let mut session_id: Option<String> = None;
    let status = loop {
        let ev = match tokio::time::timeout(idle_timeout, stream.next()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => break None,
            Err(_) => {
                eprintln!("--- timed out: no event for {idle_timeout:?}");
                std::process::exit(2);
            }
        };
        match ev {
            Ok(AgentEvent::Done { status, error, .. }) => {
                if let Some(error) = error {
                    eprintln!("ERR {error}");
                }
                break Some(status);
            }
            Ok(AgentEvent::TextDelta { text: delta }) => {
                text.push_str(&delta);
                eprintln!("TXT {delta}");
            }
            Ok(AgentEvent::ReasoningDelta { text }) => eprintln!("THK {}", text.trim_end()),
            Ok(AgentEvent::ToolCall { id, call }) => {
                tools += 1;
                eprintln!("TOOL {id} {call:?}");
            }
            Ok(AgentEvent::ToolResult {
                id,
                is_error,
                output,
                ..
            }) => {
                let output = output.unwrap_or_default();
                let clipped: String = output.chars().take(300).collect();
                eprintln!("RESULT {id} error={is_error} {clipped:?}");
            }
            Ok(AgentEvent::Subagent {
                parent_tool_use_id,
                event,
            }) => {
                subagent_events += 1;
                let line = format!("{event:?}");
                let clipped: String = line.chars().take(240).collect();
                eprintln!("SUB {parent_tool_use_id} {clipped}");
            }
            Ok(AgentEvent::Error { message }) => eprintln!("CHIP {message}"),
            Ok(AgentEvent::ContextUsage { .. }) => {}
            Ok(AgentEvent::SessionStarted {
                session_id: started,
                ..
            }) => {
                eprintln!("SESSION {started}");
                session_id = Some(started);
            }
            Ok(other) => eprintln!("EV {other:?}"),
            Err(e) => eprintln!("ERR {e}"),
        }
    };
    let (approvals, questions) = {
        let seen = seen.lock().unwrap();
        (seen.approvals, seen.questions)
    };
    eprintln!(
        "--- done: {status:?} text={text:?} tools={tools} approvals={approvals} \
         questions={questions} subagent_events={subagent_events}"
    );
    let drained = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while stream.next().await.is_some() {}
    })
    .await;
    eprintln!(
        "--- stream closed after teardown: {} ({:?} since start)",
        drained.is_ok(),
        started.elapsed()
    );
    if let Some(resolution) = luna_harness::opencode::last_opencode_resolution() {
        let version = resolution.version.as_deref().unwrap_or("unknown");
        eprintln!("RESOLVED {} version={version}", resolution.path.display());
        if let Some(report) = luna_harness::opencode::opencode_compat_report(version) {
            eprintln!("COMPAT {report:?}");
        }
    }
    let status_ok = match expect.as_str() {
        "interrupted" => status == Some(luna_proto::DoneStatus::Interrupted),
        "any" => status.is_some(),
        _ => status == Some(luna_proto::DoneStatus::Completed) && !text.trim().is_empty(),
    };
    let text_ok = expect_text
        .as_deref()
        .is_none_or(|needle| text.to_lowercase().contains(&needle.to_lowercase()));
    let counts_ok = approvals >= min_approvals
        && questions >= min_questions
        && max_approvals.is_none_or(|max| approvals <= max);
    // Later turns of the same chat: new runs on the same harness resume the
    // session (session allowances must hold while the mode is unchanged).
    let mut resume_ok = true;
    for (index, (prompt, resume_mode, min, max)) in resumes.into_iter().enumerate() {
        let Some(session) = session_id.clone() else {
            resume_ok = false;
            break;
        };
        let before = seen.lock().unwrap().approvals;
        eprintln!(
            "--- resume run {} on {session} mode={resume_mode:?}: {prompt:?}",
            index + 1
        );
        let mut resumed = request.clone();
        resumed.prompt = prompt;
        resumed.resume = Some(session);
        resumed.permission_mode = resume_mode;
        resumed.auto_approve = resume_mode == PermissionMode::Full;
        resumed.permission_policy = Some(PermissionPolicy {
            requested_mode: resume_mode,
            effective_mode: resume_mode,
            machine_name: "probe-machine".into(),
            ..Default::default()
        });
        let (_resume_tx, resume_steering) = mpsc::channel(1);
        let input = Arc::clone(&input_for_resume);
        let mut stream = harness
            .run(
                resumed,
                RunControls {
                    request_input: Box::new(move |questions| (input)(questions)),
                    steering: resume_steering,
                    interrupt: CancellationToken::new(),
                    invoke_luna_tool: None,
                },
            )
            .await
            .expect("resume run starts");
        let mut status = None;
        while let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_secs(180), stream.next()).await
        {
            match ev {
                Ok(AgentEvent::Done {
                    status: done,
                    error,
                    ..
                }) => {
                    if let Some(error) = error {
                        eprintln!("ERR {error}");
                    }
                    status = Some(done);
                    break;
                }
                Ok(AgentEvent::TextDelta { text }) => eprintln!("TXT {text}"),
                Ok(AgentEvent::ToolCall { id, call }) => eprintln!("TOOL {id} {call:?}"),
                Ok(AgentEvent::ToolResult {
                    id,
                    is_error,
                    output,
                    ..
                }) => {
                    let clipped: String = output.unwrap_or_default().chars().take(300).collect();
                    eprintln!("RESULT {id} error={is_error} {clipped:?}");
                }
                Ok(AgentEvent::Error { message }) => eprintln!("CHIP {message}"),
                Ok(_) => {}
                Err(e) => eprintln!("ERR {e}"),
            }
        }
        // Let the run tear down before the next one boots.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while stream.next().await.is_some() {}
        })
        .await;
        let resumed_approvals = seen.lock().unwrap().approvals - before;
        let ok = status == Some(luna_proto::DoneStatus::Completed)
            && resumed_approvals >= min
            && max.is_none_or(|max| resumed_approvals <= max);
        eprintln!(
            "--- resume run {} done: {status:?} approvals={resumed_approvals} ok={ok}",
            index + 1
        );
        resume_ok &= ok;
    }
    eprintln!(
        "--- verdict: status_ok={status_ok} text_ok={text_ok} counts_ok={counts_ok} resume_ok={resume_ok}"
    );
    if !(status_ok && text_ok && counts_ok && resume_ok) {
        std::process::exit(1);
    }
}
