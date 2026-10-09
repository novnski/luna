//! Headless GPUI E2E for the chrome controls' shortcut tooltips and the
//! shortcuts they name: the titlebar (left sidebar, Back, Forward, right
//! sidebar), the Threads header (view options, Activity) and the sidebar
//! footer (Settings, Devices, Usage).
//!
//! Drives the production `Shell` in a hidden window with real pointer and
//! keyboard input (through GPUI's hit testing and keymap) against synthetic
//! AppState rows (no engine, provider, account or credential). Layout probes
//! record where each control painted (`control:{id}`) and what each tooltip
//! painted (`tooltip:{title}|{keys}`).
//!
//! Failure cases this run must catch (written before the fixture):
//! 1. A chrome control paints no tooltip under the pointer (user report: the
//!    arrows, both sidebar toggles, Settings and the footer showed nothing).
//! 2. A tooltip names the wrong control, or its state is stale (the left or
//!    right sidebar toggle says Show while it hides, Activity says View
//!    while the view is on).
//! 3. A tooltip leaves out the configured chord of a bound action (left
//!    sidebar, Back, Forward, right sidebar, Settings, Usage, Activity), or
//!    shows one for a control without a shortcut (Devices, view options).
//! 4. A rebinding is not reflected: after Activity moves to another chord and
//!    Usage is cleared, a tooltip still shows the old chord, the old chord
//!    still acts, or the new one does not.
//! 5. A named chord does not act: Usage does not open and close the usage
//!    card; Activity does not open and close its view (also from a collapsed
//!    sidebar, which must open so the view shows); Back and Forward do not
//!    move through route history; the right sidebar chord does not toggle it.
//! 6. An upgrade loses or steals a chord: a keymap saved before these
//!    shortcuts (Activity unbound, no Back/Forward/Usage) does not receive
//!    the defaults where they are free; one where the user already bound the
//!    Activity chord to another action has it taken away; one saved after
//!    the upgrade with Activity cleared on purpose gets it back.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example chrome-shortcuts-fixture
//! Run:   target/debug/examples/chrome-shortcuts-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes tooltip PNGs, readback.json and chrome-shortcuts.pass; a failure
//! writes failure.txt/failure.png and exits nonzero.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, PlatformInput, WindowBounds, WindowHandle, WindowOptions, point,
    px, size,
};
use luna_proto::{ChatConfig, HarnessId, PermissionMode, SandboxLevel};
use luna_ui::settings::{KeymapConfig, ShortcutId, UiSettings, badge_combo, platform_combo};
use luna_ui::*;
use serde_json::{Value, json};

const DEVICE: &str = "studio";
const PROJECT: &str = "proj-luna";
const FIRST: &str = "c-first";
const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 860.0;

type Window = WindowHandle<shell::Shell>;

fn chat(id: &str, title: &str, minutes_ago: i64) -> luna_proto::Chat {
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
        config: Some(ChatConfig {
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
        }),
        last_message_preview: None,
        last_message_at: Some(at),
        created_at: at - TimeDelta::minutes(3),
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: Some(PROJECT.into()),
        last_seen_at: Some(at + TimeDelta::seconds(1)),
        manually_unread: false,
        room_gen: None,
    }
}

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

/// Press a stored combo ("mod-shift-m") as this platform spells it.
async fn press(window: Window, cx: &mut AsyncApp, combo: &str) -> anyhow::Result<()> {
    let keystroke = gpui::Keystroke::parse(&platform_combo(combo))?;
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
    frames(window, cx, 200).await
}

fn sidebar(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    draw(window, cx)?;
    let value = window.update(cx, |shell, _, cx| {
        shell.fixture_sidebar_sections_readback(cx)
    })?;
    anyhow::ensure!(!value.is_null(), "the sidebar probe is not recording");
    Ok(value)
}

fn frame(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    draw(window, cx)?;
    Ok(window.update(cx, |shell, _, cx| shell.fixture_frame_readback(cx))?)
}

fn usage_open(window: Window, cx: &mut AsyncApp) -> anyhow::Result<bool> {
    Ok(window.update(cx, |shell, _, _| shell.fixture_usage_card_is_open())?)
}

/// The keycaps a default chord reads as on this platform.
fn keys(id: ShortcutId) -> String {
    badge_combo(id.default_combo())
}

/// One chrome control's expected tooltip.
struct Expect {
    control: &'static str,
    title: &'static str,
    keys: Option<String>,
}

/// Hover `control` until its tooltip paints; the one painted tooltip.
async fn tooltip_of(
    window: Window,
    cx: &mut AsyncApp,
    control: &str,
) -> anyhow::Result<(String, String, [f32; 4])> {
    // A fresh Shell render re-records every control probe.
    perf_counters::clear_bounds("control:");
    window.update(cx, |_, _, cx| cx.notify())?;
    frames(window, cx, 60).await?;
    let bounds = perf_counters::bounds(&format!("control:{control}"))
        .ok_or_else(|| anyhow::anyhow!("item 1: control {control} did not paint"))?;
    let (x, y) = (bounds[0] + bounds[2] / 2.0, bounds[1] + bounds[3] / 2.0);
    perf_counters::clear_bounds("tooltip:");
    hover_at(window, cx, x, y)?;
    // Past the 400 ms show delay.
    frames(window, cx, 750).await?;
    let painted = perf_counters::probes("tooltip:");
    anyhow::ensure!(
        painted.len() == 1,
        "item 1: hovering {control} at ({x}, {y}) painted {} tooltips: {painted:?}",
        painted.len()
    );
    let name = painted[0].0.trim_start_matches("tooltip:").to_string();
    let (title, keys) = name
        .split_once('|')
        .map(|(t, k)| (t.to_string(), k.to_string()))
        .unwrap_or((name, String::new()));
    Ok((title, keys, bounds))
}

async fn leave(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    hover_at(window, cx, WIDTH / 2.0, HEIGHT / 2.0)?;
    frames(window, cx, 150).await
}

async fn check_tooltip(
    window: Window,
    cx: &mut AsyncApp,
    expect: &Expect,
    step: &str,
) -> anyhow::Result<Value> {
    let (title, keys, bounds) = tooltip_of(window, cx, expect.control).await?;
    anyhow::ensure!(
        title == expect.title,
        "item 2 ({step}): {} tooltip says {title:?}, expected {:?}",
        expect.control,
        expect.title
    );
    let want = expect.keys.clone().unwrap_or_default();
    anyhow::ensure!(
        keys == want,
        "item 3 ({step}): {} tooltip shows chord {keys:?}, expected {want:?}",
        expect.control
    );
    leave(window, cx).await?;
    Ok(json!({ "control": expect.control, "title": title, "keys": keys, "bounds": bounds }))
}

/// Items 1-3: every chrome control's tooltip, as painted.
async fn tooltips_pass(window: Window, cx: &mut AsyncApp, output: &Path) -> anyhow::Result<Value> {
    let expected = [
        Expect {
            control: "toggle-sidebar",
            title: "Hide left sidebar",
            keys: Some(keys(ShortcutId::ToggleSidebar)),
        },
        Expect {
            control: "nav-back",
            title: "Back",
            keys: Some(keys(ShortcutId::NavigateBack)),
        },
        Expect {
            control: "nav-forward",
            title: "Forward",
            keys: Some(keys(ShortcutId::NavigateForward)),
        },
        Expect {
            control: "toggle-changes",
            title: "Show right sidebar",
            keys: Some(keys(ShortcutId::ToggleChanges)),
        },
        Expect {
            control: "sidebar-view-options",
            title: "Sidebar view options",
            keys: None,
        },
        Expect {
            control: "sidebar-activity-action",
            title: "View activity",
            keys: Some(keys(ShortcutId::OpenActivity)),
        },
        Expect {
            control: "sidebar-settings-action",
            title: "Settings",
            keys: Some(badge_combo("mod-,")),
        },
        Expect {
            control: "sidebar-devices-action",
            title: "Devices",
            keys: None,
        },
        Expect {
            control: "usage-sidebar-action",
            title: "Usage",
            keys: Some(keys(ShortcutId::ToggleUsage)),
        },
    ];
    let mut out = Vec::new();
    for expect in &expected {
        out.push(check_tooltip(window, cx, expect, "defaults").await?);
    }
    // Evidence: one titlebar tooltip and one footer tooltip as painted.
    tooltip_of(window, cx, "nav-back").await?;
    capture(window, cx, output.join("tooltip-back.png"))?;
    leave(window, cx).await?;
    tooltip_of(window, cx, "sidebar-activity-action").await?;
    capture(window, cx, output.join("tooltip-activity.png"))?;
    leave(window, cx).await?;
    Ok(Value::Array(out))
}

/// Item 5 (and item 2's state-dependent titles): each named chord acts.
async fn actions_pass(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    let mut record = serde_json::Map::new();

    // Usage opens and closes the card.
    let usage = ShortcutId::ToggleUsage.default_combo();
    anyhow::ensure!(!usage_open(window, cx)?, "the usage card starts closed");
    press(window, cx, usage).await?;
    anyhow::ensure!(
        usage_open(window, cx)?,
        "item 5: {usage} did not open usage"
    );
    press(window, cx, usage).await?;
    anyhow::ensure!(
        !usage_open(window, cx)?,
        "item 5: {usage} did not close usage"
    );
    record.insert("usage".into(), json!("opens and closes"));

    // Activity opens and closes its view; the bell's tooltip follows.
    let activity = ShortcutId::OpenActivity.default_combo();
    press(window, cx, activity).await?;
    anyhow::ensure!(
        sidebar(window, cx)?["activityView"] == true,
        "item 5: {activity} did not open Activity"
    );
    check_tooltip(
        window,
        cx,
        &Expect {
            control: "sidebar-activity-action",
            title: "Turn off activity view",
            keys: Some(keys(ShortcutId::OpenActivity)),
        },
        "activity on",
    )
    .await?;
    press(window, cx, activity).await?;
    anyhow::ensure!(
        sidebar(window, cx)?["activityView"] == false,
        "item 5: {activity} did not close Activity"
    );

    // From a collapsed sidebar Activity opens the sidebar with it.
    let toggle = ShortcutId::ToggleSidebar.default_combo();
    press(window, cx, toggle).await?;
    frames(window, cx, 400).await?;
    anyhow::ensure!(
        frame(window, cx)?["sidebarCollapsed"] == true,
        "{toggle} did not collapse the sidebar"
    );
    check_tooltip(
        window,
        cx,
        &Expect {
            control: "toggle-sidebar",
            title: "Show left sidebar",
            keys: Some(keys(ShortcutId::ToggleSidebar)),
        },
        "sidebar collapsed",
    )
    .await?;
    press(window, cx, activity).await?;
    frames(window, cx, 400).await?;
    let (collapsed, view) = (
        frame(window, cx)?["sidebarCollapsed"].clone(),
        sidebar(window, cx)?["activityView"].clone(),
    );
    anyhow::ensure!(
        collapsed == false && view == true,
        "item 5: {activity} from a collapsed sidebar must show Activity: collapsed {collapsed}, view {view}"
    );
    press(window, cx, activity).await?;
    record.insert(
        "activity".into(),
        json!("opens and closes; opens a collapsed sidebar"),
    );

    // The right sidebar chord toggles it, and its tooltip follows.
    let right = ShortcutId::ToggleChanges.default_combo();
    press(window, cx, right).await?;
    frames(window, cx, 400).await?;
    anyhow::ensure!(
        frame(window, cx)?["rightOpen"] == true,
        "item 5: {right} did not open the right sidebar"
    );
    check_tooltip(
        window,
        cx,
        &Expect {
            control: "toggle-changes",
            title: "Hide right sidebar",
            keys: Some(keys(ShortcutId::ToggleChanges)),
        },
        "right sidebar open",
    )
    .await?;
    press(window, cx, right).await?;
    frames(window, cx, 400).await?;
    anyhow::ensure!(
        frame(window, cx)?["rightOpen"] == false,
        "item 5: {right} did not close the right sidebar"
    );
    record.insert("rightSidebar".into(), json!("opens and closes"));

    // Back and Forward move through route history (chat ⇄ Settings).
    let (back, forward) = (
        ShortcutId::NavigateBack.default_combo(),
        ShortcutId::NavigateForward.default_combo(),
    );
    window.update(cx, |shell, _, cx| shell.fixture_open_settings(cx))?;
    frames(window, cx, 300).await?;
    anyhow::ensure!(
        frame(window, cx)?["settingsRoute"] == true,
        "Settings did not open"
    );
    press(window, cx, back).await?;
    let rb = sidebar(window, cx)?;
    anyhow::ensure!(
        rb["route"] == "Chat" && rb["selected"] == FIRST,
        "item 5: {back} did not return to the chat: route {}, selected {}",
        rb["route"],
        rb["selected"]
    );
    press(window, cx, forward).await?;
    anyhow::ensure!(
        frame(window, cx)?["settingsRoute"] == true,
        "item 5: {forward} did not return to Settings"
    );
    press(window, cx, back).await?;
    anyhow::ensure!(
        sidebar(window, cx)?["route"] == "Chat",
        "item 5: a second {back} did not return to the chat"
    );
    record.insert(
        "history".into(),
        json!("back and forward between chat and Settings"),
    );
    frames(window, cx, 300).await?;
    Ok(Value::Object(record))
}

/// Item 4: a rebinding shows (and acts) at once; the old chord is gone.
async fn rebinding_pass(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    let mut keymap = KeymapConfig::default();
    keymap.set(ShortcutId::OpenActivity, "mod-shift-y".into());
    keymap.set(ShortcutId::ToggleUsage, String::new());
    window.update(cx, |shell, _, cx| shell.fixture_load_keymap(keymap, cx))?;
    frames(window, cx, 200).await?;
    check_tooltip(
        window,
        cx,
        &Expect {
            control: "sidebar-activity-action",
            title: "View activity",
            keys: Some(badge_combo("mod-shift-y")),
        },
        "rebound",
    )
    .await?;
    check_tooltip(
        window,
        cx,
        &Expect {
            control: "usage-sidebar-action",
            title: "Usage",
            keys: None,
        },
        "cleared",
    )
    .await?;
    press(window, cx, ShortcutId::OpenActivity.default_combo()).await?;
    anyhow::ensure!(
        sidebar(window, cx)?["activityView"] == false,
        "item 4: the old Activity chord still acts after rebinding"
    );
    press(window, cx, ShortcutId::ToggleUsage.default_combo()).await?;
    anyhow::ensure!(
        !usage_open(window, cx)?,
        "item 4: the cleared Usage chord still acts"
    );
    press(window, cx, "mod-shift-y").await?;
    anyhow::ensure!(
        sidebar(window, cx)?["activityView"] == true,
        "item 4: the new Activity chord does not act"
    );
    press(window, cx, "mod-shift-y").await?;
    window.update(cx, |shell, _, cx| {
        shell.fixture_load_keymap(KeymapConfig::default(), cx)
    })?;
    frames(window, cx, 200).await?;
    Ok(json!({ "activity": badge_combo("mod-shift-y"), "usage": "unbound" }))
}

/// Item 6: the real loader's upgrade of keymaps saved by older builds.
fn migration_pass() -> anyhow::Result<Value> {
    let load = |keymap: Value| -> anyhow::Result<KeymapConfig> {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            UiSettings::path(dir.path()),
            serde_json::to_vec(&json!({ "keymap": keymap }))?,
        )?;
        Ok(UiSettings::load(dir.path()).keymap)
    };
    // Every field an older build saved: the current keymap without the new
    // ones, Activity unbound.
    let mut legacy = serde_json::to_value(KeymapConfig::default())?;
    let fields = legacy.as_object_mut().expect("keymap object");
    for field in ["toggleUsage", "navigateBack", "navigateForward"] {
        fields.remove(field);
    }
    fields.insert("openActivity".into(), json!(""));

    let upgraded = load(legacy.clone())?;
    for id in [
        ShortcutId::OpenActivity,
        ShortcutId::ToggleUsage,
        ShortcutId::NavigateBack,
        ShortcutId::NavigateForward,
    ] {
        anyhow::ensure!(
            upgraded.get(id) == id.default_combo(),
            "item 6: an upgraded keymap gave {id:?} {:?}, expected its free default {:?}",
            upgraded.get(id),
            id.default_combo()
        );
    }

    // The user already put Activity's chord on Archive: Archive keeps it.
    let mut taken = legacy.clone();
    taken["archiveSession"] = json!(ShortcutId::OpenActivity.default_combo());
    let kept = load(taken)?;
    anyhow::ensure!(
        kept.get(ShortcutId::ArchiveSession) == ShortcutId::OpenActivity.default_combo()
            && kept.get(ShortcutId::OpenActivity).is_empty(),
        "item 6: an upgrade took the user's {} from Archive: archive {:?}, activity {:?}",
        ShortcutId::OpenActivity.default_combo(),
        kept.get(ShortcutId::ArchiveSession),
        kept.get(ShortcutId::OpenActivity)
    );

    // Saved after the upgrade with Activity cleared on purpose: stays clear.
    let mut cleared = serde_json::to_value(KeymapConfig::default())?;
    cleared["openActivity"] = json!("");
    let after = load(cleared)?;
    anyhow::ensure!(
        after.get(ShortcutId::OpenActivity).is_empty(),
        "item 6: a deliberately cleared Activity chord came back as {:?}",
        after.get(ShortcutId::OpenActivity)
    );
    Ok(json!({
        "upgraded": {
            "openActivity": upgraded.get(ShortcutId::OpenActivity),
            "toggleUsage": upgraded.get(ShortcutId::ToggleUsage),
            "navigateBack": upgraded.get(ShortcutId::NavigateBack),
            "navigateForward": upgraded.get(ShortcutId::NavigateForward),
        },
        "chordTakenByArchive": "Activity left unbound",
        "clearedAfterUpgrade": "stays unbound",
    }))
}

fn main() -> anyhow::Result<()> {
    // SAFETY: first thing in `main`, before the Tokio runtime, the GPUI
    // platform or any other thread exists; never changed afterwards.
    unsafe {
        std::env::set_var("LUNA_PERF_COUNTERS", "1");
    }
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: chrome-shortcuts-fixture ABSOLUTE_EVIDENCE_DIR")
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
                state.devices = serde_json::from_value(json!([
                    {"id": DEVICE, "name": "Studio Mac", "platform": "macos", "lastSeenAt": null}
                ]))
                .unwrap();
                state.spaces = vec![luna_proto::Space {
                    id: PROJECT.into(),
                    device_id: DEVICE.into(),
                    path: "/synthetic/luna".into(),
                    name: Some("luna".into()),
                    git_detected: true,
                    git_checked_at: None,
                    checkout_id: None,
                    execution_defaults: Default::default(),
                    created_at: Utc::now() - TimeDelta::days(30),
                    removed_at: None,
                }];
                state.chats = vec![
                    chat(FIRST, "Shortcut tooltips", 2),
                    chat("c-second", "Usage card polish", 9),
                ];
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
                let result: anyhow::Result<Value> = async {
                    cx.update(|cx| motion::set_reduced_motion(cx, true));
                    perf_counters::set_probes_enabled(true);
                    window.update(cx, |shell, _, cx| shell.fixture_enable_sidebar_probe(cx))?;
                    state.update(cx, |state, cx| state.select_chat(Some(FIRST.into()), cx));
                    frames(window, cx, 900).await?;
                    anyhow::ensure!(
                        sidebar(window, cx)?["selected"] == FIRST,
                        "the fixture chat is not selected"
                    );
                    let mut record = serde_json::Map::new();
                    record.insert("tooltips".into(), tooltips_pass(window, cx, &output).await?);
                    record.insert("actions".into(), actions_pass(window, cx).await?);
                    record.insert("rebinding".into(), rebinding_pass(window, cx).await?);
                    record.insert("migration".into(), migration_pass()?);
                    Ok(Value::Object(record))
                }
                .await;
                match result {
                    Ok(record) => {
                        let written = std::fs::write(
                            output.join("readback.json"),
                            serde_json::to_vec_pretty(&record).unwrap_or_default(),
                        )
                        .and_then(|_| std::fs::write(
                            output.join("chrome-shortcuts.pass"),
                            "PASS: headless production Shell with real pointer and keyboard input (dark): the left sidebar, Back, Forward and right sidebar titlebar controls, the Threads header's view options and Activity, and the sidebar footer's Settings, Devices and Usage each paint one tooltip naming the control in its current state with its configured chord (none for Devices and view options); the Usage, Activity (also from a collapsed sidebar), right sidebar, Back and Forward chords act; a rebinding shows and acts at once while the old and cleared chords stop; and the loader upgrades older keymaps to the free defaults without taking a chord the user already bound or restoring one they cleared\n",
                        ));
                        if let Err(error) = written {
                            let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                            std::process::exit(1);
                        }
                    }
                    Err(error) => {
                        eprintln!("chrome-shortcuts fixture failed: {error:#}");
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
