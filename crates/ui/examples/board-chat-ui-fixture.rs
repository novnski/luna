//! Headless GPUI E2E for ticket identity on chat surfaces (Board slice A10;
//! contract §6, §7.3, §9; chat-model §1–§5 and §8; automatic-review §2).
//!
//! Drives the production `Shell` in a hidden window with real pointer and
//! keyboard input (GPUI hit testing on painted bounds, right-click menus,
//! dialog buttons, Escape, the palette's own search field) against a REAL
//! in-process `EngineCore` (temp data dir, Mock harness) attached through
//! the production engine boundary. Board state is the engine's: badges come
//! from `BoardChatBadges`, roles from `BoardChatIdentity`, navigation from
//! `BoardNavigation`; the fixture never injects UI state.
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. A working chat's header shows no ticket badge, shows it by colour only
//!    (no glyph or "Ticket" text), or the badge does not open that exact
//!    ticket; the workflow-status control is missing, merged into the badge,
//!    or offers Done (engine-owned) as a choice.
//! 2. The main sidebar row of a standalone working chat lacks the compact
//!    ticket key, or the key has no accessible label/tooltip naming the
//!    ticket; an ordinary row grows one.
//! 3. A working side chat's tab lacks the same compact badge, or gaining it
//!    moves the tab out of its main chat's workbench, changes its lineage
//!    parent or lists it in the main sidebar.
//! 4. A reviewer side chat shows the working `Ticket` badge (or none)
//!    instead of a distinct `Reviewer · KEY` badge with its own glyph.
//! 5. A source discussion (a ticket created from it) shows the working
//!    badge instead of a "Related ticket" receipt with Open ticket.
//! 6. A main chat's creation chooser lacks Side chat Fresh / With context or
//!    still names "New thread or side chat"; a side chat's creation offers
//!    With context, Fork, a nested side chat, or anything beyond New side
//!    chat (a fresh sibling under the same main) and Hand off to a new main
//!    chat.
//! 7. The chat menu of a side chat still offers a context fork ("from
//!    here", "with context") without saying why it is unavailable.
//! 8. Convert this chat to a ticket creates another chat, changes the chat
//!    ID, sends a prompt, or does not propose the chat's current title; the
//!    badge does not appear from the engine's answer.
//! 9. Create ticket from this chat adds a chat (UI or engine count), gives
//!    the source the working badge, or shows no receipt with Open ticket.
//! 10. Go to chat after the side chat's tab was closed (and another chat
//!     selected) relaunches or resends it, promotes or reparents it, opens
//!     it outside its main chat's workbench, or selects the wrong tab.
//! 11. Any menu, card or strip offers Move here / Reattach / Change parent
//!     for a ticket chat.
//! 12. Palette ticket entries resolve by title instead of the stable ticket
//!     and chat IDs.
//!
//! Every engine step that a sibling slice owns (conversion, side chat
//! creation, review start, navigation) is the real engine operation; on a
//! tree where one is not implemented yet the run fails with that engine's
//! own refusal recorded in the manifest.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example board-chat-ui-fixture
//! Run:   target/debug/examples/board-chat-ui-fixture [ABSOLUTE_EVIDENCE_DIR]
//! Writes f00..fNN PNG captures, manifest.json (steps, expected/observed,
//! SHA-256 of every capture and readback) and board-chat-ui.pass under
//! `target/qa/board/chat-ui/` by default; a failure writes failure.txt and
//! exits nonzero.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, MouseButton, PlatformInput, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::mock::MockHarness;
use luna_proto::{AgentEvent, ChatConfig, DoneStatus, HarnessId, PermissionMode, SandboxLevel};
use luna_rpc::methods;
use luna_ui::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Win = WindowHandle<shell::Shell>;

const PROJECT: &str = "board-chat-ui-project";
/// The main chat that becomes a ticket and hosts the side chats.
const MAIN: &str = "b0a10000-0000-4000-8000-000000000001";
/// The discussion a ticket is created from (its source, never its worker).
const SOURCE: &str = "b0a10000-0000-4000-8000-000000000002";
/// Somewhere else to be when Go to chat runs.
const OTHER: &str = "b0a10000-0000-4000-8000-000000000003";
const MAIN_TITLE: &str = "Plan the reconnect rollout";
const SOURCE_TITLE: &str = "Discuss flaky sync tests";
const OTHER_TITLE: &str = "Polish onboarding copy";
const WIDTH: f32 = 1440.0;
const HEIGHT: f32 = 900.0;
/// Words no board surface may offer for a ticket chat (chat-model §8).
const REATTACH_WORDS: [&str; 6] = [
    "move here",
    "reattach",
    "re-attach",
    "change parent",
    "reparent",
    "attach to",
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
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        draw(window, cx)?;
        settle(cx, 16).await;
    }
    draw(window, cx)
}

fn input(window: Win, cx: &mut AsyncApp, event: PlatformInput) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(event, cx);
    })?;
    Ok(())
}

fn hover_at(window: Win, cx: &mut AsyncApp, x: f32, y: f32) -> anyhow::Result<()> {
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

/// A real press and release at a painted point (GPUI hit testing).
async fn click_at(
    window: Win,
    cx: &mut AsyncApp,
    (x, y): (f32, f32),
    button: MouseButton,
) -> anyhow::Result<()> {
    hover_at(window, cx, x, y)?;
    frames(window, cx, 60).await?;
    let position = point(px(x), px(y));
    input(
        window,
        cx,
        PlatformInput::MouseDown(gpui::MouseDownEvent {
            button,
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
            button,
            position,
            modifiers: Default::default(),
            click_count: 1,
        }),
    )?;
    frames(window, cx, 120).await?;
    // Park the pointer away from tooltips.
    hover_at(window, cx, WIDTH - 30.0, HEIGHT - 30.0)?;
    frames(window, cx, 200).await
}

/// One real key press through the window's dispatch (bindings, then the
/// focused element).
async fn press(window: Win, cx: &mut AsyncApp, key: &str) -> anyhow::Result<()> {
    let keystroke = gpui::Keystroke::parse(key)?;
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_keystroke(keystroke, cx);
    })?;
    frames(window, cx, 120).await
}

async fn type_text(window: Win, cx: &mut AsyncApp, text: &str) -> anyhow::Result<()> {
    for c in text.chars() {
        let key = if c == ' ' {
            "space".to_string()
        } else {
            c.to_string()
        };
        let keystroke = gpui::Keystroke::parse(&key)?;
        gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
            window.dispatch_keystroke(keystroke, cx);
        })?;
        draw(window, cx)?;
        settle(cx, 30).await;
    }
    frames(window, cx, 200).await
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Everything the run reads back, in one frame.
fn readback(window: Win, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    draw(window, cx)?;
    window.update(cx, |shell, _, cx| {
        json!({
            "board": shell.fixture_board_chat_readback(cx),
            "workbench": shell.fixture_workbench_readback(cx),
            "sidebar": shell.fixture_sidebar_sections_readback(cx),
            "chooser": shell.fixture_chooser_readback(cx),
        })
    })
}

fn bounds_of(rb: &Value, scope: &str, label: &str) -> Option<(f32, f32)> {
    let found = &rb[scope]["bounds"][label];
    let num = |key: &str| found[key].as_f64().map(|value| value as f32);
    Some((num("x")? + num("w")? / 2.0, num("y")? + num("h")? / 2.0))
}

fn board_bounds(rb: &Value, label: &str) -> Option<(f32, f32)> {
    bounds_of(rb, "board", label)
}

fn sidebar_row(rb: &Value, chat: &str) -> Option<(f32, f32)> {
    bounds_of(rb, "sidebar", &format!("title:{chat}"))
}

fn tab_center(rb: &Value, chat: &str) -> Option<(f32, f32)> {
    rb["workbench"]["tabBounds"]
        .as_array()?
        .iter()
        .flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default())
        .find(|tab| tab["chatId"] == chat)
        .and_then(|tab| {
            let b = &tab["visible"];
            Some((
                b["x"].as_f64()? as f32 + b["w"].as_f64()? as f32 / 2.0,
                b["y"].as_f64()? as f32 + b["h"].as_f64()? as f32 / 2.0,
            ))
        })
}

fn plus_of_pane(rb: &Value, pane: &str) -> Option<(f32, f32)> {
    rb["workbench"]["plusBounds"]
        .as_array()?
        .iter()
        .find(|entry| entry["pane"] == pane)
        .and_then(|entry| {
            let b = &entry["bounds"];
            Some((
                b["x"].as_f64()? as f32 + b["w"].as_f64()? as f32 / 2.0,
                b["y"].as_f64()? as f32 + b["h"].as_f64()? as f32 / 2.0,
            ))
        })
}

fn menu_row(rb: &Value, id: &str) -> Option<(f32, f32)> {
    rb["workbench"]["menuRows"]
        .as_array()?
        .iter()
        .find(|row| row["id"] == id)
        .and_then(|row| {
            let b = &row["bounds"];
            Some((
                b["x"].as_f64()? as f32 + b["w"].as_f64()? as f32 / 2.0,
                b["y"].as_f64()? as f32 + b["h"].as_f64()? as f32 / 2.0,
            ))
        })
}

fn panes(node: &Value, out: &mut Vec<Value>) {
    if node["kind"] == "pane" {
        out.push(node.clone());
    } else if node.is_object() {
        panes(&node["first"], out);
        panes(&node["second"], out);
    }
}

/// The pane whose tabs hold `chat`.
fn pane_of(rb: &Value, chat: &str) -> Option<Value> {
    let mut all = Vec::new();
    panes(&rb["workbench"]["root"], &mut all);
    all.into_iter().find(|pane| {
        pane["tabs"]
            .as_array()
            .is_some_and(|tabs| tabs.iter().any(|tab| tab["chatId"] == chat))
    })
}

fn badge<'a>(rb: &'a Value, chat: &str) -> Option<&'a Value> {
    rb["board"]["badges"]
        .as_array()?
        .iter()
        .find(|badge| badge["taskId"] == chat)
}

fn marks(rb: &Value, key: &str) -> Vec<String> {
    rb["board"]["marks"][key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

/// Every word any board surface drew this frame.
fn all_words(rb: &Value) -> Vec<String> {
    rb["board"]["marks"]
        .as_object()
        .into_iter()
        .flat_map(|marks| marks.values())
        .flat_map(|values| values.as_array().cloned().unwrap_or_default())
        .filter_map(|value| value.as_str().map(str::to_lowercase))
        .chain(
            rb["workbench"]["menuRows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row["id"].as_str().map(str::to_lowercase)),
        )
        .collect()
}

/// The steps, captures and failures of one run (the manifest).
struct Run {
    out: PathBuf,
    steps: Vec<Value>,
    captures: Vec<Value>,
    failures: Vec<String>,
}

impl Run {
    fn check(&mut self, step: &str, expected: &str, observed: Value, pass: bool) {
        if !pass {
            self.failures
                .push(format!("{step}: expected {expected}; observed {observed}"));
        }
        self.steps.push(json!({
            "step": step,
            "expected": expected,
            "observed": observed,
            "pass": pass,
        }));
    }

    fn capture(&mut self, window: Win, cx: &mut AsyncApp, name: &str) -> anyhow::Result<()> {
        let path = self.out.join(format!("{name}.png"));
        gpui::AnyWindowHandle::from(window).update(
            cx,
            |_, window, cx| -> anyhow::Result<()> {
                window.draw(cx).clear();
                window.render_to_image()?.save(&path)?;
                Ok(())
            },
        )??;
        let bytes = std::fs::read(&path)?;
        self.captures.push(json!({
            "file": format!("{name}.png"),
            "sha256": sha256_hex(&bytes),
            "bytes": bytes.len(),
        }));
        Ok(())
    }

    fn snapshot(&mut self, name: &str, value: &Value) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(value)?;
        let file = format!("{name}.json");
        std::fs::write(self.out.join(&file), &bytes)?;
        self.captures.push(json!({
            "file": file,
            "sha256": sha256_hex(&bytes),
            "bytes": bytes.len(),
        }));
        Ok(())
    }
}

/// Poll the readback until `done` holds (bounded), drawing every frame.
async fn wait_for(
    window: Win,
    cx: &mut AsyncApp,
    tries: usize,
    what: &str,
    done: impl Fn(&Value) -> bool,
) -> anyhow::Result<Value> {
    let mut last = Value::Null;
    for _ in 0..tries {
        frames(window, cx, 100).await?;
        last = readback(window, cx)?;
        if done(&last) {
            return Ok(last);
        }
    }
    anyhow::bail!("timed out waiting for {what}: {}", last["board"])
}

/// One engine RPC on the engine's runtime (the fixture's own readback of
/// engine truth, never a UI shortcut).
async fn rpc(
    runtime: &tokio::runtime::Handle,
    engine: &state::EngineHandle,
    method: &'static str,
    params: Value,
) -> anyhow::Result<Value> {
    let engine = engine.clone();
    runtime
        .spawn(async move {
            engine
                .client()
                .call(method, params)
                .await
                .map_err(|error| anyhow::anyhow!("{method}: {error}"))
        })
        .await?
}

async fn engine_badges(
    runtime: &tokio::runtime::Handle,
    engine: &state::EngineHandle,
    chats: &[&str],
) -> anyhow::Result<Value> {
    rpc(
        runtime,
        engine,
        methods::BOARD_CHAT_BADGES,
        json!({ "taskIds": chats }),
    )
    .await
}

fn user_messages(core: &EngineCore, chat: &str) -> usize {
    core.doc_host
        .open(chat)
        .ok()
        .and_then(|doc| doc.doc().read_entries().ok())
        .unwrap_or_default()
        .iter()
        .filter(|entry| entry.role == MessageRole::User)
        .count()
}

fn engine_chat_count(core: &EngineCore) -> usize {
    core.workspace
        .read_chats()
        .map(|chats| chats.len())
        .unwrap_or(0)
}

/// The chat row the UI holds for `chat`.
fn ui_chat(
    state: &gpui::Entity<state::AppState>,
    cx: &mut AsyncApp,
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

fn main() -> anyhow::Result<()> {
    let output = match std::env::args().nth(1) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/board/chat-ui"),
    };
    std::fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    for stale in ["manifest.json", "board-chat-ui.pass", "failure.txt"] {
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
                text: "Done: the reconnect step is in place.".into(),
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
            EngineProfile::development(&engine_data, "board-chat-ui", "board-chat-ui"),
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
    auth.dev_user_id = "board-chat-ui".into();
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
    // Chats are created through the public RPC (`Mutate createChat`), as the
    // app does, so the engine projects each chat's task identity: side chat
    // creation (a child of the main chat) validates its parent against it.
    let seeder = {
        let _entered = runtime.enter();
        luna_rpc::memory_client(core.rpc_service())
    };
    let ui_chats = runtime.block_on(async {
        let mut ui_chats = Vec::new();
        for (id, title, prompt, answer, minutes) in [
            (
                MAIN,
                MAIN_TITLE,
                "Plan the reconnect rollout: retry with backoff, then surface the offline banner.",
                "Plan: backoff first, then the banner. Each step is reversible.",
                3,
            ),
            (
                SOURCE,
                SOURCE_TITLE,
                "The sync tests flake on CI about once a day. Can we pin the clock?",
                "Yes: inject the clock and freeze it in the two flaky tests.",
                9,
            ),
            (
                OTHER,
                OTHER_TITLE,
                "Tighten the onboarding copy.",
                "Shortened the three onboarding cards.",
                45,
            ),
        ] {
            seeder
                .call(
                    "Mutate",
                    json!({
                        "op": "createChat",
                        "chatId": id,
                        "spaceId": PROJECT,
                        "cwd": project.to_string_lossy(),
                        "config": serde_json::to_value(config())?,
                    }),
                )
                .await?;
            core.workspace.rename_chat(id, title)?;
            let at = now - TimeDelta::minutes(minutes);
            let at_ms = at.timestamp_millis();
            let doc = core.doc_host.open(id)?;
            doc.doc().push_message(&text_entry(
                &format!("{id}-prompt"),
                MessageRole::User,
                prompt,
                &device,
                at_ms - 60_000,
            ))?;
            doc.doc().push_message(&text_entry(
                &format!("{id}-answer"),
                MessageRole::Assistant,
                answer,
                &device,
                at_ms,
            ))?;
            let mut chat = core.workspace.chat(id)?.expect("seeded chat");
            chat.created_at = at - TimeDelta::minutes(2);
            chat.last_message_at = Some(at);
            chat.last_seen_at = Some(at + TimeDelta::seconds(1));
            core.workspace.import_chat_row(&chat)?;
            ui_chats.push(chat);
        }
        anyhow::Ok(ui_chats)
    })?;
    let spaces = core.workspace.read_spaces()?;
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let engine_rt = runtime.handle().clone();
    let core = Arc::new(core);
    let core_for_ui = core.clone();
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59991,
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
            let engine = handle.clone();
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
                state.select_chat(Some(MAIN.into()), cx);
                state.fixture_attach_engine(handle, cx);
                state
            });
            let window = cx
                .open_window(
                    WindowOptions {
                        window_background: theme::Theme::of(cx).window_background_appearance(),
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(20.), px(20.)),
                            size(px(WIDTH), px(HEIGHT)),
                        ))),
                        show: false,
                        focus: false,
                        ..Default::default()
                    },
                    |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
                )
                .unwrap();
            let state_for_task = state.clone();
            cx.spawn(async move |cx| {
                let mut run = Run {
                    out: output.clone(),
                    steps: Vec::new(),
                    captures: Vec::new(),
                    failures: Vec::new(),
                };
                let result = scenario(window, cx, &mut run, &engine, &engine_rt, &core_for_ui, &state_for_task).await;
                if let Err(error) = &result {
                    run.failures.push(format!("{error:#}"));
                }
                let manifest = json!({
                    "fixture": "board-chat-ui-fixture",
                    "slice": "A10 board-ui-chat",
                    "commit": option_env!("LUNA_GIT_SHA").unwrap_or("unknown"),
                    "engine": "in-process EngineCore (Mock harness, temp data dir)",
                    "input": "real pointer and keyboard events through GPUI hit testing",
                    "pass": run.failures.is_empty(),
                    "failures": run.failures,
                    "steps": run.steps,
                    "artifacts": run.captures,
                });
                let written = std::fs::write(
                    output.join("manifest.json"),
                    serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
                );
                let failed = manifest["pass"] != true || written.is_err();
                if failed {
                    let text = manifest["failures"].to_string();
                    eprintln!("Board chat UI fixture failed: {text}");
                    let _ = std::fs::write(output.join("failure.txt"), format!("{text}\n"));
                    *failure_for_ui.lock().unwrap() = Some(text);
                    // AppKit's quit can end the process with status 0 before
                    // `run` returns: leave with the failure status now.
                    std::process::exit(1);
                }
                let _ = std::fs::write(
                    output.join("board-chat-ui.pass"),
                    "PASS: headless production Shell on a real in-process engine; working chat header badge + separate status control (no Done), sidebar row and side-chat tab badges with accessible labels, a distinct Reviewer badge, a Related-ticket receipt on the source discussion, main Side chat Fresh/With context vs side New side chat/Hand off only (no fork), convert in place (same chat id, nothing sent), create ticket from chat (no chat added), Go to chat after closing the side tab selects the exact side chat in its main chat's workbench, and no reattach controls\n",
                );
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

/// Open a sidebar row's chat menu with a real right-click.
async fn right_click_row(window: Win, cx: &mut AsyncApp, chat: &str) -> anyhow::Result<()> {
    let rb = readback(window, cx)?;
    let at = sidebar_row(&rb, chat)
        .ok_or_else(|| anyhow::anyhow!("no sidebar row painted for {chat}"))?;
    click_at(window, cx, at, MouseButton::Right).await
}

async fn click_row(window: Win, cx: &mut AsyncApp, chat: &str) -> anyhow::Result<()> {
    let rb = readback(window, cx)?;
    let at = sidebar_row(&rb, chat)
        .ok_or_else(|| anyhow::anyhow!("no sidebar row painted for {chat}"))?;
    click_at(window, cx, at, MouseButton::Left).await
}

/// Click a board surface by its probe label, waiting for it to paint.
async fn click_board(
    window: Win,
    cx: &mut AsyncApp,
    label: &str,
    button: MouseButton,
) -> anyhow::Result<()> {
    // Only what paints from now on counts (no stale bounds).
    window.update(cx, |shell, _, _| shell.fixture_board_chat_probe_clear())?;
    let rb = wait_for(window, cx, 80, &format!("{label} to paint"), |rb| {
        board_bounds(rb, label).is_some()
    })
    .await?;
    let at = board_bounds(&rb, label).expect("waited for it");
    click_at(window, cx, at, button).await
}

/// Collect every board word drawn so far (the reattach audit reads it).
fn audit(words: &mut Vec<String>, rb: &Value) {
    words.extend(all_words(rb));
}

#[allow(clippy::too_many_arguments)]
async fn scenario(
    window: Win,
    cx: &mut AsyncApp,
    run: &mut Run,
    engine: &state::EngineHandle,
    runtime: &tokio::runtime::Handle,
    core: &EngineCore,
    state: &gpui::Entity<state::AppState>,
) -> anyhow::Result<()> {
    let mut words: Vec<String> = Vec::new();
    window.update(cx, |shell, _, cx| {
        shell.fixture_enable_sidebar_probe(cx);
        shell.fixture_board_chat_probe(true, cx);
    })?;
    // f00. The workbench is MAIN's and the engine answers badges.
    let start = wait_for(
        window,
        cx,
        150,
        "MAIN's workbench and the first badge read",
        |rb| {
            rb["workbench"]["loaded"] == true
                && rb["workbench"]["main"] == MAIN
                && sidebar_row(rb, MAIN).is_some()
        },
    )
    .await?;
    if start["board"]["badgesUnavailable"] == true {
        anyhow::bail!(
            "the engine does not serve BoardChatBadges (BoardError::NotImplemented): the chat model slice is not in this tree"
        );
    }
    run.capture(window, cx, "f00-ordinary")?;
    run.snapshot("rb-00-ordinary", &start)?;
    let ordinary = [MAIN, SOURCE, OTHER].iter().all(|chat| {
        badge(&start, chat).is_none()
            && board_bounds(&start, &format!("row-badge:chat-{chat}")).is_none()
    });
    run.check(
        "ordinary-rows",
        "no ticket badge on any ordinary chat's row or header",
        json!({ "badges": start["board"]["badges"] }),
        ordinary,
    );
    audit(&mut words, &start);

    // f01. A main chat's `+`: Side chat, Fresh or With context.
    let main_pane = pane_of(&start, MAIN).ok_or_else(|| anyhow::anyhow!("MAIN has no pane"))?;
    let main_pane_id = main_pane["id"].as_str().unwrap_or_default().to_string();
    let plus = plus_of_pane(&start, &main_pane_id)
        .ok_or_else(|| anyhow::anyhow!("MAIN's + did not paint"))?;
    click_at(window, cx, plus, MouseButton::Left).await?;
    let chooser = wait_for(window, cx, 40, "the main chooser", |rb| {
        rb["chooser"]["open"] == true
    })
    .await?;
    run.capture(window, cx, "f01-main-chooser")?;
    let kinds = marks(&chooser, "chooser-kinds");
    run.check(
        "main-chooser",
        "a main chat's + opens Side chat with Fresh and With context (no side-origin card)",
        json!({ "source": chooser["chooser"]["source"], "kinds": kinds, "sideCard": chooser["board"]["sideCard"] }),
        chooser["chooser"]["source"] == MAIN
            && kinds == ["Fresh", "With context"]
            && chooser["board"]["sideCard"].is_null(),
    );
    audit(&mut words, &chooser);
    press(window, cx, "escape").await?;
    wait_for(window, cx, 30, "the chooser to close", |rb| {
        rb["chooser"]["open"] == false
    })
    .await?;

    // f02-f03. Convert MAIN in place from its sidebar menu.
    let chats_before = cx.update(|cx| state.read(cx).chats.len());
    let engine_chats_before = engine_chat_count(core);
    let main_prompts_before = user_messages(core, MAIN);
    right_click_row(window, cx, MAIN).await?;
    let menu = wait_for(window, cx, 40, "MAIN's chat menu", |rb| {
        !marks(rb, &format!("menu:chat:{MAIN}")).is_empty()
    })
    .await?;
    let rows = marks(&menu, &format!("menu:chat:{MAIN}"));
    run.check(
        "main-menu",
        "an ordinary main chat's menu offers Convert and Create ticket, keeps its context fork, and no side-chat-only rows",
        json!({ "rows": rows, "fork": marks(&menu, &format!("chat-menu-fork:{MAIN}")) }),
        rows.iter().any(|row| row == "Convert this chat to a ticket…")
            && rows.iter().any(|row| row == "Create ticket from this chat…")
            && !rows.iter().any(|row| row == "Hand off to a new main chat…" || row == "New side chat")
            && marks(&menu, &format!("chat-menu-fork:{MAIN}")) == ["shown"],
    );
    audit(&mut words, &menu);
    click_board(
        window,
        cx,
        &format!("menu:chat:convert:{MAIN}"),
        MouseButton::Left,
    )
    .await?;
    let dialog = wait_for(window, cx, 40, "the convert dialog", |rb| {
        rb["board"]["dialog"]["kind"] == "Convert"
    })
    .await?;
    run.capture(window, cx, "f02-convert-dialog")?;
    run.check(
        "convert-proposes-title",
        "the dialog proposes the chat's current title",
        dialog["board"]["dialog"].clone(),
        dialog["board"]["dialog"]["title"] == MAIN_TITLE
            && dialog["board"]["dialog"]["chatId"] == MAIN,
    );
    click_board(window, cx, "dialog:primary", MouseButton::Left).await?;
    let converted = wait_for(window, cx, 150, "MAIN's working badge", |rb| {
        badge(rb, MAIN).is_some_and(|badge| badge["role"] == "working")
            && board_bounds(rb, &format!("header-ticket:{MAIN}")).is_some()
            && rb["board"]["dialog"].is_null()
    })
    .await
    .map_err(|error| {
        anyhow::anyhow!(
            "{error}; dialog: {}",
            readback(window, cx)
                .map(|rb| rb["board"]["dialog"].clone())
                .unwrap_or_default()
        )
    })?;
    frames(window, cx, 400).await?;
    let converted = readback(window, cx).unwrap_or(converted);
    run.capture(window, cx, "f03-converted-header")?;
    run.snapshot("rb-03-converted", &converted)?;
    let main_badge = badge(&converted, MAIN).cloned().unwrap_or_default();
    let ticket_main = main_badge["workItemId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let key_main = main_badge["key"].as_str().unwrap_or_default().to_string();
    let engine_main = engine_badges(runtime, engine, &[MAIN]).await?;
    let engine_working = engine_main["badges"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|badge| {
            badge["taskId"] == MAIN
                && badge["role"] == "working"
                && badge["workItemId"] == ticket_main.as_str()
        });
    run.check(
        "convert-in-place",
        "the same chat id is the ticket's working chat; no chat added; nothing sent",
        json!({
            "badge": main_badge,
            "engine": engine_main,
            "uiChats": [chats_before, cx.update(|cx| state.read(cx).chats.len())],
            "engineChats": [engine_chats_before, engine_chat_count(core)],
            "userMessages": [main_prompts_before, user_messages(core, MAIN)],
        }),
        engine_working
            && cx.update(|cx| state.read(cx).chats.len()) == chats_before
            && engine_chat_count(core) == engine_chats_before
            && user_messages(core, MAIN) == main_prompts_before,
    );
    let header = board_bounds(&converted, &format!("header-ticket:{MAIN}"));
    let status = board_bounds(&converted, &format!("header-status:{MAIN}"));
    let strip = marks(&converted, &format!("strip:{MAIN}"));
    run.check(
        "header-badge-and-status",
        "the header shows `Ticket KEY` and a separate workflow-status control",
        json!({ "header": header, "status": status, "strip": strip }),
        header.is_some()
            && status.is_some()
            && header != status
            && strip
                .iter()
                .any(|text| *text == format!("Ticket {key_main}")),
    );
    let row_marks = marks(&converted, &format!("row-badge:chat-{MAIN}"));
    run.check(
        "sidebar-row-badge",
        "the main sidebar row carries the compact key with an accessible label naming the ticket",
        json!({ "bounds": board_bounds(&converted, &format!("row-badge:chat-{MAIN}")), "marks": row_marks }),
        board_bounds(&converted, &format!("row-badge:chat-{MAIN}")).is_some()
            && row_marks.first() == Some(&key_main)
            && row_marks.get(1).is_some_and(|label| label.contains(&key_main) && label.contains(MAIN_TITLE)),
    );
    audit(&mut words, &converted);

    // f04. The status control: moves, never Done.
    click_board(
        window,
        cx,
        &format!("header-status:{MAIN}"),
        MouseButton::Left,
    )
    .await?;
    let status_menu = wait_for(window, cx, 40, "the status menu", |rb| {
        !rb["board"]["statusMenu"].is_null()
    })
    .await?;
    run.capture(window, cx, "f04-status-menu")?;
    let options = marks(&status_menu, "status-menu");
    run.check(
        "status-control",
        "the status menu offers Start/Continue or Stop/Reopen and Open ticket, never Done",
        json!(options),
        !options.is_empty()
            && options
                .iter()
                .any(|row| *row == format!("Open ticket {key_main}"))
            && !options.iter().any(|row| {
                row.to_lowercase().contains("move to done")
                    || row.to_lowercase().starts_with("done")
            }),
    );
    audit(&mut words, &status_menu);
    click_at(window, cx, (WIDTH / 2.0, 12.0), MouseButton::Left).await?;
    wait_for(window, cx, 30, "the status menu to close", |rb| {
        rb["board"]["statusMenu"].is_null()
    })
    .await?;

    // f05. The badge opens that exact ticket.
    click_board(
        window,
        cx,
        &format!("header-ticket:{MAIN}"),
        MouseButton::Left,
    )
    .await?;
    let opened = wait_for(window, cx, 40, "the Board with MAIN's ticket", |rb| {
        rb["board"]["route"] == "page:board"
    })
    .await?;
    run.capture(window, cx, "f05-badge-opens-ticket")?;
    run.check(
        "badge-opens-ticket",
        "clicking `Ticket KEY` opens the Board on that ticket (stable id)",
        json!({ "route": opened["board"]["route"], "selected": opened["board"]["boardSelectedItem"] }),
        opened["board"]["boardSelectedItem"] == ticket_main.as_str(),
    );
    click_row(window, cx, MAIN).await?;
    wait_for(window, cx, 80, "back on MAIN", |rb| {
        rb["board"]["route"] == "chat"
            && rb["workbench"]["main"] == MAIN
            && rb["workbench"]["loaded"] == true
    })
    .await?;

    // f06. A fresh side chat under MAIN (the engine's own creation), opened
    // through Go to chat.
    let created = rpc(
        runtime,
        engine,
        methods::BOARD_CREATE_SIDE_CHAT,
        json!({
            "requestId": "fixture-side-1",
            "callerTaskId": MAIN,
            "context": { "kind": "fresh" },
            "title": "Reconnect banner",
            "start": false,
        }),
    )
    .await?;
    let side = created["taskId"].as_str().unwrap_or_default().to_string();
    anyhow::ensure!(
        !side.is_empty(),
        "the side chat create returned no chat: {created}"
    );
    let _ = core.workspace.rename_chat(&side, "Reconnect banner");
    let side_id = side.clone();
    for _ in 0..100 {
        if ui_chat(state, cx, &side).is_some() {
            break;
        }
        settle(cx, 100).await;
    }
    let lineage_before =
        ui_chat(state, cx, &side).map(|chat| chat.child_thread.map(|child| child.parent_task_id));
    window.update(cx, |shell, _, cx| {
        shell.fixture_board_go_to_chat(None, Some(side_id.clone()), cx)
    })?;
    let opened_side = wait_for(
        window,
        cx,
        150,
        "the side chat's tab in MAIN's workbench",
        |rb| rb["workbench"]["main"] == MAIN && rb["workbench"]["focusedChat"] == side_id.as_str(),
    )
    .await?;
    run.capture(window, cx, "f06-side-chat-tab")?;
    run.check(
        "side-chat-in-main-workbench",
        "the fresh side chat opens as a tab in MAIN's workbench, not in the sidebar",
        json!({ "side": side, "created": created, "order": opened_side["sidebar"]["order"] }),
        pane_of(&opened_side, &side).is_some() && sidebar_row(&opened_side, &side).is_none(),
    );

    // f07. The side chat's tab menu: New side chat, Hand off, Convert; no
    // context fork. Then convert it in place.
    let at = tab_center(&opened_side, &side)
        .ok_or_else(|| anyhow::anyhow!("the side tab did not paint"))?;
    click_at(window, cx, at, MouseButton::Right).await?;
    let tab_menu = wait_for(window, cx, 40, "the side tab's menu", |rb| {
        !marks(rb, &format!("menu:tab:{side}")).is_empty()
    })
    .await?;
    let rows = marks(&tab_menu, &format!("menu:tab:{side}"));
    run.capture(window, cx, "f07-side-tab-menu")?;
    run.check(
        "side-menu-vocabulary",
        "a side chat offers only New side chat (fresh sibling) and Hand off, with the no-fork explanation; no With context or Fork",
        json!(rows),
        rows.iter().any(|row| row == "New side chat")
            && rows.iter().any(|row| row == "Hand off to a new main chat…")
            && rows.iter().any(|row| row == "No context forks from a side chat")
            && !rows.iter().any(|row| {
                let row = row.to_lowercase();
                row != "no context forks from a side chat" && (row.contains("with context") || row.contains("fork"))
            }),
    );
    audit(&mut words, &tab_menu);
    click_board(
        window,
        cx,
        &format!("menu:tab:convert:{side}"),
        MouseButton::Left,
    )
    .await?;
    wait_for(window, cx, 40, "the side convert dialog", |rb| {
        rb["board"]["dialog"]["kind"] == "Convert"
    })
    .await?;
    click_board(window, cx, "dialog:primary", MouseButton::Left).await?;
    let side_converted = wait_for(
        window,
        cx,
        150,
        "the side chat's working badge on its tab",
        |rb| {
            badge(rb, &side).is_some_and(|badge| badge["role"] == "working")
                && board_bounds(rb, &format!("tab-badge:{side}")).is_some()
        },
    )
    .await?;
    frames(window, cx, 300).await?;
    let side_converted = readback(window, cx).unwrap_or(side_converted);
    run.capture(window, cx, "f08-side-tab-badge")?;
    run.snapshot("rb-08-side-converted", &side_converted)?;
    let side_badge = badge(&side_converted, &side).cloned().unwrap_or_default();
    let ticket_side = side_badge["workItemId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let key_side = side_badge["key"].as_str().unwrap_or_default().to_string();
    let lineage_after =
        ui_chat(state, cx, &side).map(|chat| chat.child_thread.map(|child| child.parent_task_id));
    run.check(
        "side-tab-badge",
        "the side chat's tab carries the same compact key; its placement and lineage are unchanged",
        json!({
            "marks": marks(&side_converted, &format!("tab-badge:{side}")),
            "main": side_converted["workbench"]["main"],
            "lineage": [lineage_before, lineage_after],
        }),
        marks(&side_converted, &format!("tab-badge:{side}")).first() == Some(&key_side)
            && side_converted["workbench"]["main"] == MAIN
            && pane_of(&side_converted, &side).is_some()
            && sidebar_row(&side_converted, &side).is_none()
            && lineage_before == lineage_after,
    );
    audit(&mut words, &side_converted);

    // f09. The side chat's `+`: its own card, never the main chooser.
    let side_pane = pane_of(&side_converted, &side)
        .and_then(|pane| pane["id"].as_str().map(str::to_string))
        .unwrap_or_default();
    let plus = plus_of_pane(&side_converted, &side_pane)
        .ok_or_else(|| anyhow::anyhow!("the side pane's + did not paint"))?;
    click_at(window, cx, plus, MouseButton::Left).await?;
    let card = wait_for(window, cx, 40, "the side chat's creation card", |rb| {
        !rb["board"]["sideCard"].is_null()
    })
    .await?;
    run.capture(window, cx, "f09-side-creation-card")?;
    run.check(
        "side-creation-card",
        "a side chat's + offers exactly New side chat and Hand off to a new main chat",
        json!({ "card": card["board"]["sideCard"], "options": marks(&card, "side-card"), "chooser": card["chooser"]["open"] }),
        card["board"]["sideCard"]["sideId"] == side.as_str()
            && marks(&card, "side-card") == ["New side chat", "Hand off to a new main chat…"]
            && card["chooser"]["open"] == false,
    );
    audit(&mut words, &card);
    press(window, cx, "escape").await?;
    wait_for(window, cx, 30, "the side card to close", |rb| {
        rb["board"]["sideCard"].is_null()
    })
    .await?;

    // f10. A reviewer for the side ticket: a distinct `Reviewer · KEY`.
    let review = rpc(
        runtime,
        engine,
        methods::BOARD_REVIEW_START,
        json!({ "requestId": "fixture-review-1", "workItemId": ticket_side }),
    )
    .await;
    let mut reviewer = review.as_ref().ok().and_then(|receipt| {
        receipt["run"]["reviewerTaskId"]
            .as_str()
            .map(str::to_string)
    });
    for _ in 0..100 {
        if reviewer.is_some() {
            break;
        }
        settle(cx, 150).await;
        reviewer = rpc(
            runtime,
            engine,
            methods::BOARD_REVIEW_STATUS,
            json!({ "workItemId": ticket_side }),
        )
        .await
        .ok()
        .and_then(|status| {
            status["runs"]
                .as_array()
                .into_iter()
                .flatten()
                .find_map(|run| run["reviewerTaskId"].as_str().map(str::to_string))
        });
    }
    match reviewer {
        Some(reviewer) => {
            for _ in 0..100 {
                if ui_chat(state, cx, &reviewer).is_some() {
                    break;
                }
                settle(cx, 100).await;
            }
            window.update(cx, |shell, _, cx| {
                shell.fixture_board_chat_refresh(cx);
                shell.fixture_board_go_to_chat(None, Some(reviewer.clone()), cx);
            })?;
            let shown = wait_for(window, cx, 150, "the reviewer's tab and badge", |rb| {
                rb["workbench"]["focusedChat"] == reviewer.as_str()
                    && badge(rb, &reviewer).is_some_and(|badge| badge["role"] == "reviewer")
                    && board_bounds(rb, &format!("tab-badge:{reviewer}")).is_some()
            })
            .await;
            match shown {
                Ok(shown) => {
                    run.capture(window, cx, "f10-reviewer-badge")?;
                    let tab = marks(&shown, &format!("tab-badge:{reviewer}"));
                    run.check(
                        "reviewer-badge",
                        "a reviewer side chat shows `Reviewer · KEY` (its own glyph and words), never the working badge",
                        json!({ "tab": tab, "header": board_bounds(&shown, &format!("header-reviewer:{reviewer}")), "working": board_bounds(&shown, &format!("header-ticket:{reviewer}")) }),
                        tab.first() == Some(&format!("Reviewer · {key_side}"))
                            && board_bounds(&shown, &format!("header-reviewer:{reviewer}")).is_some()
                            && board_bounds(&shown, &format!("header-ticket:{reviewer}")).is_none(),
                    );
                    audit(&mut words, &shown);
                }
                Err(error) => run.check("reviewer-badge", "the reviewer side chat shows `Reviewer · KEY`", json!(format!("{error:#}")), false),
            }
        }
        None => run.check(
            "reviewer-badge",
            "the engine starts a reviewer side chat for the side ticket",
            json!({ "reviewStart": review.as_ref().map(|value| value.clone()).unwrap_or_else(|error| json!(format!("{error:#}"))) }),
            false,
        ),
    }

    // f11. Create ticket from SOURCE: a receipt, no chat, no working badge.
    click_row(window, cx, SOURCE).await?;
    wait_for(window, cx, 80, "SOURCE's workbench", |rb| {
        rb["workbench"]["main"] == SOURCE && rb["workbench"]["loaded"] == true
    })
    .await?;
    let chats_before = cx.update(|cx| state.read(cx).chats.len());
    let engine_chats_before = engine_chat_count(core);
    right_click_row(window, cx, SOURCE).await?;
    click_board(
        window,
        cx,
        &format!("menu:chat:create-ticket:{SOURCE}"),
        MouseButton::Left,
    )
    .await?;
    wait_for(window, cx, 40, "the create dialog", |rb| {
        rb["board"]["dialog"]["kind"] == "CreateTicket"
    })
    .await?;
    click_board(window, cx, "dialog:primary", MouseButton::Left).await?;
    let receipt = wait_for(window, cx, 150, "SOURCE's receipt", |rb| {
        rb["board"]["bounds"].as_object().is_some_and(|bounds| {
            bounds
                .keys()
                .any(|label| label.starts_with(&format!("receipt-open:{SOURCE}:")))
        })
    })
    .await?;
    frames(window, cx, 300).await?;
    let receipt = readback(window, cx).unwrap_or(receipt);
    run.capture(window, cx, "f11-source-receipt")?;
    run.snapshot("rb-11-source-receipt", &receipt)?;
    let open_label = receipt["board"]["bounds"]
        .as_object()
        .and_then(|bounds| {
            bounds
                .keys()
                .find(|label| label.starts_with(&format!("receipt-open:{SOURCE}:")))
                .cloned()
        })
        .unwrap_or_default();
    let ticket_source = open_label
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_string();
    let engine_source = engine_badges(runtime, engine, &[SOURCE]).await?;
    run.check(
        "create-ticket-from-chat",
        "a receipt with Open ticket; no working badge on the source; no chat added (UI or engine)",
        json!({
            "strip": marks(&receipt, &format!("strip:{SOURCE}")),
            "engine": engine_source,
            "uiChats": [chats_before, cx.update(|cx| state.read(cx).chats.len())],
            "engineChats": [engine_chats_before, engine_chat_count(core)],
        }),
        badge(&receipt, SOURCE).is_none()
            && board_bounds(&receipt, &format!("header-ticket:{SOURCE}")).is_none()
            && !engine_source["badges"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|badge| badge["taskId"] == SOURCE)
            && cx.update(|cx| state.read(cx).chats.len()) == chats_before
            && engine_chat_count(core) == engine_chats_before
            && marks(&receipt, &format!("strip:{SOURCE}"))
                .iter()
                .any(|text| text.starts_with("Created ")),
    );
    audit(&mut words, &receipt);
    click_board(window, cx, &open_label, MouseButton::Left).await?;
    let opened = wait_for(window, cx, 40, "the created ticket on the Board", |rb| {
        rb["board"]["route"] == "page:board"
    })
    .await?;
    run.check(
        "receipt-opens-ticket",
        "Open ticket on the receipt opens that ticket by id",
        json!({ "selected": opened["board"]["boardSelectedItem"], "ticket": ticket_source }),
        opened["board"]["boardSelectedItem"] == ticket_source.as_str() && !ticket_source.is_empty(),
    );

    // f12. Go to chat after closing the side chat's tab, from elsewhere.
    click_row(window, cx, MAIN).await?;
    let back = wait_for(window, cx, 80, "MAIN's workbench again", |rb| {
        rb["workbench"]["main"] == MAIN
            && rb["workbench"]["loaded"] == true
            && tab_center(rb, &side).is_some()
    })
    .await?;
    let at = tab_center(&back, &side).expect("waited for it");
    click_at(window, cx, at, MouseButton::Right).await?;
    let menu = wait_for(window, cx, 40, "the tab menu's Close row", |rb| {
        menu_row(rb, "wb-menu-close").is_some()
    })
    .await?;
    click_at(
        window,
        cx,
        menu_row(&menu, "wb-menu-close").expect("waited for it"),
        MouseButton::Left,
    )
    .await?;
    wait_for(window, cx, 60, "the side tab to close", |rb| {
        pane_of(rb, &side).is_none()
    })
    .await?;
    click_row(window, cx, OTHER).await?;
    wait_for(window, cx, 80, "OTHER selected", |rb| {
        rb["board"]["selectedChat"] == OTHER
    })
    .await?;
    let chats_before = cx.update(|cx| state.read(cx).chats.len());
    let engine_chats_before = engine_chat_count(core);
    let side_prompts_before = user_messages(core, &side);
    let launches = |detail: &Value| detail["launches"].as_array().map(Vec::len).unwrap_or(0);
    let detail_before = rpc(
        runtime,
        engine,
        methods::BOARD_READ,
        json!({ "workItemId": ticket_side }),
    )
    .await?;
    press(
        window,
        cx,
        if cfg!(target_os = "macos") {
            "cmd-k"
        } else {
            "ctrl-k"
        },
    )
    .await?;
    type_text(window, cx, "banner").await?;
    let goto = format!("palette:goto:{ticket_side}");
    click_board(window, cx, &goto, MouseButton::Left).await?;
    let revealed = wait_for(
        window,
        cx,
        150,
        "the exact side chat selected in MAIN's workbench",
        |rb| {
            rb["workbench"]["main"] == MAIN
                && rb["workbench"]["loaded"] == true
                && rb["workbench"]["focusedChat"] == side.as_str()
        },
    )
    .await?;
    frames(window, cx, 400).await?;
    let revealed = readback(window, cx).unwrap_or(revealed);
    run.capture(window, cx, "f12-go-to-chat")?;
    run.snapshot("rb-12-go-to-chat", &revealed)?;
    let detail_after = rpc(
        runtime,
        engine,
        methods::BOARD_READ,
        json!({ "workItemId": ticket_side }),
    )
    .await?;
    let lineage_now =
        ui_chat(state, cx, &side).map(|chat| chat.child_thread.map(|child| child.parent_task_id));
    run.check(
        "go-to-chat-after-close",
        "Go to chat reopens the exact side chat in MAIN's workbench: no relaunch, no prompt, no promotion or reparenting",
        json!({
            "focused": revealed["workbench"]["focusedChat"],
            "main": revealed["workbench"]["main"],
            "launches": [launches(&detail_before), launches(&detail_after)],
            "prompts": [side_prompts_before, user_messages(core, &side)],
            "uiChats": [chats_before, cx.update(|cx| state.read(cx).chats.len())],
            "engineChats": [engine_chats_before, engine_chat_count(core)],
            "lineage": [lineage_before, lineage_now],
            "inSidebar": sidebar_row(&revealed, &side).is_some(),
        }),
        launches(&detail_before) == launches(&detail_after)
            && user_messages(core, &side) == side_prompts_before
            && cx.update(|cx| state.read(cx).chats.len()) == chats_before
            && engine_chat_count(core) == engine_chats_before
            && lineage_before == lineage_now
            && sidebar_row(&revealed, &side).is_none()
            && revealed["board"]["revealPending"].is_null(),
    );
    audit(&mut words, &revealed);
    let goto_marks = marks(&revealed, &goto);
    run.check(
        "palette-by-stable-id",
        "the palette's Go to chat entry is keyed by the ticket id and names its key",
        json!({ "label": goto, "marks": goto_marks }),
        goto_marks
            .first()
            .is_some_and(|label| label.contains(&key_side)),
    );

    // No reattach control anywhere this run drew.
    let offending: Vec<&String> = words
        .iter()
        .filter(|word| REATTACH_WORDS.iter().any(|bad| word.contains(bad)))
        .collect();
    run.check(
        "no-reattach-controls",
        "no Move here / Reattach / Change parent on any board surface or tab menu",
        json!(offending),
        offending.is_empty(),
    );
    let _ = (key_main, ticket_main);
    Ok(())
}
