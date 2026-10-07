//! Live OC-13 follow-up probe through the PRODUCTION engine: a fresh process
//! (no earlier OpenCode boot) dispatches OpenCode turns through
//! `Sessions::dispatch`, so the engine compiles the runtime contract from the
//! adapter's real `luna_runtime_delivery()`, attaches its real LUNA tool
//! registry and executor, and the REAL OpenCode 2 server and a real (free)
//! model call LUNA tools. Run it under an isolated HOME/XDG (the evidence
//! runner `run-probe.sh`); it never reads the user's OpenCode config,
//! storage or credentials.
//!
//!     opencode_luna_tools_probe <provider/model> <out-dir>
//!
//! Steps (each turn boots its own OpenCode server, as in the app):
//! 1. `first`: the first turn of the process calls `luna_get_workspace`.
//!    Delivery is read BEFORE anything booted (it must already promise the
//!    tools) and the engine executor must run the call.
//! 2. `resume`: the same chat again calls `luna_status` (canonical chip name,
//!    exposed to the model as `luna_status`, not `luna_luna_status`) and
//!    lists its `luna_*` tool names.
//! 3. `fallback`: the same OpenCode session resumed with NO contract (what the
//!    engine sends while delivery is withdrawn): the stale `luna` entry must
//!    be removed.
//! 4. `redeliver`: the chat through the engine again: tools and exactly one
//!    current entry are back.
//! 5. `inventory`: the shared integration inventory for OpenCode, without and
//!    with Check provider: read-only MCP rows from the session readback and
//!    the project readback.
//!
//! After every turn an INDEPENDENT `opencode serve` (same isolated profile)
//! reads the session's instruction entries back. Writes `events.jsonl`,
//! `contract-<step>.json`, `entries-<step>.json`, `inventory-*.json` and
//! `summary.json` into `<out-dir>`; exits non-zero when a check fails.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{CancellationToken, Harness, OpencodeHarness, RunControls};
use luna_proto::{
    AgentEvent, ChatConfig, DoneStatus, HarnessId, IntegrationInventoryView, IntegrationKind,
    PermissionMode, RunRequest, SandboxLevel, TaskLineage, ToolCall,
};
use luna_rpc::methods;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncBufReadExt;

const TURN_TIMEOUT: Duration = Duration::from_secs(300);

struct Probe {
    out: PathBuf,
    events: std::fs::File,
    checks: Vec<(String, bool, String)>,
}

impl Probe {
    fn check(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        let detail = detail.into();
        eprintln!(
            "CHECK {} {name}: {detail}",
            if ok { "ok  " } else { "FAIL" }
        );
        self.checks.push((name.to_owned(), ok, detail));
    }

    fn record(&mut self, step: &str, event: &AgentEvent) {
        let line = json!({"step": step, "event": event});
        let _ = writeln!(self.events, "{line}");
    }

    fn write(&self, name: &str, value: &Value) {
        let _ = std::fs::write(
            self.out.join(name),
            serde_json::to_vec_pretty(value).unwrap_or_default(),
        );
    }
}

#[derive(Default, Debug)]
struct Turn {
    session_id: Option<String>,
    status: Option<DoneStatus>,
    text: String,
    /// Root LUNA chips: (chip id, canonical tool name).
    luna_calls: Vec<(String, String)>,
    /// Chip id → (is_error, output head).
    results: Vec<(String, bool, String)>,
    notices: Vec<String>,
    inputs: usize,
    provider_turns: usize,
}

impl Turn {
    fn luna_ok(&self, tool: &str) -> usize {
        self.luna_calls
            .iter()
            .filter(|(id, name)| {
                name == tool
                    && self
                        .results
                        .iter()
                        .any(|(result, error, _)| result == id && !error)
            })
            .count()
    }
}

fn observe(turn: &mut Turn, event: &AgentEvent) {
    match event {
        AgentEvent::SessionStarted { session_id, .. } => {
            eprintln!("SESSION {session_id}");
            turn.session_id = Some(session_id.clone());
        }
        AgentEvent::ProviderTurnStarted { thread_id, turn_id } => {
            eprintln!("TURN thread={thread_id} turn={turn_id}");
            turn.provider_turns += 1;
        }
        AgentEvent::TextDelta { text } => turn.text.push_str(text),
        AgentEvent::ToolCall { id, call } => {
            eprintln!("TOOLCALL {id} {call:?}");
            if let ToolCall::Mcp { server, tool, .. } = call
                && server == "luna"
            {
                turn.luna_calls.push((id.clone(), tool.clone()));
            }
        }
        AgentEvent::ToolResult {
            id,
            output,
            is_error,
            ..
        } => {
            let head: String = output
                .clone()
                .unwrap_or_default()
                .chars()
                .take(400)
                .collect();
            eprintln!("TOOLRESULT {id} error={is_error} {head}");
            turn.results.push((id.clone(), *is_error, head));
        }
        AgentEvent::ProviderNotice { notice } => {
            eprintln!("NOTICE {notice:?}");
            turn.notices.push(notice.message.clone());
        }
        AgentEvent::InputRequested { .. } => {
            eprintln!("INPUT {event:?}");
            turn.inputs += 1;
        }
        AgentEvent::Done { status, error, .. } => {
            eprintln!("DONE {status:?} error={error:?}");
            turn.status = Some(*status);
        }
        _ => {}
    }
}

fn run_request(prompt: &str, cwd: &Path, model: &str) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: Some(HarnessId::Opencode),
        provider_account_id: None,
        model: Some(model.into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.display().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    }
}

/// One engine turn on `chat`, until its `Done`.
async fn engine_turn(
    probe: &mut Probe,
    core: &EngineCore,
    chat: &str,
    step: &str,
    request: RunRequest,
) -> Turn {
    eprintln!("=== step {step}");
    let (replay, mut events) = core.sessions.subscribe(chat, 0).expect("subscribe");
    let after = replay.last().map(|event| event.seq).unwrap_or(0);
    let mut turn = Turn::default();
    if let Err(error) = core
        .sessions
        .dispatch(
            chat,
            HarnessId::Opencode,
            request,
            Some(format!("probe-{step}-{}", uuid::Uuid::new_v4().simple())),
        )
        .await
    {
        eprintln!("DISPATCH FAILED {error}");
        turn.status = Some(DoneStatus::Errored);
        return turn;
    }
    let outcome = tokio::time::timeout(TURN_TIMEOUT, async {
        loop {
            match events.recv().await {
                Ok(journaled) if journaled.seq > after => {
                    probe.record(step, &journaled.event);
                    observe(&mut turn, &journaled.event);
                    if matches!(journaled.event, AgentEvent::Done { .. }) {
                        break;
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    })
    .await;
    if outcome.is_err() {
        eprintln!("--- step {step} timed out");
    }
    eprintln!("TEXT {:?}", turn.text.trim());
    turn
}

/// One turn straight through the adapter with NO runtime contract: exactly
/// what the engine dispatches while OpenCode delivery is withdrawn
/// (`Sessions` compiles no contract for an `Unsupported` OpenCode).
async fn contractless_turn(probe: &mut Probe, request: RunRequest) -> Turn {
    eprintln!("=== step fallback (no contract)");
    let harness = OpencodeHarness::new();
    let (_steer_tx, steering) = tokio::sync::mpsc::channel(8);
    let mut turn = Turn::default();
    let mut stream = match harness
        .run(
            request,
            RunControls {
                request_input: Box::new(|_| {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = tx.send(Vec::new());
                    rx
                }),
                steering,
                interrupt: CancellationToken::new(),
                invoke_luna_tool: None,
            },
        )
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("RUN FAILED {error}");
            turn.status = Some(DoneStatus::Errored);
            return turn;
        }
    };
    let _ = tokio::time::timeout(TURN_TIMEOUT, async {
        while let Some(event) = stream.next().await {
            let Ok(event) = event else { continue };
            probe.record("fallback", &event);
            observe(&mut turn, &event);
        }
    })
    .await;
    eprintln!("TEXT {:?}", turn.text.trim());
    turn
}

/// An independent OpenCode server in the same isolated profile reads the
/// session's instruction entries back (never LUNA's run server).
async fn read_entries(exe: &Path, dir: &Path, session: &str) -> Result<Vec<Value>, String> {
    let password = uuid::Uuid::new_v4().simple().to_string();
    let mut child = tokio::process::Command::new(exe)
        .args(["serve", "--stdio", "--port", "0"])
        .env("OPENCODE_PASSWORD", &password)
        .env("OPENCODE_SERVER_PASSWORD", &password)
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn: {error}"))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let url = tokio::time::timeout(Duration::from_secs(60), async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(value) = serde_json::from_str::<Value>(line.trim())
                && let Some(url) = value.get("url").and_then(Value::as_str)
            {
                return Some(url.trim_end_matches('/').to_owned());
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
    .ok_or("no readiness line")?;
    let client = reqwest::Client::new();
    let directory: String = dir
        .display()
        .to_string()
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    let get = |path: String| {
        client
            .get(format!("{url}{path}"))
            .basic_auth("opencode", Some(&password))
            .header("x-opencode-directory", directory.clone())
            .timeout(Duration::from_secs(30))
            .send()
    };
    for _ in 0..120 {
        if get("/api/info".into())
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let response = get(format!(
        "/api/experimental/session/{session}/instructions/entries"
    ))
    .await
    .map_err(|error| error.to_string())?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|error| error.to_string())?;
    // End the lease (stdin EOF) and wait for the exit: the next run's server
    // must not meet this one holding the database lock.
    drop(child.stdin.take());
    if tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    if !status.is_success() {
        return Err(format!("{status}: {body}"));
    }
    Ok(body
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn summarize_entries(entries: &[Value]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|entry| {
                let value = entry
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                json!({
                    "key": entry.get("key"),
                    "bytes": value.len(),
                    "sha256": format!("{:x}", Sha256::digest(value.as_bytes())),
                    "head": value.chars().take(240).collect::<String>(),
                    "mentionsToolGuidance": value.contains("luna_status guidance"),
                })
            })
            .collect(),
    )
}

async fn entries_step(
    probe: &mut Probe,
    exe: &Path,
    dir: &Path,
    session: &str,
    step: &str,
) -> Vec<Value> {
    // A concurrent OpenCode boot (the engine's title generation) can briefly
    // hold the database lock and fail this readback server's start.
    let mut outcome = read_entries(exe, dir, session).await;
    for _ in 0..3 {
        if outcome.is_ok() {
            break;
        }
        eprintln!(
            "ENTRIES {step} readback retry: {:?}",
            outcome.as_ref().err()
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        outcome = read_entries(exe, dir, session).await;
    }
    match outcome {
        Ok(entries) => {
            let summary = summarize_entries(&entries);
            eprintln!("ENTRIES {step} {summary}");
            probe.write(&format!("entries-{step}.json"), &summary);
            entries
                .into_iter()
                .filter(|entry| entry.get("key").and_then(Value::as_str) == Some("luna"))
                .collect()
        }
        Err(error) => {
            probe.check(&format!("{step}: entries readable"), false, error);
            Vec::new()
        }
    }
}

async fn contract_step(
    probe: &Probe,
    client: &luna_rpc::RpcClient,
    chat: &str,
    step: &str,
) -> Value {
    let reply = client
        .call(methods::GET_RUNTIME_CONTRACT, json!({"taskId": chat}))
        .await
        .unwrap_or_else(|error| json!({"error": error.to_string()}));
    probe.write(&format!("contract-{step}.json"), &reply);
    reply
}

async fn inventory(
    client: &luna_rpc::RpcClient,
    params: Value,
) -> Result<IntegrationInventoryView, String> {
    client
        .call_as(methods::GET_INTEGRATION_INVENTORY, params)
        .await
        .map_err(|error| error.to_string())
}

#[tokio::main]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "luna_harness::opencode=info".into()),
        )
        .with_writer(std::io::stderr)
        .try_init();
    let mut args = std::env::args().skip(1);
    let model = args.next().expect("usage: <provider/model> <out-dir>");
    let out = PathBuf::from(args.next().expect("usage: <provider/model> <out-dir>"));
    std::fs::create_dir_all(&out).expect("create the output directory");
    let mut probe = Probe {
        events: std::fs::File::create(out.join("events.jsonl")).expect("events file"),
        out: out.clone(),
        checks: Vec::new(),
    };

    // ---- 1. nothing booted yet: what does a first contract promise? -------
    let harness = Arc::new(OpencodeHarness::new());
    let resolved_before = luna_harness::opencode::last_opencode_resolution();
    let delivery_before = harness.luna_runtime_delivery();
    eprintln!("BEFORE resolution={resolved_before:?} delivery={delivery_before:?}");
    probe.check(
        "first: nothing resolved or booted before the first turn",
        resolved_before.is_none(),
        format!("{resolved_before:?}"),
    );
    probe.check(
        "first: delivery promised before any boot",
        delivery_before == luna_proto::RuntimeDelivery::OpenCodeMcpTools,
        format!("{delivery_before:?}"),
    );

    let root = tempfile::tempdir().expect("probe root");
    let base = root.path().canonicalize().expect("canonical root");
    let project = base.join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    // An ordinary disposable Git fixture: OpenCode looks for project
    // configuration up to the repository root, so nothing above the fixture
    // (the user's own folders) is read.
    let _ = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&project)
        .status();
    // A project MCP server that cannot start: the inventory must show it
    // failed in both the project and the session readback, redacted.
    std::fs::write(
        project.join("opencode.json"),
        json!({"mcp": {"servers": {"probe-dummy": {
            "type": "local",
            "command": ["/usr/bin/false", "--token=probe-secret-must-not-leak"]
        }}}})
        .to_string(),
    )
    .expect("project config");
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(harness.clone() as Arc<dyn Harness>);
    let accounts = AgentAccountsConfig {
        data_dir: base.join("providers"),
        claude_config_dir: base.join("providers/claude"),
        claude_config_file: base.join("providers/claude.json"),
        codex_home: base.join("providers/codex"),
    };
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&base.join("engine"), "oc13-probe-org", "oc13-probe-user"),
        registry,
        HarnessId::Opencode,
        None,
        accounts,
    )
    .expect("engine");
    let rpc = core.rpc_service();
    let client = luna_rpc::memory_client(rpc);
    core.workspace
        .create_space(
            "oc13-project",
            &core.device_id,
            project.to_str().unwrap(),
            Some("OC-13 probe project".into()),
            false,
        )
        .expect("project");
    let chat = uuid::Uuid::new_v4().to_string();
    let config = ChatConfig {
        harness: HarnessId::Opencode,
        provider_account_id: None,
        model: Some(model.clone()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    };
    core.workspace
        .create_chat(&chat, Some("oc13-project"), None, Some(config), None)
        .expect("chat");
    core.task_identity
        .register_new_task(
            &chat,
            Some("oc13-project"),
            None,
            project.to_str(),
            TaskLineage::default(),
        )
        .expect("task identity");

    let first = engine_turn(
        &mut probe,
        &core,
        &chat,
        "first",
        run_request(
            "Call your luna_get_workspace tool exactly once with no arguments, directly by name \
             (it is a direct tool, not inside execute). Do not call any other tool. Then reply \
             with exactly: FIRST-DONE",
            &project,
            &model,
        ),
    )
    .await;
    let exe = luna_harness::opencode::last_opencode_resolution()
        .map(|resolution| resolution.path)
        .expect("the first turn resolved OpenCode");
    let contract = contract_step(&probe, &client, &chat, "first").await;
    let session = first.session_id.clone().unwrap_or_default();
    probe.check(
        "first: turn completed",
        first.status == Some(DoneStatus::Completed),
        format!("{:?}", first.status),
    );
    probe.check(
        "first: luna_get_workspace ran through the engine executor",
        first.luna_ok("get_workspace") >= 1,
        format!("calls={:?} results={:?}", first.luna_calls, first.results),
    );
    probe.check(
        "first: no delivery warning",
        first.notices.is_empty(),
        format!("{:?}", first.notices),
    );
    let attached: Vec<String> = contract
        .pointer("/effective/attachedTools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    eprintln!("CONTRACT tools={attached:?}");
    let entries = entries_step(&mut probe, &exe, &project, &session, "first").await;
    probe.check(
        "first: exactly one luna instruction entry",
        entries.len() == 1,
        format!("{}", entries.len()),
    );

    // ---- 2. resume through the engine: canonical names ----------------------
    let mut request = run_request(
        "First write one line starting with TOOLS: followed by the exact names of every tool \
         available to you whose name starts with luna_, comma-separated. Then call your \
         luna_status tool exactly once with no arguments, directly by name (it is a direct tool, \
         not inside execute). Do not call any other tool. Then reply with exactly: RESUME-DONE",
        &project,
        &model,
    );
    request.resume = Some(session.clone());
    let resumed = engine_turn(&mut probe, &core, &chat, "resume", request).await;
    contract_step(&probe, &client, &chat, "resume").await;
    probe.check(
        "resume: same OpenCode session",
        resumed.session_id.as_deref() == Some(session.as_str()),
        format!("{:?}", resumed.session_id),
    );
    probe.check(
        "resume: luna_status ran under its canonical name",
        resumed.luna_ok("luna_status") >= 1,
        format!("calls={:?}", resumed.luna_calls),
    );
    probe.check(
        "resume: no chip carries a listed alias or a double prefix",
        resumed
            .luna_calls
            .iter()
            .chain(&first.luna_calls)
            .all(|(_, tool)| tool != "status" && !tool.starts_with("luna_luna")),
        format!("{:?}", resumed.luna_calls),
    );
    probe.check(
        "resume: the model's luna_ tool names have no double prefix",
        !resumed.text.contains("luna_luna_") && resumed.text.contains("luna_get_workspace"),
        resumed.text.trim().to_owned(),
    );
    probe.check(
        "resume: the model lists luna_status",
        resumed.text.contains("luna_status"),
        resumed.text.trim().to_owned(),
    );
    let entries = entries_step(&mut probe, &exe, &project, &session, "resume").await;
    probe.check(
        "resume: exactly one current luna instruction entry",
        entries.len() == 1,
        format!("{}", entries.len()),
    );

    // ---- 3. resumed with no contract: the stale entry goes -----------------
    let mut request = run_request("Reply with exactly: FALLBACK-DONE", &project, &model);
    request.resume = Some(session.clone());
    let fallback = contractless_turn(&mut probe, request).await;
    probe.check(
        "fallback: turn completed",
        fallback.status == Some(DoneStatus::Completed),
        format!("{:?}", fallback.status),
    );
    let entries = entries_step(&mut probe, &exe, &project, &session, "fallback").await;
    probe.check(
        "fallback: no luna instruction entry remains",
        entries.is_empty(),
        format!("{}", entries.len()),
    );

    // ---- 4. through the engine again: delivered once more -------------------
    let mut request = run_request(
        "Call your luna_get_workspace tool exactly once with no arguments, directly by name (it \
         is a direct tool, not inside execute). Do not call any other tool. Then reply with \
         exactly: REDELIVER-DONE",
        &project,
        &model,
    );
    request.resume = Some(session.clone());
    let redelivered = engine_turn(&mut probe, &core, &chat, "redeliver", request).await;
    probe.check(
        "redeliver: luna_get_workspace ran again",
        redelivered.luna_ok("get_workspace") >= 1,
        format!("calls={:?}", redelivered.luna_calls),
    );
    let entries = entries_step(&mut probe, &exe, &project, &session, "redeliver").await;
    probe.check(
        "redeliver: exactly one luna instruction entry",
        entries.len() == 1,
        format!("{}", entries.len()),
    );

    // ---- 5. the shared integration inventory --------------------------------
    for (label, probe_provider) in [("session", false), ("checked", true)] {
        match inventory(
            &client,
            json!({"provider": "opencode", "projectId": "oc13-project", "taskId": chat,
                   "probeProvider": probe_provider}),
        )
        .await
        {
            Ok(view) => {
                let value = serde_json::to_value(&view).unwrap_or_default();
                probe.write(&format!("inventory-{label}.json"), &value);
                let text = value.to_string();
                let rows: Vec<_> = view
                    .items
                    .iter()
                    .filter(|item| item.kind == IntegrationKind::McpServer)
                    .collect();
                for row in &rows {
                    eprintln!(
                        "MCPROW {label} {} status={:?} attached={:?} configured={} source={} actions={:?}",
                        row.name,
                        row.mcp.as_ref().map(|mcp| mcp.status),
                        row.attached,
                        row.configured,
                        row.source,
                        row.actions
                    );
                }
                let luna = rows.iter().find(|row| row.name == "luna");
                probe.check(
                    &format!("inventory {label}: luna row attached from the session readback"),
                    luna.is_some_and(|row| row.attached == Some(true)),
                    format!("{:?}", luna.map(|row| (&row.attached, &row.reason))),
                );
                probe.check(
                    &format!("inventory {label}: rows are read-only"),
                    rows.iter().all(|row| {
                        row.actions.enable != luna_proto::IntegrationActionAvailability::Supported
                            && row.actions.disable
                                != luna_proto::IntegrationActionAvailability::Supported
                    }),
                    format!("{} rows", rows.len()),
                );
                probe.check(
                    &format!("inventory {label}: no secret or URL leaks"),
                    !text.contains("probe-secret-must-not-leak")
                        && !text.contains("127.0.0.1")
                        && !text.contains("Bearer"),
                    "scanned".to_owned(),
                );
                if probe_provider {
                    let dummy = rows.iter().find(|row| row.name == "probe-dummy");
                    probe.check(
                        "inventory checked: project readback lists the project server",
                        dummy.is_some_and(|row| row.configured),
                        format!("{:?}", dummy.map(|row| (&row.mcp, &row.reason))),
                    );
                }
            }
            Err(error) => probe.check(&format!("inventory {label}: readable"), false, error),
        }
    }

    let failed: Vec<&(String, bool, String)> =
        probe.checks.iter().filter(|(_, ok, _)| !ok).collect();
    let summary = json!({
        "model": model,
        "opencode": exe,
        "deliveryBeforeFirstTurn": format!("{delivery_before:?}"),
        "deliveryAfter": format!("{:?}", harness.luna_runtime_delivery()),
        "session": session,
        "contractTools": attached,
        "checks": probe.checks.iter().map(|(name, ok, detail)| json!({"name": name, "ok": ok, "detail": detail})).collect::<Vec<_>>(),
        "pass": failed.is_empty(),
    });
    probe.write("summary.json", &summary);
    core.shutdown().await;
    if failed.is_empty() {
        eprintln!("=== PASS ({} checks)", probe.checks.len());
    } else {
        for (name, _, detail) in failed {
            eprintln!("FAIL {name}: {detail}");
        }
        std::process::exit(1);
    }
}
