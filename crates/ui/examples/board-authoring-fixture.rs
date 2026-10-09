//! Headless GPUI E2E for Discuss with AI (A6): the production `Shell` on the
//! Board page, driven with real pointer and keyboard input against an
//! in-process `EngineCore` whose Claude Code harness runs the deterministic
//! protocol peer `harness/tests/fixtures/fake-claude-board-authoring.py`
//! (it reads a real project file through the harness and calls the Board
//! tools over the per-process `luna` endpoint). No provider, account,
//! network or credential is used.
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. Discuss with AI is unreachable from the Board page, or opens a form:
//!    any text input besides the one composer (a title or description
//!    field, hidden or not) exists in this mode.
//! 2. Typed text does not reach the composer, Enter or Send does not send
//!    it, or a message shows twice.
//! 3. The assistant's reply, the read-only proposed ticket or the saved
//!    receipt (key, Backlog, Open ticket) is missing; clicking the proposal
//!    and typing edits it.
//! 4. The discussion assistant controls do not show the harness, model and
//!    reasoning, still offer an access control (a discussion's access is
//!    fixed: it reads, searches and runs commands and never edits files), or
//!    a Full implementation request in the conversation changes the
//!    discussion's own access.
//! 5. Close leaves the pane open or the runtime working, or the unfinished
//!    discussion is not listed as a Board draft (or appears as a chat).
//! 6. Resume opens an empty or different discussion (transcript or
//!    proposal lost).
//! 7. Save then close keeps the session listed, or the ticket is missing.
//! 8. Discard (from the pane or the drafts list) leaves the draft listed.
//! 9. The discussion is not the chat UI: its messages are not rows of the
//!    main transcript (user row, tool group, markdown reply), or the
//!    composer is not the main composer (no model pill, access lock or
//!    Send), or the old bespoke controls remain.
//! 10. A model picked in the composer's pill does not reach the engine's
//!     discussion profile (or its next run), or changes the proposed
//!     ticket's own settings.
//! 11. An image dropped on the discussion is not staged in its composer, is
//!     not uploaded with the message (no `Attached images` block, no
//!     thumbnail row), or never reaches the assistant's run as an image.
//! 12. The model the composer's pill shows is not the one that runs: a
//!     discussion opened without a model keeps none, so the provider runs
//!     its own default instead.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example board-authoring-fixture
//! Run:   target/debug/examples/board-authoring-fixture [ABSOLUTE_EVIDENCE_DIR]
//! (default `target/qa/board/authoring/fixture/`). Writes PNG captures,
//! readback.json, manifest.json (steps with expected/observed and SHA-256 of
//! each readback) and board-authoring.pass; a failure writes
//! failure.txt/failure.png and exits nonzero.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    AppContext, AsyncApp, Bounds, Entity, Keystroke, MouseButton, PlatformInput, WindowBounds,
    WindowHandle, WindowOptions, point, px, size,
};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::ClaudeHarness;
use luna_proto::HarnessId;
use luna_ui::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const PROJECT: &str = "fixture-board";
const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 1100.0;
const TITLE: &str = "Show open drafts on the Board";

type Win = WindowHandle<shell::Shell>;

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn draw(window: Win, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
    })?;
    Ok(())
}

async fn frames(window: Win, cx: &mut AsyncApp, ms: u64) -> anyhow::Result<()> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        draw(window, cx)?;
        settle(cx, 16).await;
    }
    draw(window, cx)
}

fn capture(window: Win, cx: &mut AsyncApp, path: PathBuf) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })??;
    Ok(())
}

fn input(window: Win, cx: &mut AsyncApp, event: PlatformInput) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(event, cx);
    })?;
    Ok(())
}

async fn click(window: Win, cx: &mut AsyncApp, at: (f32, f32)) -> anyhow::Result<()> {
    let position = point(px(at.0), px(at.1));
    input(
        window,
        cx,
        PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Default::default(),
        }),
    )?;
    frames(window, cx, 60).await?;
    input(
        window,
        cx,
        PlatformInput::MouseDown(gpui::MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        }),
    )?;
    input(
        window,
        cx,
        PlatformInput::MouseUp(gpui::MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count: 1,
        }),
    )?;
    frames(window, cx, 250).await
}

async fn keys(window: Win, cx: &mut AsyncApp, combos: &[&str]) -> anyhow::Result<()> {
    for combo in combos {
        let keystroke = Keystroke::parse(combo)?;
        gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
            window.dispatch_keystroke(keystroke, cx);
        })?;
        draw(window, cx)?;
        settle(cx, 15).await;
    }
    frames(window, cx, 150).await
}

/// Type lowercase text into the focused field, one key at a time.
async fn type_text(window: Win, cx: &mut AsyncApp, text: &str) -> anyhow::Result<()> {
    let combos: Vec<String> = text
        .chars()
        .map(|c| {
            if c == ' ' {
                "space".into()
            } else {
                c.to_string()
            }
        })
        .collect();
    let combos: Vec<&str> = combos.iter().map(String::as_str).collect();
    keys(window, cx, &combos).await
}

/// The open Discuss with AI pane's readback (`None` when no pane is open).
fn pane_readback(window: Win, cx: &mut AsyncApp) -> anyhow::Result<Option<Value>> {
    draw(window, cx)?;
    window.update(cx, |shell, window, cx| {
        shell
            .fixture_board_page()
            .and_then(|page| page.read(cx).authoring_pane())
            .map(|pane| pane.read(cx).fixture_readback(window, cx))
    })
}

fn drafts_readback(window: Win, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    draw(window, cx)?;
    window
        .update(cx, |shell, _, cx| {
            shell
                .fixture_board_page()
                .map(|page| page.read(cx).authoring_drafts().read(cx).fixture_readback())
        })?
        .ok_or_else(|| anyhow::anyhow!("the Board page is not mounted"))
}

fn board_keys(window: Win, cx: &mut AsyncApp) -> anyhow::Result<Vec<String>> {
    window.update(cx, |shell, _, cx| {
        shell
            .fixture_board_page()
            .map(|page| {
                page.read(cx)
                    .model()
                    .read(cx)
                    .items()
                    .iter()
                    .map(|view| view.item.key.clone())
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The engine's chat registry as the sidebar's `WatchChats` sees it.
async fn engine_chats(
    state: &Entity<state::AppState>,
    cx: &mut AsyncApp,
) -> anyhow::Result<Vec<String>> {
    let engine = cx
        .update(|cx| state.read(cx).engine().cloned())
        .ok_or_else(|| anyhow::anyhow!("no engine attached"))?;
    let mut watch = engine
        .client()
        .subscribe("WatchChats", json!({}))
        .await
        .map_err(|error| anyhow::anyhow!("WatchChats: {error}"))?;
    let first = watch.recv().await.unwrap_or_default();
    Ok(first
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|chat| chat["id"].as_str().map(str::to_owned))
        .collect())
}

fn center(readback: &Value, label: &str) -> anyhow::Result<(f32, f32)> {
    let bounds = &readback["bounds"][label];
    anyhow::ensure!(!bounds.is_null(), "no bounds for {label}: {readback:#}");
    let n = |key: &str| bounds[key].as_f64().unwrap_or(f64::NAN) as f32;
    let (x, y, w, h) = (n("x"), n("y"), n("w"), n("h"));
    anyhow::ensure!(
        w > 0.0 && h > 0.0 && y >= 0.0 && y + h / 2.0 < HEIGHT,
        "{label} is not on screen: {bounds}"
    );
    Ok((x + w / 2.0, y + h / 2.0))
}

/// Poll a readback until `ready` holds (bounded).
async fn wait_for(
    window: Win,
    cx: &mut AsyncApp,
    what: &str,
    mut read: impl FnMut(Win, &mut AsyncApp) -> anyhow::Result<Option<Value>>,
    ready: impl Fn(&Value) -> bool,
) -> anyhow::Result<Value> {
    let start = Instant::now();
    let mut last = Value::Null;
    loop {
        if let Some(value) = read(window, cx)? {
            if ready(&value) {
                return Ok(value);
            }
            last = value;
        }
        if start.elapsed() >= Duration::from_secs(60) {
            // Name the step that broke: the last readback carries the
            // composer text/focus, Send state, pending message and trace.
            let mut detail = serde_json::to_string(&json!({
                "composerText": last["composerText"],
                "composerFocused": last["composerFocused"],
                "sendEnabled": last["sendEnabled"],
                "pending": last["pending"],
                "working": last["working"],
                "sending": last["sending"],
                "state": last["state"],
                "notice": last["notice"],
                "trace": last["trace"],
            }))?;
            detail.truncate(4000);
            anyhow::bail!("timed out waiting for {what}; last readback: {detail}");
        }
        frames(window, cx, 100).await?;
    }
}

/// Center of one of the composer's picker chips (`model`, `access`).
fn chip(readback: &Value, name: &str) -> anyhow::Result<(f32, f32)> {
    let rect = readback["composer"]["pickers"]["chips"][name]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("no {name} chip: {}", readback["composer"]["pickers"]))?;
    let n = |i: usize| rect.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
    Ok((n(0) + n(2) / 2.0, n(1) + n(3) / 2.0))
}

/// Drop files on the window the way the platform does (enter, hover, drop).
async fn drop_files(
    window: Win,
    cx: &mut AsyncApp,
    at: (f32, f32),
    paths: Vec<PathBuf>,
) -> anyhow::Result<()> {
    let position = point(px(at.0), px(at.1));
    let events = [
        gpui::FileDropEvent::Entered {
            position,
            paths: gpui::ExternalPaths(paths.into()),
        },
        gpui::FileDropEvent::Pending { position },
        gpui::FileDropEvent::Submit { position },
    ];
    for event in events {
        input(window, cx, PlatformInput::FileDrop(event))?;
        frames(window, cx, 80).await?;
    }
    frames(window, cx, 300).await
}

/// The fake peer's per-turn evidence (`.board-authoring-results-N.json`).
fn peer_turns(project: &Path) -> Vec<Value> {
    (0..32)
        .filter_map(|turn| {
            let path = project.join(format!(".board-authoring-results-{turn}.json"));
            std::fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        })
        .collect()
}

fn entries(readback: &Value, role: &str) -> Vec<String> {
    readback["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry["role"] == role)
                .map(|entry| entry["text"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Steps with expected/observed values and readback hashes.
struct Evidence {
    steps: Vec<Value>,
}

impl Evidence {
    fn step(
        &mut self,
        name: &str,
        expected: Value,
        observed: Value,
        snapshot: &Value,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            observed == expected,
            "{name}: expected {expected:#}, observed {observed:#}"
        );
        self.steps.push(json!({
            "step": name,
            "expected": expected,
            "observed": observed,
            "snapshotSha256": format!("{:x}", Sha256::digest(serde_json::to_vec(snapshot)?)),
            "passed": true,
        }));
        Ok(())
    }
}

async fn send_message(
    window: Win,
    cx: &mut AsyncApp,
    text: &str,
    with_enter: bool,
) -> anyhow::Result<Value> {
    let before = pane_readback(window, cx)?.unwrap_or_default();
    let users = entries(&before, "user").len();
    click(window, cx, center(&before, "composer")?).await?;
    let focused = pane_readback(window, cx)?.unwrap_or_default();
    anyhow::ensure!(
        focused["composerFocused"] == json!(true),
        "clicking the composer did not focus it: {}",
        json!({"composerFocused": focused["composerFocused"], "trace": focused["trace"]})
    );
    type_text(window, cx, text).await?;
    let typed = pane_readback(window, cx)?.unwrap_or_default();
    anyhow::ensure!(
        typed["composerText"] == json!(text) && typed["sendEnabled"] == json!(true),
        "typed text did not reach an enabled composer: {}",
        json!({
            "composerText": typed["composerText"],
            "composerFocused": typed["composerFocused"],
            "sendEnabled": typed["sendEnabled"],
            "trace": typed["trace"],
        })
    );
    if with_enter {
        keys(window, cx, &["enter"]).await?;
    } else {
        click(window, cx, center(&typed, "send")?).await?;
    }
    wait_for(
        window,
        cx,
        "the assistant's reply",
        pane_readback,
        |readback| {
            readback["working"] == json!(false)
                && readback["sending"] == json!(false)
                && entries(readback, "user").len() == users + 1
                && entries(readback, "assistant").len() > entries(&before, "assistant").len()
        },
    )
    .await
}

async fn scenario(
    window: Win,
    cx: &mut AsyncApp,
    output: &Path,
    project: &Path,
    state: Entity<state::AppState>,
) -> anyhow::Result<Value> {
    let mut evidence = Evidence { steps: Vec::new() };
    cx.update(|cx| motion::set_reduced_motion(cx, true));
    frames(window, cx, 900).await?;

    // Board page → Discuss with AI.
    let drafts = wait_for(
        window,
        cx,
        "the Board drafts card",
        |w, cx| drafts_readback(w, cx).map(Some),
        |r| !r["bounds"]["discuss"].is_null(),
    )
    .await?;
    capture(window, cx, output.join("01-board.png"))?;
    click(window, cx, center(&drafts, "discuss")?).await?;
    let opened = wait_for(window, cx, "the discussion to open", pane_readback, |r| {
        r["status"] == "Ready" && r["sessionId"].is_string()
    })
    .await?;
    capture(window, cx, output.join("02-discuss.png"))?;
    // The assistant's settings are the chat composer's own pickers.
    let chips = &opened["composer"]["pickers"]["chips"];
    let controls = !chips["model"].is_null() && chips["access"].is_null();
    evidence.step(
        "Discuss with AI is a conversation only: one composer, no title or description field, a model pill and no access control",
        json!({"textInputs": ["composer"], "state": "active", "proposal": null, "harness": "claude-code", "controls": true}),
        json!({
            "textInputs": opened["textInputs"],
            "state": opened["state"],
            "proposal": opened["proposal"],
            "harness": opened["discussion"]["harness"],
            "controls": controls,
        }),
        &opened,
    )?;
    // The model the pill shows is recorded on the discussion.
    let recorded = wait_for(window, cx, "the shown model recorded", pane_readback, |r| {
        r["discussion"]["model"].is_string() && r["busy"] == json!(false)
    })
    .await?;
    let shown_model = recorded["discussion"]["model"].clone();
    evidence.step(
        "the model the pill shows is recorded on the discussion without a notice",
        json!({"notice": null, "recorded": true}),
        json!({"notice": recorded["notice"], "recorded": shown_model.is_string()}),
        &recorded,
    )?;
    let session = opened["sessionId"].as_str().unwrap_or_default().to_owned();

    // Turn 1 (Enter): the peer reads the project and proposes a ticket.
    let first = send_message(window, cx, "the board forgets which drafts are open", true).await?;
    capture(window, cx, output.join("03-proposal.png"))?;
    evidence.step(
        "a sent message, the assistant's reply and a read-only proposed ticket",
        json!({"users": ["the board forgets which drafts are open"], "replied": true, "read": true, "proposal": TITLE, "readOnly": true, "rendered": true, "composer": ""}),
        json!({
            "users": entries(&first, "user"),
            "replied": entries(&first, "assistant").iter().any(|text| text.to_lowercase().contains("project defaults")),
            "read": first["entries"].as_array().into_iter().flatten().any(|entry| entry["tools"].as_array().into_iter().flatten().any(|tool| tool.as_str().is_some_and(|t| t.contains("board.rs")))),
            "proposal": first["proposal"]["title"],
            "readOnly": first["proposal"]["readOnly"],
            "rendered": first["proposal"]["rendered"],
            "composer": first["composerText"],
        }),
        &first,
    )?;
    // The conversation is the main chat's transcript and composer.
    let rows = first["transcriptRows"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let kinds: Vec<&str> = rows.iter().filter_map(|row| row["kind"].as_str()).collect();
    let user_id = first["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["role"] == "user")
        .map(|entry| entry["id"].clone())
        .unwrap_or_default();
    let bespoke = [
        "authoring-harness",
        "authoring-model",
        "authoring-reasoning",
        "authoring-access",
    ]
    .iter()
    .any(|label| !first["bounds"][*label].is_null());
    evidence.step(
        "the discussion is the chat UI: transcript rows and the chat composer",
        json!({"userRow": true, "tools": true, "markdown": true, "model": true, "access": true, "send": true, "bespoke": false}),
        json!({
            "userRow": rows.iter().any(|row| row["kind"] == "user" && row["entryId"] == user_id),
            "tools": kinds.contains(&"tools"),
            "markdown": kinds.contains(&"markdown"),
            "model": chip(&first, "model").is_ok(),
            "access": chip(&first, "access").is_err(),
            "send": !first["bounds"]["send"].is_null(),
            "bespoke": bespoke,
        }),
        &first,
    )?;
    let turns = peer_turns(project);
    evidence.step(
        "the first run uses the model the pill showed",
        json!({"model": shown_model}),
        json!({"model": turns.first().map(|turn| turn["model"].clone()).unwrap_or_default()}),
        &first,
    )?;
    // Clicking the proposal and typing changes nothing.
    click(window, cx, center(&first, "proposal")?).await?;
    type_text(window, cx, "zz").await?;
    let untouched = pane_readback(window, cx)?.unwrap_or_default();
    evidence.step(
        "the proposal is not editable",
        json!({"proposal": TITLE, "textInputs": ["composer"]}),
        json!({"proposal": untouched["proposal"]["title"], "textInputs": untouched["textInputs"]}),
        &untouched,
    )?;

    // Close unfinished → a Board draft; the pane leaves.
    click(window, cx, center(&untouched, "close")?).await?;
    let listed = wait_for(
        window,
        cx,
        "the draft to be listed",
        |w, cx| {
            let open = pane_readback(w, cx)?;
            let drafts = drafts_readback(w, cx)?;
            Ok(open.is_none().then_some(drafts))
        },
        |r| r["drafts"].as_array().is_some_and(|d| !d.is_empty()),
    )
    .await?;
    capture(window, cx, output.join("04-draft-listed.png"))?;
    let chats = engine_chats(&state, cx).await?;
    evidence.step(
        "close keeps only a resumable Board draft, never a chat",
        json!({"drafts": [{"id": session, "state": "draft", "title": TITLE}], "chats": []}),
        json!({"drafts": listed["drafts"], "chats": chats}),
        &listed,
    )?;

    // Resume → the same discussion; then settings and save.
    click(window, cx, center(&listed, "resume-0")?).await?;
    let resumed = wait_for(window, cx, "the draft to reopen", pane_readback, |r| {
        r["status"] == "Ready" && r["state"] == "active" && !entries(r, "user").is_empty()
    })
    .await?;
    evidence.step(
        "resume reopens the same discussion with its transcript and proposal",
        json!({"session": session, "users": ["the board forgets which drafts are open"], "proposal": TITLE}),
        json!({"session": resumed["sessionId"], "users": entries(&resumed, "user"), "proposal": resumed["proposal"]["title"]}),
        &resumed,
    )?;
    // The assistant's model through the composer's pill: search, Enter.
    let before_model = resumed["discussion"]["model"].clone();
    let proposal_before = resumed["proposal"]["implementation"].clone();
    click(window, cx, chip(&resumed, "model")?).await?;
    wait_for(window, cx, "the model menu", pane_readback, |r| {
        r["composer"]["pickers"]["open"] == "HarnessModel"
    })
    .await?;
    type_text(window, cx, "sonnet").await?;
    let listed = wait_for(window, cx, "a sonnet model", pane_readback, |r| {
        r["composer"]["pickers"]["modelRows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] != before_model))
    })
    .await?;
    let wanted = listed["composer"]["pickers"]["modelRows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["id"] != before_model)
        .map(|row| row["id"].clone())
        .unwrap_or_default();
    let index = listed["composer"]["pickers"]["modelRows"]
        .as_array()
        .and_then(|rows| rows.iter().position(|row| row["id"] == wanted))
        .unwrap_or(0);
    for _ in 0..index {
        keys(window, cx, &["down"]).await?;
    }
    keys(window, cx, &["enter"]).await?;
    let picked = wait_for(
        window,
        cx,
        "the discussion's new model",
        pane_readback,
        |r| r["discussion"]["model"] == wanted && r["busy"] == json!(false),
    )
    .await?;
    keys(window, cx, &["escape"]).await?;
    evidence.step(
        "a model picked in the composer's pill changes the discussion, not the proposal",
        json!({"model": wanted, "proposal": proposal_before}),
        json!({"model": picked["discussion"]["model"], "proposal": picked["proposal"]["implementation"]}),
        &picked,
    )?;
    let settings = send_message(
        window,
        cx,
        "use claude code with full access for the implementation",
        true,
    )
    .await?;
    let turns = peer_turns(project);
    evidence.step(
        "the next run uses the picked model",
        json!({"model": wanted}),
        json!({"model": turns.last().map(|turn| turn["model"].clone()).unwrap_or_default()}),
        &settings,
    )?;
    evidence.step(
        "a Full implementation request leaves the discussion's own access unchanged",
        json!({"implementation": "full", "discussion": "ask"}),
        json!({
            "implementation": settings["proposal"]["implementation"]["permissionMode"],
            "discussion": settings["discussion"]["permissionMode"],
        }),
        &settings,
    )?;
    // Turn 3 (Send button): save.
    send_message(window, cx, "create the ticket", false).await?;
    let saved = wait_for(window, cx, "the saved receipt", pane_readback, |r| {
        r["receipts"]
            .as_array()
            .is_some_and(|receipts| receipts.iter().any(|receipt| receipt["key"].is_string()))
    })
    .await?;
    capture(window, cx, output.join("05-receipt.png"))?;
    let receipt = &saved["receipts"][0];
    evidence.step(
        "the saved receipt shows the real key in Backlog with Open ticket; the proposal summary gives way to it",
        json!({"receipts": 1, "column": "backlog", "keyed": true, "open": true, "state": "saved", "rendered": false}),
        json!({
            "receipts": saved["receipts"].as_array().map_or(0, Vec::len),
            "column": receipt["column"],
            "keyed": receipt["key"].as_str().is_some_and(|key| key.contains('-')),
            "open": !saved["bounds"]["open-ticket-0"].is_null(),
            "state": saved["state"],
            "rendered": saved["proposal"]["rendered"],
        }),
        &saved,
    )?;
    let key = receipt["key"].clone();
    // Save then close: the session ends and leaves the drafts list.
    click(window, cx, center(&saved, "close")?).await?;
    let ended = wait_for(
        window,
        cx,
        "the pane to close",
        |w, cx| {
            let open = pane_readback(w, cx)?;
            let drafts = drafts_readback(w, cx)?;
            Ok(open.is_none().then_some(drafts))
        },
        |r| r["drafts"].as_array().is_some_and(Vec::is_empty),
    )
    .await?;
    let on_board = wait_for(
        window,
        cx,
        "the ticket on the Board",
        |w, cx| Ok(Some(json!(board_keys(w, cx)?))),
        |keys| keys.as_array().is_some_and(|keys| keys.contains(&key)),
    )
    .await?;
    let chats = engine_chats(&state, cx).await?;
    evidence.step(
        "save then close ends the discussion; the ticket stays on the Board and no chat exists",
        json!({"drafts": [], "ticket": true, "chats": []}),
        json!({"drafts": ended["drafts"], "ticket": on_board.as_array().is_some_and(|keys| keys.contains(&key)), "chats": chats}),
        &ended,
    )?;

    // A second discussion: close it, then Discard it from the drafts list.
    click(window, cx, center(&ended, "discuss")?).await?;
    wait_for(window, cx, "a second discussion", pane_readback, |r| {
        r["status"] == "Ready" && r["sessionId"].is_string() && r["sessionId"] != json!(session)
    })
    .await?;
    // A screenshot dropped on the discussion is staged in its composer.
    let screenshot = output.join("drop-screenshot.png");
    let mut image = image::RgbaImage::new(24, 16);
    for (x, _, pixel) in image.enumerate_pixels_mut() {
        *pixel = image::Rgba([40 + (x as u8) * 8, 90, 200, 255]);
    }
    image.save(&screenshot)?;
    let ready = pane_readback(window, cx)?.unwrap_or_default();
    drop_files(
        window,
        cx,
        center(&ready, "composer")?,
        vec![screenshot.clone()],
    )
    .await?;
    let staged = wait_for(window, cx, "the staged screenshot", pane_readback, |r| {
        r["composer"]["attachments"]
            .as_array()
            .is_some_and(|names| !names.is_empty())
    })
    .await?;
    let staged_names = staged["composer"]["attachments"].clone();
    let sent = send_message(window, cx, "also the list is slow", true).await?;
    let user_text = entries(&sent, "user").last().cloned().unwrap_or_default();
    let image_rows = sent["transcriptRows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["kind"] == "user")
        .filter_map(|row| row["attachments"].as_u64())
        .max()
        .unwrap_or(0);
    let turns = peer_turns(project);
    let peer_types = turns
        .iter()
        .find(|turn| {
            turn["prompt"]
                .as_str()
                .is_some_and(|prompt| prompt.contains("also the list is slow"))
        })
        .map(|turn| turn["contentTypes"].clone())
        .unwrap_or_default();
    evidence.step(
        "a dropped image is staged, uploaded with the message, shown as a thumbnail and reaches the run",
        json!({"staged": 1, "attachedBlock": true, "thumbnails": 1, "runImage": true, "composer": []}),
        json!({
            "staged": staged_names.as_array().map_or(0, Vec::len),
            "attachedBlock": user_text.contains("Attached images (local files"),
            "thumbnails": image_rows,
            "runImage": peer_types
                .as_array()
                .is_some_and(|types| types.iter().any(|kind| kind == "image")),
            "composer": sent["composer"]["attachments"],
        }),
        &sent,
    )?;
    let second = pane_readback(window, cx)?.unwrap_or_default();
    click(window, cx, center(&second, "close")?).await?;
    let listed = wait_for(
        window,
        cx,
        "the second draft",
        |w, cx| {
            let open = pane_readback(w, cx)?;
            let drafts = drafts_readback(w, cx)?;
            Ok(open.is_none().then_some(drafts))
        },
        |r| r["drafts"].as_array().is_some_and(|d| d.len() == 1),
    )
    .await?;
    click(window, cx, center(&listed, "discard-0")?).await?;
    let discarded = wait_for(
        window,
        cx,
        "the draft to be discarded",
        |w, cx| drafts_readback(w, cx).map(Some),
        |r| r["drafts"].as_array().is_some_and(Vec::is_empty),
    )
    .await?;
    capture(window, cx, output.join("06-discarded.png"))?;
    evidence.step(
        "Discard from the drafts list removes the draft",
        json!({"drafts": []}),
        json!({"drafts": discarded["drafts"]}),
        &discarded,
    )?;

    // A third discussion discarded straight from the pane.
    click(window, cx, center(&discarded, "discuss")?).await?;
    let third = wait_for(window, cx, "a third discussion", pane_readback, |r| {
        r["status"] == "Ready" && r["sessionId"].is_string()
    })
    .await?;
    click(window, cx, center(&third, "discard")?).await?;
    let gone = wait_for(
        window,
        cx,
        "the pane discard",
        |w, cx| {
            let open = pane_readback(w, cx)?;
            let drafts = drafts_readback(w, cx)?;
            Ok(open.is_none().then_some(drafts))
        },
        |r| r["drafts"].as_array().is_some_and(Vec::is_empty),
    )
    .await?;
    evidence.step(
        "Discard from the pane closes it and lists nothing",
        json!({"drafts": []}),
        json!({"drafts": gone["drafts"]}),
        &gone,
    )?;
    Ok(json!({ "steps": evidence.steps }))
}

fn account_config(root: &Path) -> anyhow::Result<AgentAccountsConfig> {
    let config = AgentAccountsConfig {
        data_dir: root.join("providers/state"),
        claude_config_dir: root.join("providers/claude"),
        claude_config_file: root.join("providers/claude.json"),
        codex_home: root.join("providers/codex"),
    };
    config.disable_claude_keychain_fallback()?;
    std::fs::create_dir_all(&config.claude_config_dir)?;
    std::fs::write(
        &config.claude_config_file,
        json!({"oauthAccount": {"accountUuid": "claude-fixture", "emailAddress": "claude@example.test"}}).to_string(),
    )?;
    std::fs::write(
        config.claude_config_dir.join(".credentials.json"),
        json!({"claudeAiOauth": {"accessToken": "synthetic"}}).to_string(),
    )?;
    Ok(config)
}

fn peer() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-claude-board-authoring.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn main() -> anyhow::Result<()> {
    // SAFETY: set first thing in `main`, before the Tokio runtime, the GPUI
    // platform or any other thread exists, and never changed afterwards.
    unsafe {
        std::env::set_var("LUNA_OPEN_ROUTE", "board");
    }
    let output = match std::env::args().nth(1) {
        Some(dir) => PathBuf::from(dir),
        None => {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/board/authoring/fixture")
        }
    };
    std::fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    for stale in [
        "readback.json",
        "manifest.json",
        "board-authoring.pass",
        "failure.txt",
        "failure.png",
        "failure-readback.json",
    ] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let root = tempfile::tempdir()?;
    let base = root.path().canonicalize()?;
    let ui_data = base.join("ui");
    let engine_data = base.join("engine");
    let project = base.join("project");
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(project.join("src"))?;
    std::fs::write(
        project.join("src/board.rs"),
        "pub fn drafts() -> Vec<Draft> {\n    // The page never lists open discussions.\n    Vec::new()\n}\n",
    )?;
    let brief = "The Board page does not list open discussion drafts.\n\n## Acceptance criteria\n- Closed unfinished discussions are listed as drafts.\n- Resume reopens the same discussion.";
    let implementation = json!({"harness": "claude-code", "permissionMode": "full"});
    std::fs::write(
        project.join(".board-authoring-plan.json"),
        serde_json::to_vec(&json!({"turns": [
            [
                {"read": "src/board.rs"},
                {"call": "board_draft_checkpoint", "arguments": {"proposal": {"title": TITLE, "description": brief, "unresolved": ["implementation model", "access", "review"]}}},
                {"say": "src/board.rs returns no drafts. Which implementation model, reasoning, access and review should it use? Project defaults are fine."}
            ],
            [
                {"call": "board_draft_checkpoint", "arguments": {"proposal": {"title": TITLE, "description": brief, "implementation": implementation}}},
                {"say": "Updated: Claude Code with Full access for the implementation; your discussion stays as it is."}
            ],
            [
                {"call": "board_create", "arguments": {"requestId": "fixture-ticket-1", "title": TITLE, "description": brief, "implementation": implementation}},
                {"say": "Saved to Backlog."}
            ],
            [
                {"say": "Noted."}
            ]
        ]}))?,
    )?;

    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(ClaudeHarness::new().with_executable(peer())));
    let accounts = account_config(&base)?;
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            EngineProfile::development(&engine_data, "authoring-fixture", "authoring-fixture"),
            Arc::new(registry),
            HarnessId::ClaudeCode,
            None,
            accounts,
        )
    })?;
    let mut auth = AuthConfig::new("", base.join("auth"));
    auth.dev_user_id = "authoring-fixture".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let device = core.device_id.clone();
    core.workspace.create_space(
        PROJECT,
        &device,
        project.to_str().unwrap(),
        Some("Fixture".into()),
        false,
    )?;
    let spaces = core.workspace.read_spaces()?;
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 0,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::ClaudeCode,
    };
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
    let output_for_ui = output.clone();
    let project_for_ui = project.clone();
    gpui_platform::headless().with_assets(icons::Assets).run(move |cx| {
        gpui_tokio::init(cx);
        gpui_base::init(cx);
        let mut settings = settings::UiSettings::default();
        settings.space_filter = Some(PROJECT.into());
        settings::init(settings.clone(), ui_data.clone(), cx);
        let fonts = typography::register_fonts(cx);
        typography::init(
            settings.ui_font_family.clone(),
            settings.ui_font_size,
            settings.terminal_font_family.clone(),
            settings.terminal_font_size,
            settings.code_font_family.clone(),
            settings.code_font_size,
            fonts,
            cx,
        );
        theme_library::init(ui_data.clone(), cx);
        appearance::init(
            appearance::AppearanceMode::Dark,
            settings.theme_selection,
            settings.accent,
            settings.surface,
            cx,
        );
        history::init(
            settings.git_history_columns,
            settings.git_history_column_widths,
            settings.git_history_column_order,
            settings.git_history_author_display,
            cx,
        );
        composer::init(cx, settings.composer_send_behavior);
        terminal::panel::init(cx);
        let state = cx.new(|_| {
            let mut state = state::AppState::new();
            state.local_device_id = Some(device.clone());
            state.devices = vec![
                serde_json::from_value(json!({
                    "id": device, "name": "Studio Mac", "platform": "macos", "lastSeenAt": null
                }))
                .unwrap(),
            ];
            state.spaces = spaces;
            state.chats_synced = true;
            state.spaces_synced = true;
            state.auto_selected = true;
            state.connection = state::ConnectionStatus::Ready;
            state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
            state.fixture_attachment_engine(handle);
            state
        });
        let window = cx
            .open_window(
                WindowOptions {
                    window_background: theme::Theme::of(cx).window_background_appearance(),
                    show: false,
                    focus: false,
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(20.0), px(20.0)),
                        size(px(WIDTH), px(HEIGHT)),
                    ))),
                    ..Default::default()
                },
                |window, cx| {
                    window.set_rem_size(px(typography::font_size(cx).pixels()));
                    cx.new(|cx| shell::Shell::new(state.clone(), boot, cx))
                },
            )
            .unwrap();
        let _ = window.update(cx, |_, window, _| window.activate_window());
        let ui_state = state.clone();
        let project_for_ui = project_for_ui.clone();
        cx.spawn(async move |cx| {
            let output = output_for_ui;
            let result = scenario(window, cx, &output, &project_for_ui, ui_state).await;
            match result.and_then(|record| {
                let body = serde_json::to_vec_pretty(&record)?;
                std::fs::write(output.join("readback.json"), &body)?;
                let commit = std::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(env!("CARGO_MANIFEST_DIR"))
                    .output()
                    .ok()
                    .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned());
                let manifest = json!({
                    "suite": "board-authoring-fixture",
                    "contract": "docs/design/project-board/pb0-contract.md",
                    "commit": commit,
                    "recordedAtMs": chrono::Utc::now().timestamp_millis(),
                    "readback": {"file": "readback.json", "sha256": format!("{:x}", Sha256::digest(&body))},
                    "steps": record["steps"].as_array().map_or(0, Vec::len),
                    "passed": true,
                });
                std::fs::write(output.join("manifest.json"), serde_json::to_vec_pretty(&manifest)?)?;
                std::fs::write(
                    output.join("board-authoring.pass"),
                    "PASS: headless production Shell on the Board page with real pointer and keyboard input: Discuss with AI opens a conversation-only pane (one composer, no title or description field) with the discussion assistant's provider/model/reasoning/effective access; a typed message runs the real Claude Code harness peer, which reads a project file and proposes a read-only ticket; Close keeps a Board draft (no chat), Resume reopens the same discussion, a Full implementation request leaves the discussion's access unchanged, Create shows the real key in Backlog with Open ticket, save then close ends it, and Discard removes drafts from the list and the pane\n",
                )?;
                Ok(())
            }) {
                Ok(()) => {}
                Err(error) => {
                    eprintln!("board-authoring fixture failed: {error:#}");
                    *failure_for_ui.lock().unwrap() = Some(format!("{error:#}"));
                    let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                    let _ = capture(window, cx, output.join("failure.png"));
                    let readback = json!({
                        "pane": pane_readback(window, cx).ok().flatten(),
                        "drafts": drafts_readback(window, cx).ok(),
                    });
                    let _ = std::fs::write(
                        output.join("failure-readback.json"),
                        serde_json::to_vec_pretty(&readback).unwrap_or_default(),
                    );
                    // The headless platform's quit can end the process with
                    // status 0 before `main` reads the failure: exit here.
                    std::process::exit(1);
                }
            }
            let _ = gpui::AnyWindowHandle::from(window).update(cx, |_, window, _| window.remove_window());
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    runtime.block_on(core.shutdown());
    if let Some(error) = failure.lock().unwrap().take() {
        anyhow::bail!(error);
    }
    Ok(())
}
