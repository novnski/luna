//! Headless GPUI E2E for three pieces of chat chrome: the new-thread card a
//! parent shows when it creates a child chat, the timeline's disclosure
//! headers ("Thought process", "Called 2 tools"), the composer's focus
//! treatment, and the composer's model picker.
//!
//! Drives the production `Shell` in hidden windows against an in-process
//! `EngineCore` with a Mock harness. The parent's transcript is seeded with
//! a reasoning part, two tool calls (ToolSearch and an MCP call), the
//! engine's `child-created:` row and a reply; the child is a live Claude
//! Code chat. Every combination of Interface size (12, 16, 20 px) and theme
//! (dark, light) gets its own window, at 1200 px wide and at 900 px.
//!
//! Failure cases this run must catch (written before the redesign):
//! 1. The new-thread card is missing, or its identity tile, title and
//!    actions don't share one vertical centre line; its actions are not the
//!    shared kit controls; at 20 px or 900 px the card clips, overlaps or
//!    squeezes its title to nothing.
//! 2. A disclosure header has no leading kind glyph and no resting plate,
//!    so it doesn't read as something to click; its chevron doesn't turn
//!    when open; its height doesn't follow the Interface size; the open
//!    rail doesn't descend from the header's glyph column.
//! 3. Focusing the composer paints an accent ring or accent border on the
//!    pill (`ring` below counts accent pixels on the pill's edge).
//! 4. With the composer focused and empty, the send arrow is not accent,
//!    or the disc is not the recessed grey; unfocused and empty, the arrow
//!    is accent; with a draft, the disc is not solid (`send` counts accent
//!    pixels in the send disc's corner of the pill).
//! 5. The model picker shows a model's tagline under or beside its name, or
//!    misses the "New" badge on a model this device's catalog first listed
//!    now (the engine cache is seeded with an older catalog without it, so
//!    the real refresh stamps it), or badges a model from that older
//!    catalog.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example chat-chrome-fixture
//! Run:   target/debug/examples/chat-chrome-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes one PNG per state and combination, readback.json, index.html and
//! chat-chrome.pass; a failure writes failure.txt/failure.png and exits
//! nonzero.
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
    AgentEvent, ChatConfig, ChildThread, DoneStatus, HarnessId, PermissionMode, ReasoningLevel,
    SandboxLevel, Session, SessionStatus,
};
use luna_ui::*;
use serde_json::json;

const PROJECT: &str = "chrome-project";
const PARENT: &str = "c4c4c4c4-0000-4000-8000-000000000001";
const CHILD: &str = "c4c4c4c4-0000-4000-8000-000000000002";
const CHILD_PROMPT: &str = "Find a cute dog image. Search the web for one, pick a genuinely cute, \
     family-friendly photo and reply with the image and its source link.";
const DRAFT: &str = "Thanks — open it in a split when it's done.";
const HEIGHT: f32 = 860.0;
const WIDTHS: [f32; 2] = [1200.0, 900.0];
const SIZES: [f32; 3] = [12.0, 16.0, 20.0];

type Win = WindowHandle<shell::Shell>;

fn config(harness: HarnessId, model: &str, reasoning: Option<ReasoningLevel>) -> ChatConfig {
    ChatConfig {
        harness,
        provider_account_id: None,
        model: Some(model.into()),
        reasoning,
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

fn text(id: &str, body: &str) -> MessagePart {
    MessagePart::Text {
        id: id.into(),
        text: body.into(),
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

fn park_pointer(window: Win, cx: &mut AsyncApp, width: f32) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                position: point(px(width - 4.0), px(4.0)),
                pressed_button: None,
                modifiers: Default::default(),
            }),
            cx,
        );
    })?;
    Ok(())
}

fn rgb(color: Hsla) -> [f32; 3] {
    let rgba = color.to_rgb();
    [rgba.r, rgba.g, rgba.b]
}

fn hue_of([r, g, b]: [f32; 3]) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-4 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-4);
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

/// Pixels in `rect` (logical px) whose hue is the accent's.
fn accent_pixels(image: &image::RgbaImage, scale: f32, rect: [f32; 4], accent: Hsla) -> usize {
    let (accent_hue, _, _) = hue_of(rgb(accent));
    let [x, y, w, h] = rect;
    let (x0, y0) = ((x * scale).max(0.0) as u32, (y * scale).max(0.0) as u32);
    let x1 = (((x + w) * scale) as u32).min(image.width());
    let y1 = (((y + h) * scale) as u32).min(image.height());
    let mut count = 0;
    for py in y0..y1 {
        for px_ in x0..x1 {
            let p = image.get_pixel(px_, py).0;
            let (hue, sat, light) = hue_of([
                p[0] as f32 / 255.0,
                p[1] as f32 / 255.0,
                p[2] as f32 / 255.0,
            ]);
            let distance = (hue - accent_hue).abs();
            let distance = distance.min(360.0 - distance);
            if sat > 0.2 && (0.15..0.92).contains(&light) && distance < 20.0 {
                count += 1;
            }
        }
    }
    count
}

struct Shot {
    file: String,
    size: f32,
    theme: &'static str,
    width: f32,
    state: &'static str,
}

fn write_index(output: &Path, shots: &[Shot]) -> anyhow::Result<()> {
    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><title>Chat chrome</title>\
         <style>body{font:13px system-ui;background:#888;margin:16px}figure{display:inline-block;\
         margin:0 12px 16px 0;vertical-align:top}img{width:600px;display:block}</style>",
    );
    for shot in shots {
        html.push_str(&format!(
            "<figure><img src=\"{}\"><figcaption>{} · {} · {}px · {}w</figcaption></figure>",
            shot.file, shot.state, shot.theme, shot.size, shot.width
        ));
    }
    std::fs::write(output.join("index.html"), html)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: chat-chrome-fixture ABSOLUTE_EVIDENCE_DIR"))?,
    );
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in [
        "readback.json",
        "chat-chrome.pass",
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
        script: vec![AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        }],
    }));
    // An older Mock catalog from a previous cache version: the engine
    // re-fetches it, so "Fable 5", absent here, is stamped as first seen now
    // while "Mock 1" keeps this baseline.
    {
        use sha2::{Digest, Sha256};
        let dir = engine_data.join("model-cache");
        std::fs::create_dir_all(&dir)?;
        let name = format!("{:x}.json", Sha256::digest(b"Mock:device"));
        std::fs::write(
            dir.join(name),
            serde_json::to_vec(&json!({
                "version": 1,
                "checkedAtMs": 0,
                "models": [{"id": "mock-1", "label": "Mock 1"}],
            }))?,
        )?;
    }
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "chrome-fixture", "chrome-fixture"),
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
    auth.dev_user_id = "chrome-fixture".into();
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
        let mut chats = Vec::new();
        for (id, title, chat_config) in [
            (
                PARENT,
                "Side chat for a dog photo",
                config(HarnessId::Mock, "mock-fixture", None),
            ),
            (
                CHILD,
                "Find a cute dog image",
                config(
                    HarnessId::ClaudeCode,
                    "claude-fable-5-1",
                    Some(ReasoningLevel::High),
                ),
            ),
        ] {
            core.workspace.create_chat(
                id,
                Some(PROJECT),
                Some(&device),
                Some(chat_config),
                Some(project.to_string_lossy().into_owned()),
            )?;
            core.workspace.rename_chat(id, title)?;
            let mut chat = core.workspace.chat(id)?.expect("seeded chat");
            chat.created_at = now - TimeDelta::minutes(3);
            chat.last_message_at = Some(now);
            chat.last_seen_at = Some(now + TimeDelta::seconds(1));
            core.workspace.import_chat_row(&chat)?;
            if id == CHILD {
                chat.child_thread = Some(ChildThread {
                    work_origin: None,
                    parent_task_id: PARENT.into(),
                    creation_command_id: "fixture-create-child".into(),
                    permission_ceiling: Default::default(),
                    sandbox_ceiling: None,
                    initial_message_id: format!("{CHILD}-prompt"),
                    initial_prompt: CHILD_PROMPT.into(),
                    report_results: false,
                    continue_parent: false,
                    outcomes: Vec::new(),
                    outcome_floor_ms: Some(at),
                    provisioning: None,
                });
            }
            chats.push(chat);
        }
        let doc = core.doc_host.open(PARENT)?;
        doc.doc().push_message(&entry(
            "u1",
            MessageRole::User,
            vec![text(
                "u1-text",
                "Create a side chat that finds a cute dog image for me.",
            )],
            &device,
            at - 30_000,
        ))?;
        doc.doc().push_message(&entry(
            "a1",
            MessageRole::Assistant,
            vec![
                part(json!({
                    "kind": "reasoning", "id": "a1-think",
                    "text": "The user wants a separate side chat that looks for a dog photo. \
                             I should load the LUNA thread tool and create the chat with a clear prompt.",
                })),
                text(
                    "a1-text",
                    "I'll load the LUNA thread tool and create a fresh side chat with that request.",
                ),
                part(json!({
                    "kind": "tool", "id": "a1-search",
                    "call": {"kind": "unknown", "name": "ToolSearch",
                             "input": {"query": "select:mcp__luna__create_thread"}},
                    "resolved": true, "output": "Loaded 1 tool",
                })),
                part(json!({
                    "kind": "tool", "id": "a1-create",
                    "call": {"kind": "mcp", "server": "luna", "tool": "create_thread",
                             "input": {"title": "Find a cute dog image", "prompt": CHILD_PROMPT}},
                    "resolved": true, "output": "Created thread",
                })),
            ],
            &device,
            at - 20_000,
        ))?;
        doc.doc().push_message(&entry(
            &format!("child-created:{CHILD}"),
            MessageRole::System,
            vec![text("created", "Created child chat")],
            &device,
            at - 18_000,
        ))?;
        doc.doc().push_message(&entry(
            "a2",
            MessageRole::Assistant,
            vec![text(
                "a2-text",
                "The side chat is running. Open it from the card above when you want to follow along.",
            )],
            &device,
            at - 17_000,
        ))?;
        anyhow::Ok(chats)
    })?;
    let spaces = core.workspace.read_spaces()?;
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59996,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let sessions = vec![Session {
        chat_id: CHILD.into(),
        device_id: device.clone(),
        status: SessionStatus::Working,
        started_at: Some(now - TimeDelta::seconds(17)),
        updated_at: now,
        last_completed_turn: None,
        recovery: None,
    }];
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
                state.sessions = sessions;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.auto_selected = true;
                state.connection = state::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
                state.fixture_attachment_engine(handle);
                state.select_chat(Some(PARENT.into()), cx);
                state
            });
            cx.spawn(async move |cx| {
                let result = run(cx, state, boot, &output).await;
                match result {
                    Ok(record) => {
                        let written = std::fs::write(
                            output.join("readback.json"),
                            serde_json::to_vec_pretty(&record).unwrap_or_default(),
                        )
                        .and_then(|_| {
                            std::fs::write(
                                output.join("chat-chrome.pass"),
                                "PASS: headless production Shell; the new-thread card, the Thought process and Called 2 tools disclosures (closed and open) and the composer (unfocused, focused empty, focused with a draft) captured at 12/16/20 px, dark and light, 1200 and 900 px wide; no accent ring on the focused pill; the send arrow is accent only while focused and empty; a draft makes the disc solid\n",
                            )
                        });
                        if let Err(error) = written {
                            *failure_for_ui.lock().unwrap() = Some(error.to_string());
                            let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                            std::process::exit(1);
                        }
                    }
                    Err(error) => {
                        eprintln!("Chat chrome fixture failed: {error:#}");
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
    let mut shots = Vec::new();
    let mut record = serde_json::Map::new();
    for &width in &WIDTHS {
        for &pixels in &SIZES {
            for (theme_name, mode) in [
                ("dark", appearance::AppearanceMode::Dark),
                ("light", appearance::AppearanceMode::Light),
            ] {
                // The narrow window only checks the card and headers at the
                // default and largest sizes.
                if width < 1000.0 && pixels < 16.0 {
                    continue;
                }
                let name = format!("{theme_name}-{pixels}px-{width}w");
                cx.update(|cx| appearance::set_mode(mode, cx));
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
                let (accent, scale_hint) = cx.update(|cx| (theme::Theme::of(cx).accent, ()));
                let _ = scale_hint;

                // The card needs its live child row; wait for the transcript
                // to project the seeded turn.
                let mut rows = serde_json::Value::Null;
                for _ in 0..60 {
                    rows = window
                        .update(cx, |shell, _, cx| shell.fixture_transcript_rows(PARENT, cx))?;
                    if rows.as_array().is_some_and(|rows| {
                        rows.iter()
                            .any(|row| row["entryId"] == format!("child-created:{CHILD}"))
                    }) {
                        break;
                    }
                    settle(cx, 100).await;
                }
                anyhow::ensure!(
                    rows.as_array().is_some_and(|rows| rows
                        .iter()
                        .any(|row| row["entryId"] == format!("child-created:{CHILD}"))),
                    "{name}: the parent transcript never projected the child-created row: {rows}"
                );

                let surface = |window: Win, cx: &mut AsyncApp| -> anyhow::Result<[f32; 4]> {
                    window
                        .update(cx, |shell, _, cx| {
                            shell.fixture_composer_surface(PARENT, cx)
                        })?
                        .ok_or_else(|| {
                            anyhow::anyhow!("the parent's composer surface is not painted")
                        })
                };
                let capture = |state_name: &'static str,
                               image: &image::RgbaImage,
                               shots: &mut Vec<Shot>|
                 -> anyhow::Result<()> {
                    let file = format!("{state_name}-{name}.png");
                    image.save(output.join(&file))?;
                    shots.push(Shot {
                        file,
                        size: pixels,
                        theme: theme_name,
                        width,
                        state: state_name,
                    });
                    Ok(())
                };

                // Drafts persist per chat across windows: start each window
                // from an empty composer.
                let composer = window
                    .update(cx, |shell, _, cx| {
                        shell.fixture_workbench_composer(PARENT, cx)
                    })?
                    .ok_or_else(|| anyhow::anyhow!("{name}: no composer for the parent"))?;
                cx.update(|cx| {
                    composer.update(cx, |composer, cx| {
                        composer.fixture_stage_chip_preview("", cx)
                    })
                });
                settle(cx, 200).await;

                // 1. Closed disclosures, composer not focused.
                window.update(cx, |_, window, _| window.blur())?;
                park_pointer(window, cx, width)?;
                let closed = stable(window, cx).await?;
                let scale = closed.width() as f32 / width;
                capture("closed", &closed, &mut shots)?;
                let pill = surface(window, cx)?;
                // The send disc sits in the pill's bottom-right corner.
                let corner = |pill: [f32; 4]| {
                    let side = 56.0 * pixels / 16.0;
                    // Inset off the pill's own edge, so a focus ring there
                    // never counts as the arrow.
                    [
                        pill[0] + pill[2] - side,
                        pill[1] + pill[3] - side,
                        side - 5.0,
                        side - 5.0,
                    ]
                };
                let edge = |pill: [f32; 4]| [pill[0] + 24.0, pill[1] - 2.0, pill[2] - 48.0, 5.0];
                let rest_send = accent_pixels(&closed, scale, corner(pill), accent);
                let rest_ring = accent_pixels(&closed, scale, edge(pill), accent);

                // 2. Every disclosure open.
                window.update(cx, |shell, _, cx| shell.fixture_open_tool_groups(cx))?;
                park_pointer(window, cx, width)?;
                let open = stable(window, cx).await?;
                capture("open", &open, &mut shots)?;

                // 3. Composer focused, still empty.
                let focused = window.update(cx, |shell, window, cx| {
                    shell.fixture_workbench_focus_input(PARENT, window, cx)
                })?;
                anyhow::ensure!(
                    focused,
                    "{name}: the parent's composer could not take focus"
                );
                park_pointer(window, cx, width)?;
                let focus_empty = stable(window, cx).await?;
                capture("focused", &focus_empty, &mut shots)?;
                let pill = surface(window, cx)?;
                let focus_send = accent_pixels(&focus_empty, scale, corner(pill), accent);
                let focus_ring = accent_pixels(&focus_empty, scale, edge(pill), accent);

                // 4. Focused with a draft.
                cx.update(|cx| {
                    composer.update(cx, |composer, cx| {
                        composer.fixture_stage_chip_preview(DRAFT, cx)
                    })
                });
                park_pointer(window, cx, width)?;
                let drafted = stable(window, cx).await?;
                capture("draft", &drafted, &mut shots)?;
                let pill = surface(window, cx)?;
                let draft_send = accent_pixels(&drafted, scale, corner(pill), accent);
                cx.update(|cx| {
                    composer.update(cx, |composer, cx| {
                        composer.fixture_stage_chip_preview("", cx)
                    })
                });

                // 5. The model picker, once per theme at the default size.
                let mut picker = serde_json::Value::Null;
                if pixels == 16.0 && width > 1000.0 {
                    let pickers = cx.update(|cx| composer.read(cx).pickers().clone());
                    window.update(cx, |_, window, cx| {
                        pickers.update(cx, |pickers, cx| pickers.open_model_menu(window, cx))
                    })?;
                    for _ in 0..60 {
                        picker = cx.update(|cx| pickers.read(cx).fixture_model_rows(cx));
                        if picker.as_array().is_some_and(|rows| rows.len() >= 2) {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    park_pointer(window, cx, width)?;
                    let open_picker = stable(window, cx).await?;
                    capture("picker", &open_picker, &mut shots)?;
                    cx.update(|cx| pickers.update(cx, |pickers, cx| pickers.fixture_close(cx)));
                }

                let readback = json!({
                    "picker": picker,
                    "pill": pill,
                    "accentPixels": {
                        "restSend": rest_send, "restRing": rest_ring,
                        "focusSend": focus_send, "focusRing": focus_ring,
                        "draftSend": draft_send,
                    },
                });
                let mut failures = Vec::new();
                // 3: no accent on the pill's edge when focused.
                if focus_ring > rest_ring + 4 {
                    failures.push("focusing the composer paints an accent ring");
                }
                // 4: the arrow is accent only while focused and empty, and a
                // draft brings back the solid neutral disc.
                if focus_send < 12 {
                    failures.push("the focused empty send arrow is not accent");
                }
                if rest_send >= 4 {
                    failures.push("the unfocused send arrow is accent");
                }
                if draft_send >= 4 {
                    failures.push("a draft leaves the send disc accent instead of solid");
                }
                // 5: name-only rows; the newly listed model is New, the
                // baseline model isn't.
                if let Some(rows) = picker.as_array() {
                    let row = |id: &str| rows.iter().find(|row| row["id"] == id);
                    if row("mock-fable-5").is_none_or(|row| row["new"] != true) {
                        failures.push("the newly listed model has no New badge");
                    }
                    if row("mock-1").is_none_or(|row| row["new"] != false) {
                        failures.push("a baseline model wears the New badge");
                    }
                    if rows.iter().any(|row| !row["attribution"].is_null()) {
                        failures.push("a model row shows a description");
                    }
                }
                if !failures.is_empty() {
                    // CHAT_CHROME_RECORD_ONLY records a baseline (the state
                    // before a change) instead of failing on it.
                    if std::env::var_os("CHAT_CHROME_RECORD_ONLY").is_none() {
                        anyhow::bail!("{name}: {}: {readback}", failures.join("; "));
                    }
                    record.insert(format!("{name}-failures"), json!(failures));
                }
                record.insert(name.clone(), readback);
                let _ = window.update(cx, |_, window, _| window.remove_window());
                settle(cx, 100).await;
            }
        }
    }
    write_index(output, &shots)?;
    Ok(serde_json::Value::Object(record))
}
