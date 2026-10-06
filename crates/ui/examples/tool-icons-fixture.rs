//! Headless GPUI E2E for the activity chain's tool icons: every tool kind a
//! harness reports gets its own glyph, and every glyph paints in the row's
//! one tint.
//!
//! Drives the production `Shell` in hidden windows against an in-process
//! `EngineCore` with a Mock harness. One assistant turn is seeded with a
//! thought and one call of every tool kind (including the Unknown tools a
//! harness passes through by name, and one failed MCP call), so the chain
//! renders as a single open group. Dark and light, at the default Interface
//! size.
//!
//! Failure cases this run must catch (written before the fixture):
//! 1. A row shows the wrong glyph: the transcript readback for the group must
//!    name exactly the glyph chosen for that kind (a ToolSearch row falling
//!    back to the generic toolbox, an edit row still using the message pen),
//!    and the mixed group's summary must use the layers glyph.
//! 2. A glyph paints nothing (a missing or malformed SVG): every row's slot
//!    in the icon column must hold ink.
//! 3. Icons don't share one colour: every ink pixel of a row's icon must sit
//!    on the line between the background and that row's tint (the muted
//!    content colour, or the danger colour for the failed row). An SVG with
//!    its own fill colour, a partial opacity, or an accent tint breaks this.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example tool-icons-fixture
//! Run:   target/debug/examples/tool-icons-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes one PNG per theme, readback.json, index.html and tool-icons.pass;
//! a failure writes failure.txt and exits nonzero.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, Hsla, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::mock::MockHarness;
use luna_proto::{
    AgentEvent, ChatConfig, ComputerUseStatus, DoneStatus, HarnessId, PermissionMode, SandboxLevel,
    ToolCall,
};
use luna_ui::*;
use serde_json::json;

const PROJECT: &str = "tool-icons-project";
const CHAT: &str = "71c071c0-0000-4000-8000-000000000001";
const WIDTH: f32 = 1100.0;
const HEIGHT: f32 = 1500.0;

type Win = WindowHandle<shell::Shell>;

/// One chain row: its call, whether it failed, and the glyph it must show.
struct Row {
    label: &'static str,
    call: Option<ToolCall>,
    is_error: bool,
    glyph: &'static str,
}

fn unknown(name: &str) -> Option<ToolCall> {
    Some(ToolCall::Unknown {
        name: name.into(),
        input: None,
    })
}

fn rows() -> Vec<Row> {
    let row = |label, call, glyph| Row {
        label,
        call,
        is_error: false,
        glyph,
    };
    vec![
        row("thought", None, icons::BRAIN),
        row("ToolSearch", unknown("ToolSearch"), icons::PACKAGE_SEARCH),
        row(
            "MCP",
            Some(ToolCall::Mcp {
                server: "luna".into(),
                tool: "list_child_models".into(),
                input: None,
            }),
            icons::MCP_MARK,
        ),
        Row {
            label: "failed MCP",
            call: Some(ToolCall::Mcp {
                server: "luna".into(),
                tool: "list_child_models".into(),
                input: None,
            }),
            is_error: true,
            glyph: icons::DANGER_CIRCLE,
        },
        row(
            "Run",
            Some(ToolCall::Exec {
                command: "cargo check -p luna-ui".into(),
            }),
            icons::TERMINAL,
        ),
        row(
            "Read",
            Some(ToolCall::ReadFile {
                path: "crates/ui/src/transcript.rs".into(),
            }),
            icons::FILE_TEXT,
        ),
        row(
            "Write",
            Some(ToolCall::WriteFile {
                path: "notes.md".into(),
                content: None,
            }),
            icons::FILE_ADD,
        ),
        row(
            "Edit",
            Some(ToolCall::EditFile {
                path: "crates/ui/src/icons.rs".into(),
                old_string: None,
                new_string: None,
            }),
            icons::FILE_PEN,
        ),
        row(
            "Patch",
            Some(ToolCall::ApplyPatch { path: None }),
            icons::GIT_DIFF,
        ),
        row(
            "Search",
            Some(ToolCall::Search {
                pattern: "icons::WIDGET".into(),
                path: None,
            }),
            icons::FILE_SEARCH,
        ),
        row(
            "Find files",
            Some(ToolCall::Glob {
                pattern: "assets/icons/*.svg".into(),
            }),
            icons::FOLDER_SEARCH,
        ),
        row(
            "Fetch",
            Some(ToolCall::WebFetch {
                url: "https://solar-icons.vercel.app/icons".into(),
                prompt: None,
            }),
            icons::GLOBAL,
        ),
        row(
            "View image",
            Some(ToolCall::ViewImage {
                path: "screenshot.png".into(),
            }),
            icons::GALLERY,
        ),
        row(
            "Todos",
            Some(ToolCall::Todo { items: Vec::new() }),
            icons::CHECKLIST,
        ),
        row(
            "Wait for agents",
            unknown("Wait for agents"),
            icons::STOPWATCH,
        ),
        row("Hook", unknown("Hook: PostToolUse"), icons::WEBHOOK),
        row(
            "Computer use",
            Some(ToolCall::ComputerUse {
                action: "click".into(),
                app: Some("Finder".into()),
                task_id: None,
                thread_id: "thread".into(),
                turn_id: "turn".into(),
                status: ComputerUseStatus::ProviderCompleted,
                screenshot_observed: false,
            }),
            icons::CURSOR,
        ),
        row("Skill", unknown("Skill"), icons::MAGIC_WAND_3),
        row(
            "AskUserQuestion",
            unknown("AskUserQuestion"),
            icons::CHAT_ROUND_QUESTION_MARK,
        ),
        row(
            "ExitPlanMode",
            unknown("ExitPlanMode"),
            icons::CLIPBOARD_LIST,
        ),
        row("TaskStop", unknown("TaskStop"), icons::STOP_CIRCLE),
        row("NotebookEdit", unknown("NotebookEdit"), icons::NOTEBOOK),
        row("other tool", unknown("LSP"), icons::TOOLBOX),
    ]
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

fn part(value: serde_json::Value) -> MessagePart {
    serde_json::from_value(value).expect("fixture part")
}

fn entry(
    id: &str,
    role: MessageRole,
    parts: Vec<MessagePart>,
    device: &str,
    at: i64,
) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role,
        parts,
        created_at: at,
        device_id: device.into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    }
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
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
    for _ in 0..12 {
        settle(cx, 120).await;
        let next = render(window, cx)?;
        if next.as_raw() == last.as_raw() {
            return Ok(next);
        }
        last = next;
    }
    Ok(last)
}

fn park_pointer(window: Win, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                position: point(px(WIDTH - 4.0), px(4.0)),
                pressed_button: None,
                modifiers: Default::default(),
            }),
            cx,
        );
    })?;
    Ok(())
}

type Rgb = [f32; 3];

fn rgb(color: Hsla) -> Rgb {
    let rgba = color.to_rgb();
    [rgba.r, rgba.g, rgba.b]
}

fn pixel(image: &image::RgbaImage, x: u32, y: u32) -> Rgb {
    let p = image.get_pixel(x, y).0;
    [
        p[0] as f32 / 255.0,
        p[1] as f32 / 255.0,
        p[2] as f32 / 255.0,
    ]
}

fn sub(a: Rgb, b: Rgb) -> Rgb {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: Rgb, b: Rgb) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The most frequent colour in a rectangle: the surface behind the icons.
fn mode_colour(image: &image::RgbaImage, x0: u32, x1: u32, y0: u32, y1: u32) -> Rgb {
    let mut counts = std::collections::HashMap::<[u8; 3], usize>::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let p = image.get_pixel(x, y).0;
            *counts.entry([p[0], p[1], p[2]]).or_default() += 1;
        }
    }
    let top = counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(c, _)| c)
        .unwrap_or([0, 0, 0]);
    [
        top[0] as f32 / 255.0,
        top[1] as f32 / 255.0,
        top[2] as f32 / 255.0,
    ]
}

/// How one icon's ink relates to its tint: `coverage` is the strongest
/// pixel's position from background (0) to tint (1); `offLine` is the
/// share of inked pixels that leave the background→tint line, i.e. carry
/// a colour of their own.
fn measure_icon(
    image: &image::RgbaImage,
    (x0, x1, y0, y1): (u32, u32, u32, u32),
    bg: Rgb,
    tint: Rgb,
) -> serde_json::Value {
    let axis = sub(tint, bg);
    let len2 = dot(axis, axis).max(1e-6);
    let mut inked = 0usize;
    let mut off_line = 0usize;
    let mut coverage = 0f32;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = sub(pixel(image, x, y), bg);
            let a = dot(p, axis) / len2;
            if a < 0.35 {
                continue;
            }
            inked += 1;
            coverage = coverage.max(a);
            let along = [axis[0] * a, axis[1] * a, axis[2] * a];
            let residual = sub(p, along);
            if dot(residual, residual).sqrt() > 0.08 {
                off_line += 1;
            }
        }
    }
    json!({
        "inked": inked,
        "coverage": (coverage * 100.0).round() / 100.0,
        "offLine": if inked == 0 { 1.0 } else { ((off_line as f32 / inked as f32) * 1000.0).round() / 1000.0 },
    })
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: tool-icons-fixture ABSOLUTE_EVIDENCE_DIR"))?,
    );
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["readback.json", "tool-icons.pass", "failure.txt"] {
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
        script: vec![AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        }],
    }));
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "tool-icons-fixture", "tool-icons-fixture"),
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
    auth.dev_user_id = "tool-icons-fixture".into();
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
    let now = Utc::now();
    let at = now.timestamp_millis();
    let ui_chats = runtime.block_on(async {
        core.workspace.create_chat(
            CHAT,
            Some(PROJECT),
            Some(&device),
            Some(config()),
            Some(project.to_string_lossy().into_owned()),
        )?;
        core.workspace.rename_chat(CHAT, "Every tool kind")?;
        let mut chat = core.workspace.chat(CHAT)?.expect("seeded chat");
        chat.created_at = now - TimeDelta::minutes(3);
        chat.last_message_at = Some(now);
        chat.last_seen_at = Some(now + TimeDelta::seconds(1));
        core.workspace.import_chat_row(&chat)?;
        let doc = core.doc_host.open(CHAT)?;
        doc.doc().push_message(&entry(
            "u1",
            MessageRole::User,
            vec![MessagePart::Text {
                id: "u1-text".into(),
                text: "Run one of every tool so we can check the icons.".into(),
            }],
            &device,
            at - 30_000,
        ))?;
        let parts = rows()
            .into_iter()
            .enumerate()
            .map(|(ix, row)| match row.call {
                None => part(json!({
                    "kind": "reasoning", "id": format!("a1-{ix}"),
                    "text": "Load the tools, then call each one once.",
                })),
                Some(call) => part(json!({
                    "kind": "tool", "id": format!("a1-{ix}"),
                    "call": serde_json::to_value(call).expect("tool call json"),
                    "isError": row.is_error, "resolved": true, "output": "ok",
                })),
            })
            .collect();
        doc.doc().push_message(&entry(
            "a1",
            MessageRole::Assistant,
            parts,
            &device,
            at - 20_000,
        ))?;
        anyhow::Ok(vec![chat])
    })?;
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
                state.select_chat(Some(CHAT.into()), cx);
                state
            });
            cx.spawn(async move |cx| {
                match run(cx, state, boot, &output).await {
                    Ok(record) => {
                        let written = std::fs::write(
                            output.join("readback.json"),
                            serde_json::to_vec_pretty(&record).unwrap_or_default(),
                        )
                        .and_then(|_| {
                            std::fs::write(
                                output.join("tool-icons.pass"),
                                "PASS: headless production Shell; one open chain with a thought and every tool kind, dark and light at 16 px; each row names its chosen glyph, every glyph paints ink, and every icon's ink sits on its row's tint (muted, or danger for the failed row)\n",
                            )
                        });
                        if let Err(error) = written {
                            *failure_for_ui.lock().unwrap() = Some(error.to_string());
                            let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                            std::process::exit(1);
                        }
                    }
                    Err(error) => {
                        eprintln!("Tool icons fixture failed: {error:#}");
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

async fn run(
    cx: &mut AsyncApp,
    state: gpui::Entity<state::AppState>,
    boot: EngineBootConfig,
    output: &Path,
) -> anyhow::Result<serde_json::Value> {
    let expected = rows();
    let failed_ix = expected
        .iter()
        .position(|row| row.is_error)
        .expect("one failed row");
    let mut record = serde_json::Map::new();
    let mut figures = String::new();
    for (theme_name, mode) in [
        ("dark", appearance::AppearanceMode::Dark),
        ("light", appearance::AppearanceMode::Light),
    ] {
        cx.update(|cx| appearance::set_mode(mode, cx));
        let background = cx.update(|cx| theme::Theme::of(cx).window_background_appearance());
        let shell_state = state.clone();
        let shell_boot = boot.clone();
        let window: Win = cx.open_window(
            WindowOptions {
                window_background: background,
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(20.), px(20.)),
                    size(px(WIDTH), px(HEIGHT)),
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
        cx.update(|cx| state.update(cx, |_, cx| cx.notify()));
        settle(cx, 900).await;
        let (muted, danger) = cx.update(|cx| {
            let theme = theme::Theme::of(cx);
            (rgb(theme.content_muted), rgb(theme.danger))
        });

        // Wait for the seeded turn to project its tool group.
        let mut group = serde_json::Value::Null;
        for _ in 0..60 {
            let rows = window.update(cx, |shell, _, cx| shell.fixture_transcript_rows(CHAT, cx))?;
            if let Some(found) = rows
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row.get("glyphs").is_some()))
            {
                group = found.clone();
                break;
            }
            settle(cx, 100).await;
        }
        anyhow::ensure!(
            !group.is_null(),
            "{theme_name}: the transcript never projected a tool group"
        );

        window.update(cx, |shell, _, cx| shell.fixture_open_tool_groups(cx))?;
        window.update(cx, |_, window, _| window.blur())?;
        park_pointer(window, cx)?;
        let image = stable(window, cx).await?;
        let file = format!("chain-{theme_name}.png");
        image.save(output.join(&file))?;
        let scale = image.width() as f32 / WIDTH;
        let mut failures: Vec<String> = Vec::new();

        // 1: the readback names each row's chosen glyph.
        let glyphs: Vec<&str> = group["glyphs"]
            .as_array()
            .map(|g| g.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        if glyphs.len() != expected.len() {
            failures.push(format!(
                "the chain has {} rows, expected {}: {glyphs:?}",
                glyphs.len(),
                expected.len()
            ));
        }
        for (row, glyph) in expected.iter().zip(&glyphs) {
            if row.glyph != *glyph {
                failures.push(format!(
                    "{} shows {glyph}, expected {}",
                    row.label, row.glyph
                ));
            }
        }
        if group["groupGlyph"] != icons::LAYERS_MINIMALISTIC {
            failures.push(format!(
                "the mixed group's summary shows {}, expected {}",
                group["groupGlyph"],
                icons::LAYERS_MINIMALISTIC
            ));
        }

        // 2 & 3: find the icon column from the failed row's red icon, then
        // walk the column one row pitch at a time. Only ink that is the
        // danger colour blended over the transcript counts: the brand-orange
        // Claude marks and file badges sit off that line. The leftmost such
        // ink is the icon; its "Failed" label sits far right.
        let surface = mode_colour(
            &image,
            (image.width() as f32 * 0.3) as u32,
            (image.width() as f32 * 0.9) as u32,
            0,
            image.height(),
        );
        let danger_axis = sub(danger, surface);
        let danger_len2 = dot(danger_axis, danger_axis).max(1e-6);
        let mut red = Vec::new();
        for y in 0..image.height() {
            for x in 0..image.width() {
                let d = sub(pixel(&image, x, y), surface);
                let a = dot(d, danger_axis) / danger_len2;
                if a < 0.7 {
                    continue;
                }
                let r = sub(
                    d,
                    [danger_axis[0] * a, danger_axis[1] * a, danger_axis[2] * a],
                );
                if dot(r, r).sqrt() < 0.04 {
                    red.push((x, y));
                }
            }
        }
        let left = red.iter().map(|p| p.0).min();
        let Some(left) = left else {
            anyhow::bail!("{theme_name}: the failed row's icon painted no danger ink; see {file}");
        };
        let icon_span = (24.0 * scale) as u32;
        let icon: Vec<_> = red.iter().filter(|p| p.0 <= left + icon_span).collect();
        let x0 = icon.iter().map(|p| p.0).min().unwrap().saturating_sub(2);
        let x1 = (icon.iter().map(|p| p.0).max().unwrap() + 3).min(image.width());
        let red_top = icon.iter().map(|p| p.1).min().unwrap();
        let red_bottom = icon.iter().map(|p| p.1).max().unwrap();
        let bg = mode_colour(&image, x0, x1, 0, image.height());

        // Ink runs down the column (rail hairlines stay under the threshold).
        let axis = sub(muted, bg);
        let len2 = dot(axis, axis).max(1e-6);
        let inked_line = |y: u32| {
            (x0..x1).any(|x| {
                let d = sub(pixel(&image, x, y), bg);
                dot(d, d).sqrt() > 0.35 * len2.sqrt()
            })
        };
        let mut blobs: Vec<(u32, u32)> = Vec::new();
        let mut start = None;
        for y in 0..image.height() {
            match (inked_line(y), start) {
                (true, None) => start = Some(y),
                (false, Some(s)) => {
                    blobs.push((s, y));
                    start = None;
                }
                _ => {}
            }
        }
        // Glyphs built from separate bars (the checklist) leave short gaps;
        // rows sit far further apart than that.
        let gap = (3.0 * scale) as u32;
        let mut merged: Vec<(u32, u32)> = Vec::new();
        for blob in blobs {
            match merged.last_mut() {
                Some(last) if blob.0 - last.1 <= gap => last.1 = blob.1,
                _ => merged.push(blob),
            }
        }
        let mut blobs = merged;
        let min_height = (6.0 * scale) as u32;
        blobs.retain(|(s, e)| e - s >= min_height);
        let failed_center = (red_top + red_bottom) as f32 / 2.0;
        let centre = |b: &(u32, u32)| (b.0 + b.1) as f32 / 2.0;
        let fb = blobs
            .iter()
            .position(|b| (centre(b) - failed_center).abs() < 4.0 * scale)
            .ok_or_else(|| anyhow::anyhow!("{theme_name}: no column blob at the failed icon"))?;
        let mut steps: Vec<f32> = blobs[fb.saturating_sub(4)..(fb + 5).min(blobs.len())]
            .windows(2)
            .map(|w| centre(&w[1]) - centre(&w[0]))
            .collect();
        steps.sort_by(f32::total_cmp);
        let pitch = steps.get(steps.len() / 2).copied().unwrap_or(0.0);
        anyhow::ensure!(
            pitch > 8.0 * scale,
            "{theme_name}: no row pitch in the icon column"
        );

        let mut measured = Vec::new();
        for (ix, row) in expected.iter().enumerate() {
            let want = failed_center + (ix as f32 - failed_ix as f32) * pitch;
            let tint = if row.is_error { danger } else { muted };
            let blob = blobs
                .iter()
                .find(|b| (centre(b) - want).abs() < pitch * 0.35);
            let Some(&(y0, y1)) = blob else {
                failures.push(format!("{}: no ink in its icon slot", row.label));
                measured.push(json!({ "row": row.label, "glyph": row.glyph, "ink": null }));
                continue;
            };
            let m = measure_icon(&image, (x0, x1, y0, y1), bg, tint);
            if m["inked"].as_u64().unwrap_or(0) < 6 || m["coverage"].as_f64().unwrap_or(0.0) < 0.6 {
                failures.push(format!("{}: its icon barely paints ({m})", row.label));
            }
            if m["offLine"].as_f64().unwrap_or(1.0) > 0.05 {
                failures.push(format!(
                    "{}: its icon carries a colour of its own ({m})",
                    row.label
                ));
            }
            measured.push(json!({ "row": row.label, "glyph": row.glyph, "ink": m }));
        }

        // Control: the failed icon measured against the muted tint must read
        // as off-line, or the colour check could not catch a stray tint.
        let failed_want = failed_center;
        let control = blobs
            .iter()
            .find(|b| (centre(b) - failed_want).abs() < pitch * 0.35)
            .map(|&(y0, y1)| measure_icon(&image, (x0, x1, y0, y1), bg, muted));
        if control
            .as_ref()
            .is_none_or(|m| m["offLine"].as_f64().unwrap_or(0.0) < 0.5)
        {
            failures.push(format!(
                "control: the red icon measured against the muted tint did not read as off-tint ({control:?})"
            ));
        }

        let readback = json!({
            "control": control,
            "groupGlyph": group["groupGlyph"],
            "glyphs": glyphs,
            "column": { "x": [x0, x1], "pitch": pitch, "scale": scale },
            "background": bg,
            "tints": { "muted": muted, "danger": danger },
            "icons": measured,
            "failures": failures,
        });
        figures.push_str(&format!(
            "<figure><img src=\"{file}\"><figcaption>{theme_name} · 16 px</figcaption></figure>"
        ));
        record.insert(theme_name.into(), readback.clone());
        let _ = window.update(cx, |_, window, _| window.remove_window());
        settle(cx, 100).await;
        if !failures.is_empty() {
            std::fs::write(
                output.join("readback.json"),
                serde_json::to_vec_pretty(&record).unwrap_or_default(),
            )?;
            anyhow::bail!("{theme_name}: {}", failures.join("; "));
        }
    }
    std::fs::write(
        output.join("index.html"),
        format!(
            "<!doctype html><meta charset=utf-8><title>Tool icons</title>\
             <style>body{{font:13px system-ui;background:#888;margin:16px}}figure{{display:inline-block;\
             margin:0 12px 16px 0;vertical-align:top}}img{{width:550px;display:block}}</style>{figures}"
        ),
    )?;
    Ok(serde_json::Value::Object(record))
}
