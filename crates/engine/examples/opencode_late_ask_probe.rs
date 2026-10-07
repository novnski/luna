//! Live OCF-51 / OCF-124 / OCF-167 probe: a real OpenCode 2 run through the
//! whole engine (`SessionsEngine` + document writer + durable command path).
//! The root launches a BACKGROUND native subagent and ends its turn; the
//! child then needs an approval or asks a question, after the root's Done.
//! The probe checks that each request reaches the chat's shared panel (an
//! open input part on the latest assistant entry, attributed to the child)
//! while the run stays parked, answers it through the same `RespondApproval`
//! / `RespondInput` command the UI queues (approvals take `PROBE_DECISION`,
//! questions their first option), and then follows the wake turn the child's
//! completion starts into its own assistant entry.
//!
//!     opencode_late_ask_probe <opencode-exe> <provider/model> <out-dir> [prompt]
//!
//! - `PROBE_DECISION=<label>`: the approval choice to send (default
//!   `Allow once`).
//! - `PROBE_EXPECT=<text>`: text the child's transcript must contain once the
//!   approved command ran (e.g. the command's output).
//! - `PROBE_TIMEOUT_S=<s>`: overall deadline (default 420).
//!
//! Writes into `<out-dir>`: `root-entries.json` and `sub-<chip>.json` (the
//! documents as stored), `status.jsonl` (every session status transition),
//! `late-asks.json` (each card as the panel read it, and its answer) and `summary.json`
//! (each check, pass/fail). Exit 0 when every check passed.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use luna_doc::{MessagePart, MessageRole, SessionCommandPayload, SessionMessageEntry};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::OpencodeHarness;
use luna_proto::{ApprovalDecision, HarnessId, RunRequest, SandboxLevel, SessionStatus};
use serde_json::{Value, json};

const CHAT: &str = "oc-late-ask";

fn assemble(root: &Path, exe: &str) -> EngineCore {
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(OpencodeHarness::new().with_executable(exe)));
    let accounts = AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("no-claude"),
        claude_config_file: root.join("no-claude.json"),
        codex_home: root.join("no-codex"),
    };
    accounts
        .disable_claude_keychain_fallback()
        .expect("disable keychain");
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("engine"), "probe-org", "probe-user"),
        registry,
        HarnessId::Opencode,
        None,
        accounts,
    )
    .expect("assemble engine")
}

fn entries(core: &EngineCore, doc: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(doc)
        .ok()
        .and_then(|handle| handle.doc().read_entries().ok())
        .unwrap_or_default()
}

fn status(core: &EngineCore) -> Option<SessionStatus> {
    core.sessions.session_status(CHAT).map(|s| s.status)
}

/// The open request the shared panel serves: an unresolved input part on the
/// LAST assistant entry (`composer::pending_input_request`).
fn panel_request(root: &[SessionMessageEntry]) -> Option<(usize, MessagePart)> {
    let (index, entry) = root
        .iter()
        .enumerate()
        .rev()
        .find(|(_, entry)| entry.role == MessageRole::Assistant)?;
    entry
        .parts
        .iter()
        .find(|part| {
            matches!(
                part,
                MessagePart::Input {
                    resolved: false,
                    ..
                }
            )
        })
        .map(|part| (index, part.clone()))
}

fn entry_text(entry: &SessionMessageEntry) -> (String, usize) {
    let mut text = String::new();
    let mut reasoning = 0;
    for part in &entry.parts {
        match part {
            MessagePart::Text { text: t, .. } => text.push_str(t),
            MessagePart::Reasoning { text: t, .. } => reasoning += t.len(),
            _ => {}
        }
    }
    (text, reasoning)
}

async fn wait_until(
    deadline: Instant,
    what: &str,
    mut check: impl FnMut() -> bool,
) -> Result<(), String> {
    while Instant::now() < deadline {
        if check() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!("timed out waiting for {what}"))
}

#[tokio::main]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "luna_engine::sessions=info".into()),
        )
        .with_writer(std::io::stderr)
        .try_init();
    let mut args = std::env::args().skip(1);
    let exe = args.next().expect("opencode executable");
    let model = args.next().expect("provider/model");
    let out = std::path::PathBuf::from(args.next().expect("output directory"));
    let prompt = args.next().unwrap_or_else(|| {
        "Use the subagent tool with background set to true to launch ONE general subagent \
         with description 'Asking child' and prompt: 'Call your direct tool named shell (it \
         is a direct tool, not inside execute) to run exactly this command and nothing else: \
         printf LATE-ASK | shasum -a 256 | cut -c1-16 \n Do not compute it yourself. Reply \
         with only the exact output.'. \
         Do not wait for it: immediately reply with exactly ROOT-DONE and end your turn."
            .into()
    });
    let decision_label = std::env::var("PROBE_DECISION").unwrap_or_else(|_| "Allow once".into());
    let expect = std::env::var("PROBE_EXPECT").ok().filter(|e| !e.is_empty());
    let timeout = std::env::var("PROBE_TIMEOUT_S")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(420);
    std::fs::create_dir_all(&out).expect("create output directory");
    let data = tempfile::tempdir().expect("engine data dir");
    let workspace = tempfile::tempdir().expect("run workspace");
    let core = assemble(data.path(), &exe);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(timeout);
    let ms = move || started.elapsed().as_millis() as u64;

    // Every status transition, as the sidebar and the queue gate read them.
    let timeline: Arc<Mutex<Vec<Value>>> = Arc::default();
    {
        let mut watch = core.sessions.watch_sessions();
        let timeline = timeline.clone();
        tokio::spawn(async move {
            let mut last = None;
            loop {
                let now = watch
                    .borrow_and_update()
                    .iter()
                    .find(|s| s.chat_id == CHAT)
                    .map(|s| s.status);
                if now != last {
                    timeline
                        .lock()
                        .unwrap()
                        .push(json!({ "t_ms": ms(), "status": format!("{now:?}") }));
                    last = now;
                }
                if watch.changed().await.is_err() {
                    break;
                }
            }
        });
    }
    let saw = |timeline: &Arc<Mutex<Vec<Value>>>, status: &str, after: u64| {
        timeline.lock().unwrap().iter().any(|row| {
            row["status"].as_str() == Some(status) && row["t_ms"].as_u64().unwrap_or(0) >= after
        })
    };

    let request = RunRequest {
        goal: None,
        review: None,
        input_items: Vec::new(),
        prompt: prompt.clone(),
        harness: Some(HarnessId::Opencode),
        provider_account_id: None,
        model: Some(model.clone()),
        reasoning: None,
        model_options: Default::default(),
        cwd: workspace.path().display().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    };
    core.sessions
        .dispatch(
            CHAT,
            HarnessId::Opencode,
            request,
            Some("probe-user-message".into()),
        )
        .await
        .expect("dispatch");

    let mut checks: Vec<Value> = Vec::new();
    let mut check = |name: &str, ok: bool, detail: Value| {
        eprintln!(
            "[{:>6}] CHECK {name}: {}",
            ms(),
            if ok { "ok" } else { "FAIL" }
        );
        checks.push(json!({ "check": name, "ok": ok, "detail": detail }));
    };
    let mut failure: Option<String> = None;

    // 1. The root turn ends (its ROOT-DONE entry finished, the session no
    //    longer Working) and the run parks with the child live. A request the
    //    child raised during the turn is deliberately left unanswered here.
    let root_done_entry = |root: &[SessionMessageEntry]| {
        root.iter().position(|e| {
            e.role == MessageRole::Assistant
                && e.status == Some(luna_doc::MessageStatus::Complete)
                && entry_text(e).0.contains("ROOT-DONE")
        })
    };
    let root_done = wait_until(deadline, "the root turn's Done", || {
        root_done_entry(&entries(&core, CHAT)).is_some()
            && status(&core).is_some_and(|s| s != SessionStatus::Working)
    })
    .await;
    let t_root_done = ms();
    let root_done_index = root_done_entry(&entries(&core, CHAT)).unwrap_or(usize::MAX);
    check(
        "root-turn-done-and-parked",
        root_done.is_ok() && core.sessions.has_live_run(CHAT),
        json!({
            "t_root_done_ms": t_root_done,
            "root_done_entry": root_done_index,
            "status": format!("{:?}", status(&core)),
            "live_run": core.sessions.has_live_run(CHAT),
            "error": root_done.err(),
        }),
    );

    // 2-4. Every approval the child raises after that Done reaches the shared
    // panel while the run stays parked, and is answered through the durable
    // command the UI queues. The child's completion then starts the wake
    // turn, which must land in its own entry below the last card.
    let mut cards: Vec<Value> = Vec::new();
    let mut last_card: Option<usize> = None;
    let mut last_answer_ms = 0u64;
    let settled: Result<(), String> = loop {
        if Instant::now() >= deadline {
            break Err("timed out waiting for the wake turn to settle".into());
        }
        let root = entries(&core, CHAT);
        if let Some((
            index,
            MessagePart::Input {
                request_id,
                questions,
                ..
            },
        )) = panel_request(&root)
        {
            let t_asked = ms();
            let question = questions.first();
            let is_approval = question.is_some_and(|q| q.approval.is_some());
            let waiting = if is_approval {
                SessionStatus::AwaitingApproval
            } else {
                SessionStatus::AwaitingInput
            };
            // The card is written just before the status flips.
            let _ = wait_until(
                Instant::now() + Duration::from_secs(2),
                "the waiting status",
                || status(&core) == Some(waiting),
            )
            .await;
            let status_at_ask = status(&core);
            let live_at_ask = core.sessions.has_live_run(CHAT);
            let details = question
                .and_then(|q| q.approval.as_ref())
                .and_then(|a| a.details.as_ref());
            let choice_index = details.and_then(|d| {
                d.choices
                    .iter()
                    .position(|choice| choice.label == decision_label)
            });
            // Approvals take the exact choice; a question form (the child's
            // question tool) takes each question's first option.
            let payload = if is_approval {
                SessionCommandPayload::RespondApproval {
                    request_id: request_id.clone(),
                    decision: choice_index
                        .map(|index| ApprovalDecision::ProviderChoice {
                            index: index as u32,
                        })
                        .unwrap_or(ApprovalDecision::AllowOnce),
                }
            } else {
                SessionCommandPayload::RespondInput {
                    request_id: request_id.clone(),
                    answers: questions
                        .iter()
                        .map(|q| luna_proto::UserInputAnswer {
                            question_id: q.id.clone(),
                            labels: q.options.first().cloned().into_iter().collect(),
                        })
                        .collect(),
                }
            };
            let answer = format!("{payload:?}");
            let queued = core.doc_host.queue_command_with_id_and_transfers(
                CHAT,
                &format!("probe-answer-{}", cards.len()),
                payload,
                Vec::new(),
            );
            let resolved = wait_until(deadline, "the card to resolve", || {
                entries(&core, CHAT).iter().flat_map(|e| &e.parts).any(|part| {
                    matches!(part, MessagePart::Input { request_id: id, resolved: true, .. } if *id == request_id)
                })
            })
            .await;
            last_answer_ms = ms();
            eprintln!(
                "[{last_answer_ms:>6}] answered card {request_id} at entry {index}: {answer}"
            );
            cards.push(json!({
                "request_id": request_id,
                "entry_index": index,
                "root_done_entry": root_done_index,
                "t_asked_ms": t_asked,
                "after_root_done": t_asked >= t_root_done && index > root_done_index,
                "approval": is_approval,
                "header": question.map(|q| q.header.clone()),
                "question": question.map(|q| q.question.clone()),
                "details.subagent": details.and_then(|d| d.subagent.clone()),
                "status_at_ask": format!("{status_at_ask:?}"),
                "expected_status": format!("{:?}", Some(waiting)),
                "live_run_at_ask": live_at_ask,
                "answer": answer,
                "queued": queued.as_ref().err().map(|e| e.to_string()),
                "resolved": resolved.is_ok(),
                "t_answered_ms": last_answer_ms,
                "questions": questions,
            }));
            last_card = Some(index);
            continue;
        }
        let after = last_card.map_or(root.len(), |index| index + 1);
        let wake_output = root
            .iter()
            .skip(after)
            .filter(|e| e.role == MessageRole::Assistant)
            .any(|e| {
                let (text, reasoning) = entry_text(e);
                !text.trim().is_empty() || reasoning > 0
            });
        if last_card.is_some()
            && wake_output
            && saw(&timeline, "Some(Working)", last_answer_ms)
            && status(&core) == Some(SessionStatus::Idle)
        {
            break Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let _ = std::fs::write(
        out.join("late-asks.json"),
        serde_json::to_vec_pretty(&cards).unwrap(),
    );
    let all = |key: &str, want: &dyn Fn(&Value) -> bool| {
        !cards.is_empty() && cards.iter().all(|card| want(&card[key]))
    };
    check(
        "late-asks-shown-after-root-done",
        all("after_root_done", &|v| v == true),
        json!({ "cards": cards.len(), "t_root_done_ms": t_root_done }),
    );
    // Approvals name the child in header, text and details; a question form
    // keeps the model's own question text and names the child in its header.
    check(
        "late-asks-attributed-to-child",
        !cards.is_empty()
            && cards.iter().all(|card| {
                let header = card["header"]
                    .as_str()
                    .is_some_and(|h| h.contains("sub-agent"));
                header
                    && (card["approval"] != true
                        || (card["question"]
                            .as_str()
                            .is_some_and(|q| q.contains("sub-agent"))
                            && card["details.subagent"]
                                .as_str()
                                .is_some_and(|s| !s.is_empty())))
            }),
        json!(
            cards
                .iter()
                .map(|c| json!({
                    "header": c["header"],
                    "question": c["question"],
                    "subagent": c["details.subagent"],
                }))
                .collect::<Vec<_>>()
        ),
    );
    check(
        "awaiting-user-while-parked",
        !cards.is_empty()
            && cards
                .iter()
                .all(|card| card["status_at_ask"] == card["expected_status"])
            && all("live_run_at_ask", &|v| v == true),
        json!(
            cards
                .iter()
                .map(|c| c["status_at_ask"].clone())
                .collect::<Vec<_>>()
        ),
    );
    check(
        "late-asks-answered-and-resolved",
        all("resolved", &|v| v == true) && all("queued", &|v| v.is_null()),
        json!({ "cards": cards.len() }),
    );
    if cards.is_empty() {
        failure = Some("no late approval card".into());
    }
    let root = entries(&core, CHAT);
    let after = last_card.map_or(root.len(), |index| index + 1);
    let wake_entries: Vec<Value> = root
        .iter()
        .skip(after)
        .filter(|e| e.role == MessageRole::Assistant)
        .map(|e| {
            let (text, reasoning) = entry_text(e);
            json!({ "id": e.id, "status": format!("{:?}", e.status), "text": text, "reasoning_bytes": reasoning })
        })
        .collect();
    check(
        "wake-turn-own-segment",
        settled.is_ok() && !wake_entries.is_empty(),
        json!({ "wake_entries": wake_entries, "error": settled.err() }),
    );
    // The adapter ends the run once no background work remains.
    let ended = wait_until(
        Instant::now() + Duration::from_secs(30),
        "the run to end",
        || !core.sessions.has_live_run(CHAT),
    )
    .await;
    check(
        "run-ended-after-wake",
        ended.is_ok(),
        json!({ "status": format!("{:?}", status(&core)) }),
    );

    // 5. Child transcript: the approved command really ran.
    let root = entries(&core, CHAT);
    let chips: Vec<(String, Option<String>, Option<String>)> = root
        .iter()
        .flat_map(|e| &e.parts)
        .filter_map(|part| match part {
            MessagePart::Tool {
                id,
                subagent_ref: Some(sub),
                subagent_status,
                ..
            } => Some((
                id.clone(),
                Some(sub.clone()),
                subagent_status.as_ref().map(|s| format!("{s:?}")),
            )),
            _ => None,
        })
        .collect();
    let mut child_text = String::new();
    for (chip, sub, _) in &chips {
        if let Some(sub) = sub {
            let sub_entries = entries(&core, sub);
            child_text.push_str(&serde_json::to_string(&sub_entries).unwrap_or_default());
            let safe: String = chip
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let _ = std::fs::write(
                out.join(format!("sub-{safe}.json")),
                serde_json::to_vec_pretty(&sub_entries).unwrap(),
            );
        }
    }
    if failure.is_none() {
        check(
            "child-ran-approved-command",
            !chips.is_empty() && expect.as_deref().is_none_or(|e| child_text.contains(e)),
            json!({ "chips": chips, "expect": expect }),
        );
    }

    // 6. No card left open anywhere in the root transcript.
    let open_cards = root
        .iter()
        .flat_map(|e| &e.parts)
        .filter(|part| {
            matches!(
                part,
                MessagePart::Input {
                    resolved: false,
                    ..
                }
            )
        })
        .count();
    check(
        "no-dangling-cards",
        open_cards == 0,
        json!({ "open": open_cards }),
    );

    let _ = std::fs::write(
        out.join("root-entries.json"),
        serde_json::to_vec_pretty(&root).unwrap(),
    );
    let rows: Vec<String> = timeline
        .lock()
        .unwrap()
        .iter()
        .map(|row| row.to_string())
        .collect();
    let _ = std::fs::write(out.join("status.jsonl"), rows.join("\n") + "\n");
    let ok = checks.iter().all(|c| c["ok"] == true);
    let _ = std::fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&json!({
            "probe": "opencode_late_ask_probe",
            "model": model,
            "prompt": prompt,
            "elapsed_ms": ms(),
            "ok": ok,
            "failure": failure,
            "checks": checks,
        }))
        .unwrap(),
    );
    eprintln!("=== {} ({} ms)", if ok { "PASS" } else { "FAIL" }, ms());
    core.shutdown().await;
    std::process::exit(if ok { 0 } else { 1 });
}
