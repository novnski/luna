//! Headless GPUI E2E for the sidebar's list sections: the five-row cap with
//! Show more / Show less, the reset on collapse and on a project switch, and
//! the Settled and Archived shelves sitting at the bottom of the list.
//!
//! Drives the production `Shell` in a hidden window with real pointer input
//! (move/down/up through GPUI's hit testing) against synthetic AppState rows
//! (no engine, provider, account or credential). Geometry is measured, not
//! estimated: an opt-in probe records the painted bounds of the sidebar's
//! viewport, its top content, the shelf tray and every clickable header and
//! more row, plus the chat ids each section actually rendered.
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. Settled or Archived render above or between active rows, or float right
//!    under a short list with the empty space below them instead of sitting
//!    on the bottom edge of the sidebar's list viewport.
//! 2. With a long list the shelves overlap or clip the active rows, keep a
//!    phantom gap, or can't be scrolled to; opening a shelf at the bottom
//!    leaves its rows below the fold.
//! 3. An unpinned section (the active list, a device group, Settled,
//!    Archived) shows more than five rows by default, or fewer when more
//!    exist; pins count toward the cap or hide behind Show more.
//! 4. Show more reveals a count other than the next five (or the rest),
//!    duplicates or skips rows, or its label disagrees with what appears;
//!    Show less is missing once everything shows, or appears when nothing
//!    can be hidden.
//! 5. Collapsing and reopening a shelf or device group keeps the expanded
//!    page instead of returning to five; switching the project filter keeps
//!    an expanded page.
//! 6. The keyboard/jump order (`sidebarOrder`) drifts from the rendered rows:
//!    it includes capped rows or misses revealed ones.
//! 7. Height bookkeeping drifts from what paints at Interface size 12, 16 or
//!    20: a shelf body clips its last row or leaves a gap, a more row is
//!    taller or shorter than reserved, or the keyed list's heights disagree.
//! 8. An empty project loses "No tasks yet" at the top or its shelves at the
//!    bottom.
//! 9. Shelf headers read as peers of the active list (loud labels), lose the
//!    count, or paint an invisible glyph/chevron (an SVG without its own
//!    colour); text or rows don't scale with Interface size.
//! 10. Both shelves don't start collapsed, or a selected chat beyond the cap
//!     disappears from the sidebar.
//!
//! Item 9's visual half (tone, legibility) is judged from the PNGs; every
//! other item is asserted below. The run pins reduced motion: the headless
//! test clock doesn't advance animations in real time, so tweens render at
//! their end state and geometry is read where it settles.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example sidebar-sections-fixture
//! Run:   target/debug/examples/sidebar-sections-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes sNN-* PNG captures per Interface size (12, 16, 20; dark),
//! readback.json and sidebar-sections.pass; a failure writes
//! failure.txt/failure.png and exits nonzero.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, MouseButton, PlatformInput, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};
use luna_proto::{ChatConfig, HarnessId, PermissionMode, SandboxLevel};
use luna_ui::*;

const DEVICE: &str = "studio";
const PROJECT_A: &str = "proj-luna";
const PROJECT_B: &str = "proj-docs";
const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 860.0;
const PAGE: usize = 5;

const PINNED: [(&str, &str); 2] = [
    ("a-pin-1", "Release checklist"),
    ("a-pin-2", "Incident notes"),
];
const ACTIVE: [(&str, &str); 9] = [
    ("a-act-1", "Cache warmup on cold start"),
    ("a-act-2", "Docs typos in the setup guide"),
    ("a-act-3", "Flaky test triage"),
    ("a-act-4", "Settings migration"),
    ("a-act-5", "Profile startup"),
    ("a-act-6", "Audit call sites"),
    ("a-act-7", "Rename the sync worker"),
    ("a-act-8", "Window restore on login"),
    ("a-act-9", "Terminal paste bracketing"),
];
const SETTLED_A: [(&str, &str); 7] = [
    ("a-set-1", "Icon pass for the toolbar"),
    ("a-set-2", "Keychain prompt copy"),
    ("a-set-3", "Changelog for 0.2.70"),
    ("a-set-4", "Font fallback on Linux"),
    ("a-set-5", "Crash on empty diff"),
    ("a-set-6", "Theme contrast audit"),
    ("a-set-7", "Old onboarding sketch"),
];
const ARCHIVED_A: [(&str, &str); 8] = [
    ("a-arc-1", "Abandoned approach"),
    ("a-arc-2", "Spike: CRDT history"),
    ("a-arc-3", "Try the new tokenizer"),
    ("a-arc-4", "Bisect the scroll jank"),
    ("a-arc-5", "Prototype tab tearing"),
    ("a-arc-6", "Measure cold boot"),
    ("a-arc-7", "Draft blog post"),
    ("a-arc-8", "Reproduce the hang"),
];
const SETTLED_B: [(&str, &str); 2] = [
    ("b-set-1", "Fix broken anchors"),
    ("b-set-2", "Search index rebuild"),
];
const ARCHIVED_B: [(&str, &str); 6] = [
    ("b-arc-1", "Old landing copy"),
    ("b-arc-2", "Pricing table draft"),
    ("b-arc-3", "Migrate to the new theme"),
    ("b-arc-4", "Screenshot refresh"),
    ("b-arc-5", "Navigation experiment"),
    ("b-arc-6", "Typo sweep"),
];

fn config(ix: usize) -> ChatConfig {
    ChatConfig {
        harness: if ix % 2 == 0 {
            HarnessId::ClaudeCode
        } else {
            HarnessId::Codex
        },
        provider_account_id: None,
        model: Some("synthetic-model".into()),
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

fn chat(id: &str, title: &str, project: &str, minutes_ago: i64, ix: usize) -> luna_proto::Chat {
    let at = Utc::now() - TimeDelta::minutes(minutes_ago);
    luna_proto::Chat {
        child_thread: None,
        id: id.into(),
        device_id: DEVICE.into(),
        title: Some(title.into()),
        archived: false,
        settlement_override: None,
        pinned_order: None,
        cwd: None,
        branch: None,
        checkout_id: None,
        source_context: None,
        config: Some(config(ix)),
        last_message_preview: None,
        last_message_at: Some(at),
        created_at: at - TimeDelta::minutes(3),
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: Some(project.into()),
        last_seen_at: Some(at + TimeDelta::seconds(1)),
        manually_unread: false,
        room_gen: None,
    }
}

fn space(id: &str, name: &str) -> luna_proto::Space {
    luna_proto::Space {
        id: id.into(),
        device_id: DEVICE.into(),
        path: format!("/synthetic/{id}"),
        name: Some(name.into()),
        git_detected: true,
        git_checked_at: None,
        checkout_id: None,
        execution_defaults: Default::default(),
        created_at: Utc::now() - TimeDelta::days(30),
    }
}

fn seed_chats() -> Vec<luna_proto::Chat> {
    const DAY: i64 = 24 * 60;
    let mut chats = Vec::new();
    for (ix, (id, title)) in PINNED.iter().enumerate() {
        let mut row = chat(id, title, PROJECT_A, 30 + ix as i64 * 10, ix);
        row.pinned_order = Some("1".repeat(ix + 1));
        chats.push(row);
    }
    for (ix, (id, title)) in ACTIVE.iter().enumerate() {
        chats.push(chat(id, title, PROJECT_A, 2 + ix as i64 * 7, ix));
    }
    // Idle for over the three-day auto-settle window.
    for (ix, (id, title)) in SETTLED_A.iter().enumerate() {
        chats.push(chat(id, title, PROJECT_A, 5 * DAY + ix as i64 * 60, ix));
    }
    for (ix, (id, title)) in ARCHIVED_A.iter().enumerate() {
        let mut row = chat(id, title, PROJECT_A, 90 + ix as i64 * 300, ix);
        row.archived = true;
        chats.push(row);
    }
    for (ix, (id, title)) in SETTLED_B.iter().enumerate() {
        chats.push(chat(id, title, PROJECT_B, 6 * DAY + ix as i64 * 60, ix));
    }
    for (ix, (id, title)) in ARCHIVED_B.iter().enumerate() {
        let mut row = chat(id, title, PROJECT_B, 120 + ix as i64 * 300, ix);
        row.archived = true;
        chats.push(row);
    }
    chats
}

type Window = WindowHandle<shell::Shell>;

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn draw(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
    })?;
    Ok(())
}

/// Wait while rendering frames, as the display link would. A headless window
/// only renders when drawn, and an animation (a disclosure tween, the
/// list's resort glide) starts on the first frame after a change and
/// advances only on frames: plain sleeps would read it mid-flight.
async fn frames(window: Window, cx: &mut AsyncApp, ms: u64) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        draw(window, cx)?;
        settle(cx, 16).await;
    }
    draw(window, cx)
}

fn capture(window: Window, cx: &mut AsyncApp, path: PathBuf) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })??;
    Ok(())
}

fn input(window: Window, cx: &mut AsyncApp, event: PlatformInput) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(event, cx);
    })?;
    Ok(())
}

fn hover_at(window: Window, cx: &mut AsyncApp, x: f32, y: f32) -> anyhow::Result<()> {
    input(
        window,
        cx,
        PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position: point(px(x), px(y)),
            pressed_button: None,
            modifiers: Default::default(),
        }),
    )
}

/// A real click: hover (so hover-driven state paints), then down/up, then
/// park the pointer over the canvas so no tooltip or hover wash lingers.
async fn click_at(window: Window, cx: &mut AsyncApp, x: f32, y: f32) -> anyhow::Result<()> {
    hover_at(window, cx, x, y)?;
    draw(window, cx)?;
    frames(window, cx, 60).await?;
    draw(window, cx)?;
    let position = point(px(x), px(y));
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
    frames(window, cx, 80).await?;
    hover_at(window, cx, WIDTH - 40.0, HEIGHT - 40.0)?;
    // Let disclosure tweens and hover fades finish before reading geometry.
    frames(window, cx, 520).await?;
    draw(window, cx)?;
    Ok(())
}

fn readback(window: Window, cx: &mut AsyncApp) -> anyhow::Result<serde_json::Value> {
    draw(window, cx)?;
    let value = window.update(cx, |shell, _, cx| {
        shell.fixture_sidebar_sections_readback(cx)
    })?;
    anyhow::ensure!(!value.is_null(), "the sidebar probe is not recording");
    Ok(value)
}

fn num(value: &serde_json::Value) -> f32 {
    value.as_f64().unwrap_or(f64::NAN) as f32
}

fn bounds_of<'a>(rb: &'a serde_json::Value, label: &str) -> anyhow::Result<&'a serde_json::Value> {
    let found = &rb["bounds"][label];
    anyhow::ensure!(!found.is_null(), "no painted bounds for {label}");
    Ok(found)
}

fn ids(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn section_rows(rb: &serde_json::Value, section: &str) -> Vec<String> {
    rb["activeRows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|pair| pair[0] == section)
        .filter_map(|pair| pair[1].as_str().map(str::to_owned))
        .collect()
}

fn first_ids(rows: &[(&str, &str)], count: usize) -> Vec<String> {
    rows.iter()
        .take(count)
        .map(|(id, _)| id.to_string())
        .collect()
}

fn near(actual: f32, expected: f32, tolerance: f32) -> bool {
    (actual - expected).abs() <= tolerance
}

/// Scroll the sidebar so `label` is fully inside the viewport, then return
/// its painted centre.
async fn reveal(window: Window, cx: &mut AsyncApp, label: &str) -> anyhow::Result<(f32, f32)> {
    let rb = readback(window, cx)?;
    let target = bounds_of(&rb, label)?;
    let (top, bottom) = (num(&target["y"]), num(&target["bottom"]));
    let viewport = &rb["viewport"];
    let (view_top, view_bottom) = (num(&viewport["y"]), num(&viewport["bottom"]));
    let offset = -num(&rb["scroll"]["offset"]);
    let down = if bottom > view_bottom - 2.0 {
        Some(offset + bottom - view_bottom + 8.0)
    } else if top < view_top + 2.0 {
        Some(offset - (view_top - top) - 8.0)
    } else {
        None
    };
    if let Some(down) = down {
        window.update(cx, |shell, _, cx| shell.fixture_scroll_sidebar_to(down, cx))?;
        frames(window, cx, 60).await?;
    }
    let rb = readback(window, cx)?;
    let target = bounds_of(&rb, label)?;
    let viewport = &rb["viewport"];
    anyhow::ensure!(
        num(&target["y"]) >= num(&viewport["y"]) - 1.0
            && num(&target["bottom"]) <= num(&viewport["bottom"]) + 1.0,
        "{label} cannot be scrolled into view: {target} in {viewport}"
    );
    Ok((
        num(&target["x"]) + num(&target["w"]) / 2.0,
        num(&target["y"]) + num(&target["h"]) / 2.0,
    ))
}

async fn click_label(window: Window, cx: &mut AsyncApp, label: &str) -> anyhow::Result<()> {
    let (x, y) = reveal(window, cx, label).await?;
    eprintln!("click {label} at ({x}, {y})");
    click_at(window, cx, x, y).await
}

async fn scroll_top(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    window.update(cx, |shell, _, cx| shell.fixture_scroll_sidebar_to(0.0, cx))?;
    frames(window, cx, 60).await?;
    draw(window, cx)
}

/// How the shelf tray sat relative to the list, for the run's coverage
/// check (both the short-list and the long-list layout must be seen).
#[derive(Default)]
struct Layouts {
    pinned_bottom: usize,
    flowed: usize,
}

/// Item 1/2/8: the tray sits on the viewport's bottom edge when everything
/// fits, and flows right after the top content (no overlap, no phantom
/// gap) when the list is long.
fn check_tray(rb: &serde_json::Value, step: &str, layouts: &mut Layouts) -> anyhow::Result<()> {
    let children = &rb["scrollChildren"];
    let (top, spacer, shelves) = (&children["top"], &children["spacer"], &children["shelves"]);
    anyhow::ensure!(
        !top.is_null() && !spacer.is_null() && !shelves.is_null(),
        "{step}: the list needs top content, a spacer and the shelf tray in that order: {children}"
    );
    let viewport = &rb["viewport"];
    let max = num(&rb["scroll"]["max"]);
    anyhow::ensure!(
        num(&shelves["y"]) >= num(&top["bottom"]) - 0.5,
        "{step}: shelves overlap the top content: {shelves} vs {top}"
    );
    if max <= 0.5 {
        anyhow::ensure!(
            near(num(&shelves["bottom"]), num(&viewport["bottom"]), 1.5),
            "{step}: a short list must leave the shelves on the viewport's bottom edge: {shelves} vs {viewport}"
        );
        layouts.pinned_bottom += 1;
    } else {
        anyhow::ensure!(
            num(&spacer["h"]) <= 1.0 && near(num(&shelves["y"]), num(&top["bottom"]), 1.5),
            "{step}: a long list must let the shelves flow straight after it: spacer {spacer}, shelves {shelves}, top {top}"
        );
        layouts.flowed += 1;
    }
    Ok(())
}

/// Item 7: every keyed list entry paints at the height it reserved, and a
/// shelf's open body matches the tween's reserved height.
fn check_heights(rb: &serde_json::Value, step: &str) -> anyhow::Result<()> {
    let scale = num(&rb["uiScale"]);
    for entry in rb["keyed"].as_array().into_iter().flatten() {
        let key = entry[0].as_str().unwrap_or_default();
        let reserved = num(&entry[1]);
        let painted = bounds_of(rb, key)?;
        anyhow::ensure!(
            near(num(&painted["h"]), reserved, 1.0),
            "{step}: keyed {key} reserved {reserved} but painted {painted}"
        );
    }
    for shelf in ["settled", "archived"] {
        let page = &rb["shelfPages"][shelf];
        if page.is_null() {
            continue;
        }
        let header = bounds_of(rb, &format!("{shelf}-header"))?;
        anyhow::ensure!(
            near(num(&header["h"]), 28.0 * scale, 1.0),
            "{step}: {shelf} header must be 28 pt at this Interface size: {header}"
        );
        let body = bounds_of(rb, &format!("{shelf}-body"))?;
        let expected = if page["open"] == true {
            num(&page["bodyHeight"])
        } else {
            0.0
        };
        anyhow::ensure!(
            near(num(&body["h"]), expected, 1.0),
            "{step}: {shelf} body reserved {expected} but painted {body}"
        );
        if page["open"] == true {
            let visible = page["visible"].as_u64().unwrap_or_default() as usize;
            for ix in 0..visible {
                let row = bounds_of(rb, &format!("{shelf}-row-{ix}"))?;
                anyhow::ensure!(
                    near(num(&row["h"]), 36.0 * scale, 1.0),
                    "{step}: {shelf} row {ix} must be 36 pt: {row}"
                );
            }
            if !page["more"].is_null() {
                let more = bounds_of(rb, &format!("{shelf}-more"))?;
                anyhow::ensure!(
                    near(num(&more["h"]), 28.0 * scale, 1.0),
                    "{step}: {shelf} more row must be 28 pt: {more}"
                );
                let last = bounds_of(rb, &format!("{shelf}-row-{}", visible - 1))?;
                anyhow::ensure!(
                    near(num(&more["y"]), num(&last["bottom"]), 1.0)
                        && num(&more["bottom"]) <= num(&body["bottom"]) + 1.0,
                    "{step}: {shelf} more row must follow the last row inside the body: {more} / {last} / {body}"
                );
            }
        }
    }
    Ok(())
}

/// Item 2: an opened shelf is kept in view (the whole tray when it fits).
fn check_revealed(rb: &serde_json::Value, step: &str) -> anyhow::Result<()> {
    let shelves = &rb["scrollChildren"]["shelves"];
    let viewport = &rb["viewport"];
    if num(&shelves["h"]) <= num(&viewport["h"]) + 0.5 {
        anyhow::ensure!(
            num(&shelves["y"]) >= num(&viewport["y"]) - 1.0
                && num(&shelves["bottom"]) <= num(&viewport["bottom"]) + 1.0,
            "{step}: the opened shelf tray must be scrolled fully into view: {shelves} in {viewport}"
        );
    } else {
        anyhow::ensure!(
            near(num(&shelves["y"]), num(&viewport["y"]), 1.5),
            "{step}: a tray taller than the viewport must align its top: {shelves} in {viewport}"
        );
    }
    Ok(())
}

fn check_page(
    page: &serde_json::Value,
    step: &str,
    total: usize,
    visible: usize,
    more: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        page["total"] == total
            && page["visible"] == visible
            && match more {
                Some(label) => page["more"] == label,
                None => page["more"].is_null(),
            },
        "{step}: expected {total} total, {visible} visible, more {more:?}: {page}"
    );
    Ok(())
}

fn expected_order(active: usize) -> Vec<String> {
    let mut order = first_ids(&PINNED, PINNED.len());
    order.extend(first_ids(&ACTIVE, active));
    order
}

async fn scale_pass(
    window: Window,
    state: &gpui::Entity<state::AppState>,
    cx: &mut AsyncApp,
    output: &Path,
    font: typography::UiFontSize,
    layouts: &mut Layouts,
) -> anyhow::Result<serde_json::Value> {
    let px_size = font.pixels() as u32;
    let mut record = serde_json::Map::new();
    window.update(cx, |_, window, cx| {
        typography::set_font_size(font, window, cx);
    })?;
    // A fresh project scope: every section starts at five.
    window.update(cx, |shell, _, cx| {
        shell.fixture_set_space_filter(Some(PROJECT_A.into()), cx)
    })?;
    frames(window, cx, 500).await?;
    scroll_top(window, cx).await?;
    let shot = |name: &str| output.join(format!("s{px_size}-{name}.png"));

    // 01. Default: pins, five active rows + Show 4 more, both shelves
    // collapsed in the bottom tray.
    capture(window, cx, shot("01-default"))?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px default");
    anyhow::ensure!(
        near(num(&rb["uiScale"]), px_size as f32 / 16.0, 0.01),
        "{step}: Interface size did not apply: {}",
        rb["uiScale"]
    );
    anyhow::ensure!(
        section_rows(&rb, "pinned") == first_ids(&PINNED, 2),
        "{step}: pins always show and never count toward the cap: {}",
        rb["activeRows"]
    );
    anyhow::ensure!(
        section_rows(&rb, "active") == first_ids(&ACTIVE, PAGE),
        "{step}: the active list shows its five most recent rows: {}",
        rb["activeRows"]
    );
    check_page(&rb["pages"]["active"], &step, 9, 5, Some("Show 4 more"))?;
    anyhow::ensure!(
        ids(&rb["order"]) == expected_order(PAGE),
        "{step}: keyboard order must match the rendered rows: {}",
        rb["order"]
    );
    anyhow::ensure!(
        rb["settledOpen"] == false && rb["archivedOpen"] == false,
        "{step}: both shelves start collapsed: {rb}"
    );
    check_page(
        &rb["shelfPages"]["settled"],
        &step,
        7,
        5,
        Some("Show 2 more"),
    )?;
    check_page(
        &rb["shelfPages"]["archived"],
        &step,
        8,
        5,
        Some("Show 3 more"),
    )?;
    let more = bounds_of(&rb, "more:active")?;
    let last = bounds_of(&rb, &format!("c:{}", ACTIVE[PAGE - 1].0))?;
    anyhow::ensure!(
        near(num(&more["y"]), num(&last["bottom"]), 1.0),
        "{step}: Show more must close the active list: {more} after {last}"
    );
    check_tray(&rb, &step, layouts)?;
    check_heights(&rb, &step)?;
    record.insert("default".into(), rb);

    // 02. Show 4 more: all nine, then Show less.
    click_label(window, cx, "more:active").await?;
    scroll_top(window, cx).await?;
    capture(window, cx, shot("02-active-show-more"))?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px show more");
    anyhow::ensure!(
        section_rows(&rb, "active") == first_ids(&ACTIVE, 9),
        "{step}: Show 4 more reveals the remaining four, once each: {}",
        rb["activeRows"]
    );
    check_page(&rb["pages"]["active"], &step, 9, 9, Some("Show less"))?;
    anyhow::ensure!(
        ids(&rb["order"]) == expected_order(9),
        "{step}: revealed rows join the keyboard order: {}",
        rb["order"]
    );
    check_tray(&rb, &step, layouts)?;
    check_heights(&rb, &step)?;
    record.insert("activeShowMore".into(), rb);

    // Show less returns to five.
    click_label(window, cx, "more:active").await?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px show less");
    check_page(&rb["pages"]["active"], &step, 9, 5, Some("Show 4 more"))?;
    anyhow::ensure!(
        ids(&rb["order"]) == expected_order(PAGE),
        "{step}: Show less hides the extra rows again: {}",
        rb["order"]
    );

    // A project switch resets an expanded list.
    click_label(window, cx, "more:active").await?;
    check_page(
        &readback(window, cx)?["pages"]["active"],
        &step,
        9,
        9,
        Some("Show less"),
    )?;
    window.update(cx, |shell, _, cx| {
        shell.fixture_set_space_filter(Some(PROJECT_B.into()), cx)
    })?;
    frames(window, cx, 120).await?;
    window.update(cx, |shell, _, cx| {
        shell.fixture_set_space_filter(Some(PROJECT_A.into()), cx)
    })?;
    frames(window, cx, 300).await?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px project switch");
    check_page(&rb["pages"]["active"], &step, 9, 5, Some("Show 4 more"))?;
    record.insert("afterProjectSwitch".into(), rb);
    scroll_top(window, cx).await?;

    // 03. Settled opens at five + Show 2 more, kept in view.
    click_label(window, cx, "settled-header").await?;
    capture(window, cx, shot("03-settled-open"))?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px settled open");
    anyhow::ensure!(rb["settledOpen"] == true, "{step}: Settled did not open");
    anyhow::ensure!(
        ids(&rb["shelfRows"]["settled"]) == first_ids(&SETTLED_A, PAGE),
        "{step}: Settled shows its five most recent rows: {}",
        rb["shelfRows"]
    );
    check_page(
        &rb["shelfPages"]["settled"],
        &step,
        7,
        5,
        Some("Show 2 more"),
    )?;
    check_revealed(&rb, &step)?;
    check_tray(&rb, &step, layouts)?;
    check_heights(&rb, &step)?;
    record.insert("settledOpen".into(), rb);

    // 04. Show 2 more: all seven, Show less.
    click_label(window, cx, "settled-more").await?;
    capture(window, cx, shot("04-settled-show-more"))?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px settled show more");
    anyhow::ensure!(
        ids(&rb["shelfRows"]["settled"]) == first_ids(&SETTLED_A, 7),
        "{step}: Show 2 more reveals the last two: {}",
        rb["shelfRows"]
    );
    check_page(&rb["shelfPages"]["settled"], &step, 7, 7, Some("Show less"))?;
    check_heights(&rb, &step)?;
    record.insert("settledShowMore".into(), rb);

    // 05. Collapse and reopen: back to five.
    click_label(window, cx, "settled-header").await?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px settled collapsed");
    anyhow::ensure!(
        rb["settledOpen"] == false,
        "{step}: Settled did not collapse"
    );
    check_heights(&rb, &step)?;
    click_label(window, cx, "settled-header").await?;
    capture(window, cx, shot("05-settled-reopened"))?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px settled reopened");
    anyhow::ensure!(
        rb["settledOpen"] == true
            && ids(&rb["shelfRows"]["settled"]) == first_ids(&SETTLED_A, PAGE),
        "{step}: reopening Settled must reset it to five rows: {}",
        rb["shelfRows"]
    );
    check_page(
        &rb["shelfPages"]["settled"],
        &step,
        7,
        5,
        Some("Show 2 more"),
    )?;
    check_heights(&rb, &step)?;
    record.insert("settledReopened".into(), rb);
    click_label(window, cx, "settled-header").await?;

    // 06. Archived: five + Show 3 more; expand, collapse, reopen → five.
    click_label(window, cx, "archived-header").await?;
    capture(window, cx, shot("06-archived-open"))?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px archived open");
    anyhow::ensure!(
        rb["archivedOpen"] == true
            && ids(&rb["shelfRows"]["archived"]) == first_ids(&ARCHIVED_A, PAGE),
        "{step}: Archived opens on its five most recent rows: {}",
        rb["shelfRows"]
    );
    check_page(
        &rb["shelfPages"]["archived"],
        &step,
        8,
        5,
        Some("Show 3 more"),
    )?;
    check_revealed(&rb, &step)?;
    check_tray(&rb, &step, layouts)?;
    check_heights(&rb, &step)?;
    record.insert("archivedOpen".into(), rb);
    click_label(window, cx, "archived-more").await?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px archived show more");
    check_page(
        &rb["shelfPages"]["archived"],
        &step,
        8,
        8,
        Some("Show less"),
    )?;
    check_heights(&rb, &step)?;
    click_label(window, cx, "archived-header").await?;
    click_label(window, cx, "archived-header").await?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px archived reopened");
    check_page(
        &rb["shelfPages"]["archived"],
        &step,
        8,
        5,
        Some("Show 3 more"),
    )?;
    anyhow::ensure!(
        ids(&rb["shelfRows"]["archived"]) == first_ids(&ARCHIVED_A, PAGE),
        "{step}: reopening Archived must reset it to five rows: {}",
        rb["shelfRows"]
    );
    record.insert("archivedReopened".into(), rb);
    click_label(window, cx, "archived-header").await?;

    // 07/08. Empty project: the empty state at the top, the tray at the
    // bottom; Archived opens to five + Show 1 more and stays at the bottom.
    window.update(cx, |shell, _, cx| {
        shell.fixture_set_space_filter(Some(PROJECT_B.into()), cx)
    })?;
    frames(window, cx, 400).await?;
    scroll_top(window, cx).await?;
    capture(window, cx, shot("07-empty-project"))?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px empty project");
    anyhow::ensure!(
        rb["activeRows"].as_array().is_some_and(Vec::is_empty) && rb["pages"]["active"].is_null(),
        "{step}: project B has no active rows: {}",
        rb["activeRows"]
    );
    let top = &rb["scrollChildren"]["top"];
    anyhow::ensure!(
        num(&top["y"]) <= num(&rb["viewport"]["y"]) + 8.0,
        "{step}: the empty state must sit at the top of the list: {top}"
    );
    check_page(&rb["shelfPages"]["settled"], &step, 2, 2, None)?;
    check_page(
        &rb["shelfPages"]["archived"],
        &step,
        6,
        5,
        Some("Show 1 more"),
    )?;
    check_tray(&rb, &step, layouts)?;
    check_heights(&rb, &step)?;
    record.insert("emptyProject".into(), rb);
    click_label(window, cx, "archived-header").await?;
    capture(window, cx, shot("08-empty-project-archived"))?;
    let rb = readback(window, cx)?;
    let step = format!("{px_size}px empty project archived");
    anyhow::ensure!(
        ids(&rb["shelfRows"]["archived"]) == first_ids(&ARCHIVED_B, PAGE),
        "{step}: {}",
        rb["shelfRows"]
    );
    check_page(
        &rb["shelfPages"]["archived"],
        &step,
        6,
        5,
        Some("Show 1 more"),
    )?;
    check_revealed(&rb, &step)?;
    check_tray(&rb, &step, layouts)?;
    check_heights(&rb, &step)?;
    record.insert("emptyProjectArchived".into(), rb);
    click_label(window, cx, "archived-header").await?;

    // Item 10: a selected chat past the cap stays visible as its section's
    // last row (checked once, at the default size).
    if px_size == 16 {
        window.update(cx, |shell, _, cx| {
            shell.fixture_set_space_filter(Some(PROJECT_A.into()), cx)
        })?;
        state.update(cx, |state, cx| {
            state.select_chat(Some(ACTIVE[7].0.into()), cx)
        });
        frames(window, cx, 400).await?;
        scroll_top(window, cx).await?;
        capture(window, cx, shot("09-selected-past-cap"))?;
        let rb = readback(window, cx)?;
        let step = "16px selected past the cap";
        let mut expected = first_ids(&ACTIVE, PAGE);
        expected.push(ACTIVE[7].0.into());
        anyhow::ensure!(
            section_rows(&rb, "active") == expected,
            "{step}: the selected row must stay visible: {}",
            rb["activeRows"]
        );
        check_page(&rb["pages"]["active"], step, 9, 6, Some("Show 3 more"))?;
        anyhow::ensure!(
            ids(&rb["order"]).contains(&ACTIVE[7].0.to_string()),
            "{step}: the kept row is in the keyboard order: {}",
            rb["order"]
        );
        check_heights(&rb, step)?;
        record.insert("selectedPastCap".into(), rb);
        state.update(cx, |state, cx| state.select_chat(None, cx));

        // Grouped by device: the group pages its own rows; collapsing and
        // reopening the group resets it.
        window.update(cx, |shell, _, cx| {
            shell.fixture_set_sidebar_by_device(true, cx)
        })?;
        frames(window, cx, 400).await?;
        scroll_top(window, cx).await?;
        capture(window, cx, shot("10-by-device"))?;
        let rb = readback(window, cx)?;
        let step = "16px by device";
        let group = format!("device:{DEVICE}");
        check_page(&rb["pages"][&group], step, 9, 5, Some("Show 4 more"))?;
        anyhow::ensure!(
            section_rows(&rb, &group) == first_ids(&ACTIVE, PAGE),
            "{step}: the device group shows five rows: {}",
            rb["activeRows"]
        );
        check_heights(&rb, step)?;
        // The group's more row is the last 28 pt of its keyed block.
        let block = bounds_of(&rb, &format!("g:{group}"))?.clone();
        let scale = num(&rb["uiScale"]);
        let x = num(&block["x"]) + num(&block["w"]) / 2.0;
        click_at(window, cx, x, num(&block["bottom"]) - 14.0 * scale).await?;
        let rb = readback(window, cx)?;
        check_page(&rb["pages"][&group], step, 9, 9, Some("Show less"))?;
        check_heights(&rb, step)?;
        // The header sits under the group's 12 pt top gap.
        let header_y = num(&block["y"]) + (12.0 + 14.0) * scale;
        click_at(window, cx, x, header_y).await?;
        click_at(window, cx, x, header_y).await?;
        let rb = readback(window, cx)?;
        check_page(&rb["pages"][&group], step, 9, 5, Some("Show 4 more"))?;
        record.insert("byDevice".into(), rb);
        window.update(cx, |shell, _, cx| {
            shell.fixture_set_sidebar_by_device(false, cx)
        })?;
        frames(window, cx, 300).await?;
    }
    Ok(serde_json::Value::Object(record))
}

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: sidebar-sections-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in [
        "readback.json",
        "sidebar-sections.pass",
        "failure.txt",
        "failure.png",
    ] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let temp = tempfile::tempdir()?;
    let data = temp.path().to_path_buf();
    gpui_platform::headless()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let mut settings = settings::UiSettings::default();
            settings.sidebar_auto_settle_after_days = Some(3);
            settings.space_filter = Some(PROJECT_A.into());
            settings::init(settings.clone(), data.clone(), cx);
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
            theme_library::init(data.clone(), cx);
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
                state.connection = luna_proto::view::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Local);
                state.local_device_id = Some(DEVICE.into());
                state.devices = serde_json::from_value(serde_json::json!([{
                    "id": DEVICE, "name": "Studio Mac", "platform": "macos", "lastSeenAt": null
                }]))
                .unwrap();
                state.spaces = vec![space(PROJECT_A, "LUNA"), space(PROJECT_B, "Docs site")];
                state.chats = seed_chats();
                state.auto_selected = true;
                state.chats_synced = true;
                state.spaces_synced = true;
                state
            });
            let boot = EngineBootConfig {
                data_dir: data,
                ipc_port: 0,
                edge_url: String::new(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: HarnessId::ClaudeCode,
            };
            let ui_state = state.clone();
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
            cx.spawn(async move |cx| {
                let result: anyhow::Result<serde_json::Value> = async {
                    // The headless test clock does not advance animations in
                    // real time; reduced motion renders every tween (the
                    // disclosure bodies, the list's resort glide) at its end
                    // state, so geometry is read where it settles.
                    cx.update(|cx| motion::set_reduced_motion(cx, true));
                    window.update(cx, |shell, _, cx| shell.fixture_enable_sidebar_probe(cx))?;
                    frames(window, cx, 900).await?;
                    let mut record = serde_json::Map::new();
                    let mut layouts = Layouts::default();
                    for font in [
                        typography::UiFontSize::ALL[0],
                        typography::UiFontSize::ALL[4],
                        typography::UiFontSize::ALL[6],
                    ] {
                        let pass =
                            scale_pass(window, &ui_state, cx, &output, font, &mut layouts)
                                .await?;
                        record.insert(format!("{}px", font.pixels() as u32), pass);
                    }
                    // Both layouts must have been exercised and verified.
                    anyhow::ensure!(
                        layouts.pinned_bottom > 0 && layouts.flowed > 0,
                        "coverage: the run must see the tray both on the bottom edge ({}) and flowing after a long list ({})",
                        layouts.pinned_bottom,
                        layouts.flowed
                    );
                    record.insert(
                        "layouts".into(),
                        serde_json::json!({
                            "pinnedBottom": layouts.pinned_bottom,
                            "flowed": layouts.flowed,
                        }),
                    );
                    Ok(serde_json::Value::Object(record))
                }
                .await;
                match result {
                    Ok(record) => {
                        let written = std::fs::write(
                            output.join("readback.json"),
                            serde_json::to_vec_pretty(&record).unwrap_or_default(),
                        )
                        .and_then(|_| {
                            std::fs::write(
                                output.join("sidebar-sections.pass"),
                                "PASS: headless production Shell with real pointer input at Interface sizes 12, 16 and 20 (dark): pins always show; the active list, a device group, Settled and Archived each show five rows with Show N more (next five) and Show less; collapsing and reopening a shelf or device group, and switching projects, reset to five; the keyboard order matches the rendered rows; a selected row past the cap stays visible; both shelves start collapsed in one tray that sits on the bottom edge of a short list and flows after a long one, and an opened shelf is scrolled into view; the empty project keeps its empty state at the top and the tray at the bottom; every keyed row, header, shelf body and more row paints at its reserved height\n",
                            )
                        });
                        if let Err(error) = written {
                            let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                            std::process::exit(1);
                        }
                    }
                    Err(error) => {
                        eprintln!("sidebar-sections fixture failed: {error:#}");
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                        let _ = capture(window, cx, output.join("failure.png"));
                        std::process::exit(1);
                    }
                }
                let _ = gpui::AnyWindowHandle::from(window)
                    .update(cx, |_, window, _| window.remove_window());
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    Ok(())
}
