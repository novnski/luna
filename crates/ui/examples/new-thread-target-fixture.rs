//! Headless GPUI E2E for the new thread's target controls in the frame's
//! bottom row: the machine, project, checkout-kind and branch pickers that
//! used to float above and below the new-thread composer now sit where a
//! working thread names its checkout and branch.
//!
//! Drives the production `Shell` in hidden windows against an in-process
//! `EngineCore`. Two Git projects (real repositories with a `main` branch
//! and a long feature branch) on a local machine with a long name, a second
//! machine, and one working chat whose footer is the comparison. Every
//! combination of Interface size (12, 16, 20 px) and theme (dark, light)
//! gets its own window, at 1200 px wide and at 900 px.
//!
//! Failure cases this run must catch (written before the change):
//! 1. A selector row still renders around the new-thread pill: any target
//!    chip (machine, project, checkout, branch) painted above the pill, or
//!    within 48 px below it, instead of inside the frame's bottom row.
//! 2. The bottom row of a new thread for a Git project misses one of the
//!    four target pickers, or shows Review changes / Commit & push… (there
//!    is no task to review or commit yet).
//! 3. A target chip leaves the bottom row: it extends past the row's left
//!    or right edge, crosses its top or bottom, overlaps another chip, or
//!    (900 px, 20 px, long machine and branch names) the row overflows
//!    instead of yielding. The yield order: the machine and branch labels
//!    truncate, the project never does (same width at 900 px as at
//!    1200 px); on a row narrower than 560 pt (at 16 px) the machine and
//!    checkout labels collapse to their glyphs, named by tooltip, and only
//!    then.
//! 4. A menu opened from the bottom row is clipped or misplaced: it does not
//!    open on a click, opens downward (its card's bottom is below the chip's
//!    top), or its card leaves the window's 8 px gutter on any side.
//! 5. Choosing in a bottom-row menu no longer sets the new thread's target:
//!    picking another project (click to open, arrow, Enter) leaves the
//!    selected project unchanged; picking another machine leaves the
//!    effective machine unchanged; New worktree leaves the checkout plan
//!    unchanged; another branch leaves the ref label unchanged.
//! 6. The two screens don't read as one design: the new-thread target
//!    chips and the working thread's checkout/branch labels differ in
//!    vertical centre by more than 1 px, or in glyph size; a chip is taller
//!    than the row's buttons (22 pt) or not centred in the row.
//! 7. Text-adjacent sizes don't follow the Interface size: a chip's height
//!    at 12 and 20 px is not 0.75x and 1.25x of its 16 px height (±1 px).
//! 8. A target chip has no accessible name naming its kind and value
//!    ("Project: luna"), or is not a keyboard control (Enter opens it).
//! 9. The pill is no longer placed sensibly once the rows are gone: its
//!    centre is more than 24 px from the canvas centre, or it overlaps the
//!    bottom row.
//! 10. The route change jumps: sending from the new thread (selecting the
//!     working chat) shows the target pickers and the thread's footer at
//!     the same time, the target layer's opacity ever rises or the thread
//!     footer's ever falls during the change, or the change does not end
//!     with the thread footer fully shown and no target chips; the reverse
//!     (Cmd+N back) must mirror it.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example new-thread-target-fixture
//! Run:   target/debug/examples/new-thread-target-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes one PNG per state and combination, readback.json, index.html and
//! new-thread-target.pass; a failure writes failure.txt and exits nonzero.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, Keystroke, MouseButton, PlatformInput, WindowBounds,
    WindowHandle, WindowOptions, point, px, size,
};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::mock::MockHarness;
use luna_proto::{
    AgentEvent, ChatConfig, ConversationSourceContext, DoneStatus, HarnessId, PermissionMode,
    SandboxLevel,
};
use luna_ui::*;
use serde_json::{Value, json};

const LUNA_SPACE: &str = "luna-space";
const SITE_SPACE: &str = "website-space";
const THREAD: &str = "d7d7d7d7-0000-4000-8000-000000000001";
/// The luna checkout sits on this branch, so the branch chip carries a long
/// label on every window; `main` exists beside it.
const LONG_BRANCH: &str = "newthread-polish/target-footer-in-the-bottom-bar";
const LOCAL_NAME: &str = "Iulian's MacBook Pro 16";
const OTHER_DEVICE: &str = "studio-mac-fixture-device";
const OTHER_NAME: &str = "Studio Mac";
const HEIGHT: f32 = 860.0;
const WIDTHS: [f32; 2] = [1200.0, 900.0];
const SIZES: [f32; 3] = [12.0, 16.0, 20.0];
const TARGETS: [&str; 4] = [
    "picker-device",
    "picker-project",
    "picker-checkout",
    "picker-branch",
];

type Win = WindowHandle<shell::Shell>;

fn git(dir: &Path, args: &[&str]) -> anyhow::Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn repository(dir: &Path, branch: &str, others: &[&str]) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    git(dir, &["init", "-q", "-b", branch])?;
    std::fs::write(dir.join("README.md"), "fixture\n")?;
    git(dir, &["add", "README.md"])?;
    git(dir, &["commit", "-q", "-m", "init"])?;
    for other in others {
        git(dir, &["branch", other])?;
    }
    Ok(())
}

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

fn render(window: Win, cx: &mut AsyncApp) -> anyhow::Result<image::RgbaImage> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.render_to_image()
    })?
}

/// Two identical consecutive frames, or the last of a few tries.
async fn stable(window: Win, cx: &mut AsyncApp) -> anyhow::Result<image::RgbaImage> {
    let mut last = render(window, cx)?;
    for _ in 0..10 {
        settle(cx, 120).await;
        let next = render(window, cx)?;
        if next.as_raw() == last.as_raw() {
            return Ok(next);
        }
        last = next;
    }
    Ok(last)
}

fn input(window: Win, cx: &mut AsyncApp, event: PlatformInput) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(event, cx);
    })?;
    Ok(())
}

fn hover(window: Win, cx: &mut AsyncApp, at: (f32, f32)) -> anyhow::Result<()> {
    input(
        window,
        cx,
        PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position: point(px(at.0), px(at.1)),
            pressed_button: None,
            modifiers: Default::default(),
        }),
    )
}

async fn click(window: Win, cx: &mut AsyncApp, at: (f32, f32)) -> anyhow::Result<()> {
    hover(window, cx, at)?;
    draw(window, cx)?;
    settle(cx, 60).await;
    let position = point(px(at.0), px(at.1));
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
    settle(cx, 250).await;
    draw(window, cx)?;
    Ok(())
}

async fn keys(window: Win, cx: &mut AsyncApp, combos: &[&str]) -> anyhow::Result<()> {
    for combo in combos {
        let keystroke = Keystroke::parse(combo)?;
        gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
            window.dispatch_keystroke(keystroke, cx);
        })?;
        draw(window, cx)?;
        settle(cx, 40).await;
    }
    settle(cx, 200).await;
    draw(window, cx)?;
    Ok(())
}

/// Type `text` into the focused field, one key at a time.
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

fn park_pointer(window: Win, cx: &mut AsyncApp, width: f32) -> anyhow::Result<()> {
    hover(window, cx, (width - 4.0, 4.0))
}

fn footer(window: Win, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    window.update(cx, |shell, _, cx| shell.fixture_frame_footer(cx))
}

fn rect(value: &Value) -> Option<[f32; 4]> {
    let a = value.as_array()?;
    Some([
        a.first()?.as_f64()? as f32,
        a.get(1)?.as_f64()? as f32,
        a.get(2)?.as_f64()? as f32,
        a.get(3)?.as_f64()? as f32,
    ])
}

fn chip(readback: &Value, id: &str) -> Option<(Value, [f32; 4])> {
    readback["targets"]["chips"]
        .as_array()?
        .iter()
        .find(|chip| chip["id"] == id)
        .and_then(|chip| rect(&chip["bounds"]).map(|bounds| (chip.clone(), bounds)))
}

fn centre(r: [f32; 4]) -> (f32, f32) {
    (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
}

/// Poll the bottom row until `ready` holds (or time out with the last one).
async fn footer_until(
    window: Win,
    cx: &mut AsyncApp,
    timeout_ms: u64,
    ready: impl Fn(&Value) -> bool,
) -> anyhow::Result<Value> {
    let started = Instant::now();
    loop {
        draw(window, cx)?;
        let readback = footer(window, cx)?;
        if ready(&readback) || started.elapsed() > Duration::from_millis(timeout_ms) {
            return Ok(readback);
        }
        settle(cx, 60).await;
    }
}

struct Shot {
    file: String,
    caption: String,
}

fn write_index(output: &Path, shots: &[Shot]) -> anyhow::Result<()> {
    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><title>New thread target</title>\
         <style>body{font:13px system-ui;background:#888;margin:16px}figure{display:inline-block;\
         margin:0 12px 16px 0;vertical-align:top}img{width:600px;display:block}</style>",
    );
    for shot in shots {
        html.push_str(&format!(
            "<figure><img src=\"{}\"><figcaption>{}</figcaption></figure>",
            shot.file, shot.caption
        ));
    }
    std::fs::write(output.join("index.html"), html)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
        anyhow::anyhow!("usage: new-thread-target-fixture ABSOLUTE_EVIDENCE_DIR")
    })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["readback.json", "new-thread-target.pass", "failure.txt"] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let root = tempfile::tempdir()?;
    let ui_data = root.path().join("ui");
    let engine_data = root.path().join("engine");
    let luna = root.path().join("luna");
    let website = root.path().join("website");
    std::fs::create_dir_all(&ui_data)?;
    repository(&luna, LONG_BRANCH, &["main"])?;
    repository(&website, "main", &[])?;
    let runtime = tokio::runtime::Runtime::new()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        }],
    }));
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "target-fixture", "target-fixture"),
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
    auth.dev_user_id = "target-fixture".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let device = core.device_id.clone();
    for (id, path, name) in [
        (LUNA_SPACE, &luna, "luna"),
        (SITE_SPACE, &website, "website"),
    ] {
        core.workspace.create_space(
            id,
            &device,
            path.to_str().unwrap(),
            Some(name.into()),
            true,
        )?;
    }
    let now = Utc::now();
    core.workspace.create_chat(
        THREAD,
        Some(LUNA_SPACE),
        Some(&device),
        Some(config()),
        Some(luna.to_string_lossy().into_owned()),
    )?;
    core.workspace
        .rename_chat(THREAD, "Polish the new thread footer")?;
    let mut thread = core.workspace.chat(THREAD)?.expect("seeded chat");
    thread.created_at = now - TimeDelta::minutes(5);
    thread.last_message_at = Some(now - TimeDelta::minutes(1));
    thread.last_seen_at = Some(now);
    thread.source_context = Some(ConversationSourceContext {
        checkout_id: "fixture-checkout".into(),
        repo_root: luna.to_string_lossy().into_owned(),
        cwd: luna.to_string_lossy().into_owned(),
        branch: "main".into(),
        head_sha: None,
        observed_at: now,
    });
    core.workspace.import_chat_row(&thread)?;
    let spaces = core.workspace.read_spaces()?;
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
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
    gpui_platform::headless()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::default();
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
            let state = cx.new(|cx| {
                let mut state = state::AppState::new();
                state.local_device_id = Some(device.clone());
                state.devices = vec![
                    serde_json::from_value(json!({
                        "id": device, "name": LOCAL_NAME, "platform": "macos", "lastSeenAt": null
                    }))
                    .unwrap(),
                    serde_json::from_value(json!({
                        "id": OTHER_DEVICE, "name": OTHER_NAME, "platform": "macos",
                        "lastSeenAt": Utc::now().to_rfc3339(),
                    }))
                    .unwrap(),
                ];
                state.spaces = spaces;
                state.chats = vec![thread];
                state.chats_synced = true;
                state.spaces_synced = true;
                state.auto_selected = true;
                state.connection = state::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
                state.fixture_attachment_engine(handle);
                state.select_space(Some(LUNA_SPACE.into()), cx);
                state
            });
            cx.spawn(async move |cx| {
                let result = run(cx, state, boot, device, &output).await;
                match result {
                    Ok(record) => {
                        let written = std::fs::write(
                            output.join("readback.json"),
                            serde_json::to_vec_pretty(&record).unwrap_or_default(),
                        )
                        .and_then(|_| {
                            std::fs::write(
                                output.join("new-thread-target.pass"),
                                "PASS: headless production Shell; the new thread's machine, project, checkout and branch pickers sit in the frame's bottom row (none around the pill, no Review/Commit), inside the row and the window at 12/16/20 px, dark and light, 1200 and 900 px wide; their menus open upward inside the window; picking a project, machine, checkout and branch from the row sets the target; chips share the working thread's footer centre line and scale with the Interface size; the route crossfade never shows both rows and runs monotonically both ways\n",
                            )
                        });
                        if let Err(error) = written {
                            *failure_for_ui.lock().unwrap() = Some(error.to_string());
                            let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                            std::process::exit(1);
                        }
                    }
                    Err(error) => {
                        eprintln!("New thread target fixture failed: {error:#}");
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                        *failure_for_ui.lock().unwrap() = Some(error.to_string());
                        std::process::exit(1);
                    }
                }
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

/// Geometry checks shared by every new-thread capture (cases 1-3, 6, 7, 9).
fn check_new_thread(
    name: &str,
    readback: &Value,
    pixels: f32,
    width: f32,
    failures: &mut Vec<String>,
) {
    let mut fail = |message: String| failures.push(format!("{name}: {message}"));
    if readback["framed"] != false {
        fail("the new thread renders inside the workbench frame".into());
    }
    let Some(row) = rect(&readback["row"]) else {
        fail("the bottom row was not painted".into());
        return;
    };
    let Some(pill) = rect(&readback["pill"]) else {
        fail("the composer pill was not painted".into());
        return;
    };
    // Case 2: all four pickers, no Git actions.
    let chips: Vec<(String, [f32; 4], Value)> = TARGETS
        .iter()
        .filter_map(|id| chip(readback, id).map(|(chip, bounds)| (id.to_string(), bounds, chip)))
        .collect();
    if chips.len() != TARGETS.len() {
        fail(format!(
            "the bottom row shows {} of the four target pickers: {}",
            chips.len(),
            readback["targets"]["chips"]
        ));
    }
    if readback["buttons"]
        .as_array()
        .is_some_and(|buttons| !buttons.is_empty())
    {
        fail(format!(
            "a new thread shows Git actions: {}",
            readback["buttons"]
        ));
    }
    let scale = pixels / 16.0;
    let row_mid = row[1] + row[3] / 2.0;
    for (index, (id, b, chip)) in chips.iter().enumerate() {
        // Case 1: in the bottom row, never around the pill.
        if b[1] < pill[1] + pill[3] + 48.0 {
            fail(format!(
                "{id} sits around the pill ({b:?} vs pill {pill:?})"
            ));
        }
        // Case 3: inside the row.
        if b[0] < row[0] - 0.5
            || b[0] + b[2] > row[0] + row[2] + 0.5
            || b[1] < row[1] - 0.5
            || b[1] + b[3] > row[1] + row[3] + 0.5
        {
            fail(format!("{id} leaves the bottom row ({b:?} vs row {row:?})"));
        }
        if b[0] + b[2] > width - 8.0 {
            fail(format!("{id} runs past the window edge ({b:?})"));
        }
        for (other, o, _) in chips.iter().skip(index + 1) {
            if b[0] < o[0] + o[2] - 0.5 && o[0] < b[0] + b[2] - 0.5 {
                fail(format!("{id} overlaps {other} ({b:?} / {o:?})"));
            }
        }
        // Cases 6 and 7: the row buttons' height, scaled; centred.
        if (b[3] - 22.0 * scale).abs() > 1.0 {
            fail(format!(
                "{id} is {} px tall, expected {} at {pixels} px",
                b[3],
                22.0 * scale
            ));
        }
        if (centre(*b).1 - row_mid).abs() > 1.0 {
            fail(format!(
                "{id} is off the row's centre line ({b:?}, row {row:?})"
            ));
        }
        // Case 8: named by kind and value.
        let name = chip["name"].as_str().unwrap_or_default();
        let label = chip["label"].as_str().unwrap_or_default();
        if label.is_empty() || !name.ends_with(label) || !name.contains(": ") {
            fail(format!(
                "{id} has no accessible name with its value: {chip}"
            ));
        }
    }
    // Case 3: how a narrow row yields. Below 560 pt (at 16 px) the machine
    // chip is its glyph alone; above it, labelled.
    if let Some((_, device)) = chip(readback, "picker-device") {
        let glyph_only = device[2] < 2.6 * device[3];
        let narrow = row[2] < 560.0 * scale;
        if glyph_only != narrow {
            fail(format!(
                "the machine chip is {} on a {} px row (narrow below {})",
                if glyph_only { "glyph-only" } else { "labelled" },
                row[2],
                560.0 * scale
            ));
        }
    }
    // Case 9: the pill near the canvas centre, clear of the row.
    if let Some(canvas) = rect(&readback["canvas"]) {
        let offset = centre(pill).1 - centre(canvas).1;
        if offset.abs() > 24.0 {
            fail(format!("the pill sits {offset} px from the canvas centre"));
        }
    } else {
        fail("the canvas was not measured".into());
    }
    if pill[1] + pill[3] > row[1] {
        fail("the pill overlaps the bottom row".into());
    }
}

/// Case 4: a menu opened from the row opens upward, inside the window.
fn check_menu(
    name: &str,
    readback: &Value,
    trigger: [f32; 4],
    kind: &str,
    width: f32,
    failures: &mut Vec<String>,
) {
    let open = readback["targets"]["open"].as_str().unwrap_or_default();
    if open != kind {
        failures.push(format!(
            "{name}: a click on the chip did not open {kind} (open: {open})"
        ));
        return;
    }
    let Some(menu) = rect(&readback["targets"]["menu"]) else {
        failures.push(format!("{name}: the {kind} menu card was not painted"));
        return;
    };
    if menu[1] + menu[3] > trigger[1] + 0.5 {
        failures.push(format!(
            "{name}: the {kind} menu opens downward ({menu:?} vs chip {trigger:?})"
        ));
    }
    if menu[0] < 8.0 - 0.5
        || menu[1] < 8.0 - 0.5
        || menu[0] + menu[2] > width - 8.0 + 0.5
        || menu[1] + menu[3] > HEIGHT - 8.0 + 0.5
    {
        failures.push(format!(
            "{name}: the {kind} menu leaves the window ({menu:?})"
        ));
    }
}

#[allow(clippy::too_many_arguments)]
async fn open_menu(
    window: Win,
    cx: &mut AsyncApp,
    id: &str,
    kind: &str,
    name: &str,
    width: f32,
    failures: &mut Vec<String>,
) -> anyhow::Result<(Value, image::RgbaImage)> {
    let before = footer(window, cx)?;
    let (_, trigger) = chip(&before, id)
        .ok_or_else(|| anyhow::anyhow!("{name}: {id} is not painted: {before}"))?;
    click(window, cx, centre(trigger)).await?;
    let readback = footer_until(window, cx, 3000, |r| {
        r["targets"]["open"] == kind && !r["targets"]["menu"].is_null()
    })
    .await?;
    let image = stable(window, cx).await?;
    check_menu(name, &readback, trigger, kind, width, failures);
    Ok((readback, image))
}

async fn close_menu(window: Win, cx: &mut AsyncApp) -> anyhow::Result<()> {
    keys(window, cx, &["escape"]).await?;
    footer_until(window, cx, 2000, |r| r["targets"]["open"].is_null()).await?;
    settle(cx, 250).await;
    Ok(())
}

/// Open a bottom-row menu by clicking its chip, optionally type into its
/// search box, then press `keys_after`; returns the target afterwards.
#[allow(clippy::too_many_arguments)]
async fn pick(
    window: Win,
    cx: &mut AsyncApp,
    width: f32,
    id: &str,
    kind: &str,
    typed: Option<&str>,
    keys_after: &[&str],
) -> anyhow::Result<Value> {
    let mut checks = Vec::new();
    open_menu(
        window,
        cx,
        id,
        kind,
        &format!("pick-{kind}"),
        width,
        &mut checks,
    )
    .await?;
    if let Some(text) = typed {
        type_text(window, cx, text).await?;
    }
    keys(window, cx, keys_after).await?;
    settle(cx, 300).await;
    let after = footer_until(window, cx, 2000, |r| r["targets"]["open"].is_null()).await?;
    Ok(json!({"kind": kind, "menuChecks": checks, "after": after["targets"].clone()}))
}

/// Case 5: every bottom-row menu still sets the new thread's target.
async fn check_picks(
    window: Win,
    cx: &mut AsyncApp,
    local: &str,
    width: f32,
    failures: &mut Vec<String>,
) -> anyhow::Result<Value> {
    let mut log = Vec::new();
    // Project: luna -> website -> luna.
    let to_site = pick(
        window,
        cx,
        width,
        "picker-project",
        "Space",
        Some("website"),
        &["enter"],
    )
    .await?;
    if to_site["after"]["selectedSpace"] != SITE_SPACE {
        failures.push(format!(
            "picking website in the bottom row left the project at {}",
            to_site["after"]["selectedSpace"]
        ));
    }
    log.push(to_site);
    let back = pick(
        window,
        cx,
        width,
        "picker-project",
        "Space",
        Some("luna"),
        &["enter"],
    )
    .await?;
    if back["after"]["selectedSpace"] != LUNA_SPACE {
        failures.push(format!(
            "picking luna in the bottom row left the project at {}",
            back["after"]["selectedSpace"]
        ));
    }
    log.push(back);
    // Machine: local -> Studio Mac -> local (then luna again).
    let studio = pick(
        window,
        cx,
        width,
        "picker-device",
        "Device",
        Some("studio"),
        &["enter"],
    )
    .await?;
    if studio["after"]["effectiveDevice"] != OTHER_DEVICE {
        failures.push(format!(
            "picking Studio Mac in the bottom row left the machine at {}",
            studio["after"]["effectiveDevice"]
        ));
    }
    log.push(studio);
    let home = pick(
        window,
        cx,
        width,
        "picker-device",
        "Device",
        Some("macbook"),
        &["enter"],
    )
    .await?;
    if home["after"]["effectiveDevice"] != local {
        failures.push(format!(
            "picking the MacBook in the bottom row left the machine at {}",
            home["after"]["effectiveDevice"]
        ));
    }
    log.push(home);
    let luna = pick(
        window,
        cx,
        width,
        "picker-project",
        "Space",
        Some("luna"),
        &["enter"],
    )
    .await?;
    log.push(luna);
    // Checkout: current checkout -> new worktree -> current checkout.
    let worktree = pick(
        window,
        cx,
        width,
        "picker-checkout",
        "Checkout",
        None,
        &["down", "enter"],
    )
    .await?;
    if !worktree["after"]["checkoutPlan"]
        .as_str()
        .unwrap_or_default()
        .starts_with("NewWorktree")
    {
        failures.push(format!(
            "New worktree from the bottom row left the plan at {}",
            worktree["after"]["checkoutPlan"]
        ));
    }
    log.push(worktree);
    let current = pick(
        window,
        cx,
        width,
        "picker-checkout",
        "Checkout",
        None,
        &["up", "enter"],
    )
    .await?;
    if current["after"]["checkoutPlan"]
        .as_str()
        .unwrap_or_default()
        .starts_with("NewWorktree")
    {
        failures.push(format!(
            "Current checkout from the bottom row left the plan at {}",
            current["after"]["checkoutPlan"]
        ));
    }
    log.push(current);
    // Branch: the long branch -> main -> the long branch.
    let main_ref = pick(
        window,
        cx,
        width,
        "picker-branch",
        "Branch",
        Some("main"),
        &["enter"],
    )
    .await?;
    if main_ref["after"]["ref"] != "main" {
        failures.push(format!(
            "picking main in the bottom row left the ref at {}",
            main_ref["after"]["ref"]
        ));
    }
    log.push(main_ref);
    let long = pick(
        window,
        cx,
        width,
        "picker-branch",
        "Branch",
        Some("target-footer"),
        &["enter"],
    )
    .await?;
    if long["after"]["ref"] != LONG_BRANCH {
        failures.push(format!(
            "picking {LONG_BRANCH} in the bottom row left the ref at {}",
            long["after"]["ref"]
        ));
    }
    log.push(long);
    Ok(json!(log))
}

/// Case 10: sample the route clock across a change; returns the samples.
async fn sample_route(
    window: Win,
    cx: &mut AsyncApp,
    arriving_thread: bool,
    output: &Path,
    shots: &mut Vec<Shot>,
    failures: &mut Vec<String>,
) -> anyhow::Result<Value> {
    let label = if arriving_thread {
        "to-thread"
    } else {
        "to-new-thread"
    };
    let mut samples = Vec::new();
    // The frame whose fading layer is nearest half opacity.
    let mut mid: Option<(f64, image::RgbaImage, f64, f64)> = None;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1100) {
        let image = render(window, cx)?;
        let readback = footer(window, cx)?;
        let target = readback["dock"]["targetOpacity"].as_f64().unwrap_or(-1.0);
        let thread = readback["dock"]["threadOpacity"].as_f64().unwrap_or(-1.0);
        let chips = readback["targets"]["chips"].as_array().map_or(0, Vec::len);
        let labels = readback["labels"].as_array().map_or(0, Vec::len);
        samples.push(json!({
            "ms": started.elapsed().as_millis() as u64,
            "target": target, "thread": thread, "chips": chips, "labels": labels,
        }));
        let fading = [target, thread]
            .into_iter()
            .filter(|opacity| *opacity > 0.0 && *opacity < 1.0)
            .map(|opacity| (opacity - 0.5).abs())
            .reduce(f64::min);
        if let Some(distance) = fading
            && mid.as_ref().is_none_or(|(best, ..)| distance < *best)
        {
            mid = Some((distance, image, target, thread));
        }
        settle(cx, 16).await;
    }
    let mid_saved = mid.is_some();
    if let Some((_, image, target, thread)) = mid {
        let file = format!("transition-{label}.png");
        image.save(output.join(&file))?;
        shots.push(Shot {
            file,
            caption: format!("mid crossfade {label} (target {target:.2}, thread {thread:.2})"),
        });
    }
    let mut last_target = if arriving_thread { 1.0 } else { 0.0 };
    let mut last_thread = if arriving_thread { 0.0 } else { 1.0 };
    for sample in &samples {
        let target = sample["target"].as_f64().unwrap_or(-1.0);
        let thread = sample["thread"].as_f64().unwrap_or(-1.0);
        if target > 0.0 && thread > 0.0 {
            failures.push(format!("{label}: both rows show at once: {sample}"));
        }
        let (target_ok, thread_ok) = if arriving_thread {
            (target <= last_target + 1e-4, thread >= last_thread - 1e-4)
        } else {
            (target >= last_target - 1e-4, thread <= last_thread + 1e-4)
        };
        if !target_ok || !thread_ok {
            failures.push(format!("{label}: the crossfade runs backwards: {sample}"));
        }
        if target <= 0.0 && sample["chips"].as_u64() != Some(0) {
            failures.push(format!(
                "{label}: target chips paint at zero opacity: {sample}"
            ));
        }
        last_target = target;
        last_thread = thread;
    }
    let end = samples.last().cloned().unwrap_or(Value::Null);
    let (want_target, want_thread) = if arriving_thread {
        (0.0, 1.0)
    } else {
        (1.0, 0.0)
    };
    if end["target"].as_f64() != Some(want_target) || end["thread"].as_f64() != Some(want_thread) {
        failures.push(format!("{label}: the change does not settle: {end}"));
    }
    if !mid_saved {
        failures.push(format!("{label}: no frame caught the crossfade"));
    }
    Ok(json!(samples))
}

async fn run(
    cx: &mut AsyncApp,
    state: gpui::Entity<state::AppState>,
    boot: EngineBootConfig,
    local: String,
    output: &Path,
) -> anyhow::Result<Value> {
    let mut shots = Vec::new();
    let mut record = serde_json::Map::new();
    let mut failures: Vec<String> = Vec::new();
    let mut heights: Vec<(f32, f32, f32)> = Vec::new();
    let mut project_widths: Vec<(f32, &str, f32, f32)> = Vec::new();
    for &width in &WIDTHS {
        for &pixels in &SIZES {
            for (theme_name, mode) in [
                ("dark", appearance::AppearanceMode::Dark),
                ("light", appearance::AppearanceMode::Light),
            ] {
                let name = format!("{theme_name}-{pixels}px-{width}w");
                cx.update(|cx| appearance::set_mode(mode, cx));
                cx.update(|cx| {
                    state.update(cx, |state, cx| {
                        state.select_chat(None, cx);
                        state.select_space(Some(LUNA_SPACE.into()), cx);
                    })
                });
                let interface = typography::UiFontSize::ALL
                    .into_iter()
                    .find(|size| size.pixels() == pixels)
                    .ok_or_else(|| anyhow::anyhow!("no {pixels}px Interface size"))?;
                let background =
                    cx.update(|cx| theme::Theme::of(cx).window_background_appearance());
                let shell_state = state.clone();
                let shell_boot = boot.clone();
                let window: Win = cx.open_window(
                    WindowOptions {
                        window_background: background,
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(20.), px(20.)),
                            size(px(width), px(HEIGHT)),
                        ))),
                        show: false,
                        focus: false,
                        ..Default::default()
                    },
                    move |window, cx| {
                        window.set_rem_size(px(typography::font_size(cx).pixels()));
                        cx.new(|cx| shell::Shell::new(shell_state, shell_boot, cx))
                    },
                )?;
                window.update(cx, |_, window, cx| {
                    typography::set_font_size(interface, window, cx);
                })?;
                cx.update(|cx| state.update(cx, |_, cx| cx.notify()));
                settle(cx, 900).await;
                let capture = |state_name: &str,
                               image: &image::RgbaImage,
                               shots: &mut Vec<Shot>|
                 -> anyhow::Result<()> {
                    let file = format!("{state_name}-{name}.png");
                    image.save(output.join(&file))?;
                    shots.push(Shot {
                        file,
                        caption: format!("{state_name} · {name}"),
                    });
                    Ok(())
                };
                let mut readback = serde_json::Map::new();

                // 1. The new thread: four chips (refs loaded), settled.
                window.update(cx, |_, window, _| window.blur())?;
                park_pointer(window, cx, width)?;
                let fresh = footer_until(window, cx, 8000, |r| {
                    r["targets"]["refsLoaded"] == true
                        && r["targets"]["chips"].as_array().map_or(0, Vec::len) == 4
                        && r["dock"]["active"] == false
                })
                .await?;
                let image = stable(window, cx).await?;
                capture("new-thread", &image, &mut shots)?;
                let fresh = footer(window, cx).ok().unwrap_or(fresh);
                check_new_thread(&name, &fresh, pixels, width, &mut failures);
                if let Some((_, b)) = chip(&fresh, "picker-project") {
                    heights.push((pixels, width, b[3]));
                    project_widths.push((pixels, theme_name, width, b[2]));
                }
                readback.insert("newThread".into(), fresh.clone());

                // 2. Menus from the row: the leftmost (machine) and the
                // widest (branch, rightmost).
                let (device_menu, image) = open_menu(
                    window,
                    cx,
                    "picker-device",
                    "Device",
                    &format!("{name}-device-menu"),
                    width,
                    &mut failures,
                )
                .await?;
                capture("device-menu", &image, &mut shots)?;
                close_menu(window, cx).await?;
                let (branch_menu, image) = open_menu(
                    window,
                    cx,
                    "picker-branch",
                    "Branch",
                    &format!("{name}-branch-menu"),
                    width,
                    &mut failures,
                )
                .await?;
                capture("branch-menu", &image, &mut shots)?;
                close_menu(window, cx).await?;
                readback.insert("deviceMenu".into(), device_menu["targets"].clone());
                readback.insert("branchMenu".into(), branch_menu["targets"].clone());

                // 3. Picks and the crossfade, once (16 px, dark, 1200 px).
                let primary = pixels == 16.0 && theme_name == "dark" && width > 1000.0;
                if primary {
                    let picks = check_picks(window, cx, &local, width, &mut failures).await?;
                    record.insert("picks".into(), picks);
                    window.update(cx, |_, window, _| window.blur())?;
                    park_pointer(window, cx, width)?;
                    footer_until(window, cx, 3000, |r| r["dock"]["active"] == false).await?;
                    cx.update(|cx| {
                        state.update(cx, |state, cx| state.select_chat(Some(THREAD.into()), cx))
                    });
                    let forward =
                        sample_route(window, cx, true, output, &mut shots, &mut failures).await?;
                    footer_until(window, cx, 3000, |r| r["dock"]["active"] == false).await?;
                    window.update(cx, |shell, _, cx| shell.fixture_new_session(cx))?;
                    let back =
                        sample_route(window, cx, false, output, &mut shots, &mut failures).await?;
                    record.insert(
                        "crossfade".into(),
                        json!({"toThread": forward, "toNewThread": back}),
                    );
                    footer_until(window, cx, 3000, |r| r["dock"]["active"] == false).await?;
                }

                // 4. The working thread's row, for comparison.
                cx.update(|cx| {
                    state.update(cx, |state, cx| state.select_chat(Some(THREAD.into()), cx))
                });
                window.update(cx, |_, window, _| window.blur())?;
                park_pointer(window, cx, width)?;
                let thread = footer_until(window, cx, 6000, |r| {
                    r["framed"] == true
                        && r["dock"]["active"] == false
                        && r["labels"].as_array().map_or(0, Vec::len) >= 2
                })
                .await?;
                let image = stable(window, cx).await?;
                capture("thread", &image, &mut shots)?;
                let thread = footer(window, cx).ok().unwrap_or(thread);
                {
                    let mut fail = |message: String| failures.push(format!("{name}: {message}"));
                    let labels = thread["labels"].as_array().cloned().unwrap_or_default();
                    let texts: Vec<&str> =
                        labels.iter().filter_map(|l| l["text"].as_str()).collect();
                    if !texts.contains(&"Local checkout") || !texts.contains(&"main") {
                        fail(format!(
                            "the thread row does not name its checkout and branch: {texts:?}"
                        ));
                    }
                    if thread["targets"]["chips"]
                        .as_array()
                        .is_some_and(|c| !c.is_empty())
                    {
                        fail("target chips remain in the working thread's row".into());
                    }
                    let ids: Vec<&str> = thread["buttons"]
                        .as_array()
                        .map(|b| b.iter().filter_map(|b| b["id"].as_str()).collect())
                        .unwrap_or_default();
                    if !ids.contains(&"wb-footer-review") || !ids.contains(&"wb-footer-commit") {
                        fail(format!("the thread row misses its Git actions: {ids:?}"));
                    }
                    // Case 6: one centre line across both screens.
                    let new_mid = chip(&fresh, "picker-checkout").map(|(_, b)| centre(b).1);
                    for label in &labels {
                        if let (Some(b), Some(mid)) = (rect(&label["bounds"]), new_mid)
                            && (centre(b).1 - mid).abs() > 1.0
                        {
                            fail(format!(
                                "{} sits {} px off the new thread's chips' centre line",
                                label["text"],
                                centre(b).1 - mid
                            ));
                        }
                    }
                }
                readback.insert("thread".into(), thread);
                record.insert(name.clone(), Value::Object(readback));
                cx.update(|cx| state.update(cx, |state, cx| state.select_chat(None, cx)));
                let _ = window.update(cx, |_, window, _| window.remove_window());
                settle(cx, 150).await;
            }
        }
    }
    // Case 7: the chip height follows the Interface size.
    for &(pixels, width, height) in &heights {
        if let Some(&(_, _, base)) = heights.iter().find(|(p, w, _)| *p == 16.0 && *w == width) {
            let want = base * pixels / 16.0;
            if (height - want).abs() > 1.0 {
                failures.push(format!(
                    "{width}w: the chip is {height} px at {pixels} px, expected {want}"
                ));
            }
        }
    }
    // Case 3: the narrow row yields from the machine and branch, never by
    // truncating the project.
    for &(pixels, theme_name, width, project) in &project_widths {
        if let Some(&(_, _, _, wide)) = project_widths
            .iter()
            .find(|(p, t, w, _)| *p == pixels && *t == theme_name && *w > width)
            && (project - wide).abs() > 0.5
        {
            failures.push(format!(
                "{theme_name}-{pixels}px-{width}w: the project chip shrank to {project} px (whole: {wide})"
            ));
        }
    }
    write_index(output, &shots)?;
    record.insert("failures".into(), json!(failures));
    if !failures.is_empty() && std::env::var_os("NEW_THREAD_TARGET_RECORD_ONLY").is_none() {
        std::fs::write(
            output.join("readback.json"),
            serde_json::to_vec_pretty(&Value::Object(record.clone()))?,
        )?;
        anyhow::bail!("{} failure(s):\n{}", failures.len(), failures.join("\n"));
    }
    Ok(Value::Object(record))
}
