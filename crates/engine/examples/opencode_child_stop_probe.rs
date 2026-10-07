//! Live OCE-03 (b) / S2X per-child Stop probe: a real OpenCode 2 run through
//! the whole engine (task command path, document writer, the `GetTaskWork` /
//! `PreviewStopTaskWork` / `StopTaskWork` RPCs the work panel calls). The root
//! launches ONE foreground native subagent whose shell command sleeps; the
//! probe answers its approval like the UI, then stops only that child from
//! the task work panel and follows the root to the end of its turn. This is
//! the same sequence the Claude comparison run passes (canStop true, stop
//! requested, chip interrupted, root carries on).
//!
//!     opencode_child_stop_probe <opencode-exe> <provider/model> <out-dir>
//!
//! - `PROBE_TIMEOUT_S=<s>`: overall deadline (default 480).
//! - `PROBE_NONCE=<id>`: the run's marker id (default random). A T2 mock
//!   script that runs the child's command needs to know it.
//! - `PROBE_PROMPT=<text>`: the root prompt (`{nonce}` is substituted;
//!   default: the S2X foreground-child prompt).
//! - `PROBE_ROOT_EXPECT=<text>`: what the root reply must contain after the
//!   child stopped (`{nonce}` substituted; default `FG-ROOT-{nonce}`).
//!
//! Failure cases (written before this probe and the fix):
//!
//! 1. The running foreground child row is not stoppable (`canStop:false`):
//!    the engine offers native control only to one provider. *Expected:*
//!    `child-row-stoppable` fails.
//! 2. The row is stoppable but the engine never observed OpenCode's native
//!    session, so the launching-runtime fence cannot match. *Expected:*
//!    `child-row-stoppable` fails (no control) or the receipt is `rejected`.
//! 3. The stop reaches OpenCode with a tool id its run does not know (the
//!    document's chip id differs from the adapter's spawn id), or with a
//!    session id no live run is registered under. *Expected:* receipt
//!    `rejected` ("Provider could not address this live child").
//! 4. OpenCode acknowledges, but the chip never settles. *Expected:*
//!    `child-chip-interrupted` times out with the chip still running.
//! 5. The child stop takes the whole root turn down. *Expected:*
//!    `root-continued` fails (root entry aborted, no root reply after the
//!    child's result).
//! 6. A repeated `StopTaskWork` with the same request id calls the provider
//!    again or answers differently. *Expected:* `stop-receipt-idempotent`
//!    fails.
//! 7. The stopped child's command still finishes. *Expected:*
//!    `child-command-stopped` fails (`FG-CHILD-<nonce>` in the child
//!    transcript).
//! 8. Work rows stay live and blocking after the child and the root ended
//!    (the child's in-flight shell tool never resolves). *Expected:*
//!    `no-blocking-work-after` fails.
//!
//! Writes into `<out-dir>`: `task-work-before-stop.json`,
//! `task-work-stop-preview.json`, `task-work-stop-receipt.json`,
//! `task-work-stop-receipt-replay.json`, `task-work-after.json`,
//! `root-entries.json`, `sub-<chip>.json`, `approvals.json` and
//! `summary.json`. Exit 0 when every check passed.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use luna_doc::{MessagePart, SessionCommandPayload, SessionMessageEntry};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::OpencodeHarness;
use luna_proto::{ApprovalDecision, HarnessId, SessionStatus};
use luna_rpc::RpcClient;
use serde_json::{Value, json};

const SPACE: &str = "oc-child-stop";

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

async fn call(c: &RpcClient, method: &str, params: Value) -> Result<Value, String> {
    c.call(method, params).await.map_err(|e| e.to_string())
}

fn entries(core: &EngineCore, doc: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(doc)
        .ok()
        .and_then(|handle| handle.doc().read_entries().ok())
        .unwrap_or_default()
}

fn json_entries(core: &EngineCore, doc: &str) -> Vec<Value> {
    entries(core, doc)
        .iter()
        .filter_map(|e| serde_json::to_value(e).ok())
        .collect()
}

/// Root entries after the probe's user message.
fn after_message(core: &EngineCore, chat: &str, message: &str) -> Vec<Value> {
    let all = json_entries(core, chat);
    match all.iter().position(|e| e["id"] == message) {
        Some(at) => all[at + 1..].to_vec(),
        None => Vec::new(),
    }
}

/// The first subagent spawn chip after the probe's message.
fn chip(core: &EngineCore, chat: &str, message: &str) -> Option<Value> {
    after_message(core, chat, message)
        .iter()
        .flat_map(|e| e["parts"].as_array().cloned().unwrap_or_default())
        .find(|p| p["kind"] == "tool" && p.get("subagentRef").is_some_and(|r| r.is_string()))
}

fn status(core: &EngineCore, chat: &str) -> Option<SessionStatus> {
    core.sessions.session_status(chat).map(|s| s.status)
}

/// Answer every open approval card on the chat like the UI's "Allow once";
/// a question form takes each question's first option.
fn answer_open_cards(core: &EngineCore, chat: &str, answered: &mut Vec<Value>) {
    for entry in entries(core, chat) {
        for part in &entry.parts {
            let MessagePart::Input {
                request_id,
                questions,
                resolved: false,
                ..
            } = part
            else {
                continue;
            };
            if answered.iter().any(|a| a["request_id"] == *request_id) {
                continue;
            }
            let question = questions.first();
            let approval = question.and_then(|q| q.approval.as_ref());
            let payload = if let Some(approval) = approval {
                let index = approval.details.as_ref().and_then(|d| {
                    d.choices
                        .iter()
                        .position(|choice| choice.label == "Allow once")
                });
                SessionCommandPayload::RespondApproval {
                    request_id: request_id.clone(),
                    decision: index
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
            let queued = core.doc_host.queue_command_with_id_and_transfers(
                chat,
                &format!("probe-answer-{}", answered.len()),
                payload,
                Vec::new(),
            );
            eprintln!("answered card {request_id} (queued: {:?})", queued.is_ok());
            answered.push(json!({
                "request_id": request_id,
                "header": question.map(|q| q.header.clone()),
                "subagent": approval
                    .and_then(|a| a.details.as_ref())
                    .and_then(|d| d.subagent.clone()),
                "queued": queued.is_ok(),
            }));
        }
    }
}

fn write(out: &Path, name: &str, value: &Value) {
    let _ = std::fs::write(
        out.join(name),
        serde_json::to_vec_pretty(value).unwrap_or_default(),
    );
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
    let timeout = std::env::var("PROBE_TIMEOUT_S")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(480);
    std::fs::create_dir_all(&out).expect("create output directory");
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let nonce = env("PROBE_NONCE")
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()[..8].to_uppercase());
    let root_expect = env("PROBE_ROOT_EXPECT")
        .map(|e| e.replace("{nonce}", &nonce))
        .unwrap_or_else(|| format!("FG-ROOT-{nonce}"));
    // The S2X foreground-child prompt, provider neutral.
    let prompt = env("PROBE_PROMPT")
        .map(|p| p.replace("{nonce}", &nonce))
        .unwrap_or_else(|| {
            format!(
                "Use your subagent tool to launch ONE subagent in the foreground (do not run it in \
         the background) with the description \"Foreground child\" and this task: \"Use your \
         shell tool to run exactly this command and nothing else: python3 -c 'import time; \
         time.sleep(90); print(\"FG-CHILD-{nonce}\")'. Then reply with only the command \
         output.\" Wait for the subagent to finish, then reply with exactly FG-ROOT-{nonce} \
         followed by what the subagent reported. If the subagent was stopped or failed, do \
         not launch it again."
            )
        });
    let data = tempfile::tempdir().expect("engine data dir");
    let workspace = tempfile::tempdir().expect("run workspace");
    std::fs::write(workspace.path().join("README.md"), "# Child stop probe\n").ok();
    let core = assemble(data.path(), &exe);
    let c = luna_rpc::memory_client(core.rpc_service());
    let started = Instant::now();
    let deadline = started + Duration::from_secs(timeout);
    let ms = move || started.elapsed().as_millis() as u64;
    let mut checks: Vec<Value> = Vec::new();
    let mut check = |name: &str, ok: bool, detail: Value| {
        eprintln!(
            "[{:>6}] CHECK {name}: {}",
            ms(),
            if ok { "ok" } else { "FAIL" }
        );
        checks.push(json!({ "check": name, "ok": ok, "detail": detail }));
    };

    // Task creation and the first message go through the same RPCs the app
    // uses (task command, then QueueMessage).
    let chat = uuid::Uuid::new_v4().to_string();
    let message = format!("probe-fg-{nonce}");
    core.workspace
        .create_space(
            SPACE,
            &core.device_id,
            workspace.path().to_str().expect("utf-8 workspace"),
            None,
            true,
        )
        .expect("create space");
    let snapshot = call(&c, "GetTaskCommandSnapshot", json!({ "taskId": chat }))
        .await
        .expect("task snapshot");
    let created = call(
        &c,
        "ExecuteTaskCommand",
        json!({
            "taskId": chat,
            "commandId": uuid::Uuid::new_v4().to_string(),
            "actorId": "probe",
            "clientId": "probe",
            "clientSessionId": "probe",
            "expectedTaskRevision": snapshot["snapshot"]["taskRevision"],
            "expectedBindingGeneration": snapshot["snapshot"]["bindingGeneration"],
            "intent": {
                "kind": "createChat",
                "spaceId": SPACE,
                "cwd": workspace.path(),
                "config": {
                    "harness": "opencode",
                    "model": model,
                    "sandbox": "workspace-write",
                    "permissionMode": "ask",
                },
            },
        }),
    )
    .await;
    let created_ok = created.as_ref().is_ok_and(|v| v["status"] == "applied");
    check(
        "task-created",
        created_ok,
        json!({ "result": format!("{created:?}") }),
    );
    let queued = call(
        &c,
        "QueueMessage",
        json!({ "chatId": chat, "messageId": message, "text": prompt, "holdForTurnEnd": true }),
    )
    .await;
    check(
        "message-queued",
        queued.is_ok(),
        json!({ "error": queued.err() }),
    );

    // 1. The foreground child is running its approved command.
    let mut answered: Vec<Value> = Vec::new();
    let mut running_chip = None;
    while Instant::now() < deadline && created_ok {
        answer_open_cards(&core, &chat, &mut answered);
        if let Some(chip) = chip(&core, &chat, &message)
            && chip["subagentStatus"] == "running"
            && !answered.is_empty()
        {
            running_chip = Some(chip);
            break;
        }
        // The root turn ended without a running child (provider error or
        // the model did not spawn): nothing left to stop.
        let ended = after_message(&core, &chat, &message).iter().any(|e| {
            e["role"] == "assistant" && (e["status"] == "complete" || e["status"] == "aborted")
        });
        if ended
            && ms() > 5_000
            && core.sessions.active_turn_id(&chat).is_none()
            && status(&core, &chat).is_some_and(|s| s != SessionStatus::Working)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let root_errors: Vec<Value> = after_message(&core, &chat, &message)
        .iter()
        .flat_map(|e| e["parts"].as_array().cloned().unwrap_or_default())
        .filter(|p| p["kind"] == "error")
        .map(|p| p["message"].clone())
        .collect();
    check(
        "fg-child-running",
        running_chip.is_some(),
        json!({
            "chip": running_chip,
            "approvals": answered.len(),
            "root_errors": root_errors,
            "t_ms": ms(),
        }),
    );
    // Let the approved command start inside the child before stopping it.
    tokio::time::sleep(Duration::from_secs(3)).await;
    answer_open_cards(&core, &chat, &mut answered);

    // 2. The child is a stoppable nativeChild row in the task work panel.
    let before = call(&c, "GetTaskWork", json!({ "taskId": chat }))
        .await
        .unwrap_or_else(|e| json!({ "error": e }));
    write(&out, "task-work-before-stop.json", &before);
    let rows = before["items"].as_array().cloned().unwrap_or_default();
    let item = rows
        .iter()
        .find(|i| i["kind"] == "nativeChild" && i["state"] == "running" && i["canStop"] == true)
        .cloned();
    check(
        "child-row-stoppable",
        item.is_some(),
        json!(
            rows.iter()
                .map(|i| json!({ "title": i["title"], "kind": i["kind"], "state": i["state"], "canStop": i["canStop"], "detail": i["detail"] }))
                .collect::<Vec<_>>()
        ),
    );

    if let Some(item) = &item {
        // 3. Preview, then stop exactly that row; replay the same request.
        let preview = call(
            &c,
            "PreviewStopTaskWork",
            json!({ "taskId": chat, "workIds": [item["id"]] }),
        )
        .await
        .unwrap_or_else(|e| json!({ "error": e }));
        write(&out, "task-work-stop-preview.json", &preview);
        let args = json!({
            "taskId": chat,
            "workIds": [item["id"]],
            "previewId": preview["previewId"],
            "requestId": format!("probe-child-stop-{nonce}"),
        });
        let receipt = call(&c, "StopTaskWork", args.clone())
            .await
            .unwrap_or_else(|e| json!({ "error": e }));
        let stop_requested_ms = ms();
        write(&out, "task-work-stop-receipt.json", &receipt);
        let statuses: Vec<Value> = receipt["results"]
            .as_array()
            .map(|r| r.iter().map(|r| r["status"].clone()).collect())
            .unwrap_or_default();
        check(
            "child-stop-receipt",
            statuses.iter().any(|s| s == "requested" || s == "stopped"),
            json!({ "statuses": statuses, "receipt": receipt }),
        );
        let replay = call(&c, "StopTaskWork", args)
            .await
            .unwrap_or_else(|e| json!({ "error": e }));
        write(&out, "task-work-stop-receipt-replay.json", &replay);
        check(
            "stop-receipt-idempotent",
            replay == receipt,
            json!({ "replay": replay }),
        );

        // 4. The chip settles from the child's own terminal event.
        let mut end = None;
        while Instant::now() < deadline {
            answer_open_cards(&core, &chat, &mut answered);
            if let Some(chip) = chip(&core, &chat, &message)
                && ["interrupted", "failed", "done", "unobserved"]
                    .contains(&chip["subagentStatus"].as_str().unwrap_or(""))
            {
                end = Some(chip["subagentStatus"].clone());
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        check(
            "child-chip-interrupted",
            end.as_ref().is_some_and(|s| s == "interrupted"),
            json!({ "status": end, "t_ms": ms(), "stop_ms": stop_requested_ms }),
        );
    }

    // 5. The root carries on and ends its own turn (only the child stopped).
    let mut root_end = None;
    while Instant::now() < deadline {
        answer_open_cards(&core, &chat, &mut answered);
        if core.sessions.active_turn_id(&chat).is_none()
            && status(&core, &chat).is_some_and(|s| s != SessionStatus::Working)
        {
            root_end = status(&core, &chat);
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if root_end.is_none() {
        // Do not leave a model run behind on failure.
        let _ = core.sessions.interrupt(&chat).await;
    }
    let tail = after_message(&core, &chat, &message);
    let root_reply: String = tail
        .iter()
        .filter(|e| e["role"] == "assistant")
        .flat_map(|e| e["parts"].as_array().cloned().unwrap_or_default())
        .filter(|p| p["kind"] == "text")
        .filter_map(|p| p["text"].as_str().map(str::to_owned))
        .collect();
    let last_status = tail
        .iter()
        .rev()
        .find(|e| e["role"] == "assistant")
        .map(|e| e["status"].clone());
    check(
        "root-continued",
        item.is_some()
            && root_end == Some(SessionStatus::Idle)
            && last_status.as_ref().is_some_and(|s| s == "complete")
            && root_reply.contains(&root_expect),
        json!({
            "session": format!("{root_end:?}"),
            "expect": root_expect,
            "last_assistant_status": last_status,
            "reply": root_reply.chars().take(400).collect::<String>(),
        }),
    );

    // 6. The stopped child's command never finished: its marker is in no
    //    tool output and no assistant text of the child (the command text
    //    itself carries the marker, so call inputs and the prompt do not
    //    count; S2X `markerInTools` reads outputs the same way).
    let mut child_outputs: Vec<String> = Vec::new();
    let mut child_entries = 0;
    for entry in &tail {
        for part in entry["parts"].as_array().cloned().unwrap_or_default() {
            let (Some(sub), Some(id)) = (part["subagentRef"].as_str(), part["id"].as_str()) else {
                continue;
            };
            let sub_entries = json_entries(&core, sub);
            child_entries += sub_entries.len();
            for sub_entry in &sub_entries {
                for sub_part in sub_entry["parts"].as_array().cloned().unwrap_or_default() {
                    if sub_part["kind"] == "tool" {
                        child_outputs.push(sub_part["output"].as_str().unwrap_or("").to_owned());
                    } else if sub_part["kind"] == "text" && sub_entry["role"] == "assistant" {
                        child_outputs.push(sub_part["text"].as_str().unwrap_or("").to_owned());
                    }
                }
            }
            let safe: String = id
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            write(&out, &format!("sub-{safe}.json"), &json!(sub_entries));
        }
    }
    let marker = format!("FG-CHILD-{nonce}");
    let leaked = child_outputs.iter().any(|o| o.contains(&marker));
    check(
        "child-command-stopped",
        item.is_some() && child_entries > 0 && !leaked,
        json!({ "leaked": leaked, "child_entries": child_entries, "child_outputs": child_outputs }),
    );

    // 7. Nothing is left live or blocking once the child and root ended.
    let after = call(&c, "GetTaskWork", json!({ "taskId": chat }))
        .await
        .unwrap_or_else(|e| json!({ "error": e }));
    write(&out, "task-work-after.json", &after);
    check(
        "no-blocking-work-after",
        after["blockingCount"] == 0,
        json!({
            "blockingCount": after["blockingCount"],
            "rows": after["items"].as_array().map(|rows| rows
                .iter()
                .map(|i| json!({ "title": i["title"], "kind": i["kind"], "state": i["state"], "blocks": i["blocksCompletion"] }))
                .collect::<Vec<_>>()),
        }),
    );

    write(
        &out,
        "root-entries.json",
        &json!(json_entries(&core, &chat)),
    );
    write(&out, "approvals.json", &json!(answered));
    let ok = checks.iter().all(|c| c["ok"] == true);
    write(
        &out,
        "summary.json",
        &json!({
            "probe": "opencode_child_stop_probe",
            "model": model,
            "nonce": nonce,
            "prompt": prompt,
            "elapsed_ms": ms(),
            "ok": ok,
            "checks": checks,
        }),
    );
    eprintln!("=== {} ({} ms)", if ok { "PASS" } else { "FAIL" }, ms());
    core.shutdown().await;
    // `exit` skips destructors: remove the disposable engine data and run
    // workspace first.
    drop(c);
    drop(core);
    drop(workspace);
    drop(data);
    std::process::exit(if ok { 0 } else { 1 });
}
