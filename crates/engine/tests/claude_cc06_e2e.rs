//! CC-06 production-path E2E: engine queue/dispatch/RPC → the real
//! `ClaudeHarness` → `harness/tests/fixtures/fake-claude-cc06.py` (a
//! deterministic Claude Code 2.1.286 peer). No model, account or network.
//!
//! Scenarios (see CC-06 `failure-cases.md`):
//! - E11 explicit steer settles its durable row only on the native replay;
//! - E24 a Claude-initiated result never completes the queued human turn;
//! - E10 a parked run switches model/effort natively (no restart) and the
//!   Usage RPC reports latest-wins live accounting and stream-observed limits;
//! - C2 a rejected switch re-dispatches the same message once, fresh, with the
//!   new configuration.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use luna_doc::{MessagePart, MessageRole, SessionMessageEntry};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::ClaudeHarness;
use luna_proto::{HarnessId, ReasoningLevel, SessionStatus};

const CHAT: &str = "chat-cc06";

fn fake() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-claude-cc06.py")
        .canonicalize()
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

/// The engine's background model refresh constructs `ClaudeHarness::new()`;
/// point every in-process resolution at the fake so no real CLI is spawned.
fn isolate_claude_executable() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: runs once, before any engine in this test binary starts the
        // threads that read the environment.
        unsafe { std::env::set_var("CLAUDE_CODE_EXECUTABLE", fake()) };
    });
}

struct Fixture {
    core: EngineCore,
    project: PathBuf,
    _root: tempfile::TempDir,
}

async fn setup(model: &str, reasoning: ReasoningLevel) -> Fixture {
    isolate_claude_executable();
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let project = root.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(".cc06-log"), "").unwrap();
    let accounts_dir = data.join("provider-state");
    std::fs::create_dir_all(&accounts_dir).unwrap();
    std::fs::write(
        data.join("synthetic-claude.json"),
        r#"{"oauthAccount":{"accountUuid":"cc06-synthetic","emailAddress":"qa@example.invalid"}}"#,
    )
    .unwrap();
    // A fresh price cache keeps the Usage RPC off the network.
    let rates = serde_json::json!({
        "fetched_at_ms": chrono::Utc::now().timestamp_millis(),
        "rates": {"claude-sonnet-5-5": {"inputPerMillion": 3.0, "cachedPerMillion": 0.3,
            "creationPerMillion": 3.75, "outputPerMillion": 15.0}}
    });
    for dir in [&data, &accounts_dir] {
        std::fs::write(dir.join("usage-model-rates.json"), rates.to_string()).unwrap();
    }
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(
        ClaudeHarness::new()
            .with_executable(fake())
            .with_graces(Duration::from_millis(200), Duration::from_millis(200)),
    ));
    let accounts = AgentAccountsConfig {
        data_dir: accounts_dir,
        claude_config_dir: data.join("synthetic-claude"),
        claude_config_file: data.join("synthetic-claude.json"),
        codex_home: data.join("synthetic-codex"),
    };
    accounts
        .disable_claude_keychain_fallback()
        .expect("synthetic accounts");
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&data, "dev-org", "dev-user"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
        accounts,
    )
    .expect("engine core assembles");
    core.workspace
        .create_space(
            "space-cc06",
            &core.device_id,
            project.to_str().unwrap(),
            None,
            false,
        )
        .expect("create project");
    luna_rpc::memory_client(core.rpc_service())
        .call(
            luna_rpc::methods::MUTATE,
            serde_json::json!({"op": "createChat", "chatId": CHAT, "spaceId": "space-cc06"}),
        )
        .await
        .expect("createChat");
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");
    set_config(&core, model, reasoning, false);
    Fixture {
        core,
        project,
        _root: root,
    }
}

fn set_config(core: &EngineCore, model: &str, reasoning: ReasoningLevel, fast: bool) {
    let mut model_options = serde_json::Map::new();
    if fast {
        model_options.insert("fastMode".into(), "on".into());
    }
    core.workspace
        .set_chat_config(
            CHAT,
            &luna_proto::ChatConfig {
                harness: HarnessId::ClaudeCode,
                provider_account_id: None,
                model: Some(model.into()),
                reasoning: Some(reasoning),
                model_options,
                sandbox: luna_proto::SandboxLevel::WorkspaceWrite,
                permission_mode: luna_proto::PermissionMode::Ask,
                effective_permission_mode: luna_proto::PermissionMode::Ask,
                permission_ceiling_source: None,
                permission_degradation_reason: None,
                permission_audit: Vec::new(),
            },
        )
        .unwrap();
}

async fn wait_for(mut predicate: impl FnMut() -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn entries(core: &EngineCore) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default()
}

fn texts(core: &EngineCore, role: MessageRole) -> Vec<(String, String)> {
    entries(core)
        .into_iter()
        .filter(|entry| entry.role == role)
        .map(|entry| {
            let text = entry
                .parts
                .iter()
                .filter_map(|part| match part {
                    MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            (entry.id, text)
        })
        .collect()
}

fn queue_is_empty(core: &EngineCore) -> bool {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_queue().ok())
        .is_some_and(|queue| queue.is_empty())
}

fn completion(core: &EngineCore) -> Option<String> {
    core.sessions
        .session_status(CHAT)
        .and_then(|s| s.last_completed_turn)
}

fn idle(core: &EngineCore) -> bool {
    core.sessions
        .session_status(CHAT)
        .is_some_and(|s| s.status == SessionStatus::Idle && s.last_completed_turn.is_some())
}

fn log(project: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(project.join("cc06-fake.log"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Spawns that started a conversation (initialize discovery probes excluded).
fn conversation_spawns(project: &Path) -> Vec<Vec<String>> {
    let entries = log(project);
    let mut spawns = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry["event"] != "spawn" {
            continue;
        }
        let first = entries[index + 1..]
            .iter()
            .find(|next| next["event"] == "stdin")
            .and_then(|next| next["line"].as_str())
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap());
        if first.is_some_and(|frame| frame["type"] == "user") {
            spawns.push(
                entry["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|arg| arg.as_str().unwrap().to_owned())
                    .collect(),
            );
        }
    }
    spawns
}

fn stdin_users(project: &Path) -> Vec<serde_json::Value> {
    log(project)
        .into_iter()
        .filter(|entry| entry["event"] == "stdin")
        .map(|entry| {
            serde_json::from_str::<serde_json::Value>(entry["line"].as_str().unwrap()).unwrap()
        })
        .collect()
}

/// E11: the queued steer's durable row settles from the CLI's replay of the
/// SAME uuid, which is the queue id itself — one process, no replay of text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_claude_steer_settles_by_native_replay_receipt() {
    let fx = setup("claude-sonnet-5-5", ReasoningLevel::High).await;
    let core = &fx.core;
    core.doc_host
        .queue_message(CHAT, "cc06:delivery opening", Vec::new())
        .unwrap();
    wait_for(
        || {
            texts(core, MessageRole::Assistant)
                .iter()
                .any(|(_, text)| text.contains("working"))
        },
        "the first Claude turn to stream",
    )
    .await;
    let reply = luna_rpc::memory_client(core.rpc_service())
        .call(
            luna_rpc::methods::QUEUE_MESSAGE,
            serde_json::json!({"chatId": CHAT, "text": "redirect please",
                "holdForTurnEnd": true, "followUpBehavior": "steer"}),
        )
        .await
        .expect("queue steer");
    let id = reply["id"].as_str().unwrap().to_owned();
    wait_for(
        || {
            queue_is_empty(core)
                && texts(core, MessageRole::User)
                    .iter()
                    .any(|(_, text)| text == "redirect please")
        },
        "the native replay receipt to settle the durable row",
    )
    .await;
    let user = texts(core, MessageRole::User)
        .into_iter()
        .find(|(_, text)| text == "redirect please")
        .unwrap();
    assert_eq!(user.0, id, "transcript row keeps the queue id");
    let steer_line = stdin_users(&fx.project)
        .into_iter()
        .find(|frame| frame["type"] == "user" && frame["message"]["content"] == "redirect please")
        .expect("steer written once");
    assert_eq!(steer_line["uuid"], id, "the native uuid IS the queue id");
    assert_eq!(conversation_spawns(&fx.project).len(), 1, "no second run");
    wait_for(|| idle(core), "the turn to complete").await;
    core.shutdown().await;
}

/// E24: a task-notification result arriving before the queued human prompt
/// does not complete it; the human answer stays in the transcript.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_claude_result_does_not_complete_the_human_turn() {
    let fx = setup("claude-sonnet-5-5", ReasoningLevel::High).await;
    let core = &fx.core;
    core.doc_host
        .queue_message(CHAT, "cc06:foreign opening", Vec::new())
        .unwrap();
    wait_for(
        || {
            texts(core, MessageRole::Assistant)
                .iter()
                .any(|(_, text)| text.contains("human answer"))
        },
        "the human turn's answer to reach the transcript",
    )
    .await;
    wait_for(|| idle(core), "the human turn to complete").await;
    let assistant = texts(core, MessageRole::Assistant);
    let answer = assistant
        .iter()
        .position(|(_, text)| text.contains("human answer"))
        .unwrap();
    let background = assistant
        .iter()
        .position(|(_, text)| text.contains("background report"))
        .expect("the background turn's text is kept");
    assert!(background <= answer, "{assistant:?}");
    core.shutdown().await;
}

fn live_row(snapshot: &serde_json::Value) -> serde_json::Value {
    snapshot["liveSessions"]
        .as_array()
        .expect("liveSessions present")
        .iter()
        .find(|row| row["taskId"] == CHAT)
        .cloned()
        .expect("a live row for the chat")
}

/// E10 + D1/D5: a parked run switches model/effort/fast natively — no
/// restart — and the Usage RPC shows latest-wins cost and observed limits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parked_claude_run_switches_model_natively_and_usage_reports_it() {
    let fx = setup("claude-sonnet-5-5", ReasoningLevel::High).await;
    let core = &fx.core;
    core.doc_host
        .queue_message(CHAT, "cc06:switch opening", Vec::new())
        .unwrap();
    wait_for(|| idle(core), "the first turn to park").await;
    let first_completion = completion(core);
    set_config(core, "claude-opus-5-5", ReasoningLevel::Max, true);
    core.doc_host
        .queue_message(CHAT, "follow-up", Vec::new())
        .unwrap();
    wait_for(
        || {
            texts(core, MessageRole::Assistant)
                .iter()
                .any(|(_, text)| text.contains("echo:follow-up model=claude-opus-5-5 effort=max"))
        },
        "the follow-up to run under the switched runtime",
    )
    .await;
    assert_eq!(
        conversation_spawns(&fx.project).len(),
        1,
        "model/effort change applied without a restart"
    );
    let frames = stdin_users(&fx.project);
    let set_model = frames
        .iter()
        .position(|f| f["request"]["subtype"] == "set_model")
        .expect("set_model sent");
    let prompt = frames
        .iter()
        .position(|f| f["type"] == "user" && f["message"]["content"] == "follow-up")
        .expect("prompt sent");
    assert!(set_model < prompt);

    wait_for(
        || idle(core) && completion(core) != first_completion,
        "the follow-up to complete",
    )
    .await;
    let snapshot = luna_rpc::memory_client(core.rpc_service())
        .call(
            luna_rpc::methods::GET_USAGE_SNAPSHOT,
            serde_json::json!({"days": 1, "includeLimits": true}),
        )
        .await
        .expect("usage snapshot");
    let row = live_row(&snapshot);
    assert_eq!(row["nativeSessionId"], "sess-cc06");
    assert_eq!(row["costUsd"], 0.025, "latest running total, never summed");
    assert_eq!(row["results"], 2);
    assert_eq!(row["tokens"]["cachedInput"], 8000);
    assert_eq!(row["runtime"]["source"], "liveSwitch");
    assert_eq!(row["runtime"]["matches"], true);
    assert_eq!(row["runtime"]["effectiveEffort"], "max");
    assert!(
        snapshot["summary"]["buckets"]
            .as_array()
            .unwrap()
            .is_empty(),
        "live rows never enter history buckets"
    );
    let claude = snapshot["limits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|limit| limit["provider"] == "claude")
        .cloned()
        .expect("claude limit");
    assert_eq!(claude["availability"], "unverified");
    assert!(
        claude["detail"]
            .as_str()
            .unwrap()
            .contains("live Claude session stream")
    );
    let five_hour = claude["windows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == "five_hour")
        .cloned()
        .expect("five-hour window");
    assert!((five_hour["usedPercent"].as_f64().unwrap() - 91.0).abs() < 0.01);
    std::fs::write(
        fx.project.join("usage-snapshot.json"),
        serde_json::to_string_pretty(&snapshot).unwrap(),
    )
    .unwrap();
    if let Ok(dir) = std::env::var("CC06_EVIDENCE_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::copy(
            fx.project.join("usage-snapshot.json"),
            Path::new(&dir).join("e2e-usage-snapshot.json"),
        );
        let _ = std::fs::copy(
            fx.project.join("cc06-fake.log"),
            Path::new(&dir).join("e2e-live-switch-fake.log"),
        );
    }
    core.shutdown().await;
}

/// C2: the CLI rejects the switch; the prompt is never written to the old
/// runtime, and the engine re-dispatches the same message once, fresh, with
/// the new model and the native session resumed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_claude_switch_redispatches_once_with_the_new_model() {
    let fx = setup("claude-sonnet-5-5", ReasoningLevel::High).await;
    let core = &fx.core;
    core.doc_host
        .queue_message(CHAT, "cc06:switch-reject opening", Vec::new())
        .unwrap();
    wait_for(|| idle(core), "the first turn to park").await;
    set_config(core, "claude-opus-5-5", ReasoningLevel::High, false);
    core.doc_host
        .queue_message(CHAT, "follow-up", Vec::new())
        .unwrap();
    wait_for(
        || {
            texts(core, MessageRole::Assistant)
                .iter()
                .any(|(_, text)| text.contains("echo:follow-up model=claude-opus-5-5 effort=high"))
        },
        "the re-dispatched follow-up to run on the new model",
    )
    .await;
    let spawns = conversation_spawns(&fx.project);
    assert_eq!(spawns.len(), 2, "{spawns:?}");
    let second = &spawns[1];
    let model = second.iter().position(|arg| arg == "--model").unwrap();
    assert_eq!(second[model + 1], "claude-opus-5-5");
    assert!(
        second.iter().any(|arg| arg == "--resume=sess-cc06"),
        "native session resumed: {second:?}"
    );
    let follow_lines = stdin_users(&fx.project)
        .into_iter()
        .filter(|f| f["type"] == "user" && f["message"]["content"] == "follow-up")
        .count();
    assert_eq!(follow_lines, 1, "delivered exactly once");
    assert_eq!(
        texts(core, MessageRole::User)
            .iter()
            .filter(|(_, text)| text == "follow-up")
            .count(),
        1
    );
    wait_for(|| idle(core), "the follow-up to complete").await;
    core.shutdown().await;
}
