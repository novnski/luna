//! Headless GPUI E2E for the central chat workbench: the inset canvas, tab
//! groups and nested splits that hold the Main chat and its child, side and
//! alongside chats, with Related chats as the reopen library.
//!
//! Drives the production `Shell` in a hidden window with real pointer and
//! keyboard input (GPUI hit testing, drag and drop, divider drags, clicks,
//! Cmd-W / Cmd-Shift-T) against an in-process `EngineCore` with a Mock
//! harness. The layout is the engine's: every gesture goes through
//! `MutateChatWorkbench`, and the reload step reads it back from a new Shell.
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. Main is missing, duplicated, closable or replaced: it has no close
//!    glyph; Cmd-W, Close tab, Close others and Close pane leave it open and
//!    labelled Main in its own pane after every step, save and reload.
//! 2. Opening an already open chat (Related click, double click, Enter, Open
//!    tab, drag) adds a second tab, a second composer or a second watch,
//!    instead of focusing the existing tab. One chat ID appears once.
//! 3. Tab reorder inside a strip lands one slot off; a move between groups or
//!    onto any of the four pane edges and the outer workbench edge loses or
//!    duplicates another tab; a self drop or a cancelled drag (Escape, release
//!    outside) changes the layout or focus.
//! 4. Three nested panes render with stale or duplicate node IDs, a dead
//!    divider, a phantom empty pane after its last tab leaves, or a split
//!    that ignores the minimum pane size.
//! 5. A divider drag writes on every pointer move, does not follow the pointer
//!    locally, loses the final ratio, or double-click does not equalize.
//! 6. Close tab cancels a running send, ends a side chat, archives or stops a
//!    chat, or loses its draft; reopen (Related row, Cmd-Shift-T, palette)
//!    restores another chat, a blank draft or a new process.
//! 7. Two visible composers share a draft, or Send in one pane reaches the
//!    neighbouring or Main chat; moving a pane while a send is in flight
//!    retargets or cancels it.
//! 8. `+` creates more than one child for one gesture, parents it to the
//!    focused child instead of Main, starts a turn, or does not open it as an
//!    empty child tab in the focused group with its composer focused.
//! 9. Related chats drops rows that are open, shows no Open state, or still
//!    renders a conversation inside the right sidebar.
//! 10. A child's breadcrumb, badge or reporting follows its tab position
//!     instead of its record; an alongside chat gains a parent.
//! 11. The layout does not survive a settings flush and a new Shell, switching
//!     main chats loses either layout, or a late response from the previous
//!     main chat or engine mutates the current one.
//! 12. Light, dark and narrow-window captures clip the tab strip, overlap the
//!     composer, or hide the focused pane and drop previews.
//! 13. The real Cmd-W key/menu path (CloseWindow) falls through to the right
//!     pane or the window while Main is focused in the canvas, or does not
//!     close a focused child or an empty secondary group.
//! 14. A mutation response that arrives after a profile switch (or for an
//!     earlier engine) settles, discards or pumps the current workbench; a
//!     profile the engine does not recognise writes a layout; an engine with
//!     no profile writes anything.
//! 16. The surrounding frame shows seams, mismatched bands or misaligned
//!     rows: the canvas is not inset evenly from both sidebars and the
//!     titlebar in every open/collapsed combination, a sidebar resize does
//!     not reflow it, or the Related/Changes/Files top rows are unreadable
//!     in dark or light.
//! 17. A composer-scoped key (model picker Cmd-/, its Cmd-1…9 slots,
//!     Escape stop) reaches Main while a child tab's composer has focus; a
//!     child's open picker lets session jumps or Cmd-W act underneath it; a
//!     slot picks something other than that menu's own entry; Escape stops
//!     a running Main while Child A is focused, or fails to stop Child A.
//! 18. Keyboard focus moved into another group (Tab / Shift-Tab) leaves the
//!     focused group, the frame's checkout/Git row and Cmd-W/New child bound
//!     to the previous one (an alongside chat on another checkout shows
//!     Main's checkout).
//! 27. Balance panes counts tabs instead of groups, changes an axis, the
//!     order, IDs, active chats or focus, leaves a mixed 2-over-1 or six-group
//!     tree with unequal group areas, writes again on a repeat, is lost on a
//!     new Shell, starts a provider, or is offered with a single group.
//! 28. A sidebar opening (or a narrower window) over a fitting split scrolls
//!     the canvas to the focused right group instead of staying anchored at
//!     the left (the rightmost groups are the ones covered); or an explicit
//!     focus no longer reveals its group.
//! 26. A split layout in a canvas too small for it (window or sidebars
//!     shrunk) squeezes groups below 320x240 (words wrap to letters, the
//!     composer's controls overlap); or the fix rewrites the saved layout
//!     (axis, ratio, groups, revision), swaps axes or hides groups; or the
//!     overflow is not reachable by scrolling, the focused/opened/reopened
//!     group is not scrolled into view, manual scrolling snaps back on the
//!     next frame, the footer or sidebars scroll with the canvas, drops and
//!     previews land on panes scrolled out of view (or over a sidebar), or
//!     dividers and tab drags misread scrolled coordinates; or widening
//!     does not restore the exact saved presentation.
//! 25. With both sidebars open, a long focused title in the frame row
//!     pushes Review changes / Commit & push past the row's right edge, under
//!     the right sidebar; or a real click on Review changes does not open
//!     Changes bound to the focused chat's checkout.
//! 24. With both sidebars open after a wider split, an active tab is wider
//!     than its strip, so its close control sits under the trailing
//!     controls; or after End (or archive) removes the focused composer,
//!     Cmd-/ (without a click in between) opens Main's model picker.
//! 23. A one-move drag (a fast flick or a native synthetic drag) only
//!     selects a tab, or leaves a divider or Related row unchanged, because
//!     no drag move reached the workbench; or its release fallback commits
//!     after Escape, on a sub-threshold press, a self drop, or a divider
//!     click / double-click.
//! 22. The mid-reflow capture shows a finished (or not started) transition
//!     instead of one instant in it, its canvas and readback come from
//!     different moments, or reduced motion still animates.
//! 21. In a crowded narrow strip, a newly opened, keyboard-selected (Home /
//!     End) or reopened (Cmd-Shift-T) active tab stays scrolled out of view,
//!     or a tab covers the group's trailing controls.
//! 20. An empty group cannot be focused again (pointer or Tab) after
//!     another group was, so Cmd-W closes, and New child opens in, the
//!     group focused before it.
//! 19. Two ended Side tabs from different parents, open side by side, share
//!     one panel (one shows a placeholder), lose their exact source parent,
//!     or start a provider process again; a tab without a rendered composer
//!     (ended Side, archived) still opens a hidden picker or falls back to
//!     Main's.
//! 15. A side chat opens as a Side tab without its exact source parent, End
//!     is confused with Close, or a closed Side tab cannot be reopened from
//!     Main's Related library (including ended, read-only history).
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example chat-workbench-fixture
//! Run:   target/debug/examples/chat-workbench-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes w01..w20 PNG captures, readback.json and chat-workbench.pass; a
//! failure writes failure.txt/failure.png and exits nonzero.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, Keystroke, MouseButton, PlatformInput, WindowBounds,
    WindowHandle, WindowOptions, point, px, size,
};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::mock::MockHarness;
use luna_proto::{
    AgentEvent, ChatConfig, ChildThread, DoneStatus, HarnessId, PermissionMode, SandboxLevel,
};
use luna_ui::*;
use serde_json::Value;

const PROJECT: &str = "workbench-project";
const MAIN: &str = "e0e0e0e0-0000-4000-8000-000000000001";
const MAIN2: &str = "e0e0e0e0-0000-4000-8000-000000000002";
const CHILD_A: &str = "e1e1e1e1-0000-4000-8000-000000000001";
const CHILD_B: &str = "e1e1e1e1-0000-4000-8000-000000000002";
/// An ordinary chat on a second checkout, opened alongside Main.
const ALONG: &str = "e2e2e2e2-0000-4000-8000-000000000001";
const ALONG_CHECKOUT: &str = "checkout-b";
const MAIN_TITLE: &str =
    "Release plan for the v0.6 cut: cache migration, settings rename and the UI flag rollout";
const MOCK_ANSWER: &str = "Checked; the notes match the changelog.";
const WIDTH: f32 = 1480.0;
const HEIGHT: f32 = 920.0;
const PANE_WIDTH: f32 = 420.0;

struct Spec {
    id: &'static str,
    title: &'static str,
    minutes_ago: i64,
    parent: Option<&'static str>,
    /// A checkout directory other than the project root (a worktree).
    checkout: Option<&'static str>,
}

const SPECS: [Spec; 5] = [
    Spec {
        id: MAIN,
        title: MAIN_TITLE,
        minutes_ago: 1,
        parent: None,
        checkout: None,
    },
    Spec {
        id: MAIN2,
        title: "Docs cleanup",
        minutes_ago: 6,
        parent: None,
        checkout: None,
    },
    Spec {
        id: CHILD_A,
        title: "Benchmark the cache",
        minutes_ago: 2,
        parent: Some(MAIN),
        checkout: None,
    },
    Spec {
        id: CHILD_B,
        title: "Draft release notes",
        minutes_ago: 3,
        parent: Some(MAIN),
        checkout: None,
    },
    Spec {
        id: ALONG,
        // Long on purpose: the frame row must truncate it before its actions.
        title: "Benchmark on the second checkout: cold and warm cache timings for every fixture project before the v0.6 cut",
        minutes_ago: 30,
        parent: None,
        checkout: Some(ALONG_CHECKOUT),
    },
];

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some("mock-fixture".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::ReadOnly,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn text_entry(
    id: &str,
    role: MessageRole,
    text: &str,
    device: &str,
    at: i64,
) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role,
        parts: vec![MessagePart::Text {
            id: format!("{id}-text"),
            text: text.into(),
        }],
        created_at: at,
        device_id: device.into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    }
}

fn child_thread(spec: &Spec, at_ms: i64) -> Option<ChildThread> {
    Some(ChildThread {
        parent_task_id: spec.parent?.into(),
        creation_command_id: format!("fixture-create-{}", spec.id),
        permission_ceiling: Default::default(),
        sandbox_ceiling: None,
        initial_message_id: format!("{}-prompt", spec.id),
        initial_prompt: format!("{} (fixture)", spec.title),
        report_results: false,
        continue_parent: false,
        outcomes: Vec::new(),
        outcome_floor_ms: Some(at_ms),
        provisioning: None,
    })
}

type Window = WindowHandle<shell::Shell>;

/// A settled screenshot: the pointer rests on empty sidebar space first, so
/// no hover tooltip or hover wash covers tab or body text.
async fn shot(window: Window, cx: &mut AsyncApp, path: PathBuf) -> anyhow::Result<()> {
    hover_at(window, cx, (120.0, 560.0), None)?;
    draw(window, cx)?;
    settle(cx, 350).await;
    capture(window, cx, path)
}

/// A screenshot exactly as things are (a drag held mid-gesture).
fn capture(window: Window, cx: &mut AsyncApp, path: PathBuf) -> anyhow::Result<()> {
    let window = gpui::AnyWindowHandle::from(window);
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })??;
    Ok(())
}

fn draw(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
    })?;
    Ok(())
}

fn input(window: Window, cx: &mut AsyncApp, event: PlatformInput) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(event, cx);
    })?;
    Ok(())
}

fn hover_at(
    window: Window,
    cx: &mut AsyncApp,
    at: (f32, f32),
    pressed: Option<MouseButton>,
) -> anyhow::Result<()> {
    input(
        window,
        cx,
        PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position: point(px(at.0), px(at.1)),
            pressed_button: pressed,
            modifiers: Default::default(),
        }),
    )
}

fn press(
    window: Window,
    cx: &mut AsyncApp,
    at: (f32, f32),
    button: MouseButton,
    down: bool,
    clicks: usize,
) -> anyhow::Result<()> {
    let position = point(px(at.0), px(at.1));
    input(
        window,
        cx,
        if down {
            PlatformInput::MouseDown(gpui::MouseDownEvent {
                button,
                position,
                modifiers: Default::default(),
                click_count: clicks,
                first_mouse: false,
            })
        } else {
            PlatformInput::MouseUp(gpui::MouseUpEvent {
                button,
                position,
                modifiers: Default::default(),
                click_count: clicks,
            })
        },
    )
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

async fn click(
    window: Window,
    cx: &mut AsyncApp,
    at: (f32, f32),
    button: MouseButton,
) -> anyhow::Result<()> {
    hover_at(window, cx, at, None)?;
    draw(window, cx)?;
    settle(cx, 60).await;
    press(window, cx, at, button, true, 1)?;
    press(window, cx, at, button, false, 1)?;
    settle(cx, 250).await;
    draw(window, cx)?;
    Ok(())
}

async fn double_click(window: Window, cx: &mut AsyncApp, at: (f32, f32)) -> anyhow::Result<()> {
    hover_at(window, cx, at, None)?;
    draw(window, cx)?;
    press(window, cx, at, MouseButton::Left, true, 1)?;
    press(window, cx, at, MouseButton::Left, false, 1)?;
    press(window, cx, at, MouseButton::Left, true, 2)?;
    press(window, cx, at, MouseButton::Left, false, 2)?;
    settle(cx, 300).await;
    draw(window, cx)?;
    Ok(())
}

fn key(window: Window, cx: &mut AsyncApp, combo: &str) -> anyhow::Result<()> {
    input(
        window,
        cx,
        PlatformInput::KeyDown(gpui::KeyDownEvent {
            keystroke: Keystroke::parse(combo)?,
            is_held: false,
            prefer_character_input: false,
        }),
    )
}

/// A one-move drag, as a fast flick or a synthetic native drag (CUA's
/// `drag(from, to)`) delivers it: press, ONE pointer move with the button
/// held, release, with no frame drawn in between. GPUI starts the drag on
/// that move after the canvas's drag-move listeners already ran, so no drag
/// move reaches the workbench; only the release can resolve it.
async fn flick(
    window: Window,
    cx: &mut AsyncApp,
    from: (f32, f32),
    to: (f32, f32),
) -> anyhow::Result<()> {
    hover_at(window, cx, from, None)?;
    draw(window, cx)?;
    settle(cx, 60).await;
    press(window, cx, from, MouseButton::Left, true, 1)?;
    hover_at(window, cx, to, Some(MouseButton::Left))?;
    press(window, cx, to, MouseButton::Left, false, 1)?;
    settle(cx, 250).await;
    draw(window, cx)?;
    Ok(())
}

/// Press at `from`, travel in steps to `to` with the button held; no release.
async fn drag_hold(
    window: Window,
    cx: &mut AsyncApp,
    from: (f32, f32),
    to: (f32, f32),
) -> anyhow::Result<()> {
    hover_at(window, cx, from, None)?;
    draw(window, cx)?;
    press(window, cx, from, MouseButton::Left, true, 1)?;
    for step in 1..=16 {
        let t = step as f32 / 16.0;
        hover_at(
            window,
            cx,
            (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t),
            Some(MouseButton::Left),
        )?;
        draw(window, cx)?;
    }
    settle(cx, 80).await;
    draw(window, cx)?;
    Ok(())
}

async fn release(window: Window, cx: &mut AsyncApp, at: (f32, f32)) -> anyhow::Result<()> {
    press(window, cx, at, MouseButton::Left, false, 1)?;
    settle(cx, 500).await;
    draw(window, cx)?;
    Ok(())
}

async fn drag(
    window: Window,
    cx: &mut AsyncApp,
    from: (f32, f32),
    to: (f32, f32),
) -> anyhow::Result<()> {
    drag_hold(window, cx, from, to).await?;
    release(window, cx, to).await
}

fn wb(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    draw(window, cx)?;
    window.update(cx, |shell, _, cx| shell.fixture_workbench_readback(cx))
}

fn related(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    window.update(cx, |shell, _, cx| shell.fixture_related_readback(cx))
}

/// Poll until `done(readback)` or fail with the last readback.
async fn wait_for(
    window: Window,
    cx: &mut AsyncApp,
    what: &str,
    done: impl Fn(&Value) -> bool,
) -> anyhow::Result<Value> {
    let mut last = Value::Null;
    for _ in 0..60 {
        last = wb(window, cx)?;
        if done(&last) && last["pendingMutations"] == 0 {
            return Ok(last);
        }
        settle(cx, 100).await;
    }
    anyhow::bail!("timed out waiting for {what}: {last}")
}

fn panes(node: &Value, out: &mut Vec<Value>) {
    match node["kind"].as_str() {
        Some("pane") => out.push(node.clone()),
        Some("split") => {
            panes(&node["first"], out);
            panes(&node["second"], out);
        }
        _ => {}
    }
}

fn all_panes(rb: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    panes(&rb["root"], &mut out);
    out
}

fn tabs_of(pane: &Value) -> Vec<String> {
    pane["tabs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tab| tab["chatId"].as_str().map(str::to_string))
        .collect()
}

fn pane_of(rb: &Value, chat: &str) -> Option<Value> {
    all_panes(rb)
        .into_iter()
        .find(|pane| tabs_of(pane).iter().any(|id| id == chat))
}

fn occurrences(rb: &Value, chat: &str) -> usize {
    all_panes(rb)
        .iter()
        .map(|pane| tabs_of(pane).iter().filter(|id| *id == chat).count())
        .sum()
}

fn role_of(rb: &Value, chat: &str) -> Option<String> {
    all_panes(rb).iter().find_map(|pane| {
        pane["tabs"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|tab| {
                (tab["chatId"] == chat)
                    .then(|| tab["role"].as_str().unwrap_or_default().to_string())
            })
    })
}

fn rect(bounds: &Value) -> (f32, f32, f32, f32) {
    let get = |key: &str| bounds[key].as_f64().unwrap_or(0.0) as f32;
    (get("x"), get("y"), get("w"), get("h"))
}

/// A tab's visible (strip-clipped) rect: what can be seen and hit.
fn tab_rect(rb: &Value, chat: &str) -> anyhow::Result<(f32, f32, f32, f32)> {
    let tab = rb["tabBounds"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default())
        .find(|tab| tab["chatId"] == chat)
        .ok_or_else(|| anyhow::anyhow!("tab {chat} not painted: {rb}"))?;
    let visible = rect(if tab["visible"].is_object() {
        &tab["visible"]
    } else {
        &tab["bounds"]
    });
    anyhow::ensure!(
        visible.2 >= 12.0,
        "tab {chat} is scrolled out of its strip: {tab}"
    );
    Ok(visible)
}

/// `chat`'s tab is wholly inside its strip's viewport (title, status and
/// close control all on screen and hit-testable).
fn fully_revealed(rb: &Value, chat: &str) -> bool {
    rb["tabBounds"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|pane| {
            let view = rect(&pane["view"]);
            pane["tabs"].as_array().into_iter().flatten().any(|tab| {
                let (bounds, shown) = (rect(&tab["bounds"]), rect(&tab["visible"]));
                tab["chatId"] == chat
                    && bounds.2 > 12.0
                    && (shown.2 - bounds.2).abs() <= 0.5
                    && shown.0 >= view.0 - 0.5
                    && shown.0 + shown.2 <= view.0 + view.2 + 0.5
            })
        })
}

/// The group's trailing controls (New child, Split) sit outside the tab
/// viewport, never under a tab.
fn controls_clear(rb: &Value, pane_id: &str) -> bool {
    let view = rb["tabBounds"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|pane| pane["pane"] == pane_id)
        .map(|pane| rect(&pane["view"]));
    let plus = rb["plusBounds"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|plus| plus["pane"] == pane_id)
        .map(|plus| rect(&plus["bounds"]));
    matches!((view, plus), (Some(view), Some(plus)) if plus.0 >= view.0 + view.2 - 0.5 && plus.2 > 0.0)
}

/// Every group's active tab fits wholly inside its strip viewport (label and
/// close control visible) and no tab is wider than that viewport; the
/// trailing controls stay clear.
fn active_tabs_fit(rb: &Value) -> bool {
    all_panes(rb).iter().all(|pane| {
        let id = pane["id"].as_str().unwrap_or_default();
        let Some(active) = pane["active"].as_str() else {
            return true;
        };
        let narrow_enough = rb["tabBounds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|strip| strip["pane"] == id)
            .all(|strip| {
                let view = rect(&strip["view"]);
                strip["tabs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .all(|tab| rect(&tab["bounds"]).2 <= view.2 + 0.5)
            });
        // Within its own strip (the canvas may be scrolled so the group
        // itself is out of view: that is the canvas's business, not the
        // strip's).
        let active_inside = rb["tabBounds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|strip| strip["pane"] == id)
            .any(|strip| {
                let view = rect(&strip["view"]);
                strip["tabs"].as_array().into_iter().flatten().any(|tab| {
                    let bounds = rect(&tab["bounds"]);
                    tab["chatId"] == active
                        && bounds.2 > 12.0
                        && bounds.0 >= view.0 - 0.5
                        && bounds.0 + bounds.2 <= view.0 + view.2 + 0.5
                })
            });
        narrow_enough && active_inside && controls_clear(rb, id)
    })
}

/// Some tab of the group is not wholly visible: the strip overflows.
fn strip_overflows(rb: &Value, pane_id: &str) -> bool {
    rb["tabBounds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|pane| pane["pane"] == pane_id)
        .flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default())
        .any(|tab| (rect(&tab["visible"]).2 - rect(&tab["bounds"]).2).abs() > 0.5)
}

/// Every group's painted area (w x h), from the readback.
fn pane_areas(rb: &Value) -> Vec<f32> {
    rb["paneBounds"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| {
            let (_, _, w, h) = rect(&entry["bounds"]);
            w * h
        })
        .collect()
}

/// A real click on the frame row's Balance panes button.
async fn click_balance(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    draw(window, cx)?;
    let state = frame(window, cx)?;
    let button = rect(&state["balanceButton"]);
    anyhow::ensure!(button.2 > 0.0, "Balance panes is offered: {state}");
    click(
        window,
        cx,
        (button.0 + button.2 / 2.0, button.1 + button.3 / 2.0),
        MouseButton::Left,
    )
    .await
}

/// Open or close the right sidebar (through its own toggle) and let it settle.
async fn set_right_sidebar(window: Window, cx: &mut AsyncApp, open: bool) -> anyhow::Result<()> {
    if frame(window, cx)?["rightOpen"].as_bool() != Some(open) {
        window.update(cx, |shell, _, cx| shell.fixture_toggle_sidebar(true, cx))?;
        settle_frames(window, cx).await?;
    }
    anyhow::ensure!(
        frame(window, cx)?["rightOpen"].as_bool() == Some(open),
        "the right sidebar is {}",
        if open { "open" } else { "closed" }
    );
    Ok(())
}

/// Let a sidebar tween settle.
async fn settle_frames(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    for _ in 0..8 {
        draw(window, cx)?;
        settle(cx, 60).await;
    }
    Ok(())
}

/// Bring `chat`'s tab where a real pointer can click it, with real wheel
/// events, and return the fresh readback. Two scroll layers, checked in
/// order, each only when it is the one hiding the tab:
/// 1. The canvas (it scrolls only when the saved layout is larger than the
///    viewport): if the tab's strip is not in the canvas viewport, wheel the
///    canvas (on a context row, which does not scroll itself) toward it.
/// 2. The tab strip (it scrolls when its tabs are wider than it): if the
///    strip is in view but the tab is wholly scrolled out of it, wheel the
///    strip itself toward the tab.
///
/// A tab that is partly visible (at least 12 px) is already clickable, as a
/// person would click it; the product's own focus and active-tab reveal show
/// the rest after the click.
async fn reveal_tab(window: Window, cx: &mut AsyncApp, chat: &str) -> anyhow::Result<Value> {
    let wheel = |window: Window, cx: &mut AsyncApp, at: (f32, f32), dx: f32, dy: f32| {
        input(
            window,
            cx,
            PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                position: point(px(at.0), px(at.1)),
                delta: gpui::ScrollDelta::Pixels(point(px(dx), px(dy))),
                modifiers: Default::default(),
                touch_phase: gpui::TouchPhase::Moved,
            }),
        )
    };
    for _ in 0..12 {
        let rb = wb(window, cx)?;
        let Some((strip, tab)) =
            rb["tabBounds"]
                .as_array()
                .into_iter()
                .flatten()
                .find_map(|strip| {
                    strip["tabs"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|tab| tab["chatId"] == chat)
                        .map(|tab| (rect(&strip["view"]), tab.clone()))
                })
        else {
            anyhow::bail!("tab {chat} not painted: {rb}");
        };
        let (bounds, shown) = (rect(&tab["bounds"]), rect(&tab["visible"]));
        if shown.2 >= 12.0 {
            return Ok(rb);
        }
        let view = rect(&rb["rootBounds"]);
        let in_canvas_width = (strip.0 + strip.2).min(view.0 + view.2) - strip.0.max(view.0);
        let strip_middle = strip.1 + strip.3 / 2.0;
        let strip_in_canvas =
            in_canvas_width >= 40.0 && strip_middle >= view.1 && strip_middle <= view.1 + view.3;
        if !strip_in_canvas {
            // Layer 1: the canvas hides the whole strip.
            let scrolls = rb["canvasScroll"]["maxX"].as_f64().unwrap_or(0.0) > 0.5
                || rb["canvasScroll"]["maxY"].as_f64().unwrap_or(0.0) > 0.5;
            anyhow::ensure!(
                scrolls,
                "tab {chat}'s strip is outside a canvas that does not scroll: {rb}"
            );
            let dx = if strip.0 < view.0 {
                view.0 + 8.0 - strip.0
            } else if strip.0 + strip.2 > view.0 + view.2 {
                ((view.0 + view.2 - 8.0) - (strip.0 + strip.2)).max(view.0 + 8.0 - strip.0)
            } else {
                0.0
            };
            let dy = if strip.1 < view.1 {
                view.1 + 8.0 - strip.1
            } else if strip.1 + strip.3 > view.1 + view.3 {
                (view.1 + view.3 - 8.0) - (strip.1 + strip.3)
            } else {
                0.0
            };
            wheel(window, cx, (view.0 + view.2 / 2.0, view.1 + 50.0), dx, dy)?;
        } else {
            // Layer 2: the strip itself has the tab scrolled out of it.
            let left = strip.0.max(view.0);
            let right = (strip.0 + strip.2).min(view.0 + view.2);
            let dx = if bounds.0 < left {
                left + 8.0 - bounds.0
            } else {
                (right - 8.0) - (bounds.0 + bounds.2)
            };
            wheel(window, cx, ((left + right) / 2.0, strip_middle), dx, 0.0)?;
        }
        draw(window, cx)?;
        settle(cx, 40).await;
    }
    let rb = wb(window, cx)?;
    anyhow::bail!("tab {chat} could not be brought into view: {rb}")
}

/// A real click on `chat`'s tab, scrolled into view first if the canvas
/// had it out of view.
async fn click_tab(window: Window, cx: &mut AsyncApp, chat: &str) -> anyhow::Result<()> {
    let rb = reveal_tab(window, cx, chat).await?;
    click(window, cx, tab_center(&rb, chat)?, MouseButton::Left).await
}

fn tab_center(rb: &Value, chat: &str) -> anyhow::Result<(f32, f32)> {
    let (x, y, w, h) = tab_rect(rb, chat)?;
    Ok((x + w / 2.0, y + h / 2.0))
}

fn tab_left(rb: &Value, chat: &str) -> anyhow::Result<(f32, f32)> {
    let (x, y, _, h) = tab_rect(rb, chat)?;
    Ok((x + 4.0, y + h / 2.0))
}

fn pane_rect(rb: &Value, pane: &str) -> anyhow::Result<(f32, f32, f32, f32)> {
    rb["paneBounds"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["id"] == pane)
        .map(|entry| rect(&entry["bounds"]))
        .ok_or_else(|| anyhow::anyhow!("pane {pane} not painted: {rb}"))
}

fn plus_center(rb: &Value, pane: &str) -> anyhow::Result<(f32, f32)> {
    rb["plusBounds"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["pane"] == pane)
        .map(|entry| {
            let (x, y, w, h) = rect(&entry["bounds"]);
            (x + w / 2.0, y + h / 2.0)
        })
        .ok_or_else(|| anyhow::anyhow!("New child chat button of {pane} not painted: {rb}"))
}

fn split_rect(rb: &Value) -> anyhow::Result<(f32, f32, f32, f32)> {
    let id = rb["root"]["id"].as_str().unwrap_or_default();
    rb["splitBounds"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["id"] == id)
        .map(|entry| rect(&entry["bounds"]))
        .ok_or_else(|| anyhow::anyhow!("root split not painted: {rb}"))
}

/// Black-box Related chats rows: sweep the pane column and record hover.
fn related_rows(window: Window, cx: &mut AsyncApp, x: f32) -> anyhow::Result<Vec<(String, f32)>> {
    draw(window, cx)?;
    let mut spans: Vec<(String, f32, f32)> = Vec::new();
    let mut y = 60.0;
    while y < HEIGHT - 40.0 {
        hover_at(window, cx, (x, y), None)?;
        let hovered = related(window, cx)?["hover"].as_str().map(str::to_string);
        if let Some(id) = hovered.and_then(|hover| {
            hover
                .strip_prefix("child-pane-chat-")
                .or_else(|| hover.strip_prefix("side-pane-chat-"))
                .map(str::to_string)
        }) {
            match spans.last_mut() {
                Some((last, _, bottom)) if *last == id => *bottom = y,
                _ => spans.push((id, y, y)),
            }
        }
        y += 2.0;
    }
    hover_at(window, cx, (WIDTH / 2.0, HEIGHT / 2.0), None)?;
    Ok(spans
        .into_iter()
        .map(|(id, top, bottom)| (id, (top + bottom) / 2.0))
        .collect())
}

fn row_y(rows: &[(String, f32)], id: &str) -> anyhow::Result<f32> {
    rows.iter()
        .find(|(row, _)| row == id)
        .map(|(_, y)| *y)
        .ok_or_else(|| anyhow::anyhow!("Related row {id} not found in {rows:?}"))
}

fn related_ids(rb: &Value) -> Vec<String> {
    rb["related"]["related"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pair| pair[1].as_str().map(str::to_string))
        .collect()
}

/// The engine's chat rows (what WatchChats would deliver), merged into the
/// UI copy for chats it does not have yet (a child the `+` created).
async fn merge_engine_chats(
    reader: &luna_rpc::RpcClient,
    state: &gpui::Entity<state::AppState>,
    cx: &mut AsyncApp,
) -> anyhow::Result<Vec<luna_proto::Chat>> {
    let mut rx = reader
        .subscribe(luna_rpc::methods::WATCH_CHATS, serde_json::json!({}))
        .await?;
    let frame = rx
        .recv()
        .await
        .ok_or_else(|| anyhow::anyhow!("no chats frame"))?;
    let side_rows: Vec<(String, state::SideChatRow)> = frame
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            Some((
                row.get("id")?.as_str()?.to_string(),
                state::SideChatRow {
                    parent_id: row.get("sideChatParentId")?.as_str()?.to_string(),
                    state: row
                        .get("sideChatState")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                },
            ))
        })
        .collect();
    let rows: Vec<luna_proto::Chat> = serde_json::from_value(frame)?;
    let fresh = rows.clone();
    cx.update(|cx| {
        state.update(cx, |state, cx| {
            for row in fresh {
                match state.chats.iter_mut().find(|chat| chat.id == row.id) {
                    // Seeded rows keep their UI-only lineage.
                    Some(existing) if existing.child_thread.is_some() => {}
                    Some(existing) => *existing = row,
                    None => state.chats.push(row),
                }
            }
            for (id, row) in side_rows {
                state.side_chat_rows.insert(id, row);
            }
            cx.notify();
        })
    });
    Ok(rows)
}

/// Record a milestone and save the readback so far to
/// `readback-progress.json`, so a later failure keeps every earlier
/// milestone's evidence (the final `readback.json` is written at the end).
fn progress(
    output: &std::path::Path,
    record: &mut serde_json::Map<String, Value>,
    key: String,
    value: Value,
) {
    record.insert(key, value);
    let saved = serde_json::to_vec_pretty(record)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| {
            Ok(std::fs::write(
                output.join("readback-progress.json"),
                bytes,
            )?)
        });
    if let Err(error) = saved {
        eprintln!("could not save readback-progress.json: {error:#}");
    }
}

/// Make the window the app's active one, so `CloseWindow` reaches it.
fn activate(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    window.update(cx, |_, window, _| window.activate_window())?;
    Ok(())
}

/// Dispatch Cmd-W through the real keymap → `CloseWindow` → menu handler.
async fn cmd_w(window: Window, cx: &mut AsyncApp) -> anyhow::Result<bool> {
    activate(window, cx)?;
    key(window, cx, "cmd-w")?;
    settle(cx, 400).await;
    // The window survives (an `update` on a closed window fails).
    Ok(window.update(cx, |_, _, _| ()).is_ok())
}

/// Rendered pixels at `points` (RGBA), from the same frame a screenshot saves.
fn sample(
    window: Window,
    cx: &mut AsyncApp,
    points: &[(f32, f32)],
) -> anyhow::Result<Vec<[u8; 4]>> {
    let window = gpui::AnyWindowHandle::from(window);
    window.update(cx, |_, window, cx| -> anyhow::Result<Vec<[u8; 4]>> {
        window.draw(cx).clear();
        let image = window.render_to_image()?;
        let scale = image.width() as f32 / f32::from(window.viewport_size().width);
        Ok(points
            .iter()
            .map(|(x, y)| {
                let px = ((x * scale) as u32).min(image.width() - 1);
                let py = ((y * scale) as u32).min(image.height() - 1);
                image.get_pixel(px, py).0
            })
            .collect())
    })?
}

/// A sampled pixel as an opaque color over `backdrop` (headless glass keeps
/// its alpha; the real desktop is unknown, so both extremes are measured).
fn over(pixel: [u8; 4], backdrop: f32) -> gpui::Hsla {
    let a = pixel[3] as f32 / 255.0;
    let channel = |c: u8| c as f32 / 255.0 * a + backdrop * (1.0 - a);
    gpui::Rgba {
        r: channel(pixel[0]),
        g: channel(pixel[1]),
        b: channel(pixel[2]),
        a: 1.0,
    }
    .into()
}

fn frame(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    draw(window, cx)?;
    window.update(cx, |shell, _, cx| shell.fixture_frame_readback(cx))
}

/// The canvas sits below the titlebar and 8 px inside whatever is open.
fn assert_frame(state: &Value, what: &str) -> anyhow::Result<()> {
    assert_frame_within(state, what, 2.0)
}

/// `tolerance` covers a tween advancing between paint and readback.
fn assert_frame_within(state: &Value, what: &str, tolerance: f32) -> anyhow::Result<()> {
    let canvas = &state["canvas"];
    let get = |value: &Value| value.as_f64().unwrap_or(f64::NAN) as f32;
    let (x, y, w) = (get(&canvas["x"]), get(&canvas["y"]), get(&canvas["w"]));
    let titlebar = get(&state["titlebarHeight"]);
    anyhow::ensure!(
        (y - titlebar).abs() <= 1.5,
        "{what}: the canvas starts below the titlebar: {state}"
    );
    let left = get(&state["sidebarNow"]);
    anyhow::ensure!(
        (x - (left + 8.0)).abs() <= 2.0,
        "{what}: canvas inset from the left sidebar: {state}"
    );
    let right_edge = get(&state["viewportWidth"]) - get(&state["rightNow"]);
    anyhow::ensure!(
        ((x + w) - (right_edge - 8.0)).abs() <= tolerance,
        "{what}: canvas inset from the right sidebar: {state}"
    );
    Ok(())
}

async fn toggle(window: Window, cx: &mut AsyncApp, right: bool) -> anyhow::Result<()> {
    window.update(cx, |shell, _, cx| shell.fixture_toggle_sidebar(right, cx))?;
    for _ in 0..8 {
        draw(window, cx)?;
        settle(cx, 60).await;
    }
    Ok(())
}

/// Mock pacing for the whole run: each scripted event (an answer, then
/// Done) waits this long, so every Mock turn lasts about six seconds. That
/// keeps Main and Child A observably running while w05b proves Escape stops
/// only the focused one; no step waits for an answer to finish quickly.
const MOCK_DELAY_MS: &str = "3000";

fn main() -> anyhow::Result<()> {
    // SAFETY: set first thing in `main`, before the Tokio runtime, the GPUI
    // platform or any other thread exists, and never changed afterwards; the
    // Mock harness only reads it. Process-global on purpose: the fixture's
    // engine runs in this process.
    unsafe { std::env::set_var("LUNA_MOCK_DELAY_MS", MOCK_DELAY_MS) };
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: chat-workbench-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in [
        "readback.json",
        "readback-progress.json",
        "chat-workbench.pass",
        "failure.txt",
        "failure.png",
    ] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let root = tempfile::tempdir()?;
    let ui_data = root.path().join("ui");
    let engine_data = root.path().join("engine");
    let project = root.path().join("project");
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(&project)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: MOCK_ANSWER.into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            },
        ],
    }));
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "workbench-fixture", "workbench-fixture"),
            Arc::new(registry),
            HarnessId::Mock,
            None,
            AgentAccountsConfig {
                data_dir: engine_data.clone(),
                claude_config_dir: root.path().join("providers/claude"),
                claude_config_file: root.path().join("providers/claude.json"),
                codex_home: root.path().join("providers/codex"),
            },
        )
    })?;
    let mut auth = AuthConfig::new("", root.path().join("auth"));
    auth.dev_user_id = "workbench-fixture".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let device = core.device_id.clone();
    core.workspace.create_space(
        PROJECT,
        &device,
        project.to_str().unwrap(),
        Some("LUNA".into()),
        true,
    )?;
    // Seed through the public RPC (`Mutate createChat`), as the app does,
    // so the engine projects each chat's identity (a `+` child validates its
    // parent against that projection).
    let reader = {
        let _entered = runtime.enter();
        Arc::new(luna_rpc::memory_client(core.rpc_service()))
    };
    let now = Utc::now();
    let ui_chats = runtime.block_on(async {
        let mut ui_chats = Vec::new();
        for spec in &SPECS {
            let cwd = match spec.checkout {
                Some(dir) => {
                    let path = root.path().join(dir);
                    std::fs::create_dir_all(&path)?;
                    path
                }
                None => project.clone(),
            };
            reader
                .call(
                    "Mutate",
                    serde_json::json!({
                        "op": "createChat",
                        "chatId": spec.id,
                        "spaceId": PROJECT,
                        "cwd": cwd.to_string_lossy(),
                        "config": serde_json::to_value(config())?,
                    }),
                )
                .await?;
            core.workspace.rename_chat(spec.id, spec.title)?;
            let at = now - TimeDelta::minutes(spec.minutes_ago);
            let at_ms = at.timestamp_millis();
            let doc = core.doc_host.open(spec.id)?;
            doc.doc().push_message(&text_entry(
                &format!("{}-prompt", spec.id),
                MessageRole::User,
                &format!("{} (fixture)", spec.title),
                &device,
                at_ms - 60_000,
            ))?;
            doc.doc().push_message(&text_entry(
                &format!("{}-answer", spec.id),
                MessageRole::Assistant,
                "Noted.",
                &device,
                at_ms,
            ))?;
            let mut chat = core.workspace.chat(spec.id)?.expect("seeded chat");
            chat.created_at = at - TimeDelta::minutes(2);
            chat.last_message_at = Some(at);
            chat.last_seen_at = Some(at + TimeDelta::seconds(1));
            core.workspace.import_chat_row(&chat)?;
            // Lineage lives on the UI copy only: the engine never starts or
            // sweeps these seeded children.
            chat.child_thread = child_thread(spec, at_ms);
            ui_chats.push(chat);
        }
        anyhow::Ok(ui_chats)
    })?;
    let seeded_ids: Vec<String> = SPECS.iter().map(|spec| spec.id.to_string()).collect();
    let spaces = core.workspace.read_spaces()?;
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let handle_a = handle.clone();
    // The same engine reached under another profile id (accounts under one
    // device root), and an engine that names no profile.
    let profile_handle = |profile: Option<&str>, label: &str| {
        let _entered = runtime.enter();
        state::EngineHandle::from_authenticated_remote(
            luna_rpc::memory_client(core.rpc_service()),
            luna_proto::EngineInfo {
                device_id: core.device_id.clone(),
                workspace_scope: luna_proto::WorkspaceScope::Development,
                capabilities: luna_proto::capabilities::current(),
                profile_id: profile.map(str::to_string),
            },
            format!("memory://workbench-{label}"),
        )
    };
    let handle_b = profile_handle(Some("fixture-profile-b"), "b");
    let handle_none = profile_handle(None, "none");
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59994,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
    gpui_platform::headless().with_assets(icons::Assets).run(move |cx| {
        gpui_tokio::init(cx);
        gpui_base::init(cx);
        let mut settings = settings::UiSettings::default();
        settings.sidebar_auto_settle_after_days = None;
        // w05b proves Escape stops the focused tab's running chat only.
        settings.escape_stops_active_agent = true;
        settings.right_pane_width = PANE_WIDTH;
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
        appearance::init(appearance::AppearanceMode::Dark, settings.theme_selection, settings.accent, settings.surface, cx);
        history::init(
            settings.git_history_columns,
            settings.git_history_column_widths,
            settings.git_history_column_order,
            settings.git_history_author_display,
            cx,
        );
        composer::init(cx, settings.composer_send_behavior);
        // The production Cmd-W handler (without the native quit hooks).
        app_menus::register_close_window(cx);
        terminal::panel::init(cx);
        let state = cx.new(|_| {
            let mut state = state::AppState::new();
            state.local_device_id = Some(device.clone());
            state.devices = vec![
                serde_json::from_value(serde_json::json!({
                    "id": device, "name": "Studio Mac", "platform": "macos", "lastSeenAt": null
                }))
                .unwrap(),
            ];
            state.spaces = spaces;
            state.chats = ui_chats;
            state.chats_synced = true;
            state.spaces_synced = true;
            state.auto_selected = true;
            state.connection = state::ConnectionStatus::Ready;
            state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
            state.fixture_attachment_engine(handle);
            state
        });
        // The authoritative run feed (production `WatchSessions` →
        // `apply_sessions`), which the lightweight attachment does not start:
        // the sidebar/tab indicators and Escape's "is it running" read it.
        // The chats watch stays off so the seeded UI-only lineage holds.
        let sessions_watched = state.update(cx, |state, cx| state.fixture_watch_sessions(cx));
        let open = |cx: &mut gpui::App,
                    state: gpui::Entity<state::AppState>,
                    boot: EngineBootConfig,
                    width: f32,
                    height: f32| {
            cx.open_window(
                WindowOptions {
                    window_background: theme::Theme::of(cx).window_background_appearance(),
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(20.), px(20.)),
                        size(px(width), px(height)),
                    ))),
                    show: false,
                    focus: false,
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| shell::Shell::new(state, boot, cx)),
            )
            .unwrap()
        };
        let mut window = open(cx, state.clone(), boot.clone(), WIDTH, HEIGHT);
        let _ = window.update(cx, |_, window, _| window.activate_window());
        let ui_state = state.clone();
        cx.spawn(async move |cx| {
            let state = ui_state;
            let result: anyhow::Result<Value> = async {
                let mut record = serde_json::Map::new();
                let select = |cx: &mut AsyncApp, id: Option<&str>| {
                    let id = id.map(str::to_string);
                    cx.update(|cx| state.update(cx, |state, cx| state.select_chat(id, cx)));
                };
                anyhow::ensure!(sessions_watched, "the sessions feed needs an attached engine");
                // Boot: the Shell exists before the selection lands, as in the app.
                settle(cx, 600).await;
                select(cx, Some(MAIN));
                let main_only = |rb: &Value| {
                    rb["available"] == true
                        && rb["loaded"] == true
                        && rb["main"] == MAIN
                        && all_panes(rb).len() == 1
                };
                // The boot overlay leaves only after the first state frame
                // (connection Ready) plus its fade; Main's proof runs after it.
                let initial = wait_for(window, cx, "Main's layout without the boot overlay", |rb| {
                    main_only(rb) && rb["bootOverlay"] == false
                })
                .await?;

                // w01. Main alone in the inset canvas: its own pane, labelled
                // Main, once. Middle-click and the Cmd-W path leave it open.
                anyhow::ensure!(tabs_of(&all_panes(&initial)[0]) == [MAIN], "Main alone: {initial}");
                anyhow::ensure!(role_of(&initial, MAIN).as_deref() == Some("Main"), "Main badge: {initial}");
                shot(window, cx, output.join("w01-main.png")).await?;
                let main_tab = tab_center(&initial, MAIN)?;
                click(window, cx, main_tab, MouseButton::Middle).await?;
                click(window, cx, main_tab, MouseButton::Left).await?;
                let in_canvas = window.update(cx, |shell, window, cx| shell.fixture_workbench_has_focus(window, cx))?;
                anyhow::ensure!(in_canvas, "keyboard focus must be inside the canvas after clicking Main");
                let alive = cmd_w(window, cx).await?;
                let after = wb(window, cx)?;
                anyhow::ensure!(alive, "Cmd-W on Main must not close the window");
                anyhow::ensure!(occurrences(&after, MAIN) == 1 && after["revision"] == initial["revision"], "Main must not close: {after}");
                progress(&output, &mut record, "initial".into(), initial);

                // w02. Related chats is the library: a row click opens the
                // child as a center tab (focused, Child), the row stays, and
                // the right pane never renders the conversation. A second
                // click focuses the same tab.
                window.update(cx, |shell, _, cx| {
                    shell.fixture_open_right_pane(cx);
                    shell.fixture_open_child_chats(cx);
                })?;
                settle(cx, 700).await;
                let pane_x = WIDTH - PANE_WIDTH / 2.0;
                let rows = related_rows(window, cx, pane_x)?;
                click(window, cx, (pane_x, row_y(&rows, CHILD_A)?), MouseButton::Left).await?;
                let opened = wait_for(window, cx, "Child A's tab", |rb| occurrences(rb, CHILD_A) == 1).await?;
                anyhow::ensure!(role_of(&opened, CHILD_A).as_deref() == Some("Child") && opened["focusedChat"] == CHILD_A, "child opens focused, as Child: {opened}");
                anyhow::ensure!(has(&opened["views"], CHILD_A), "child view mounted: {opened}");
                click(window, cx, (pane_x, row_y(&rows, CHILD_A)?), MouseButton::Left).await?;
                click(window, cx, (pane_x, row_y(&rows, CHILD_A)?), MouseButton::Left).await?;
                let again = wb(window, cx)?;
                anyhow::ensure!(occurrences(&again, CHILD_A) == 1, "repeated Open must not duplicate: {again}");
                let library = related(window, cx)?;
                anyhow::ensure!(related_ids(&library).iter().any(|id| id == CHILD_A), "the row stays in the library: {library}");
                anyhow::ensure!(library["childOpen"].is_null() && library["paneIdentity"].is_null(), "the right pane must not render the conversation: {library}");
                shot(window, cx, output.join("w02-related-open.png")).await?;
                progress(&output, &mut record, "opened".into(), again.clone());

                // w03. Drag Child A's tab onto the right edge of Main's pane:
                // a split right, previewed before release.
                let main_pane = pane_of(&again, MAIN).unwrap()["id"].as_str().unwrap().to_string();
                let (px0, py0, pw, ph) = pane_rect(&again, &main_pane)?;
                let target = (px0 + pw - 30.0, py0 + ph / 2.0);
                drag_hold(window, cx, tab_center(&again, CHILD_A)?, target).await?;
                let previewing = wb(window, cx)?;
                capture(window, cx, output.join("w03-drop-preview.png"))?;
                anyhow::ensure!(previewing["preview"] == "Split right", "edge preview before release: {previewing}");
                release(window, cx, target).await?;
                let split = wait_for(window, cx, "a right split", |rb| all_panes(rb).len() == 2).await?;
                anyhow::ensure!(split["root"]["kind"] == "split" && split["root"]["axis"] == "horizontal", "horizontal split: {split}");
                anyhow::ensure!(tabs_of(&all_panes(&split)[0]) == [MAIN] && tabs_of(&all_panes(&split)[1]) == [CHILD_A], "Main left, Child A right: {split}");
                shot(window, cx, output.join("w03-split-right.png")).await?;

                // w04. Drag Child B's Related row onto the bottom edge of
                // Child A's pane: three panes, nested.
                let a_pane = pane_of(&split, CHILD_A).unwrap()["id"].as_str().unwrap().to_string();
                let (ax, ay, aw, ah) = pane_rect(&split, &a_pane)?;
                let rows = related_rows(window, cx, pane_x)?;
                let target = (ax + aw / 2.0, ay + ah - 24.0);
                drag_hold(window, cx, (pane_x, row_y(&rows, CHILD_B)?), target).await?;
                let row_preview = wb(window, cx)?;
                anyhow::ensure!(row_preview["preview"] == "Split down", "row drag previews the same zones: {row_preview}");
                release(window, cx, target).await?;
                let nested = wait_for(window, cx, "three panes", |rb| all_panes(rb).len() == 3).await?;
                anyhow::ensure!(nested["root"]["second"]["kind"] == "split" && nested["root"]["second"]["axis"] == "vertical", "nested vertical split: {nested}");
                anyhow::ensure!(occurrences(&nested, CHILD_B) == 1 && role_of(&nested, CHILD_B).as_deref() == Some("Child"), "Child B once, as Child: {nested}");
                shot(window, cx, output.join("w04-nested-splits.png")).await?;
                progress(&output, &mut record, "nested".into(), nested.clone());

                // w05. Independent composers: drafts stay with their chat; a
                // send in Child B lands in Child B even when its tab moves
                // into Main's group while the send is in flight.
                window.update(cx, |shell, _, cx| {
                    shell.fixture_workbench_type(CHILD_A, "draft kept for A", false, cx);
                    shell.fixture_workbench_type(MAIN, "draft kept for Main", false, cx);
                })?;
                let sent = window.update(cx, |shell, _, cx| {
                    shell.fixture_workbench_type(CHILD_B, "Check the release notes", true, cx)
                })?;
                anyhow::ensure!(sent, "Child B's composer must exist");
                let main_strip = {
                    let (x, y, w, _) = pane_rect(&nested, &main_pane)?;
                    let _ = w;
                    (x + 200.0, y + 18.0)
                };
                drag(window, cx, tab_center(&nested, CHILD_B)?, main_strip).await?;
                let moved = wait_for(window, cx, "Child B in Main's group", |rb| {
                    pane_of(rb, CHILD_B).is_some_and(|pane| pane["id"] == main_pane.as_str())
                })
                .await?;
                anyhow::ensure!(all_panes(&moved).len() == 2, "Child B's empty source branch collapses: {moved}");
                let mut landed = false;
                for _ in 0..50 {
                    let texts = window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(CHILD_B, cx))?;
                    if texts.iter().any(|text| text.contains("Check the release notes")) {
                        landed = true;
                        break;
                    }
                    settle(cx, 100).await;
                }
                let main_texts = window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(MAIN, cx))?;
                anyhow::ensure!(landed, "the send must land in Child B");
                anyhow::ensure!(!main_texts.iter().any(|text| text.contains("Check the release notes")), "the send must not reach Main: {main_texts:?}");
                let drafts = window.update(cx, |shell, _, cx| {
                    (
                        shell.fixture_workbench_draft(CHILD_A, cx),
                        shell.fixture_workbench_draft(MAIN, cx),
                    )
                })?;
                anyhow::ensure!(drafts.0.as_deref() == Some("draft kept for A") && drafts.1.as_deref() == Some("draft kept for Main"), "drafts belong to their chats: {drafts:?}");
                shot(window, cx, output.join("w05-independent-composers.png")).await?;
                progress(&output, &mut record, "moved".into(), moved.clone());

                // w05b. Composer-scoped keys follow keyboard focus: with Child
                // A's composer focused, the model shortcut opens Child A's
                // picker (not Main's), its slot key picks inside it without
                // switching sessions or touching Main's config, Cmd-W closes
                // the popover rather than the tab, and Escape routes its stop
                // to Child A; back in Main, Escape routes to Main.
                click_tab(window, cx, MAIN).await?;
                wait_for(window, cx, "Main shown in its group", |rb| {
                    pane_of(rb, MAIN).is_some_and(|pane| pane["active"] == MAIN)
                })
                .await?;
                let models = |window: Window, cx: &mut AsyncApp| {
                    window.update(cx, |shell, _, cx| {
                        (
                            shell.fixture_workbench_model(MAIN, cx),
                            shell.fixture_workbench_model(CHILD_A, cx),
                        )
                    })
                };
                let focus_input = |window: Window, cx: &mut AsyncApp, chat: &'static str| {
                    window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(chat, window, cx))
                };
                let keyboard_chat = |window: Window, cx: &mut AsyncApp| {
                    window.update(cx, |shell, window, cx| shell.fixture_workbench_keyboard_chat(window, cx))
                };
                activate(window, cx)?;
                anyhow::ensure!(focus_input(window, cx, CHILD_A)?, "Child A's composer must exist");
                draw(window, cx)?;
                settle(cx, 150).await;
                anyhow::ensure!(keyboard_chat(window, cx)?.as_deref() == Some(CHILD_A), "keys reach Child A's composer while it has focus");
                let (main_before, child_before) = models(window, cx)?;
                let main_before = main_before.ok_or_else(|| anyhow::anyhow!("Main's composer"))?;
                let child_before = child_before.ok_or_else(|| anyhow::anyhow!("Child A's composer"))?;
                anyhow::ensure!(!main_before.1 && !child_before.1, "no picker open before the shortcut");
                key(window, cx, "cmd-/")?;
                settle(cx, 300).await;
                draw(window, cx)?;
                let (main_open, child_open) = models(window, cx)?;
                let (main_open, child_open) = (main_open.unwrap(), child_open.unwrap());
                anyhow::ensure!(child_open.1 && !main_open.1, "the model shortcut opens Child A's picker, not Main's: main {main_open:?}, child {child_open:?}");
                shot(window, cx, output.join("w05b-child-model-picker.png")).await?;
                let main_chat_before = wb(window, cx)?["main"].clone();
                // Slot 2 of the menu that is open (Child A's) is its second
                // listed model: that is what Child A must switch to.
                let child_rows = window.update(cx, |shell, _, cx| shell.fixture_workbench_model_rows(CHILD_A, cx))?;
                let expected_child_model = child_rows.get(1).cloned().ok_or_else(|| anyhow::anyhow!("Child A's model menu lists fewer than two models: {child_rows:?}"))?;
                key(window, cx, "cmd-2")?;
                let mut child_slot = None;
                let mut main_slot = None;
                for _ in 0..30 {
                    settle(cx, 100).await;
                    draw(window, cx)?;
                    let (main_now, child_now) = models(window, cx)?;
                    let (main_now, child_now) = (main_now.unwrap(), child_now.unwrap());
                    let done = child_now.0.as_deref() == Some(expected_child_model.as_str());
                    main_slot = Some(main_now);
                    child_slot = Some(child_now);
                    if done {
                        break;
                    }
                }
                let (main_slot, child_slot) = (main_slot.unwrap(), child_slot.unwrap());
                let after_slot = wb(window, cx)?;
                anyhow::ensure!(child_slot.0.as_deref() == Some(expected_child_model.as_str()), "Cmd-2 in Child A's picker selects its second model {expected_child_model}: {child_before:?} -> {child_slot:?}");
                anyhow::ensure!(main_slot.0 == main_before.0, "a model slot in Child A's picker leaves Main's model: {main_before:?} -> {main_slot:?}");
                anyhow::ensure!(after_slot["main"] == main_chat_before, "the slot key must not switch sessions under the popover: {after_slot}");
                anyhow::ensure!(child_slot.1, "Child A's picker stays open after a pick: {child_slot:?}");
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_overlay_owns_keyboard(cx))?, "Child A's open picker owns the keyboard");
                // Cmd-W through the real keymap: the binding dispatches
                // `CloseWindow` along the focused element's path, where the
                // Shell handles it for its own window. Record whether the
                // platform even reports this hidden window as active (the
                // app-level fallback depends on that; the Shell path does not).
                let platform_active = cx.update(|cx| cx.active_window().map(|active| active == gpui::AnyWindowHandle::from(window)));
                let closed_ok = cmd_w(window, cx).await?;
                anyhow::ensure!(closed_ok, "Cmd-W with a popover open must not close the window");
                let after_close = wb(window, cx)?;
                let (_, child_closed) = models(window, cx)?;
                anyhow::ensure!(occurrences(&after_close, CHILD_A) == 1 && !child_closed.unwrap().1, "Cmd-W closes Child A's popover, not its tab (platform active window: {platform_active:?}): {after_close}");
                anyhow::ensure!(!window.update(cx, |shell, _, cx| shell.fixture_overlay_owns_keyboard(cx))?, "no popover owns the keyboard after Cmd-W");

                // A real Stop. "Working" in the UI can mean a send that is
                // still pending, before the engine has a turn to stop, so the
                // run is ready only when the engine's own command snapshot
                // names an active turn for BOTH chats (the fence a stop must
                // carry). Then Escape with Child A focused must produce an
                // Applied stop receipt for Child A; Child A's turn ends
                // without completing while Main's same turn keeps running and
                // completes on its own. The Mock is paced for the whole run
                // (`MOCK_DELAY_MS`), so an unstopped turn takes ~6 s.
                const MAIN_PROMPT: &str = "Keep working on the release plan";
                const CHILD_PROMPT: &str = "Keep benchmarking";
                let sent = window.update(cx, |shell, _, cx| {
                    shell.fixture_workbench_type(MAIN, MAIN_PROMPT, true, cx)
                        && shell.fixture_workbench_type(CHILD_A, CHILD_PROMPT, true, cx)
                })?;
                anyhow::ensure!(sent, "both composers send");
                let snapshot = |chat: &'static str| {
                    let reader = reader.clone();
                    async move {
                        reader
                            .call(
                                luna_rpc::methods::GET_TASK_COMMAND_SNAPSHOT,
                                serde_json::json!({ "taskId": chat }),
                            )
                            .await
                            .map(|read| read["snapshot"].clone())
                    }
                };
                let active_turn = |snapshot: &Value| snapshot["activeTurnId"].as_str().filter(|id| !id.is_empty()).map(str::to_string);
                let mut ready = None;
                for _ in 0..100 {
                    settle(cx, 100).await;
                    let (main_snap, child_snap) = (snapshot(MAIN).await?, snapshot(CHILD_A).await?);
                    if active_turn(&main_snap).is_some() && active_turn(&child_snap).is_some() {
                        ready = Some((main_snap, child_snap));
                        break;
                    }
                }
                let (main_ready, child_ready) = match ready {
                    Some(pair) => pair,
                    None => anyhow::bail!(
                        "the engine never reported active turns for both live runs (backend metadata): main {}, child {}",
                        snapshot(MAIN).await?,
                        snapshot(CHILD_A).await?
                    ),
                };
                let (main_turn, child_turn) = (active_turn(&main_ready).unwrap(), active_turn(&child_ready).unwrap());
                // The UI's own view of both runs comes from the engine's
                // sessions feed (not the optimistic pending-send overlay):
                // Escape decides from it, so wait until it reports both live.
                let session = |cx: &mut AsyncApp, chat: &'static str| {
                    cx.update(|cx| state.read(cx).fixture_session_indicator(chat))
                };
                let live = |indicator: &str| matches!(indicator, "Working" | "AwaitingApproval" | "AwaitingInput");
                let mut sessions_live = None;
                for _ in 0..80 {
                    let (main_session, child_session) = (session(cx, MAIN), session(cx, CHILD_A));
                    if main_session.1 && live(&main_session.0) && live(&child_session.0) {
                        sessions_live = Some((main_session.0, child_session.0));
                        break;
                    }
                    settle(cx, 100).await;
                }
                let (main_session, child_session) = sessions_live.ok_or_else(|| {
                    anyhow::anyhow!(
                        "the sessions feed never showed both runs live while the engine reported turns {main_turn} / {child_turn}: main {:?}, child {:?}",
                        session(cx, MAIN),
                        session(cx, CHILD_A)
                    )
                })?;
                anyhow::ensure!(focus_input(window, cx, CHILD_A)?, "refocus Child A");
                draw(window, cx)?;
                settle(cx, 120).await;
                anyhow::ensure!(keyboard_chat(window, cx)?.as_deref() == Some(CHILD_A), "keys reach Child A's composer while it has focus");
                key(window, cx, "escape")?;
                let escapes = window.update(cx, |shell, _, _| shell.fixture_workbench_escape_log())?;
                anyhow::ensure!(escapes.last().is_some_and(|(chat, outcome)| chat.as_deref() == Some(CHILD_A) && *outcome == "interrupt"), "Escape interrupts Child A: {escapes:?}");
                // The receipt for the exact stop command the composer sent.
                let mut stop = Value::Null;
                for _ in 0..80 {
                    settle(cx, 100).await;
                    stop = window.update(cx, |shell, _, cx| shell.fixture_workbench_last_stop(CHILD_A, cx))?;
                    if !stop.is_null() {
                        break;
                    }
                }
                // That first receipt is the engine's acceptance (an async
                // control is `pending` until it settles); an error string
                // means it was never accepted.
                anyhow::ensure!(stop.get("error").is_none() && stop["commandId"].is_string(), "the engine accepted Child A's stop: {stop}");
                anyhow::ensure!(stop["taskId"] == CHILD_A, "the stop names Child A: {stop}");
                anyhow::ensure!(matches!(stop["status"].as_str(), Some("pending" | "applied")), "Child A's stop was accepted, not refused: {stop}");
                // Settlement: read the durable receipt for that command id
                // until it is terminal; only Applied passes.
                let command_id = stop["commandId"].as_str().unwrap_or_default().to_string();
                let mut settled = Value::Null;
                for _ in 0..100 {
                    let receipt = reader
                        .call(
                            luna_rpc::methods::GET_TASK_COMMAND_RECEIPT,
                            serde_json::json!({ "commandId": command_id }),
                        )
                        .await?;
                    let terminal = receipt["status"].as_str().is_some_and(|status| status != "pending");
                    settled = receipt;
                    if terminal {
                        break;
                    }
                    settle(cx, 100).await;
                }
                anyhow::ensure!(settled["status"] == "applied", "Child A's stop settles Applied (not pending, conflict, rejected, superseded or failed): accepted {stop}, settled {settled}");
                anyhow::ensure!(settled["commandId"] == command_id.as_str() && settled["taskId"] == CHILD_A, "the settled receipt is that command, for Child A: {settled}");
                let main_stop = window.update(cx, |shell, _, cx| shell.fixture_workbench_last_stop(MAIN, cx))?;
                anyhow::ensure!(main_stop.is_null(), "Main's composer sent no stop: {main_stop}");
                // Child A's turn ends; at that moment Main's same turn is
                // still the active one.
                let mut child_ended = None;
                for _ in 0..80 {
                    let child_now = snapshot(CHILD_A).await?;
                    if active_turn(&child_now).as_deref() != Some(child_turn.as_str()) {
                        child_ended = Some((child_now, snapshot(MAIN).await?));
                        break;
                    }
                    settle(cx, 100).await;
                }
                let (child_after, main_at_child_end) = child_ended.ok_or_else(|| anyhow::anyhow!("Child A's turn {child_turn} never ended after an Applied stop"))?;
                anyhow::ensure!(active_turn(&main_at_child_end).as_deref() == Some(main_turn.as_str()), "Main's turn {main_turn} is still active when Child A's ends: {main_at_child_end}");
                shot(window, cx, output.join("w05b-child-stopped-main-running.png")).await?;
                // Main completes its own turn with the Mock's answer.
                let mut main_done = None;
                for _ in 0..200 {
                    settle(cx, 100).await;
                    let main_now = snapshot(MAIN).await?;
                    let output_now = window.update(cx, |shell, _, cx| shell.fixture_workbench_turn_after(MAIN, MAIN_PROMPT, cx))?;
                    let completed = output_now.iter().any(|entry| entry["role"] == "Assistant" && entry["status"] == "Complete" && entry["text"].as_str().is_some_and(|text| text.contains(MOCK_ANSWER)));
                    if active_turn(&main_now).as_deref() != Some(main_turn.as_str()) && completed {
                        main_done = Some((main_now, output_now));
                        break;
                    }
                }
                let (main_after, main_output) = main_done.ok_or_else(|| anyhow::anyhow!("Main's turn {main_turn} never completed with the Mock's answer"))?;
                // By now an unstopped Child A turn would have completed too:
                // its turn must show no completed answer (an aborted partial,
                // if any, is allowed).
                let child_output = window.update(cx, |shell, _, cx| shell.fixture_workbench_turn_after(CHILD_A, CHILD_PROMPT, cx))?;
                anyhow::ensure!(!child_output.iter().any(|entry| entry["role"] == "Assistant" && entry["status"] == "Complete"), "Child A's stopped turn has no completed answer: {child_output:?}");
                let main_never_interrupted = !window.update(cx, |shell, _, cx| shell.fixture_workbench_interrupting(MAIN, cx))?;
                anyhow::ensure!(main_never_interrupted, "Main was never interrupted");
                let stop_evidence = serde_json::json!({
                    "readySnapshots": {"main": main_ready, "child": child_ready},
                    "activeTurns": {"main": main_turn, "child": child_turn},
                    "sessionIndicatorsAtEscape": {"main": main_session, "child": child_session},
                    "childStopReceipt": {"accepted": stop, "settled": settled},
                    "childSnapshotAfterStop": child_after,
                    "mainSnapshotWhenChildEnded": main_at_child_end,
                    "mainSnapshotAfterCompletion": main_after,
                    "childTurnOutput": child_output,
                    "mainTurnOutput": main_output,
                    "childInterrupted": true,
                    "mainCompleted": true,
                });
                // Saved now, so a later step's failure keeps this evidence.
                progress(&output, &mut record, "stop".into(), stop_evidence.clone());
                // Back in Main (now idle), Escape routes to Main and stops
                // nothing.
                anyhow::ensure!(focus_input(window, cx, MAIN)?, "focus Main");
                draw(window, cx)?;
                settle(cx, 120).await;
                anyhow::ensure!(keyboard_chat(window, cx)?.as_deref() == Some(MAIN), "keys reach Main's composer while it has focus");
                key(window, cx, "escape")?;
                settle(cx, 150).await;
                let escapes = window.update(cx, |shell, _, _| shell.fixture_workbench_escape_log())?;
                anyhow::ensure!(escapes.last().is_some_and(|(chat, outcome)| chat.as_deref() == Some(MAIN) && *outcome == "ignored"), "Escape in idle Main routes to Main and stops nothing: {escapes:?}");
                progress(&output, &mut record,
                    "keyboardRouting".into(),
                    serde_json::json!({
                        "mainModelBefore": main_before.0,
                        "mainModelAfterChildSlot": main_slot.0,
                        "childModelBefore": child_before.0,
                        "childModelExpected": expected_child_model,
                        "childModelAfterSlot": child_slot.0,
                        "platformReportsWindowActive": platform_active,
                        "stop": stop_evidence,
                        "escapeTargets": escapes.iter().map(|(chat, outcome)| serde_json::json!({"chat": chat, "outcome": outcome})).collect::<Vec<_>>(),
                    }),
                );

                // w05c. Keyboard focus moving between groups (Tab / Shift-Tab
                // through the real key path) makes the group it lands in the
                // focused one, so the frame's checkout/Git context, Cmd-W and
                // New child follow it, including an alongside chat on another
                // checkout.
                // Three visible groups: Main | Child A | the alongside chat
                // (opened in its own group right of Child A's, so nothing
                // hides Main or Child A).
                let before_along = wb(window, cx)?;
                let main_group = pane_of(&before_along, MAIN).ok_or_else(|| anyhow::anyhow!("Main's group: {before_along}"))?["id"].as_str().unwrap().to_string();
                let child_group = pane_of(&before_along, CHILD_A).ok_or_else(|| anyhow::anyhow!("Child A's group: {before_along}"))?["id"].as_str().unwrap().to_string();
                let opened = window.update(cx, |shell, _, cx| shell.fixture_workbench_open_beside(ALONG, &child_group, cx))?;
                anyhow::ensure!(opened, "open the alongside chat beside Child A");
                let along_open = wait_for(window, cx, "the alongside chat in its own visible group", |rb| {
                    pane_of(rb, ALONG).is_some_and(|pane| pane["active"] == ALONG && tabs_of(&pane) == [ALONG])
                        && pane_of(rb, MAIN).is_some_and(|pane| pane["active"] == MAIN)
                        && pane_of(rb, CHILD_A).is_some_and(|pane| pane["active"] == CHILD_A)
                })
                .await?;
                let along_pane = pane_of(&along_open, ALONG).unwrap()["id"].as_str().unwrap().to_string();
                anyhow::ensure!(along_pane != main_group && along_pane != child_group, "a separate group: {along_open}");
                anyhow::ensure!(role_of(&along_open, ALONG).as_deref() == Some("Alongside"), "an ordinary chat opens as Alongside: {along_open}");
                anyhow::ensure!(along_open["views"].as_array().is_some_and(|views| views.iter().any(|id| id == ALONG)), "the alongside chat's view is mounted: {along_open}");
                // Three groups with both sidebars open, after the wider
                // two-group split: each tab narrows to its strip, so every
                // active tab (Main's long title included) keeps its label and
                // close control inside the strip, clear of the controls.
                let fitted = wait_for(window, cx, "every active tab fits its narrowed strip", active_tabs_fit).await?;
                anyhow::ensure!(fitted["root"]["kind"] == "split", "still split: {fitted}");
                // Start in Main's group with a real click on its tab.
                click_tab(window, cx, MAIN).await?;
                let in_main = wait_for(window, cx, "Main's group focused by its tab", |rb| rb["focusedPane"] == main_group.as_str()).await?;
                anyhow::ensure!(in_main["focusedChat"] == MAIN && in_main["footer"]["chat"] == MAIN, "the frame row binds Main: {in_main}");
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_has_focus(window, cx))?, "keyboard focus is in the canvas after the click");
                let mut reached = None;
                for presses in 1..=200 {
                    key(window, cx, "tab")?;
                    settle(cx, 40).await;
                    draw(window, cx)?;
                    let inside = window.update(cx, |shell, window, cx| shell.fixture_workbench_has_focus(window, cx))?;
                    if inside && keyboard_chat(window, cx)?.as_deref() == Some(ALONG) {
                        reached = Some(presses);
                        break;
                    }
                }
                let presses = reached.ok_or_else(|| anyhow::anyhow!("Tab never reached the alongside chat's composer"))?;
                draw(window, cx)?;
                let in_along = wait_for(window, cx, "the alongside group focused", |rb| rb["focusedPane"] == along_pane.as_str()).await?;
                anyhow::ensure!(in_along["focusedChat"] == ALONG && in_along["footer"]["chat"] == ALONG, "Tab into the alongside group moves the frame row to it: {in_along}");
                anyhow::ensure!(in_along["footer"]["cwd"].as_str().is_some_and(|cwd| cwd.ends_with(ALONG_CHECKOUT)), "the frame row names the alongside chat's own checkout: {in_along}");
                shot(window, cx, output.join("w05c-tab-focus-alongside.png")).await?;
                let in_canvas = |window: Window, cx: &mut AsyncApp| {
                    window.update(cx, |shell, window, cx| shell.fixture_workbench_has_focus(window, cx))
                };
                let mut back = None;
                for presses in 1..=160 {
                    key(window, cx, "shift-tab")?;
                    settle(cx, 40).await;
                    // Outside the canvas the keys belong to Main as well;
                    // only focus inside Main's group counts as arriving.
                    draw(window, cx)?;
                    if in_canvas(window, cx)? && keyboard_chat(window, cx)?.as_deref() == Some(MAIN) {
                        back = Some(presses);
                        break;
                    }
                }
                let back = back.ok_or_else(|| anyhow::anyhow!("Shift-Tab never returned to Main's composer"))?;
                draw(window, cx)?;
                let back_in_main = wait_for(window, cx, "Main's group focused again", |rb| rb["focusedPane"] == main_group.as_str()).await?;
                anyhow::ensure!(back_in_main["footer"]["chat"] == MAIN, "Shift-Tab back moves the frame row to Main: {back_in_main}");
                anyhow::ensure!(back_in_main["footer"]["cwd"].as_str().is_some_and(|cwd| !cwd.ends_with(ALONG_CHECKOUT)), "Main's own checkout: {back_in_main}");
                // Leave the alongside tab closed again; Child A shows in its
                // group as before.
                let closed_along = window.update(cx, |shell, _, cx| shell.fixture_workbench_close(ALONG, cx))?;
                anyhow::ensure!(closed_along, "close the alongside tab");
                wait_for(window, cx, "the alongside tab closed", |rb| occurrences(rb, ALONG) == 0).await?;
                progress(&output, &mut record,
                    "focusSync".into(),
                    serde_json::json!({"tabPresses": presses, "shiftTabPresses": back, "alongside": in_along, "main": back_in_main}),
                );

                // w06. Divider: follows the pointer locally inside the legal
                // range with no write per move and commits once; a drag past
                // the minimum pane width stops at that minimum; double-click
                // equalizes. (Re-read: w05b/w05c's focus moves advanced the
                // revision since w05.)
                let moved = wb(window, cx)?;
                anyhow::ensure!(all_panes(&moved).len() == 2 && moved["pendingMutations"] == 0, "back to Main's and Child A's groups, settled: {moved}");
                let (sx, sy, sw, sh) = split_rect(&moved)?;
                // Both sides keep >= 320 px (engine floor 0.1).
                let floor = (320.0 / sw).max(0.1);
                let ratio = moved["root"]["ratio"].as_f64().unwrap_or(0.5) as f32;
                let legal = (sw * (ratio - floor) * 0.6).max(8.0);
                let divider = (sx + sw * ratio, sy + sh / 2.0);
                let revision = moved["revision"].as_u64().unwrap_or(0);
                drag_hold(window, cx, divider, (divider.0 - legal, divider.1)).await?;
                let resizing = wb(window, cx)?;
                anyhow::ensure!(resizing["resize"].is_object() && resizing["revision"].as_u64() == Some(revision) && resizing["pendingMutations"] == 0, "local preview, no writes while dragging: {resizing}");
                let live = resizing["resize"]["ratio"].as_f64().unwrap_or(0.0) as f32;
                anyhow::ensure!((live - (ratio - legal / sw)).abs() < 0.015, "the live ratio tracks the pointer: {live} vs {}", ratio - legal / sw);
                release(window, cx, (divider.0 - legal, divider.1)).await?;
                let resized = wait_for(window, cx, "the committed ratio", |rb| rb["revision"].as_u64() == Some(revision + 1)).await?;
                let followed = resized["root"]["ratio"].as_f64().unwrap_or(0.5) as f32;
                anyhow::ensure!((followed - (ratio - legal / sw)).abs() < 0.015 && followed >= floor - 0.002, "an in-range drag commits where the pointer is: {resized}");
                shot(window, cx, output.join("w06-resized.png")).await?;
                // Past the minimum: the ratio stops at the floor (exactly the
                // clamp), and both panes stay at least 320 px wide.
                let divider = (sx + sw * followed, sy + sh / 2.0);
                let revision = resized["revision"].as_u64().unwrap_or(0);
                drag(window, cx, divider, (divider.0 - 260.0, divider.1)).await?;
                let clamped = wait_for(window, cx, "the clamped ratio", |rb| rb["revision"].as_u64() == Some(revision + 1)).await?;
                let at_floor = clamped["root"]["ratio"].as_f64().unwrap_or(0.5) as f32;
                anyhow::ensure!((at_floor - floor).abs() < 0.004, "a drag past the minimum stops at it: {at_floor} vs floor {floor}: {clamped}");
                for entry in clamped["paneBounds"].as_array().into_iter().flatten() {
                    let (_, _, w, _) = rect(&entry["bounds"]);
                    anyhow::ensure!(w >= 319.0, "no pane below the minimum width: {clamped}");
                }
                shot(window, cx, output.join("w06b-min-width.png")).await?;
                let divider = (sx + sw * at_floor, sy + sh / 2.0);
                double_click(window, cx, divider).await?;
                let equal = wait_for(window, cx, "equal panes", |rb| (rb["root"]["ratio"].as_f64().unwrap_or(0.0) - 0.5).abs() < 0.001).await?;
                progress(&output, &mut record, "equalized".into(), equal.clone());

                // w07. `+` while Child A's group is focused: one blank child
                // of Main (not of Child A), in that group, composer focused.
                let a_pane = pane_of(&equal, CHILD_A).unwrap()["id"].as_str().unwrap().to_string();
                click_tab(window, cx, CHILD_A).await?;
                let before_rows = merge_engine_chats(&reader, &state, cx).await?;
                let plus = plus_center(&wb(window, cx)?, &a_pane)?;
                click(window, cx, plus, MouseButton::Left).await?;
                let created = wait_for(window, cx, "a new child tab", |rb| {
                    rb["creatingChild"] == false
                        && pane_of(rb, CHILD_A).is_some_and(|pane| tabs_of(&pane).len() == 2)
                })
                .await?;
                let new_child = tabs_of(&pane_of(&created, CHILD_A).unwrap())
                    .into_iter()
                    .find(|id| id != CHILD_A)
                    .unwrap();
                let after_rows = merge_engine_chats(&reader, &state, cx).await?;
                anyhow::ensure!(after_rows.len() == before_rows.len() + 1, "exactly one child per gesture: {} -> {}", before_rows.len(), after_rows.len());
                let row = after_rows.iter().find(|chat| chat.id == new_child).cloned().unwrap();
                anyhow::ensure!(row.child_thread.as_ref().is_some_and(|child| child.parent_task_id == MAIN), "the parent is Main, not the focused child: {row:?}");
                settle(cx, 400).await;
                let created = wb(window, cx)?;
                anyhow::ensure!(created["focusedChat"] == new_child.as_str() && role_of(&created, &new_child).as_deref() == Some("Child"), "new child focused, as Child: {created}");
                let texts = window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(&new_child, cx))?;
                anyhow::ensure!(texts.is_empty(), "a new child starts empty: {texts:?}");
                let focused = window.update(cx, |shell, window, cx| shell.fixture_workbench_has_focus(window, cx))?;
                anyhow::ensure!(focused, "its composer takes keyboard focus");
                shot(window, cx, output.join("w07-new-child.png")).await?;
                progress(&output, &mut record, "newChild".into(), created.clone());

                // w08. Reorder: the new tab before Child A, marker first; a
                // drop in place changes nothing.
                let (lx, ly) = tab_left(&created, CHILD_A)?;
                drag_hold(window, cx, tab_center(&created, &new_child)?, (lx, ly)).await?;
                let marker = wb(window, cx)?;
                anyhow::ensure!(marker["preview"] == "Move tab here", "insertion preview: {marker}");
                release(window, cx, (lx, ly)).await?;
                let reordered = wait_for(window, cx, "the new order", |rb| {
                    pane_of(rb, CHILD_A).is_some_and(|pane| tabs_of(&pane) == [new_child.clone(), CHILD_A.to_string()])
                })
                .await?;
                let revision = reordered["revision"].clone();
                let at = tab_center(&reordered, &new_child)?;
                drag(window, cx, at, (at.0 + 8.0, at.1)).await?;
                let same = wb(window, cx)?;
                anyhow::ensure!(same["revision"] == revision && same["root"] == reordered["root"], "self drop is a no-op: {same}");

                // w09. Cancel: Escape mid-drag removes the preview; release
                // changes nothing. The target is a legal drop: the center of
                // Main's group (join it), not an edge split that the minimum
                // pane width correctly refuses there.
                let (mx, my, mw, mh) = pane_rect(&same, &main_pane)?;
                let cancel_to = (mx + mw / 2.0, my + mh / 2.0);
                drag_hold(window, cx, tab_center(&same, &new_child)?, cancel_to).await?;
                let previewing = wb(window, cx)?;
                anyhow::ensure!(previewing["preview"] == "Move to this group", "a join preview to cancel: {previewing}");
                key(window, cx, "escape")?;
                draw(window, cx)?;
                anyhow::ensure!(wb(window, cx)?["preview"].is_null(), "Escape clears the preview");
                release(window, cx, cancel_to).await?;
                let cancelled = wb(window, cx)?;
                anyhow::ensure!(cancelled["revision"] == revision && cancelled["root"] == same["root"], "a cancelled drag keeps the layout: {cancelled}");

                // w09b. A crowded, narrow strip: a newly opened, a
                // keyboard-selected (Home/End) and a reopened tab is scrolled
                // wholly into view (label and close control visible), and
                // the trailing controls stay clear of the tabs.
                let child_group = pane_of(&cancelled, CHILD_A).ok_or_else(|| anyhow::anyhow!("Child A's group: {cancelled}"))?["id"].as_str().unwrap().to_string();
                click_tab(window, cx, CHILD_A).await?;
                wait_for(window, cx, "Child A's group focused", |rb| rb["focusedPane"] == child_group.as_str()).await?;
                for extra in [ALONG, MAIN2] {
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_open(extra, cx))?;
                    let group = child_group.clone();
                    wait_for(window, cx, &format!("{extra} opened, active and revealed in Child A's group"), move |rb| {
                        pane_of(rb, extra).is_some_and(|pane| pane["id"] == group.as_str() && pane["active"] == extra)
                            && fully_revealed(rb, extra)
                            && controls_clear(rb, &group)
                    })
                    .await?;
                }
                let crowded = wb(window, cx)?;
                let group_tabs = pane_of(&crowded, CHILD_A).map(|pane| tabs_of(&pane)).unwrap_or_default();
                anyhow::ensure!(group_tabs.len() >= 4 && strip_overflows(&crowded, &child_group), "a crowded strip that overflows: {crowded}");
                shot(window, cx, output.join("w09b-reveal-opened.png")).await?;
                // Keyboard: Shift-Tab back from the new tab's composer to a
                // tab in this group, then Home / End select the strip's ends.
                let mut on_tab = None;
                for _ in 0..40 {
                    key(window, cx, "shift-tab")?;
                    settle(cx, 40).await;
                    draw(window, cx)?;
                    let focused = window.update(cx, |shell, window, _| shell.fixture_workbench_focused_tab(window))?;
                    if focused.as_ref().is_some_and(|chat| group_tabs.contains(chat)) {
                        on_tab = focused;
                        break;
                    }
                }
                anyhow::ensure!(on_tab.is_some(), "Shift-Tab reaches a tab in Child A's group");
                let (first, last) = (group_tabs[0].clone(), group_tabs[group_tabs.len() - 1].clone());
                key(window, cx, "home")?;
                let first_shown = {
                    let (group, first) = (child_group.clone(), first.clone());
                    wait_for(window, cx, "Home selects and reveals the first tab", move |rb| {
                        pane_of(rb, CHILD_A).is_some_and(|pane| pane["active"] == first.as_str())
                            && fully_revealed(rb, &first)
                            && controls_clear(rb, &group)
                    })
                    .await?
                };
                key(window, cx, "end")?;
                let last_shown = {
                    let (group, last) = (child_group.clone(), last.clone());
                    wait_for(window, cx, "End selects and reveals the last tab", move |rb| {
                        pane_of(rb, CHILD_A).is_some_and(|pane| pane["active"] == last.as_str())
                            && fully_revealed(rb, &last)
                            && controls_clear(rb, &group)
                    })
                    .await?
                };
                // Close the last tab, scroll the strip back to its start
                // (Home), then reopen it: Cmd-Shift-T reveals it again.
                let closed = window.update(cx, |shell, _, cx| shell.fixture_workbench_close(&last, cx))?;
                anyhow::ensure!(closed, "close {last}");
                {
                    let last = last.clone();
                    wait_for(window, cx, "the last tab closed", move |rb| occurrences(rb, &last) == 0).await?;
                }
                let mut back_on_tab = false;
                for _ in 0..40 {
                    let focused = window.update(cx, |shell, window, _| shell.fixture_workbench_focused_tab(window))?;
                    if focused.as_ref().is_some_and(|chat| group_tabs.contains(chat)) {
                        back_on_tab = true;
                        break;
                    }
                    key(window, cx, "shift-tab")?;
                    settle(cx, 40).await;
                    draw(window, cx)?;
                }
                anyhow::ensure!(back_on_tab, "keyboard focus back on a tab in Child A's group");
                key(window, cx, "home")?;
                {
                    let first = first.clone();
                    wait_for(window, cx, "the strip back at its start", move |rb| fully_revealed(rb, &first)).await?;
                }
                activate(window, cx)?;
                key(window, cx, "cmd-shift-t")?;
                let reopened_shown = {
                    let (group, last) = (child_group.clone(), last.clone());
                    wait_for(window, cx, "Cmd-Shift-T reopens and reveals the tab", move |rb| {
                        pane_of(rb, &last).is_some_and(|pane| pane["id"] == group.as_str() && pane["active"] == last.as_str())
                            && fully_revealed(rb, &last)
                            && controls_clear(rb, &group)
                    })
                    .await?
                };
                shot(window, cx, output.join("w09b-reveal-reopened.png")).await?;
                progress(
                    &output,
                    &mut record,
                    "tabReveal".into(),
                    serde_json::json!({"crowded": crowded, "home": first_shown, "end": last_shown, "reopened": reopened_shown}),
                );
                // Back to the w09 layout for the steps that follow.
                for extra in [ALONG, MAIN2] {
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_close(extra, cx))?;
                }
                wait_for(window, cx, "the extra tabs closed", |rb| occurrences(rb, ALONG) == 0 && occurrences(rb, MAIN2) == 0).await?;
                // The new child (the group's first tab) selected and in view
                // again, by keyboard, for w10's click.
                let mut restore_on_tab = false;
                for _ in 0..40 {
                    let focused = window.update(cx, |shell, window, _| shell.fixture_workbench_focused_tab(window))?;
                    if focused.as_deref().is_some_and(|chat| chat == new_child || chat == CHILD_A) {
                        restore_on_tab = true;
                        break;
                    }
                    key(window, cx, "shift-tab")?;
                    settle(cx, 40).await;
                    draw(window, cx)?;
                }
                anyhow::ensure!(restore_on_tab, "keyboard focus on a tab in Child A's group");
                key(window, cx, "home")?;
                let cancelled = {
                    let first = new_child.clone();
                    wait_for(window, cx, "the new child selected and revealed again", move |rb| {
                        pane_of(rb, &first).is_some_and(|pane| pane["active"] == first.as_str() && tabs_of(&pane)[0] == first)
                            && fully_revealed(rb, &first)
                    })
                    .await?
                };

                // w09c. One-move drags (a fast flick, or a native synthetic
                // drag): the release resolves the same drop the preview
                // would have — a tab split, a join, a Related row move and a
                // divider ratio — while a sub-threshold press stays a click,
                // a self drop and a divider click change nothing, a
                // double-click still equalizes, and Escape (before the drag
                // starts or after it, with no preview or live ratio yet)
                // cancels the gesture so its release commits nothing.
                let base = wb(window, cx)?;
                let child_group = pane_of(&base, CHILD_A).ok_or_else(|| anyhow::anyhow!("Child A's group: {base}"))?["id"].as_str().unwrap().to_string();
                let (gx, gy, gw, gh) = pane_rect(&base, &child_group)?;
                // Tab -> bottom edge of its own group: a vertical split.
                flick(window, cx, tab_center(&base, CHILD_A)?, (gx + gw / 2.0, gy + gh - 20.0)).await?;
                let flick_split = {
                    let group = child_group.clone();
                    wait_for(window, cx, "a one-move tab drag splits its group down", move |rb| {
                        all_panes(rb).len() == 3
                            && pane_of(rb, CHILD_A).is_some_and(|pane| pane["id"] != group.as_str() && tabs_of(&pane) == [CHILD_A])
                    })
                    .await?
                };
                anyhow::ensure!(flick_split["revision"].as_u64() > base["revision"].as_u64(), "the split is committed: {flick_split}");
                // Back: a one-move drag onto the old group's center joins it
                // (the emptied group collapses).
                let (hx, hy, hw, hh) = pane_rect(&flick_split, &child_group)?;
                flick(window, cx, tab_center(&flick_split, CHILD_A)?, (hx + hw / 2.0, hy + hh / 2.0)).await?;
                let flick_join = {
                    let group = child_group.clone();
                    wait_for(window, cx, "a one-move tab drag joins a group", move |rb| {
                        all_panes(rb).len() == 2 && pane_of(rb, CHILD_A).is_some_and(|pane| pane["id"] == group.as_str())
                    })
                    .await?
                };
                // Sub-threshold travel (3 px): a click; the layout stays.
                let at = tab_center(&flick_join, CHILD_A)?;
                flick(window, cx, at, (at.0 + 3.0, at.1)).await?;
                let after_click = wb(window, cx)?;
                anyhow::ensure!(after_click["root"] == flick_join["root"] && all_panes(&after_click).len() == 2, "a sub-threshold one-move press is a click: {after_click}");
                // A self drop (8 px along its own strip): no change.
                flick(window, cx, at, (at.0 + 8.0, at.1)).await?;
                let after_self = wb(window, cx)?;
                anyhow::ensure!(after_self["root"] == after_click["root"] && after_self["revision"] == after_click["revision"], "a one-move self drop is a no-op: {after_self}");
                // Escape before the drag starts: press, Escape, one move to
                // a valid edge, release: nothing.
                let edge = (gx + gw / 2.0, gy + gh - 20.0);
                hover_at(window, cx, at, None)?;
                draw(window, cx)?;
                press(window, cx, at, MouseButton::Left, true, 1)?;
                key(window, cx, "escape")?;
                hover_at(window, cx, edge, Some(MouseButton::Left))?;
                press(window, cx, edge, MouseButton::Left, false, 1)?;
                settle(cx, 250).await;
                let escaped_early = wb(window, cx)?;
                anyhow::ensure!(escaped_early["root"] == after_self["root"] && escaped_early["revision"] == after_self["revision"] && escaped_early["preview"].is_null(), "Escape before the drag starts cancels it: {escaped_early}");
                // Escape after the drag starts, before any preview: press,
                // one move (the drag starts; no preview yet), Escape, release.
                hover_at(window, cx, at, None)?;
                draw(window, cx)?;
                press(window, cx, at, MouseButton::Left, true, 1)?;
                hover_at(window, cx, edge, Some(MouseButton::Left))?;
                let no_preview = wb(window, cx)?;
                anyhow::ensure!(no_preview["preview"].is_null(), "no preview before any drag move: {no_preview}");
                key(window, cx, "escape")?;
                press(window, cx, edge, MouseButton::Left, false, 1)?;
                settle(cx, 250).await;
                let escaped_late = wb(window, cx)?;
                anyhow::ensure!(escaped_late["root"] == after_self["root"] && escaped_late["revision"] == after_self["revision"], "Escape after the drag starts, before a preview, cancels it: {escaped_late}");
                // Divider: one move commits the ratio at the release point.
                let (sx, sy, sw, sh) = split_rect(&escaped_late)?;
                let ratio = escaped_late["root"]["ratio"].as_f64().unwrap_or(0.5) as f32;
                let floor = (320.0 / sw).max(0.1);
                let divider = (sx + sw * ratio, sy + sh / 2.0);
                let shift = (sw * (ratio - floor) * 0.5).max(8.0);
                let revision = escaped_late["revision"].as_u64().unwrap_or(0);
                flick(window, cx, divider, (divider.0 - shift, divider.1)).await?;
                let flicked = wait_for(window, cx, "a one-move divider drag commits", |rb| rb["revision"].as_u64() == Some(revision + 1)).await?;
                let committed = flicked["root"]["ratio"].as_f64().unwrap_or(0.0) as f32;
                anyhow::ensure!((committed - (ratio - shift / sw)).abs() < 0.015 && committed >= floor - 0.002, "the ratio follows the release: {committed} vs {}: {flicked}", ratio - shift / sw);
                // A divider click (no travel) changes nothing.
                let at_divider = (sx + sw * committed, sy + sh / 2.0);
                click(window, cx, at_divider, MouseButton::Left).await?;
                let clicked = wb(window, cx)?;
                anyhow::ensure!(clicked["revision"] == flicked["revision"] && clicked["root"]["ratio"] == flicked["root"]["ratio"], "a divider click does not resize: {clicked}");
                // Escape mid-gesture on the divider: nothing.
                hover_at(window, cx, at_divider, None)?;
                draw(window, cx)?;
                press(window, cx, at_divider, MouseButton::Left, true, 1)?;
                key(window, cx, "escape")?;
                hover_at(window, cx, (at_divider.0 + shift, at_divider.1), Some(MouseButton::Left))?;
                press(window, cx, (at_divider.0 + shift, at_divider.1), MouseButton::Left, false, 1)?;
                settle(cx, 250).await;
                let divider_escaped = wb(window, cx)?;
                anyhow::ensure!(divider_escaped["revision"] == clicked["revision"] && divider_escaped["root"]["ratio"] == clicked["root"]["ratio"] && divider_escaped["resize"].is_null(), "Escape cancels a one-move divider drag: {divider_escaped}");
                // Double-click still equalizes; a click after it keeps it.
                double_click(window, cx, at_divider).await?;
                let equalized_again = wait_for(window, cx, "double-click equalizes", |rb| (rb["root"]["ratio"].as_f64().unwrap_or(0.0) - 0.5).abs() < 0.002).await?;
                let at_middle = (sx + sw * 0.5, sy + sh / 2.0);
                click(window, cx, at_middle, MouseButton::Left).await?;
                let kept = wb(window, cx)?;
                anyhow::ensure!(kept["revision"] == equalized_again["revision"] && kept["root"]["ratio"] == equalized_again["root"]["ratio"], "a click after equalizing does not undo it: {kept}");
                // A Related row, one move onto Child A's group center: the
                // chat moves there (it was open in Main's group).
                window.update(cx, |shell, _, cx| {
                    shell.fixture_open_right_pane(cx);
                    shell.fixture_open_child_chats(cx);
                })?;
                settle(cx, 600).await;
                let with_related = wb(window, cx)?;
                let pane_x = WIDTH - PANE_WIDTH / 2.0;
                let rows = related_rows(window, cx, pane_x)?;
                let (cgx, cgy, cgw, cgh) = pane_rect(&with_related, &child_group)?;
                flick(window, cx, (pane_x, row_y(&rows, CHILD_B)?), (cgx + cgw / 2.0, cgy + cgh / 2.0)).await?;
                let row_moved = {
                    let group = child_group.clone();
                    wait_for(window, cx, "a one-move Related row drag moves the chat", move |rb| {
                        pane_of(rb, CHILD_B).is_some_and(|pane| pane["id"] == group.as_str()) && occurrences(rb, CHILD_B) == 1
                    })
                    .await?
                };
                // Back to Main's group, by a one-move tab drag onto its center.
                let (mgx, mgy, mgw, mgh) = pane_rect(&row_moved, &main_pane)?;
                flick(window, cx, tab_center(&row_moved, CHILD_B)?, (mgx + mgw / 2.0, mgy + mgh / 2.0)).await?;
                let row_back = wait_for(window, cx, "Child B back in Main's group", |rb| {
                    pane_of(rb, CHILD_B).is_some_and(|pane| pane["id"] == main_pane.as_str())
                })
                .await?;
                progress(
                    &output,
                    &mut record,
                    "oneMoveDrags".into(),
                    serde_json::json!({
                        "tabSplit": flick_split, "tabJoin": flick_join, "subThreshold": after_click,
                        "selfDrop": after_self, "escapeBeforeStart": escaped_early, "escapeBeforePreview": escaped_late,
                        "dividerCommit": flicked, "dividerClick": clicked, "dividerEscape": divider_escaped,
                        "dividerDoubleClick": equalized_again, "clickAfterEqualize": kept,
                        "relatedRowMove": row_moved, "relatedRowBack": row_back,
                    }),
                );
                // The new child (its group's first tab) selected and in view
                // again for w10, by keyboard.
                let mut restore_focus = false;
                for _ in 0..60 {
                    let focused = window.update(cx, |shell, window, _| shell.fixture_workbench_focused_tab(window))?;
                    if focused.as_deref().is_some_and(|chat| chat == new_child || chat == CHILD_A) {
                        restore_focus = true;
                        break;
                    }
                    key(window, cx, "shift-tab")?;
                    settle(cx, 40).await;
                    draw(window, cx)?;
                }
                anyhow::ensure!(restore_focus, "keyboard focus on a tab in Child A's group");
                key(window, cx, "home")?;
                let cancelled = {
                    let first = new_child.clone();
                    wait_for(window, cx, "the new child selected and revealed for w10", move |rb| {
                        pane_of(rb, &first).is_some_and(|pane| pane["active"] == first.as_str() && tabs_of(&pane)[0] == first)
                            && fully_revealed(rb, &first)
                    })
                    .await?
                };

                // w10. Close is a view action: Cmd-W closes the focused new
                // child (draft kept); Main never closes; Cmd-Shift-T and the
                // Related row restore it with its draft.
                window.update(cx, |shell, _, cx| {
                    shell.fixture_workbench_type(&new_child, "keep this draft", false, cx);
                })?;
                click_tab(window, cx, &new_child).await?;
                let alive = cmd_w(window, cx).await?;
                let after_close = wait_for(window, cx, "the closed tab", |rb| occurrences(rb, &new_child) == 0).await?;
                anyhow::ensure!(alive && after_close["closed"].as_array().is_some_and(|closed| closed.first().and_then(Value::as_str) == Some(new_child.as_str())), "Cmd-W hides it and remembers it: {after_close}");
                shot(window, cx, output.join("w10-closed.png")).await?;
                click_tab(window, cx, MAIN).await?;
                // Main focused with Related chats open: Cmd-W is consumed by
                // the workbench, not the right pane's surface or the window.
                let right_before = related(window, cx)?["rightActive"].clone();
                let alive = cmd_w(window, cx).await?;
                let right_after = related(window, cx)?["rightActive"].clone();
                anyhow::ensure!(alive && right_after == right_before && occurrences(&wb(window, cx)?, MAIN) == 1, "Cmd-W on Main closes nothing: {right_before} -> {right_after}");
                let ran = window.update(cx, |shell, _, cx| {
                    shell.fixture_workbench_action("Close other tabs (keeps Main)", cx)
                })?;
                anyhow::ensure!(ran, "Close other tabs is offered for Main's group");
                let kept = wait_for(window, cx, "Main kept", |rb| occurrences(rb, MAIN) == 1).await?;
                anyhow::ensure!(role_of(&kept, MAIN).as_deref() == Some("Main") && occurrences(&kept, CHILD_B) == 0, "close others keeps Main only in its group: {kept}");
                key(window, cx, "cmd-shift-t")?;
                let reopened = wait_for(window, cx, "a reopened tab", |rb| occurrences(rb, CHILD_B) == 1).await?;
                key(window, cx, "cmd-shift-t")?;
                let reopened = wait_for(window, cx, "the closed child", |rb| occurrences(rb, &new_child) == 1).await.unwrap_or(reopened);
                anyhow::ensure!(occurrences(&reopened, &new_child) == 1, "Cmd-Shift-T reopens: {reopened}");
                let draft = window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(&new_child, cx))?;
                anyhow::ensure!(draft.as_deref() == Some("keep this draft"), "the draft survives close/reopen: {draft:?}");
                let library = related(window, cx)?;
                anyhow::ensure!(related_ids(&library).iter().any(|id| id == &new_child) || library["related"].is_object(), "the new child is in the library: {library}");
                shot(window, cx, output.join("w11-reopened.png")).await?;
                progress(&output, &mut record, "reopened".into(), reopened.clone());

                // w12. Reload: a new Shell restores the engine's layout.
                let saved = wb(window, cx)?;
                window.update(cx, |_, window, _| window.remove_window())?;
                select(cx, None);
                let reload_state = state.clone();
                let reload_boot = boot.clone();
                window = cx.update(|cx| open(cx, reload_state, reload_boot, WIDTH, HEIGHT));
                settle(cx, 600).await;
                select(cx, Some(MAIN));
                let restored = wait_for(window, cx, "the restored layout", |rb| rb["bootOverlay"] == false && rb["loaded"] == true && rb["main"] == MAIN && all_panes(rb).len() == all_panes(&saved).len()).await?;
                anyhow::ensure!(restored["root"] == saved["root"] && restored["closed"] == saved["closed"], "layout survives a new Shell: {restored} vs {saved}");
                shot(window, cx, output.join("w12-reloaded.png")).await?;

                // w13. Switching main: each main has its own layout.
                select(cx, Some(MAIN2));
                let other = wait_for(window, cx, "Main 2's layout", |rb| rb["loaded"] == true && rb["main"] == MAIN2).await?;
                anyhow::ensure!(all_panes(&other).len() == 1 && tabs_of(&all_panes(&other)[0]) == [MAIN2], "another main starts alone: {other}");
                select(cx, Some(MAIN));
                let back = wait_for(window, cx, "Main's layout again", |rb| rb["loaded"] == true && rb["main"] == MAIN).await?;
                anyhow::ensure!(back["root"] == saved["root"], "switching back restores it: {back}");
                progress(&output, &mut record, "switched".into(), back.clone());

                // w14. Light theme capture.
                cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Light, cx));
                settle(cx, 300).await;
                shot(window, cx, output.join("w14-light.png")).await?;
                cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark, cx));
                settle(cx, 200).await;

                // w15. Narrow window: the split stays usable and the saved
                // wide ratio is not rewritten.
                let wide_revision = back["revision"].clone();
                window.update(cx, |_, window, _| window.remove_window())?;
                select(cx, None);
                let narrow_state = state.clone();
                let narrow_boot = boot.clone();
                window = cx.update(|cx| open(cx, narrow_state, narrow_boot, 980.0, 640.0));
                settle(cx, 600).await;
                select(cx, Some(MAIN));
                let narrow = wait_for(window, cx, "the narrow layout", |rb| rb["bootOverlay"] == false && rb["loaded"] == true && rb["main"] == MAIN).await?;
                settle(cx, 400).await;
                let narrow = wb(window, cx).unwrap_or(narrow);
                shot(window, cx, output.join("w15-narrow.png")).await?;
                anyhow::ensure!(narrow["revision"] == wide_revision, "a narrow window must not write the layout: {narrow}");
                for entry in narrow["paneBounds"].as_array().into_iter().flatten() {
                    let (_, _, w, h) = rect(&entry["bounds"]);
                    anyhow::ensure!(w >= 120.0 && h >= 160.0, "panes stay usable when narrow: {narrow}");
                }
                progress(&output, &mut record, "narrow".into(), narrow);

                // w15b. The frame row with both sidebars open and a long
                // focused title: the context truncates and both Git actions
                // stay wholly inside the row, left of the right sidebar —
                // labelled at a ~525 px row, icon-only (still present and
                // hittable) when narrower.
                let mut footer_checks = Vec::new();
                for (width, labelled) in [(1220.0_f32, true), (980.0_f32, false)] {
                    // A fresh window at this width (the right sidebar keeps a
                    // 300 px conversation column: ~528 px row at 1220,
                    // ~284 px at 980).
                    {
                        window.update(cx, |_, window, _| window.remove_window())?;
                        select(cx, None);
                        let footer_state = state.clone();
                        let footer_boot = boot.clone();
                        window = cx.update(|cx| open(cx, footer_state, footer_boot, width, HEIGHT));
                        settle(cx, 600).await;
                        select(cx, Some(MAIN));
                        wait_for(window, cx, "the layout in this window", |rb| rb["bootOverlay"] == false && rb["loaded"] == true && rb["main"] == MAIN).await?;
                    }
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_open_right_pane(cx);
                        shell.fixture_open_child_chats(cx);
                    })?;
                    for _ in 0..8 {
                        draw(window, cx)?;
                        settle(cx, 60).await;
                    }
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_open(ALONG, cx))?;
                    let opened = wait_for(window, cx, "the long-titled alongside chat focused", |rb| rb["focusedChat"] == ALONG).await?;
                    for _ in 0..4 {
                        draw(window, cx)?;
                        settle(cx, 60).await;
                    }
                    let state_now = frame(window, cx)?;
                    anyhow::ensure!(state_now["rightOpen"] == true && state_now["sidebarCollapsed"] == false, "both sidebars open: {state_now}");
                    let row = rect(&state_now["footer"]);
                    let right_limit = state_now["viewportWidth"].as_f64().unwrap_or(0.0) as f32 - state_now["rightNow"].as_f64().unwrap_or(0.0) as f32;
                    let buttons: Vec<(String, (f32, f32, f32, f32))> = state_now["footerButtons"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|button| (button["id"].as_str().unwrap_or_default().to_string(), rect(button)))
                        .collect();
                    for id in ["wb-footer-review", "wb-footer-commit"] {
                        let (_, b) = buttons.iter().find(|(name, _)| name == id).ok_or_else(|| anyhow::anyhow!("{id} is in the frame row at {width}: {state_now}"))?;
                        anyhow::ensure!(b.2 > 12.0 && b.0 >= row.0 - 0.5 && b.0 + b.2 <= row.0 + row.2 + 0.5 && b.0 + b.2 <= right_limit + 0.5, "{id} stays wholly inside the row, left of the right sidebar, at {width}: {b:?} in {row:?} (limit {right_limit}): {state_now}");
                        // Labelled at a ~525 px row; icon-only when narrower.
                        anyhow::ensure!(labelled == (b.2 > 60.0), "{id} is {} at a {} px row: {b:?}", if labelled { "labelled" } else { "compact" }, row.2);
                    }
                    shot(window, cx, output.join(format!("w15b-footer-{}.png", width as i32))).await?;
                    // A real pointer click on the row's Review changes opens
                    // Changes bound to the focused alongside chat's checkout
                    // (no Git mutation: it only opens the view).
                    let (_, review) = buttons
                        .iter()
                        .find(|(name, _)| name == "wb-footer-review")
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("Review changes button"))?;
                    click(window, cx, (review.0 + review.2 / 2.0, review.1 + review.3 / 2.0), MouseButton::Left).await?;
                    let mut reviewed = Value::Null;
                    for _ in 0..30 {
                        draw(window, cx)?;
                        reviewed = frame(window, cx)?;
                        if reviewed["rightDiffTarget"] == ALONG {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    anyhow::ensure!(
                        reviewed["rightActive"].as_str().is_some_and(|active| active.starts_with("Diff")) && reviewed["rightDiffTarget"] == ALONG,
                        "clicking Review changes opens Changes for the alongside chat's checkout at {width}: {reviewed}"
                    );
                    shot(window, cx, output.join(format!("w15b-review-{}.png", width as i32))).await?;
                    footer_checks.push(serde_json::json!({"windowWidth": width, "frame": state_now, "layout": opened, "afterReviewClick": reviewed}));
                    let closed = window.update(cx, |shell, _, cx| shell.fixture_workbench_close(ALONG, cx))?;
                    anyhow::ensure!(closed, "close the alongside tab");
                    wait_for(window, cx, "the alongside tab closed", |rb| occurrences(rb, ALONG) == 0).await?;
                }
                progress(&output, &mut record, "footerFit".into(), Value::Array(footer_checks));

                // w15c. The saved split in this 980 px window with both
                // sidebars open: the canvas keeps every group at its minimum
                // and scrolls instead of squeezing, rewriting or hiding
                // anything; the focused group is scrolled into view; a manual
                // scroll stays put; a drag over the sidebar drops nothing;
                // widening shows the exact saved layout again.
                for _ in 0..6 {
                    draw(window, cx)?;
                    settle(cx, 60).await;
                }
                let squeezed_frame = frame(window, cx)?;
                anyhow::ensure!(squeezed_frame["rightOpen"] == true && squeezed_frame["sidebarCollapsed"] == false, "both sidebars open: {squeezed_frame}");
                let narrow_base = wb(window, cx)?;
                anyhow::ensure!(all_panes(&narrow_base).len() >= 2, "a split layout to fit: {narrow_base}");
                let view = rect(&narrow_base["rootBounds"]);
                let minimum = (
                    narrow_base["canvasMinimum"]["w"].as_f64().unwrap_or(0.0) as f32,
                    narrow_base["canvasMinimum"]["h"].as_f64().unwrap_or(0.0) as f32,
                );
                anyhow::ensure!(minimum.0 > view.2 && narrow_base["canvasScroll"]["maxX"].as_f64().unwrap_or(0.0) > 0.5, "the saved layout needs more than the canvas, and the canvas scrolls: {narrow_base}");
                for entry in narrow_base["paneBounds"].as_array().into_iter().flatten() {
                    let (_, _, w, h) = rect(&entry["bounds"]);
                    anyhow::ensure!(w >= 320.0 - 0.5 && h >= 240.0 - 0.5, "every group keeps its 320x240 minimum: {entry} in {narrow_base}");
                }
                for (chat, width) in narrow_base["composerWidths"].as_object().into_iter().flatten() {
                    let visible = all_panes(&narrow_base).iter().any(|pane| pane["active"] == chat.as_str());
                    anyhow::ensure!(!visible || width.as_f64().unwrap_or(0.0) >= 280.0, "{chat}'s composer has room for its controls: {width} in {narrow_base}");
                }
                shot(window, cx, output.join("w15c-scrolled-canvas.png")).await?;
                // Focus into the canvas (a press on the visible group), then
                // Tab into another group: it scrolls into view.
                click(window, cx, (view.0 + view.2 / 2.0, view.1 + view.3 / 2.0), MouseButton::Left).await?;
                let start = wb(window, cx)?;
                let start_pane = start["focusedPane"].as_str().unwrap_or_default().to_string();
                let mut moved_to = None;
                for _ in 0..160 {
                    key(window, cx, "tab")?;
                    settle(cx, 40).await;
                    draw(window, cx)?;
                    let now = wb(window, cx)?;
                    let inside = window.update(cx, |shell, window, cx| shell.fixture_workbench_has_focus(window, cx))?;
                    if inside && now["focusedPane"] != start_pane.as_str() {
                        moved_to = now["focusedPane"].as_str().map(str::to_string);
                        break;
                    }
                }
                let target_pane = moved_to.ok_or_else(|| anyhow::anyhow!("Tab never reached another group"))?;
                let revealed = {
                    let target = target_pane.clone();
                    wait_for(window, cx, "the focused group scrolled into view", move |rb| {
                        let view = rect(&rb["rootBounds"]);
                        pane_rect(rb, &target).is_ok_and(|pane| {
                            pane.0 >= view.0 - 0.5 && pane.0 + pane.2.min(view.2) <= view.0 + view.2 + 0.5
                                && pane.1 >= view.1 - 0.5
                        })
                    })
                    .await?
                };
                shot(window, cx, output.join("w15c-revealed-focused-group.png")).await?;
                // A tab dragged out over the left sidebar previews and drops
                // nothing.
                let dragged = all_panes(&revealed).into_iter().find(|pane| pane["id"] == target_pane.as_str()).and_then(|pane| pane["active"].as_str().map(str::to_string)).ok_or_else(|| anyhow::anyhow!("the focused group's active tab"))?;
                let before_drag = wb(window, cx)?;
                let sidebar_point = (120.0, HEIGHT / 2.0);
                drag_hold(window, cx, tab_center(&before_drag, &dragged)?, sidebar_point).await?;
                let over_sidebar = wb(window, cx)?;
                anyhow::ensure!(over_sidebar["preview"].is_null(), "no drop preview over the sidebar: {over_sidebar}");
                release(window, cx, sidebar_point).await?;
                let after_sidebar = wb(window, cx)?;
                anyhow::ensure!(after_sidebar["root"] == before_drag["root"] && after_sidebar["revision"] == before_drag["revision"], "a release over the sidebar drops nothing: {after_sidebar}");
                // A manual scroll stays where the user put it.
                let before_scroll = revealed["canvasScroll"]["x"].as_f64().unwrap_or(0.0);
                let wheel_at = (view.0 + view.2 / 2.0, view.1 + 50.0);
                input(
                    window,
                    cx,
                    PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                        position: point(px(wheel_at.0), px(wheel_at.1)),
                        delta: gpui::ScrollDelta::Pixels(point(px(if before_scroll < -1.0 { 120.0 } else { -120.0 }), px(0.0))),
                        modifiers: Default::default(),
                        touch_phase: gpui::TouchPhase::Moved,
                    }),
                )?;
                draw(window, cx)?;
                let scrolled = wb(window, cx)?["canvasScroll"]["x"].as_f64().unwrap_or(0.0);
                anyhow::ensure!((scrolled - before_scroll).abs() > 1.0, "a wheel scroll moves the canvas: {before_scroll} -> {scrolled}");
                for _ in 0..6 {
                    draw(window, cx)?;
                    settle(cx, 60).await;
                }
                let held = wb(window, cx)?;
                anyhow::ensure!((held["canvasScroll"]["x"].as_f64().unwrap_or(0.0) - scrolled).abs() < 0.5, "the manual scroll is not snapped back: {scrolled} -> {held}");
                let before_widen = wb(window, cx)?;
                // Widen: a fresh full-size window shows the exact saved
                // layout, unscrolled, with no write from any of this.
                window.update(cx, |_, window, _| window.remove_window())?;
                select(cx, None);
                let wide_state = state.clone();
                let wide_boot = boot.clone();
                window = cx.update(|cx| open(cx, wide_state, wide_boot, WIDTH, HEIGHT));
                settle(cx, 600).await;
                select(cx, Some(MAIN));
                let widened = wait_for(window, cx, "the wide layout again", |rb| rb["bootOverlay"] == false && rb["loaded"] == true && rb["main"] == MAIN).await?;
                anyhow::ensure!(widened["root"] == before_widen["root"] && widened["revision"] == before_widen["revision"], "widening restores the exact saved axes, ratios and groups with no write: {before_widen} -> {widened}");
                anyhow::ensure!(narrow_base["root"]["axis"] == widened["root"]["axis"] && narrow_base["root"]["ratio"] == widened["root"]["ratio"], "the saved axis and ratio never changed: {narrow_base} -> {widened}");
                anyhow::ensure!(widened["canvasScroll"]["maxX"].as_f64().unwrap_or(1.0) < 0.5, "the wide canvas fits without scrolling: {widened}");
                progress(
                    &output,
                    &mut record,
                    "narrowCanvas".into(),
                    serde_json::json!({"narrow": narrow_base, "revealed": revealed, "manualScroll": held, "overSidebar": after_sidebar, "widened": widened}),
                );
                window.update(cx, |_, window, _| window.remove_window())?;
                select(cx, None);
                let final_state = state.clone();
                let final_boot = boot.clone();
                window = cx.update(|cx| open(cx, final_state, final_boot, WIDTH, HEIGHT));
                settle(cx, 600).await;
                select(cx, Some(MAIN));
                wait_for(window, cx, "the final layout", |rb| rb["bootOverlay"] == false && rb["loaded"] == true && rb["main"] == MAIN).await?;

                // w16. An empty secondary group: a split focuses it; Cmd-W
                // closes that group (not the window).
                let base = wb(window, cx)?;
                let main_pane = pane_of(&base, MAIN).unwrap()["id"].as_str().unwrap().to_string();
                let panes_before = all_panes(&base).len();
                // The strip's split control (here: below, where there is room).
                window.update(cx, |shell, _, cx| shell.fixture_workbench_split_empty(&main_pane, cx))?;
                let empty = wait_for(window, cx, "an empty group", |rb| {
                    all_panes(rb).iter().any(|pane| tabs_of(pane).is_empty())
                })
                .await?;
                shot(window, cx, output.join("w16-empty-group.png")).await?;
                let empty_pane = all_panes(&empty).into_iter().find(|pane| tabs_of(pane).is_empty()).unwrap()["id"].as_str().unwrap().to_string();
                anyhow::ensure!(empty["focusedPane"] == empty_pane.as_str(), "the new empty group is focused: {empty}");
                let (ex, ey, ew, eh) = pane_rect(&empty, &empty_pane)?;
                // Another group takes focus (a click on Main's tab) ...
                click_tab(window, cx, MAIN).await?;
                wait_for(window, cx, "Main's group focused", |rb| rb["focusedPane"] == main_pane.as_str()).await?;
                // ... and a pointer press in the empty group focuses it again
                // (`focusPane`: an empty group has no tab to focus).
                hover_at(window, cx, (ex + ew / 2.0, ey + eh / 2.0 + 60.0), None)?;
                press(window, cx, (ex + 12.0, ey + eh - 12.0), MouseButton::Left, true, 1)?;
                press(window, cx, (ex + 12.0, ey + eh - 12.0), MouseButton::Left, false, 1)?;
                settle(cx, 200).await;
                let pointer_focus = wait_for(window, cx, "the empty group focused by pointer", |rb| rb["focusedPane"] == empty_pane.as_str()).await?;
                // Keyboard: from Main's composer, Tab moves into the empty
                // group (its next-step buttons) and focuses it too.
                let focus_main = window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(MAIN, window, cx))?;
                anyhow::ensure!(focus_main, "focus Main's composer");
                draw(window, cx)?;
                wait_for(window, cx, "Main's group focused again", |rb| rb["focusedPane"] == main_pane.as_str()).await?;
                let mut tabbed_into_empty = None;
                for presses in 1..=160 {
                    key(window, cx, "tab")?;
                    settle(cx, 40).await;
                    draw(window, cx)?;
                    let inside = window.update(cx, |shell, window, cx| shell.fixture_workbench_has_focus(window, cx))?;
                    if inside && wb(window, cx)?["focusedPane"] == empty_pane.as_str() {
                        tabbed_into_empty = Some(presses);
                        break;
                    }
                }
                let tabbed_into_empty = tabbed_into_empty.ok_or_else(|| anyhow::anyhow!("Tab never focused the empty group"))?;
                // Cmd-W now targets the empty group the keyboard is in.
                let alive = cmd_w(window, cx).await?;
                let closed_empty = wait_for(window, cx, "the empty group closed", |rb| {
                    !all_panes(rb).iter().any(|pane| pane["id"] == empty_pane.as_str())
                })
                .await?;
                anyhow::ensure!(alive && all_panes(&closed_empty).len() < all_panes(&empty).len(), "Cmd-W closes the empty group only: {closed_empty}");
                anyhow::ensure!(occurrences(&closed_empty, MAIN) == 1, "Main stays: {closed_empty}");
                // New child (palette) lands in the focused empty group, not
                // in the group that was focused before.
                window.update(cx, |shell, _, cx| shell.fixture_workbench_split_empty(&main_pane, cx))?;
                let second_empty = wait_for(window, cx, "a second empty group", |rb| {
                    all_panes(rb).iter().any(|pane| tabs_of(pane).is_empty())
                })
                .await?;
                let second_pane = all_panes(&second_empty).into_iter().find(|pane| tabs_of(pane).is_empty()).unwrap()["id"].as_str().unwrap().to_string();
                click_tab(window, cx, MAIN).await?;
                wait_for(window, cx, "Main's group focused", |rb| rb["focusedPane"] == main_pane.as_str()).await?;
                let (sx, sy, sw, sh) = pane_rect(&second_empty, &second_pane)?;
                hover_at(window, cx, (sx + sw / 2.0, sy + sh / 2.0 + 60.0), None)?;
                press(window, cx, (sx + 12.0, sy + sh - 12.0), MouseButton::Left, true, 1)?;
                press(window, cx, (sx + 12.0, sy + sh - 12.0), MouseButton::Left, false, 1)?;
                settle(cx, 200).await;
                wait_for(window, cx, "the second empty group focused", |rb| rb["focusedPane"] == second_pane.as_str()).await?;
                let started = window.update(cx, |shell, _, cx| shell.fixture_workbench_action("New child chat", cx))?;
                anyhow::ensure!(started, "the palette offers New child chat");
                let child_landed = wait_for(window, cx, "the new child in the focused empty group", |rb| {
                    all_panes(rb).iter().any(|pane| pane["id"] == second_pane.as_str() && tabs_of(pane).len() == 1)
                })
                .await?;
                let new_in_empty = all_panes(&child_landed).into_iter().find(|pane| pane["id"] == second_pane.as_str()).map(|pane| tabs_of(&pane)).unwrap_or_default();
                anyhow::ensure!(pane_of(&child_landed, MAIN).is_some_and(|pane| tabs_of(&pane).iter().all(|id| !new_in_empty.contains(id))), "the new child is not in Main's group: {child_landed}");
                shot(window, cx, output.join("w16-new-child-in-focused-empty-group.png")).await?;
                // Close it again (its group collapses with its last tab).
                let new_child_id = new_in_empty[0].clone();
                let closed_new = window.update(cx, |shell, _, cx| shell.fixture_workbench_close(&new_child_id, cx))?;
                anyhow::ensure!(closed_new, "close the new child tab");
                wait_for(window, cx, "the new child's group collapsed", |rb| occurrences(rb, &new_child_id) == 0).await?;
                progress(&output, &mut record,
                    "emptyGroupFocus".into(),
                    serde_json::json!({"pointer": pointer_focus, "tabPresses": tabbed_into_empty, "closed": closed_empty, "newChildLanded": child_landed}),
                );
                let _ = panes_before;

                // w16b. Balance panes (failure case 27). Two groups stacked
                // on the left of one: one real click on the frame row's
                // button sets the root to 2/3 and the stack to 1/2, so the
                // three groups get equal area (tabs in a group count once:
                // Main's group holds two).
                let single = wb(window, cx)?;
                let main_group = pane_of(&single, MAIN).ok_or_else(|| anyhow::anyhow!("Main's group: {single}"))?["id"].as_str().unwrap().to_string();
                let right_group = all_panes(&single).into_iter().find(|pane| pane["id"] != main_group.as_str()).ok_or_else(|| anyhow::anyhow!("a second group: {single}"))?["id"].as_str().unwrap().to_string();
                anyhow::ensure!(single["root"]["kind"] == "split" && single["root"]["axis"] == "horizontal" && all_panes(&single).len() == 2, "two side-by-side groups to start: {single}");
                anyhow::ensure!(pane_of(&single, MAIN).is_some_and(|pane| tabs_of(&pane).len() >= 2), "Main's group holds several tabs (they count as one): {single}");
                window.update(cx, |shell, _, cx| shell.fixture_workbench_split(&main_group, "bottom", cx))?;
                let stacked = wait_for(window, cx, "two groups stacked left of one", |rb| all_panes(rb).len() == 3 && rb["root"]["first"]["kind"] == "split" && rb["root"]["first"]["axis"] == "vertical").await?;
                // Unbalance the root first, by its divider.
                let (sx, sy, sw, sh) = split_rect(&stacked)?;
                let ratio = stacked["root"]["ratio"].as_f64().unwrap_or(0.5) as f32;
                let from = (sx + sw * ratio, sy + sh / 2.0);
                let to = (from.0 + sw * 0.1, from.1);
                drag_hold(window, cx, from, to).await?;
                release(window, cx, to).await?;
                let unbalanced = wait_for(window, cx, "an unbalanced root", |rb| (rb["root"]["ratio"].as_f64().unwrap_or(0.5) - 2.0 / 3.0).abs() > 0.02).await?;
                let before_balance = unbalanced["revision"].as_u64().unwrap_or(0);
                click_balance(window, cx).await?;
                let balanced = wait_for(window, cx, "balanced 2-over-1", |rb| {
                    (rb["root"]["ratio"].as_f64().unwrap_or(0.0) - 2.0 / 3.0).abs() < 0.002
                        && (rb["root"]["first"]["ratio"].as_f64().unwrap_or(0.0) - 0.5).abs() < 0.002
                })
                .await?;
                anyhow::ensure!(balanced["revision"].as_u64() == Some(before_balance + 1), "one shared write: {unbalanced} -> {balanced}");
                anyhow::ensure!(balanced["root"]["axis"] == unbalanced["root"]["axis"] && balanced["root"]["first"]["axis"] == unbalanced["root"]["first"]["axis"], "axes kept: {balanced}");
                for (before, after) in all_panes(&unbalanced).iter().zip(all_panes(&balanced).iter()) {
                    anyhow::ensure!(before["id"] == after["id"] && before["tabs"] == after["tabs"] && before["active"] == after["active"], "order, IDs, tabs and active chats kept: {before} -> {after}");
                }
                anyhow::ensure!(balanced["focusedPane"] == unbalanced["focusedPane"] && balanced["closed"] == unbalanced["closed"], "focus and closed history kept: {balanced}");
                let areas = pane_areas(&balanced);
                let (low, high) = areas.iter().fold((f32::MAX, 0.0_f32), |(lo, hi), a| (lo.min(*a), hi.max(*a)));
                anyhow::ensure!(areas.len() == 3 && high / low <= 1.02, "three equal group areas (within the 1 px seams): {areas:?}");
                shot(window, cx, output.join("w16b-balanced-2-over-1.png")).await?;
                // A repeat changes nothing.
                click_balance(window, cx).await?;
                settle(cx, 300).await;
                let repeated = wb(window, cx)?;
                anyhow::ensure!(repeated["revision"] == balanced["revision"] && repeated["root"] == balanced["root"], "Balance on a balanced layout is a no-op: {repeated}");

                // w16c. Left anchor (failure case 28): with the right sidebar
                // closed the balanced split fits; focus its right group, then
                // open the sidebar: the canvas stays at its left (the right
                // group is the one covered). An explicit focus still reveals.
                set_right_sidebar(window, cx, false).await?;
                let wide_fit = wb(window, cx)?;
                anyhow::ensure!(wide_fit["canvasScroll"]["maxX"].as_f64().unwrap_or(1.0) < 0.5, "the split fits without the sidebar: {wide_fit}");
                let right_active = all_panes(&wide_fit).into_iter().find(|pane| pane["id"] == right_group.as_str()).and_then(|pane| pane["active"].as_str().map(str::to_string)).ok_or_else(|| anyhow::anyhow!("the right group has an active tab: {wide_fit}"))?;
                click_tab(window, cx, &right_active).await?;
                {
                    let group = right_group.clone();
                    wait_for(window, cx, "the right group focused", move |rb| rb["focusedPane"] == group.as_str()).await?;
                }
                set_right_sidebar(window, cx, true).await?;
                let covered = wb(window, cx)?;
                anyhow::ensure!(covered["canvasScroll"]["maxX"].as_f64().unwrap_or(0.0) > 0.5, "now the split overflows: {covered}");
                anyhow::ensure!(covered["canvasScroll"]["x"].as_f64().unwrap_or(-1.0).abs() < 0.5, "the canvas stays anchored at its left: {covered}");
                anyhow::ensure!(covered["focusedPane"] == right_group.as_str(), "focus did not move: {covered}");
                let view = rect(&covered["rootBounds"]);
                let left_pane = pane_rect(&covered, &main_group)?;
                let right_pane = pane_rect(&covered, &right_group)?;
                anyhow::ensure!(left_pane.0 >= view.0 - 0.5 && left_pane.0 < view.0 + 0.5, "the left group stays in view at the left: {covered}");
                anyhow::ensure!(right_pane.0 + right_pane.2 > view.0 + view.2 + 0.5, "the rightmost group is the covered one: {covered}");
                shot(window, cx, output.join("w16c-left-anchored.png")).await?;
                // Explicit focus: the left group, then the right one again
                // (a click on its partly visible tab) reveals it.
                click_tab(window, cx, MAIN).await?;
                {
                    let group = main_group.clone();
                    wait_for(window, cx, "Main's group focused", move |rb| rb["focusedPane"] == group.as_str()).await?;
                }
                click_tab(window, cx, &right_active).await?;
                let revealed_right = {
                    let group = right_group.clone();
                    wait_for(window, cx, "an explicit focus reveals the right group", move |rb| {
                        let view = rect(&rb["rootBounds"]);
                        rb["focusedPane"] == group.as_str()
                            && rb["canvasScroll"]["x"].as_f64().unwrap_or(0.0) < -0.5
                            && pane_rect(rb, &group).is_ok_and(|pane| pane.0 >= view.0 - 0.5 && pane.0 + pane.2.min(view.2) <= view.0 + view.2 + 0.5)
                    })
                    .await?
                };

                // Six groups, mixed axes: the right group split down, each
                // left group split right (with the sidebar closed, for room),
                // the root unbalanced, then Balance: six equal areas.
                set_right_sidebar(window, cx, false).await?;
                let three = wb(window, cx)?;
                let lower_left = all_panes(&three).into_iter().map(|pane| pane["id"].as_str().unwrap_or_default().to_string()).find(|id| *id != main_group && *id != right_group).ok_or_else(|| anyhow::anyhow!("the stacked empty group: {three}"))?;
                for (pane, edge) in [(right_group.as_str(), "bottom"), (main_group.as_str(), "right"), (lower_left.as_str(), "right")] {
                    let count = all_panes(&wb(window, cx)?).len();
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_split(pane, edge, cx))?;
                    wait_for(window, cx, "one more group", move |rb| all_panes(rb).len() == count + 1).await?;
                    settle_frames(window, cx).await?;
                }
                let six = wb(window, cx)?;
                anyhow::ensure!(all_panes(&six).len() == 6, "six groups: {six}");
                let (sx, sy, sw, sh) = split_rect(&six)?;
                let ratio = six["root"]["ratio"].as_f64().unwrap_or(0.5) as f32;
                let from = (sx + sw * ratio, sy + sh / 2.0);
                let to = (from.0 - sw * 0.08, from.1);
                drag_hold(window, cx, from, to).await?;
                release(window, cx, to).await?;
                let six_unbalanced = wait_for(window, cx, "an unbalanced six-group root", |rb| (rb["root"]["ratio"].as_f64().unwrap_or(0.0) - 2.0 / 3.0).abs() > 0.02).await?;
                click_balance(window, cx).await?;
                let six_balanced = wait_for(window, cx, "six balanced groups", |rb| (rb["root"]["ratio"].as_f64().unwrap_or(0.0) - 2.0 / 3.0).abs() < 0.002).await?;
                anyhow::ensure!(six_balanced["revision"].as_u64() == six_unbalanced["revision"].as_u64().map(|r| r + 1), "one shared write: {six_balanced}");
                let areas = pane_areas(&six_balanced);
                let (low, high) = areas.iter().fold((f32::MAX, 0.0_f32), |(lo, hi), a| (lo.min(*a), hi.max(*a)));
                anyhow::ensure!(areas.len() == 6 && high / low <= 1.03, "six equal group areas (within the seams): {areas:?}");
                shot(window, cx, output.join("w16b-balanced-six.png")).await?;
                // Persistence: a new Shell shows the same balanced tree.
                window.update(cx, |_, window, _| window.remove_window())?;
                select(cx, None);
                let balance_state = state.clone();
                let balance_boot = boot.clone();
                window = cx.update(|cx| open(cx, balance_state, balance_boot, WIDTH, HEIGHT));
                settle(cx, 600).await;
                select(cx, Some(MAIN));
                let reloaded_balance = wait_for(window, cx, "the balanced layout after a new Shell", |rb| rb["bootOverlay"] == false && rb["loaded"] == true && rb["main"] == MAIN).await?;
                anyhow::ensure!(reloaded_balance["root"] == six_balanced["root"] && reloaded_balance["revision"] == six_balanced["revision"], "Balance persists: {six_balanced} -> {reloaded_balance}");
                // No provider ran for any of this.
                for chat in [MAIN, CHILD_A, CHILD_B] {
                    let (indicator, _) = cx.update(|cx| state.read(cx).fixture_session_indicator(chat));
                    anyhow::ensure!(!matches!(indicator.as_str(), "Working" | "AwaitingApproval" | "AwaitingInput"), "{chat} did not start a run: {indicator}");
                }
                progress(
                    &output,
                    &mut record,
                    "balancePanes".into(),
                    serde_json::json!({
                        "twoOverOne": balanced, "repeat": repeated, "leftAnchored": covered,
                        "explicitReveal": revealed_right, "sixUnbalanced": six_unbalanced,
                        "six": six_balanced, "afterNewShell": reloaded_balance,
                    }),
                );
                // Back to the two groups the next steps start from: close the
                // four empty groups (Main's and Child A's tabs stay).
                let empties: Vec<String> = all_panes(&reloaded_balance)
                    .into_iter()
                    .filter(|pane| tabs_of(pane).is_empty())
                    .filter_map(|pane| pane["id"].as_str().map(str::to_string))
                    .collect();
                for pane in &empties {
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_close_pane(pane, cx))?;
                    let gone = pane.clone();
                    wait_for(window, cx, "an empty group closed", move |rb| !all_panes(rb).iter().any(|entry| entry["id"] == gone.as_str())).await?;
                }
                anyhow::ensure!(all_panes(&wb(window, cx)?).len() == 2, "two groups again");

                // w17. A late response after a profile switch changes nothing;
                // the other profile cannot write; no profile writes nothing.
                let before_switch = wb(window, cx)?;
                let focus_target = all_panes(&before_switch)
                    .into_iter()
                    .flat_map(|pane| tabs_of(&pane))
                    .find(|id| before_switch["focusedChat"].as_str() != Some(id.as_str()))
                    .ok_or_else(|| anyhow::anyhow!("need a second tab: {before_switch}"))?;
                window.update(cx, |shell, _, cx| shell.fixture_workbench_hold_responses(true, cx))?;
                click_tab(window, cx, &focus_target).await?;
                settle(cx, 300).await;
                let parked = wb(window, cx)?;
                anyhow::ensure!(parked["pendingMutations"].as_u64().unwrap_or(0) >= 1, "a mutation is in flight: {parked}");
                window.update(cx, |shell, _, cx| shell.fixture_workbench_engine_switch(handle_b.clone(), cx))?;
                let in_b = wait_for(window, cx, "profile B's layout", |rb| rb["loaded"] == true && rb["main"] == MAIN).await?;
                let delivered = window.update(cx, |shell, _, cx| shell.fixture_workbench_hold_responses(false, cx))?;
                settle(cx, 200).await;
                let after_late = wb(window, cx)?;
                anyhow::ensure!(delivered >= 1, "the parked response was delivered");
                anyhow::ensure!(after_late["root"] == in_b["root"] && after_late["revision"] == in_b["revision"] && after_late["notice"].is_null() && after_late["pendingMutations"] == 0, "a late response must not touch the new profile's workbench: {in_b} -> {after_late}");
                // Profile B: the engine refuses its writes; the layout stays.
                let b_target = all_panes(&after_late)
                    .into_iter()
                    .flat_map(|pane| tabs_of(&pane))
                    .find(|id| after_late["focusedChat"].as_str() != Some(id.as_str()));
                if let Some(target) = b_target {
                    click_tab(window, cx, &target).await?;
                    settle(cx, 600).await;
                    let refused = wb(window, cx)?;
                    anyhow::ensure!(refused["revision"] == after_late["revision"] && refused["root"] == after_late["root"] && refused["notice"].is_string(), "profile B cannot write A's layout: {refused}");
                }
                shot(window, cx, output.join("w17-profile-b.png")).await?;
                // No profile id: Main renders; layout actions refuse.
                window.update(cx, |shell, _, cx| shell.fixture_workbench_engine_switch(handle_none.clone(), cx))?;
                settle(cx, 800).await;
                let unnamed = wb(window, cx)?;
                let plus = plus_center(&unnamed, unnamed["focusedPane"].as_str().unwrap_or("main-pane"))?;
                click(window, cx, plus, MouseButton::Left).await?;
                let notice = related(window, cx)?["notice"].clone();
                let unnamed_after = wb(window, cx)?;
                anyhow::ensure!(occurrences(&unnamed_after, MAIN) == 1 && unnamed_after["creatingChild"] == false && unnamed_after["revision"] == unnamed["revision"], "no profile, no writes: {unnamed_after}");
                anyhow::ensure!(notice.as_str().is_some_and(|text| text.contains("profile")), "the refusal says why: {notice}");
                // Back to profile A: its layout (including the late command it
                // committed) and writes work again.
                window.update(cx, |shell, _, cx| shell.fixture_workbench_engine_switch(handle_a.clone(), cx))?;
                let back_in_a = wait_for(window, cx, "profile A again", |rb| rb["loaded"] == true && rb["main"] == MAIN).await?;
                anyhow::ensure!(back_in_a["available"] == true, "profile A is available again: {back_in_a}");
                progress(&output, &mut record, "profiles".into(), serde_json::json!({"late": after_late, "unnamed": unnamed_after, "backInA": back_in_a}));
                // The refused write's notice was part of the proof above; clear
                // it with its own Dismiss control before the visual steps.
                let noticed = wb(window, cx)?;
                if noticed["notice"].is_string() {
                    let dismiss = rect(&noticed["noticeDismiss"]);
                    anyhow::ensure!(dismiss.2 > 0.0, "the notice offers Dismiss: {noticed}");
                    click(window, cx, (dismiss.0 + dismiss.2 / 2.0, dismiss.1 + dismiss.3 / 2.0), MouseButton::Left).await?;
                    wait_for(window, cx, "the notice dismissed", |rb| rb["notice"].is_null()).await?;
                }

                // w18. Side tabs: New side chat opens as a Side tab with its
                // exact source; End leaves read-only history (the tab stays);
                // Close then reopen from Main's Related library.
                window.update(cx, |shell, _, cx| {
                    shell.fixture_open_right_pane(cx);
                    shell.fixture_open_side_chats(cx);
                })?;
                settle(cx, 600).await;
                window.update(cx, |shell, _, cx| shell.fixture_side_chat("new", "", cx))?;
                let mut side_id = None;
                for _ in 0..80 {
                    merge_engine_chats(&reader, &state, cx).await?;
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("refresh", "", cx))?;
                    settle(cx, 150).await;
                    let rb = wb(window, cx)?;
                    side_id = all_panes(&rb).into_iter().flat_map(|pane| {
                        pane["tabs"].as_array().cloned().unwrap_or_default()
                    }).find(|tab| tab["role"] == "Side").and_then(|tab| tab["chatId"].as_str().map(str::to_string));
                    if side_id.is_some() {
                        break;
                    }
                }
                let side_id = side_id.ok_or_else(|| anyhow::anyhow!("New side chat never became a Side tab"))?;
                let side_tab = wb(window, cx)?;
                let tab = all_panes(&side_tab).into_iter().flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default()).find(|tab| tab["chatId"] == side_id.as_str()).unwrap();
                anyhow::ensure!(tab["sideParentId"] == MAIN && tab["parent"] == MAIN, "the Side tab keeps its exact source: {tab}");
                shot(window, cx, output.join("w18-side-tab.png")).await?;
                // End by its own button (a real click: focus goes to that
                // button, which then disappears with the composer).
                let end_at = window
                    .update(cx, |shell, _, cx| shell.fixture_workbench_end_bounds(&side_id, cx))?
                    .ok_or_else(|| anyhow::anyhow!("the Side tab shows its End control"))?;
                click(window, cx, end_at, MouseButton::Left).await?;
                let mut ended = false;
                for _ in 0..60 {
                    merge_engine_chats(&reader, &state, cx).await?;
                    settle(cx, 150).await;
                    if window.update(cx, |shell, _, cx| shell.fixture_workbench_side_active(&side_id, cx))? == Some(false) {
                        ended = true;
                        break;
                    }
                }
                anyhow::ensure!(ended, "End leaves the side chat ended");
                // Immediately, with no click in between: the model shortcut
                // must not reach Main (the ended tab has no composer), and
                // the group and frame row stay on the side chat.
                activate(window, cx)?;
                key(window, cx, "cmd-/")?;
                settle(cx, 300).await;
                draw(window, cx)?;
                let after_shortcut = wb(window, cx)?;
                let main_picker = window.update(cx, |shell, _, cx| shell.fixture_workbench_model(MAIN, cx))?;
                anyhow::ensure!(main_picker.as_ref().is_some_and(|(_, open)| !*open), "Cmd-/ after End does not open Main's model picker: {main_picker:?}");
                anyhow::ensure!(!window.update(cx, |shell, _, cx| shell.fixture_overlay_owns_keyboard(cx))?, "no picker opened anywhere");
                anyhow::ensure!(after_shortcut["focusedChat"] == side_id.as_str() && after_shortcut["footer"]["chat"] == side_id.as_str(), "the side chat's group and frame row stay focused: {after_shortcut}");
                let chip = window.update(cx, |shell, window, _| shell.fixture_workbench_focused_tab(window))?;
                anyhow::ensure!(chip.as_deref() == Some(side_id.as_str()), "keyboard focus stays on the ended tab's chip: {chip:?}");
                progress(&output, &mut record, "endThenModelShortcut".into(), serde_json::json!({"layout": after_shortcut, "focusedTab": chip}));
                let after_end = wb(window, cx)?;
                anyhow::ensure!(occurrences(&after_end, &side_id) == 1, "End is not Close: the tab stays: {after_end}");
                shot(window, cx, output.join("w19-side-ended.png")).await?;
                click_tab(window, cx, &side_id).await?;
                let alive = cmd_w(window, cx).await?;
                let side_closed = wait_for(window, cx, "the Side tab closed", |rb| occurrences(rb, &side_id) == 0).await?;
                anyhow::ensure!(alive, "closing a Side tab keeps the window");
                window.update(cx, |shell, _, cx| {
                    shell.fixture_open_right_pane(cx);
                    shell.fixture_open_child_chats(cx);
                })?;
                settle(cx, 600).await;
                let pane_x = WIDTH - PANE_WIDTH / 2.0;
                let rows = related_rows(window, cx, pane_x)?;
                click(window, cx, (pane_x, row_y(&rows, &side_id)?), MouseButton::Left).await?;
                let reopened_side = wait_for(window, cx, "the Side tab reopened", |rb| occurrences(rb, &side_id) == 1).await?;
                let tab = all_panes(&reopened_side).into_iter().flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default()).find(|tab| tab["chatId"] == side_id.as_str()).unwrap();
                anyhow::ensure!(tab["sideParentId"] == MAIN && tab["role"] == "Side", "reopened with its exact source: {tab}");
                shot(window, cx, output.join("w20-side-reopened.png")).await?;
                progress(&output, &mut record, "side".into(), serde_json::json!({"closed": side_closed, "reopened": reopened_side}));

                // w20b. Two ended side chats from different parents (Main's,
                // and one forked from Child A), side by side: each tab has
                // its own read-only history under its exact source parent,
                // both visible at once, and neither starts a provider again;
                // neither tab offers a composer, picker or Stop.
                let (child_side, child_lease) = async {
                    let created = reader
                        .call(
                            luna_rpc::methods::CREATE_SIDE_CHAT,
                            serde_json::json!({"parentId": CHILD_A, "requestId": uuid::Uuid::new_v4().to_string(), "forceSnapshot": false}),
                        )
                        .await?;
                    let id = created["meta"]["id"].as_str().unwrap_or_default().to_string();
                    let lease = created["lease"].clone();
                    reader
                        .call(luna_rpc::methods::UNLOAD_SIDE_CHAT, serde_json::json!({"id": id, "lease": lease}))
                        .await?;
                    anyhow::Ok((id, lease))
                }
                .await?;
                anyhow::ensure!(!child_side.is_empty(), "a side chat forked from Child A");
                let mut child_side_ended = false;
                for _ in 0..60 {
                    merge_engine_chats(&reader, &state, cx).await?;
                    settle(cx, 100).await;
                    if window.update(cx, |shell, _, cx| shell.fixture_workbench_side_active(&child_side, cx))? == Some(false) {
                        child_side_ended = true;
                        break;
                    }
                }
                anyhow::ensure!(child_side_ended, "Child A's side chat is ended before it opens");
                click_tab(window, cx, &side_id).await?;
                let opened = window.update(cx, |shell, _, cx| shell.fixture_workbench_open_side_split(&child_side, CHILD_A, cx))?;
                anyhow::ensure!(opened, "open Child A's side chat in a split");
                let both_sides = wait_for(window, cx, "both ended side histories visible", |rb| {
                    pane_of(rb, &side_id).is_some_and(|pane| pane["active"] == side_id.as_str())
                        && pane_of(rb, &child_side).is_some_and(|pane| pane["active"] == child_side.as_str())
                })
                .await?;
                let child_tab = all_panes(&both_sides).into_iter().flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default()).find(|tab| tab["chatId"] == child_side.as_str()).unwrap();
                anyhow::ensure!(child_tab["sideParentId"] == CHILD_A && child_tab["role"] == "Side", "Child A's side tab keeps its exact source: {child_tab}");
                wait_for(window, cx, "every active tab fits beside two histories", active_tabs_fit).await?;
                let mut histories = (Value::Null, Value::Null);
                for _ in 0..60 {
                    draw(window, cx)?;
                    settle(cx, 100).await;
                    histories = window.update(cx, |shell, _, cx| {
                        (shell.fixture_workbench_history(&side_id, cx), shell.fixture_workbench_history(&child_side, cx))
                    })?;
                    if histories.0["page"] == side_id.as_str() && histories.1["page"] == child_side.as_str() {
                        break;
                    }
                }
                anyhow::ensure!(histories.0["page"] == side_id.as_str() && histories.0["fixed"] == side_id.as_str() && histories.0["parent"] == MAIN, "Main's side history shows in its own panel under Main: {histories:?}");
                anyhow::ensure!(histories.1["page"] == child_side.as_str() && histories.1["fixed"] == child_side.as_str() && histories.1["parent"] == CHILD_A, "Child A's side history shows in its own panel under Child A: {histories:?}");
                shot(window, cx, output.join("w20b-two-side-histories.png")).await?;
                // Read-only and inert: still ended after a few seconds of
                // display, no composer to type into, no picker to open.
                settle(cx, 1500).await;
                merge_engine_chats(&reader, &state, cx).await?;
                let still_ended = window.update(cx, |shell, _, cx| {
                    (shell.fixture_workbench_side_active(&side_id, cx), shell.fixture_workbench_side_active(&child_side, cx))
                })?;
                anyhow::ensure!(still_ended == (Some(false), Some(false)), "showing history restarts neither side chat: {still_ended:?}");
                let composers = window.update(cx, |shell, _, cx| {
                    (shell.fixture_workbench_tab_has_composer(&side_id, cx), shell.fixture_workbench_tab_has_composer(&child_side, cx))
                })?;
                anyhow::ensure!(composers == (false, false), "an ended side tab renders no composer: {composers:?}");
                click_tab(window, cx, &child_side).await?;
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_keyboard_chat(window, cx))?.is_none(), "keys on a history tab reach no composer");
                key(window, cx, "cmd-/")?;
                settle(cx, 300).await;
                anyhow::ensure!(!window.update(cx, |shell, _, cx| shell.fixture_overlay_owns_keyboard(cx))?, "the model shortcut opens no hidden picker on a history tab");
                let main_model = window.update(cx, |shell, _, cx| shell.fixture_workbench_model(MAIN, cx))?;
                anyhow::ensure!(main_model.as_ref().is_some_and(|(_, open)| !*open), "nor does it fall back to Main's picker: {main_model:?}");
                progress(&output, &mut record,
                    "sideHistories".into(),
                    serde_json::json!({"main": histories.0, "child": histories.1, "childLease": child_lease, "layout": both_sides}),
                );

                // w21-w30. The surrounding frame: one chrome surface around
                // the inset canvas in every sidebar combination, while
                // reflowing, after a resize, and under each right surface.
                window.update(cx, |shell, window, cx| shell.fixture_open_surface("related", window, cx))?;
                for _ in 0..8 {
                    draw(window, cx)?;
                    settle(cx, 60).await;
                }
                let both = frame(window, cx)?;
                anyhow::ensure!(both["rightOpen"] == true && both["sidebarNow"].as_f64().unwrap_or(0.0) > 100.0, "both sidebars open: {both}");
                assert_frame(&both, "both open")?;
                shot(window, cx, output.join("w21-frame-both-open.png")).await?;
                toggle(window, cx, false).await?;
                let left_collapsed = frame(window, cx)?;
                assert_frame(&left_collapsed, "left collapsed")?;
                shot(window, cx, output.join("w22-frame-left-collapsed.png")).await?;
                toggle(window, cx, true).await?;
                let both_collapsed = frame(window, cx)?;
                anyhow::ensure!(both_collapsed["rightOpen"] == false, "right collapsed: {both_collapsed}");
                assert_frame(&both_collapsed, "both collapsed")?;
                shot(window, cx, output.join("w23-frame-both-collapsed.png")).await?;
                toggle(window, cx, false).await?;
                let right_collapsed = frame(window, cx)?;
                assert_frame(&right_collapsed, "right collapsed")?;
                shot(window, cx, output.join("w24-frame-right-collapsed.png")).await?;
                // Mid-reflow, deterministically: the right sidebar opening,
                // with the animation clock pinned halfway through its
                // transition, so the geometry, the readback and the captured
                // pixels all come from that one instant (wall-clock sleeps
                // and extra draws let it finish). The app's reduced-motion
                // flag mirrors the system setting; the animated variant runs
                // with it off, the no-animation variant with it on, and the
                // original value is restored after both.
                let system_reduced = cx.update(|cx| motion::reduced_motion(cx));
                cx.update(|cx| motion::set_reduced_motion(cx, false));
                draw(window, cx)?;
                window.update(cx, |shell, _, cx| shell.fixture_toggle_sidebar(true, cx))?;
                let pinned_ms = window
                    .update(cx, |shell, _, cx| shell.fixture_pin_right_motion(0.5, cx))?
                    .ok_or_else(|| anyhow::anyhow!("opening the right sidebar starts a width transition"))?;
                draw(window, cx)?;
                let reflowing = frame(window, cx)?;
                capture(window, cx, output.join("w25-frame-reflowing.png"))?;
                let live = reflowing["rightNow"].as_f64().unwrap_or(0.0);
                let target = reflowing["rightTarget"].as_f64().unwrap_or(0.0);
                anyhow::ensure!(reflowing["reducedMotion"] == false, "the animated variant runs with motion on: {reflowing}");
                anyhow::ensure!(live > 1.0 && live < target - 1.0, "captured mid-transition at {pinned_ms} ms: {reflowing}");
                // Same instant: the canvas follows the live width exactly.
                assert_frame(&reflowing, "mid-reflow")?;
                window.update(cx, |shell, _, cx| shell.fixture_unpin_motion(cx))?;
                for _ in 0..8 {
                    draw(window, cx)?;
                    settle(cx, 60).await;
                }
                let opened = frame(window, cx)?;
                anyhow::ensure!((opened["rightNow"].as_f64().unwrap_or(0.0) - target).abs() < 0.5, "the transition completes at its target: {opened}");
                assert_frame(&opened, "after the reflow")?;
                // The no-animation equivalent: with reduced motion the same
                // toggle lands at its target in the first frame.
                window.update(cx, |shell, _, cx| shell.fixture_toggle_sidebar(true, cx))?;
                for _ in 0..8 {
                    draw(window, cx)?;
                    settle(cx, 60).await;
                }
                anyhow::ensure!(frame(window, cx)?["rightOpen"] == false, "the right sidebar closed again");
                cx.update(|cx| motion::set_reduced_motion(cx, true));
                draw(window, cx)?;
                window.update(cx, |shell, _, cx| shell.fixture_toggle_sidebar(true, cx))?;
                draw(window, cx)?;
                let reduced = frame(window, cx)?;
                capture(window, cx, output.join("w25b-frame-reduced-motion.png"))?;
                anyhow::ensure!(reduced["reducedMotion"] == true && (reduced["rightNow"].as_f64().unwrap_or(0.0) - reduced["rightTarget"].as_f64().unwrap_or(-1.0)).abs() < 0.5, "under reduced motion the width lands in the first frame: {reduced}");
                assert_frame(&reduced, "reduced motion")?;
                cx.update(|cx| motion::set_reduced_motion(cx, system_reduced));
                draw(window, cx)?;
                for _ in 0..4 {
                    draw(window, cx)?;
                    settle(cx, 60).await;
                }
                // Resize the left sidebar by its seam: the canvas reflows.
                let before_resize = frame(window, cx)?;
                let seam = before_resize["sidebarNow"].as_f64().unwrap_or(256.0) as f32;
                drag(window, cx, (seam, HEIGHT / 2.0), (seam + 60.0, HEIGHT / 2.0)).await?;
                let resized = frame(window, cx)?;
                anyhow::ensure!(resized["sidebarNow"].as_f64().unwrap_or(0.0) > before_resize["sidebarNow"].as_f64().unwrap_or(0.0) + 30.0, "the sidebar seam resizes: {before_resize} -> {resized}");
                assert_frame(&resized, "after a sidebar resize")?;
                shot(window, cx, output.join("w26-frame-sidebar-resized.png")).await?;
                progress(&output, &mut record, "frame".into(), serde_json::json!({
                    "bothOpen": both, "leftCollapsed": left_collapsed, "bothCollapsed": both_collapsed,
                    "rightCollapsed": right_collapsed, "reflowing": reflowing, "reflowPinnedMs": pinned_ms,
                    "reflowCompleted": opened, "reducedMotion": reduced, "systemReducedMotion": system_reduced,
                    "resized": resized,
                }));
                // Right sidebar top rows, dark then light.
                for (mode, suffix) in [
                    (appearance::AppearanceMode::Dark, "dark"),
                    (appearance::AppearanceMode::Light, "light"),
                ] {
                    cx.update(|cx| appearance::set_mode(mode, cx));
                    for surface in ["related", "changes", "files"] {
                        window.update(cx, |shell, window, cx| shell.fixture_open_surface(surface, window, cx))?;
                        for _ in 0..6 {
                            draw(window, cx)?;
                            settle(cx, 80).await;
                        }
                        let state = frame(window, cx)?;
                        assert_frame(&state, surface)?;
                        shot(window, cx, output.join(format!("w27-{surface}-{suffix}.png"))).await?;
                    }
                }
                cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark, cx));
                settle(cx, 200).await;

                // w28. Representative themes: the frame in each, with text
                // measured against the RENDERED surface pixels (canvas, left
                // chrome, right chrome) over both desktop extremes.
                window.update(cx, |shell, window, cx| shell.fixture_open_surface("related", window, cx))?;
                let mut sampled = Vec::new();
                for (variant, dark, surface) in [
                    ("luna-dark", true, luna_theme::SurfacePreference::ThemeDefault),
                    ("luna-light", false, luna_theme::SurfacePreference::ThemeDefault),
                    ("luna-dark", true, luna_theme::SurfacePreference::Opaque),
                    ("github-light", false, luna_theme::SurfacePreference::ThemeDefault),
                    ("dracula", true, luna_theme::SurfacePreference::Frosted),
                    ("gruvbox-light", false, luna_theme::SurfacePreference::ThemeDefault),
                    ("catppuccin-latte", false, luna_theme::SurfacePreference::Frosted),
                    ("tokyo-night", true, luna_theme::SurfacePreference::ThemeDefault),
                ] {
                    cx.update(|cx| {
                        let (appearance, mode) = if dark {
                            (theme::Appearance::Dark, appearance::AppearanceMode::Dark)
                        } else {
                            (theme::Appearance::Light, appearance::AppearanceMode::Light)
                        };
                        appearance::set_mode(mode, cx);
                        appearance::set_theme(appearance, variant, cx);
                        appearance::set_surface(surface, cx);
                    });
                    for _ in 0..4 {
                        draw(window, cx)?;
                        settle(cx, 60).await;
                    }
                    let state = frame(window, cx)?;
                    let canvas = &state["canvas"];
                    let (cx0, cy0, ch) = (
                        canvas["x"].as_f64().unwrap_or(0.0) as f32,
                        canvas["y"].as_f64().unwrap_or(0.0) as f32,
                        canvas["h"].as_f64().unwrap_or(0.0) as f32,
                    );
                    let points = [
                        (cx0 + 5.0, cy0 + ch / 2.0),
                        (60.0, HEIGHT - 90.0),
                        (WIDTH - 24.0, HEIGHT - 24.0),
                    ];
                    let pixels = sample(window, cx, &points)?;
                    shot(window, cx, output.join(format!("w28-theme-{variant}-{surface:?}.png"))).await?;
                    let theme = cx.update(|cx| theme::Theme::of(cx).clone());
                    let mut measured = Vec::new();
                    for ((name, pixel), backdrop) in ["canvas", "left chrome", "right chrome"]
                        .iter()
                        .zip(pixels.iter())
                        .flat_map(|pair| [(pair, 0.0_f32), (pair, 1.0_f32)])
                    {
                        let bg = over(*pixel, backdrop);
                        for (role, fg, min) in [
                            ("text", theme.text, 4.5_f32),
                            ("secondary", theme.readable_muted(0.5), 4.5),
                            ("focus edge", theme.border_strong, 3.0),
                        ] {
                            let ratio = theme::contrast_ratio(theme::flatten(fg, bg), bg);
                            anyhow::ensure!(
                                ratio >= min,
                                "{variant} {surface:?}: {role} on rendered {name} (backdrop {backdrop}) is {ratio:.2}:1 < {min}"
                            );
                            measured.push(serde_json::json!({
                                "surface": name, "backdrop": backdrop, "role": role,
                                "pixel": pixel, "ratio": ratio,
                            }));
                        }
                    }
                    sampled.push(serde_json::json!({ "variant": variant, "surface": format!("{surface:?}"), "pairs": measured }));
                }
                cx.update(|cx| {
                    appearance::set_mode(appearance::AppearanceMode::Dark, cx);
                    appearance::set_theme(theme::Appearance::Dark, "luna-dark", cx);
                    appearance::set_surface(luna_theme::SurfacePreference::ThemeDefault, cx);
                });
                settle(cx, 200).await;
                progress(&output, &mut record, "renderedContrast".into(), Value::Array(sampled));

                // Lineage never follows tab position.
                let after = wb(window, cx)?;
                for chat in [CHILD_A, CHILD_B] {
                    if occurrences(&after, chat) == 1 {
                        anyhow::ensure!(role_of(&after, chat).as_deref() == Some("Child"), "{chat} stays a Child wherever it sits: {after}");
                    }
                }
                anyhow::ensure!(seeded_ids.iter().all(|id| occurrences(&after, id) <= 1), "each chat at most once: {after}");
                progress(&output, &mut record, "final".into(), after);
                Ok(Value::Object(record))
            }
            .await;
            match result {
                Ok(record) => {
                    let written = std::fs::write(output.join("readback.json"), serde_json::to_vec_pretty(&record).unwrap_or_default())
                        .and_then(|_| {
                            std::fs::write(
                                output.join("chat-workbench.pass"),
                                "PASS: headless production Shell against an in-process engine with real pointer and keyboard input; Main stays open and labelled in its own pane through middle-click, the Cmd-W path and Close others; Related rows open, focus and never duplicate center tabs and stay in the library while the right pane never renders a conversation; tab and row drags preview then commit right and nested-down splits, a strip reorder, and collapse an emptied branch; self drops and an Escape-cancelled drag change nothing; the divider follows the pointer without writes, commits once and double-click equalizes; `+` creates exactly one blank child of Main in the focused group with its composer focused; two composers keep separate drafts and a send stays with its chat while its tab moves; Cmd-W closes a child view with its draft kept and Cmd-Shift-T reopens it; the layout survives a new Shell, switching main chats and a narrow window without rewriting it\n",
                            )
                        });
                    if let Err(error) = written {
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                        std::process::exit(1);
                    }
                }
                Err(error) => {
                    eprintln!("Chat workbench fixture failed: {error:#}");
                    let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                    let _ = capture(window, cx, output.join("failure.png"));
                    *failure_for_ui.lock().unwrap() = Some(error.to_string());
                    std::process::exit(1);
                }
            }
            let _ = window.update(cx, |_, window, _| window.remove_window());
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    let failed = failure.lock().unwrap().take();
    runtime.block_on(core.shutdown());
    drop(root);
    if let Some(error) = failed {
        eprintln!("{error}");
        std::process::exit(1);
    }
    Ok(())
}

fn has(list: &Value, id: &str) -> bool {
    list.as_array()
        .into_iter()
        .flatten()
        .any(|item| item.as_str() == Some(id))
}
