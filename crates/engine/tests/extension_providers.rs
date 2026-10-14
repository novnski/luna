//! CORE-08 / E10: an independently installed provider package joins the
//! ordinary registry, picker, run, input and Stop paths with no compiled
//! provider id, and every unsupported or broken case is explicit.
//!
//! Real package bytes go through `InstallExtension` and `SetExtensionEnabled`;
//! the registry is read through `WatchProviderCatalog`, `GetProviderCatalog`,
//! `GetProviderModels` and `ListExtensionProviders`; messages and Stop go
//! through the doc command queue the composer uses. The worker is an
//! independently written Python peer of provider profile 1.0
//! (`support/extension_provider_peer.rs`) that starts a real stand-in CLI
//! process. Readback the engine does not produce: process existence from the
//! OS, the peer's own call log in the task checkout, and the package hash
//! recomputed from the published rule. Set `LUNA_CORE08_EVIDENCE_DIR` to
//! retain each test's readback as JSON.
#![cfg(unix)]

#[path = "support/extension_runtime_fixture.rs"]
mod fixture;
#[path = "support/extension_provider_peer.rs"]
mod peer;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fixture::{Engine, MANIFEST, NATIVE_CODE, Package, process_exists, sha256, wait_process_gone};
use luna_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry,
};
use luna_proto::{
    HarnessId, PermissionMode, RecoveryAcceptance, RunRequest, SandboxLevel, TaskRecovery, ToolCall,
};
use peer::{PROVIDER_REGISTER, calls, cli_pid, declaration, provider_package, starts};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(15);

/// A package that must not register: (name, its `luna-provider.json`,
/// whether `provider.register` is required, the grants it is enabled with,
/// the expected refusal reason).
type RefusedCase = (
    &'static str,
    Option<Value>,
    bool,
    &'static [&'static str],
    &'static str,
);

fn evidence(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("LUNA_CORE08_EVIDENCE_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}

fn run_request(prompt: &str, cwd: &Path, provider: &str, mode: PermissionMode) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: Some(HarnessId::parse(provider).unwrap()),
        provider_account_id: None,
        model: Some("peer-fast".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: mode,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: Vec::new(),
        worktree: None,
    }
}

/// A titled task on this device whose checkout is `root/<id>`.
fn task(engine: &Engine, root: &Path, id: &str) -> PathBuf {
    let checkout = root.join(id);
    fs::create_dir_all(&checkout).unwrap();
    engine.create_task(id, &checkout);
    engine
        .core
        .workspace
        .rename_chat(id, "Provider task")
        .unwrap();
    checkout
}

/// Send like the composer does: a `Run` command in the task's doc queue.
fn send(engine: &Engine, chat: &str, message_id: &str, request: RunRequest) {
    engine
        .core
        .doc_host
        .queue_command(
            chat,
            SessionCommandPayload::Run {
                request,
                message_id: message_id.into(),
            },
        )
        .expect("queue run");
}

/// Press Stop like the composer does.
fn stop(engine: &Engine, chat: &str) {
    engine
        .core
        .doc_host
        .queue_command(chat, SessionCommandPayload::Interrupt {})
        .expect("queue interrupt");
}

fn entries(engine: &Engine, chat: &str) -> Vec<SessionMessageEntry> {
    engine
        .core
        .doc_host
        .open(chat)
        .ok()
        .and_then(|handle| handle.doc().read_entries().ok())
        .unwrap_or_default()
}

fn assistant(engine: &Engine, chat: &str) -> Vec<SessionMessageEntry> {
    entries(engine, chat)
        .into_iter()
        .filter(|entry| entry.role == MessageRole::Assistant)
        .collect()
}

fn text_of(entry: &SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Turns that completed cleanly with `needle` in their text: an errored turn
/// is also stamped complete, but carries an error part.
fn completed_with(engine: &Engine, chat: &str, needle: &str) -> usize {
    assistant(engine, chat)
        .iter()
        .filter(|entry| {
            entry.status == Some(MessageStatus::Complete)
                && text_of(entry).contains(needle)
                && !entry
                    .parts
                    .iter()
                    .any(|part| matches!(part, MessagePart::Error { .. }))
        })
        .count()
}

fn errors(engine: &Engine, chat: &str) -> Vec<String> {
    entries(engine, chat)
        .iter()
        .flat_map(|entry| entry.parts.iter())
        .filter_map(|part| match part {
            MessagePart::Error { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

fn has_input_card(engine: &Engine, chat: &str) -> bool {
    entries(engine, chat)
        .iter()
        .flat_map(|entry| entry.parts.iter())
        .any(|part| matches!(part, MessagePart::Input { .. }))
}

async fn wait_until(within: Duration, what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A failure after the provider accepted the message: an error part in the
/// turn's transcript entry.
async fn wait_for_error(engine: &Engine, chat: &str, needle: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(found) = errors(engine, chat)
            .into_iter()
            .find(|message| message.contains(needle))
        {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "no error part containing {needle:?} in {chat}: {:?}",
            errors(engine, chat)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A refusal before the provider accepted the message. As for every
/// provider, no turn started, so it is not a transcript part: the chat shows
/// it as its recovery notice (the transcript's `turn-recovery` trailer
/// renders `recovery.detail`) with the exact reason, and it is never resent
/// automatically.
async fn wait_for_refusal(engine: &Engine, chat: &str, needle: &str) -> TaskRecovery {
    let deadline = Instant::now() + WAIT;
    loop {
        let recovery = engine
            .core
            .sessions
            .session_status(chat)
            .and_then(|session| session.recovery);
        if let Some(recovery) = recovery.as_ref()
            && recovery.detail.contains(needle)
        {
            assert_eq!(
                recovery.acceptance,
                RecoveryAcceptance::RejectedBeforeAcceptance,
                "{recovery:?}"
            );
            assert!(
                !recovery.automatic && recovery.retry_at_ms.is_none(),
                "a refusal was scheduled to be sent again: {recovery:?}"
            );
            return recovery.clone();
        }
        assert!(
            Instant::now() < deadline,
            "no recovery notice containing {needle:?} in {chat}: {recovery:?}; error parts {:?}",
            errors(engine, chat)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn provider_row(catalog: &Value, id: &str) -> Option<Value> {
    catalog["providers"]
        .as_array()?
        .iter()
        .find(|row| row["id"] == id)
        .cloned()
}

async fn catalog(engine: &Engine) -> Value {
    engine
        .call("GetProviderCatalog", json!({}))
        .await
        .expect("catalog")
}

/// The next catalog the open watch delivers that satisfies `wanted`.
async fn watched(watch: &mut mpsc::Receiver<Value>, wanted: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let item = tokio::time::timeout(left, watch.recv())
            .await
            .expect("the open catalog watch did not change in time")
            .expect("the catalog watch ended");
        if item.get("providers").is_some() && wanted(&item) {
            return item;
        }
    }
}

async fn providers(engine: &Engine) -> Vec<Value> {
    engine
        .call("ListExtensionProviders", json!({}))
        .await
        .expect("list extension providers")["providers"]
        .as_array()
        .unwrap()
        .clone()
}

async fn provider_entry(engine: &Engine, id: &str) -> Value {
    providers(engine)
        .await
        .into_iter()
        .find(|entry| entry["providerId"] == id)
        .unwrap_or_else(|| panic!("{id} is not listed by ListExtensionProviders"))
}

fn omitted(entry: &Value) -> Vec<(String, String)> {
    entry["declaration"]["omitted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["capability"].as_str().unwrap().to_owned(),
                item["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

async fn models(engine: &Engine, id: &str) -> Result<Value, String> {
    engine
        .call("GetProviderModels", json!({"harness": id, "refresh": true}))
        .await
}

/// No instance of the package is still running, and every one that exited
/// left no process of its group behind.
async fn nothing_running(engine: &Engine, package: &Package) -> Vec<Value> {
    let deadline = Instant::now() + WAIT;
    loop {
        let views = engine.instances_of(package).await;
        let live = views
            .iter()
            .any(|view| view["state"] == "running" || view["state"] == "starting");
        if !live {
            for view in &views {
                if let Some(exit) = view.get("lastExit").filter(|exit| !exit.is_null()) {
                    assert_eq!(exit["groupCleared"], true, "{view}");
                }
            }
            return views;
        }
        assert!(
            Instant::now() < deadline,
            "an instance of {} is still running: {views:?}",
            package.id
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Enabling adds the provider to the open catalog watch with its declared
/// facts, its models come from the live worker with the package's
/// provenance, a message completes through the composer's queue with the
/// peer's own output, a Full-access approval is settled without a card, the
/// next message resumes the same provider session, and disabling removes
/// the provider while the chat stays readable. Regressions caught: only a
/// catalog row appears; the provider needs a compiled id or a restart;
/// installation alone registers it; a Full run parks an approval card; the
/// CLI keeps running after a turn; continuity silently starts another
/// session; disabling leaves it runnable or loses history.
#[tokio::test(flavor = "multi_thread")]
async fn enabled_provider_package_joins_the_picker_runs_and_leaves_on_disable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let engine = Engine::open(&data);

    let id = "core08.peer/full";
    let package = provider_package(
        &sources,
        "full",
        json!({"approval": true, "session": "peer-session-1"}),
        Some(declaration(id, json!({}))),
        true,
    );
    assert_eq!(
        package.hash,
        sha256(&fs::read(package.dir.join(MANIFEST)).unwrap()),
        "the package hash is the SHA-256 of the manifest bytes"
    );

    let mut watch = engine
        .client
        .subscribe("WatchProviderCatalog", json!({}))
        .await
        .expect("watch the provider catalog");
    let first = watched(&mut watch, |_| true).await;
    assert!(provider_row(&first, id).is_none());

    let installed = engine
        .call("InstallExtension", json!({"path": package.dir}))
        .await
        .expect("install");
    assert_eq!(installed["state"], "disabled");
    assert!(
        provider_row(&catalog(&engine).await, id).is_none(),
        "an installed but disabled package joined the picker"
    );
    engine
        .set_enabled(&package, true, &[NATIVE_CODE, PROVIDER_REGISTER])
        .await
        .expect("enable");

    let joined = watched(&mut watch, |catalog| provider_row(catalog, id).is_some()).await;
    let row = provider_row(&joined, id).unwrap();
    let source = format!("extension:{id}@1.0.0#{}", package.hash);
    assert_eq!(row["name"], "Peer Provider");
    assert_eq!(row["installed"], true);
    assert_eq!(row["enabled"], true);
    assert_eq!(row["available"], true);
    assert_eq!(row["supportsSteering"], false);
    assert_eq!(row["steeringMode"], "turn-boundary");
    assert_eq!(row["capabilities"]["safeRecovery"], true);
    assert_eq!(row["capabilities"]["authoritativePromptEnd"], true);
    assert_eq!(row["capabilities"]["fork"], json!("none"));
    assert_eq!(row["executable"], source);

    let entry = provider_entry(&engine, id).await;
    assert_eq!(entry["state"], "registered");
    assert_eq!(entry["packageHash"], package.hash);
    assert_eq!(entry["trust"], "native");
    assert_eq!(entry["osSandbox"], false);
    assert_eq!(
        entry["declaration"]["optionalProfiles"],
        json!(["resume", "approvals"])
    );
    assert_eq!(entry["declaration"]["permissionModes"], json!(["full"]));
    let gaps = omitted(&entry);
    for capability in ["questions", "attachments", "fork", "lunaTools"] {
        assert!(
            gaps.iter().any(|(name, _)| name == capability),
            "{capability} is not reported as omitted: {gaps:?}"
        );
    }
    assert!(
        gaps.contains(&("steer".into(), "the peer takes one prompt per run".into())),
        "the provider's own reason for steer was replaced: {gaps:?}"
    );

    let harness = HarnessId::parse(id).unwrap();
    let declared = luna_engine::provider_capabilities(harness);
    assert_eq!(declared.supported_modes, vec![PermissionMode::Full]);
    assert!(
        declared
            .known_limitations
            .iter()
            .any(|line| line.contains("does not sandbox")),
        "{:?}",
        declared.known_limitations
    );
    assert_eq!(
        luna_engine::permissions::resolve_preview(harness, PermissionMode::Full).effective_mode,
        PermissionMode::Full
    );

    let listed = models(&engine, id).await.expect("models from the worker");
    let ids: Vec<&str> = listed["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["peer-fast", "peer-deep"]);
    assert_eq!(listed["provenance"]["providerId"], id);
    assert_eq!(listed["provenance"]["executable"], source);
    let described = provider_entry(&engine, id).await["lastDescribe"].clone();
    assert_eq!(described["conforms"], true);
    assert_eq!(described["executable"]["path"], "/peer/cli");

    let chat = "chat-provider-full";
    let checkout = task(&engine, &root, chat);
    send(
        &engine,
        chat,
        "msg-1",
        run_request("say the reply", &checkout, id, PermissionMode::Full),
    );
    wait_until(WAIT, "the first turn to complete", || {
        completed_with(&engine, chat, "PEER-REPLY") == 1
    })
    .await;
    let first_start = starts(&checkout);
    assert_eq!(first_start.len(), 1, "{first_start:?}");
    assert_eq!(first_start[0]["prompt"], "say the reply");
    assert_eq!(first_start[0]["permissionMode"], "full");
    assert_eq!(first_start[0]["model"], "peer-fast");
    assert_eq!(first_start[0]["resume"], Value::Null);
    assert!(
        calls(&checkout).contains(&r#"approval {"decision": "allow"}"#.to_owned()),
        "Full access did not allow the approval once: {:?}",
        calls(&checkout)
    );
    assert!(
        !has_input_card(&engine, chat),
        "a Full run parked an input or approval card"
    );
    let tool = assistant(&engine, chat)
        .iter()
        .flat_map(|entry| entry.parts.clone())
        .find_map(|part| match part {
            MessagePart::Tool { call, resolved, .. } => Some((call, resolved)),
            _ => None,
        })
        .expect("the tool call reached the transcript");
    assert!(
        matches!(&tool.0, ToolCall::Exec { command } if command == "echo peer"),
        "{tool:?}"
    );
    assert!(tool.1, "the tool result was not applied");
    let cli = cli_pid(&checkout, WAIT).await;
    assert!(
        wait_process_gone(cli, WAIT).await,
        "the stand-in CLI survived the completed turn"
    );
    let after_first = nothing_running(&engine, &package).await;

    send(
        &engine,
        chat,
        "msg-2",
        run_request("again", &checkout, id, PermissionMode::Full),
    );
    wait_until(WAIT, "the second turn to complete", || {
        completed_with(&engine, chat, "PEER-REPLY") == 2
    })
    .await;
    let both = starts(&checkout);
    assert_eq!(both.len(), 2, "{both:?}");
    assert_eq!(
        both[1]["resume"], "peer-session-1",
        "the follow-up did not continue the provider's own session"
    );

    engine
        .set_enabled(&package, false, &[])
        .await
        .expect("disable");
    let left = watched(&mut watch, |catalog| provider_row(catalog, id).is_none()).await;
    assert!(providers(&engine).await.is_empty());
    let refused_models = models(&engine, id).await.expect_err("models after disable");
    let refused_run = engine
        .core
        .sessions
        .dispatch(
            chat,
            harness,
            run_request("after disable", &checkout, id, PermissionMode::Full),
            None,
        )
        .await
        .expect_err("a run after disable")
        .to_string();
    assert_eq!(
        starts(&checkout).len(),
        2,
        "a disabled provider was started"
    );
    assert_eq!(
        completed_with(&engine, chat, "PEER-REPLY"),
        2,
        "disabling changed the chat's history"
    );

    engine
        .set_enabled(&package, true, &[NATIVE_CODE, PROVIDER_REGISTER])
        .await
        .expect("enable again");
    watched(&mut watch, |catalog| provider_row(catalog, id).is_some()).await;
    models(&engine, id)
        .await
        .expect("models after enabling again");

    evidence(
        "provider_joins_runs_and_leaves",
        &json!({
            "packageHash": package.hash,
            "catalogRow": row,
            "providerEntry": entry,
            "models": listed,
            "starts": both,
            "calls": calls(&checkout),
            "instancesAfterFirstTurn": after_first,
            "catalogAfterDisable": left,
            "refusedModels": refused_models,
            "refusedRun": refused_run,
        }),
    );
    drop(watch);
    engine.close().await;
}

/// Broken packages never pass for working providers. Regressions caught: a
/// wrong worker major, a missing mandatory operation, a description of
/// another provider or an unavailable CLI turns into an empty model list or a
/// silent run; a package without a declaration, with an invalid one, with
/// an unsupported profile or without the register grant still registers.
#[tokio::test(flavor = "multi_thread")]
async fn conformance_failures_are_explicit_and_leave_nothing_running() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let engine = Engine::open(&data);
    let mut readback = Vec::new();

    let registered_but_broken: [(&str, Value, &str); 4] = [
        ("major2", json!({"hello": "major2"}), "failed its handshake"),
        (
            "no-stop",
            json!({"operations": [
                "provider.describe", "provider.models", "provider.run.start",
                "provider.run.events", "provider.run.answer"
            ]}),
            "required operation provider.run.stop",
        ),
        (
            "other-id",
            json!({"describe": {"providerId": "someone.else/provider"}}),
            "describes provider someone.else/provider",
        ),
        (
            "unavailable",
            json!({"describe": {"available": false, "unavailableReason": "the peer CLI is not installed"}}),
            "the peer CLI is not installed",
        ),
    ];
    for (name, fixture, expected) in registered_but_broken {
        let id = format!("core08.peer/{name}");
        let package = provider_package(
            &sources,
            name,
            fixture,
            Some(declaration(&id, json!({}))),
            true,
        );
        engine
            .install_enabled(&package, &[NATIVE_CODE, PROVIDER_REGISTER])
            .await;
        assert!(
            provider_row(&catalog(&engine).await, &id).is_some(),
            "{name}: a valid declaration did not register"
        );
        let error = models(&engine, &id).await.expect_err(name);
        assert!(error.contains(expected), "{name}: {error}");

        let chat = format!("chat-{name}");
        let checkout = task(&engine, &root, &chat);
        send(
            &engine,
            &chat,
            "msg-1",
            run_request("hello", &checkout, &id, PermissionMode::Full),
        );
        let shown = wait_for_refusal(&engine, &chat, "Peer Provider")
            .await
            .detail;
        // The same reason as discovery, unless repeated handshake faults have
        // quarantined the package by then (which is also stated).
        assert!(
            shown.contains(expected) || shown.contains("quarantined"),
            "{name}: {shown}"
        );
        assert_eq!(
            completed_with(&engine, &chat, "PEER-REPLY"),
            0,
            "{name}: a turn completed"
        );
        assert!(
            starts(&checkout).is_empty(),
            "{name}: the prompt reached the worker"
        );
        assert!(
            !checkout.join("peer-cli.pid").exists(),
            "{name}: a CLI started"
        );
        let views = nothing_running(&engine, &package).await;
        readback.push(json!({"case": name, "models": error, "run": shown, "instances": views}));
    }

    let refused: [RefusedCase; 4] = [
        (
            "undeclared",
            None,
            true,
            &[NATIVE_CODE, PROVIDER_REGISTER],
            "lists no luna-provider.json",
        ),
        (
            "bad-declaration",
            Some(json!({"declarationVersion": 1, "providers": [], "installHook": "run me"})),
            true,
            &[NATIVE_CODE, PROVIDER_REGISTER],
            "luna-provider.json is invalid",
        ),
        (
            "profile2",
            Some(declaration(
                "core08.peer/profile2",
                json!({"profile": {"major": 2, "minor": 0}}),
            )),
            true,
            &[NATIVE_CODE, PROVIDER_REGISTER],
            "provider profile 2.0",
        ),
        (
            "ungranted",
            Some(declaration("core08.peer/ungranted", json!({}))),
            false,
            &[NATIVE_CODE],
            "is not granted",
        ),
    ];
    for (name, declared, register_required, grants, expected) in refused {
        let id = format!("core08.peer/{name}");
        let package = provider_package(&sources, name, json!({}), declared, register_required);
        engine.install_enabled(&package, grants).await;
        assert!(
            provider_row(&catalog(&engine).await, &id).is_none(),
            "{name}: a refused provider joined the picker"
        );
        let entry = provider_entry(&engine, &id).await;
        assert_eq!(entry["state"], "refused", "{name}: {entry}");
        let reason = entry["reason"].as_str().unwrap_or_default().to_owned();
        assert!(reason.contains(expected), "{name}: {reason}");
        assert!(
            engine.instances_of(&package).await.is_empty(),
            "{name}: a refused provider started a worker"
        );
        readback.push(json!({"case": name, "entry": entry}));
    }

    evidence("conformance_failures", &json!(readback));
    engine.close().await;
}

/// Stop and failures end the turn honestly. Regressions caught: Stop leaves
/// the CLI running, including when the worker ignores both its stop and
/// SIGTERM; a crash mid-run is reported as complete, replayed, shown as two
/// identical error parts, or leaves the CLI behind; a start the worker
/// refused is resent.
#[tokio::test(flavor = "multi_thread")]
async fn stop_and_crash_end_the_turn_honestly_and_end_the_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let engine = Engine::open(&data);
    let mut readback = Vec::new();

    for (name, scenario, marker) in [
        ("hang", "hang", "stopped"),
        ("ignore-stop", "ignoreStop", "ignored-stop"),
    ] {
        let id = format!("core08.peer/{name}");
        let package = provider_package(
            &sources,
            name,
            json!({"run": scenario}),
            Some(declaration(&id, json!({}))),
            true,
        );
        engine
            .install_enabled(&package, &[NATIVE_CODE, PROVIDER_REGISTER])
            .await;
        let chat = format!("chat-{name}");
        let checkout = task(&engine, &root, &chat);
        send(
            &engine,
            &chat,
            "msg-1",
            run_request("work for a while", &checkout, &id, PermissionMode::Full),
        );
        wait_until(WAIT, "the run's first output", || {
            assistant(&engine, &chat)
                .iter()
                .any(|entry| text_of(entry).contains("PEER-REPLY"))
        })
        .await;
        let cli = cli_pid(&checkout, WAIT).await;
        assert!(process_exists(cli), "{name}: the CLI is not running");
        let pressed = Instant::now();
        stop(&engine, &chat);
        assert!(
            wait_process_gone(cli, WAIT).await,
            "{name}: Stop left the CLI running"
        );
        let stopped_in = pressed.elapsed();
        wait_until(WAIT, "the turn to end aborted", || {
            assistant(&engine, &chat)
                .iter()
                .any(|entry| entry.status == Some(MessageStatus::Aborted))
        })
        .await;
        let log = calls(&checkout);
        assert!(log.contains(&"stop".to_owned()), "{name}: {log:?}");
        assert!(log.contains(&marker.to_owned()), "{name}: {log:?}");
        assert_eq!(
            starts(&checkout).len(),
            1,
            "{name}: the run was started again"
        );
        assert_eq!(
            completed_with(&engine, &chat, "PEER-REPLY"),
            0,
            "{name}: Stop completed the turn"
        );
        let views = nothing_running(&engine, &package).await;
        readback.push(json!({
            "case": name,
            "stoppedInMs": stopped_in.as_millis() as u64,
            "calls": log,
            "instances": views,
        }));
    }

    let id = "core08.peer/crash";
    let crash = provider_package(
        &sources,
        "crash",
        json!({"run": "crash"}),
        Some(declaration(id, json!({}))),
        true,
    );
    engine
        .install_enabled(&crash, &[NATIVE_CODE, PROVIDER_REGISTER])
        .await;
    let chat = "chat-crash";
    let checkout = task(&engine, &root, chat);
    send(
        &engine,
        chat,
        "msg-1",
        run_request("crash please", &checkout, id, PermissionMode::Full),
    );
    let shown = wait_for_error(&engine, chat, "exited during the run").await;
    assert!(shown.contains("not replayed"), "{shown}");
    wait_until(WAIT, "the crashed turn to end", || {
        !engine.core.sessions.has_live_run(chat)
    })
    .await;
    assert_eq!(
        errors(&engine, chat),
        [shown.clone()],
        "the crash is not the turn's one error part"
    );
    let cli = cli_pid(&checkout, WAIT).await;
    assert!(
        wait_process_gone(cli, WAIT).await,
        "the crashed worker's CLI survived"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(starts(&checkout).len(), 1, "the crashed run was replayed");
    assert_eq!(
        completed_with(&engine, chat, "PEER-REPLY"),
        0,
        "a crash completed the turn"
    );
    let crash_views = nothing_running(&engine, &crash).await;
    readback.push(json!({"case": "crash", "error": shown, "instances": crash_views}));

    let id = "core08.peer/reject";
    let reject = provider_package(
        &sources,
        "reject",
        json!({"start": "reject"}),
        Some(declaration(id, json!({}))),
        true,
    );
    engine
        .install_enabled(&reject, &[NATIVE_CODE, PROVIDER_REGISTER])
        .await;
    let chat = "chat-reject";
    let checkout = task(&engine, &root, chat);
    send(
        &engine,
        chat,
        "msg-1",
        run_request("refuse please", &checkout, id, PermissionMode::Full),
    );
    let shown = wait_for_refusal(&engine, chat, "the peer refused before starting its CLI")
        .await
        .detail;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(starts(&checkout).len(), 1, "a refused start was sent again");
    assert!(!checkout.join("peer-cli.pid").exists());
    nothing_running(&engine, &reject).await;
    readback.push(json!({"case": "reject", "error": shown}));

    evidence("stop_and_crash", &json!(readback));
    engine.close().await;
}

/// What a provider does not support is omitted, reported and refused, never
/// improvised. Regressions caught: Auto offered without a reviewer; legacy
/// Ask, a stored session without the resume profile, or an approval without
/// the approvals profile reaching the CLI; a resume that continues another
/// session accepted as continuity.
#[tokio::test(flavor = "multi_thread")]
async fn unsupported_optional_capabilities_are_omitted_and_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let engine = Engine::open(&data);

    let id = "core08.peer/narrow";
    let narrow = provider_package(
        &sources,
        "narrow",
        json!({}),
        Some(declaration(
            id,
            json!({
                "optional": [],
                "permissionModes": {
                    "full": "the peer runs every action",
                    "auto": "the peer approves edits only",
                    "ask": "the peer asks in its terminal",
                },
                "automaticReviewer": false,
            }),
        )),
        true,
    );
    engine
        .install_enabled(&narrow, &[NATIVE_CODE, PROVIDER_REGISTER])
        .await;
    let entry = provider_entry(&engine, id).await;
    assert_eq!(entry["state"], "registered");
    assert_eq!(entry["declaration"]["permissionModes"], json!(["full"]));
    let gaps = omitted(&entry);
    let reason = |capability: &str| {
        gaps.iter()
            .find(|(name, _)| name == capability)
            .map(|(_, reason)| reason.clone())
            .unwrap_or_else(|| panic!("{capability} is not reported: {gaps:?}"))
    };
    assert!(reason("auto").contains("automatic reviewer"));
    assert!(reason("ask").contains("legacy Ask"));
    assert!(reason("resume").contains("saved conversation"));
    assert!(reason("approvals").contains("approval"));
    let harness = HarnessId::parse(id).unwrap();
    assert_eq!(
        luna_engine::provider_capabilities(harness).supported_modes,
        vec![PermissionMode::Full]
    );

    let chat = "chat-narrow";
    let checkout = task(&engine, &root, chat);
    let auto = engine
        .core
        .sessions
        .dispatch(
            chat,
            harness,
            run_request("auto please", &checkout, id, PermissionMode::Auto),
            None,
        )
        .await
        .expect_err("Auto without a reviewer")
        .to_string();
    assert!(auto.contains("hidden Ask"), "{auto}");
    // A legacy Ask record reaches the bridge, which refuses it before any
    // process starts (its own chat: the refusal records a recovery there).
    let ask_chat = "chat-narrow-ask";
    let ask_checkout = task(&engine, &root, ask_chat);
    send(
        &engine,
        ask_chat,
        "msg-ask",
        run_request("ask please", &ask_checkout, id, PermissionMode::Ask),
    );
    let ask = wait_for_refusal(&engine, ask_chat, "does not offer legacy Ask")
        .await
        .detail;
    assert!(
        starts(&ask_checkout).is_empty() && starts(&checkout).is_empty(),
        "a refused mode reached the worker"
    );

    send(
        &engine,
        chat,
        "msg-1",
        run_request("first", &checkout, id, PermissionMode::Full),
    );
    wait_until(WAIT, "the narrow provider's first turn", || {
        completed_with(&engine, chat, "PEER-REPLY") == 1
    })
    .await;
    send(
        &engine,
        chat,
        "msg-2",
        run_request("second", &checkout, id, PermissionMode::Full),
    );
    let resume = wait_for_refusal(&engine, chat, "cannot continue a saved conversation")
        .await
        .detail;
    assert_eq!(
        starts(&checkout).len(),
        1,
        "a stored session reached a provider without the resume profile"
    );

    let id = "core08.peer/no-approvals";
    let no_approvals = provider_package(
        &sources,
        "no-approvals",
        json!({"run": "approvalWithoutProfile"}),
        Some(declaration(id, json!({"optional": ["resume"]}))),
        true,
    );
    engine
        .install_enabled(&no_approvals, &[NATIVE_CODE, PROVIDER_REGISTER])
        .await;
    let chat = "chat-no-approvals";
    let checkout = task(&engine, &root, chat);
    send(
        &engine,
        chat,
        "msg-1",
        run_request("needs approval", &checkout, id, PermissionMode::Full),
    );
    let approval = wait_for_error(&engine, chat, "without declaring the approvals profile").await;
    let cli = cli_pid(&checkout, WAIT).await;
    assert!(
        wait_process_gone(cli, WAIT).await,
        "the CLI survived the refusal"
    );
    assert!(!has_input_card(&engine, chat), "an approval became a card");
    nothing_running(&engine, &no_approvals).await;

    let id = "core08.peer/wrong-session";
    let wrong = provider_package(
        &sources,
        "wrong-session",
        json!({"run": "wrongSession", "session": "peer-session-w"}),
        Some(declaration(id, json!({}))),
        true,
    );
    engine
        .install_enabled(&wrong, &[NATIVE_CODE, PROVIDER_REGISTER])
        .await;
    let chat = "chat-wrong-session";
    let checkout = task(&engine, &root, chat);
    send(
        &engine,
        chat,
        "msg-1",
        run_request("first", &checkout, id, PermissionMode::Full),
    );
    wait_until(WAIT, "the first turn of the resuming provider", || {
        completed_with(&engine, chat, "PEER-REPLY") == 1
    })
    .await;
    send(
        &engine,
        chat,
        "msg-2",
        run_request("second", &checkout, id, PermissionMode::Full),
    );
    let split = wait_for_error(&engine, chat, "instead of this chat's session").await;
    let both = starts(&checkout);
    assert_eq!(both.len(), 2, "{both:?}");
    assert_eq!(both[1]["resume"], "peer-session-w");
    assert_eq!(
        completed_with(&engine, chat, "PEER-REPLY"),
        1,
        "a run on another session completed"
    );
    nothing_running(&engine, &wrong).await;

    evidence(
        "unsupported_optional_capabilities",
        &json!({
            "narrowEntry": entry,
            "auto": auto,
            "ask": ask,
            "resume": resume,
            "approvalWithoutProfile": approval,
            "wrongSession": split,
        }),
    );
    engine.close().await;
}
