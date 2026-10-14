//! CORE-11 / E11: moving OpenCode into its first-party provider package
//! changes neither what a chat shows nor which chat and OpenCode session a
//! message belongs to.
//!
//! One test, two engines on the same data root, in sequence:
//!
//! 1. **Built-in era** (`LUNA_OPENCODE_WORKER=off`): nothing is installed
//!    and the compiled OpenCode registration serves, its adapter inside the
//!    engine. A chat is answered, then every scenario below runs.
//! 2. **Package era** (the bundled worker): opening the engine installs and
//!    enables `luna.provider/opencode` from the binary Cargo built for this
//!    target and removes the compiled registration. The old chat continues
//!    in the same OpenCode session, and every scenario runs again.
//!
//! The expected side of each comparison is the retained in-process adapter,
//! the behavior being preserved, never the new path. Both eras talk to the
//! same scripted OpenCode 2 server, which records and asserts nothing. The
//! scenarios cover the wire cases the extraction could break: ordinary
//! streaming, text after the execution's terminal frame, a malformed frame
//! and an unknown event, an SSE reconnect mid-turn, a provider failure,
//! OpenCode's stray permission ask in Full access, and session rules that
//! did not apply (the run must not start).
#![cfg(unix)]

#[path = "support/opencode_package.rs"]
mod support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use luna_proto::PermissionMode;
use serde_json::{Value, json};
use support::*;

/// (name, prompt, mode, checkout markers)
type Scenario = (
    &'static str,
    String,
    PermissionMode,
    &'static [&'static str],
);

const NO_MARKERS: &[&str] = &[];
const DROP_RULES: &[&str] = &[".fake-opencode-drop-rules"];

fn scenarios() -> Vec<Scenario> {
    let full = PermissionMode::Full;
    vec![
        ("reply", "Reply with PONG".to_owned(), full, NO_MARKERS),
        ("late", "scenario:late".to_owned(), full, NO_MARKERS),
        (
            "malformed",
            "scenario:malformed".to_owned(),
            full,
            NO_MARKERS,
        ),
        (
            "reconnect",
            "scenario:reconnect".to_owned(),
            full,
            NO_MARKERS,
        ),
        ("fail", "scenario:fail".to_owned(), full, NO_MARKERS),
        ("full-ask", "scenario:ask".to_owned(), full, NO_MARKERS),
        (
            "rules-not-applied",
            "Reply with PONG".to_owned(),
            full,
            DROP_RULES,
        ),
        // A 60 KB prompt and a 100 KB reply: both larger than one frame.
        (
            "large",
            format!("scenario:large {}", "a".repeat(60 * 1024)),
            full,
            NO_MARKERS,
        ),
    ]
}

/// Run every scenario in its own chat; what each chat shows, plus what the
/// server was asked to do.
async fn run_scenarios(engine: &Engine, root: &Path, era: &str) -> Vec<(String, Value)> {
    let mut seen = Vec::new();
    for (name, prompt, mode, markers) in scenarios() {
        let chat = format!("parity-{name}-{era}");
        let checkout = engine.task(root, &chat);
        for marker in markers {
            fs::write(checkout.join(marker), b"").unwrap();
        }
        engine.send(&chat, "msg-1", run_request(&prompt, &checkout, mode));
        wait_until(WAIT, &format!("{chat} to settle"), || engine.settled(&chat)).await;
        // Frames the server sends after the turn's end must have no effect.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let replies: Vec<Value> = records(&checkout, "replies")
            .iter()
            .map(|reply| reply["decision"].clone())
            .collect();
        seen.push((
            name.to_owned(),
            json!({
                "transcript": transcript(engine, &chat, &checkout),
                "prompts": turn_prompts(&checkout).len(),
                "steers": records(&checkout, "prompts").len() - turn_prompts(&checkout).len(),
                "replies": replies,
            }),
        ));
    }
    seen
}

/// What OpenCode reports outside a turn for `chat`: its slash commands and
/// skills, and its native extension view (fresh readbacks).
async fn outside_turns(engine: &Engine, chat: &str) -> Value {
    let commands = engine
        .call(
            "ListCommands",
            json!({"harness": "opencode", "chatId": chat}),
        )
        .await
        .expect("commands");
    let extensions = engine
        .call(
            "GetNativeExtensions",
            json!({"harness": "opencode", "chatId": chat, "refresh": true}),
        )
        .await
        .expect("native extensions");
    json!({"commands": commands, "extensions": extensions})
}

/// Regressions caught: the package path changes streaming, terminal,
/// malformed-frame, reconnect, failure, stray-ask or rule-readback
/// behavior; an existing chat gets a new OpenCode session, provider spelling
/// or a second OpenCode registration; the turn's server is still owned by
/// the engine; the installed worker is not the bundled binary.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_package_keeps_the_builtin_behavior_and_the_chat_identity() {
    environment(false);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");

    // ---- Built-in era --------------------------------------------------
    let engine = Engine::open(&data, builtin_registry());
    let before = engine.providers().await["firstParty"][0].clone();
    assert_eq!(before["bundle"], "off", "{before}");
    assert_eq!(before["installed"], false, "{before}");
    assert!(
        engine.package().await.is_none(),
        "the package was installed while off"
    );
    let rows = opencode_rows(&engine.catalog().await);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0]["executable"]
            .as_str()
            .is_none_or(|source| !source.starts_with("extension:")),
        "the built-in era already ran OpenCode through a package: {rows:?}"
    );

    let old = "chat-old";
    // A project chat: its out-of-turn reads resolve through its project.
    let old_checkout = engine.project_task(&root, old);
    engine.send(
        old,
        "old-1",
        run_request("Reply with PONG", &old_checkout, PermissionMode::Full),
    );
    wait_until(WAIT, "the built-in turn", || {
        engine.texts(old).contains("PONG 1")
    })
    .await;
    wait_until(WAIT, "the built-in turn to settle", || engine.settled(old)).await;
    let first = turn_prompts(&old_checkout);
    assert_eq!(first.len(), 1, "{first:?}");
    let session = first[0]["session"].clone();
    let boots = records(&old_checkout, "servers");
    assert_eq!(
        boots[0]["ppid"].as_u64(),
        Some(u64::from(std::process::id())),
        "the built-in adapter's server is the engine's own child"
    );
    let builtin_outside = outside_turns(&engine, old).await;
    let builtin = run_scenarios(&engine, &root, "builtin").await;
    engine.close().await;

    // ---- Package era ---------------------------------------------------
    bundle_worker();
    let engine = Engine::open(&data, builtin_registry());
    let providers = engine.providers().await;
    let status = providers["firstParty"][0].clone();
    assert_eq!(status["packageId"], PACKAGE_ID, "{status}");
    assert_eq!(status["providerId"], "opencode", "{status}");
    assert_eq!(status["bundle"], "installed", "{status}");
    assert_eq!(status["enabled"], true, "{status}");
    assert_eq!(status["enabledByDefault"], true, "{status}");
    assert_eq!(status["replacesBuiltin"], true, "{status}");
    let package = engine.package().await.expect("the package is installed");
    assert_eq!(package["state"], "enabled", "{package}");
    let hash = package["packageHash"].as_str().unwrap().to_owned();
    assert_eq!(status["packageHash"], hash.as_str());

    // The installed package is the bundled worker, pinned by its manifest.
    let installed = data.join("extensions").join("packages").join(&hash);
    let manifest_bytes = fs::read(installed.join("luna-plugin.json")).unwrap();
    assert_eq!(
        sha256(&manifest_bytes),
        hash,
        "the package hash is the manifest's SHA-256"
    );
    let manifest: Value = serde_json::from_slice(&manifest_bytes).unwrap();
    let entry = manifest["runtime"]["entries"]
        .as_object()
        .and_then(|entries| entries.values().next())
        .and_then(Value::as_str)
        .expect("a native entry point")
        .to_owned();
    assert_eq!(
        sha256(&fs::read(installed.join(&entry)).unwrap()),
        sha256(&fs::read(worker_binary()).unwrap()),
        "the installed worker is not the bundled binary"
    );

    let registered = providers["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["providerId"] == "opencode")
        .cloned()
        .expect("OpenCode is a registered package provider");
    assert_eq!(registered["state"], "registered", "{registered}");
    assert_eq!(registered["extensionId"], PACKAGE_ID);
    assert_eq!(registered["trust"], "native");
    assert_eq!(registered["osSandbox"], false);
    assert_eq!(registered["declaration"]["profile"]["major"], 2);
    assert_eq!(registered["declaration"]["hosted"]["fork"], "checkpoint");
    assert_eq!(
        registered["declaration"]["permissionModes"],
        json!(["ask", "full"])
    );

    let rows = opencode_rows(&engine.catalog().await);
    assert_eq!(rows.len(), 1, "two OpenCode registrations: {rows:?}");
    let row = &rows[0];
    let source = format!(
        "extension:{PACKAGE_ID}@{}#{hash}",
        package["version"].as_str().unwrap()
    );
    assert_eq!(row["executable"], source.as_str(), "{row}");
    assert_eq!(row["name"], "OpenCode");
    assert_eq!(row["supportsSteering"], true);
    assert_eq!(row["steeringMode"], "step-boundary");
    assert_eq!(
        row["reasoningLevels"],
        json!(["low", "medium", "high", "xhigh", "max"])
    );
    assert_eq!(row["capabilities"]["fork"], "checkpoint");
    assert_eq!(row["capabilities"]["nativeChildStop"], true);
    assert_eq!(row["capabilities"]["authoritativePromptEnd"], true);
    assert_eq!(row["capabilities"]["safeRecovery"], false);
    assert_eq!(row["capabilities"]["runtimeDelivery"], "openCodeMcpTools");

    // The old chat continues in the same OpenCode session, from a worker.
    engine.send(
        old,
        "old-2",
        run_request("Reply with PONG", &old_checkout, PermissionMode::Full),
    );
    wait_until(WAIT, "the package turn", || {
        engine.texts(old).contains("PONG 2")
    })
    .await;
    wait_until(WAIT, "the package turn to settle", || engine.settled(old)).await;
    let both = turn_prompts(&old_checkout);
    assert_eq!(both.len(), 2, "{both:?}");
    assert_eq!(
        both[1]["session"], session,
        "the package continued an existing chat in another OpenCode session"
    );
    let boots = records(&old_checkout, "servers");
    assert_ne!(
        boots.last().unwrap()["ppid"].as_u64(),
        Some(u64::from(std::process::id())),
        "the package turn's server is still the engine's own child"
    );
    let packaged_outside = outside_turns(&engine, old).await;
    assert_eq!(
        packaged_outside, builtin_outside,
        "commands or native extensions differ on the package path"
    );

    let packaged = run_scenarios(&engine, &root, "package").await;
    let mut compared = Vec::new();
    for ((name, expected), (_, observed)) in builtin.iter().zip(&packaged) {
        assert_eq!(
            observed, expected,
            "{name}: the package path differs from the built-in adapter"
        );
        compared.push(json!({"scenario": name, "builtin": expected, "package": observed}));
    }

    evidence(
        "package_parity",
        &json!({
            "builtInEra": before,
            "packageEra": status,
            "package": package,
            "catalogRow": row,
            "oldChatPrompts": both,
            "outsideTurns": packaged_outside,
            "scenarios": compared,
        }),
    );
    engine.close().await;
}
