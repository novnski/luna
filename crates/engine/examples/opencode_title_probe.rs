//! Live probe (X1 / OCE-20 "first-turn title"): automatic chat titles for an
//! OpenCode chat through the PRODUCTION engine (`Sessions::dispatch` → the
//! engine's `TitleGenerator` → `OpencodeHarness::run_title`) against the REAL
//! OpenCode 2 server and a real model. Run it with the isolated QA profile's
//! wrapper (`scripts/qa-opencode-env.sh`); it never touches the user's
//! OpenCode storage.
//!
//!     LUNA_OPENCODE_WIRE_LOG=<out-dir>/wire \
//!     opencode_title_probe <opencode-exe> <provider/model> <out-dir>
//!
//! Claude comparison (OCE-20 compares with a Claude chat): pass `claude` as
//! the executable and a Claude model. The chat runs on the user's normal
//! Claude Code login (production account paths, read only); checks 1-3 and
//! the stable title of check 6 apply (there is no wire log to count runs).
//!
//! Ways the title path can fail, each a check below:
//! 1. `turn-completed`: the chat's first turn never completes (nothing to
//!    title from; the probe is void).
//! 2. `title-landed`: a title is computed (model or first-words fallback) but
//!    never written, e.g. a provider-native rename that only Codex supports
//!    is attempted for an OpenCode/Claude binding and its error aborts the
//!    local rename.
//! 3. `title-generated`: the title is only the prompt's first words because
//!    every model attempt failed.
//! 4. `title-model-from-chat-provider`: LUNA picks a title model outside the
//!    provider the chat runs on (a rate-limited free model, an
//!    unauthenticated provider) when that provider offers a small model.
//! 5. `title-attempts-bounded`: the ladder burns every attempt on the same
//!    failing model, booting an `opencode serve` per attempt.
//! 6. `title-not-repeated`: the Done-time retry re-runs the title model on
//!    every later turn because the chat never got its title.
//! 7. `title-sessions-removed`: a throwaway title session lingers.
//!
//! Writes `summary.json` (every check, the title, the title model, each
//! title request's status and the servers booted) into `<out-dir>`; exits
//! non-zero when a check fails.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use luna_doc::{MessagePart, MessageRole, MessageStatus};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{ClaudeHarness, OpencodeHarness};
use luna_proto::{
    ChatConfig, HarnessId, PermissionMode, RunRequest, SandboxLevel, SessionStatus, TaskLineage,
};
use serde_json::{Value, json};

const FIRST_PROMPT: &str =
    "In one short sentence, explain what a hash map is used for. Do not use any tools.";
const SECOND_PROMPT: &str =
    "Now in one short sentence, explain what a linked list is. Do not use any tools.";
const TURN_TIMEOUT: Duration = Duration::from_secs(240);
const TITLE_TIMEOUT: Duration = Duration::from_secs(120);

/// The engine's first-words fallback (titles.rs), to tell it apart from a
/// generated title.
fn fallback_title(prompt: &str) -> String {
    prompt
        .split_whitespace()
        .take(7)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(48)
        .collect()
}

fn request(harness: HarnessId, model: &str, cwd: &Path, prompt: &str) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: Some(harness),
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
        worktree: None,
        resume: None,
    }
}

/// Completed assistant entries in the chat's document.
fn completed_replies(core: &EngineCore, chat: &str) -> usize {
    core.doc_host
        .open(chat)
        .ok()
        .and_then(|handle| handle.doc().read_entries().ok())
        .unwrap_or_default()
        .iter()
        .filter(|entry| {
            entry.role == MessageRole::Assistant && entry.status == Some(MessageStatus::Complete)
        })
        .count()
}

/// The error the latest finished assistant entry carries (a failed turn,
/// e.g. a provider quota), if any.
fn last_reply_error(core: &EngineCore, chat: &str) -> Option<String> {
    core.doc_host
        .open(chat)
        .ok()
        .and_then(|handle| handle.doc().read_entries().ok())
        .unwrap_or_default()
        .iter()
        .rev()
        .find(|entry| {
            entry.role == MessageRole::Assistant && entry.status == Some(MessageStatus::Complete)
        })
        .and_then(|entry| {
            entry.parts.iter().find_map(|part| match part {
                MessagePart::Error { message, .. } => Some(message.clone()),
                _ => None,
            })
        })
}

fn title(core: &EngineCore, chat: &str) -> Option<String> {
    core.workspace
        .chat(chat)
        .ok()
        .flatten()
        .and_then(|chat| chat.title)
        .filter(|title| !title.trim().is_empty())
}

async fn wait_until(deadline: Instant, mut check: impl FnMut() -> bool) -> bool {
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    check()
}

async fn turn(core: &EngineCore, chat: &str, request: RunRequest, replies_before: usize) -> bool {
    let message = format!("title-probe-{}", uuid::Uuid::new_v4());
    let harness = request.harness.unwrap_or(HarnessId::Opencode);
    if let Err(error) = core
        .sessions
        .dispatch(chat, harness, request, Some(message))
        .await
    {
        eprintln!("dispatch failed: {error}");
        return false;
    }
    wait_until(Instant::now() + TURN_TIMEOUT, || {
        completed_replies(core, chat) > replies_before
            && core
                .sessions
                .session_status(chat)
                .is_some_and(|session| session.status != SessionStatus::Working)
    })
    .await
}

/// Every `http` record of this process's wire log.
fn wire_records(dir: &Path) -> Vec<Value> {
    let path = dir.join(format!("opencode-wire-{}.jsonl", std::process::id()));
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record["kind"] == "http")
        .collect()
}

/// The title requests in the wire log: each throwaway session (created with
/// the hidden `title` agent) with its model, `/generate` status and removal.
fn title_attempts(records: &[Value]) -> Vec<Value> {
    let mut attempts = Vec::new();
    for record in records {
        if record["method"] != "POST" || record["path"] != "/api/session" {
            continue;
        }
        let body = &record["request"]["body"];
        if body["agent"] != "title" {
            continue;
        }
        let id = record["response"]["body"]
            .pointer("/data/id")
            .or_else(|| record["response"]["body"].get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let generate = records.iter().find(|r| {
            r["method"] == "POST" && r["path"] == format!("/api/session/{id}/generate").as_str()
        });
        let removed = records.iter().any(|r| {
            r["method"] == "DELETE"
                && r["path"] == format!("/api/session/{id}").as_str()
                && r["status"].as_u64().is_some_and(|s| s < 300)
        });
        attempts.push(json!({
            "t_ms": record["t_ms"],
            "server": record["server"],
            "session": id,
            "model": body["model"],
            "generate_status": generate.and_then(|r| r["status"].as_u64()),
            "generate_error": generate.and_then(|r| r["response"]["body"].get("message").cloned()),
            "removed": removed,
        }));
    }
    attempts
}

/// Which `opencode serve` processes booted, and the routes each served.
fn servers(records: &[Value]) -> Vec<Value> {
    let mut servers: Vec<Value> = Vec::new();
    for record in records {
        let server = record["server"].as_str().unwrap_or_default();
        let path = record["path"].as_str().unwrap_or_default();
        // Session ids vary; keep the route shape.
        let route = path
            .split('/')
            .map(|seg| if seg.starts_with("ses_") { "{id}" } else { seg })
            .collect::<Vec<_>>()
            .join("/");
        let label = json!(format!(
            "{} {route}",
            record["method"].as_str().unwrap_or_default()
        ));
        match servers.iter_mut().find(|s| s["server"] == server) {
            Some(entry) => {
                let routes = entry["routes"].as_array_mut().unwrap();
                if !routes.contains(&label) {
                    routes.push(label);
                }
            }
            None => servers.push(json!({ "server": server, "routes": [label] })),
        }
    }
    servers
}

#[tokio::main]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "luna_engine::titles=debug,luna_engine::sessions=warn,luna_harness=info".into()
            }),
        )
        .with_writer(std::io::stderr)
        .try_init();
    let mut args = std::env::args().skip(1);
    let exe = args.next().expect("opencode executable, or `claude`");
    let model = args.next().expect("model");
    let out = PathBuf::from(args.next().expect("output directory"));
    std::fs::create_dir_all(&out).expect("output directory");
    let harness = if exe == "claude" {
        HarnessId::ClaudeCode
    } else {
        HarnessId::Opencode
    };
    let wire = std::env::var_os("LUNA_OPENCODE_WIRE_LOG")
        .map(PathBuf::from)
        .filter(|_| harness == HarnessId::Opencode);
    assert!(
        harness != HarnessId::Opencode || wire.is_some(),
        "LUNA_OPENCODE_WIRE_LOG names the wire-log directory"
    );

    let base = tempfile::tempdir().expect("engine data dir");
    let project = tempfile::tempdir().expect("project dir");
    let registry = Arc::new(HarnessRegistry::new());
    let accounts = if harness == HarnessId::ClaudeCode {
        registry.register(Arc::new(ClaudeHarness::new()));
        // The user's normal Claude Code login, read where production reads it.
        AgentAccountsConfig::detect(&base.path().join("providers"))
    } else {
        registry.register(Arc::new(OpencodeHarness::new().with_executable(&exe)));
        let accounts = AgentAccountsConfig {
            data_dir: base.path().join("providers"),
            claude_config_dir: base.path().join("providers/claude"),
            claude_config_file: base.path().join("providers/claude.json"),
            codex_home: base.path().join("providers/codex"),
        };
        accounts
            .disable_claude_keychain_fallback()
            .expect("disable keychain");
        accounts
    };
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(
            &base.path().join("engine"),
            "title-probe-org",
            "title-probe",
        ),
        registry,
        harness,
        None,
        accounts,
    )
    .expect("engine");
    let project_path = project.path().to_str().unwrap().to_owned();
    core.workspace
        .create_space(
            "title-probe-project",
            &core.device_id,
            &project_path,
            Some("Title probe".into()),
            false,
        )
        .expect("project");
    let chat = uuid::Uuid::new_v4().to_string();
    let config = ChatConfig {
        harness,
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
        .create_chat(&chat, Some("title-probe-project"), None, Some(config), None)
        .expect("chat");
    core.task_identity
        .register_new_task(
            &chat,
            Some("title-probe-project"),
            None,
            Some(project_path.as_str()),
            TaskLineage::default(),
        )
        .expect("task identity");

    let started = Instant::now();
    let mut checks: Vec<Value> = Vec::new();
    let mut check = |name: &str, ok: bool, detail: Value| {
        eprintln!("CHECK {name}: {}", if ok { "ok" } else { "FAIL" });
        checks.push(json!({ "check": name, "ok": ok, "detail": detail }));
    };

    // Turn 1: the title is requested at dispatch and retried at Done.
    let first_ok = turn(
        &core,
        &chat,
        request(harness, &model, project.path(), FIRST_PROMPT),
        0,
    )
    .await;
    let t_first_done = started.elapsed().as_millis() as u64;
    let first_error = last_reply_error(&core, &chat);
    check(
        "turn-completed",
        first_ok && first_error.is_none(),
        json!({
            "t_ms": t_first_done,
            "replies": completed_replies(&core, &chat),
            "error": first_error,
        }),
    );
    let landed = wait_until(Instant::now() + TITLE_TIMEOUT, || {
        title(&core, &chat).is_some()
    })
    .await;
    let t_title = started.elapsed().as_millis() as u64;
    let first_title = title(&core, &chat);
    check(
        "title-landed",
        landed,
        json!({ "title": first_title, "t_ms": t_title }),
    );
    let fallback = fallback_title(FIRST_PROMPT);
    check(
        "title-generated",
        first_title
            .as_deref()
            .is_some_and(|title| title != fallback.as_str()),
        json!({ "title": first_title, "fallback": fallback }),
    );
    // Let the ladder (if any) settle before reading the wire log.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let read_wire = || wire.as_deref().map(wire_records).unwrap_or_default();
    let after_first = title_attempts(&read_wire());

    // Turn 2 on the same chat (the engine resumes the provider session): a
    // titled chat must not run the title model again.
    let second_ok = turn(
        &core,
        &chat,
        request(harness, &model, project.path(), SECOND_PROMPT),
        completed_replies(&core, &chat),
    )
    .await;
    // A failed turn never reaches the Done-time retry, so it proves nothing.
    let second_error = last_reply_error(&core, &chat);
    let second_ok = second_ok && second_error.is_none();
    tokio::time::sleep(Duration::from_secs(8)).await;
    let records = read_wire();
    let attempts = title_attempts(&records);
    if harness == HarnessId::Opencode {
        let chat_provider = model.split_once('/').map(|(provider, _)| provider);
        let first_model = after_first
            .first()
            .map(|attempt| attempt["model"].clone())
            .unwrap_or(Value::Null);
        check(
            "title-model-from-chat-provider",
            first_model["providerID"].as_str() == chat_provider,
            json!({ "chat_model": model, "first_title_model": first_model }),
        );
        let generated_ok = after_first
            .iter()
            .filter(|attempt| attempt["generate_status"].as_u64() == Some(200))
            .count();
        check(
            "title-attempts-bounded",
            !after_first.is_empty() && after_first.len() <= 2 && generated_ok == 1,
            json!({ "attempts": after_first.len(), "generated_200": generated_ok }),
        );
    }
    check(
        "title-not-repeated",
        second_ok
            && first_title.is_some()
            && attempts.len() == after_first.len()
            && title(&core, &chat) == first_title,
        json!({
            "second_turn_completed": second_ok,
            "second_turn_error": second_error,
            "attempts_after_turn_1": after_first.len(),
            "attempts_after_turn_2": attempts.len(),
            "title_after_turn_2": title(&core, &chat),
        }),
    );
    if harness == HarnessId::Opencode {
        check(
            "title-sessions-removed",
            attempts
                .iter()
                .all(|attempt| attempt["removed"].as_bool() == Some(true)),
            json!({ "attempts": attempts.len() }),
        );
    }

    let servers = servers(&records);
    let ok = checks.iter().all(|c| c["ok"] == true);
    let summary = json!({
        "ok": ok,
        "harness": format!("{harness:?}"),
        "chat_model": model,
        "executable": exe,
        "title": first_title,
        "checks": checks,
        "title_attempts": attempts,
        "servers_booted": servers.len(),
        "servers": servers,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    });
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .expect("summary");
    println!(
        "{}",
        serde_json::to_string_pretty(&summary["checks"]).unwrap()
    );
    core.shutdown().await;
    std::process::exit(if ok { 0 } else { 1 });
}
