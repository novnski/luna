//! Headless GPUI E2E for the sidebar redesign: the single-project view
//! (Design A: a project pill, two-line rows with the status first) and the
//! All projects view (Design D: one collapsible group per project with compact
//! rows), plus the project picker that switches between them.
//!
//! Drives the production `Shell` in a hidden window with real pointer input
//! (move/down/up through GPUI's hit testing) against synthetic AppState rows,
//! sessions and an Attention snapshot (no engine, provider, account or
//! credential). An opt-in probe records what the sidebar rendered and where.
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. The project pill loses its identity: no project tile, the wrong name, or
//!    a summary that disagrees with the rows (active count, needs-you count),
//!    in a single project or in All projects.
//! 2. The picker hides what changed: a project row without its tile, path or
//!    live counts (needs you, working), the current project not marked, the
//!    All projects row missing its total, or "New project…" / "Manage
//!    projects…" missing or unreachable by keyboard.
//! 3. A single-project row shows the wrong status: the working, approval,
//!    input, failed or done state draws the wrong mark, the harness mark is
//!    not on the meta line, or a needs-you row lacks its Approve / Reply /
//!    Failed label.
//! 4. The working glyph is not LUNA's existing square loader (a circle or a
//!    static dot replaced it).
//! 5. All projects is still one flat list: rows are not grouped under their
//!    project, a project group lists another project's task, a group header
//!    lacks its tile, name or needs-you/working counts, or pins move into a
//!    project group.
//! 6. All projects rows are not compact: a row reserves the two-line height,
//!    or the keyed height bookkeeping disagrees with what paints at Interface
//!    size 12, 16 or 20.
//! 7. Collapsing a project group hides the wrong rows, leaves its rows in the
//!    keyboard/jump order mismatch, loses the group's summary, or fails to
//!    reopen; switching back to a single project still shows groups.
//! 8. A project group shows more than five rows by default, or its Show more
//!    pages another project's rows.
//! 9. The keyboard/jump order (`order`) drifts from the rendered rows in
//!    either view.
//! 10. The Settled and Archived history is unreachable from the All projects
//!     view, or a single project's shelves regress.
//! 11. A row with nothing going on keeps an empty status column (user report:
//!     a read, finished task left a gap above its provider icon): titles do
//!     not all start on the same left edge across idle and busy rows, the
//!     provider icon does not start under the title, a status is not at the
//!     row's right edge, or an idle row still paints a status.
//! 12. The top row still carries more than the project pill (the list's
//!     view options or Activity), or the pill does not span the sidebar.
//! 13. The navigation section is missing or out of order: New thread then
//!     Automations, between the pill and the Threads header, on the rows'
//!     left edge.
//! 14. New thread does not open the new-thread canvas; Automations does not
//!     open its own page beside the thread list (route, marked row, rows
//!     still listed, no thread marked current); Back does not return;
//!     Settings still lists Automations;
//!     opening Settings › Automations does not land on the page.
//! 15. The Threads header lacks its label, view options or Activity toggle,
//!     or Activity no longer opens and closes from it.
//! 16. A pin is painted in the status spot (user report: it hid the status),
//!     or hovering a row paints more than one pin.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example sidebar-redesign-fixture
//! Run:   target/debug/examples/sidebar-redesign-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes PNG captures per Interface size (12, 16, 20; dark), readback.json
//! and sidebar-redesign.pass; a failure writes failure.txt/failure.png and
//! exits nonzero.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, MouseButton, PlatformInput, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};
use luna_proto::{ChatConfig, HarnessId, PermissionMode, SandboxLevel, Session, SessionStatus};
use luna_ui::*;

const DEVICE: &str = "studio";
const REMOTE: &str = "mini";
const P_LUNA: &str = "proj-luna";
const P_WS: &str = "proj-ws";
const P_INFRA: &str = "proj-infra";
const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 900.0;

/// (id, title, project, minutes ago, session status, unread, device)
type Seed = (
    &'static str,
    &'static str,
    &'static str,
    i64,
    Option<SessionStatus>,
    bool,
    &'static str,
);

const ACTIVE: [Seed; 13] = [
    (
        "l-work",
        "Sidebar redesign mockups",
        P_LUNA,
        1,
        Some(SessionStatus::Working),
        false,
        DEVICE,
    ),
    (
        "l-approve",
        "Preserve answer drafts",
        P_LUNA,
        2,
        Some(SessionStatus::AwaitingApproval),
        false,
        DEVICE,
    ),
    (
        "l-input",
        "Reconcile OpenCode streams",
        P_LUNA,
        6,
        Some(SessionStatus::AwaitingInput),
        false,
        DEVICE,
    ),
    (
        "l-error",
        "Native QA audit runners",
        P_LUNA,
        14,
        Some(SessionStatus::Errored),
        true,
        DEVICE,
    ),
    (
        "l-done",
        "Provider usage refresh",
        P_LUNA,
        31,
        None,
        true,
        DEVICE,
    ),
    (
        "l-idle-1",
        "Build cache GC budget",
        P_LUNA,
        180,
        None,
        false,
        DEVICE,
    ),
    (
        "l-idle-2",
        "VS Code theme import",
        P_LUNA,
        480,
        None,
        false,
        DEVICE,
    ),
    (
        "l-remote",
        "Sync registry edge cases",
        P_LUNA,
        600,
        None,
        false,
        REMOTE,
    ),
    (
        "w-work",
        "Refresh project map",
        P_WS,
        0,
        Some(SessionStatus::Working),
        false,
        DEVICE,
    ),
    (
        "w-idle-1",
        "Intercom reply drafts",
        P_WS,
        120,
        None,
        false,
        DEVICE,
    ),
    (
        "w-idle-2",
        "Brain notes checkpoint",
        P_WS,
        300,
        None,
        false,
        DEVICE,
    ),
    (
        "i-approve",
        "Rotate alert thresholds",
        P_INFRA,
        20,
        Some(SessionStatus::AwaitingApproval),
        false,
        DEVICE,
    ),
    (
        "i-idle",
        "Dashboard query cleanup",
        P_INFRA,
        240,
        None,
        false,
        DEVICE,
    ),
];

/// The single-project list: the pin, then the first five unpinned rows by
/// recency (the section cap).
const PROJECT_ORDER: [&str; 6] = [
    "l-pin",
    "l-work",
    "l-approve",
    "l-input",
    "l-error",
    "l-done",
];
/// All projects: the pin, then each project's capped rows, projects in the
/// picker's alphabetical order (infra-monitors, luna, workspace).
const ALL_ORDER: [&str; 11] = [
    "l-pin",
    "i-approve",
    "i-idle",
    "l-work",
    "l-approve",
    "l-input",
    "l-error",
    "l-done",
    "w-work",
    "w-idle-1",
    "w-idle-2",
];

fn config(harness: HarnessId) -> ChatConfig {
    ChatConfig {
        harness,
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

fn chat(
    id: &str,
    title: &str,
    project: &str,
    minutes_ago: i64,
    unread: bool,
    device: &str,
) -> luna_proto::Chat {
    let at = Utc::now() - TimeDelta::minutes(minutes_ago);
    luna_proto::Chat {
        child_thread: None,
        id: id.into(),
        device_id: device.into(),
        title: Some(title.into()),
        archived: false,
        settlement_override: None,
        pinned_order: None,
        cwd: None,
        branch: Some(format!("feat/{id}")),
        checkout_id: None,
        source_context: None,
        config: Some(config(if id.len().is_multiple_of(2) {
            HarnessId::ClaudeCode
        } else {
            HarnessId::Codex
        })),
        last_message_preview: None,
        last_message_at: Some(at),
        created_at: at - TimeDelta::minutes(3),
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: Some(project.into()),
        last_seen_at: Some(if unread {
            at - TimeDelta::minutes(1)
        } else {
            at + TimeDelta::seconds(1)
        }),
        manually_unread: false,
        room_gen: None,
    }
}

fn space(id: &str, name: &str) -> luna_proto::Space {
    luna_proto::Space {
        id: id.into(),
        device_id: DEVICE.into(),
        path: format!("/synthetic/{name}"),
        name: Some(name.into()),
        git_detected: true,
        git_checked_at: None,
        checkout_id: None,
        execution_defaults: Default::default(),
        created_at: Utc::now() - TimeDelta::days(30),
        removed_at: None,
    }
}

fn seed_chats() -> Vec<luna_proto::Chat> {
    let mut chats = Vec::new();
    let mut pin = chat("l-pin", "LUNA release checklist", P_LUNA, 40, false, DEVICE);
    pin.pinned_order = Some("1".into());
    chats.push(pin);
    for (id, title, project, minutes, _, unread, device) in ACTIVE {
        chats.push(chat(id, title, project, minutes, unread, device));
    }
    // Idle past the one-day automatic settle window.
    for (ix, (id, title)) in [
        ("l-set-1", "Fix rail hover flicker"),
        ("l-set-2", "Composer image paste"),
    ]
    .into_iter()
    .enumerate()
    {
        chats.push(chat(
            id,
            title,
            P_LUNA,
            3 * 24 * 60 + ix as i64 * 60,
            false,
            DEVICE,
        ));
    }
    let mut archived = chat("l-arc-1", "Grok harness spike", P_LUNA, 90, false, DEVICE);
    archived.archived = true;
    chats.push(archived);
    chats
}

fn sessions() -> Vec<Session> {
    ACTIVE
        .iter()
        .filter_map(|(id, _, _, _, status, _, device)| {
            status.map(|status| Session {
                last_completed_turn: None,
                chat_id: (*id).into(),
                device_id: (*device).into(),
                status,
                started_at: Some(Utc::now()),
                // Heartbeats are 45 s fresh; the run outlives that, so the
                // synthetic heartbeat sits ahead of the clock.
                updated_at: Utc::now() + TimeDelta::hours(1),
                recovery: None,
            })
        })
        .collect()
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

async fn click_at(window: Window, cx: &mut AsyncApp, x: f32, y: f32) -> anyhow::Result<()> {
    hover_at(window, cx, x, y)?;
    frames(window, cx, 60).await?;
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
    frames(window, cx, 520).await
}

async fn press(window: Window, cx: &mut AsyncApp, key: &str) -> anyhow::Result<()> {
    let keystroke = gpui::Keystroke::parse(key)?;
    input(
        window,
        cx,
        PlatformInput::KeyDown(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        }),
    )?;
    input(
        window,
        cx,
        PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }),
    )?;
    frames(window, cx, 120).await
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

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn bounds_of<'a>(rb: &'a serde_json::Value, label: &str) -> anyhow::Result<&'a serde_json::Value> {
    let found = &rb["bounds"][label];
    anyhow::ensure!(!found.is_null(), "no painted bounds for {label}");
    Ok(found)
}

fn keyed(rb: &serde_json::Value) -> Vec<(String, f32)> {
    rb["keyed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pair| Some((pair[0].as_str()?.to_owned(), num(&pair[1]))))
        .collect()
}

fn keyed_height(rb: &serde_json::Value, key: &str) -> anyhow::Result<f32> {
    keyed(rb)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, h)| h)
        .ok_or_else(|| anyhow::anyhow!("{key} is not in the keyed list"))
}

fn mark(rb: &serde_json::Value, key: &str) -> anyhow::Result<Vec<String>> {
    let values = strings(&rb["marks"][key]);
    anyhow::ensure!(!values.is_empty(), "nothing recorded for {key}");
    Ok(values)
}

fn active_rows(rb: &serde_json::Value) -> Vec<(String, String)> {
    rb["activeRows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pair| Some((pair[0].as_str()?.to_owned(), pair[1].as_str()?.to_owned())))
        .collect()
}

fn project_of(id: &str) -> &'static str {
    match id.as_bytes().first() {
        Some(b'w') => P_WS,
        Some(b'i') => P_INFRA,
        _ => P_LUNA,
    }
}

/// Items 3, 4 and 9 in a single project: status first, square loader,
/// needs-you labels, two-line rows, jump order = rendered order.
fn check_project(rb: &serde_json::Value, step: &str) -> anyhow::Result<serde_json::Value> {
    let scale = num(&rb["uiScale"]);
    let trigger = strings(&rb["texts"]["spaces-trigger"]);
    anyhow::ensure!(
        trigger == ["luna", "9 active · 3 need you"],
        "{step}: item 1, the pill must name the project and its live counts: {trigger:?}"
    );
    let order = strings(&rb["order"]);
    anyhow::ensure!(
        order == PROJECT_ORDER,
        "{step}: the single-project list drifted: {order:?}"
    );
    let rendered: Vec<String> = active_rows(rb).into_iter().map(|(_, id)| id).collect();
    anyhow::ensure!(
        rendered == order,
        "{step}: item 9, the jump order {order:?} disagrees with the rows {rendered:?}"
    );
    let expect = [
        ("l-pin", "none", ""),
        ("l-work", "loader", ""),
        ("l-approve", "approval", "Approve"),
        ("l-input", "input", "Reply"),
        ("l-error", "failed", "Failed"),
        ("l-done", "dot", ""),
    ];
    for (id, lead, pill) in expect {
        let marks = mark(rb, &format!("row:{id}"))?;
        let want = vec![
            format!("status={lead}"),
            format!("pill={pill}"),
            "metaHarness=true".to_owned(),
            "compact=false".to_owned(),
            "pins=0".to_owned(),
            "selected=false".to_owned(),
        ];
        anyhow::ensure!(
            marks == want,
            "{step}: items 3/4, row {id} drew {marks:?}, expected {want:?}"
        );
        // Pins render inside the Pinned group's keyed entry.
        if id == "l-pin" {
            continue;
        }
        let height = keyed_height(rb, &format!("c:{id}"))?;
        anyhow::ensure!(
            (height - 52.0 * scale).abs() < 0.01,
            "{step}: row {id} reserved {height}, expected two lines ({})",
            52.0 * scale
        );
        let painted = num(&bounds_of(rb, &format!("c:{id}"))?["h"]);
        anyhow::ensure!(
            (painted - height).abs() <= 1.0,
            "{step}: row {id} painted {painted} but reserved {height}"
        );
    }
    anyhow::ensure!(
        !keyed(rb)
            .iter()
            .any(|(key, _)| key.starts_with("g:project:")),
        "{step}: item 7, a single project must not show project groups"
    );
    anyhow::ensure!(
        rb["bounds"]["settled"].is_object() && rb["bounds"]["archived"].is_object(),
        "{step}: item 10, the project's Settled and Archived shelves are missing"
    );
    Ok(serde_json::json!({ "order": order, "trigger": trigger }))
}

/// Item 11: every listed row's title starts on one left edge, its provider
/// icon (two-line rows) starts under the title, a status ends on the row's
/// right inset, and a row with nothing going on paints no status at all.
fn check_alignment(
    rb: &serde_json::Value,
    step: &str,
    ids: &[String],
    idle: &[&str],
    compact: bool,
) -> anyhow::Result<serde_json::Value> {
    let mut left: Option<f32> = None;
    let mut record = serde_json::Map::new();
    for id in ids {
        let title = bounds_of(rb, &format!("title:{id}"))?;
        let x = num(&title["x"]);
        let edge = *left.get_or_insert(x);
        anyhow::ensure!(
            (x - edge).abs() <= 0.5,
            "{step}: item 11, {id}'s title starts at {x}, the others at {edge}"
        );
        if !compact {
            let meta = num(&bounds_of(rb, &format!("meta:{id}"))?["x"]);
            anyhow::ensure!(
                (meta - x).abs() <= 0.5,
                "{step}: item 11, {id}'s provider icon starts at {meta}, not under its title at {x}"
            );
        }
        let status = &rb["bounds"][format!("status:{id}")];
        if idle.contains(&id.as_str()) {
            anyhow::ensure!(
                status.is_null(),
                "{step}: item 11, idle row {id} still paints a status: {status}"
            );
        } else {
            anyhow::ensure!(status.is_object(), "{step}: item 11, {id} lost its status");
            if let Some(row) = rb["bounds"][format!("c:{id}")].as_object() {
                let right = (row["x"].as_f64().unwrap_or(f64::NAN)
                    + row["w"].as_f64().unwrap_or(f64::NAN)) as f32;
                let end = num(&status["x"]) + num(&status["w"]);
                anyhow::ensure!(
                    (right - 8.0 - end).abs() <= 1.0,
                    "{step}: item 11, {id}'s status ends at {end}, not at the row's right inset {}",
                    right - 8.0
                );
            }
        }
        record.insert(id.clone(), serde_json::json!(x));
    }
    Ok(serde_json::Value::Object(record))
}

/// Items 2: the picker's rows, tiles' data, counts and current project.
fn check_picker(rb: &serde_json::Value, step: &str, current: Option<&str>) -> anyhow::Result<()> {
    let details = strings(&rb["texts"]["spaces-menu-details"]);
    let row = |name: &str| {
        details
            .iter()
            .find(|row| row.starts_with(&format!("{name}|")))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{step}: item 2, no picker row for {name}: {details:?}"))
    };
    let all = row("All projects")?;
    anyhow::ensure!(
        all.contains("14 active across 3 projects"),
        "{step}: item 2, the All projects row lost its total: {all}"
    );
    for (name, path, counts) in [
        ("luna", "/synthetic/luna", "need=3 working=1 active=9"),
        (
            "workspace",
            "/synthetic/workspace",
            "need=0 working=1 active=3",
        ),
        (
            "infra-monitors",
            "/synthetic/infra-monitors",
            "need=1 working=0 active=2",
        ),
    ] {
        let line = row(name)?;
        anyhow::ensure!(
            line.contains(path) && line.ends_with(counts),
            "{step}: item 2, {name}'s row must show {path} and {counts}: {line}"
        );
        let selected = line.contains("|selected|");
        anyhow::ensure!(
            selected == (current == Some(name)),
            "{step}: item 2, {name} selection mark is {selected}"
        );
    }
    anyhow::ensure!(
        all.contains("|selected|") == current.is_none(),
        "{step}: item 2, All projects selection mark is wrong: {all}"
    );
    Ok(())
}

/// Items 1, 5, 6, 8, 9 and 10 in All projects.
fn check_all(rb: &serde_json::Value, step: &str) -> anyhow::Result<serde_json::Value> {
    let scale = num(&rb["uiScale"]);
    let trigger = strings(&rb["texts"]["spaces-trigger"]);
    anyhow::ensure!(
        trigger == ["All projects", "14 active · 4 need you"],
        "{step}: item 1, the All projects pill must sum every project: {trigger:?}"
    );
    let keys: Vec<String> = keyed(rb).into_iter().map(|(key, _)| key).collect();
    let expected_keys = [
        "g:pinned:__luna_pinned__",
        "divider:pinned",
        "g:project:proj-infra",
        "g:project:proj-luna",
        "g:project:proj-ws",
    ];
    anyhow::ensure!(
        keys == expected_keys,
        "{step}: item 5, All projects must list Pinned then one group per project: {keys:?}"
    );
    let rows = active_rows(rb);
    for (section, id) in &rows {
        let want = if id == "l-pin" {
            "pinned".to_owned()
        } else {
            format!("project:{}", project_of(id))
        };
        anyhow::ensure!(
            *section == want,
            "{step}: item 5, {id} is listed under {section}, expected {want}"
        );
        let marks = mark(rb, &format!("row:{id}"))?;
        anyhow::ensure!(
            marks.contains(&"compact=true".to_owned())
                && marks.contains(&"metaHarness=false".to_owned()),
            "{step}: item 6, {id} is not a compact row: {marks:?}"
        );
    }
    let order = strings(&rb["order"]);
    let rendered: Vec<String> = rows.into_iter().map(|(_, id)| id).collect();
    let aligned = check_alignment(
        rb,
        step,
        &rendered,
        &["l-pin", "i-idle", "w-idle-1", "w-idle-2"],
        true,
    )?;
    anyhow::ensure!(
        order == ALL_ORDER && rendered == order,
        "{step}: item 9, order {order:?} / rendered {rendered:?}, expected {ALL_ORDER:?}"
    );
    let luna_page = &rb["pages"]["project:proj-luna"];
    anyhow::ensure!(
        luna_page["total"] == 8 && luna_page["visible"] == 5 && luna_page["more"] == "Show 3 more",
        "{step}: item 8, luna must show five of eight rows: {luna_page}"
    );
    for (space, want) in [
        (
            P_LUNA,
            ["luna", "need=3", "working=1", "active=8", "collapsed=false"],
        ),
        (
            P_WS,
            [
                "workspace",
                "need=0",
                "working=1",
                "active=3",
                "collapsed=false",
            ],
        ),
        (
            P_INFRA,
            [
                "infra-monitors",
                "need=1",
                "working=0",
                "active=2",
                "collapsed=false",
            ],
        ),
    ] {
        let marks = mark(rb, &format!("project-header:{space}"))?;
        anyhow::ensure!(
            marks == want,
            "{step}: item 5, {space}'s header drew {marks:?}, expected {want:?}"
        );
    }
    // Item 6: workspace's three compact rows under its header, exactly as
    // reserved and as painted.
    let ws = keyed_height(rb, "g:project:proj-ws")?;
    let want = (12.0 + 28.0 + 4.0) * scale + 3.0 * 32.0 * scale;
    anyhow::ensure!(
        (ws - want).abs() < 0.01,
        "{step}: item 6, workspace reserved {ws}, expected {want}"
    );
    for (key, height) in keyed(rb) {
        let painted = num(&bounds_of(rb, &key)?["h"]);
        anyhow::ensure!(
            (painted - height).abs() <= 1.0,
            "{step}: item 6, {key} painted {painted} but reserved {height}"
        );
    }
    anyhow::ensure!(
        strings(&rb["scrollChildren"]).contains(&"shelves".to_owned())
            || rb["bounds"]["settled"].is_object(),
        "{step}: item 10, Settled and Archived are unreachable from All projects"
    );
    Ok(serde_json::json!({ "order": order, "keys": keys, "titleLeft": aligned }))
}

async fn scale_pass(
    window: Window,
    cx: &mut AsyncApp,
    output: &Path,
    font: typography::UiFontSize,
) -> anyhow::Result<serde_json::Value> {
    let px_size = font.pixels() as u32;
    window.update(cx, |_, window, cx| {
        typography::set_font_size(font, window, cx)
    })?;
    window.update(cx, |shell, _, cx| {
        shell.fixture_set_space_filter(Some(P_LUNA.into()), cx)
    })?;
    frames(window, cx, 500).await?;
    let rb = readback(window, cx)?;
    let project = check_project(&rb, &format!("{px_size}px project"))?;
    capture(window, cx, output.join(format!("s{px_size}-project.png")))?;

    // Item 11 over every row of the project, idle ones included.
    let more = bounds_of(&rb, "more:active")?.clone();
    let (mx, my) = (
        num(&more["x"]) + 40.0,
        num(&more["y"]) + num(&more["h"]) / 2.0,
    );
    click_at(window, cx, mx, my).await?;
    let rb = readback(window, cx)?;
    let ids: Vec<String> = active_rows(&rb).into_iter().map(|(_, id)| id).collect();
    anyhow::ensure!(
        ids.len() == 9,
        "{px_size}px: Show more must list all nine luna rows: {ids:?}"
    );
    let aligned = check_alignment(
        &rb,
        &format!("{px_size}px project, all rows"),
        &ids,
        &["l-pin", "l-idle-1", "l-idle-2", "l-remote"],
        false,
    )?;
    capture(
        window,
        cx,
        output.join(format!("s{px_size}-project-all-rows.png")),
    )?;
    let more = bounds_of(&rb, "more:active")?.clone();
    click_at(
        window,
        cx,
        num(&more["x"]) + 40.0,
        num(&more["y"]) + num(&more["h"]) / 2.0,
    )
    .await?;

    let trigger = bounds_of(&rb, "header-trigger")?.clone();
    let (tx, ty) = (
        num(&trigger["x"]) + num(&trigger["w"]) / 2.0,
        num(&trigger["y"]) + num(&trigger["h"]) / 2.0,
    );
    click_at(window, cx, tx, ty).await?;
    let rb = readback(window, cx)?;
    check_picker(&rb, &format!("{px_size}px picker"), Some("luna"))?;
    capture(window, cx, output.join(format!("s{px_size}-picker.png")))?;
    press(window, cx, "escape").await?;

    window.update(cx, |shell, _, cx| shell.fixture_set_space_filter(None, cx))?;
    frames(window, cx, 500).await?;
    let rb = readback(window, cx)?;
    let all = check_all(&rb, &format!("{px_size}px all"))?;
    capture(window, cx, output.join(format!("s{px_size}-all.png")))?;
    Ok(serde_json::json!({ "project": project, "titleLeft": aligned, "all": all }))
}

async fn behaviour_pass(
    window: Window,
    cx: &mut AsyncApp,
    output: &Path,
) -> anyhow::Result<serde_json::Value> {
    window.update(cx, |_, window, cx| {
        typography::set_font_size(typography::UiFontSize::ALL[4], window, cx)
    })?;
    window.update(cx, |shell, _, cx| shell.fixture_set_space_filter(None, cx))?;
    frames(window, cx, 500).await?;
    let rb = readback(window, cx)?;
    let scale = num(&rb["uiScale"]);
    let open_height = keyed_height(&rb, "g:project:proj-luna")?;
    let group = bounds_of(&rb, "g:project:proj-luna")?.clone();
    let header_y = num(&group["y"]) + (12.0 + 14.0) * scale;
    let header_x = num(&group["x"]) + 60.0;

    // Item 7: collapse luna, keep its summary and the order, reopen.
    click_at(window, cx, header_x, header_y).await?;
    let rb = readback(window, cx)?;
    let folded = keyed_height(&rb, "g:project:proj-luna")?;
    anyhow::ensure!(
        (folded - 40.0 * scale).abs() < 0.01,
        "item 7: a folded group must keep only its header: {folded}"
    );
    let marks = mark(&rb, "project-header:proj-luna")?;
    anyhow::ensure!(
        marks == ["luna", "need=3", "working=1", "active=8", "collapsed=true"],
        "item 7: the folded header lost its summary: {marks:?}"
    );
    anyhow::ensure!(
        strings(&rb["order"]) == ALL_ORDER,
        "item 7: folding changed the jump order: {:?}",
        rb["order"]
    );
    capture(window, cx, output.join("b-collapsed.png"))?;
    click_at(window, cx, header_x, header_y).await?;
    let rb = readback(window, cx)?;
    let reopened = keyed_height(&rb, "g:project:proj-luna")?;
    anyhow::ensure!(
        (reopened - open_height).abs() < 0.01,
        "item 7: luna did not reopen: {reopened} vs {open_height}"
    );

    // Item 8: Show more pages only luna.
    let group = bounds_of(&rb, "g:project:proj-luna")?.clone();
    click_at(
        window,
        cx,
        num(&group["x"]) + 60.0,
        num(&group["bottom"]) - 8.0,
    )
    .await?;
    let rb = readback(window, cx)?;
    let luna_page = &rb["pages"]["project:proj-luna"];
    let ws_page = &rb["pages"]["project:proj-ws"];
    anyhow::ensure!(
        luna_page["visible"] == 8 && luna_page["more"] == "Show less" && ws_page["visible"] == 3,
        "item 8: Show more must reveal luna's three hidden rows only: {luna_page} / {ws_page}"
    );
    let luna_rows = active_rows(&rb)
        .into_iter()
        .filter(|(section, _)| section == "project:proj-luna")
        .count();
    anyhow::ensure!(luna_rows == 8, "item 8: luna rendered {luna_rows} rows");
    capture(window, cx, output.join("b-show-more.png"))?;

    // Item 2: the picker's footer is reachable by keyboard.
    let trigger = bounds_of(&rb, "header-trigger")?.clone();
    let (tx, ty) = (
        num(&trigger["x"]) + num(&trigger["w"]) / 2.0,
        num(&trigger["y"]) + num(&trigger["h"]) / 2.0,
    );
    click_at(window, cx, tx, ty).await?;
    let rb = readback(window, cx)?;
    check_picker(&rb, "picker from All projects", None)?;
    capture(window, cx, output.join("b-picker-all.png"))?;
    // All, infra-monitors, luna, workspace, New project…, Manage projects…
    for _ in 0..5 {
        press(window, cx, "down").await?;
    }
    press(window, cx, "enter").await?;
    let rb = readback(window, cx)?;
    let route = rb["route"].as_str().unwrap_or_default().to_owned();
    anyhow::ensure!(
        route.contains("Projects"),
        "item 2: Manage projects… did not open Settings › Projects: {route}"
    );
    window.update(cx, |shell, _, cx| shell.fixture_leave_settings(true, cx))?;
    frames(window, cx, 400).await?;

    // Item 7: picking a project by keyboard returns to the flat list.
    let rb = readback(window, cx)?;
    let trigger = bounds_of(&rb, "header-trigger")?.clone();
    click_at(
        window,
        cx,
        num(&trigger["x"]) + num(&trigger["w"]) / 2.0,
        num(&trigger["y"]) + num(&trigger["h"]) / 2.0,
    )
    .await?;
    press(window, cx, "down").await?;
    press(window, cx, "down").await?;
    press(window, cx, "enter").await?;
    frames(window, cx, 400).await?;
    let rb = readback(window, cx)?;
    anyhow::ensure!(
        rb["filter"] == P_LUNA,
        "item 7: choosing luna in the picker set the filter to {}",
        rb["filter"]
    );
    anyhow::ensure!(
        !keyed(&rb)
            .iter()
            .any(|(key, _)| key.starts_with("g:project:")),
        "item 7: switching to a single project still shows project groups"
    );
    capture(window, cx, output.join("b-back-to-project.png"))?;
    Ok(serde_json::json!({
        "foldedHeight": folded,
        "openHeight": open_height,
        "route": route,
    }))
}

fn center(bounds: &serde_json::Value) -> (f32, f32) {
    (
        num(&bounds["x"]) + num(&bounds["w"]) / 2.0,
        num(&bounds["y"]) + num(&bounds["h"]) / 2.0,
    )
}

async fn click_label(window: Window, cx: &mut AsyncApp, label: &str) -> anyhow::Result<()> {
    let rb = readback(window, cx)?;
    let (x, y) = center(bounds_of(&rb, label)?);
    click_at(window, cx, x, y).await
}

/// Items 12 to 16: the top row, the navigation section, the Automations
/// page, the Threads header and the pin.
async fn navigation_pass(
    window: Window,
    cx: &mut AsyncApp,
    output: &Path,
) -> anyhow::Result<serde_json::Value> {
    window.update(cx, |shell, _, cx| {
        shell.fixture_set_space_filter(Some(P_LUNA.into()), cx)
    })?;
    frames(window, cx, 400).await?;
    let rb = readback(window, cx)?;
    let row = bounds_of(&rb, "c:l-work")?.clone();
    let row_left = num(&row["x"]);
    let row_right = row_left + num(&row["w"]);
    let trigger = bounds_of(&rb, "header-trigger")?.clone();
    let bell = bounds_of(&rb, "header-activity")?.clone();
    anyhow::ensure!(
        (num(&trigger["x"]) - row_left).abs() <= 0.5
            && (num(&trigger["x"]) + num(&trigger["w"]) - row_right).abs() <= 0.5,
        "item 12: the project pill must span the rows' width: {trigger} vs rows {row_left}..{row_right}"
    );
    anyhow::ensure!(
        num(&bell["y"]) > num(&trigger["bottom"]),
        "item 12: the Activity toggle must leave the top row: bell {bell}, pill {trigger}"
    );
    let new_thread = bounds_of(&rb, "nav-new-thread")?.clone();
    let automations = bounds_of(&rb, "nav-automations")?.clone();
    let threads = bounds_of(&rb, "threads-label")?.clone();
    anyhow::ensure!(
        num(&trigger["bottom"]) <= num(&new_thread["y"])
            && num(&new_thread["bottom"]) <= num(&automations["y"])
            && num(&automations["bottom"]) <= num(&threads["y"])
            && num(&threads["bottom"]) <= num(&row["y"]),
        "item 13: expected pill, New thread, Automations, Threads, rows top to bottom: {trigger} {new_thread} {automations} {threads} {row}"
    );
    for (label, bounds) in [("New thread", &new_thread), ("Automations", &automations)] {
        anyhow::ensure!(
            (num(&bounds["x"]) - row_left).abs() <= 0.5,
            "item 13: {label} must start on the rows' left edge: {bounds} vs {row_left}"
        );
    }
    anyhow::ensure!(
        rb["bounds"]["header-view"].is_object()
            && (num(&bell["x"]) + num(&bell["w"]) - row_right).abs() <= 0.5,
        "item 15: the Threads header must carry the view options and end the bell on the rows' edge"
    );
    capture(window, cx, output.join("n-layout.png"))?;

    // Item 16: the pinned row paints no pin at rest and one under the
    // pointer; an unpinned row's hover shows the one pin to pin it.
    let marks = mark(&rb, "row:l-pin")?;
    anyhow::ensure!(
        marks.contains(&"status=none".to_owned()) && marks.contains(&"pins=0".to_owned()),
        "item 16: a pinned row at rest must paint no pin: {marks:?}"
    );
    for id in ["l-pin", "l-work"] {
        let rb = readback(window, cx)?;
        let (x, y) = center(bounds_of(&rb, &format!("title:{id}"))?);
        hover_at(window, cx, x, y)?;
        frames(window, cx, 300).await?;
        let rb = readback(window, cx)?;
        let marks = mark(&rb, &format!("row:{id}"))?;
        anyhow::ensure!(
            marks.contains(&"pins=1".to_owned())
                && marks.contains(&format!(
                    "status={}",
                    if id == "l-pin" { "none" } else { "loader" }
                )),
            "item 16: hovering {id} must paint exactly one pin and keep its status: {marks:?}"
        );
        if id == "l-pin" {
            capture(window, cx, output.join("n-pinned-hover.png"))?;
        }
    }
    hover_at(window, cx, WIDTH - 40.0, HEIGHT - 40.0)?;
    frames(window, cx, 300).await?;

    // Item 15: Activity opens and closes from the Threads header.
    click_label(window, cx, "header-activity").await?;
    let rb = readback(window, cx)?;
    anyhow::ensure!(
        rb["activityView"] == true && rb["bounds"]["header-activity-label"].is_object(),
        "item 15: the Threads header's bell did not open Activity"
    );
    click_label(window, cx, "header-activity").await?;
    let rb = readback(window, cx)?;
    anyhow::ensure!(
        rb["activityView"] == false,
        "item 15: Activity did not close"
    );

    // Item 14: Automations is its own page beside the thread list.
    click_label(window, cx, "c:l-work").await?;
    click_label(window, cx, "nav-automations").await?;
    let rb = readback(window, cx)?;
    let route = rb["route"].as_str().unwrap_or_default().to_owned();
    anyhow::ensure!(
        route == "Page(Automations)",
        "item 14: Automations opened {route}, not its page"
    );
    anyhow::ensure!(
        mark(&rb, "nav:automations")? == ["Automations", "current=true"]
            && mark(&rb, "nav:new-thread")? == ["New thread", "current=false"],
        "item 14: the Automations row is not marked while its page is open"
    );
    anyhow::ensure!(
        rb["bounds"]["c:l-work"].is_object(),
        "item 14: the thread list must stay beside the Automations page"
    );
    anyhow::ensure!(
        mark(&rb, "row:l-work")?.contains(&"selected=false".to_owned()),
        "item 14: the open thread must not read as current beside the Automations page"
    );
    capture(window, cx, output.join("n-automations.png"))?;
    window.update(cx, |shell, _, cx| shell.fixture_leave_settings(true, cx))?;
    frames(window, cx, 300).await?;
    let rb = readback(window, cx)?;
    anyhow::ensure!(
        rb["route"] == "Chat"
            && rb["selected"] == "l-work"
            && mark(&rb, "row:l-work")?.contains(&"selected=true".to_owned()),
        "item 14: Back must return to the open, marked thread: {} {}",
        rb["route"],
        rb["selected"]
    );
    window.update(cx, |shell, _, cx| shell.fixture_open_settings(cx))?;
    frames(window, cx, 300).await?;
    let rb = readback(window, cx)?;
    let listed = strings(&rb["texts"]["settings-nav"]);
    anyhow::ensure!(
        !listed.is_empty() && !listed.contains(&"Automations".to_owned()),
        "item 14: Settings must no longer list Automations: {listed:?}"
    );
    window.update(cx, |shell, _, cx| {
        shell.fixture_open_automation_settings(cx)
    })?;
    frames(window, cx, 300).await?;
    let rb = readback(window, cx)?;
    anyhow::ensure!(
        rb["route"] == "Page(Automations)",
        "item 14: Settings › Automations must land on the page: {}",
        rb["route"]
    );

    // Item 14: New thread opens the new-thread canvas.
    click_label(window, cx, "nav-new-thread").await?;
    let rb = readback(window, cx)?;
    anyhow::ensure!(
        rb["route"] == "Chat" && rb["selected"].is_null(),
        "item 14: New thread must open the new-thread canvas: {} {}",
        rb["route"],
        rb["selected"]
    );
    capture(window, cx, output.join("n-new-thread.png"))?;
    Ok(serde_json::json!({ "settingsNav": listed, "route": route }))
}

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: sidebar-redesign-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(
        output.is_absolute(),
        "the evidence directory must be absolute"
    );
    std::fs::create_dir_all(&output)?;
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
            settings.space_filter = Some(P_LUNA.into());
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
                state.devices = serde_json::from_value(serde_json::json!([
                    {"id": DEVICE, "name": "Studio Mac", "platform": "macos", "lastSeenAt": null},
                    {"id": REMOTE, "name": "Mac mini", "platform": "macos", "lastSeenAt": null}
                ]))
                .unwrap();
                state.spaces = vec![
                    space(P_LUNA, "luna"),
                    space(P_WS, "workspace"),
                    space(P_INFRA, "infra-monitors"),
                ];
                state.chats = seed_chats();
                state.sessions = sessions();
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
                    cx.update(|cx| motion::set_reduced_motion(cx, true));
                    window.update(cx, |shell, _, cx| shell.fixture_enable_sidebar_probe(cx))?;
                    frames(window, cx, 900).await?;
                    let mut record = serde_json::Map::new();
                    for font in [
                        typography::UiFontSize::ALL[0],
                        typography::UiFontSize::ALL[4],
                        typography::UiFontSize::ALL[6],
                    ] {
                        let pass = scale_pass(window, cx, &output, font).await?;
                        record.insert(format!("{}px", font.pixels() as u32), pass);
                    }
                    let behaviour = behaviour_pass(window, cx, &output).await?;
                    record.insert("behaviour".into(), behaviour);
                    let navigation = navigation_pass(window, cx, &output).await?;
                    record.insert("navigation".into(), navigation);
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
                                output.join("sidebar-redesign.pass"),
                                "PASS: headless production Shell with real pointer and keyboard input at Interface sizes 12, 16 and 20 (dark): a single project shows a pill with its tile, name and live counts and two-line rows whose titles and provider icons share one left edge, with the status at the right edge (square loader, done dot, Approve/Reply/Failed pills) and none at all on idle rows; the picker lists every project with its path and need/working counts, marks the current one and reaches New project… and Manage projects… by keyboard; All projects groups tasks under alphabetical project headers with their own counts, compact one-line rows on one title edge whose reserved and painted heights agree, a five-row cap per project, folding that keeps the summary and jump order, and Settled/Archived still reachable; the project pill owns the top row, New thread and Automations sit between it and the Threads header (which carries the view options and Activity), Automations opens as its own page beside the thread list (Back returns, Settings no longer lists it), New thread opens the canvas, and a row paints a pin only as its one hover control\n",
                            )
                        });
                        if let Err(error) = written {
                            let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                            std::process::exit(1);
                        }
                    }
                    Err(error) => {
                        eprintln!("sidebar-redesign fixture failed: {error:#}");
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
