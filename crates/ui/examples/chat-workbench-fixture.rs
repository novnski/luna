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
//! 29. Persistent forks (side threads) keep a separate lifecycle: an End
//!     control, expiry/countdown, lease or renewal, unload/reload, a
//!     read-only "ended" panel, or a provider/account/project picker lock;
//!     closing or reopening a fork's tab stops its run or loses its draft;
//!     two forks share a draft, a send, model keys or Stop; Escape leaves a
//!     fork's open model picker open (or reaches the stop path under it,
//!     or takes the keys away from that fork); an archived fork
//!     lets the model shortcut fall back to Main; a fork cannot become the
//!     workbench Main, be placed, pinned, settled or archived like any chat,
//!     or loses its fork icon/source link; nested forks (fork of a fork,
//!     child of a fork) or orphan forks (source deleted) disappear from the
//!     Related library or the sidebar; old device-local side marks are lost
//!     or cleared before the ordinary marks are acknowledged.
//! 30. With either sidebar open (or a narrower window), the workbench is
//!     not contained: a group, its composer pill or a focus outline is
//!     clipped square at the canvas's rounded corner or paints over its
//!     radius-14 hairline (the clip is not inset at least 4.1px, the
//!     arc-safe minimum, on every side); the overflow cue is a bright
//!     border or an opaque patch instead of a pointer-transparent fade
//!     shown only toward the side with more content (fades take presses,
//!     scroll or drops; one shows with nothing beyond it); a selected tab
//!     is marked by an underline, or a group by a full colored pane or tab
//!     outline, instead of the tab's own rounded fill (the focused group's
//!     active tab: a quiet accent tint with an accent role icon; other
//!     groups' active tabs: the neutral selected fill), or the two fills
//!     are the same, or a keyboard focus-visible ring is lost; or the inset
//!     moves the saved
//!     layout, the left anchor, Balance, focus reveal, minimum sizes, or
//!     drop/divider hit geometry relative to what is drawn.
//! 31. A new Main whose engine registry is not ready yet (the layout read
//!     refuses "Main chat is unavailable") latches a permanent "Main stays
//!     open here" notice, or stops retrying, instead of loading the layout
//!     quietly once the engine knows the chat (and clearing any notice).
//! 32. A group's split control opens an empty group or a choice instead of
//!     creating one blank child chat in a new group to its right; or it
//!     starts a provider, makes two children on a repeat/retry, ignores the
//!     group minimum (no room: nothing changes), or loses the failure state.
//! 33. A blank child (never sent) cannot switch provider directly (it asks
//!     for a switch confirmation or a context handoff), or its switch
//!     changes the parent's provider; a started chat or a fork with history
//!     switches without the ordinary confirmation; a second provider pick
//!     replaces (or drops) a direct switch already sent; a late switch
//!     result resets a picker since bound to another chat, engine or device,
//!     or a refused direct switch rolls the row back over newer config; or
//!     a provider's model catalog is queried with another provider's account
//!     (a Claude chat's `claude-native` sent to Codex), or a catalog listed
//!     for one account is reused (or left loading) after the account changes.
//! 34. A child or fork pane still shows an always-visible context row
//!     (status, "Child of"/"Side chat from", parent, provider/model); or its
//!     tab loses live state: no spinner while working, no approval/input/
//!     error glyph, no accent unread dot for an unseen completion (or a dot
//!     that stays on the visible active tab); or the parent/source is no
//!     longer reachable from the tab (tooltip, context menu).
//! 35. Context usage still renders under the composer (or the reclaimed
//!     space stays), is missing from the controls row next to Goal for
//!     Main, a child or a fork, loses its details card or accessible name,
//!     or overlaps/drops the model, access or Goal controls at narrow widths.
//! 36. Settings is not inside the contained rounded content plane (flat on
//!     the chrome, a double frame, clipped corners, mismatched gutters); or
//!     entering Settings with the chat sidebar collapsed hides its
//!     navigation; or Settings' own collapse/expand rewrites the saved chat
//!     sidebar preference, or leaving Settings does not restore it.
//! 37. A tooltip or rich hover card is translucent (underlying text shows
//!     through), loses its readable text/icon contrast in any theme or
//!     surface preference, or its opaque fill is defeated by parent opacity.
//! 38. UI copy calls context-carrying conversations forks (or "Child chat")
//!     instead of Side chat (Fresh or With context); a tab's help names the
//!     wrong source (placement instead of lineage), promises future sync,
//!     hides a deleted source, or says a fresh side chat carries its main
//!     chat's context.
//! 39. `+` creates a conversation (or opens anything but its chooser);
//!     opening, choosing a kind or browsing providers creates something; the
//!     chooser does not name the actual source (the active chat of the group
//!     it was opened from) before Create; a double click or retry creates
//!     two; a chooser left open across a source/main/profile/engine change
//!     still creates (for the stale source). One level of side chats: a side
//!     chat's `+` opens the main chooser or offers With context, Fork or a
//!     nested side chat instead of only New side chat and Hand off; a fresh
//!     side chat asked of a side chat lands under it instead of beside it
//!     under the same main chat; the engine creates a fork of a side chat;
//!     or a context fork asked of a side chat in the UI is neither refused
//!     nor explained.
//! 40. A New thread ignores the chosen provider, reuses another provider's
//!     account, is created under Main instead of the chooser's source, or
//!     starts a provider; a Side chat leaves its source's provider/account,
//!     silently falls back to recent visible context after a native refusal,
//!     offers recent context with no visible messages, or replays an
//!     uncertain create instead of checking its status.
//! 41. The split shortcut stops creating one fresh New thread (from the
//!     group's active chat) directly, ignores the minimum-size guard, or the
//!     chooser breaks drafts, keyboard navigation (arrows, number keys,
//!     Enter, Escape), focus return, or command-palette alternatives.
//! 42. Enter in the chooser creates what no visible button offers: a New
//!     thread with no provider (catalog loading), a Side chat while its
//!     capability is loading or its source has nothing to carry, or a plain
//!     fork where only the recent-messages action is offered (Enter must run
//!     exactly the offered action, recent context with forceSnapshot).
//! 43. A New thread asked for while an earlier creation's outcome is unknown
//!     silently replays that earlier intent (its source, provider, group or
//!     placement) or starts a second one; Check status does not resend the
//!     exact original request (same key), or recovery creates twice.
//! 44. A shortcut's Reset assigns a default another saved shortcut (or a
//!     fixed chord) already uses, so two actions share one chord.
//! 45. Closing the chooser (Escape, Cancel, outside click) moves the keyboard
//!     to Main instead of the source group's chat; or cycling to an empty
//!     group leaves Main's composer focused, so typing, send, stop or close
//!     act on Main while the empty group looks focused.
//! 46. A configurable shortcut is recorded or reset onto a chord a surface
//!     still binds in its own context (the composer's Mod+S Stash prompt,
//!     the browser's Mod+L/T/[/]), silently shadowing one of them; or the
//!     check refuses the action that shares that meaning (Save file's Mod+S,
//!     Reload's own default); or a refused Reset/record changes a saved
//!     custom binding.
//! 47. A Side chat or New thread opened as its own workbench Main loses its
//!     lineage help (reads as a plain Main), or a deleted source is not
//!     named as "a deleted chat"; a parentless Main gains lineage copy.
//! 48. Background work: legacy children (ancestry, no launch ownership) are
//!     selectable for an owned-work stop, or the real host's view is missing.
//! 49. An owned child or grandchild is not stoppable, a service or a later
//!     independent child is selectable, or counts come from the page.
//! 50. The Work chip shows in, or acts from, a composer that is not the
//!     focused chat's; paging duplicates rows or treats a page as everything.
//! 51. A stop runs without an explicit selection and preview; Requested reads
//!     as Stopped; one preview sends more than one request id.
//! 52. A lost stop answer is retried or shown as done.
//! 53. A refused (stale) preview is retargeted instead of returning to the
//!     selection with rows that are no longer stoppable dropped.
//! 54. After the work stream ends, the last list reads as live or can be
//!     stopped.
//! 55. Escape in the Work popover reaches the stop path.
//! 56. A Work row's Open targets the focused chat, a same-titled chat or the
//!     owner instead of the row's own chat (Open chat), or the row's chat
//!     instead of its lifecycle owner (Open owner).
//! 57. Open from a pane composer's popover routes through Main's or opens
//!     outside the workbench's related-chat routing; the popover stays open.
//! 58. A row whose chat is unknown or deleted offers Open, or Open falls back
//!     to the selected chat.
//! 59. A service offers Open chat instead of its project commands, or Show
//!     commands acts on another chat's checkout.
//! 60. Open from a list read on an earlier engine connection still navigates.
//! 61. A row held as `executionUnavailable` (a known pre-provider rejection)
//!     shows anything but "Provider unavailable — review and send", loses its
//!     exact id/revision, appears in another chat's composer, or is resent
//!     automatically after a reconnect; its explicit Send now runs from
//!     another composer, or for another chat, or sends twice.
//! 27. Balance panes counts tabs instead of groups, changes an axis, the
//!     order, IDs, active chats or focus, leaves a mixed 2-over-1 or six-group
//!     tree with unequal group areas, writes again on a repeat, is lost on a
//!     new Shell, starts a provider, or is offered with a single group.
//! 28. A sidebar opening (or a narrower window) over a fitting split makes
//!     the workbench overflow or scroll, moves focus, or rewrites the saved
//!     layout; or an explicit focus no longer selects its group. (Before
//!     slice 6 the canvas stayed anchored left and scrolled; the user's
//!     fit-to-window requirement replaced that: groups shrink to fit.)
//! 26. A split layout in a canvas smaller than its saved minimums (window or
//!     sidebars shrunk) overflows the viewport or scrolls the workbench
//!     horizontally or vertically, distributes width out of the saved
//!     proportions, lets a composer spill out of its group, rewrites the
//!     saved layout (axis, ratio, groups, revision), swaps axes or hides
//!     groups; drops or previews land over a sidebar; or widening does not
//!     restore the exact saved presentation. (Before slice 6 this case
//!     required a 320x240 minimum per group and a scrolling canvas; native
//!     RED `ui-resize-red-1200` showed that design overflowing by up to
//!     1,014 px, so groups now share the viewport.)
//! 25. With both sidebars open, a long focused title in the frame row
//!     pushes Review changes / Commit & push past the row's right edge, under
//!     the right sidebar; or a real click on Review changes does not open
//!     Changes bound to the focused chat's checkout.
//! 24. With both sidebars open after a wider split, an active tab is wider
//!     than its strip, so its close control sits under the trailing
//!     controls; or after archive removes the focused composer, Cmd-/
//!     (without a click in between) opens Main's model picker.
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
//! 19. Forks from different sources (Main, a fork, Child A) lose their
//!     exact source in Main's Related library; a tab without a rendered
//!     composer (an archived fork) still opens a hidden picker or falls back
//!     to Main's.
//! 15. A side chat opens as a Side tab without its exact source parent,
//!     closing its tab stops or ends it, or a closed Side tab cannot be
//!     reopened from Main's Related library.
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
        // Legacy fixture children: ancestry only, no launch ownership.
        work_origin: None,
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
    let keystroke = Keystroke::parse(combo)?;
    // Include GPUI's simulated IME path: a bare KeyDown dispatch runs
    // shortcuts but never inserts ordinary text into the focused input.
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_keystroke(keystroke, cx);
    })?;
    Ok(())
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

/// Session/read-attention status is live data, not part of the persisted
/// split tree. Reopening a viewed chat can legitimately clear Completed.
/// Keep every other tab and geometry field in the persistence comparison.
fn layout_without_live_status(root: &Value) -> Value {
    match root {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(key, _)| key.as_str() != "status")
                .map(|(key, value)| (key.clone(), layout_without_live_status(value)))
                .collect(),
        ),
        Value::Array(values) => {
            Value::Array(values.iter().map(layout_without_live_status).collect())
        }
        _ => root.clone(),
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

/// The group's trailing controls sit outside the tab viewport, never under a
/// tab, and inside the group. Exactly one entry control is painted: New
/// child (`+`) while the group is wide, or Group actions (the compact strip,
/// whose menu holds `+`, split and close) while it is narrow; neither, or
/// both at once, fails.
fn controls_clear(rb: &Value, pane_id: &str) -> bool {
    let view = rb["tabBounds"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|pane| pane["pane"] == pane_id)
        .map(|pane| rect(&pane["view"]));
    let control = |key: &str| {
        rb[key]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["pane"] == pane_id)
            .map(|entry| rect(&entry["bounds"]))
    };
    let control = match (control("plusBounds"), control("groupActionsBounds")) {
        (Some(control), None) | (None, Some(control)) => control,
        _ => return false,
    };
    let group = pane_rect(rb, pane_id).ok();
    matches!((view, group), (Some(view), Some(group))
        if control.0 >= view.0 + view.2 - 0.5
            && control.2 > 0.0
            && control.0 >= group.0 - 0.5
            && control.0 + control.2 <= group.0 + group.2 + 0.5)
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
/// Failure case 30: the workbench stays contained in its rounded canvas.
/// The viewport's rectangular clip sits at least 4.1 px (the arc-safe
/// minimum for the 14 px radius) inside the canvas on every side; overflow
/// fades show exactly toward the sides with more content; no group or tab
/// has an outline or underline: each group's active tab is marked by its
/// own rounded fill, the focused group's by the accent tint (distinct from
/// the others' neutral fill), with a full-strength title.
fn assert_contained(rb: &Value, what: &str) -> anyhow::Result<()> {
    let insets: Vec<f64> = rb["viewportInsets"]
        .as_array()
        .map(|values| values.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    anyhow::ensure!(
        insets.len() == 4 && insets.iter().all(|inset| (4.1..=6.5).contains(inset)),
        "{what}: the viewport clip is inset 4.1-6.5 px inside the rounded canvas on every side: {:?} in {rb}",
        insets
    );
    let (x, y) = (
        rb["canvasScroll"]["x"].as_f64().unwrap_or(0.0),
        rb["canvasScroll"]["y"].as_f64().unwrap_or(0.0),
    );
    let (max_x, max_y) = (
        rb["canvasScroll"]["maxX"].as_f64().unwrap_or(0.0),
        rb["canvasScroll"]["maxY"].as_f64().unwrap_or(0.0),
    );
    let expected = serde_json::json!([x < -0.5, max_x + x > 0.5, y < -0.5, max_y + y > 0.5]);
    anyhow::ensure!(
        rb["overflowFades"] == expected,
        "{what}: fades show only toward more content (expected {expected}): {rb}"
    );
    anyhow::ensure!(rb["paneOutline"] == false, "{what}: no group outline: {rb}");
    let selection = rb["tabSelection"].as_array().cloned().unwrap_or_default();
    let multi = all_panes(rb).len() > 1;
    anyhow::ensure!(
        selection.len() == all_panes(rb).len(),
        "{what}: every group reports its selected tab: {rb}"
    );
    for tab in &selection {
        let focused = multi && tab["pane"] == rb["focusedPane"];
        anyhow::ensure!(
            tab["focused"] == focused
                && tab["accentTint"] == focused
                && tab["underline"] == false
                && tab["outline"] == false
                && tab["fill"][3].as_f64().is_some_and(|alpha| alpha > 0.0),
            "{what}: the selected tab is its rounded fill (the accent tint only in the focused group), no underline or outline: {tab} in {rb}"
        );
    }
    if multi {
        let fills: std::collections::HashSet<String> = selection
            .iter()
            .map(|tab| tab["fill"].to_string())
            .collect();
        anyhow::ensure!(
            fills.len() == 2,
            "{what}: the focused group's selected fill differs from the other groups': {selection:?}"
        );
    }
    Ok(())
}

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

/// w20l's queued prompt: the only request the w20l peer treats specially.
const W20L_PROMPT: &str = "Run the release checklist again";
/// w20m's automation prompt: delivered by the host's own worker, never typed.
const W20M_PROMPT: &str = "Automation check: summarize what changed since yesterday";

/// The fixture's Mock provider, unchanged for every request except w20l's
/// exact queued prompt and w20m's automation prompt. Those runs behave like a
/// real native peer: each reports `SessionStarted` (a unique native session
/// per chat, in the request's own cwd) before its reply, so the engine can
/// durably correlate acceptance.
struct FixtureMock {
    inner: MockHarness,
}

#[async_trait::async_trait]
impl luna_harness::Harness for FixtureMock {
    fn id(&self) -> HarnessId {
        self.inner.id()
    }
    fn display_name(&self) -> &str {
        self.inner.display_name()
    }
    fn supports_steering(&self) -> bool {
        self.inner.supports_steering()
    }
    fn steering_mode(&self) -> luna_proto::SteeringMode {
        self.inner.steering_mode()
    }
    fn reasoning_levels(&self) -> &[luna_proto::ReasoningLevel] {
        self.inner.reasoning_levels()
    }
    fn supports_safe_recovery_continuation(&self) -> bool {
        self.inner.supports_safe_recovery_continuation()
    }
    async fn models(&self) -> Result<Vec<luna_proto::Model>, luna_harness::HarnessError> {
        self.inner.models().await
    }
    async fn compact_context(
        &self,
        request: luna_harness::ContextCompactRequest,
    ) -> Result<(), luna_harness::HarnessError> {
        self.inner.compact_context(request).await
    }
    async fn run_title(
        &self,
        request: luna_proto::RunRequest,
        controls: luna_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<AgentEvent, luna_harness::HarnessError>>,
        luna_harness::HarnessError,
    > {
        self.inner.run_title(request, controls).await
    }
    async fn run(
        &self,
        request: luna_proto::RunRequest,
        controls: luna_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<AgentEvent, luna_harness::HarnessError>>,
        luna_harness::HarnessError,
    > {
        let (session_id, assistant_message_id) = if request.prompt.contains(W20L_PROMPT) {
            ("w20l-native-session", "w20l-assistant-message")
        } else if request.prompt.contains(W20M_PROMPT) {
            ("w20m-native-session", "w20m-assistant-message")
        } else {
            return self.inner.run(request, controls).await;
        };
        let session_id = session_id.to_string();
        let peer = MockHarness {
            script: vec![
                AgentEvent::SessionStarted {
                    harness: HarnessId::Mock,
                    model: request.model.clone().unwrap_or_else(|| "mock-1".into()),
                    tools: Vec::new(),
                    cwd: request.cwd.clone(),
                    session_id: session_id.clone(),
                    assistant_message_id: assistant_message_id.into(),
                },
                AgentEvent::TextDelta {
                    text: MOCK_ANSWER.into(),
                },
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: Some(session_id),
                },
            ],
        };
        peer.run(request, controls).await
    }
}

/// w20m: the mounted automations panel's readback (`settings`: app-wide).
fn automations(window: Window, cx: &mut AsyncApp, settings: bool) -> anyhow::Result<Value> {
    window.update(cx, |shell, _, cx| {
        shell
            .fixture_automations(settings)
            .map(|panel| panel.read(cx).fixture_readback(cx))
            .unwrap_or(Value::Null)
    })
}

/// w20m: draw and re-read the panel until `done` (100 ms steps).
async fn wait_automations(
    window: Window,
    cx: &mut AsyncApp,
    settings: bool,
    what: &str,
    tries: usize,
    done: impl Fn(&Value) -> bool,
) -> anyhow::Result<Value> {
    let mut last = Value::Null;
    for _ in 0..tries {
        draw(window, cx)?;
        last = automations(window, cx, settings)?;
        if done(&last) {
            return Ok(last);
        }
        settle(cx, 100).await;
    }
    anyhow::bail!("timed out waiting for {what}: {last}")
}

/// A host-reported work row (slice 3) for the scripted work host.
#[allow(clippy::too_many_arguments)]
fn work_item(
    id: &str,
    task: &str,
    title: &str,
    kind: luna_proto::TaskWorkKind,
    state: luna_proto::TaskWorkState,
    owned: bool,
    blocks: bool,
    can_stop: bool,
    attention: bool,
    owner: Option<(&str, &str)>,
) -> luna_proto::TaskWorkItem {
    luna_proto::TaskWorkItem {
        id: id.into(),
        task_id: task.into(),
        title: title.into(),
        kind,
        state,
        origin: owner.map(|(owner, launch)| luna_proto::WorkOrigin {
            owner_task_id: owner.into(),
            owner_turn_id: format!("{owner}-msg-1"),
            launch_task_id: launch.into(),
            launch_turn_id: format!("{launch}-msg-1"),
        }),
        owned,
        blocks_completion: blocks,
        needs_attention: attention,
        can_stop,
        detail: String::new(),
    }
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
    // The lineage and context metadata the live chats watch keeps.
    let fork_sources = state::fork_source_rows(&frame);
    let context_modes = state::side_chat_context_modes(&frame);
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
            state.fork_source_rows.extend(fork_sources);
            state.side_chat_context_modes.extend(context_modes);
            cx.notify();
        })
    });
    Ok(rows)
}

/// A tab's readback entry.
fn tab_json(rb: &Value, chat: &str) -> Option<Value> {
    all_panes(rb)
        .into_iter()
        .flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default())
        .find(|tab| tab["chatId"] == chat)
}

/// A title on one line, as tabs show it.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `chat`'s row as the UI state holds it.
fn chat_row(
    cx: &mut AsyncApp,
    state: &gpui::Entity<state::AppState>,
    chat: &str,
) -> Option<luna_proto::Chat> {
    cx.update(|cx| {
        state
            .read(cx)
            .chats
            .iter()
            .find(|row| row.id == chat)
            .cloned()
    })
}

/// Whether `parent`'s Related library lists `chat` as `kind`, under `via`
/// (its actual source when nested) or directly.
fn tree_has(rb: &Value, parent: &str, chat: &str, kind: &str, via: Option<&str>) -> bool {
    rb["relatedTrees"][parent].as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            row[0] == chat
                && row[1] == kind
                && match via {
                    Some(source) => row[2] == source,
                    None => row[2].is_null(),
                }
        })
    })
}

/// The engine's command snapshot for `chat`.
async fn task_snapshot(reader: &luna_rpc::RpcClient, chat: &str) -> anyhow::Result<Value> {
    Ok(reader
        .call(
            luna_rpc::methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": chat }),
        )
        .await?["snapshot"]
        .clone())
}

fn active_turn(snapshot: &Value) -> Option<String> {
    snapshot["activeTurnId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// Wait until the engine reports an active turn for `chat`.
async fn wait_active_turn(
    cx: &mut AsyncApp,
    reader: &luna_rpc::RpcClient,
    chat: &str,
    what: &str,
) -> anyhow::Result<String> {
    for _ in 0..100 {
        settle(cx, 100).await;
        if let Some(turn) = active_turn(&task_snapshot(reader, chat).await?) {
            return Ok(turn);
        }
    }
    anyhow::bail!(
        "{what}: the engine never reported an active turn: {}",
        task_snapshot(reader, chat).await?
    )
}

/// Whether the engine still publishes `chat`.
async fn engine_has(reader: &luna_rpc::RpcClient, chat: &str) -> anyhow::Result<bool> {
    let mut rx = reader
        .subscribe(luna_rpc::methods::WATCH_CHATS, serde_json::json!({}))
        .await?;
    let frame = rx
        .recv()
        .await
        .ok_or_else(|| anyhow::anyhow!("no chats frame"))?;
    Ok(frame
        .as_array()
        .into_iter()
        .flatten()
        .any(|row| row["id"] == chat))
}

/// Merge engine rows until `chat`'s row satisfies `done`.
async fn wait_row(
    cx: &mut AsyncApp,
    reader: &luna_rpc::RpcClient,
    state: &gpui::Entity<state::AppState>,
    chat: &str,
    what: &str,
    done: impl Fn(&luna_proto::Chat) -> bool,
) -> anyhow::Result<luna_proto::Chat> {
    for _ in 0..80 {
        merge_engine_chats(reader, state, cx).await?;
        if let Some(row) = chat_row(cx, state, chat).filter(|row| done(row)) {
            return Ok(row);
        }
        settle(cx, 150).await;
    }
    anyhow::bail!("{what}: {:?}", chat_row(cx, state, chat))
}

/// The user's New side chat, until it opens as a Side tab that is not one
/// of `known`.
async fn create_fork(
    window: Window,
    cx: &mut AsyncApp,
    reader: &luna_rpc::RpcClient,
    state: &gpui::Entity<state::AppState>,
    known: &[String],
    what: &str,
) -> anyhow::Result<String> {
    window.update(cx, |shell, _, cx| shell.fixture_side_chat("new", "", cx))?;
    for _ in 0..80 {
        merge_engine_chats(reader, state, cx).await?;
        settle(cx, 150).await;
        let rb = wb(window, cx)?;
        let created = all_panes(&rb)
            .into_iter()
            .flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default())
            .filter(|tab| tab["role"] == "Side")
            .filter_map(|tab| tab["chatId"].as_str().map(str::to_string))
            .find(|id| !known.contains(id));
        if let Some(id) = created {
            return Ok(id);
        }
    }
    let side = window.update(cx, |shell, _, cx| shell.fixture_side_chat_readback(cx))?;
    anyhow::bail!("{what}: New side chat never opened as a Side tab: {side}")
}

/// A fork's tab is an ordinary thread's: a live composer (unless archived),
/// a fork identity, a source that links only while it exists, and no End,
/// lease, countdown, expiry or read-only state anywhere in its header.
async fn assert_ordinary_fork(
    window: Window,
    cx: &mut AsyncApp,
    chat: &str,
    linked: bool,
    what: &str,
) -> anyhow::Result<()> {
    draw(window, cx)?;
    settle(cx, 200).await;
    let strip = window.update(cx, |shell, _, cx| shell.fixture_workbench_strip(chat, cx))?;
    anyhow::ensure!(
        strip["identity"] == "Side chat" && strip["parentLinked"] == linked,
        "{what}: a fork names its source (linked: {linked}): {strip}"
    );
    // Titles and provider/model labels are user content (the source here is
    // named "Release plan"). Inspect the generated lifecycle status only.
    let words = strip["status"].as_str().unwrap_or_default().to_lowercase();
    for lifecycle in [
        "ended",
        "read-only",
        "writable",
        "expire",
        "min left",
        "lease",
        "unload",
        "end side chat",
    ] {
        anyhow::ensure!(
            !words.contains(lifecycle),
            "{what}: no separate fork lifecycle ({lifecycle:?}) in its header: {strip}"
        );
    }
    let composer = window.update(cx, |shell, _, cx| {
        shell.fixture_workbench_tab_has_composer(chat, cx)
    })?;
    anyhow::ensure!(composer, "{what}: the fork's tab renders its own composer");
    anyhow::ensure!(
        strip["metaRow"] == false,
        "{what}: the pane renders no context row (identity is on the tab): {strip}"
    );
    Ok(())
}

/// Close `chat`'s tab, then reopen it from Main's Related library with a
/// real click: it returns as a Side tab under `source`.
async fn close_and_reopen(
    window: Window,
    cx: &mut AsyncApp,
    chat: &str,
    source: &str,
    what: &str,
) -> anyhow::Result<()> {
    if occurrences(&wb(window, cx)?, chat) > 0 {
        window.update(cx, |shell, _, cx| shell.fixture_workbench_close(chat, cx))?;
        let id = chat.to_string();
        wait_for(window, cx, "the fork's tab closed", move |rb| {
            occurrences(rb, &id) == 0
        })
        .await?;
    }
    window.update(cx, |shell, _, cx| {
        shell.fixture_open_right_pane(cx);
        shell.fixture_open_child_chats(cx);
    })?;
    settle(cx, 600).await;
    let pane_x = WIDTH - PANE_WIDTH / 2.0;
    let rows = related_rows(window, cx, pane_x)?;
    click(window, cx, (pane_x, row_y(&rows, chat)?), MouseButton::Left).await?;
    let id = chat.to_string();
    let reopened = wait_for(window, cx, "the fork's tab reopened", move |rb| {
        occurrences(rb, &id) == 1
    })
    .await?;
    let tab = tab_json(&reopened, chat).unwrap_or_default();
    anyhow::ensure!(
        tab["role"] == "Side" && tab["parent"] == source,
        "{what}: reopened as a Side tab under its source: {tab}"
    );
    Ok(())
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

/// The content viewport sits 6 px inside the rounded canvas, whose outer
/// frame starts below the titlebar and 8 px inside either sidebar.
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
        (y - (titlebar + 6.0)).abs() <= 1.5,
        "{what}: the content viewport is inset below the titlebar: {state}"
    );
    let left = get(&state["sidebarNow"]);
    anyhow::ensure!(
        (x - (left + 14.0)).abs() <= 2.0,
        "{what}: canvas inset from the left sidebar: {state}"
    );
    let right_edge = get(&state["viewportWidth"]) - get(&state["rightNow"]);
    anyhow::ensure!(
        ((x + w) - (right_edge - 14.0)).abs() <= tolerance,
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

/// The user API intentionally rejects an explicit Mock provider. The GUI
/// fixture displays only that registered synthetic provider; choosing it on
/// a Mock parent is equivalent to the API's legal inherited-provider request.
/// Keep this adaptation in the fixture transport, never in production UI or
/// engine validation. Real cross-provider choices have engine and native QA.
struct FixtureProviderRpc {
    inner: Arc<dyn luna_rpc::RpcService>,
    workspace: luna_engine::WorkspaceHost,
}

#[async_trait::async_trait]
impl luna_rpc::RpcService for FixtureProviderRpc {
    async fn handle(
        &self,
        method: &str,
        mut params: Value,
    ) -> Result<luna_rpc::RpcReply, luna_rpc::RpcError> {
        if method == luna_rpc::methods::CREATE_CHILD_TAB && params["provider"] == "mock" {
            let parent = params["parentTaskId"].as_str().unwrap_or_default();
            let same_mock = self
                .workspace
                .chat(parent)
                .ok()
                .flatten()
                .and_then(|chat| chat.config)
                .is_some_and(|config| config.harness == HarnessId::Mock);
            if same_mock && let Some(params) = params.as_object_mut() {
                params.remove("provider");
            }
        }
        self.inner.handle(method, params).await
    }
}

fn main() -> anyhow::Result<()> {
    // SAFETY: set first thing in `main`, before the Tokio runtime, the GPUI
    // platform or any other thread exists, and never changed afterwards; the
    // Mock harness only reads it. Process-global on purpose: the fixture's
    // engine runs in this process.
    unsafe {
        std::env::set_var("LUNA_MOCK_DELAY_MS", MOCK_DELAY_MS);
        // This fixture offers the registered synthetic provider in selectors;
        // ordinary app runs intentionally hide it.
        std::env::set_var("LUNA_HARNESS", "mock");
    }
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
    registry.register(Arc::new(FixtureMock {
        inner: MockHarness {
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
        },
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
            // The engine row carries the same lineage, inert (no initial
            // prompt to start, no results to report): the chat model's
            // canonical roles see Child A and Child B as side chats of Main,
            // while the engine never starts or sweeps these seeded children.
            chat.child_thread = child_thread(spec, at_ms).map(|mut child| {
                child.initial_prompt.clear();
                child
            });
            core.workspace.import_chat_row(&chat)?;
            chat.child_thread = child_thread(spec, at_ms);
            ui_chats.push(chat);
        }
        anyhow::Ok(ui_chats)
    })?;
    let seeded_ids: Vec<String> = SPECS.iter().map(|spec| spec.id.to_string()).collect();
    let spaces = core.workspace.read_spaces()?;
    let fixture_service: Arc<dyn luna_rpc::RpcService> = Arc::new(FixtureProviderRpc {
        inner: core.rpc_service(),
        workspace: core.workspace.clone(),
    });
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core_with_service(
        &core,
        fixture_service,
    ))?;
    let handle_a = handle.clone();
    // w20l seeds one held queue row through the real document host.
    let queue_doc_host = core.doc_host.clone();
    let queue_device = core.device_id.clone();
    // ...and proves Send now's disposition from the durable command ledger.
    let command_store = core.store.clone();
    // w20m adds its own chats through the engine: a fresh local automation
    // chat and a replicated row owned by another host.
    let automation_workspace = core.workspace.clone();
    let automation_cwd = project.clone();
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
                |window, cx| {
                    // Match the app window: the rem is the Interface size.
                    window.set_rem_size(px(typography::font_size(cx).pixels()));
                    cx.new(|cx| shell::Shell::new(state, boot, cx))
                },
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

                // w07 (failure cases 39-40; one level of side chats). Child
                // A is a side chat of Main, so `+` in its group opens the
                // side chat's own card naming Child A, never the main
                // chooser: New side chat and Hand off only. Opening and
                // Escape create nothing and return the keyboard to Child A.
                // New side chat makes exactly one fresh sibling under Main
                // (the same main chat, never a child of Child A), opened in
                // that group with its composer focused.
                let a_pane = pane_of(&equal, CHILD_A).unwrap()["id"].as_str().unwrap().to_string();
                window.update(cx, |shell, _, cx| shell.fixture_board_chat_probe(true, cx))?;
                click_tab(window, cx, CHILD_A).await?;
                let before_rows = merge_engine_chats(&reader, &state, cx).await?;
                let plus = plus_center(&wb(window, cx)?, &a_pane)?;
                click(window, cx, plus, MouseButton::Left).await?;
                settle(cx, 300).await;
                draw(window, cx)?;
                let card = window.update(cx, |shell, _, cx| shell.fixture_board_chat_readback(cx))?;
                let chooser = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                anyhow::ensure!(
                    card["sideCard"]["sideId"] == CHILD_A && card["sideCard"]["mainId"] == MAIN && chooser["open"] == false,
                    "`+` in a side chat's group opens that side chat's own card (Child A, main Main), never the main chooser: {} {chooser}",
                    card["sideCard"]
                );
                anyhow::ensure!(
                    card["marks"]["side-card"] == serde_json::json!(["New side chat", "Hand off to a new main chat…"]),
                    "a side chat offers only a fresh sibling and a handoff: {}",
                    card["marks"]
                );
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_overlay_owns_keyboard(cx))?, "the card owns the keyboard");
                shot(window, cx, output.join("w07a-side-card.png")).await?;
                key(window, cx, "escape")?;
                settle(cx, 300).await;
                let cancelled = window.update(cx, |shell, _, cx| shell.fixture_board_chat_readback(cx))?;
                let cancelled_rows = merge_engine_chats(&reader, &state, cx).await?;
                anyhow::ensure!(cancelled["sideCard"].is_null() && cancelled_rows.len() == before_rows.len() && wb(window, cx)?["creatingChild"] == false, "Escape closes the card and creates nothing: {}", cancelled["sideCard"]);
                // Failure case 45: the keyboard returns to the source group's
                // chat (Child A), not Main; a typed key lands in Child A.
                let owner = window.update(cx, |shell, window, cx| shell.fixture_workbench_keyboard_chat(window, cx))?;
                anyhow::ensure!(owner.as_deref() == Some(CHILD_A), "closing the card returns the keyboard to Child A: {owner:?}");
                let main_draft_before = window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))?;
                key(window, cx, "q")?;
                settle(cx, 150).await;
                let child_draft = window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(CHILD_A, cx))?;
                anyhow::ensure!(child_draft.as_deref().is_some_and(|draft| draft.ends_with('q')) && window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))? == main_draft_before, "the key typed after Escape lands in Child A, not Main: {child_draft:?}");
                window.update(cx, |shell, _, cx| shell.fixture_workbench_type(CHILD_A, "", false, cx))?;
                click(window, cx, plus, MouseButton::Left).await?;
                settle(cx, 300).await;
                draw(window, cx)?;
                let card = window.update(cx, |shell, _, cx| shell.fixture_board_chat_readback(cx))?;
                let new_side = {
                    let b = &card["bounds"]["side-card:new"];
                    let num = |key: &str| b[key].as_f64().unwrap_or(f64::NAN) as f32;
                    anyhow::ensure!(b.is_object(), "the card's New side chat painted: {}", card["bounds"]);
                    (num("x") + num("w") / 2.0, num("y") + num("h") / 2.0)
                };
                click(window, cx, new_side, MouseButton::Left).await?;
                // The fixture merges registry rows itself (the chats watch is
                // off), so the new sibling's row is merged while it opens.
                let mut created = Value::Null;
                for _ in 0..120 {
                    merge_engine_chats(&reader, &state, cx).await?;
                    settle(cx, 150).await;
                    created = wb(window, cx)?;
                    if created["creatingChild"] == false
                        && created["pendingMutations"] == 0
                        && pane_of(&created, CHILD_A).is_some_and(|pane| tabs_of(&pane).len() == 2)
                    {
                        break;
                    }
                }
                let new_child = pane_of(&created, CHILD_A)
                    .map(|pane| tabs_of(&pane))
                    .unwrap_or_default()
                    .into_iter()
                    .find(|id| id != CHILD_A)
                    .ok_or_else(|| anyhow::anyhow!("New side chat from Child A opened no tab in its group: {created} {}", window.update(cx, |shell, _, cx| shell.fixture_board_chat_readback(cx)).unwrap_or_default()["notice"]))?;
                let after_rows = merge_engine_chats(&reader, &state, cx).await?;
                anyhow::ensure!(after_rows.len() == before_rows.len() + 1, "exactly one side chat per gesture: {} -> {}", before_rows.len(), after_rows.len());
                let row = after_rows.iter().find(|chat| chat.id == new_child).cloned().unwrap();
                anyhow::ensure!(row.child_thread.as_ref().is_some_and(|child| child.parent_task_id == MAIN), "a fresh side chat requested from a side chat is a sibling under the same main (Main), never Child A's child: {row:?}");
                settle(cx, 400).await;
                let created = wb(window, cx)?;
                anyhow::ensure!(created["focusedChat"] == new_child.as_str() && role_of(&created, &new_child).as_deref() == Some("Child"), "new sibling focused, as a (fresh) side chat: {created}");
                let texts = window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(&new_child, cx))?;
                anyhow::ensure!(texts.is_empty(), "a new side chat starts empty: {texts:?}");
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
                anyhow::ensure!(layout_without_live_status(&restored["root"]) == layout_without_live_status(&saved["root"]) && restored["closed"] == saved["closed"] && restored["focusedPane"] == saved["focusedPane"], "layout survives a new Shell: {restored} vs {saved}");
                shot(window, cx, output.join("w12-reloaded.png")).await?;

                // w13. Switching main: each main has its own layout.
                select(cx, Some(MAIN2));
                let other = wait_for(window, cx, "Main 2's layout", |rb| rb["loaded"] == true && rb["main"] == MAIN2).await?;
                anyhow::ensure!(all_panes(&other).len() == 1 && tabs_of(&all_panes(&other)[0]) == [MAIN2], "another main starts alone: {other}");
                select(cx, Some(MAIN));
                let back = wait_for(window, cx, "Main's layout again", |rb| rb["loaded"] == true && rb["main"] == MAIN).await?;
                anyhow::ensure!(layout_without_live_status(&back["root"]) == layout_without_live_status(&saved["root"]), "switching back restores it: {back}");
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
                // sidebars open (failure case 26, fit-to-window): every group
                // fits inside the viewport in the saved proportions with no
                // outer scroll; composers stay inside their groups; nothing
                // is rewritten; Tab still moves between groups; a drag over
                // the sidebar drops nothing; widening shows the exact saved
                // layout.
                for _ in 0..6 {
                    draw(window, cx)?;
                    settle(cx, 60).await;
                }
                let squeezed_frame = frame(window, cx)?;
                anyhow::ensure!(squeezed_frame["rightOpen"] == true && squeezed_frame["sidebarCollapsed"] == false, "both sidebars open: {squeezed_frame}");
                // w15b just closed the alongside tab that sat first in the
                // second group: once its ghost has narrowed out, every
                // group's active tab is wholly inside its strip, before any
                // click or Tab (candidates 3/4 left the neighbour revealed
                // and the active tab a 2 px sliver: the reveal settled
                // while the ghost still held its place).
                let revealed = wait_for(window, cx, "every active tab revealed after the close", |rb| {
                    all_panes(rb).iter().all(|pane| pane["active"].as_str().is_none_or(|chat| fully_revealed(rb, chat)))
                })
                .await?;
                let focused_tab = window.update(cx, |shell, window, _| shell.fixture_workbench_focused_tab(window))?;
                progress(&output, &mut record, "w15cBeforeTab".into(), serde_json::json!({"readback": revealed, "focusedTab": focused_tab}));
                let narrow_base = wb(window, cx)?;
                anyhow::ensure!(all_panes(&narrow_base).len() >= 2, "a split layout to fit: {narrow_base}");
                let view = rect(&narrow_base["rootBounds"]);
                let scroll = &narrow_base["canvasScroll"];
                anyhow::ensure!(
                    scroll["maxX"].as_f64().unwrap_or(1.0) < 0.5
                        && scroll["maxY"].as_f64().unwrap_or(1.0) < 0.5
                        && scroll["x"].as_f64().unwrap_or(1.0).abs() < 0.5,
                    "the workbench has no outer scroll range: {narrow_base}"
                );
                for entry in narrow_base["paneBounds"].as_array().into_iter().flatten() {
                    let (x, y, w, h) = rect(&entry["bounds"]);
                    anyhow::ensure!(
                        x >= view.0 - 0.5 && x + w <= view.0 + view.2 + 0.5 && y >= view.1 - 0.5 && y + h <= view.1 + view.3 + 0.5 && w > 0.0,
                        "every group fits inside the viewport: {entry} in {narrow_base}"
                    );
                }
                if narrow_base["root"]["kind"] == "split" && narrow_base["root"]["axis"] == "horizontal" {
                    let mut first = Vec::new();
                    panes(&narrow_base["root"]["first"], &mut first);
                    let first_right = first
                        .iter()
                        .filter_map(|pane| pane_rect(&narrow_base, pane["id"].as_str().unwrap_or_default()).ok())
                        .map(|(x, _, w, _)| x + w)
                        .fold(view.0, f32::max);
                    let ratio = narrow_base["root"]["ratio"].as_f64().unwrap_or(0.5) as f32;
                    anyhow::ensure!(
                        ((first_right - view.0) / view.2 - ratio).abs() < 0.03,
                        "groups share the width in the saved proportion {ratio}: first side ends at {first_right} in {view:?}"
                    );
                }
                for (chat, width) in narrow_base["composerWidths"].as_object().into_iter().flatten() {
                    let pane = all_panes(&narrow_base).into_iter().find(|pane| pane["active"] == chat.as_str());
                    if let Some(pane) = pane {
                        let (_, _, pw, _) = pane_rect(&narrow_base, pane["id"].as_str().unwrap_or_default())?;
                        anyhow::ensure!(width.as_f64().unwrap_or(f64::MAX) as f32 <= pw + 0.5, "{chat}'s composer stays inside its group: {width} vs {pw} in {narrow_base}");
                    }
                }
                assert_contained(&narrow_base, "fitted canvas")?;
                shot(window, cx, output.join("w15c-fitted-canvas.png")).await?;
                // Focus into the canvas (a press on a group), then Tab into
                // another group: focus moves, everything still fits.
                click(window, cx, (view.0 + view.2 / 4.0, view.1 + view.3 / 2.0), MouseButton::Left).await?;
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
                let focused_rb = wb(window, cx)?;
                // Evidence for the drag below: which tab chip (if any) holds
                // keyboard focus against each group's active tab.
                let focused_tab = window.update(cx, |shell, window, _| shell.fixture_workbench_focused_tab(window))?;
                progress(&output, &mut record, "w15cAfterTab".into(), serde_json::json!({"readback": focused_rb, "focusedTab": focused_tab, "targetPane": target_pane}));
                anyhow::ensure!(focused_rb["canvasScroll"]["x"].as_f64().unwrap_or(1.0).abs() < 0.5, "moving focus scrolls nothing: {focused_rb}");
                shot(window, cx, output.join("w15c-focused-group.png")).await?;
                // A tab dragged out over the left sidebar previews and drops
                // nothing.
                let dragged = all_panes(&focused_rb).into_iter().find(|pane| pane["id"] == target_pane.as_str()).and_then(|pane| pane["active"].as_str().map(str::to_string)).ok_or_else(|| anyhow::anyhow!("the focused group's active tab"))?;
                let before_drag = wb(window, cx)?;
                let sidebar_point = (120.0, HEIGHT / 2.0);
                drag_hold(window, cx, tab_center(&before_drag, &dragged)?, sidebar_point).await?;
                let over_sidebar = wb(window, cx)?;
                anyhow::ensure!(over_sidebar["preview"].is_null(), "no drop preview over the sidebar: {over_sidebar}");
                release(window, cx, sidebar_point).await?;
                let after_sidebar = wb(window, cx)?;
                anyhow::ensure!(after_sidebar["root"] == before_drag["root"] && after_sidebar["revision"] == before_drag["revision"], "a release over the sidebar drops nothing: {after_sidebar}");
                let before_widen = wb(window, cx)?;
                // Widen: a fresh full-size window shows the exact saved
                // layout, with no write from any of this.
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
                    serde_json::json!({"narrow": narrow_base, "focused": focused_rb, "overSidebar": after_sidebar, "widened": widened}),
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
                let started = window.update(cx, |shell, _, cx| shell.fixture_workbench_action("New side chat (fresh)", cx))?;
                anyhow::ensure!(started, "the palette offers New thread");
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

                // w16c. Sidebar over a fitting split (failure case 28,
                // fit-to-window): with the right sidebar closed the balanced
                // split fits; focus its right group, then open the sidebar:
                // both groups shrink to fit in their saved proportions, focus
                // stays, nothing scrolls and nothing is written.
                set_right_sidebar(window, cx, false).await?;
                let wide_fit = wb(window, cx)?;
                anyhow::ensure!(wide_fit["canvasScroll"]["maxX"].as_f64().unwrap_or(1.0) < 0.5, "the split fits without the sidebar: {wide_fit}");
                assert_contained(&wide_fit, "a fitting split")?;
                let right_active = all_panes(&wide_fit).into_iter().find(|pane| pane["id"] == right_group.as_str()).and_then(|pane| pane["active"].as_str().map(str::to_string)).ok_or_else(|| anyhow::anyhow!("the right group has an active tab: {wide_fit}"))?;
                click_tab(window, cx, &right_active).await?;
                let before_sidebar = {
                    let group = right_group.clone();
                    wait_for(window, cx, "the right group focus committed", move |rb| rb["focusedPane"] == group.as_str() && rb["pendingMutations"] == 0).await?
                };
                let wide_left = pane_rect(&wb(window, cx)?, &main_group)?;
                let wide_right = pane_rect(&wb(window, cx)?, &right_group)?;
                set_right_sidebar(window, cx, true).await?;
                let covered = wb(window, cx)?;
                anyhow::ensure!(
                    covered["canvasScroll"]["maxX"].as_f64().unwrap_or(1.0) < 0.5 && covered["canvasScroll"]["x"].as_f64().unwrap_or(1.0).abs() < 0.5,
                    "the split still fits with the sidebar open (no outer scroll): {covered}"
                );
                anyhow::ensure!(covered["focusedPane"] == right_group.as_str(), "focus did not move: {covered}");
                anyhow::ensure!(covered["root"] == before_sidebar["root"] && covered["revision"] == before_sidebar["revision"], "opening the sidebar writes nothing: {before_sidebar} -> {covered}");
                let view = rect(&covered["rootBounds"]);
                let left_pane = pane_rect(&covered, &main_group)?;
                let right_pane = pane_rect(&covered, &right_group)?;
                anyhow::ensure!(
                    left_pane.0 >= view.0 - 0.5 && right_pane.0 + right_pane.2 <= view.0 + view.2 + 0.5,
                    "both groups fit inside the viewport: {covered}"
                );
                let share = |l: (f32, f32, f32, f32), r: (f32, f32, f32, f32)| l.2 / (l.2 + r.2);
                anyhow::ensure!(
                    (share(left_pane, right_pane) - share(wide_left, wide_right)).abs() < 0.02,
                    "the groups keep their saved proportions: {covered}"
                );
                assert_contained(&covered, "fitted split under the sidebar")?;
                anyhow::ensure!(covered["overflowFades"] == serde_json::json!([false, false, false, false]), "no overflow fades: {covered}");
                shot(window, cx, output.join("w16c-sidebar-fits.png")).await?;
                // Explicit focus: the left group, then the right one again.
                click_tab(window, cx, MAIN).await?;
                {
                    let group = main_group.clone();
                    wait_for(window, cx, "Main's group focused", move |rb| rb["focusedPane"] == group.as_str()).await?;
                }
                click_tab(window, cx, &right_active).await?;
                let revealed_right = {
                    let group = right_group.clone();
                    wait_for(window, cx, "an explicit focus selects the right group", move |rb| {
                        let view = rect(&rb["rootBounds"]);
                        rb["focusedPane"] == group.as_str()
                            && rb["canvasScroll"]["x"].as_f64().unwrap_or(1.0).abs() < 0.5
                            && pane_rect(rb, &group).is_ok_and(|pane| pane.0 >= view.0 - 0.5 && pane.0 + pane.2 <= view.0 + view.2 + 0.5)
                    })
                    .await?
                };
                assert_contained(&revealed_right, "explicitly focused right group")?;
                shot(window, cx, output.join("w16d-explicit-focus.png")).await?;

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
                // Its known profile is back: a layout write is accepted again
                // (the no-profile refusal above must not linger).
                if let Some(target) = all_panes(&back_in_a)
                    .into_iter()
                    .flat_map(|pane| tabs_of(&pane))
                    .find(|id| back_in_a["focusedChat"].as_str() != Some(id.as_str()))
                {
                    click_tab(window, cx, &target).await?;
                    let revision = back_in_a["revision"].as_u64().unwrap_or(0);
                    let written = wait_for(window, cx, "profile A accepts a layout write", move |rb| rb["revision"].as_u64().unwrap_or(0) > revision && rb["pendingMutations"] == 0).await?;
                    anyhow::ensure!(written["focusedChat"] == target.as_str(), "the write landed in profile A: {written}");
                }
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

                // w18. Persistent forks (failure case 29): New side chat
                // makes an ordinary thread. It opens as a Side tab under its
                // exact source, published with its real project, titled by
                // its own row, with a live composer and no End, lease,
                // countdown or read-only state; it lists in Main's Related
                // library only, until it is explicitly placed in the sidebar.
                window.update(cx, |shell, _, cx| {
                    shell.fixture_open_right_pane(cx);
                    shell.fixture_open_side_chats(cx);
                })?;
                settle(cx, 600).await;
                let fork_one = create_fork(window, cx, &reader, &state, &[], "the first fork").await?;
                let forked = wb(window, cx)?;
                let tab = tab_json(&forked, &fork_one).ok_or_else(|| anyhow::anyhow!("the fork's tab: {forked}"))?;
                anyhow::ensure!(tab["sideParentId"] == MAIN && tab["parent"] == MAIN && tab["role"] == "Side", "the Side tab keeps its exact source: {tab}");
                let row = chat_row(cx, &state, &fork_one).ok_or_else(|| anyhow::anyhow!("the fork's ordinary row is published"))?;
                anyhow::ensure!(row.space_id.is_some(), "the fork is published with its real project (spaceId): {row:?}");
                if let Some(title) = row.title.as_deref().filter(|title| !title.trim().is_empty()) {
                    anyhow::ensure!(tab["title"] == one_line(title).as_str(), "the tab uses the fork's own chat title: {tab} vs {title}");
                }
                assert_ordinary_fork(window, cx, &fork_one, true, "the new fork").await?;
                let listed = related(window, cx)?;
                anyhow::ensure!(!has(&listed["sidebarOrder"], &fork_one), "an unplaced fork is not a sidebar row: {listed}");
                anyhow::ensure!(tree_has(&listed, MAIN, &fork_one, "Side chat", None), "Main's Related library lists the fork: {listed}");
                // The explicit placement (the Related row's drop path) makes it
                // a sidebar row while it stays under its source.
                window.update(cx, |shell, _, cx| shell.fixture_place_in_sidebar(&fork_one, "side", cx))?;
                let mut listed = related(window, cx)?;
                for _ in 0..40 {
                    if has(&listed["sidebarOrder"], &fork_one) {
                        break;
                    }
                    settle(cx, 100).await;
                    listed = related(window, cx)?;
                }
                anyhow::ensure!(
                    has(&listed["sidebarOrder"], &fork_one) && tree_has(&listed, MAIN, &fork_one, "Side chat", None),
                    "an explicitly placed fork is a sidebar row and stays under its source: {listed}"
                );
                shot(window, cx, output.join("w18-fork-tab.png")).await?;

                // w19. Its own draft and run survive closing and reopening
                // the tab: Close is not End, and nothing stops the run.
                const FORK_DRAFT: &str = "fork draft kept across close";
                const FORK_PROMPT: &str = "Fork: keep investigating the release notes";
                let main_draft = window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))?;
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_workbench_type(&fork_one, FORK_DRAFT, false, cx))?, "the fork's own composer takes a draft");
                settle(cx, 600).await;
                close_and_reopen(window, cx, &fork_one, MAIN, "the drafted fork").await?;
                let mut draft = None;
                for _ in 0..40 {
                    draft = window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(&fork_one, cx))?;
                    if draft.as_deref() == Some(FORK_DRAFT) {
                        break;
                    }
                    settle(cx, 100).await;
                }
                anyhow::ensure!(draft.as_deref() == Some(FORK_DRAFT), "the fork's draft survives close and reopen: {draft:?}");
                let main_draft_after = window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))?;
                anyhow::ensure!(main_draft_after == main_draft, "Main's draft is untouched: {main_draft:?} -> {main_draft_after:?}");
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_workbench_type(&fork_one, FORK_PROMPT, true, cx))?, "the fork sends from its own composer");
                let fork_turn = wait_active_turn(cx, &reader, &fork_one, "the fork's run").await?;
                window.update(cx, |shell, _, cx| shell.fixture_workbench_close(&fork_one, cx))?;
                {
                    let id = fork_one.clone();
                    wait_for(window, cx, "the running fork's tab closed", move |rb| occurrences(rb, &id) == 0).await?;
                }
                let running = task_snapshot(&reader, &fork_one).await?;
                anyhow::ensure!(active_turn(&running).as_deref() == Some(fork_turn.as_str()), "closing the tab does not stop the fork's run: {running}");
                close_and_reopen(window, cx, &fork_one, MAIN, "the running fork").await?;
                let mut fork_output = Vec::new();
                for _ in 0..200 {
                    settle(cx, 100).await;
                    fork_output = window.update(cx, |shell, _, cx| shell.fixture_workbench_turn_after(&fork_one, FORK_PROMPT, cx))?;
                    if fork_output.iter().any(|entry| entry["role"] == "Assistant" && entry["status"] == "Complete" && entry["text"].as_str().is_some_and(|text| text.contains(MOCK_ANSWER))) {
                        break;
                    }
                }
                anyhow::ensure!(fork_output.iter().any(|entry| entry["role"] == "Assistant" && entry["status"] == "Complete"), "the fork's run completes after close and reopen: {fork_output:?}");
                assert_ordinary_fork(window, cx, &fork_one, true, "the reopened fork").await?;
                shot(window, cx, output.join("w19-fork-run-survives-close.png")).await?;
                progress(&output, &mut record, "forkDraftAndRun".into(), serde_json::json!({"fork": fork_one, "turn": fork_turn, "output": fork_output}));

                // w20. A second fork is independent: its own draft, model
                // keys and Stop. Escape in it stops only it.
                let fork_two = create_fork(window, cx, &reader, &state, std::slice::from_ref(&fork_one), "the second fork").await?;
                anyhow::ensure!(fork_two != fork_one && occurrences(&wb(window, cx)?, &fork_one) == 1, "two forks, two tabs");
                window.update(cx, |shell, _, cx| {
                    shell.fixture_workbench_type(&fork_one, "draft one", false, cx);
                    shell.fixture_workbench_type(&fork_two, "draft two", false, cx);
                })?;
                settle(cx, 400).await;
                let drafts = window.update(cx, |shell, _, cx| (shell.fixture_workbench_draft(&fork_one, cx), shell.fixture_workbench_draft(&fork_two, cx)))?;
                anyhow::ensure!(drafts == (Some(String::from("draft one")), Some(String::from("draft two"))), "each fork keeps its own draft: {drafts:?}");
                activate(window, cx)?;
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(&fork_two, window, cx))?, "focus the second fork's composer");
                draw(window, cx)?;
                settle(cx, 150).await;
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_keyboard_chat(window, cx))?.as_deref() == Some(fork_two.as_str()), "keys reach the second fork");
                key(window, cx, "cmd-/")?;
                settle(cx, 300).await;
                let pickers = window.update(cx, |shell, _, cx| {
                    (shell.fixture_workbench_model(&fork_one, cx), shell.fixture_workbench_model(&fork_two, cx), shell.fixture_workbench_model(MAIN, cx))
                })?;
                anyhow::ensure!(pickers.1.as_ref().is_some_and(|(_, open)| *open) && pickers.0.as_ref().is_some_and(|(_, open)| !*open) && pickers.2.as_ref().is_some_and(|(_, open)| !*open), "Cmd-/ opens only the focused fork's model picker: {pickers:?}");
                // The real Escape key closes the open popover (its pane's own,
                // like Main's) and never reaches the stop path underneath.
                let escapes_before = window.update(cx, |shell, _, _| shell.fixture_workbench_escape_log().len())?;
                key(window, cx, "escape")?;
                settle(cx, 300).await;
                draw(window, cx)?;
                let after_escape = window.update(cx, |shell, window, cx| {
                    (
                        shell.fixture_overlay_owns_keyboard(cx),
                        shell.fixture_workbench_model(&fork_two, cx),
                        shell.fixture_workbench_keyboard_chat(window, cx),
                        shell.fixture_workbench_escape_log(),
                    )
                })?;
                anyhow::ensure!(!after_escape.0 && after_escape.1.as_ref().is_some_and(|(_, open)| !*open), "Escape closed the picker: {after_escape:?}");
                anyhow::ensure!(after_escape.3.len() == escapes_before, "an Escape that closes a popover stops nothing: {after_escape:?}");
                anyhow::ensure!(after_escape.2.as_deref() == Some(fork_two.as_str()), "focus returns to the second fork's composer: {after_escape:?}");
                const RACE_ONE: &str = "Fork one: keep going";
                const RACE_TWO: &str = "Fork two: keep going";
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_workbench_type(&fork_one, RACE_ONE, true, cx) && shell.fixture_workbench_type(&fork_two, RACE_TWO, true, cx))?, "both forks send");
                let turn_one = wait_active_turn(cx, &reader, &fork_one, "fork one's run").await?;
                let turn_two = wait_active_turn(cx, &reader, &fork_two, "fork two's run").await?;
                let live = |indicator: &str| matches!(indicator, "Working" | "AwaitingApproval" | "AwaitingInput");
                let mut both_live = false;
                for _ in 0..80 {
                    let (one, two) = cx.update(|cx| (state.read(cx).fixture_session_indicator(&fork_one), state.read(cx).fixture_session_indicator(&fork_two)));
                    if live(&one.0) && live(&two.0) {
                        both_live = true;
                        break;
                    }
                    settle(cx, 100).await;
                }
                anyhow::ensure!(both_live, "the sessions feed shows both forks running ({turn_one} / {turn_two})");
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(&fork_two, window, cx))?, "refocus the second fork");
                draw(window, cx)?;
                settle(cx, 120).await;
                key(window, cx, "escape")?;
                let escapes = window.update(cx, |shell, _, _| shell.fixture_workbench_escape_log())?;
                anyhow::ensure!(escapes.last().is_some_and(|(chat, outcome)| chat.as_deref() == Some(fork_two.as_str()) && *outcome == "interrupt"), "Escape interrupts the second fork: {escapes:?}");
                let mut stop = Value::Null;
                for _ in 0..80 {
                    settle(cx, 100).await;
                    stop = window.update(cx, |shell, _, cx| shell.fixture_workbench_last_stop(&fork_two, cx))?;
                    if !stop.is_null() {
                        break;
                    }
                }
                anyhow::ensure!(stop["taskId"] == fork_two.as_str() && matches!(stop["status"].as_str(), Some("pending" | "applied")), "the second fork's own stop was accepted: {stop}");
                let other_stop = window.update(cx, |shell, _, cx| (shell.fixture_workbench_last_stop(&fork_one, cx), shell.fixture_workbench_last_stop(MAIN, cx)))?;
                anyhow::ensure!(other_stop.0.is_null() && other_stop.1.is_null(), "neither the first fork nor Main sent a stop: {other_stop:?}");
                let mut one_done = Vec::new();
                for _ in 0..200 {
                    settle(cx, 100).await;
                    one_done = window.update(cx, |shell, _, cx| shell.fixture_workbench_turn_after(&fork_one, RACE_ONE, cx))?;
                    if one_done.iter().any(|entry| entry["role"] == "Assistant" && entry["status"] == "Complete") {
                        break;
                    }
                }
                anyhow::ensure!(one_done.iter().any(|entry| entry["role"] == "Assistant" && entry["status"] == "Complete"), "the first fork completes its own run: {one_done:?}");
                let two_out = window.update(cx, |shell, _, cx| shell.fixture_workbench_turn_after(&fork_two, RACE_TWO, cx))?;
                anyhow::ensure!(!two_out.iter().any(|entry| entry["role"] == "Assistant" && entry["status"] == "Complete"), "the stopped fork has no completed answer: {two_out:?}");
                shot(window, cx, output.join("w20-independent-forks.png")).await?;
                progress(&output, &mut record, "independentForks".into(), serde_json::json!({"one": fork_one, "two": fork_two, "stop": stop, "escapes": format!("{escapes:?}")}));

                // w20b. One level of side chats, and orphans. A side chat
                // neither forks nor carries its context: the engine refuses a
                // fork of a fork and a fork of Child A (creating nothing);
                // asking a side chat for a context fork in the UI shows why
                // instead of a dialog; its `+` and its tab menu offer only New
                // side chat (a fresh sibling) and Hand off, with the reason
                // for the missing fork. A fork of a main chat survives that
                // main chat's deletion as a thread (a sidebar row, openable,
                // its source named but not linked).
                let fork_via_rpc = |parent: String| {
                    let reader = reader.clone();
                    async move {
                        let created = reader
                            .call(
                                luna_rpc::methods::CREATE_SIDE_CHAT,
                                serde_json::json!({"parentId": parent, "requestId": uuid::Uuid::new_v4().to_string(), "forceSnapshot": false}),
                            )
                            .await?;
                        anyhow::Ok(created["meta"]["id"].as_str().unwrap_or_default().to_string())
                    }
                };
                // A disposable main chat with history, made through the
                // engine (the UI list is fixture-managed: merged explicitly).
                let seed_main = |id: &'static str, title: &'static str| {
                    let reader = reader.clone();
                    let workspace = &automation_workspace;
                    let docs = &queue_doc_host;
                    let device = queue_device.clone();
                    let cwd = automation_cwd.to_string_lossy().into_owned();
                    async move {
                        reader
                            .call(
                                "Mutate",
                                serde_json::json!({
                                    "op": "createChat",
                                    "chatId": id,
                                    "spaceId": PROJECT,
                                    "cwd": cwd,
                                    "config": serde_json::to_value(config())?,
                                }),
                            )
                            .await?;
                        workspace.rename_chat(id, title)?;
                        let at = Utc::now().timestamp_millis();
                        let doc = docs.open(id)?;
                        doc.doc().push_message(&text_entry(&format!("{id}-prompt"), MessageRole::User, &format!("{title} (fixture)"), &device, at - 60_000))?;
                        doc.doc().push_message(&text_entry(&format!("{id}-answer"), MessageRole::Assistant, "Noted.", &device, at))?;
                        anyhow::Ok(())
                    }
                };
                let rows_before_refusals = merge_engine_chats(&reader, &state, cx).await?.len();
                let fork_of_fork = fork_via_rpc(fork_two.clone()).await;
                let fork_of_child = fork_via_rpc(CHILD_A.to_string()).await;
                let rows_after_refusals = merge_engine_chats(&reader, &state, cx).await?.len();
                let refusals = serde_json::json!({
                    "forkOfFork": fork_of_fork.as_ref().map(|id| id.clone()).unwrap_or_else(|error| format!("refused: {error:#}")),
                    "forkOfChild": fork_of_child.as_ref().map(|id| id.clone()).unwrap_or_else(|error| format!("refused: {error:#}")),
                });
                anyhow::ensure!(
                    fork_of_fork.is_err() && fork_of_child.is_err() && rows_after_refusals == rows_before_refusals,
                    "the engine refuses a fork of a side chat (a fork, Child A) and creates nothing: {refusals} ({rows_before_refusals} -> {rows_after_refusals} chats)"
                );
                // The UI path (chat menu / message Fork) explains instead.
                window.update(cx, |shell, _, cx| shell.fixture_board_chat_probe(true, cx))?;
                let mut fork_notices = Vec::new();
                for side in [fork_two.as_str(), CHILD_A] {
                    window.update(cx, |shell, _, cx| shell.fixture_chat_action(side, "fork", cx))?;
                    settle(cx, 300).await;
                    draw(window, cx)?;
                    let notice = window.update(cx, |shell, _, cx| shell.fixture_board_chat_readback(cx))?["notice"].clone();
                    anyhow::ensure!(
                        notice.as_str().is_some_and(|notice| notice.starts_with("Side chats can't fork or carry their context")),
                        "a context fork asked of a side chat ({side}) shows the refusal: {notice}"
                    );
                    fork_notices.push(notice);
                }
                anyhow::ensure!(merge_engine_chats(&reader, &state, cx).await?.len() == rows_before_refusals, "a refused fork creates nothing");
                // The side chat's `+`: its own card, never the chooser.
                click_tab(window, cx, &fork_two).await?;
                let at_fork = wb(window, cx)?;
                let fork_group = pane_of(&at_fork, &fork_two).and_then(|pane| pane["id"].as_str().map(str::to_string)).ok_or_else(|| anyhow::anyhow!("the second fork's group: {at_fork}"))?;
                click(window, cx, plus_center(&at_fork, &fork_group)?, MouseButton::Left).await?;
                settle(cx, 300).await;
                draw(window, cx)?;
                let card = window.update(cx, |shell, _, cx| shell.fixture_board_chat_readback(cx))?;
                let chooser = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                anyhow::ensure!(
                    card["sideCard"]["sideId"] == fork_two.as_str()
                        && card["sideCard"]["mainId"] == MAIN
                        && card["marks"]["side-card"] == serde_json::json!(["New side chat", "Hand off to a new main chat…"])
                        && chooser["open"] == false,
                    "a fork's `+` offers only New side chat and Hand off (no With context): {} {} {chooser}",
                    card["sideCard"],
                    card["marks"]["side-card"]
                );
                shot(window, cx, output.join("w20b-side-chat-card.png")).await?;
                key(window, cx, "escape")?;
                settle(cx, 300).await;
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_board_chat_readback(cx))?["sideCard"].is_null(), "Escape closes the card");
                // The tab menu: the same two creations, the reason for the
                // missing fork, and no reattach.
                let at_fork = wb(window, cx)?;
                click(window, cx, tab_center(&at_fork, &fork_two)?, MouseButton::Right).await?;
                settle(cx, 300).await;
                draw(window, cx)?;
                let tab_menu = window.update(cx, |shell, _, cx| shell.fixture_board_chat_readback(cx))?;
                let rows: Vec<String> = tab_menu["marks"][format!("menu:tab:{fork_two}").as_str()]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|row| row.as_str().map(str::to_string))
                    .collect();
                anyhow::ensure!(
                    rows.iter().any(|row| row == "New side chat")
                        && rows.iter().any(|row| row == "Hand off to a new main chat…")
                        && rows.iter().any(|row| row == "No context forks from a side chat")
                        && !rows.iter().any(|row| {
                            let row = row.to_lowercase();
                            row.contains("with context") || row.contains("reattach") || row.contains("move here")
                        }),
                    "a fork's tab menu offers New side chat and Hand off and says why there is no fork: {rows:?}"
                );
                shot(window, cx, output.join("w20b-side-chat-menu.png")).await?;
                // A press elsewhere closes the menu (focusing the same tab).
                let fork_tab = tab_center(&wb(window, cx)?, &fork_two)?;
                click(window, cx, fork_tab, MouseButton::Left).await?;
                settle(cx, 300).await;
                // Orphans: a fork of a disposable main chat, placed in the
                // sidebar, then its source deleted.
                const FORK_SOURCE: &str = "e0e0e0e0-0000-4000-8000-0000000000d1";
                seed_main(FORK_SOURCE, "Disposable fork source").await?;
                merge_engine_chats(&reader, &state, cx).await?;
                let orphan = fork_via_rpc(FORK_SOURCE.to_string()).await?;
                anyhow::ensure!(!orphan.is_empty(), "a fork of a main chat");
                merge_engine_chats(&reader, &state, cx).await?;
                window.update(cx, |shell, _, cx| shell.fixture_workbench_open(&orphan, cx))?;
                {
                    let id = orphan.clone();
                    wait_for(window, cx, "the fork opens as a tab in Main's workbench", move |rb| occurrences(rb, &id) == 1).await?;
                }
                let mut nested = Value::Null;
                for _ in 0..60 {
                    merge_engine_chats(&reader, &state, cx).await?;
                    settle(cx, 150).await;
                    nested = related(window, cx)?;
                    if tree_has(&nested, MAIN, &orphan, "Side chat", Some(FORK_SOURCE)) {
                        break;
                    }
                }
                anyhow::ensure!(tree_has(&nested, MAIN, &orphan, "Side chat", Some(FORK_SOURCE)), "a fork in Main's workbench lists in Main's library under its actual source: {nested}");
                // Placed explicitly (forks start in Related only), so its
                // sidebar row can be checked across its source's deletion.
                anyhow::ensure!(!has(&nested["sidebarOrder"], &orphan), "an unplaced fork is not a sidebar row: {nested}");
                window.update(cx, |shell, _, cx| shell.fixture_place_in_sidebar(&orphan, "side", cx))?;
                let mut placed_nested = related(window, cx)?;
                for _ in 0..40 {
                    if has(&placed_nested["sidebarOrder"], &orphan) {
                        break;
                    }
                    settle(cx, 100).await;
                    placed_nested = related(window, cx)?;
                }
                anyhow::ensure!(has(&placed_nested["sidebarOrder"], &orphan), "the fork is placed in the sidebar: {placed_nested}");
                window.update(cx, |shell, _, cx| shell.fixture_chat_action(FORK_SOURCE, "delete", cx))?;
                let mut source_gone = false;
                for _ in 0..80 {
                    settle(cx, 150).await;
                    if !engine_has(&reader, FORK_SOURCE).await? {
                        source_gone = true;
                        break;
                    }
                }
                // The merge only adds rows; drop the deleted one as the live
                // chats watch does.
                cx.update(|cx| {
                    state.update(cx, |state, cx| {
                        state.chats.retain(|chat| chat.id != FORK_SOURCE);
                        cx.notify();
                    })
                });
                anyhow::ensure!(source_gone, "the fork's source (a main chat) is deleted");
                settle(cx, 300).await;
                let orphaned = related(window, cx)?;
                anyhow::ensure!(has(&orphaned["sidebarOrder"], &orphan), "the placed orphan fork is still a sidebar row: {orphaned}");
                anyhow::ensure!(orphaned["forkSources"][orphan.as_str()] == FORK_SOURCE, "its source stays a link to the deleted chat: {orphaned}");
                draw(window, cx)?;
                settle(cx, 300).await;
                let orphan_strip = window.update(cx, |shell, _, cx| shell.fixture_workbench_strip(&orphan, cx))?;
                anyhow::ensure!(orphan_strip["parentLinked"] == false && orphan_strip["parentTitle"] == "a deleted chat", "an orphan names its deleted source without a link: {orphan_strip}");
                assert_ordinary_fork(window, cx, &orphan, false, "the orphan fork").await?;
                anyhow::ensure!(tree_has(&related(window, cx)?, MAIN, &orphan, "Side chat", Some(FORK_SOURCE)), "Main's library still lists the orphan (it is in Main's workbench)");
                shot(window, cx, output.join("w20b-nested-and-orphan.png")).await?;
                progress(&output, &mut record, "sideChatLimitsAndOrphan".into(), serde_json::json!({"refusals": refusals, "forkNotices": fork_notices, "card": card["sideCard"], "tabMenu": rows, "library": nested, "orphaned": orphaned, "orphanStrip": orphan_strip}));

                // w20c. Ordinary chat operations on forks: archive (then
                // the model shortcut cannot fall back to Main), the fork as
                // its own workbench Main (a fresh side chat asked of it is a
                // sibling under Main, never its child), sidebar placement,
                // and legacy device-local marks migrated to the fork's
                // synced flags before they are cleared.
                click_tab(window, cx, &fork_one).await?;
                {
                    let id = fork_one.clone();
                    wait_for(window, cx, "the first fork focused", move |rb| rb["focusedChat"] == id.as_str()).await?;
                }
                window.update(cx, |shell, _, cx| shell.fixture_chat_action(&fork_one, "archive", cx))?;
                wait_row(cx, &reader, &state, &fork_one, "the fork archived", |row| row.archived).await?;
                draw(window, cx)?;
                settle(cx, 300).await;
                anyhow::ensure!(!window.update(cx, |shell, _, cx| shell.fixture_workbench_tab_has_composer(&fork_one, cx))?, "an archived fork's tab offers no composer");
                activate(window, cx)?;
                key(window, cx, "cmd-/")?;
                settle(cx, 300).await;
                draw(window, cx)?;
                let after_shortcut = wb(window, cx)?;
                let main_picker = window.update(cx, |shell, _, cx| shell.fixture_workbench_model(MAIN, cx))?;
                anyhow::ensure!(main_picker.as_ref().is_some_and(|(_, open)| !*open), "Cmd-/ on an archived fork does not open Main's model picker: {main_picker:?}");
                anyhow::ensure!(!window.update(cx, |shell, _, cx| shell.fixture_overlay_owns_keyboard(cx))?, "no picker opened anywhere");
                anyhow::ensure!(after_shortcut["focusedChat"] == fork_one.as_str() && after_shortcut["footer"]["chat"] == fork_one.as_str(), "the archived fork's group and frame row stay focused: {after_shortcut}");
                let chip = window.update(cx, |shell, window, _| shell.fixture_workbench_focused_tab(window))?;
                anyhow::ensure!(chip.as_deref() == Some(fork_one.as_str()), "keyboard focus stays on the archived fork's tab: {chip:?}");
                shot(window, cx, output.join("w20c-archived-fork.png")).await?;
                window.update(cx, |shell, _, cx| shell.fixture_chat_action(&fork_one, "unarchive", cx))?;
                wait_row(cx, &reader, &state, &fork_one, "the fork unarchived", |row| !row.archived).await?;
                // Placement: a fork's Related row dropped on the sidebar is
                // placed there like any chat (no refusal).
                anyhow::ensure!(!has(&related(window, cx)?["sidebarOrder"], &fork_two), "an unplaced fork is not a sidebar row");
                window.update(cx, |shell, _, cx| shell.fixture_place_in_sidebar(&fork_two, "side", cx))?;
                settle(cx, 300).await;
                let placed = related(window, cx)?;
                anyhow::ensure!(!placed["notice"].as_str().is_some_and(|notice| notice.contains("stay with")), "a fork is not refused the sidebar: {placed}");
                anyhow::ensure!(has(&placed["sidebarOrder"], &fork_two), "the fork is a sidebar row: {placed}");
                // Legacy marks: archive intent on one fork, a pin on another.
                window.update(cx, |shell, _, cx| {
                    shell.fixture_seed_side_marks(&fork_two, luna_ui::settings::SideChatMarks { pinned_at_ms: Some(1), settled: false, archived: true }, cx);
                })?;
                wait_row(cx, &reader, &state, &fork_two, "the legacy archive mark is applied", |row| row.archived).await?;
                window.update(cx, |shell, _, cx| {
                    shell.fixture_seed_side_marks(&orphan, luna_ui::settings::SideChatMarks { pinned_at_ms: Some(2), settled: false, archived: false }, cx);
                })?;
                wait_row(cx, &reader, &state, &orphan, "the legacy pin mark is applied", |row| row.pinned_order.is_some()).await?;
                let mut marks = Value::Null;
                for _ in 0..60 {
                    settle(cx, 150).await;
                    marks = related(window, cx)?["sideMarks"].clone();
                    if marks.get(fork_two.as_str()).is_none() && marks.get(orphan.as_str()).is_none() {
                        break;
                    }
                }
                anyhow::ensure!(marks.get(fork_two.as_str()).is_none() && marks.get(orphan.as_str()).is_none(), "legacy marks clear only after the rows read back their flags: {marks}");
                // The fork as its own workbench Main: its help keeps its
                // lineage (only a parentless Main reads as plain Main).
                window.update(cx, |shell, _, cx| shell.fixture_workbench_open_as_main(&fork_one, cx))?;
                let fork_main = {
                    let id = fork_one.clone();
                    wait_for(window, cx, "the fork is the workbench Main", move |rb| rb["main"] == id.as_str() && rb["loaded"] == true).await?
                };
                const MAIN_NOTE: &str = "It is this workbench's Main and stays open.";
                let fork_main_help = tab_json(&fork_main, &fork_one).map(|tab| tab["help"].clone()).unwrap_or(Value::Null);
                anyhow::ensure!(
                    fork_main_help["title"].as_str().is_some_and(|title| title.starts_with("Side chat of ") && title != "Side chat of a deleted chat")
                        && fork_main_help["details"].as_str().is_some_and(|details| details.starts_with("Side chats carry the context of the parent thread.") && details.ends_with(MAIN_NOTE)),
                    "a Side chat as its own Main keeps its Side chat help: {fork_main_help}"
                );
                shot(window, cx, output.join("w20c-fork-as-main.png")).await?;
                // Even as its own workbench Main, the fork is a side chat: a
                // fresh side chat asked of it (the palette's New side chat,
                // the same path as a split or an empty group's button) is a
                // sibling under Main, revealed in Main's workbench.
                let known_before: std::collections::HashSet<String> = merge_engine_chats(&reader, &state, cx).await?.into_iter().map(|row| row.id).collect();
                let rows_before_sibling = known_before.len();
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_workbench_action("New side chat (fresh)", cx))?, "the palette offers New side chat");
                let mut sibling = None;
                let mut with_sibling = Value::Null;
                for _ in 0..120 {
                    let rows = merge_engine_chats(&reader, &state, cx).await?;
                    settle(cx, 150).await;
                    with_sibling = wb(window, cx)?;
                    if let Some(row) = rows.iter().find(|row| {
                        !known_before.contains(&row.id) && with_sibling["focusedChat"] == row.id.as_str() && with_sibling["pendingMutations"] == 0
                    }) {
                        sibling = Some(row.clone());
                        break;
                    }
                }
                let sibling = sibling.ok_or_else(|| anyhow::anyhow!("New side chat from a fork revealed no sibling: {with_sibling} {}", window.update(cx, |shell, _, cx| shell.fixture_board_chat_readback(cx)).unwrap_or_default()["notice"]))?;
                let fork_child = sibling.id.clone();
                anyhow::ensure!(
                    merge_engine_chats(&reader, &state, cx).await?.len() == rows_before_sibling + 1
                        && sibling.child_thread.as_ref().is_some_and(|child| child.parent_task_id == MAIN),
                    "a fresh side chat asked of a side chat is one sibling under the same main (Main), never the fork's child: {sibling:?}"
                );
                anyhow::ensure!(with_sibling["main"] == MAIN && role_of(&with_sibling, &fork_child).as_deref() == Some("Child"), "the sibling opens in Main's workbench, as a fresh side chat: {with_sibling}");
                window.update(cx, |shell, _, cx| shell.fixture_workbench_open_as_main(&fork_child, cx))?;
                let child_main = {
                    let id = fork_child.clone();
                    wait_for(window, cx, "the sibling is the workbench Main", move |rb| rb["main"] == id.as_str() && rb["loaded"] == true).await?
                };
                let child_main_help = tab_json(&child_main, &fork_child).map(|tab| tab["help"].clone()).unwrap_or(Value::Null);
                anyhow::ensure!(
                    child_main_help["title"].as_str().is_some_and(|title| title.starts_with("Side chat of ") && title != "Side chat of a deleted chat")
                        && child_main_help["details"] == format!("Started fresh: it does not carry the main chat's conversation. {MAIN_NOTE}").as_str(),
                    "a fresh side chat as its own Main keeps its fresh side chat help: {child_main_help}"
                );
                // Deleted sources: the orphan Side chat (its source was
                // deleted in w20b) and a fresh side chat whose main chat is
                // deleted.
                window.update(cx, |shell, _, cx| shell.fixture_workbench_open_as_main(&orphan, cx))?;
                let orphan_main = {
                    let id = orphan.clone();
                    wait_for(window, cx, "the orphan Side chat is the workbench Main", move |rb| rb["main"] == id.as_str() && rb["loaded"] == true).await?
                };
                let orphan_main_help = tab_json(&orphan_main, &orphan).map(|tab| tab["help"].clone()).unwrap_or(Value::Null);
                anyhow::ensure!(
                    orphan_main_help["title"] == "Side chat of a deleted chat" && orphan_main_help["details"].as_str().is_some_and(|details| details.ends_with(MAIN_NOTE)),
                    "an orphan Side chat as Main names its deleted source: {orphan_main_help}"
                );
                // A disposable main chat (one level: only a main chat has
                // side chats), its fresh side chat, then the main deleted.
                const DISPOSABLE: &str = "e0e0e0e0-0000-4000-8000-0000000000d2";
                seed_main(DISPOSABLE, "Disposable parent").await?;
                let disposable = DISPOSABLE.to_string();
                merge_engine_chats(&reader, &state, cx).await?;
                window.update(cx, |shell, _, cx| shell.fixture_workbench_open_as_main(&disposable, cx))?;
                let disposable_main = {
                    let id = disposable.clone();
                    wait_for(window, cx, "the disposable main chat is the workbench Main", move |rb| rb["main"] == id.as_str() && rb["loaded"] == true).await?
                };
                let disposable_pane = pane_of(&disposable_main, &disposable).and_then(|pane| pane["id"].as_str().map(str::to_string)).ok_or_else(|| anyhow::anyhow!("its group: {disposable_main}"))?;
                window.update(cx, |shell, _, cx| shell.fixture_chooser_open(Some(&disposable_pane), false, cx))?;
                settle(cx, 300).await;
                window.update(cx, |shell, _, cx| shell.fixture_chooser_create(cx))?;
                let orphan_child = {
                    let id = disposable.clone();
                    let created = wait_for(window, cx, "a fresh side chat of the disposable main chat", move |rb| rb["creatingChild"] == false && all_panes(rb).iter().flat_map(tabs_of).any(|tab| tab != id)).await?;
                    all_panes(&created).iter().flat_map(tabs_of).find(|tab| *tab != disposable).unwrap()
                };
                merge_engine_chats(&reader, &state, cx).await?;
                window.update(cx, |shell, _, cx| shell.fixture_workbench_open_as_main(&orphan_child, cx))?;
                {
                    let id = orphan_child.clone();
                    wait_for(window, cx, "the fresh side chat is the workbench Main", move |rb| rb["main"] == id.as_str() && rb["loaded"] == true).await?;
                }
                window.update(cx, |shell, _, cx| shell.fixture_chat_action(&disposable, "delete", cx))?;
                let mut parent_gone = false;
                for _ in 0..80 {
                    settle(cx, 150).await;
                    if !engine_has(&reader, &disposable).await? {
                        parent_gone = true;
                        break;
                    }
                }
                anyhow::ensure!(parent_gone, "the side chat's main chat is deleted");
                cx.update(|cx| {
                    state.update(cx, |state, cx| {
                        state.chats.retain(|chat| chat.id != disposable);
                        cx.notify();
                    })
                });
                anyhow::ensure!(engine_has(&reader, &orphan_child).await?, "deleting a main chat keeps its side chat");
                let orphan_child_help = {
                    let id = orphan_child.clone();
                    let rb = wait_for(window, cx, "the side chat names its deleted main chat", move |rb| tab_json(rb, &id).is_some_and(|tab| tab["help"]["title"] == "Side chat of a deleted chat")).await?;
                    tab_json(&rb, &orphan_child).map(|tab| tab["help"].clone()).unwrap_or(Value::Null)
                };
                anyhow::ensure!(orphan_child_help["details"] == format!("Started fresh: it does not carry the main chat's conversation. {MAIN_NOTE}").as_str(), "a fresh side chat as Main with a deleted main chat: {orphan_child_help}");
                shot(window, cx, output.join("w20c-lineage-as-main.png")).await?;
                progress(&output, &mut record, "lineageAsMain".into(), serde_json::json!({"sideMain": fork_main_help, "childMain": child_main_help, "orphanSideMain": orphan_main_help, "orphanChildMain": orphan_child_help}));
                window.update(cx, |shell, _, cx| shell.fixture_workbench_open_as_main(MAIN, cx))?;
                wait_for(window, cx, "back to Main's workbench", |rb| rb["main"] == MAIN && rb["loaded"] == true).await?;
                settle(cx, 300).await;
                let back = related(window, cx)?;
                anyhow::ensure!(tree_has(&back, MAIN, &fork_child, "Child chat", None), "the sibling lists directly in Main's library, not under the fork: {back}");
                // Its tab leaves Main's workbench as it found it for the
                // steps that follow (closing never ends a chat).
                window.update(cx, |shell, _, cx| shell.fixture_workbench_close(&fork_child, cx))?;
                {
                    let id = fork_child.clone();
                    wait_for(window, cx, "the sibling's tab closed", move |rb| occurrences(rb, &id) == 0).await?;
                }
                progress(&output, &mut record, "forkOperations".into(), serde_json::json!({"archive": after_shortcut, "placed": placed["sidebarOrder"], "marks": marks, "sibling": fork_child, "library": back["relatedTrees"][MAIN]}));

                // w20d. Failure case 31: a new Main before the engine's
                // registry has it. The layout read is refused ("Main chat is
                // unavailable") and retried quietly; a refusal that outlasts
                // the grace is shown with Retry but never latched; once the
                // engine registers the chat, the layout loads and the notice
                // clears.
                const FRESH: &str = "e0e0e0e0-0000-4000-8000-0000000000f1";
                let main_row = chat_row(cx, &state, MAIN).ok_or_else(|| anyhow::anyhow!("Main's row"))?;
                let mut fresh_row = main_row.clone();
                fresh_row.id = FRESH.into();
                fresh_row.title = Some("Fresh main before the registry".into());
                fresh_row.child_thread = None;
                fresh_row.pinned_order = None;
                cx.update(|cx| {
                    state.update(cx, |state, cx| {
                        state.chats.push(fresh_row.clone());
                        state.select_chat(Some(FRESH.into()), cx);
                    })
                });
                settle(cx, 3000).await;
                let early = wb(window, cx)?;
                anyhow::ensure!(early["main"] == FRESH && early["loaded"] == false, "the fresh Main is selected, its layout not loaded yet: {early}");
                anyhow::ensure!(early["unavailableLatched"] == false && early["loadNotice"].is_null(), "a fresh Main waits quietly (no notice, nothing latched): {early}");
                settle(cx, 5000).await;
                let late = wb(window, cx)?;
                anyhow::ensure!(late["unavailableLatched"] == false, "a refused Main is never latched unavailable: {late}");
                anyhow::ensure!(late["loadNotice"].as_str().is_some_and(|notice| notice.contains("Retrying")), "a refusal past the grace is shown, still retrying: {late}");
                shot(window, cx, output.join("w20d-fresh-main-retrying.png")).await?;
                reader
                    .call(
                        "Mutate",
                        serde_json::json!({
                            "op": "createChat",
                            "chatId": FRESH,
                            "spaceId": PROJECT,
                            "cwd": main_row.cwd.clone().unwrap_or_default(),
                            "config": serde_json::to_value(config())?,
                        }),
                    )
                    .await?;
                let ready = wait_for(window, cx, "the fresh Main's layout loads once the engine knows it", |rb| {
                    rb["main"] == FRESH && rb["loaded"] == true && rb["loadNotice"].is_null() && rb["unavailableLatched"] == false
                })
                .await?;
                cx.update(|cx| state.update(cx, |state, cx| state.select_chat(Some(MAIN.into()), cx)));
                wait_for(window, cx, "back to Main's workbench", |rb| rb["main"] == MAIN && rb["loaded"] == true).await?;
                progress(&output, &mut record, "freshMainReadiness".into(), serde_json::json!({"early": early["loadNotice"], "late": late["loadNotice"], "ready": ready["revision"]}));

                // w20e. Failure case 32: a group's split control makes one
                // blank child of Main in a new group to its right (no empty
                // group, no choice, nothing started). Main is its group's
                // active chat (a side chat's split would be a sibling).
                click_tab(window, cx, MAIN).await?;
                let before_split = wb(window, cx)?;
                let main_group = pane_of(&before_split, MAIN).and_then(|pane| pane["id"].as_str().map(str::to_string)).ok_or_else(|| anyhow::anyhow!("Main's group: {before_split}"))?;
                let groups_before = all_panes(&before_split).len();
                let known: std::collections::HashSet<String> = all_panes(&before_split).iter().flat_map(tabs_of).collect();
                let split_source = pane_of(&before_split, MAIN)
                    .and_then(|pane| pane["active"].as_str().map(str::to_string))
                    .unwrap_or_else(|| MAIN.to_string());
                set_right_sidebar(window, cx, false).await?;
                window.update(cx, |shell, _, cx| shell.fixture_workbench_split_child(&main_group, cx))?;
                let split_child = {
                    let known = known.clone();
                    wait_for(window, cx, "the split control's child in a new group", move |rb| {
                        rb["creatingChild"] == false
                            && all_panes(rb).len() == groups_before + 1
                            && all_panes(rb).iter().flat_map(tabs_of).any(|tab| !known.contains(&tab))
                    })
                    .await?
                };
                let new_child = all_panes(&split_child).iter().flat_map(tabs_of).find(|tab| !known.contains(tab)).unwrap();
                let new_group = pane_of(&split_child, &new_child).ok_or_else(|| anyhow::anyhow!("the child's group: {split_child}"))?;
                anyhow::ensure!(tabs_of(&new_group) == [new_child.clone()] && new_group["active"] == new_child.as_str(), "the new group holds just the new child: {split_child}");
                anyhow::ensure!(pane_of(&split_child, MAIN).is_some_and(|pane| pane["id"] == main_group.as_str()), "Main stays in its group: {split_child}");
                let rows = merge_engine_chats(&reader, &state, cx).await?;
                anyhow::ensure!(rows.iter().find(|chat| chat.id == new_child).is_some_and(|chat| chat.child_thread.as_ref().is_some_and(|child| child.parent_task_id == split_source)), "the new thread comes from the group's active chat ({split_source})");
                anyhow::ensure!(active_turn(&task_snapshot(&reader, &new_child).await?).is_none(), "the blank child starts nothing");
                shot(window, cx, output.join("w20e-split-child.png")).await?;

                // w20f. Failure case 33: the blank child switches provider
                // directly (the ordinary SwitchProvider, no confirmation or
                // handoff); a started chat or a fork confirms first.
                let switch_path = |cx: &mut AsyncApp, chat: &str| {
                    let chat = chat.to_string();
                    window.update(cx, move |shell, _, cx| {
                        shell
                            .fixture_workbench_composer(&chat, cx)
                            .map(|composer| composer.read(cx).pickers().read(cx).fixture_switch_path(cx))
                    })
                };
                let mut blank_path = None;
                for _ in 0..40 {
                    blank_path = switch_path(cx, &new_child)?;
                    if blank_path == Some("direct") {
                        break;
                    }
                    settle(cx, 100).await;
                }
                anyhow::ensure!(blank_path == Some("direct"), "a blank child switches provider directly: {blank_path:?}");
                // Provider catalogs are queried with that provider's own
                // account: a chat bound to one provider's account never
                // sends it to another provider's catalog (the native repro:
                // "Unknown Codex account selection: claude-native").
                cx.update(|cx| {
                    state.update(cx, |state, cx| {
                        if let Some(config) = state
                            .chats
                            .iter_mut()
                            .find(|chat| chat.id == new_child)
                            .and_then(|chat| chat.config.as_mut())
                        {
                            config.provider_account_id = Some("claude-native".into());
                        }
                        cx.notify();
                    })
                });
                let catalog_accounts = {
                    let chat = new_child.clone();
                    window.update(cx, move |shell, _, cx| {
                        shell.fixture_workbench_composer(&chat, cx).map(|composer| {
                            let pickers = composer.read(cx).pickers().read(cx);
                            (
                                pickers.fixture_catalog_account(HarnessId::Mock, cx),
                                pickers.fixture_catalog_account(HarnessId::Codex, cx),
                                pickers.fixture_catalog_account(HarnessId::ClaudeCode, cx),
                            )
                        })
                    })?
                };
                anyhow::ensure!(
                    catalog_accounts.as_ref().is_some_and(|(own, codex, claude)| {
                        own.as_deref() == Some("claude-native")
                            && codex.as_deref() != Some("claude-native")
                            && claude.is_none()
                    }),
                    "each provider's catalog uses only its own account selection: {catalog_accounts:?}"
                );
                let started = switch_path(cx, MAIN)?;
                anyhow::ensure!(started == Some("confirm"), "a started chat confirms a provider switch: {started:?}");
                let fork_path = switch_path(cx, &orphan)?;
                anyhow::ensure!(fork_path.is_none_or(|path| path == "confirm"), "a fork never switches without confirming: {fork_path:?}");
                progress(&output, &mut record, "splitChild".into(), serde_json::json!({"layout": split_child, "child": new_child, "source":split_source,"blankPath": blank_path, "startedPath": started}));

                // w20g. Failure case 34: no context row in child/fork panes;
                // an unseen completion on a hidden tab shows the accent
                // unread dot (status Completed), cleared once the tab is
                // on screen.
                let strip = window.update(cx, |shell, _, cx| shell.fixture_workbench_strip(&new_child, cx))?;
                anyhow::ensure!(strip["metaRow"] == false, "a child pane renders no context row: {strip}");
                let hidden = all_panes(&wb(window, cx)?)
                    .into_iter()
                    .flat_map(|pane| {
                        let active = pane["active"].as_str().unwrap_or_default().to_string();
                        tabs_of(&pane).into_iter().filter(move |tab| *tab != active)
                    })
                    .find(|tab| tab != MAIN)
                    .ok_or_else(|| anyhow::anyhow!("a hidden tab to receive an unseen completion"))?;
                cx.update(|cx| {
                    state.update(cx, |state, cx| {
                        if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == hidden) {
                            let now = chrono::Utc::now();
                            chat.last_message_at = Some(now);
                            chat.last_seen_at = Some(now - chrono::TimeDelta::minutes(1));
                        }
                        cx.notify();
                    })
                });
                let dotted = {
                    let id = hidden.clone();
                    wait_for(window, cx, "the hidden tab shows its unread dot", move |rb| tab_json(rb, &id).is_some_and(|tab| tab["status"] == "Completed")).await?
                };
                shot(window, cx, output.join("w20g-unread-tab.png")).await?;
                activate(window, cx)?;
                click_tab(window, cx, &hidden).await?;
                let seen = {
                    let id = hidden.clone();
                    wait_for(window, cx, "the unread dot clears once the tab is on screen", move |rb| tab_json(rb, &id).is_some_and(|tab| tab["status"] == "Idle")).await?
                };
                progress(&output, &mut record, "tabStatus".into(), serde_json::json!({"dotted": tab_json(&dotted, &hidden), "seen": tab_json(&seen, &hidden)}));

                // w20g2. Failure case 42: Enter runs only the action the
                // chooser visibly offers. The split child is a side chat (its
                // group's `+` opens the side chat's card), so the chooser's
                // guards run on a blank main chat opened in that group: a
                // Side chat with context from it (nothing to carry; Mock has
                // no native history) offers nothing. Hold both catalog and
                // capability loading states deterministically; real Enter
                // must never create while either result is unavailable.
                let blank_group = new_group["id"].as_str().unwrap_or_default().to_string();
                click_tab(window, cx, &new_child).await?;
                const BLANK: &str = "e0e0e0e0-0000-4000-8000-0000000000b1";
                reader
                    .call(
                        "Mutate",
                        serde_json::json!({
                            "op": "createChat",
                            "chatId": BLANK,
                            "spaceId": PROJECT,
                            "cwd": automation_cwd.to_string_lossy(),
                            "config": serde_json::to_value(config())?,
                        }),
                    )
                    .await?;
                automation_workspace.rename_chat(BLANK, "Blank main chat")?;
                merge_engine_chats(&reader, &state, cx).await?;
                window.update(cx, |shell, _, cx| shell.fixture_workbench_open(BLANK, cx))?;
                wait_for(window, cx, "the blank main chat active in the split group", |rb| {
                    pane_of(rb, BLANK).is_some_and(|pane| pane["id"] == blank_group.as_str() && pane["active"] == BLANK)
                })
                .await?;
                let rows_before_blank = merge_engine_chats(&reader, &state, cx).await?.len();
                let held_providers = window.update(cx, |shell, _, cx| shell.fixture_chooser_hold_providers(cx))?
                    .ok_or_else(|| anyhow::anyhow!("the warm engine provider catalog to hold"))?;
                window.update(cx, |shell, _, cx| shell.fixture_chooser_open(Some(&blank_group), false, cx))?;
                draw(window, cx)?;
                let provider_loading = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                anyhow::ensure!(provider_loading["providers"].is_null() && provider_loading["action"].as_str().is_some_and(|action| action.starts_with("Disabled")), "provider loading is actually held: {provider_loading}");
                key(window, cx, "enter")?;
                settle(cx, 200).await;
                anyhow::ensure!(wb(window, cx)?["creatingChild"] == false && window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?["open"] == true, "Enter with no provider result creates nothing");
                anyhow::ensure!(merge_engine_chats(&reader, &state, cx).await?.len() == rows_before_blank, "cold catalog browsing created no chat");
                shot(window, cx, output.join("w20g2-cold-provider-chooser.png")).await?;
                window.update(cx, |shell, _, cx| {
                    shell.fixture_chooser_restore_providers(held_providers.clone(), cx);
                    shell.fixture_chooser_provider(HarnessId::Mock, cx);
                })?;
                let provider_ready = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                anyhow::ensure!(provider_ready["action"].as_str().is_some_and(|action| action.starts_with("NewThread")), "the actual provider result restores the explicit choice: {provider_ready}");
                window.update(cx, |shell, _, cx| shell.fixture_chooser_restore_providers(Vec::new(), cx))?;
                let provider_removed = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                anyhow::ensure!(provider_removed["action"].as_str().is_some_and(|action| action.starts_with("Disabled")), "a provider removed from the offered catalog cannot create: {provider_removed}");
                key(window, cx, "enter")?;
                settle(cx, 150).await;
                anyhow::ensure!(merge_engine_chats(&reader, &state, cx).await?.len() == rows_before_blank, "the removed provider creates no chat");
                window.update(cx, |shell, _, cx| shell.fixture_chooser_restore_providers(held_providers, cx))?;
                key(window, cx, "escape")?;
                window.update(cx, |shell, _, cx| {
                    shell.fixture_chooser_open(Some(&blank_group), true, cx);
                    shell.fixture_chooser_hold_capability(true, cx);
                })?;
                draw(window, cx)?;
                let loading = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                anyhow::ensure!(loading["capability"].is_null() && loading["action"].as_str().is_some_and(|action| action.starts_with("Disabled")), "capability loading is actually held: {loading}");
                key(window, cx, "enter")?;
                settle(cx, 200).await;
                let after_loading = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                anyhow::ensure!(after_loading["open"] == true && after_loading["sideRequested"] == false, "Enter before the preview is known creates nothing: {loading} -> {after_loading}");
                window.update(cx, |shell, _, cx| shell.fixture_chooser_hold_capability(false, cx))?;
                let mut cold = Value::Null;
                for _ in 0..40 {
                    cold = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                    if cold["capability"].is_object() {
                        break;
                    }
                    settle(cx, 100).await;
                }
                anyhow::ensure!(cold["source"] == BLANK && cold["action"].as_str().is_some_and(|action| action.starts_with("Disabled")), "a blank source offers no Side chat with context: {cold}");
                shot(window, cx, output.join("w20g2-blank-side-chooser.png")).await?;
                key(window, cx, "enter")?;
                settle(cx, 400).await;
                let after_cold = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                anyhow::ensure!(after_cold["open"] == true && after_cold["sideRequested"] == false, "Enter on a disabled action does nothing: {after_cold}");
                anyhow::ensure!(merge_engine_chats(&reader, &state, cx).await?.len() == rows_before_blank, "no chat was created from the blank source");
                key(window, cx, "escape")?;
                settle(cx, 300).await;
                let blank_owner = window.update(cx, |shell, window, cx| shell.fixture_workbench_keyboard_chat(window, cx))?;
                anyhow::ensure!(blank_owner.as_deref() == Some(BLANK), "Escape returns the keyboard to the source's composer: {blank_owner:?}");
                progress(&output, &mut record, "chooserDisabledEnter".into(), serde_json::json!({"providerLoading":provider_loading,"providerReady":provider_ready,"providerRemoved":provider_removed,"loading": loading, "afterLoading": after_loading, "cold": cold}));

                // w20g3. Failure case 43: a New thread whose outcome is
                // unknown (its response lost after the engine created it)
                // blocks any other intent (another source and provider, the
                // chooser's Enter) without replaying it, keeps the original
                // request, and Check status resends exactly that request
                // (same key, source, provider, group, placement): one thread.
                click_tab(window, cx, MAIN).await?;
                let rows_before_unknown = merge_engine_chats(&reader, &state, cx).await?.len();
                window.update(cx, |shell, _, _| shell.fixture_workbench_lose_child_response())?;
                window.update(cx, |shell, _, cx| shell.fixture_workbench_request_child(&main_group, Some(HarnessId::Mock), cx))?;
                let unknown = wait_for(window, cx, "an unknown New thread outcome", |rb| rb["creatingChild"] == false && rb["childFailureRequest"]["ambiguous"] == true).await?;
                let original = unknown["childFailureRequest"].clone();
                anyhow::ensure!(original["parent"] == MAIN && original["provider"] == "Mock" && original["pane"] == main_group.as_str() && original["split"] == false, "the unknown request is recorded as asked: {original}");
                anyhow::ensure!(merge_engine_chats(&reader, &state, cx).await?.len() == rows_before_unknown + 1, "the engine did create it (only the response was lost)");
                shot(window, cx, output.join("w20g3-unknown-child.png")).await?;
                window.update(cx, |shell, _, cx| shell.fixture_workbench_request_child(&blank_group, Some(HarnessId::Codex), cx))?;
                settle(cx, 300).await;
                let blocked = wb(window, cx)?;
                anyhow::ensure!(blocked["creatingChildRequest"].is_null() && blocked["childBlocked"] == true && blocked["childFailureRequest"] == original, "another source/provider is blocked and the original kept: {blocked}");
                window.update(cx, |shell, _, cx| shell.fixture_chooser_open(Some(&blank_group), false, cx))?;
                settle(cx, 300).await;
                let blocked_chooser = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                anyhow::ensure!(blocked_chooser["action"].as_str().is_some_and(|action| action.starts_with("Disabled")), "the chooser offers no New thread while one is unresolved: {blocked_chooser}");
                key(window, cx, "enter")?;
                settle(cx, 300).await;
                anyhow::ensure!(wb(window, cx)?["creatingChildRequest"].is_null() && wb(window, cx)?["childFailureRequest"] == original, "Enter on the blocked chooser sends nothing");
                key(window, cx, "escape")?;
                settle(cx, 200).await;
                window.update(cx, |shell, _, cx| shell.fixture_workbench_retry_child(cx))?;
                let checking = wb(window, cx)?;
                anyhow::ensure!(
                    ["key", "parent", "provider", "pane", "split"].iter().all(|field| checking["creatingChildRequest"][*field] == original[*field]),
                    "Check status resends exactly the original request: {checking}"
                );
                let recovered = wait_for(window, cx, "Check status settles", |rb| rb["creatingChild"] == false && rb["childFailureRequest"].is_null()).await?;
                anyhow::ensure!(merge_engine_chats(&reader, &state, cx).await?.len() == rows_before_unknown + 1, "recovery creates nothing more (exactly one New thread)");
                let recovered_child = pane_of(&recovered, MAIN)
                    .map(|pane| tabs_of(&pane))
                    .unwrap_or_default()
                    .into_iter()
                    .find(|tab| !known.contains(tab) && *tab != new_child)
                    .ok_or_else(|| anyhow::anyhow!("the recovered New thread opens in Main's group: {recovered}"))?;
                progress(&output, &mut record, "unknownChild".into(), serde_json::json!({"original": original, "blocked": blocked["childBlocked"], "recovered": recovered_child}));
                window.update(cx, |shell, _, cx| shell.fixture_workbench_close(&recovered_child, cx))?;
                settle(cx, 300).await;
                // Restore the arrangement the frame steps expect: the split
                // child's group closes (the child stays in Related chats).
                let new_group_id = new_group["id"].as_str().unwrap_or_default().to_string();
                window.update(cx, |shell, _, cx| shell.fixture_workbench_close_pane(&new_group_id, cx))?;
                wait_for(window, cx, "the split child's group closed", move |rb| all_panes(rb).len() == groups_before).await?;
                // The blank main chat was only a chooser source: remove it.
                window.update(cx, |shell, _, cx| shell.fixture_chat_action(BLANK, "delete", cx))?;
                for _ in 0..80 {
                    settle(cx, 150).await;
                    if !engine_has(&reader, BLANK).await? {
                        break;
                    }
                }
                cx.update(|cx| {
                    state.update(cx, |state, cx| {
                        state.chats.retain(|chat| chat.id != BLANK);
                        cx.notify();
                    })
                });
                set_right_sidebar(window, cx, true).await?;

                // w20h. Failure cases 39-40: the chooser's Side chat path.
                // The source's provider (Mock) has no native history forks:
                // the plain "Create side chat" is not offered as if it were
                // native; the recent-visible-messages action is explicit and
                // creates exactly one side chat of Main, on Main's provider.
                // A chooser whose group changes its active chat closes
                // without creating.
                let main_group = pane_of(&wb(window, cx)?, MAIN)
                    .and_then(|pane| pane["id"].as_str().map(str::to_string))
                    .ok_or_else(|| anyhow::anyhow!("Main's group"))?;
                click_tab(window, cx, MAIN).await?;
                let side_before = merge_engine_chats(&reader, &state, cx).await?.len();
                let known_sides: std::collections::HashSet<String> =
                    cx.update(|cx| state.read(cx).side_chat_rows.keys().cloned().collect());
                window.update(cx, |shell, _, cx| shell.fixture_chooser_open(Some(&main_group), true, cx))?;
                let mut side_chooser = Value::Null;
                for _ in 0..40 {
                    settle(cx, 100).await;
                    side_chooser = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                    if side_chooser["capability"].is_object() {
                        break;
                    }
                }
                anyhow::ensure!(side_chooser["open"] == true && side_chooser["source"] == MAIN && side_chooser["kind"] == "SideChat", "the Side chat chooser names Main as its source: {side_chooser}");
                anyhow::ensure!(side_chooser["capability"]["capability"] == "None", "the preview reports no native fork for this provider: {side_chooser}");
                shot(window, cx, output.join("w20h-side-chooser.png")).await?;
                // Browsing does not create: switch kinds and providers back and forth.
                window.update(cx, |shell, _, cx| {
                    shell.fixture_chooser_kind(false, cx);
                    shell.fixture_chooser_provider(HarnessId::Mock, cx);
                    shell.fixture_chooser_kind(true, cx);
                })?;
                settle(cx, 300).await;
                anyhow::ensure!(merge_engine_chats(&reader, &state, cx).await?.len() == side_before, "browsing the chooser creates nothing");
                // The explicit recent-context side chat, through real Enter:
                // the visibly offered action (forceSnapshot), never a plain
                // native request.
                let offered = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                anyhow::ensure!(offered["action"] == "SideRecent", "a non-native source with messages offers only the recent-messages side chat: {offered}");
                key(window, cx, "enter")?;
                let mut recent_fork = None;
                for _ in 0..80 {
                    let rows = merge_engine_chats(&reader, &state, cx).await?;
                    if rows.len() > side_before {
                        recent_fork = cx.update(|cx| {
                            state
                                .read(cx)
                                .side_chat_rows
                                .iter()
                                .filter(|(id, row)| row.parent_id == MAIN && !known_sides.contains(*id))
                                .map(|(id, _)| id.clone())
                                .find(|id| rows.iter().any(|chat| chat.id == *id))
                        });
                        if recent_fork.is_some() {
                            break;
                        }
                    }
                    settle(cx, 150).await;
                }
                let recent_fork = recent_fork.ok_or_else(|| anyhow::anyhow!("the recent-context side chat of Main"))?;
                anyhow::ensure!(merge_engine_chats(&reader, &state, cx).await?.len() == side_before + 1, "exactly one side chat per Create");
                let fork_row = chat_row(cx, &state, &recent_fork).ok_or_else(|| anyhow::anyhow!("its row"))?;
                // Its tab's tooltip names its source and says it copied
                // recent visible messages only (never full native context).
                let mut recent_help = Value::Null;
                for _ in 0..60 {
                    merge_engine_chats(&reader, &state, cx).await?;
                    settle(cx, 100).await;
                    recent_help = tab_json(&wb(window, cx)?, &recent_fork).map(|tab| tab["help"].clone()).unwrap_or(Value::Null);
                    if recent_help["details"].as_str().is_some_and(|details| details.contains("recent visible messages only")) {
                        break;
                    }
                }
                anyhow::ensure!(recent_help["title"].as_str().is_some_and(|title| title.starts_with("Side chat of ")), "the recent-context tab is a Side chat of its source: {recent_help}");
                let details = recent_help["details"].as_str().unwrap_or_default();
                anyhow::ensure!(
                    details.starts_with("Side chats carry the context of the parent thread. Context is captured when created; later parent messages are not copied.")
                        && details.ends_with("This side chat copied recent visible messages only."),
                    "the recent-context tab's tooltip says it copied recent visible messages only: {recent_help}"
                );
                anyhow::ensure!(fork_row.config.as_ref().is_some_and(|config| config.harness == HarnessId::Mock), "the side chat stays on the source's provider: {fork_row:?}");
                // A stale chooser: its group's active chat changes while it is
                // open, so its Create does nothing.
                window.update(cx, |shell, _, cx| shell.fixture_chooser_open(Some(&main_group), false, cx))?;
                settle(cx, 200).await;
                let other_tab = pane_of(&wb(window, cx)?, MAIN)
                    .map(|pane| tabs_of(&pane))
                    .unwrap_or_default()
                    .into_iter()
                    .find(|tab| tab != MAIN);
                if let Some(other) = other_tab {
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_open(&other, cx))?;
                    settle(cx, 500).await;
                    let rows_before = merge_engine_chats(&reader, &state, cx).await?.len();
                    window.update(cx, |shell, _, cx| shell.fixture_chooser_create(cx))?;
                    settle(cx, 600).await;
                    let stale = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                    anyhow::ensure!(stale["open"] == false && merge_engine_chats(&reader, &state, cx).await?.len() == rows_before, "a chooser for a changed source closes without creating: {stale}");
                } else {
                    anyhow::bail!("Main's group needs a second tab to test a stale chooser");
                }
                click_tab(window, cx, MAIN).await?;
                progress(&output, &mut record, "chooserSideChat".into(), serde_json::json!({"chooser": side_chooser, "recentFork": recent_fork}));

                // w20i. Failure case 36: Settings in the contained plane, its
                // navigation shown even with the chat sidebar collapsed; its
                // own collapse never rewrites the saved chat preference, and
                // leaving restores the chat sidebar exactly.
                let chat_frame = frame(window, cx)?;
                if chat_frame["sidebarCollapsed"] == false {
                    toggle(window, cx, false).await?;
                }
                let collapsed = frame(window, cx)?;
                anyhow::ensure!(collapsed["sidebarCollapsed"] == true && collapsed["sidebarShown"] == false, "the chat sidebar is collapsed: {collapsed}");
                let rows_before_settings = merge_engine_chats(&reader, &state, cx).await?.len();
                window.update(cx, |shell, _, cx| shell.fixture_chooser_open(None, false, cx))?;
                draw(window, cx)?;
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?["open"] == true, "the chooser is open before navigating to Settings");
                window.update(cx, |shell, _, cx| shell.fixture_open_settings(cx))?;
                for _ in 0..12 {
                    draw(window, cx)?;
                    settle(cx, 60).await;
                }
                let in_settings = frame(window, cx)?;
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?["open"] == false, "Settings dismisses the creation chooser");
                key(window, cx, "enter")?;
                window.update(cx, |shell, _, cx| shell.fixture_chooser_create(cx))?;
                settle(cx, 150).await;
                anyhow::ensure!(merge_engine_chats(&reader, &state, cx).await?.len() == rows_before_settings, "no creation is possible from the Settings route");
                anyhow::ensure!(in_settings["settingsRoute"] == true && in_settings["sidebarShown"] == true && in_settings["sidebarWidthNow"].as_f64().unwrap_or(0.0) > 100.0, "Settings shows its navigation: {in_settings}");
                anyhow::ensure!(in_settings["sidebarCollapsed"] == true, "entering Settings leaves the saved chat preference collapsed: {in_settings}");
                shot(window, cx, output.join("w20i-settings-contained.png")).await?;
                window.update(cx, |shell, _, cx| shell.fixture_toggle_sidebar(false, cx))?;
                for _ in 0..12 {
                    draw(window, cx)?;
                    settle(cx, 60).await;
                }
                let settings_collapsed = frame(window, cx)?;
                anyhow::ensure!(settings_collapsed["sidebarShown"] == false && settings_collapsed["sidebarCollapsed"] == true, "Settings' own collapse: {settings_collapsed}");
                window.update(cx, |shell, _, cx| shell.fixture_toggle_sidebar(false, cx))?;
                window.update(cx, |shell, _, cx| shell.fixture_leave_settings(true, cx))?;
                for _ in 0..12 {
                    draw(window, cx)?;
                    settle(cx, 60).await;
                }
                let back_in_chat = frame(window, cx)?;
                anyhow::ensure!(back_in_chat["settingsRoute"] == false && back_in_chat["sidebarCollapsed"] == true && back_in_chat["sidebarShown"] == false, "Back restores the collapsed chat sidebar: {back_in_chat}");
                if chat_frame["sidebarCollapsed"] == false {
                    toggle(window, cx, false).await?;
                }
                progress(&output, &mut record, "settingsSidebar".into(), serde_json::json!({"inSettings": in_settings, "backInChat": back_in_chat}));

                // w20j. Failure case 41 and the reviewed shortcut map: the
                // configured Mod+Alt+N opens the focused group's chooser (a
                // plain N typed in the composer does not); Escape closes it
                // before any stop handling and nothing is created; group
                // shortcuts do nothing underneath the open chooser and move
                // focus in reading order once it is closed.
                activate(window, cx)?;
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(MAIN, window, cx))?, "focus Main's composer");
                draw(window, cx)?;
                settle(cx, 120).await;
                let rows_before_keys = merge_engine_chats(&reader, &state, cx).await?.len();
                key(window, cx, "n")?;
                settle(cx, 150).await;
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?["open"] == false, "a plain N in the composer is text, not a shortcut");
                window.update(cx, |shell, _, cx| shell.fixture_workbench_type(MAIN, "", false, cx))?;
                key(window, cx, "cmd-alt-n")?;
                settle(cx, 300).await;
                let keyed = window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?;
                anyhow::ensure!(keyed["open"] == true && keyed["kind"] == "NewThread", "Mod+Alt+N opens the focused group's chooser: {keyed}");
                let focused_before = wb(window, cx)?["focusedPane"].clone();
                key(window, cx, "cmd-alt-]")?;
                settle(cx, 300).await;
                anyhow::ensure!(wb(window, cx)?["focusedPane"] == focused_before, "group shortcuts do nothing under the open chooser");
                let escapes_before_keys = window.update(cx, |shell, _, _| shell.fixture_workbench_escape_log().len())?;
                key(window, cx, "escape")?;
                settle(cx, 300).await;
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_chooser_readback(cx))?["open"] == false, "Escape closes the chooser");
                anyhow::ensure!(window.update(cx, |shell, _, _| shell.fixture_workbench_escape_log().len())? == escapes_before_keys, "an Escape that closes the chooser stops nothing");
                anyhow::ensure!(merge_engine_chats(&reader, &state, cx).await?.len() == rows_before_keys, "keyboard browsing created nothing");
                let keyed_owner = window.update(cx, |shell, window, cx| shell.fixture_workbench_keyboard_chat(window, cx))?;
                anyhow::ensure!(keyed_owner.as_deref() == Some(MAIN), "Escape returns the keyboard to the chooser's source (Main): {keyed_owner:?}");
                // Real macOS events fold Shift+[ / ] into { / } and clear
                // Shift. Inject that event shape, not the stored combo.
                let before_tab_keys = wb(window, cx)?;
                let main_group = pane_of(&before_tab_keys, MAIN).ok_or_else(|| anyhow::anyhow!("Main's group for tab shortcuts"))?;
                anyhow::ensure!(tabs_of(&main_group).len() > 1, "tab shortcuts require multiple tabs in Main's group");
                key(window, cx, "cmd-}")?;
                settle(cx, 300).await;
                let next_tab_owner = window.update(cx, |shell, window, cx| shell.fixture_workbench_keyboard_chat(window, cx))?;
                anyhow::ensure!(next_tab_owner.is_some() && next_tab_owner.as_deref() != Some(MAIN), "native Cmd+Shift+] moves to another tab: {next_tab_owner:?}");
                key(window, cx, "cmd-{")?;
                settle(cx, 300).await;
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_keyboard_chat(window, cx))?.as_deref() == Some(MAIN), "native Cmd+Shift+[ returns to Main");
                progress(&output, &mut record, "nativeTabShortcuts".into(), serde_json::json!({"nextTab": next_tab_owner, "returnedTo": MAIN}));
                if all_panes(&wb(window, cx)?).len() > 1 {
                    key(window, cx, "cmd-alt-]")?;
                    settle(cx, 400).await;
                    let moved = wb(window, cx)?;
                    anyhow::ensure!(moved["focusedPane"] != focused_before, "Mod+Alt+] focuses the next group: {moved}");
                    key(window, cx, "cmd-alt-[")?;
                    settle(cx, 400).await;
                    anyhow::ensure!(wb(window, cx)?["focusedPane"] == focused_before, "Mod+Alt+[ returns to the previous group");
                }
                // Failure case 45: cycling to an empty group moves the
                // keyboard there (no composer owns it); typing, Escape and
                // send never act on Main.
                let main_pane_keys = pane_of(&wb(window, cx)?, MAIN).and_then(|pane| pane["id"].as_str().map(str::to_string)).ok_or_else(|| anyhow::anyhow!("Main's group"))?;
                let groups_before_empty = all_panes(&wb(window, cx)?).len();
                window.update(cx, |shell, _, cx| shell.fixture_workbench_split_empty(&main_pane_keys, cx))?;
                let with_empty = wait_for(window, cx, "an empty group", move |rb| all_panes(rb).len() == groups_before_empty + 1).await?;
                let empty_group = all_panes(&with_empty)
                    .into_iter()
                    .find(|pane| tabs_of(pane).is_empty())
                    .and_then(|pane| pane["id"].as_str().map(str::to_string))
                    .ok_or_else(|| anyhow::anyhow!("the empty group: {with_empty}"))?;
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(MAIN, window, cx))?, "focus Main's composer");
                draw(window, cx)?;
                settle(cx, 120).await;
                let main_draft = window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))?;
                let mut reached = false;
                for _ in 0..all_panes(&with_empty).len() {
                    key(window, cx, "cmd-alt-]")?;
                    settle(cx, 300).await;
                    if wb(window, cx)?["focusedPane"] == empty_group.as_str() {
                        reached = true;
                        break;
                    }
                }
                anyhow::ensure!(reached, "Mod+Alt+] reaches the empty group");
                let empty_owner = window.update(cx, |shell, window, cx| shell.fixture_workbench_keyboard_chat(window, cx))?;
                anyhow::ensure!(empty_owner.is_none(), "no composer owns the keyboard in an empty group: {empty_owner:?}");
                let escapes_empty = window.update(cx, |shell, _, _| shell.fixture_workbench_escape_log().len())?;
                key(window, cx, "x")?;
                key(window, cx, "enter")?;
                key(window, cx, "escape")?;
                settle(cx, 300).await;
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))? == main_draft, "typing in the empty group never reaches Main's draft");
                let empty_escapes: Vec<(Option<String>, &str)> = window.update(cx, |shell, _, _| shell.fixture_workbench_escape_log())?.into_iter().skip(escapes_empty).collect();
                anyhow::ensure!(empty_escapes.iter().all(|(target, outcome)| target.as_deref() != Some(MAIN) && *outcome != "interrupt"), "Escape in the empty group stops nothing in Main: {empty_escapes:?}");
                anyhow::ensure!(merge_engine_chats(&reader, &state, cx).await?.len() == rows_before_keys, "nothing was sent or created from the empty group");
                window.update(cx, |shell, _, cx| shell.fixture_workbench_close_pane(&empty_group, cx))?;
                wait_for(window, cx, "the empty group closed", move |rb| all_panes(rb).len() == groups_before_empty).await?;
                progress(&output, &mut record, "keyboardRouting".into(), serde_json::json!({"chooser": keyed, "emptyGroup": empty_group}));

                // w20j2. Failure cases 44 and 46: a keymap an older build
                // saved (Mod+Alt+N already New session, Mod+T on the
                // terminal) loads through the real migration; Reset and the
                // recorder refuse chords another saved shortcut, a fixed key
                // or a contextual key (composer Stash, browser keys) owns,
                // leave every saved value unchanged, and still accept the
                // compatible defaults (Save file's Mod+S, Reload's own).
                let old_dir = output.join("old-keymap-settings");
                std::fs::create_dir_all(&old_dir)?;
                std::fs::write(
                    settings::UiSettings::path(&old_dir),
                    serde_json::json!({"keymap": {"newSession": "mod-alt-n", "toggleTerminal": "mod-t", "saveFile": "mod-shift-s", "browserReload": "mod-alt-r"}}).to_string(),
                )?;
                let loaded = settings::UiSettings::load(&old_dir).keymap;
                use settings::ShortcutId as Sid;
                anyhow::ensure!(
                    loaded.get(Sid::NewSession) == "mod-alt-n" && loaded.get(Sid::NewThreadChooser).is_empty() && loaded.get(Sid::ToggleTerminal) == "mod-t" && loaded.get(Sid::NextGroup) == "mod-alt-]",
                    "the upgrade keeps saved bindings and leaves a taken new default unbound: {loaded:?}"
                );
                let punctuation_dir = old_dir.join("native-punctuation");
                std::fs::create_dir_all(&punctuation_dir)?;
                std::fs::write(settings::UiSettings::path(&punctuation_dir), serde_json::json!({"keymap": {"toggleTerminal": "mod-{"}}).to_string())?;
                let punctuation_keymap = settings::UiSettings::load(&punctuation_dir).keymap;
                anyhow::ensure!(punctuation_keymap.get(Sid::ToggleTerminal) == "mod-{" && punctuation_keymap.get(Sid::PreviousTab).is_empty(), "a saved native brace chord keeps ownership of the shifted-bracket default: {punctuation_keymap:?}");
                let keymap_before = window.update(cx, |shell, _, _| shell.fixture_saved_keymap())?;
                window.update(cx, |shell, _, cx| {
                    shell.fixture_load_keymap(loaded.clone(), cx);
                    shell.fixture_open_shortcuts(cx);
                })?;
                for _ in 0..4 {
                    draw(window, cx)?;
                    settle(cx, 80).await;
                }
                let page = window.update(cx, |shell, _, _| shell.fixture_shortcuts_page())?.ok_or_else(|| anyhow::anyhow!("the Shortcuts page"))?;
                let loaded_json = serde_json::to_value(&loaded)?;
                let saved = |cx: &mut AsyncApp| window.update(cx, |shell, _, _| shell.fixture_saved_keymap());
                cx.update(|cx| page.update(cx, |page, cx| page.fixture_reset(Sid::NewThreadChooser, cx)));
                let refused_reset = cx.update(|cx| page.read(cx).fixture_readback());
                anyhow::ensure!(
                    refused_reset["notice"].as_str().is_some_and(|notice| notice.starts_with("Can't reset:") && notice.contains("New session"))
                        && refused_reset["keymap"] == loaded_json
                        && saved(cx)? == loaded_json,
                    "Reset onto New session's saved chord is refused and changes nothing: {refused_reset}"
                );
                shot(window, cx, output.join("w20j2-reset-refused.png")).await?;
                let mut refused_records = Vec::new();
                for (keystroke, owner) in [
                    ("cmd-]", "the browser's Forward"),
                    ("cmd-[", "the browser's Back"),
                    ("cmd-s", "the composer's Stash prompt"),
                    ("cmd-l", "the browser's address bar"),
                    ("cmd-k", "the command palette"),
                    ("cmd-shift-enter", "the composer's alternate send"),
                    ("cmd-left", "text editing"),
                    ("left", "text editing and focus navigation"),
                    ("ctrl-a", "text editing"),
                    ("x", "text entry"),
                    ("cmd-alt-n", ""),
                    ("cmd-{", ""),
                    ("cmd-}", ""),
                ] {
                    cx.update(|cx| page.update(cx, |page, cx| page.fixture_record(Sid::NextGroup, keystroke, cx)));
                    let readback = cx.update(|cx| page.read(cx).fixture_readback());
                    let notice = readback["notice"].as_str().unwrap_or_default().to_string();
                    anyhow::ensure!(
                        !notice.is_empty() && (owner.is_empty() || notice.ends_with(&format!("is reserved for {owner}."))) && readback["keymap"] == loaded_json && saved(cx)? == loaded_json,
                        "recording {keystroke} for Next group is refused ({owner}) and nothing changes: {readback}"
                    );
                    refused_records.push(serde_json::json!({"keystroke": keystroke, "notice": notice}));
                }
                cx.update(|cx| page.update(cx, |page, cx| {
                    page.fixture_reset(Sid::SaveFile, cx);
                    page.fixture_reset(Sid::BrowserReload, cx);
                    page.fixture_record(Sid::NewThreadChooser, "cmd-alt-g", cx);
                }));
                settle(cx, 100).await;
                let accepted = saved(cx)?;
                anyhow::ensure!(
                    accepted["saveFile"] == "mod-s" && accepted["browserReload"] == "mod-shift-r" && accepted["newThreadChooser"] == "mod-alt-g"
                        && accepted["newSession"] == "mod-alt-n" && accepted["toggleTerminal"] == "mod-t",
                    "compatible defaults reset, a free chord records, saved custom bindings stay: {accepted}"
                );
                window.update(cx, |shell, _, cx| {
                    shell.fixture_load_keymap(serde_json::from_value(keymap_before.clone()).unwrap_or_default(), cx);
                    shell.fixture_leave_settings(false, cx);
                })?;
                settle(cx, 300).await;
                progress(&output, &mut record, "shortcutConflicts".into(), serde_json::json!({"loaded": loaded_json, "refusedReset": refused_reset, "refusedRecords": refused_records, "accepted": accepted}));

                // w20k. Failure cases 48-55: background work. The real host's
                // view first (fixture children are legacy ancestry: never
                // stoppable), then a scripted host for the rest.
                use luna_proto::{TaskWorkKind as WorkKind, TaskWorkState as WorkState, TaskWorkStopStatus as StopStatus};
                let work = |cx: &mut AsyncApp, chat: &str| {
                    let chat = chat.to_string();
                    window.update(cx, move |shell, _, cx| {
                        shell
                            .fixture_workbench_composer(&chat, cx)
                            .map(|composer| composer.read(cx).fixture_work_readback(cx))
                            .unwrap_or(serde_json::Value::Null)
                    })
                };
                let on_work = |cx: &mut AsyncApp, chat: &str, f: Box<dyn FnOnce(&mut composer::Composer, &mut gpui::Context<composer::Composer>)>| {
                    let chat = chat.to_string();
                    window.update(cx, move |shell, _, cx| {
                        if let Some(composer) = shell.fixture_workbench_composer(&chat, cx) {
                            composer.update(cx, f);
                        }
                    })
                };
                activate(window, cx)?;
                click_tab(window, cx, MAIN).await?;
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(MAIN, window, cx))?, "focus Main's composer");
                let mut real = serde_json::Value::Null;
                for _ in 0..60 {
                    draw(window, cx)?;
                    real = window.update(cx, |shell, _, cx| shell.fixture_task_work_view(cx))?;
                    if real["taskId"] == MAIN && real["status"].as_str().is_some_and(|status| !status.starts_with("Loading")) {
                        break;
                    }
                    settle(cx, 100).await;
                }
                anyhow::ensure!(real["taskId"] == MAIN && real["status"] == "Live", "the host reports Main's work through the focused watch: {real}");
                let real_rows = work(cx, MAIN)?["rows"].clone();
                anyhow::ensure!(
                    real_rows.as_array().is_none_or(|rows| rows.iter().filter(|row| row["kind"] == "Child chat").all(|row| row["selectable"] == false)),
                    "legacy children (ancestry only) are never stoppable: {real_rows}"
                );
                // The scripted host.
                window.update(cx, |shell, _, cx| shell.fixture_task_work_fake(Some(Default::default()), cx))?;
                let main_items = vec![
                    work_item("w-child", CHILD_B, "Port the parser (child)", WorkKind::ChildChat, WorkState::Running, true, true, true, false, Some((MAIN, MAIN))),
                    work_item("w-grandchild", MAIN, "Parser tests (grandchild)", WorkKind::NativeChild, WorkState::Running, true, true, true, false, Some((MAIN, CHILD_A))),
                    work_item("w-workflow", MAIN, "Release checklist workflow", WorkKind::Workflow, WorkState::Waiting, true, true, true, true, Some((MAIN, MAIN))),
                    work_item("w-service", MAIN, "Dev server", WorkKind::Service, WorkState::Running, true, false, true, false, Some((MAIN, MAIN))),
                    work_item("w-later", MAIN, "Child's own later message", WorkKind::ChildChat, WorkState::Running, false, false, true, false, Some((CHILD_A, CHILD_A))),
                    work_item("w-legacy", "f0f0f0f0-0000-4000-8000-00000000dead", "Legacy child (gone)", WorkKind::ChildChat, WorkState::Running, false, false, false, false, None),
                    work_item("w-owner", MAIN, "Result owned by Child B", WorkKind::ResultDelivery, WorkState::Waiting, false, false, false, false, Some((CHILD_B, MAIN))),
                ];
                window.update(cx, |shell, _, cx| shell.fixture_fake_work(MAIN, main_items.clone(), cx))?;
                draw(window, cx)?;
                let seeded = work(cx, MAIN)?;
                let selectable: Vec<String> = seeded["rows"].as_array().into_iter().flatten().filter(|row| row["selectable"] == true).filter_map(|row| row["id"].as_str().map(str::to_string)).collect();
                anyhow::ensure!(
                    seeded["chip"] == true && seeded["blocking"] == 3 && selectable == ["w-child", "w-grandchild", "w-workflow"],
                    "owned child, grandchild and workflow are stoppable; the service, later child and legacy row are not: {seeded}"
                );
                anyhow::ensure!(
                    seeded["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["id"] == "w-service" && row["group"] == "Services")
                        && rows.iter().any(|row| row["id"] == "w-later" && row["group"] == "Elsewhere")),
                    "the service and the later independent child keep their own groups: {seeded}"
                );
                // Failure cases 56-60: Open routes. Each row opens its own
                // chat, its lifecycle owner, or a service's commands; unknown
                // chats offer nothing; the shell routes it like Related chats.
                let opens: std::collections::HashMap<String, serde_json::Value> = seeded["rows"].as_array().into_iter().flatten().filter_map(|row| Some((row["id"].as_str()?.to_string(), row["open"].clone()))).collect();
                anyhow::ensure!(
                    opens["w-child"]["taskId"] == CHILD_B && opens["w-child"]["label"] == "Open chat"
                        && opens["w-owner"]["taskId"] == CHILD_B && opens["w-owner"]["label"] == "Open owner"
                        && opens["w-service"]["taskId"] == MAIN && opens["w-service"]["commands"] == true
                        && opens["w-legacy"].is_null() && opens["w-grandchild"].is_null(),
                    "Open targets follow each row's chat or owner, never an unknown chat: {opens:?}"
                );
                let open_row = |cx: &mut AsyncApp, chat: &str, row: &str| {
                    let (chat, row) = (chat.to_string(), row.to_string());
                    window.update(cx, move |shell, window, cx| {
                        shell
                            .fixture_workbench_composer(&chat, cx)
                            .is_some_and(|composer| composer.update(cx, |composer, cx| {
                                composer.fixture_open_work(cx);
                                composer.fixture_work_open(&row, window, cx)
                            }))
                    })
                };
                anyhow::ensure!(open_row(cx, MAIN, "w-child")?, "the child row offers Open");
                let opened_child = wait_for(window, cx, "Open chat focuses Child B in the workbench", |rb| rb["focusedChat"] == CHILD_B && rb["main"] == MAIN).await?;
                let route = window.update(cx, |shell, _, _| shell.fixture_last_work_open())?;
                anyhow::ensure!(route["route"] == "chat" && route["taskId"] == CHILD_B && work(cx, MAIN)?["menu"].is_null(), "Open routes Child B through related-chat routing and closes the popover: {route} {}", opened_child["focusedChat"]);
                click_tab(window, cx, MAIN).await?;
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(MAIN, window, cx))?, "focus Main again");
                draw(window, cx)?;
                anyhow::ensure!(open_row(cx, MAIN, "w-service")?, "the service row offers its commands");
                let commands = window.update(cx, |shell, _, _| shell.fixture_last_work_open())?;
                anyhow::ensure!(commands["route"] == "commands" && commands["terminalOpen"] == true, "a service opens its own chat's project commands: {commands}");
                window.update(cx, |shell, window, cx| shell.fixture_close_terminal(window, cx))?;
                settle(cx, 400).await;
                // Failure case 50: only the focused chat's composer has the chip;
                // a second pane's watch replaces it and pages by the aggregate.
                let other = work(cx, CHILD_A)?;
                anyhow::ensure!(other.is_null() || other["chip"] == false, "an unfocused composer shows no Work chip: {other}");
                let many: Vec<_> = (0..120).map(|index| work_item(&format!("w-a-{index:03}"), CHILD_A, &format!("Batch item {index}"), WorkKind::Shell, WorkState::Running, true, true, true, false, Some((CHILD_A, CHILD_A)))).collect();
                window.update(cx, |shell, _, cx| shell.fixture_fake_work(CHILD_A, many, cx))?;
                click_tab(window, cx, CHILD_A).await?;
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(CHILD_A, window, cx))?, "focus Child A's composer");
                let mut paged = serde_json::Value::Null;
                for _ in 0..40 {
                    draw(window, cx)?;
                    paged = work(cx, CHILD_A)?;
                    if paged["taskId"] == CHILD_A {
                        break;
                    }
                    settle(cx, 100).await;
                }
                anyhow::ensure!(
                    paged["chip"] == true && paged["loaded"] == 50 && paged["total"] == 120 && paged["blocking"] == 120 && paged["moreOffset"] == 50,
                    "the focused second pane pages and counts from the aggregate: {paged}"
                );
                anyhow::ensure!(work(cx, MAIN)?["chip"] == false, "Main's composer no longer shows the chip");
                on_work(cx, CHILD_A, Box::new(|composer: &mut composer::Composer, cx: &mut gpui::Context<composer::Composer>| composer.fixture_work_more(cx)))?;
                on_work(cx, CHILD_A, Box::new(|composer: &mut composer::Composer, cx: &mut gpui::Context<composer::Composer>| composer.fixture_work_more(cx)))?;
                let all = work(cx, CHILD_A)?;
                let ids: std::collections::HashSet<String> = all["rows"].as_array().into_iter().flatten().filter_map(|row| row["id"].as_str().map(str::to_string)).collect();
                anyhow::ensure!(all["loaded"] == 120 && ids.len() == 120 && all["moreOffset"].is_null(), "Show more appends each page once: {}", all["loaded"]);
                shot(window, cx, output.join("w20k-work-paging.png")).await?;

                // Open from a pane composer's popover routes through the shell
                // too: a Child A row owned by Main opens Main's tab.
                let mut pane_rows = (0..120).map(|index| work_item(&format!("w-a-{index:03}"), CHILD_A, &format!("Batch item {index}"), WorkKind::Shell, WorkState::Running, true, true, true, false, Some((CHILD_A, CHILD_A)))).collect::<Vec<_>>();
                pane_rows.insert(0, work_item("w-a-main", MAIN, "Main's release workflow", WorkKind::Workflow, WorkState::Running, false, false, false, false, Some((MAIN, CHILD_A))));
                window.update(cx, |shell, _, cx| shell.fixture_fake_work(CHILD_A, pane_rows, cx))?;
                draw(window, cx)?;
                anyhow::ensure!(open_row(cx, CHILD_A, "w-a-main")?, "the pane's row offers Open");
                wait_for(window, cx, "Open from the pane focuses Main's tab", |rb| rb["focusedChat"] == MAIN).await?;
                let pane_route = window.update(cx, |shell, _, _| shell.fixture_last_work_open())?;
                anyhow::ensure!(pane_route["route"] == "chat" && pane_route["taskId"] == MAIN, "the pane composer's Open reaches the shell: {pane_route}");
                // Failure case 60: an earlier connection's list never navigates.
                click_tab(window, cx, CHILD_A).await?;
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(CHILD_A, window, cx))?, "focus Child A again");
                draw(window, cx)?;
                let stale_open = window.update(cx, |shell, window, cx| {
                    // The popover opens on the current connection, then the
                    // engine reconnects before its row's Open is clicked.
                    let composer = shell.fixture_workbench_composer(CHILD_A, cx);
                    if let Some(composer) = &composer {
                        composer.update(cx, |composer, cx| composer.fixture_open_work(cx));
                    }
                    shell.fixture_reconnect_engine(cx);
                    composer.is_some_and(|composer| composer.update(cx, |composer, cx| composer.fixture_work_open("w-a-main", window, cx)))
                })?;
                settle(cx, 300).await;
                let refused_open = window.update(cx, |shell, _, _| shell.fixture_last_work_open())?;
                anyhow::ensure!(
                    stale_open && refused_open["route"] == "refused" && wb(window, cx)?["focusedChat"] == CHILD_A,
                    "a list from an earlier connection never navigates: {refused_open}"
                );
                // Restore the controlled frame: the refusal's notice is
                // persistent, so dismiss it with its own Dismiss control
                // (later steps sample the left chrome's background). With
                // projects present the notice is a toast above the composer
                // (the sidebar copy is only for the empty app), so the shell
                // reports where that toast's Dismiss painted.
                let mut dismiss = related(window, cx)?["noticeDismiss"].clone();
                for _ in 0..20 {
                    if dismiss.is_object() {
                        break;
                    }
                    draw(window, cx)?;
                    settle(cx, 50).await;
                    dismiss = related(window, cx)?["noticeDismiss"].clone();
                }
                anyhow::ensure!(dismiss.is_object(), "the refusal's Dismiss control is painted");
                let get = |value: &Value| value.as_f64().unwrap_or(f64::NAN) as f32;
                let dismiss_at = (
                    get(&dismiss["x"]) + get(&dismiss["w"]) / 2.0,
                    get(&dismiss["y"]) + get(&dismiss["h"]) / 2.0,
                );
                click(window, cx, dismiss_at, MouseButton::Left).await?;
                let mut cleared = related(window, cx)?;
                for _ in 0..20 {
                    if cleared["notice"].is_null() {
                        break;
                    }
                    settle(cx, 100).await;
                    cleared = related(window, cx)?;
                }
                anyhow::ensure!(cleared["notice"].is_null(), "Dismiss clears the refusal notice (clicked at {dismiss_at:?}): {}", cleared["notice"]);
                click_tab(window, cx, MAIN).await?;
                anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(MAIN, window, cx))?, "focus Main again");
                for _ in 0..40 {
                    draw(window, cx)?;
                    if work(cx, MAIN)?["taskId"] == MAIN {
                        break;
                    }
                    settle(cx, 100).await;
                }
                // Failure case 51: explicit selection, preview, one request id.
                on_work(cx, MAIN, Box::new(|composer: &mut composer::Composer, cx: &mut gpui::Context<composer::Composer>| {
                    composer.fixture_open_work(cx);
                    for id in ["w-child", "w-grandchild", "w-service", "w-later", "w-legacy"] {
                        composer.fixture_work_toggle(id, cx);
                    }
                }))?;
                draw(window, cx)?;
                shot(window, cx, output.join("w20k-work-popover.png")).await?;
                let chosen = work(cx, MAIN)?;
                anyhow::ensure!(chosen["menu"]["selected"] == serde_json::json!(["w-child", "w-grandchild"]), "only owned stoppable rows can be chosen: {chosen}");
                on_work(cx, MAIN, Box::new(|composer: &mut composer::Composer, cx: &mut gpui::Context<composer::Composer>| composer.fixture_work_review(cx)))?;
                let reviewed = work(cx, MAIN)?;
                anyhow::ensure!(
                    reviewed["menu"]["phase"]["kind"] == "preview" && reviewed["menu"]["phase"]["items"] == serde_json::json!(["w-child", "w-grandchild"]),
                    "the preview names exactly the chosen rows: {reviewed}"
                );
                let statuses: std::collections::HashMap<String, StopStatus> = [("w-child".to_string(), StopStatus::Requested), ("w-grandchild".to_string(), StopStatus::Stopped)].into_iter().collect();
                window.update(cx, |shell, _, cx| shell.fixture_fake_stop(Some(statuses), false, cx))?;
                on_work(cx, MAIN, Box::new(|composer: &mut composer::Composer, cx: &mut gpui::Context<composer::Composer>| composer.fixture_work_confirm(cx)))?;
                let answered = work(cx, MAIN)?;
                let receipt = answered["menu"]["phase"]["answer"]["receipt"].clone();
                anyhow::ensure!(
                    receipt.as_array().is_some_and(|results| results.iter().any(|result| result["workId"] == "w-child" && result["label"] == "Stop requested; not stopped yet")
                        && results.iter().any(|result| result["workId"] == "w-grandchild" && result["label"] == "Stopped")),
                    "Requested never reads as Stopped: {answered}"
                );
                let calls = window.update(cx, |shell, _, cx| shell.fixture_task_work_calls(cx))?;
                anyhow::ensure!(calls["stopCalls"] == 1 && calls["requestIds"].as_array().map_or(0, Vec::len) == 1, "one stop request per preview: {calls}");
                shot(window, cx, output.join("w20k-work-receipt.png")).await?;
                // Failure case 52: a lost answer is unknown and never retried.
                on_work(cx, MAIN, Box::new(|composer: &mut composer::Composer, cx: &mut gpui::Context<composer::Composer>| {
                    composer.fixture_work_refresh(cx);
                    composer.fixture_work_toggle("w-workflow", cx);
                    composer.fixture_work_review(cx);
                }))?;
                window.update(cx, |shell, _, cx| shell.fixture_fake_stop(None, false, cx))?;
                on_work(cx, MAIN, Box::new(|composer: &mut composer::Composer, cx: &mut gpui::Context<composer::Composer>| composer.fixture_work_confirm(cx)))?;
                settle(cx, 1000).await;
                let lost = work(cx, MAIN)?;
                let calls = window.update(cx, |shell, _, cx| shell.fixture_task_work_calls(cx))?;
                anyhow::ensure!(lost["menu"]["phase"]["answer"]["lost"] == true && calls["stopCalls"] == 2, "a lost answer reads unknown and is not retried: {lost} {calls}");
                // Failure case 53: a refused (stale) preview returns to the
                // selection, dropping rows that are no longer stoppable.
                on_work(cx, MAIN, Box::new(|composer: &mut composer::Composer, cx: &mut gpui::Context<composer::Composer>| {
                    composer.fixture_work_refresh(cx);
                    composer.fixture_work_toggle("w-child", cx);
                    composer.fixture_work_review(cx);
                }))?;
                let mut ended = main_items.clone();
                ended[0].state = WorkState::Completed;
                window.update(cx, |shell, _, cx| {
                    shell.fixture_fake_work(MAIN, ended.clone(), cx);
                    shell.fixture_fake_stop(Some(Default::default()), true, cx);
                })?;
                on_work(cx, MAIN, Box::new(|composer: &mut composer::Composer, cx: &mut gpui::Context<composer::Composer>| composer.fixture_work_confirm(cx)))?;
                let refused = work(cx, MAIN)?;
                anyhow::ensure!(
                    refused["menu"]["phase"]["kind"] == "select" && refused["menu"]["selected"] == serde_json::json!([]) && refused["menu"]["note"].is_string(),
                    "a stale preview returns to the selection without the ended row: {refused}"
                );
                // Failure case 54: after the stream ends nothing is stoppable.
                window.update(cx, |shell, _, cx| shell.fixture_end_task_work_watch(cx))?;
                let previews_before = window.update(cx, |shell, _, cx| shell.fixture_task_work_calls(cx))?["previewCalls"].clone();
                on_work(cx, MAIN, Box::new(|composer: &mut composer::Composer, cx: &mut gpui::Context<composer::Composer>| {
                    composer.fixture_work_toggle("w-grandchild", cx);
                    composer.fixture_work_review(cx);
                }))?;
                let unknown = work(cx, MAIN)?;
                anyhow::ensure!(
                    unknown["status"] == "unknown" && unknown["chip"] == true && unknown["menu"]["selected"] == serde_json::json!([])
                        && window.update(cx, |shell, _, cx| shell.fixture_task_work_calls(cx))?["previewCalls"] == previews_before,
                    "an ended stream reads unknown and nothing can be stopped: {unknown}"
                );
                // Failure case 55: Escape closes the popover, never the stop path.
                let escapes_before = window.update(cx, |shell, _, _| shell.fixture_workbench_escape_log().len())?;
                key(window, cx, "escape")?;
                settle(cx, 200).await;
                anyhow::ensure!(
                    work(cx, MAIN)?["menu"].is_null() && window.update(cx, |shell, _, _| shell.fixture_workbench_escape_log().len())? == escapes_before,
                    "Escape closes the Work popover without reaching stop handling"
                );
                window.update(cx, |shell, _, cx| shell.fixture_task_work_fake(None, cx))?;
                progress(&output, &mut record, "backgroundWork".into(), serde_json::json!({"real": real, "seeded": seeded, "paged": paged["loaded"], "receipt": receipt, "lost": lost["menu"], "refused": refused["menu"], "unknown": unknown["status"]}));

                // w20l. Failure case 61: a queued message held because its
                // provider was unavailable before any provider I/O. It keeps
                // its exact row and revision, reads the specific label, stays
                // held across a reconnect (no automatic resend), and only its
                // own composer's explicit Send now acts on it.
                let held_id = "w20l-held-provider-unavailable";
                let held_text = W20L_PROMPT;
                let held = luna_doc::QueuedMessage {
                    input_items: Vec::new(),
                    id: held_id.into(),
                    text: held_text.into(),
                    attachments: Vec::new(),
                    hold_for_turn_end: false,
                    follow_up_behavior: luna_proto::FollowUpBehavior::Queue,
                    hold_reason: Some(luna_doc::QueueHoldReason::ExecutionUnavailable),
                    issued_by: queue_device.clone(),
                    issued_at: Utc::now().timestamp_millis(),
                    edited_at: None,
                    revision: 3,
                    execution: None,
                    status: luna_doc::QueueMessageStatus::Held,
                    delivery_gate: None,
                };
                queue_doc_host.open(CHILD_A)?.doc().push_queued(&held)?;
                let queue_rows = |cx: &mut AsyncApp, chat: &str| {
                    let chat = chat.to_string();
                    window.update(cx, move |shell, _, cx| {
                        shell
                            .fixture_workbench_composer(&chat, cx)
                            .map(|composer| composer.read(cx).fixture_queue_rows(cx))
                            .unwrap_or(serde_json::Value::Null)
                    })
                };
                let held_row = |rows: &serde_json::Value| rows.as_array().and_then(|rows| rows.iter().find(|row| row["id"] == held_id).cloned());
                click_tab(window, cx, CHILD_A).await?;
                let mut child_rows = serde_json::Value::Null;
                for _ in 0..60 {
                    draw(window, cx)?;
                    child_rows = queue_rows(cx, CHILD_A)?;
                    if held_row(&child_rows).is_some() {
                        break;
                    }
                    settle(cx, 100).await;
                }
                let row = held_row(&child_rows).ok_or_else(|| anyhow::anyhow!("the held row reaches Child A's composer: {child_rows}"))?;
                anyhow::ensure!(
                    row["holdReason"] == "ExecutionUnavailable" && row["holdLabel"] == "Provider unavailable — review and send"
                        && row["revision"] == 3 && row["text"] == held_text && row["primary"] == "Send now" && row["primaryEnabled"] == true,
                    "the held row reads its specific label with its exact revision and an explicit Send now: {row}"
                );
                anyhow::ensure!(held_row(&queue_rows(cx, MAIN)?).is_none(), "the row is Child A's alone");
                shot(window, cx, output.join("w20l-provider-unavailable-hold.png")).await?;
                // A reconnect resends nothing: the exact row stays held.
                let texts_before = window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(CHILD_A, cx))?;
                window.update(cx, |shell, _, cx| shell.fixture_reconnect_engine(cx))?;
                settle(cx, 2000).await;
                draw(window, cx)?;
                let after_reconnect = held_row(&queue_rows(cx, CHILD_A)?).ok_or_else(|| anyhow::anyhow!("the held row survives the reconnect"))?;
                anyhow::ensure!(
                    after_reconnect["revision"] == 3 && after_reconnect["holdReason"] == "ExecutionUnavailable"
                        && window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(CHILD_A, cx))? == texts_before,
                    "no automatic resend after a reconnect: {after_reconnect}"
                );
                // Explicit Send now from Child A's own composer. The row was
                // seeded without an executable grant. The explicit local
                // command supplies one-use consent; issued_by alone never
                // authorizes delivery. A host refusal must have a receipt.
                // Proof comes from the
                // durable command ledger: a NEW sendQueuedNow receipt for
                // Child A correlated to this row, settled, and a matching
                // disposition; nothing new for Main.
                let main_texts = window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(MAIN, cx))?;
                let main_draft = window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))?;
                let child_sequence = command_store.task_command_snapshot(CHILD_A)?.snapshot.task_sequence;
                let main_sequence = command_store.task_command_snapshot(MAIN)?.snapshot.task_sequence;
                anyhow::ensure!(
                    command_store.task_command_receipt_for_correlation(CHILD_A, held_id)?.is_none(),
                    "the seeded row has no earlier delivery receipt"
                );
                let acted = window.update(cx, |shell, _, cx| {
                    shell
                        .fixture_workbench_composer(CHILD_A, cx)
                        .is_some_and(|composer| composer.update(cx, |composer, cx| composer.fixture_queue_primary(held_id, cx)))
                })?;
                anyhow::ensure!(acted, "Child A's Send now acts on the held row");
                let new_send = |task: &str, after: u64| -> anyhow::Result<Vec<luna_proto::CommandReceipt>> {
                    Ok(command_store
                        .read_task_commands(task, after, 100)?
                        .events
                        .into_iter()
                        .map(|event| event.receipt)
                        .filter(|receipt| receipt.intent == "sendQueuedNow" || receipt.intent == "session")
                        .collect())
                };
                // The w20l peer reports SessionStarted, so acceptance becomes
                // durable: only Applied, sent exactly once, row gone, proves
                // it. Pending, a refusal or a timeout fails (with the last
                // observation saved as evidence).
                let mut settled = None;
                let mut last_seen = serde_json::Value::Null;
                for _ in 0..160 {
                    settle(cx, 150).await;
                    draw(window, cx)?;
                    let receipts = new_send(CHILD_A, child_sequence)?;
                    let receipt = receipts
                        .iter()
                        .filter(|receipt| receipt.correlation_id.as_deref() == Some(held_id))
                        .next_back()
                        .cloned();
                    let rows = queue_rows(cx, CHILD_A)?;
                    let texts = window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(CHILD_A, cx))?;
                    let sent = texts.iter().filter(|text| text.as_str() == held_text).count();
                    let still = held_row(&rows);
                    last_seen = serde_json::json!({
                        "receipts": receipts,
                        "rows": rows,
                        "texts": texts,
                        "sent": sent,
                    });
                    let Some(receipt) = receipt else {
                        continue;
                    };
                    if receipt.status == luna_proto::CommandReceiptStatus::Applied && sent == 1 && still.is_none() {
                        settled = Some(serde_json::json!({
                            "status": receipt.status,
                            "commandId": receipt.command_id,
                            "sent": sent,
                        }));
                        break;
                    }
                    if receipt.status.is_terminal() && receipt.status != luna_proto::CommandReceiptStatus::Applied {
                        break;
                    }
                }
                let Some(settled) = settled else {
                    let _ = std::fs::write(
                        output.join("w20l-timeout.json"),
                        serde_json::to_vec_pretty(&last_seen).unwrap_or_default(),
                    );
                    anyhow::bail!(
                        "Send now never proved delivery (Applied sendQueuedNow for Child A with this row's correlation, sent once, row gone); last observation saved to w20l-timeout.json: {last_seen}"
                    );
                };
                anyhow::ensure!(
                    new_send(MAIN, main_sequence)?.is_empty()
                        && window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(MAIN, cx))? == main_texts
                        && window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))? == main_draft
                        && held_row(&queue_rows(cx, MAIN)?).is_none(),
                    "Send now acts for Child A only: no new send on Main, Main's draft/queue/transcript unchanged: {settled}"
                );
                progress(&output, &mut record, "providerUnavailableHold".into(), serde_json::json!({"held": row, "afterReconnect": after_reconnect, "sendNow": settled}));
                click_tab(window, cx, MAIN).await?;

                // w20m. Failure cases 62-72: host-owned automations (slice 5)
                // through the real RPC and the production 2-second worker.
                // The right pane's Automations surface follows Main, so each
                // scope here is a real Main selection. Delivery goes to a
                // fresh automation chat: earlier steps can leave Main with
                // unresolved delivery work, which the host rightly protects.
                // Main and Child A must receive nothing (command ledger).
                // Every change is reviewed by the host and applied as that
                // exact request; a lost answer resolves only through its
                // receipt.
                macro_rules! panel {
                    ($settings:expr, |$p:ident, $c:ident| $body:expr) => {
                        window.update(cx, |shell, _, cx| {
                            shell
                                .fixture_automations($settings)
                                .map(|panel| panel.update(cx, |$p, $c| $body))
                        })?
                    };
                }
                const AUTO_CHAT: &str = "e2e2e2e2-0000-4000-8000-000000000001";
                const REMOTE_CHAT: &str = "e3e3e3e3-0000-4000-8000-000000000001";
                const REMOTE_HOST: &str = "w20m-remote-host";
                let right_was_open = frame(window, cx)?["rightOpen"].as_bool() == Some(true);
                let main_sequence = command_store.task_command_snapshot(MAIN)?.snapshot.task_sequence;
                let child_sequence = command_store.task_command_snapshot(CHILD_A)?.snapshot.task_sequence;
                reader
                    .call(
                        "Mutate",
                        serde_json::json!({
                            "op": "createChat",
                            "chatId": AUTO_CHAT,
                            "spaceId": PROJECT,
                            "cwd": automation_cwd.to_string_lossy(),
                            "config": serde_json::to_value(config())?,
                        }),
                    )
                    .await?;
                automation_workspace.rename_chat(AUTO_CHAT, "Automation checks")?;
                // Another host's chat as this device sees it: a replicated
                // row whose owner is that host.
                let mut remote_row = automation_workspace.chat(MAIN)?.ok_or_else(|| anyhow::anyhow!("Main's engine row"))?;
                remote_row.id = REMOTE_CHAT.into();
                remote_row.device_id = REMOTE_HOST.into();
                remote_row.title = Some("Office Mini notes".into());
                automation_workspace.import_chat_row(&remote_row)?;
                let auto_row = automation_workspace.chat(AUTO_CHAT)?.ok_or_else(|| anyhow::anyhow!("the automation chat's engine row"))?;
                let remote_row = automation_workspace.chat(REMOTE_CHAT)?.ok_or_else(|| anyhow::anyhow!("the remote row"))?;
                // The UI's chat list is fixture-managed (its chats watch is
                // off): mirror exactly the engine's rows, and the paired host.
                cx.update(|cx| {
                    state.update(cx, |state, cx| {
                        state.chats.push(auto_row);
                        state.chats.push(remote_row);
                        state.devices.push(
                            serde_json::from_value(serde_json::json!({
                                "id": REMOTE_HOST, "name": "Office Mini", "platform": "macos", "lastSeenAt": null
                            }))
                            .expect("device"),
                        );
                        cx.notify();
                    })
                });
                // Case 62: a conversation owned by another host shows no local
                // automations, names its host and offers no editor.
                select(cx, Some(REMOTE_CHAT));
                window.update(cx, |shell, _, cx| shell.fixture_open_automations(cx))?;
                wait_automations(window, cx, false, "the remote conversation's view", 60, |rb| {
                    rb["scope"]["conversation"] == REMOTE_CHAT && rb["loading"] == false
                })
                .await?;
                panel!(false, |p, c| p.fixture_new(c));
                draw(window, cx)?;
                let remote = automations(window, cx, false)?;
                anyhow::ensure!(
                    remote["scope"]["conversation"] == REMOTE_CHAT
                        && remote["foreignHost"] == "Office Mini"
                        && remote["rows"].as_array().is_some_and(Vec::is_empty)
                        && remote["editor"].is_null()
                        && remote["error"].as_str().is_some_and(|error| error.contains("managed on Office Mini")),
                    "another host's conversation names its host and offers no local editor: {remote}"
                );
                shot(window, cx, output.join("w20m-automations-remote-host.png")).await?;
                select(cx, Some(AUTO_CHAT));
                wait_for(window, cx, "the automation chat as Main", |rb| rb["loaded"] == true && rb["main"] == AUTO_CHAT).await?;
                window.update(cx, |shell, _, cx| shell.fixture_open_automations(cx))?;
                let empty = wait_automations(window, cx, false, "the automation chat's view", 60, |rb| {
                    rb["scope"]["conversation"] == AUTO_CHAT && rb["loading"] == false && rb["error"].is_null()
                })
                .await?;
                anyhow::ensure!(
                    empty["foreignHost"].is_null() && empty["rows"].as_array().is_some_and(Vec::is_empty),
                    "a fresh local chat starts with no automations: {empty}"
                );
                // Cases 63-64: local limits name the problem (PR watch: an
                // owner/repository, polls 1..1440 minutes, 1..100 wakes).
                panel!(false, |p, c| p.fixture_new(c));
                panel!(false, |p, c| p.fixture_set("name", "W20M nightly check", c));
                panel!(false, |p, c| p.fixture_set("prompt", W20M_PROMPT, c));
                panel!(false, |p, c| p.fixture_set("kind", "pr", c));
                let mut refusals = Vec::new();
                for (field, value, expected) in [
                    ("repository", "not-a-repository", "Enter the repository as owner/repository."),
                    ("repository", "novnski/luna", "Enter a whole number for the pull request number."),
                    ("number", "12", "Allow 1 to 100 wakes."),
                    ("budget", "101", "Allow 1 to 100 wakes."),
                    // The check interval is validated before the budget.
                    ("poll", "1441", "Check every 1 to 1440 minutes."),
                    ("poll", "0", "Check every 1 to 1440 minutes."),
                ] {
                    panel!(false, |p, c| p.fixture_set(field, value, c));
                    if field == "repository" && value == "novnski/luna" {
                        // A fresh editor's PR number is empty; poll stays 15.
                        panel!(false, |p, c| p.fixture_set("budget", "0", c));
                    }
                    let built = automations(window, cx, false)?["editor"]["built"].clone();
                    anyhow::ensure!(built == expected, "{field}={value} is refused locally with its reason ({expected}): {built}");
                    refusals.push(serde_json::json!({"field": field, "value": value, "refusal": built}));
                }
                panel!(false, |p, c| p.fixture_set("poll", "15", c));
                panel!(false, |p, c| p.fixture_set("budget", "100", c));
                let valid_watch = automations(window, cx, false)?["editor"]["built"].clone();
                anyhow::ensure!(valid_watch.is_null(), "a bounded PR watch builds: {valid_watch}");
                // Case 65: a one-shot entered as wall time in its IANA zone;
                // the host's preview names exactly that instant.
                let zone = "Europe/Bucharest";
                let now_ms = Utc::now().timestamp_millis();
                let due = (now_ms + 30_000).div_euclid(60_000) * 60_000 + 60_000;
                let local = luna_engine::automation::format_in_zone(due, zone);
                panel!(false, |p, c| p.fixture_set("kind", "once", c));
                panel!(false, |p, c| p.fixture_set("timezone", zone, c));
                panel!(false, |p, c| p.fixture_set("date", &local[..10], c));
                panel!(false, |p, c| p.fixture_set("time", &local[11..16], c));
                panel!(false, |p, c| p.fixture_review(c));
                let reviewed = wait_automations(window, cx, false, "the host's review", 50, |rb| {
                    let phase = rb["editor"]["phase"].as_str().unwrap_or("");
                    phase == "reviewed" || phase.starts_with("error")
                })
                .await?;
                anyhow::ensure!(
                    reviewed["editor"]["phase"] == "reviewed"
                        && reviewed["editor"]["previewCurrent"] == true
                        && reviewed["editor"]["nextOccurrencesMs"] == serde_json::json!([due])
                        && reviewed["editor"]["expectedRevision"] == 0,
                    "the host previews exactly {local} for a new automation: {reviewed}"
                );
                let request_id = reviewed["editor"]["requestId"].as_str().unwrap_or_default().to_string();
                let automation_id = reviewed["editor"]["automationId"].as_str().unwrap_or_default().to_string();
                anyhow::ensure!(!request_id.is_empty() && !automation_id.is_empty(), "the editor holds its ids");
                shot(window, cx, output.join("w20m-automation-reviewed.png")).await?;
                // Case 66: an edit after review invalidates it; restoring the
                // exact request makes the same review current again.
                panel!(false, |p, c| p.fixture_set("name", "W20M nightly check (edited)", c));
                let edited = automations(window, cx, false)?;
                let refused = panel!(false, |p, c| p.fixture_apply(c)) == Some(true);
                anyhow::ensure!(
                    edited["editor"]["previewCurrent"] == false && !refused,
                    "an edit after review disables Apply: {edited}"
                );
                panel!(false, |p, c| p.fixture_set("name", "W20M nightly check", c));
                // Case 67: the answer is lost. The same request id stays
                // unconfirmed (no new id, no silent resend) until its receipt
                // is read.
                panel!(false, |p, _c| p.fixture_lose_next_mutation());
                let applied = panel!(false, |p, c| p.fixture_apply(c)) == Some(true);
                anyhow::ensure!(applied, "Apply runs for the exact reviewed request");
                let unknown = wait_automations(window, cx, false, "the lost answer", 50, |rb| {
                    rb["editor"]["phase"].as_str().is_some_and(|phase| phase.starts_with("unknown") || phase.starts_with("error"))
                })
                .await?;
                anyhow::ensure!(
                    unknown["editor"]["phase"] == "unknown" && unknown["editor"]["requestId"] == request_id.as_str(),
                    "a lost answer is unconfirmed under the same request id: {unknown}"
                );
                shot(window, cx, output.join("w20m-automation-unconfirmed.png")).await?;
                panel!(false, |p, c| p.fixture_check(c));
                let saved = wait_automations(window, cx, false, "Check status resolves the change", 80, |rb| {
                    rb["editor"].is_null()
                        && rb["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["id"] == automation_id.as_str()))
                })
                .await?;
                let receipt: Option<luna_proto::AutomationMutationReceipt> = reader
                    .call_as(
                        luna_rpc::methods::GET_AUTOMATION_MUTATION_RECEIPT,
                        serde_json::json!({ "requestId": request_id }),
                    )
                    .await?;
                let receipt = receipt.ok_or_else(|| anyhow::anyhow!("the host holds the receipt for {request_id}"))?;
                let row = saved["rows"].as_array().and_then(|rows| rows.iter().find(|row| row["id"] == automation_id.as_str()).cloned()).unwrap_or_default();
                anyhow::ensure!(
                    receipt.record.id == automation_id
                        && receipt.record.revision == 1
                        && row["revision"] == 1
                        && row["nextDueMs"] == due
                        && row["target"]["taskId"] == AUTO_CHAT
                        && row["status"].as_str().is_some_and(|status| status.starts_with("Active · next") && status.contains(zone)),
                    "the receipt for the same request id resolves the change, saved once at revision 1: {row}"
                );
                // Cases 68-69: the worker delivers the occurrence to its chat;
                // the history shows its own scheduled/claimed times and ids,
                // and only Completed means the turn finished.
                panel!(false, |p, c| p.fixture_toggle_runs(&automation_id, c));
                let auto_texts_before = window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(AUTO_CHAT, cx))?;
                let mut seen_states = std::collections::BTreeSet::new();
                let mut completed = None;
                let mut last_runs = Value::Null;
                for _ in 0..1500 {
                    draw(window, cx)?;
                    last_runs = automations(window, cx, false)?["runs"][automation_id.as_str()].clone();
                    for run in last_runs["items"].as_array().into_iter().flatten() {
                        seen_states.insert(run["state"].as_str().unwrap_or_default().to_string());
                        if run["state"] == "Completed" {
                            completed = Some(run.clone());
                        }
                    }
                    if completed.is_some() {
                        break;
                    }
                    settle(cx, 100).await;
                }
                let Some(run) = completed else {
                    let _ = std::fs::write(
                        output.join("w20m-timeout.json"),
                        serde_json::to_vec_pretty(&serde_json::json!({"runs": last_runs, "seen": seen_states})).unwrap_or_default(),
                    );
                    anyhow::bail!("the one-shot never completed in the automation chat; last history saved to w20m-timeout.json: {last_runs}");
                };
                let auto_texts = window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(AUTO_CHAT, cx))?;
                let delivered = auto_texts.iter().filter(|text| text.contains(W20M_PROMPT)).count();
                anyhow::ensure!(
                    run["taskId"] == AUTO_CHAT
                        && run["scheduledAtMs"] == due
                        && run["claimedAtMs"].as_i64().is_some_and(|claimed| claimed >= due)
                        && run["coalesced"] == 0
                        && run["commandId"].as_str().is_some_and(|id| !id.is_empty())
                        && run["messageId"].as_str().is_some_and(|id| !id.is_empty())
                        && last_runs["items"].as_array().map(Vec::len) == Some(1)
                        && delivered == 1
                        && auto_texts_before.iter().all(|text| !text.contains(W20M_PROMPT)),
                    "one occurrence delivered once to the automation chat, with its own times and ids: {run} (seen {seen_states:?})"
                );
                shot(window, cx, output.join("w20m-automation-completed.png")).await?;
                // Case 70: Pause is reviewed and confirmed; a spent one-shot
                // reads as needing a new future time, never a plain Resume.
                panel!(false, |p, c| p.fixture_action(&automation_id, "pause", c));
                wait_automations(window, cx, false, "the pause review", 50, |rb| rb["action"]["phase"] == "reviewed").await?;
                panel!(false, |p, c| p.fixture_confirm(c));
                let paused = wait_automations(window, cx, false, "the pause applies", 80, |rb| {
                    rb["action"].is_null()
                        && rb["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["id"] == automation_id.as_str() && row["enabled"] == false))
                })
                .await?;
                let paused_row = paused["rows"].as_array().and_then(|rows| rows.iter().find(|row| row["id"] == automation_id.as_str()).cloned()).unwrap_or_default();
                anyhow::ensure!(
                    paused_row["revision"] == 2
                        && paused_row["status"].as_str().is_some_and(|status| status.starts_with("Paused.")),
                    "Pause applies at the next revision with an honest status: {paused_row}"
                );
                anyhow::ensure!(paused_row["toggle"] == "Edit time", "a spent paused one-shot offers Edit time: {paused_row}");
                let offered = panel!(false, |p, c| p.fixture_toggle(&automation_id, c));
                let editing = automations(window, cx, false)?;
                anyhow::ensure!(
                    offered == Some(Some("Edit time"))
                        && editing["action"].is_null()
                        && editing["editor"]["automationId"] == automation_id.as_str()
                        && editing["editor"]["existing"] == true
                        && editing["editor"]["resumes"] == true
                        && editing["editor"]["expectedRevision"] == 2,
                    "Edit time opens the exact row's editor without a Resume request: {editing}"
                );
                // Case 71: switching Main drops answers for the previous
                // conversation (rapid real selections, each surface drawn),
                // and a reconnect reloads saved state without new runs.
                for chat in [MAIN2, AUTO_CHAT, MAIN2] {
                    select(cx, Some(chat));
                    window.update(cx, |shell, _, cx| shell.fixture_open_automations(cx))?;
                    draw(window, cx)?;
                }
                let other_view = wait_automations(window, cx, false, "Main 2's (empty) automations", 50, |rb| {
                    rb["scope"]["conversation"] == MAIN2 && rb["loading"] == false
                })
                .await?;
                settle(cx, 300).await;
                let other_view = automations(window, cx, false).unwrap_or(other_view);
                anyhow::ensure!(
                    other_view["scope"]["conversation"] == MAIN2 && other_view["rows"].as_array().is_some_and(Vec::is_empty),
                    "the automation chat's rows never land in Main 2's view: {other_view}"
                );
                select(cx, Some(AUTO_CHAT));
                wait_for(window, cx, "the automation chat as Main again", |rb| rb["loaded"] == true && rb["main"] == AUTO_CHAT).await?;
                window.update(cx, |shell, _, cx| shell.fixture_open_automations(cx))?;
                wait_automations(window, cx, false, "the automation chat's rows again", 60, |rb| {
                    rb["scope"]["conversation"] == AUTO_CHAT
                        && rb["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["id"] == automation_id.as_str()))
                })
                .await?;
                let runs_request = serde_json::json!({ "automationId": automation_id, "includeDeleted": false });
                let runs_before: luna_proto::AutomationPage<luna_proto::AutomationRun> =
                    reader.call_as(luna_rpc::methods::LIST_AUTOMATION_RUNS, runs_request.clone()).await?;
                window.update(cx, |shell, _, cx| shell.fixture_reconnect_engine(cx))?;
                settle(cx, 2000).await;
                let reloaded = wait_automations(window, cx, false, "saved automations after a reconnect", 80, |rb| {
                    rb["loading"] == false
                        && rb["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["id"] == automation_id.as_str() && row["revision"] == 2))
                })
                .await?;
                let runs_after: luna_proto::AutomationPage<luna_proto::AutomationRun> =
                    reader.call_as(luna_rpc::methods::LIST_AUTOMATION_RUNS, runs_request).await?;
                anyhow::ensure!(
                    runs_after.items.len() == runs_before.items.len()
                        && window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(AUTO_CHAT, cx))?.iter().filter(|text| text.contains(W20M_PROMPT)).count() == 1,
                    "a reconnect reloads saved state and resends nothing: {reloaded}"
                );
                window.update(cx, |shell, window, cx| shell.fixture_close_automations(window, cx))?;
                // Case 72: Settings pages app-wide (50 per page); Load more
                // adds the rest once, without duplicates.
                for index in 0..51i64 {
                    let request = luna_proto::AutomationMutationRequest {
                        request_id: format!("w20m-bulk-request-{index:02}"),
                        automation_id: format!("w20m-bulk-{index:02}"),
                        owner_host_id: queue_device.clone(),
                        expected_revision: 0,
                        operation: luna_proto::AutomationOperation::Save {
                            definition: luna_proto::AutomationDefinition {
                                name: format!("Paging row {index:02}"),
                                prompt: "Paging row".into(),
                                timezone: "UTC".into(),
                                target: luna_proto::AutomationTarget::Existing { task_id: AUTO_CHAT.into() },
                                trigger: luna_proto::AutomationTrigger::Once { at_ms: 4_102_444_800_000 + index * 60_000 },
                                max_lateness_ms: 600_000,
                            },
                        },
                        preview_id: String::new(),
                    };
                    let preview: luna_proto::AutomationPreview = reader
                        .call_as(luna_rpc::methods::PREVIEW_AUTOMATION, serde_json::to_value(&request)?)
                        .await?;
                    let mut apply = request;
                    apply.preview_id = preview.preview_id;
                    reader.call(luna_rpc::methods::MUTATE_AUTOMATION, serde_json::to_value(&apply)?).await?;
                }
                window.update(cx, |shell, _, cx| shell.fixture_open_automation_settings(cx))?;
                let first_page = wait_automations(window, cx, true, "the first app-wide page", 80, |rb| {
                    rb["scope"] == "all" && rb["loading"] == false && rb["rows"].as_array().is_some_and(|rows| rows.len() == 50)
                })
                .await?;
                anyhow::ensure!(first_page["next"].is_string(), "52 automations leave a next page after 50: {}", first_page["next"]);
                shot(window, cx, output.join("w20m-automations-settings.png")).await?;
                panel!(true, |p, c| p.fixture_more(c));
                let all_pages = wait_automations(window, cx, true, "the second page", 80, |rb| {
                    rb["rows"].as_array().is_some_and(|rows| rows.len() == 52) && rb["next"].is_null()
                })
                .await?;
                let ids: std::collections::BTreeSet<String> = all_pages["rows"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|row| row["id"].as_str().map(str::to_string))
                    .collect();
                anyhow::ensure!(ids.len() == 52 && ids.contains(&automation_id), "Load more adds the rest once: {} ids", ids.len());
                window.update(cx, |shell, _, cx| shell.fixture_leave_settings(false, cx))?;
                // Isolation: no send reached Main or Child A since w20m began
                // (the same send intents w20l counts; Main's earlier recovery
                // may still settle on its own and is not a send).
                let new_sends = |task: &str, after: u64| -> anyhow::Result<usize> {
                    Ok(command_store
                        .read_task_commands(task, after, 100)?
                        .events
                        .into_iter()
                        .filter(|event| event.receipt.intent == "sendQueuedNow" || event.receipt.intent == "session")
                        .count())
                };
                let main_after = new_sends(MAIN, main_sequence)?;
                let child_after = new_sends(CHILD_A, child_sequence)?;
                anyhow::ensure!(
                    main_after == 0 && child_after == 0,
                    "automations never send to Main or Child A: {main_after} new Main sends, {child_after} new Child A sends"
                );
                // Back to Main as later steps expect; the fixture-managed
                // chat list returns to its seeded rows (the engine keeps both
                // w20m chats).
                select(cx, Some(MAIN));
                wait_for(window, cx, "Main's layout after automations", |rb| rb["loaded"] == true && rb["main"] == MAIN).await?;
                cx.update(|cx| {
                    state.update(cx, |state, cx| {
                        state.chats.retain(|chat| chat.id != AUTO_CHAT && chat.id != REMOTE_CHAT);
                        state.devices.retain(|device| device.id != REMOTE_HOST);
                        cx.notify();
                    })
                });
                // Later frame steps start from the right sidebar as w20m found it.
                set_right_sidebar(window, cx, right_was_open).await?;
                progress(&output, &mut record, "automations".into(), serde_json::json!({
                    "remote": remote,
                    "refusals": refusals,
                    "due": due,
                    "dueLocal": local,
                    "requestId": request_id,
                    "automationId": automation_id,
                    "receiptRevision": receipt.record.revision,
                    "run": run,
                    "seenStates": seen_states,
                    "paused": paused_row,
                    "pausedEditor": editing,
                    "runsAfterReconnect": runs_after.items.len(),
                    "pagedRows": ids.len(),
                    "automationChat": AUTO_CHAT,
                    "newMainCommands": main_after,
                    "newChildACommands": child_after,
                }));
                click_tab(window, cx, MAIN).await?;

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
