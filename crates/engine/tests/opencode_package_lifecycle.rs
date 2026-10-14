//! CORE-11 / E11: OpenCode through its first-party provider package.
//!
//! A real engine installs and enables `luna.provider/opencode` from the
//! `luna-opencode-worker` binary Cargo built for this target (the bundled
//! worker), exactly as LUNA Dev does from its app bundle, and replaces the
//! compiled OpenCode registration. Everything then goes through the
//! ordinary paths: `GetProviderModels`, the doc command queue (`Run`,
//! `Steer`, `Interrupt`), `SetExtensionEnabled`, `SetExtensionSafeMode` and
//! engine restarts. The worker hosts LUNA's own OpenCode 2 adapter, which
//! talks to a scripted OpenCode 2 server (`support/fake-opencode-package.py`)
//! that records what it was asked to do and asserts nothing.
//!
//! Readback the engine does not produce: the server's own records in each
//! checkout, process existence from the OS (the server and the worker that
//! owns it, its parent), the instance list and the installed package files.
//! Set `LUNA_CORE11_EVIDENCE_DIR` to keep each test's readback as JSON.
#![cfg(unix)]

#[path = "support/opencode_package.rs"]
mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use luna_doc::MessageStatus;
use luna_proto::{HarnessId, PermissionMode, RecoveryAcceptance};
use serde_json::{Value, json};
use support::*;

const NATIVE_CODE: &str = "luna.capability/native-code";
const PROVIDER_REGISTER: &str = "luna.capability/provider.register";

async fn models(engine: &Engine) -> Result<Value, String> {
    engine
        .call(
            "GetProviderModels",
            json!({"harness": "opencode", "refresh": true}),
        )
        .await
}

/// The latest scripted server in `checkout` (after `boots` boots) and the
/// process that started it.
async fn latest_server(checkout: &Path, boots: usize) -> (u64, u64) {
    let boot = server_boots(checkout, boots)
        .await
        .last()
        .cloned()
        .expect("a boot record");
    (
        boot["pid"].as_u64().expect("server pid"),
        boot["ppid"].as_u64().expect("server parent pid"),
    )
}

/// The package instance whose worker is (or was) `pid`.
async fn instance_with_pid(engine: &Engine, pid: u64) -> Option<Value> {
    engine.instances().await.into_iter().find(|view| {
        view["pid"].as_u64() == Some(pid) || view["lastExit"]["pid"].as_u64() == Some(pid)
    })
}

/// A failure the chat shows: an error part in the turn, or the chat's
/// recovery notice for a message that never became a turn.
async fn wait_for_notice(engine: &Engine, chat: &str, needle: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(found) = engine
            .errors(chat)
            .into_iter()
            .find(|message| message.contains(needle))
        {
            return found;
        }
        if let Some(recovery) = engine.recovery(chat)
            && recovery.detail.contains(needle)
        {
            return recovery.detail;
        }
        assert!(
            Instant::now() < deadline,
            "no notice containing {needle:?} in {chat}: errors {:?}, recovery {:?}",
            engine.errors(chat),
            engine.recovery(chat)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn kill(pid: u64) {
    // SAFETY: plain kill(2) on a worker process this test found by pid.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

fn strings(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Models come from the live worker with the package's provenance; a turn
/// runs in its own worker that owns its `opencode serve`; a steer reaches
/// the running turn on the same OpenCode session; Stop reaches OpenCode's
/// own interrupt and ends the server; a provider failure, a worker crash
/// and a start whose answer was lost are each shown honestly and never
/// resent; Full answers OpenCode's stray ask once and legacy Ask shows the
/// adapter's own card. Regressions caught: models from a static row; a turn
/// served by the compiled adapter or by an engine-owned server; a steer
/// dropped or queued as a new turn; Stop that leaves `opencode serve`
/// running; a crash or lost start that completes, replays or resends the
/// message; a crash shown as two identical error parts (the bridge's own
/// end reported both as an error event and on its `Done`); a Full run that
/// parks an approval card.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turns_steer_stop_and_fail_honestly_through_the_package() {
    environment(true);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let engine = Engine::open(&data, builtin_registry());

    let providers = engine.providers().await;
    let status = providers["firstParty"][0].clone();
    assert_eq!(status["replacesBuiltin"], true, "{status}");
    assert_eq!(status["enabled"], true, "{status}");
    let package = engine.package().await.expect("the package is installed");
    let source = format!(
        "extension:{PACKAGE_ID}@{}#{}",
        package["version"].as_str().unwrap(),
        package["packageHash"].as_str().unwrap()
    );

    // Models: the worker's live discovery, with the package's provenance.
    let listed = models(&engine).await.expect("models through the package");
    let ids: BTreeSet<String> = listed["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| model["id"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        ids,
        BTreeSet::from([
            "fake/package-model".to_owned(),
            "fake/second-model".to_owned()
        ])
    );
    assert_eq!(listed["provenance"]["executable"], source);
    let variants = listed["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == MODEL)
        .map(|model| model["reasoningLevels"].clone())
        .unwrap();
    assert_eq!(
        variants,
        json!(["low", "high"]),
        "OpenCode's variants map to effort"
    );
    let service = engine
        .instances()
        .await
        .into_iter()
        .find(|view| view["state"] == "running")
        .expect("model discovery runs in a package worker");
    assert_eq!(service["isolation"]["osSandbox"], false);
    assert!(
        strings(&service["isolation"]["notEnforced"]).contains("environmentAllowList")
            && !strings(&service["isolation"]["enforced"]).contains("environmentAllowList"),
        "the first-party worker's inherited environment is not reported: {}",
        service["isolation"]
    );

    // A turn, a steer during it, then Stop.
    let chat = "chat-steer";
    let checkout = engine.task(&root, chat);
    engine.send(
        chat,
        "steer-1",
        run_request("scenario:slow", &checkout, PermissionMode::Full),
    );
    wait_until(WAIT, "the first ticks", || {
        engine.texts(chat).contains("tick 2")
    })
    .await;
    let (server, worker) = latest_server(&checkout, 1).await;
    assert!(
        process_exists(server),
        "the turn's opencode serve is not running"
    );
    assert_ne!(
        worker,
        u64::from(std::process::id()),
        "the engine itself owns the turn's opencode serve"
    );
    let owner = instance_with_pid(&engine, worker)
        .await
        .expect("the turn's server belongs to a package worker");
    assert_eq!(owner["state"], "running", "{owner}");
    assert_ne!(
        owner["instanceId"], service["instanceId"],
        "the turn shares the service worker"
    );
    engine.steer(chat, "steer-2", "focus on the tests");
    wait_until(WAIT, "the steer to reach the running turn", || {
        let texts = engine.texts(chat);
        texts.contains("steered: ") && texts.contains("focus on the tests")
    })
    .await;
    let prompts = records(&checkout, "prompts");
    let turn = prompts
        .iter()
        .find(|prompt| prompt["delivery"] != "steer")
        .cloned()
        .unwrap();
    let steer = prompts
        .iter()
        .find(|prompt| prompt["delivery"] == "steer")
        .cloned()
        .expect("the steer reached OpenCode as a native steer");
    assert_eq!(
        steer["session"], turn["session"],
        "the steer went to another session"
    );
    assert!(
        steer["text"]
            .as_str()
            .unwrap_or_default()
            .contains("focus on the tests"),
        "{steer}"
    );
    let pressed = Instant::now();
    engine.stop(chat);
    wait_until(WAIT, "the turn to end aborted", || {
        engine
            .assistant(chat)
            .iter()
            .any(|entry| entry.status == Some(MessageStatus::Aborted))
    })
    .await;
    assert!(
        wait_process_gone(server, WAIT).await,
        "Stop left opencode serve running"
    );
    let stopped_in = pressed.elapsed();
    assert!(
        records(&checkout, "interrupts")
            .iter()
            .any(|interrupt| interrupt["running"] == true),
        "Stop never reached OpenCode's own interrupt"
    );
    assert_eq!(turn_prompts(&checkout).len(), 1, "the turn was sent again");
    wait_until(WAIT, "the turn's worker to stop", || {
        !process_exists(worker)
    })
    .await;

    // A provider failure: OpenCode's own error, on an errored turn.
    let chat = "chat-fail";
    let checkout = engine.task(&root, chat);
    engine.send(
        chat,
        "fail-1",
        run_request("scenario:fail", &checkout, PermissionMode::Full),
    );
    let failure = wait_for_notice(&engine, chat, "fake provider refused the request").await;
    let (server, _) = latest_server(&checkout, 1).await;
    assert!(
        wait_process_gone(server, WAIT).await,
        "a failed turn left its server"
    );
    wait_until(WAIT, "the failed turn to settle", || engine.settled(chat)).await;

    // The worker crashes mid-turn: the turn errors, nothing is replayed,
    // and the server of the crashed worker's group is gone.
    let chat = "chat-crash";
    let checkout = engine.task(&root, chat);
    engine.send(
        chat,
        "crash-1",
        run_request("scenario:slow", &checkout, PermissionMode::Full),
    );
    wait_until(WAIT, "the crash turn's first tick", || {
        engine.texts(chat).contains("tick 1")
    })
    .await;
    let (server, worker) = latest_server(&checkout, 1).await;
    kill(worker);
    let crash = wait_for_notice(&engine, chat, "the turn was not replayed").await;
    assert!(crash.contains("the worker exited"), "{crash}");
    wait_until(WAIT, "the crashed turn to settle", || engine.settled(chat)).await;
    assert_eq!(
        engine.errors(chat),
        [crash.clone()],
        "the crash is not the turn's one error part"
    );
    assert!(
        wait_process_gone(server, WAIT).await,
        "the crashed worker's opencode serve survived"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        turn_prompts(&checkout).len(),
        1,
        "the crashed turn was replayed"
    );
    assert_eq!(
        records(&checkout, "servers").len(),
        1,
        "the crashed turn was restarted"
    );

    // The worker dies before the adapter confirmed the run: delivery is
    // unknown, the message is never sent again.
    let chat = "chat-held";
    let checkout = engine.task(&root, chat);
    fs::write(checkout.join(".fake-opencode-hold-boot"), b"").unwrap();
    engine.send(
        chat,
        "held-1",
        run_request("Reply with PONG", &checkout, PermissionMode::Full),
    );
    let (server, worker) = latest_server(&checkout, 1).await;
    kill(worker);
    let unconfirmed = wait_for_notice(&engine, chat, "before confirming the run").await;
    assert!(
        unconfirmed.contains("does not send it again"),
        "{unconfirmed}"
    );
    assert!(
        wait_process_gone(server, WAIT).await,
        "the held server survived"
    );
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(
        records(&checkout, "servers").len(),
        1,
        "an unconfirmed start was sent again"
    );
    assert!(
        turn_prompts(&checkout).is_empty(),
        "the held server got a prompt"
    );

    // Full access: the adapter answers OpenCode's stray ask once; no card.
    let chat = "chat-full-ask";
    let checkout = engine.task(&root, chat);
    engine.send(
        chat,
        "ask-1",
        run_request("scenario:ask", &checkout, PermissionMode::Full),
    );
    wait_until(WAIT, "the Full ask to be answered", || {
        engine.texts(chat).contains("permission answered:")
    })
    .await;
    let replies = records(&checkout, "replies");
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0]["decision"], "once", "{replies:?}");
    assert!(
        !engine.has_approval_card(chat),
        "a Full run parked an approval card"
    );

    // Legacy Ask: the adapter's own approval card, answered by nobody yet.
    let chat = "chat-ask";
    let checkout = engine.task(&root, chat);
    engine.send(
        chat,
        "ask-2",
        run_request("scenario:ask", &checkout, PermissionMode::Ask),
    );
    wait_until(WAIT, "the legacy Ask card", || {
        engine.has_approval_card(chat)
    })
    .await;
    assert!(
        records(&checkout, "replies").is_empty(),
        "the card was answered for the user"
    );
    engine.stop(chat);
    wait_until(WAIT, "the Ask turn to end", || engine.settled(chat)).await;

    evidence(
        "turns_steer_stop_and_fail",
        &json!({
            "firstParty": status,
            "package": package,
            "models": listed,
            "serviceInstance": service,
            "turnInstance": owner,
            "steerPrompt": steer,
            "stoppedInMs": stopped_in.as_millis() as u64,
            "failure": failure,
            "crash": crash,
            "unconfirmedStart": unconfirmed,
            "fullAskReplies": replies,
        }),
    );
    engine.close().await;
}

/// The package is OpenCode's only path. Disabling removes OpenCode from the
/// catalog and refuses a send without touching the chat; the compiled
/// adapter does not come back, not even after a restart; enabling restores
/// it and the chat continues in its own OpenCode session; safe mode refuses
/// the send before anything starts; a restart keeps the installed bytes
/// and the user's choice. Regressions caught: disable falls back to the
/// built-in; a restart re-enables a package the user disabled or reinstalls
/// unchanged bytes; safe mode starts a worker; continuity starts a new
/// OpenCode session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disable_enable_safe_mode_and_restarts_keep_one_opencode_path() {
    environment(true);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let engine = Engine::open(&data, builtin_registry());
    let package = engine.package().await.expect("installed");
    let hash = package["packageHash"].as_str().unwrap().to_owned();
    let installed_at = package["installedAtMs"].clone();

    let chat = "chat-life";
    let checkout = engine.task(&root, chat);
    engine.send(
        chat,
        "life-1",
        run_request("Reply with PONG", &checkout, PermissionMode::Full),
    );
    wait_until(WAIT, "the first turn", || {
        engine.texts(chat).contains("PONG 1")
    })
    .await;
    wait_until(WAIT, "the first turn to settle", || engine.settled(chat)).await;
    let session = turn_prompts(&checkout)[0]["session"].clone();

    // Disable: no OpenCode row, a refused send, an unchanged chat.
    let disable = json!({
        "extensionId": PACKAGE_ID,
        "packageHash": hash,
        "enabled": false,
        "grants": [],
    });
    engine
        .call("SetExtensionEnabled", disable.clone())
        .await
        .expect("disable");
    assert!(
        opencode_rows(&engine.catalog().await).is_empty(),
        "disabling left OpenCode in the catalog"
    );
    let refused = engine
        .core
        .sessions
        .dispatch(
            chat,
            HarnessId::Opencode,
            run_request("after disable", &checkout, PermissionMode::Full),
            None,
        )
        .await
        .expect_err("a send to a disabled OpenCode")
        .to_string();
    assert_eq!(
        turn_prompts(&checkout).len(),
        1,
        "a disabled OpenCode was started"
    );
    assert!(
        engine.texts(chat).contains("PONG 1"),
        "disabling changed the chat"
    );
    engine.close().await;

    // Restart: the user's disable holds, and the compiled adapter stays out.
    let engine = Engine::open(&data, builtin_registry());
    let status = engine.providers().await["firstParty"][0].clone();
    assert_eq!(status["installed"], true, "{status}");
    assert_eq!(
        status["enabled"], false,
        "a restart re-enabled what the user disabled: {status}"
    );
    assert_eq!(status["enabledByDefault"], false, "{status}");
    assert_eq!(status["replacesBuiltin"], true, "{status}");
    assert!(
        opencode_rows(&engine.catalog().await).is_empty(),
        "the compiled OpenCode adapter came back while its package is installed"
    );
    let reopened = engine.package().await.expect("still installed");
    assert_eq!(
        reopened["packageHash"],
        hash.as_str(),
        "unchanged bytes were reinstalled"
    );
    assert_eq!(
        reopened["installedAtMs"], installed_at,
        "unchanged bytes were reinstalled"
    );

    // Enable: the chat continues in the same OpenCode session.
    let mut enable = disable.clone();
    enable["enabled"] = json!(true);
    enable["grants"] = json!([NATIVE_CODE, PROVIDER_REGISTER]);
    engine
        .call("SetExtensionEnabled", enable)
        .await
        .expect("enable");
    assert_eq!(
        opencode_rows(&engine.catalog().await).len(),
        1,
        "enabling did not restore OpenCode"
    );
    engine.send(
        chat,
        "life-2",
        run_request("Reply with PONG", &checkout, PermissionMode::Full),
    );
    wait_until(WAIT, "the turn after enabling", || {
        engine.texts(chat).contains("PONG 2")
    })
    .await;
    wait_until(WAIT, "it to settle", || engine.settled(chat)).await;
    assert_eq!(
        turn_prompts(&checkout)[1]["session"],
        session,
        "re-enabling continued in another OpenCode session"
    );

    // Safe mode: refused before anything starts (its own chat: the refusal
    // records a recovery there).
    engine
        .call("SetExtensionSafeMode", json!({"enabled": true}))
        .await
        .expect("safe mode on");
    let safe_chat = "chat-safe";
    let safe_checkout = engine.task(&root, safe_chat);
    engine.send(
        safe_chat,
        "safe-1",
        run_request("Reply with PONG", &safe_checkout, PermissionMode::Full),
    );
    let safe = wait_for_notice(&engine, safe_chat, "safe mode").await;
    if let Some(recovery) = engine.recovery(safe_chat) {
        assert_eq!(
            recovery.acceptance,
            RecoveryAcceptance::RejectedBeforeAcceptance,
            "{recovery:?}"
        );
    }
    assert!(
        records(&safe_checkout, "servers").is_empty(),
        "safe mode started OpenCode"
    );
    engine
        .call("SetExtensionSafeMode", json!({"enabled": false}))
        .await
        .expect("safe mode off");
    engine.send(
        chat,
        "life-4",
        run_request("Reply with PONG", &checkout, PermissionMode::Full),
    );
    wait_until(WAIT, "the turn after safe mode", || {
        engine.texts(chat).contains("PONG 3")
    })
    .await;

    evidence(
        "disable_enable_safe_mode_restart",
        &json!({
            "packageHash": hash,
            "refusedWhileDisabled": refused,
            "afterRestart": status,
            "safeMode": safe,
            "prompts": records(&checkout, "prompts"),
        }),
    );
    engine.close().await;
}
