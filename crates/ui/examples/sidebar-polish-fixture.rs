//! Headless GPUI E2E for three sidebar fixes: even side spacing around the
//! thread rows, an Activity filter that only lists tasks with something
//! happening, and a project picker without the machine name.
//!
//! Drives the production `Shell` in a hidden window with real pointer input
//! (move/down/up through GPUI's hit testing) against synthetic AppState rows
//! and a synthetic Activity snapshot (no engine, provider, account or
//! credential). Geometry is measured through the opt-in sidebar probe and the
//! workbench canvas bounds, never estimated.
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. Uneven sides: a thread row's gap to the window's left edge differs from
//!    its gap to the main canvas's left border (the visible edge on its
//!    right) by more than a point, at Interface size 12, 16 or 20, in the
//!    task list or in the Activity view.
//! 2. The header drifts off the rows' grid: the project trigger doesn't start
//!    where the rows start, or the bell doesn't end where they end.
//! 3. Activity lists a task with nothing happening: a read, idle task whose
//!    only record is an earlier completion (the engine's Recent list) shows
//!    up, in this project or another one.
//! 4. Activity drops a task that needs you or is running: an unread
//!    completion, a pending approval or a working session is missing.
//! 5. A completion read from Activity lingers after you move on: once another
//!    task is opened, the read one must leave the list (it may stay while it
//!    is the one being read).
//! 6. Closing Activity loses tasks: the project's full list (idle ones
//!    included) must come back.
//! 7. The machine name is still shown: the project trigger renders an
//!    "@ device" tag, or a uniquely named project's dropdown row does.
//! 8. Same-named projects on different machines become indistinguishable:
//!    the dropdown must still tag those rows with their machine.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example sidebar-polish-fixture
//! Run:   target/debug/examples/sidebar-polish-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes PNG captures per Interface size (12, 16, 20; dark), readback.json
//! and sidebar-polish.pass; a failure writes failure.txt/failure.png and
//! exits nonzero.
//!
//! Set LUNA_SIDEBAR_AX_NATIVE=1 for a native macOS window that checks the
//! rendered accessibility tree instead of the headless geometry cases.
//! Failure cases, before the cache mitigation: sidebar Buttons vanish after
//! a composer-only redraw; Settings navigation vanishes on a later frame;
//! an unchanged AX frame is mistaken for fresh evidence. Preserve initial
//! and subsequent tree JSON plus screenshots. This mode uses no provider.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, MouseButton, PlatformInput, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};
use luna_proto::{
    ATTENTION_CONTRACT_VERSION, AttentionItem, AttentionKind, AttentionLifecycle,
    AttentionSeverity, AttentionSnapshot, AttentionSourcePointer, AttentionTarget, ChatConfig,
    HarnessId, PermissionMode, SandboxLevel, Session, SessionStatus,
};
use luna_ui::*;

const DEVICE: &str = "studio";
const REMOTE: &str = "build";
const PROJECT: &str = "proj-luna";
const DOCS: &str = "proj-docs";
const ANARA_LOCAL: &str = "proj-anara-local";
const ANARA_REMOTE: &str = "proj-anara-remote";
const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 860.0;

/// Read, idle tasks whose only Activity record is an earlier completion.
/// With the three below the project lists five tasks: exactly one page.
const IDLE: [(&str, &str, i64); 2] = [
    ("l-idle-1", "Idle review", 4),
    ("l-idle-2", "Old refactor", 8),
];
const DOCS_IDLE: &str = "d-idle";
const UNREAD: &str = "l-unread";
const WORKING: &str = "l-working";
const APPROVAL: &str = "l-approval";

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::ClaudeCode,
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

/// A task last active `minutes_ago`, already read unless `unread`.
fn chat(id: &str, title: &str, project: &str, minutes_ago: i64, unread: bool) -> luna_proto::Chat {
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
        branch: Some("main".into()),
        checkout_id: None,
        source_context: None,
        config: Some(config()),
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

fn space(id: &str, device: &str, name: &str) -> luna_proto::Space {
    luna_proto::Space {
        id: id.into(),
        device_id: device.into(),
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
    let mut chats: Vec<_> = IDLE
        .iter()
        .map(|(id, title, minutes)| chat(id, title, PROJECT, *minutes, false))
        .collect();
    chats.push(chat(UNREAD, "Unread result", PROJECT, 6, true));
    chats.push(chat(WORKING, "Running build", PROJECT, 12, false));
    chats.push(chat(APPROVAL, "Needs approval", PROJECT, 15, false));
    chats.push(chat(DOCS_IDLE, "Docs pass", DOCS, 5, false));
    chats
}

fn attention(task_id: &str, kind: AttentionKind, minutes_ago: i64) -> AttentionItem {
    let at = (Utc::now() - TimeDelta::minutes(minutes_ago)).timestamp_millis();
    AttentionItem {
        contract_version: ATTENTION_CONTRACT_VERSION,
        id: format!("{task_id}-{kind:?}"),
        dedup_key: format!("{task_id}-{kind:?}"),
        source_key: "synthetic".into(),
        kind,
        severity: AttentionSeverity::Info,
        lifecycle: if kind == AttentionKind::TaskCompleted {
            AttentionLifecycle::Resolved
        } else {
            AttentionLifecycle::Unresolved
        },
        environment_id: DEVICE.into(),
        device_id: DEVICE.into(),
        task_id: Some(task_id.into()),
        turn_id: None,
        request_id: None,
        title: "Synthetic".into(),
        summary: "Synthetic".into(),
        created_at_ms: at,
        updated_at_ms: at,
        seen_at_ms: None,
        acknowledged_at_ms: None,
        resolved_at_ms: None,
        target: AttentionTarget::Task {
            task_id: task_id.into(),
        },
        source: AttentionSourcePointer {
            task_id: Some(task_id.into()),
            message_id: None,
            part_id: None,
            request_id: None,
            queue_id: None,
            recovery_id: None,
            activity_event_id: None,
        },
    }
}

/// What the engine reported for the user's report: nothing needs attention
/// but the approval, and every task (idle ones included) has a completion
/// from the last few minutes in Recent.
fn snapshot() -> AttentionSnapshot {
    let mut recent: Vec<_> = IDLE
        .iter()
        .map(|(id, _, minutes)| attention(id, AttentionKind::TaskCompleted, *minutes))
        .collect();
    recent.push(attention(UNREAD, AttentionKind::TaskCompleted, 6));
    recent.push(attention(DOCS_IDLE, AttentionKind::TaskCompleted, 5));
    let needs_attention = vec![attention(APPROVAL, AttentionKind::ApprovalRequired, 15)];
    AttentionSnapshot {
        contract_version: ATTENTION_CONTRACT_VERSION,
        revision: 1,
        read_at_ms: Utc::now().timestamp_millis(),
        unresolved_count: needs_attention.len() as u32,
        needs_attention,
        recent,
        producers: Vec::new(),
    }
}

fn working_session() -> Session {
    Session {
        last_completed_turn: None,
        chat_id: WORKING.into(),
        device_id: DEVICE.into(),
        status: SessionStatus::Working,
        started_at: Some(Utc::now()),
        // Heartbeats are 45 s fresh; the run outlives that, so the synthetic
        // heartbeat sits ahead of the clock instead of going stale mid-run.
        updated_at: Utc::now() + TimeDelta::hours(1),
        recovery: None,
    }
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

#[cfg(target_os = "macos")]
fn activate_accessibility(window: &gpui::Window) -> anyhow::Result<()> {
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("window handle unavailable: {error}"))?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        anyhow::bail!("native AX fixture requires an AppKit window");
    };
    let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: this live window and its native view are accessed on the main
    // thread. Check each returned pointer before sending its next message.
    let native_window: *mut AnyObject = unsafe { objc2::msg_send![view, window] };
    anyhow::ensure!(!native_window.is_null(), "fixture view has no NSWindow");
    let content: *mut AnyObject = unsafe { objc2::msg_send![native_window, contentView] };
    anyhow::ensure!(!content.is_null(), "fixture window has no content view");
    let _children: *mut AnyObject = unsafe { objc2::msg_send![content, accessibilityChildren] };
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn activate_accessibility(_: &gpui::Window) -> anyhow::Result<()> {
    anyhow::bail!("native AX fixture is implemented for macOS only")
}

fn check_ax_frame(
    window: Window,
    cx: &mut AsyncApp,
    output: &Path,
    name: &str,
    labels: &[&str],
    previous_frame: Option<u64>,
) -> anyhow::Result<u64> {
    let text = gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, _| window.debug_a11y_tree_json())?
        .ok_or_else(|| anyhow::anyhow!("AX tree missing for {name}"))?;
    std::fs::write(output.join(format!("{name}.json")), &text)?;
    let tree: serde_json::Value = serde_json::from_str(&text)?;
    let frame = tree["frame"]["frame_number"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("AX frame number missing for {name}"))?;
    anyhow::ensure!(
        previous_frame.is_none_or(|previous| frame > previous),
        "{name} reused stale AX frame {frame} (previous {previous_frame:?})"
    );
    let nodes = tree["nodes"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("AX nodes missing for {name}"))?;
    for label in labels {
        anyhow::ensure!(
            nodes.values().any(|node| {
                node["aria"]["role"] == "Button" && node["aria"]["label"] == *label
            }),
            "{name}: accessible Button {label:?} is missing"
        );
    }
    Ok(frame)
}

async fn native_ax_pass(
    window: Window,
    cx: &mut AsyncApp,
    output: &Path,
) -> anyhow::Result<serde_json::Value> {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, _| activate_accessibility(window))??;
    // Let GPUI observe AccessKit activation, then establish a known full frame.
    settle(cx, 100).await;
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.refresh();
        window.draw(cx).clear();
    })?;
    let sidebar_labels = [
        "Idle review",
        "Old refactor",
        "Sidebar view options",
        "LUNA",
    ];
    let mut frame = check_ax_frame(
        window,
        cx,
        output,
        "ax-sidebar-initial",
        &sidebar_labels,
        None,
    )?;
    capture(window, cx, output.join("ax-sidebar-initial.png"))?;
    let composer = window.update(cx, |shell, _, _| shell.fixture_appshots_composer())?;
    for index in 1..=3 {
        // No Shell notification or window.refresh: only a sibling is dirty.
        composer.update(cx, |_, cx| cx.notify());
        draw(window, cx)?;
        frame = check_ax_frame(
            window,
            cx,
            output,
            &format!("ax-sidebar-sibling-{index}"),
            &sidebar_labels,
            Some(frame),
        )?;
    }
    capture(window, cx, output.join("ax-sidebar-after-sibling.png"))?;

    window.update(cx, |shell, _, cx| shell.fixture_open_settings(cx))?;
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.refresh();
        window.draw(cx).clear();
    })?;
    let settings_labels = ["General settings", "Appearance settings", "Back to chat"];
    frame = check_ax_frame(
        window,
        cx,
        output,
        "ax-settings-initial",
        &settings_labels,
        Some(frame),
    )?;
    capture(window, cx, output.join("ax-settings-initial.png"))?;
    for index in 1..=3 {
        // A new frame without dirtying the Shell must retain cached siblings.
        draw(window, cx)?;
        frame = check_ax_frame(
            window,
            cx,
            output,
            &format!("ax-settings-frame-{index}"),
            &settings_labels,
            Some(frame),
        )?;
    }
    capture(window, cx, output.join("ax-settings-after-frames.png"))?;
    Ok(serde_json::json!({
        "native": true,
        "sidebarLabels": sidebar_labels,
        "settingsLabels": settings_labels,
        "subsequentFramesPerSurface": 3,
        "lastFrame": frame,
    }))
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

/// A real click: hover, down/up, then park the pointer over the canvas so
/// no tooltip or hover wash lingers into the next readback.
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
    frames(window, cx, 400).await
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

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

async fn click_label(window: Window, cx: &mut AsyncApp, label: &str) -> anyhow::Result<()> {
    let rb = readback(window, cx)?;
    let target = bounds_of(&rb, label)?;
    let x = num(&target["x"]) + num(&target["w"]) / 2.0;
    let y = num(&target["y"]) + num(&target["h"]) / 2.0;
    eprintln!("click {label} at ({x}, {y})");
    click_at(window, cx, x, y).await
}

/// Items 1 and 2: every listed row sits evenly between the window's left
/// edge and the canvas's left border, and the header shares the rows' edges.
fn check_spacing(
    rb: &serde_json::Value,
    step: &str,
    activity: bool,
) -> anyhow::Result<serde_json::Value> {
    let canvas = &rb["canvas"];
    anyhow::ensure!(
        !canvas.is_null(),
        "{step}: the canvas bounds were not recorded"
    );
    // The recorded canvas is the padding box inside its 1 px hairline.
    let border = num(&canvas["x"]) - 1.0;
    let mut rows = 0;
    let (mut row_left, mut row_right) = (f32::NAN, f32::NAN);
    for entry in rb["keyed"].as_array().into_iter().flatten() {
        let key = entry[0].as_str().unwrap_or_default();
        if !key.starts_with("c:") {
            continue;
        }
        let row = bounds_of(rb, key)?;
        let (left, right) = (num(&row["x"]), num(&row["x"]) + num(&row["w"]));
        let (outer, inner) = (left, border - right);
        anyhow::ensure!(
            outer > 0.0 && (outer - inner).abs() <= 1.0,
            "{step}: {key} must sit evenly between the window edge and the canvas border: {outer} pt on the left, {inner} pt on the right ({row}, canvas {canvas})"
        );
        row_left = left;
        row_right = right;
        rows += 1;
    }
    anyhow::ensure!(rows > 0, "{step}: no thread rows painted");
    let bell = bounds_of(rb, "header-activity")?;
    let bell_right = num(&bell["x"]) + num(&bell["w"]);
    anyhow::ensure!(
        (bell_right - row_right).abs() <= 0.5,
        "{step}: the bell must end where the rows end: {bell_right} vs {row_right}"
    );
    let lead = bounds_of(
        rb,
        if activity {
            "header-activity-label"
        } else {
            "header-trigger"
        },
    )?;
    anyhow::ensure!(
        (num(&lead["x"]) - row_left).abs() <= 0.5,
        "{step}: the header must start where the rows start: {lead} vs {row_left}"
    );
    Ok(serde_json::json!({
        "rows": rows,
        "left": row_left,
        "right": border - row_right,
        "canvasBorder": border,
    }))
}

fn listed(rb: &serde_json::Value) -> Vec<String> {
    strings(&rb["order"])
}

/// Items 7 and 8: no machine on the trigger; in the dropdown only the two
/// same-named projects carry theirs.
fn check_picker_texts(rb: &serde_json::Value, menu_open: bool) -> anyhow::Result<()> {
    // The pill's first line is the project's name alone; its second line is
    // the live task summary (sidebar redesign), never a machine.
    let trigger = strings(&rb["texts"]["spaces-trigger"]);
    anyhow::ensure!(
        trigger.first().map(String::as_str) == Some("LUNA")
            && !trigger.iter().any(|line| line.contains('@')),
        "the project trigger must show only the project name: {trigger:?}"
    );
    if !menu_open {
        return Ok(());
    }
    let menu: BTreeSet<String> = strings(&rb["texts"]["spaces-menu"]).into_iter().collect();
    let expected: BTreeSet<String> = [
        "All projects",
        "LUNA",
        "Docs site",
        "anara @ Studio Mac",
        "anara @ Build server",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    anyhow::ensure!(
        menu == expected,
        "the dropdown rows must drop the machine except to tell same-named projects apart: {menu:?}"
    );
    Ok(())
}

async fn scale_pass(
    window: Window,
    cx: &mut AsyncApp,
    output: &Path,
    font: typography::UiFontSize,
) -> anyhow::Result<serde_json::Value> {
    let px_size = font.pixels() as u32;
    window.update(cx, |_, window, cx| {
        typography::set_font_size(font, window, cx);
    })?;
    frames(window, cx, 500).await?;
    let rb = readback(window, cx)?;
    let spacing = check_spacing(&rb, &format!("{px_size}px list"), false)?;
    check_picker_texts(&rb, false)?;
    capture(window, cx, output.join(format!("s{px_size}-list.png")))?;

    click_label(window, cx, "header-activity").await?;
    let rb = readback(window, cx)?;
    anyhow::ensure!(
        rb["activityView"] == true,
        "{px_size}px: the bell did not open Activity"
    );
    let activity_spacing = check_spacing(&rb, &format!("{px_size}px activity"), true)?;
    capture(window, cx, output.join(format!("s{px_size}-activity.png")))?;
    click_label(window, cx, "header-activity").await?;
    let rb = readback(window, cx)?;
    anyhow::ensure!(
        rb["activityView"] == false,
        "{px_size}px: the bell did not close Activity"
    );
    Ok(serde_json::json!({
        "list": spacing,
        "activity": activity_spacing,
    }))
}

/// Items 3-8 at the default Interface size, with real clicks.
async fn behaviour_pass(
    window: Window,
    state: &gpui::Entity<state::AppState>,
    cx: &mut AsyncApp,
    output: &Path,
) -> anyhow::Result<serde_json::Value> {
    let mut record = serde_json::Map::new();
    window.update(cx, |_, window, cx| {
        typography::set_font_size(typography::UiFontSize::default(), window, cx);
    })?;
    frames(window, cx, 400).await?;

    // Items 7 and 8: the trigger, then the open dropdown.
    let rb = readback(window, cx)?;
    check_picker_texts(&rb, false)?;
    click_label(window, cx, "header-trigger").await?;
    let rb = readback(window, cx)?;
    check_picker_texts(&rb, true)?;
    capture(window, cx, output.join("b-picker-open.png"))?;
    record.insert("picker".into(), rb["texts"].clone());
    // A second press on the trigger closes the menu.
    click_label(window, cx, "header-trigger").await?;

    // Item 6 baseline: the project's whole list.
    let rb = readback(window, cx)?;
    let full: BTreeSet<String> = listed(&rb).into_iter().collect();
    let expected_full: BTreeSet<String> = IDLE
        .iter()
        .map(|(id, _, _)| id.to_string())
        .chain([UNREAD, WORKING, APPROVAL].map(str::to_owned))
        .collect();
    anyhow::ensure!(
        full == expected_full,
        "the project's list must show every task before Activity: {full:?}"
    );

    // Items 3 and 4.
    click_label(window, cx, "header-activity").await?;
    let rb = readback(window, cx)?;
    anyhow::ensure!(rb["activityView"] == true, "the bell did not open Activity");
    let activity = listed(&rb);
    anyhow::ensure!(
        activity == [UNREAD, APPROVAL, WORKING],
        "Activity must list exactly the unread completion and the approval (most recent first), then the running task: {activity:?}"
    );
    capture(window, cx, output.join("b-activity.png"))?;
    record.insert("activity".into(), serde_json::json!(activity));

    // Item 5: open the unread completion from Activity, then move on.
    click_label(window, cx, &format!("c:{UNREAD}")).await?;
    let rb = readback(window, cx)?;
    let reading = listed(&rb);
    anyhow::ensure!(
        reading.iter().any(|id| id == UNREAD),
        "the completion being read must stay listed while it is open: {reading:?}"
    );
    click_label(window, cx, &format!("c:{WORKING}")).await?;
    let rb = readback(window, cx)?;
    // The task being read joins the "needs you" group, so only membership
    // is fixed here, not order.
    let after = listed(&rb);
    let after_set: BTreeSet<&str> = after.iter().map(String::as_str).collect();
    anyhow::ensure!(
        after_set == BTreeSet::from([APPROVAL, WORKING]),
        "a completion that has been read must leave Activity once another task opens: {after:?}"
    );
    let seen = state.update(cx, |state, _| {
        state
            .chats
            .iter()
            .find(|chat| chat.id == UNREAD)
            .is_some_and(|chat| !chat.unseen())
    });
    anyhow::ensure!(seen, "moving on did not mark the completion read");
    capture(window, cx, output.join("b-activity-after-read.png"))?;
    record.insert("activityAfterRead".into(), serde_json::json!(after));

    // Item 6: closing Activity restores the list.
    click_label(window, cx, "header-activity").await?;
    let rb = readback(window, cx)?;
    anyhow::ensure!(
        rb["activityView"] == false,
        "the bell did not close Activity"
    );
    let restored: BTreeSet<String> = listed(&rb).into_iter().collect();
    anyhow::ensure!(
        restored == expected_full,
        "closing Activity must restore the project's list: {restored:?}"
    );
    capture(window, cx, output.join("b-list-restored.png"))?;

    // The Settings sidebar shares the gutter; its capture is for review.
    window.update(cx, |shell, _, cx| shell.fixture_open_settings(cx))?;
    frames(window, cx, 400).await?;
    capture(window, cx, output.join("b-settings.png"))?;
    window.update(cx, |shell, _, cx| shell.fixture_leave_settings(true, cx))?;
    frames(window, cx, 200).await?;
    Ok(serde_json::Value::Object(record))
}

fn main() -> anyhow::Result<()> {
    let native_ax = std::env::var("LUNA_SIDEBAR_AX_NATIVE").is_ok_and(|value| value == "1");
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: sidebar-polish-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in [
        "readback.json",
        "sidebar-polish.pass",
        "failure.txt",
        "failure.png",
    ] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let temp = tempfile::tempdir()?;
    let data = temp.path().to_path_buf();
    let application = if native_ax {
        gpui_platform::application()
    } else {
        gpui_platform::headless()
    };
    application
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let mut settings = settings::UiSettings::default();
            settings.space_filter = Some(PROJECT.into());
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
                    {"id": REMOTE, "name": "Build server", "platform": "linux", "lastSeenAt": null}
                ]))
                .unwrap();
                state.spaces = vec![
                    space(PROJECT, DEVICE, "LUNA"),
                    space(DOCS, DEVICE, "Docs site"),
                    space(ANARA_LOCAL, DEVICE, "anara"),
                    space(ANARA_REMOTE, REMOTE, "anara"),
                ];
                state.chats = seed_chats();
                state.sessions = vec![working_session()];
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
                        show: native_ax,
                        focus: native_ax,
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
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_enable_sidebar_probe(cx);
                        shell.fixture_set_attention_snapshot(snapshot(), cx);
                    })?;
                    frames(window, cx, 900).await?;
                    let mut record = serde_json::Map::new();
                    if native_ax {
                        let pass = native_ax_pass(window, cx, &output).await?;
                        record.insert("nativeAccessibility".into(), pass);
                    } else {
                        for font in [
                            typography::UiFontSize::ALL[0],
                            typography::UiFontSize::ALL[4],
                            typography::UiFontSize::ALL[6],
                        ] {
                            let pass = scale_pass(window, cx, &output, font).await?;
                            record.insert(format!("{}px", font.pixels() as u32), pass);
                        }
                        let behaviour = behaviour_pass(window, &ui_state, cx, &output).await?;
                        record.insert("behaviour".into(), behaviour);
                    }
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
                                output.join("sidebar-polish.pass"),
                                if native_ax {
                                    "PASS: native rendered AX tree retains sidebar task/project Buttons through composer-only frames and Settings navigation Buttons through later frames; all frame numbers advance; JSON and PNG evidence saved\n"
                                } else {
                                    "PASS: headless production Shell with real pointer input at Interface sizes 12, 16 and 20 (dark): thread rows sit evenly between the window edge and the canvas border in the list and in Activity, with the header on the same edges; Activity lists only the unread completion, the pending approval and the running task (no read, idle task with just an earlier completion, in any project); a completion read from Activity leaves once another task opens, and closing Activity restores the full list; the project trigger shows no machine, and the dropdown names a machine only for the two same-named projects\n"
                                },
                            )
                        });
                        if let Err(error) = written {
                            let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                            std::process::exit(1);
                        }
                    }
                    Err(error) => {
                        eprintln!("sidebar-polish fixture failed: {error:#}");
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
