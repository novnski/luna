//! Headless GPUI E2E for sidebar drag-reorder and the right pane's related
//! chat lists (Child chats, Side chats) built from the sidebar's task row.
//!
//! Drives the production `Shell` in a hidden window with real mouse input
//! (`PlatformInput` move/down/up through GPUI's hit testing, drag and drop)
//! against an in-process `EngineCore` with a scripted Mock harness. Rows are
//! located black-box: a pointer sweep reads which row reports hover. Task
//! commands reach the real engine; the fixture copies the engine's
//! authoritative row fields back into the UI (no workspace watch, so seeded
//! lineage and statuses stay put). Side chats (persistent forks) are created
//! through the production creator and the real CreateSideChat RPC; their
//! published rows (real project, source link) are copied in the same way,
//! and they are titled through the engine's ordinary rename. Fork answers
//! are covered by `side-chat-pane-fixture`, not here.
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. A pinned task dragged below an unpinned task becomes unpinned or lands
//!    among unpinned rows; an unpinned task dragged into the pins gets pinned.
//! 2. Reordering pins sends bounds the engine rejects ("pinned order
//!    changed"), or the UI snaps back until a frame arrives.
//! 3. The first unpinned drag reorders only the visible page, forgets tasks
//!    outside it, or leaves the sort on Last updated so the drop is undone.
//! 4. A drop released outside every row leaves the source dimmed or a stale
//!    drop line; a click without travel starts a drag instead of opening.
//! 5. Child/side chat rows do not use the sidebar row (project line, harness,
//!    title, time), or their pin/settle/archive actions do nothing, act on the
//!    wrong record, or put the row in the wrong section; a fork's actions
//!    write device-local marks instead of its own synced flags.
//! 6. A fork shows its source's title instead of its own chat title, or does
//!    not open as a Side tab under its source.
//! 7. Back from Side/Child chats closes their tabs or cannot reach the
//!    surface menu; Side chats and Child chats are not adjacent in it.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example related-chats-fixture
//! Run:   target/debug/examples/related-chats-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes r01..r15 PNG captures, readback.json and related-chats.pass; a
//! failure writes failure.txt/failure.png and exits nonzero.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, Entity, MouseButton, PlatformInput, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
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

const PROJECT: &str = "related-project";
const PARENT: &str = "e0e0e0e0-0000-4000-8000-000000000001";
const PIN_A: &str = "e0e0e0e0-0000-4000-8000-0000000000a1";
const PIN_B: &str = "e0e0e0e0-0000-4000-8000-0000000000a2";
const PIN_C: &str = "e0e0e0e0-0000-4000-8000-0000000000a3";
const OPEN_1: &str = "e0e0e0e0-0000-4000-8000-0000000000b1";
const OPEN_2: &str = "e0e0e0e0-0000-4000-8000-0000000000b2";
const OPEN_3: &str = "e0e0e0e0-0000-4000-8000-0000000000b3";
const CHILD_NEW: &str = "e1e1e1e1-0000-4000-8000-000000000001";
const CHILD_OLD: &str = "e1e1e1e1-0000-4000-8000-000000000002";
const CHILD_ARCHIVED: &str = "e1e1e1e1-0000-4000-8000-000000000003";
const CHILD_ACTIVE: &str = "e1e1e1e1-0000-4000-8000-000000000004";
const SIDE_Q1: &str = "Does the cache migration need a feature flag?";
const SIDE_Q2: &str = "Which call sites still read the old settings key?";
const SIDE_Q3: &str = "Should the rollout flag default to off?";
const MOCK_ANSWER: &str = "Yes; gate it behind the existing rollout flag.";
const WIDTH: f32 = 1440.0;
const HEIGHT: f32 = 900.0;
const PANE_WIDTH: f32 = 520.0;

struct Spec {
    id: &'static str,
    title: &'static str,
    minutes_ago: i64,
    pin: Option<&'static str>,
    parent: Option<&'static str>,
    archived: bool,
}

const SPECS: [Spec; 11] = [
    Spec {
        id: PARENT,
        title: "Settings migration",
        minutes_ago: 1,
        pin: None,
        parent: None,
        archived: false,
    },
    Spec {
        id: PIN_A,
        title: "Release checklist",
        minutes_ago: 40,
        pin: Some("1"),
        parent: None,
        archived: false,
    },
    Spec {
        id: PIN_B,
        title: "Incident notes",
        minutes_ago: 30,
        pin: Some("11"),
        parent: None,
        archived: false,
    },
    Spec {
        id: PIN_C,
        title: "Roadmap draft",
        minutes_ago: 50,
        pin: Some("111"),
        parent: None,
        archived: false,
    },
    Spec {
        id: OPEN_1,
        title: "Cache warmup",
        minutes_ago: 5,
        pin: None,
        parent: None,
        archived: false,
    },
    Spec {
        id: OPEN_2,
        title: "Docs typos",
        minutes_ago: 10,
        pin: None,
        parent: None,
        archived: false,
    },
    Spec {
        id: OPEN_3,
        title: "Flaky test triage",
        minutes_ago: 20,
        pin: None,
        parent: None,
        archived: false,
    },
    Spec {
        id: CHILD_NEW,
        title: "Migrate settings reads",
        minutes_ago: 3,
        pin: None,
        parent: Some(PARENT),
        archived: false,
    },
    Spec {
        id: CHILD_OLD,
        title: "Audit call sites",
        minutes_ago: 8,
        pin: None,
        parent: Some(PARENT),
        archived: false,
    },
    Spec {
        id: CHILD_ARCHIVED,
        title: "Abandoned approach",
        minutes_ago: 90,
        pin: None,
        parent: Some(PARENT),
        archived: true,
    },
    // Never acted on: the active child shown beside active side chats.
    Spec {
        id: CHILD_ACTIVE,
        title: "Profile startup",
        minutes_ago: 2,
        pin: None,
        parent: Some(PARENT),
        archived: false,
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

fn capture(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    path: PathBuf,
) -> anyhow::Result<()> {
    let window = gpui::AnyWindowHandle::from(window);
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })??;
    Ok(())
}

fn draw(window: WindowHandle<shell::Shell>, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
    })?;
    Ok(())
}

fn input(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    event: PlatformInput,
) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(event, cx);
    })?;
    Ok(())
}

fn hover_at(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    x: f32,
    y: f32,
    pressed: Option<MouseButton>,
) -> anyhow::Result<()> {
    input(
        window,
        cx,
        PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position: point(px(x), px(y)),
            pressed_button: pressed,
            modifiers: Default::default(),
        }),
    )
}

fn press_at(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    x: f32,
    y: f32,
    down: bool,
) -> anyhow::Result<()> {
    let position = point(px(x), px(y));
    input(
        window,
        cx,
        if down {
            PlatformInput::MouseDown(gpui::MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            })
        } else {
            PlatformInput::MouseUp(gpui::MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
            })
        },
    )
}

/// A real click: hover (so hover-revealed controls paint), then down/up.
async fn click_at(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    x: f32,
    y: f32,
) -> anyhow::Result<()> {
    hover_at(window, cx, x, y, None)?;
    draw(window, cx)?;
    settle(cx, 60).await;
    draw(window, cx)?;
    press_at(window, cx, x, y, true)?;
    press_at(window, cx, x, y, false)?;
    settle(cx, 120).await;
    Ok(())
}

fn readback(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
) -> anyhow::Result<serde_json::Value> {
    window.update(cx, |shell, _, cx| shell.fixture_related_readback(cx))
}

/// Black-box row geometry: sweep the pointer down a column and record the
/// rows that report hover, with their top/bottom edges in window pixels.
fn row_spans(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    x: f32,
    from: f32,
    to: f32,
    prefix: &str,
) -> anyhow::Result<Vec<(String, f32, f32)>> {
    draw(window, cx)?;
    let mut spans: Vec<(String, f32, f32)> = Vec::new();
    let mut y = from;
    while y < to {
        hover_at(window, cx, x, y, None)?;
        let hovered = readback(window, cx)?["hover"].as_str().map(str::to_string);
        if let Some(id) = hovered.and_then(|hover| hover.strip_prefix(prefix).map(str::to_string)) {
            match spans.last_mut() {
                Some((last, _, bottom)) if *last == id => *bottom = y,
                _ => spans.push((id, y, y)),
            }
        }
        y += 1.0;
    }
    // Leave hover so revealed controls don't sit over the next capture.
    hover_at(window, cx, WIDTH / 2.0, HEIGHT - 4.0, None)?;
    Ok(spans)
}

fn span<'a>(spans: &'a [(String, f32, f32)], id: &str) -> anyhow::Result<&'a (String, f32, f32)> {
    spans
        .iter()
        .find(|(row, _, _)| row == id)
        .ok_or_else(|| anyhow::anyhow!("row {id} not found in {spans:?}"))
}

/// Press on `from`, travel past GPUI's drag threshold, move to `to` in
/// steps, optionally capture the in-flight drag, then release.
async fn drag(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    from: (f32, f32),
    to: (f32, f32),
    mid_capture: Option<PathBuf>,
) -> anyhow::Result<serde_json::Value> {
    hover_at(window, cx, from.0, from.1, None)?;
    draw(window, cx)?;
    press_at(window, cx, from.0, from.1, true)?;
    hover_at(window, cx, from.0, from.1 + 6.0, Some(MouseButton::Left))?;
    draw(window, cx)?;
    let steps = 12;
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        hover_at(
            window,
            cx,
            from.0 + (to.0 - from.0) * t,
            from.1 + 6.0 + (to.1 - from.1 - 6.0) * t,
            Some(MouseButton::Left),
        )?;
        draw(window, cx)?;
    }
    settle(cx, 80).await;
    let during = readback(window, cx)?;
    if let Some(path) = mid_capture {
        capture(window, cx, path)?;
    }
    press_at(window, cx, to.0, to.1, false)?;
    settle(cx, 450).await;
    draw(window, cx)?;
    Ok(during)
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn ids(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `[(section, id)]` of one related list.
fn sections(readback: &serde_json::Value, list: &str) -> Vec<(String, String)> {
    readback["related"][list]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pair| Some((pair[0].as_str()?.to_string(), pair[1].as_str()?.to_string())))
        .collect()
}

fn section_of(readback: &serde_json::Value, list: &str, id: &str) -> Option<String> {
    sections(readback, list)
        .into_iter()
        .find(|(_, row)| row == id)
        .map(|(section, _)| section)
}

/// Copy the engine's authoritative organization fields into the UI row (the
/// fixture runs without a workspace watch; lineage stays UI-only).
async fn sync_from_engine(
    cx: &mut AsyncApp,
    state: &Entity<state::AppState>,
    workspace: &luna_engine::WorkspaceHost,
    id: &str,
    done: impl Fn(&luna_proto::Chat) -> bool,
) -> anyhow::Result<luna_proto::Chat> {
    for _ in 0..80 {
        if let Some(row) = workspace.chat(id)?
            && done(&row)
        {
            state.update(cx, |state, cx| {
                if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == id) {
                    chat.pinned_order = row.pinned_order.clone();
                    chat.archived = row.archived;
                    chat.settlement_override = row.settlement_override.clone();
                }
                cx.notify();
            });
            return Ok(row);
        }
        settle(cx, 100).await;
    }
    anyhow::bail!("engine never applied the expected change to {id}")
}

/// New side chat through the production creator. The engine's fork row is
/// copied into the UI as the chats watch would publish it (the fixture runs
/// without one), titled `title` through the engine's ordinary rename; the
/// creator then opens it as a Side tab under its source.
async fn fork(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    state: &Entity<state::AppState>,
    workspace: &luna_engine::WorkspaceHost,
    title: &str,
) -> anyhow::Result<String> {
    window.update(cx, |shell, _, cx| shell.fixture_side_chat("new", "", cx))?;
    let mut created = None;
    let mut last = serde_json::Value::Null;
    for _ in 0..150 {
        last = window.update(cx, |shell, _, cx| shell.fixture_side_chat_readback(cx))?;
        anyhow::ensure!(
            last["error"].is_null(),
            "the side chat creator failed: {last}"
        );
        if let Some(id) = last["created"].as_str() {
            created = Some(id.to_string());
            break;
        }
        settle(cx, 100).await;
    }
    let id =
        created.ok_or_else(|| anyhow::anyhow!("New side chat never created a fork: {last}"))?;
    workspace.rename_chat(&id, title)?;
    let row = workspace
        .chat(&id)?
        .ok_or_else(|| anyhow::anyhow!("the engine has no row for fork {id}"))?;
    anyhow::ensure!(
        row.space_id.is_some(),
        "the fork is published with its real project: {row:?}"
    );
    state.update(cx, |state, cx| {
        state.side_chat_rows.insert(
            id.clone(),
            state::SideChatRow {
                parent_id: PARENT.into(),
                state: Some("active".into()),
            },
        );
        state.chats.push(row);
        cx.notify();
    });
    for _ in 0..100 {
        let workbench = window.update(cx, |shell, _, cx| shell.fixture_workbench_readback(cx))?;
        if let Some(tab) = workbench_tab(&workbench, &id) {
            anyhow::ensure!(
                tab["role"] == "Side" && tab["parent"] == PARENT && tab["title"] == title,
                "the fork opens as a Side tab under its source, with its own title: {tab}"
            );
            return Ok(id);
        }
        settle(cx, 100).await;
    }
    anyhow::bail!("fork {id} never opened as a Side tab")
}

fn workbench_tab(workbench: &serde_json::Value, chat: &str) -> Option<serde_json::Value> {
    fn panes(node: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
        if node["kind"] == "pane" {
            out.push(node.clone());
        } else {
            panes(&node["first"], out);
            panes(&node["second"], out);
        }
    }
    let mut all = Vec::new();
    panes(&workbench["root"], &mut all);
    all.into_iter()
        .flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default())
        .find(|tab| tab["chatId"] == chat)
}

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: related-chats-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["readback.json", "related-chats.pass", "failure.txt"] {
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
            EngineProfile::development(&engine_data, "related-fixture", "related-fixture"),
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
    auth.dev_user_id = "related-fixture".into();
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
    let ui_chats = runtime.block_on(async {
        let mut ui_chats = Vec::new();
        for spec in &SPECS {
            core.workspace.create_chat(
                spec.id,
                Some(PROJECT),
                Some(&device),
                Some(config()),
                Some(project.to_string_lossy().into_owned()),
            )?;
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
            chat.archived = spec.archived;
            chat.pinned_order = spec.pin.map(str::to_string);
            core.workspace.import_chat_row(&chat)?;
            chat.child_thread = child_thread(spec, at_ms);
            ui_chats.push(chat);
        }
        anyhow::Ok(ui_chats)
    })?;
    let spaces = core.workspace.read_spaces()?;
    let workspace = core.workspace.clone();
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59997,
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
        settings.confirm_archive = false;
        settings.confirm_settle = false;
        settings.sidebar_auto_settle_after_days = None;
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
        terminal::panel::init(cx);
        let state = cx.new(|cx| {
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
            state.select_chat(Some(PARENT.into()), cx);
            state
        });
        let ui_state = state.clone();
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
        cx.spawn(async move |cx| {
            let state = ui_state;
            let result: anyhow::Result<serde_json::Value> = async {
                let mut record = serde_json::Map::new();
                settle(cx, 900).await;
                let sidebar_x = settings::SIDEBAR_DEFAULT / 2.0;

                // r01. Seeded order: pins A, B, C; then recency.
                capture(window, cx, output.join("r01-sidebar-initial.png"))?;
                let initial = readback(window, cx)?;
                let expected = [PIN_A, PIN_B, PIN_C, PARENT, OPEN_1, OPEN_2, OPEN_3];
                anyhow::ensure!(ids(&initial["sidebarOrder"]) == expected, "seeded sidebar order: {initial}");
                anyhow::ensure!(initial["sidebarSort"] == "LastUpdated", "default sort: {initial}");
                record.insert("initial".into(), initial);

                // A click without travel opens the row; it never drags.
                let spans = row_spans(window, cx, sidebar_x, 40.0, HEIGHT - 40.0, "chat-")?;
                let (_, top, bottom) = span(&spans, OPEN_2)?.clone();
                click_at(window, cx, sidebar_x, (top + bottom) / 2.0).await?;
                anyhow::ensure!(state.read_with(cx, |state, _| state.selected_chat.clone()) == Some(OPEN_2.into()), "click must open the row");
                let clicked = readback(window, cx)?;
                anyhow::ensure!(clicked["drag"].is_null() && clicked["sidebarSort"] == "LastUpdated", "a click must not drag: {clicked}");
                state.update(cx, |state, cx| state.select_chat(Some(PARENT.into()), cx));
                settle(cx, 300).await;

                // r02/r03. Drag pin C above pin A (upper half): mid-drag
                // ghost + line, then C, A, B — optimistic and engine-applied.
                let spans = row_spans(window, cx, sidebar_x, 40.0, HEIGHT - 40.0, "chat-")?;
                let (_, c_top, c_bottom) = span(&spans, PIN_C)?.clone();
                let (_, a_top, _) = span(&spans, PIN_A)?.clone();
                let during = drag(
                    window,
                    cx,
                    (sidebar_x, (c_top + c_bottom) / 2.0),
                    (sidebar_x, a_top + 10.0),
                    Some(output.join("r02-drag-pin-in-flight.png")),
                )
                .await?;
                anyhow::ensure!(
                    during["drag"]["chat"] == PIN_C && during["drag"]["target"] == serde_json::json!([PIN_A, "Before"]),
                    "in-flight drop slot must be before A: {during}"
                );
                capture(window, cx, output.join("r03-pins-reordered.png"))?;
                let pins = readback(window, cx)?;
                anyhow::ensure!(ids(&pins["pinnedOrder"]) == [PIN_C, PIN_A, PIN_B], "pin order after drop: {pins}");
                anyhow::ensure!(pins["drag"].is_null(), "drag state must clear on drop: {pins}");
                let engine_c = sync_from_engine(cx, &state, &workspace, PIN_C, |row| {
                    row.pinned_order.as_deref().is_some_and(|key| luna_proto::compare_pin_order(key, "1").is_lt())
                })
                .await?;
                record.insert("pinsDuring".into(), during);
                record.insert("pins".into(), pins);
                record.insert("enginePinC".into(), serde_json::json!(engine_c.pinned_order));

                // Drag pin A down onto an unpinned row: clamped to the end of
                // the pins, still pinned; the unpinned order is untouched.
                let spans = row_spans(window, cx, sidebar_x, 40.0, HEIGHT - 40.0, "chat-")?;
                let (_, a_top, a_bottom) = span(&spans, PIN_A)?.clone();
                let (_, o2_top, o2_bottom) = span(&spans, OPEN_2)?.clone();
                let during = drag(
                    window,
                    cx,
                    (sidebar_x, (a_top + a_bottom) / 2.0),
                    (sidebar_x, (o2_top + o2_bottom) / 2.0),
                    Some(output.join("r04-drag-pin-clamped.png")),
                )
                .await?;
                anyhow::ensure!(during["drag"]["target"] == serde_json::json!([PIN_B, "After"]), "a pin over unpinned rows must clamp after the last pin: {during}");
                let clamped = readback(window, cx)?;
                anyhow::ensure!(ids(&clamped["pinnedOrder"]) == [PIN_C, PIN_B, PIN_A], "pins after clamped drop: {clamped}");
                anyhow::ensure!(ids(&clamped["sidebarOrder"]) == [PIN_C, PIN_B, PIN_A, PARENT, OPEN_1, OPEN_2, OPEN_3], "unpinned rows must not move: {clamped}");
                anyhow::ensure!(clamped["sidebarSort"] == "LastUpdated", "pin drags never change the sort: {clamped}");
                sync_from_engine(cx, &state, &workspace, PIN_A, |row| {
                    row.pinned_order.as_deref().is_some_and(|key| luna_proto::compare_pin_order(key, "11").is_gt())
                })
                .await?;
                record.insert("pinClamped".into(), clamped);

                // r05. Drag an unpinned row up into the pins: clamped before
                // the first unpinned row, sort becomes Manual, never pinned.
                let spans = row_spans(window, cx, sidebar_x, 40.0, HEIGHT - 40.0, "chat-")?;
                let (_, o3_top, o3_bottom) = span(&spans, OPEN_3)?.clone();
                let (_, c_top, _) = span(&spans, PIN_C)?.clone();
                let during = drag(window, cx, (sidebar_x, (o3_top + o3_bottom) / 2.0), (sidebar_x, c_top + 8.0), None).await?;
                anyhow::ensure!(during["drag"]["target"] == serde_json::json!([PARENT, "Before"]), "unpinned over pins must clamp before the first unpinned row: {during}");
                capture(window, cx, output.join("r05-unpinned-manual.png"))?;
                let manual = readback(window, cx)?;
                anyhow::ensure!(ids(&manual["sidebarOrder"]) == [PIN_C, PIN_B, PIN_A, OPEN_3, PARENT, OPEN_1, OPEN_2], "manual order after drop: {manual}");
                anyhow::ensure!(manual["sidebarSort"] == "Manual", "first unpinned drag switches to Manual: {manual}");
                anyhow::ensure!(ids(&manual["pinnedOrder"]) == [PIN_C, PIN_B, PIN_A], "unpinned drags never pin: {manual}");
                anyhow::ensure!(manual["notice"].as_str().is_some_and(|text| text.contains("sorted manually")), "switch notice: {manual}");
                let saved = ids(&manual["manualOrder"]);
                for id in [OPEN_3, PARENT, OPEN_1, OPEN_2] {
                    anyhow::ensure!(saved.contains(&id.to_string()), "manual order must keep every unpinned task: {manual}");
                }
                record.insert("manual".into(), manual);

                // A second unpinned drag (lower half of Parent → after it).
                let spans = row_spans(window, cx, sidebar_x, 40.0, HEIGHT - 40.0, "chat-")?;
                let (_, o2_top, o2_bottom) = span(&spans, OPEN_2)?.clone();
                let (_, p_top, p_bottom) = span(&spans, PARENT)?.clone();
                drag(window, cx, (sidebar_x, (o2_top + o2_bottom) / 2.0), (sidebar_x, p_bottom - 6.0), None).await?;
                let _ = p_top;
                let manual2 = readback(window, cx)?;
                anyhow::ensure!(ids(&manual2["sidebarOrder"]) == [PIN_C, PIN_B, PIN_A, OPEN_3, PARENT, OPEN_2, OPEN_1], "second manual drop: {manual2}");
                // New activity no longer reorders a manual list.
                state.update(cx, |state, cx| {
                    if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == OPEN_1) {
                        chat.last_message_at = Some(Utc::now());
                    }
                    cx.notify();
                });
                settle(cx, 200).await;
                let activity = readback(window, cx)?;
                anyhow::ensure!(ids(&activity["sidebarOrder"]) == ids(&manual2["sidebarOrder"]), "manual order must survive new activity: {activity}");
                record.insert("manual2".into(), manual2);

                // A drop outside every row: no change, no stale dim/line.
                let spans = row_spans(window, cx, sidebar_x, 40.0, HEIGHT - 40.0, "chat-")?;
                let (_, o1_top, o1_bottom) = span(&spans, OPEN_1)?.clone();
                drag(window, cx, (sidebar_x, (o1_top + o1_bottom) / 2.0), (WIDTH / 2.0, HEIGHT / 2.0), None).await?;
                let outside = readback(window, cx)?;
                anyhow::ensure!(outside["drag"].is_null() && ids(&outside["sidebarOrder"]) == ids(&activity["sidebarOrder"]), "outside drop must be a no-op: {outside}");

                // r06. Surface menu: Side chats and Child chats adjacent.
                window.update(cx, |shell, _, cx| shell.fixture_open_right_pane(cx))?;
                settle(cx, 600).await;
                capture(window, cx, output.join("r06-surface-menu.png"))?;

                // r07. Child chats: sidebar rows in sidebar sections.
                window.update(cx, |shell, _, cx| shell.fixture_open_child_chats(cx))?;
                settle(cx, 500).await;
                capture(window, cx, output.join("r07-child-chats.png"))?;
                let child = readback(window, cx)?;
                anyhow::ensure!(child["rightActive"] == "RelatedChats", "related surface: {child}");
                anyhow::ensure!(
                    sections(&child, "related") == [("Active".to_string(), CHILD_ACTIVE.to_string()), ("Active".into(), CHILD_NEW.into()), ("Active".into(), CHILD_OLD.into()), ("Archived".into(), CHILD_ARCHIVED.into())],
                    "child sections: {child}"
                );
                record.insert("childList".into(), child);

                // Real clicks on the row's hover actions: pin, then settle
                // and archive, each applied by the engine.
                let pane_x = WIDTH - PANE_WIDTH / 2.0;
                let right = WIDTH - 16.0;
                let spans = row_spans(window, cx, pane_x, 80.0, HEIGHT - 40.0, "child-pane-chat-")?;
                let (_, old_top, _) = span(&spans, CHILD_OLD)?.clone();
                // Two-line rows (no branch): the corner sits 22 px down. The
                // corner holds Pin · Settle · Archive, right-aligned 20 px.
                click_at(window, cx, right - 54.0, old_top + 22.0).await?;
                sync_from_engine(cx, &state, &workspace, CHILD_OLD, |row| row.pinned_order.is_some()).await?;
                settle(cx, 300).await;
                draw(window, cx)?;
                let pinned_child = readback(window, cx)?;
                anyhow::ensure!(section_of(&pinned_child, "related", CHILD_OLD).as_deref() == Some("Pinned"), "pinned child section: {pinned_child}");
                capture(window, cx, output.join("r08-child-pinned.png"))?;

                let spans = row_spans(window, cx, pane_x, 80.0, HEIGHT - 40.0, "child-pane-chat-")?;
                let (_, new_top, _) = span(&spans, CHILD_NEW)?.clone();
                click_at(window, cx, right - 32.0, new_top + 22.0).await?;
                sync_from_engine(cx, &state, &workspace, CHILD_NEW, |row| row.settlement_override.is_some()).await?;
                settle(cx, 300).await;
                let settled_child = readback(window, cx)?;
                anyhow::ensure!(section_of(&settled_child, "related", CHILD_NEW).as_deref() == Some("Settled"), "settled child section: {settled_child}");
                capture(window, cx, output.join("r09-child-settled.png"))?;

                let spans = row_spans(window, cx, pane_x, 80.0, HEIGHT - 40.0, "child-pane-chat-")?;
                let (_, old_top, _) = span(&spans, CHILD_OLD)?.clone();
                click_at(window, cx, right - 10.0, old_top + 22.0).await?;
                sync_from_engine(cx, &state, &workspace, CHILD_OLD, |row| row.archived).await?;
                settle(cx, 300).await;
                let archived_child = readback(window, cx)?;
                anyhow::ensure!(section_of(&archived_child, "related", CHILD_OLD).as_deref() == Some("Archived"), "archived child section: {archived_child}");
                record.insert("childActions".into(), archived_child);

                // r10. Back (a real click) shows the surface menu with the
                // tab still open; the tab returns to the list.
                click_at(window, cx, WIDTH - PANE_WIDTH + 20.0, theme::Theme::TITLEBAR_HEIGHT * 1.5).await?;
                settle(cx, 200).await;
                capture(window, cx, output.join("r10-back-to-menu.png"))?;
                let back = readback(window, cx)?;
                anyhow::ensure!(back["rightActive"] == "Picker" && back["pickerOpen"] == true, "Back must show the surface menu: {back}");
                anyhow::ensure!(ids(&back["rightTabs"]).contains(&"RelatedChats".to_string()), "Back must keep the tab: {back}");
                window.update(cx, |shell, _, cx| shell.fixture_open_child_chats(cx))?;
                settle(cx, 200).await;
                let returned = readback(window, cx)?;
                anyhow::ensure!(returned["rightActive"] == "RelatedChats" && returned["pickerOpen"] == false, "choosing the surface again returns: {returned}");
                record.insert("back".into(), back);

                // r11-r14. Side chats: two real forks, each a Side tab with
                // its own title, listed with the children; then pin, settle
                // and archive by real clicks on their ordinary rows, applied
                // by the engine (no device-local marks).
                window.update(cx, |shell, _, cx| shell.fixture_open_side_chats(cx))?;
                settle(cx, 400).await;
                let first = fork(window, cx, &state, &workspace, SIDE_Q1).await?;
                let second = fork(window, cx, &state, &workspace, SIDE_Q2).await?;
                window.update(cx, |shell, _, cx| shell.fixture_open_child_chats(cx))?;
                settle(cx, 300).await;
                draw(window, cx)?;
                capture(window, cx, output.join("r11-mixed-related-list.png"))?;
                let side = window.update(cx, |shell, _, cx| shell.fixture_side_chat_readback(cx))?;
                let list = readback(window, cx)?;
                anyhow::ensure!(
                    section_of(&list, "related", &first).as_deref() == Some("Active")
                        && section_of(&list, "related", &second).as_deref() == Some("Active")
                        && section_of(&list, "related", CHILD_ACTIVE).as_deref() == Some("Active"),
                    "active child and side chats must list together: {list}"
                );
                // Non-archived: two children (one settled) and two side chats.
                anyhow::ensure!(list["relatedCounts"][PARENT] == 4, "combined related count: {list}");
                record.insert("mixedList".into(), list.clone());
                record.insert("sideList".into(), side);

                // Fork rows are ordinary related rows (two lines, no branch):
                // the corner sits 22 px down, as on a child's.
                let spans = row_spans(window, cx, pane_x, 80.0, HEIGHT - 40.0, "child-pane-chat-")?;
                let (_, first_top, _) = span(&spans, &first)?.clone();
                click_at(window, cx, right - 54.0, first_top + 22.0).await?;
                sync_from_engine(cx, &state, &workspace, &first, |row| row.pinned_order.is_some()).await?;
                settle(cx, 200).await;
                let spans = row_spans(window, cx, pane_x, 80.0, HEIGHT - 40.0, "child-pane-chat-")?;
                let (_, second_top, _) = span(&spans, &second)?.clone();
                click_at(window, cx, right - 32.0, second_top + 22.0).await?;
                sync_from_engine(cx, &state, &workspace, &second, |row| row.settlement_override.is_some()).await?;
                settle(cx, 200).await;
                capture(window, cx, output.join("r12-side-pinned-settled.png"))?;
                let marked = readback(window, cx)?;
                anyhow::ensure!(section_of(&marked, "related", &first).as_deref() == Some("Pinned"), "side pin: {marked}");
                anyhow::ensure!(section_of(&marked, "related", &second).as_deref() == Some("Settled"), "side settle: {marked}");

                let spans = row_spans(window, cx, pane_x, 80.0, HEIGHT - 40.0, "child-pane-chat-")?;
                let (_, first_top, _) = span(&spans, &first)?.clone();
                click_at(window, cx, right - 10.0, first_top + 22.0).await?;
                sync_from_engine(cx, &state, &workspace, &first, |row| row.archived).await?;
                settle(cx, 200).await;
                let archived_side = readback(window, cx)?;
                anyhow::ensure!(section_of(&archived_side, "related", &first).as_deref() == Some("Archived"), "side archive: {archived_side}");
                anyhow::ensure!(archived_side["sideMarks"].as_object().is_none_or(|marks| marks.is_empty()), "fork actions write the engine's flags, never device-local marks: {archived_side}");
                capture(window, cx, output.join("r13-side-archived.png"))?;
                record.insert("sideMarks".into(), archived_side);

                // Back from Side chats too; both related tabs stay open.
                click_at(window, cx, WIDTH - PANE_WIDTH + 20.0, theme::Theme::TITLEBAR_HEIGHT * 1.5).await?;
                settle(cx, 200).await;
                capture(window, cx, output.join("r14-side-back-to-menu.png"))?;
                let side_back = readback(window, cx)?;
                anyhow::ensure!(side_back["rightActive"] == "Picker", "side Back: {side_back}");
                let tabs = ids(&side_back["rightTabs"]);
                anyhow::ensure!(tabs == ["RelatedChats".to_string()], "one Related chats tab, kept: {side_back}");
                record.insert("sideBack".into(), side_back);

                // r15. Final mixed list: an active child and a new active side
                // chat together, and the sidebar's combined count on the parent.
                window.update(cx, |shell, _, cx| shell.fixture_open_child_chats(cx))?;
                settle(cx, 300).await;
                let third = fork(window, cx, &state, &workspace, SIDE_Q3).await?;
                window.update(cx, |shell, _, cx| shell.fixture_open_child_chats(cx))?;
                settle(cx, 300).await;
                hover_at(window, cx, WIDTH / 2.0, HEIGHT - 4.0, None)?;
                settle(cx, 400).await;
                capture(window, cx, output.join("r15-final-mixed-list.png"))?;
                let mixed = readback(window, cx)?;
                anyhow::ensure!(
                    section_of(&mixed, "related", CHILD_ACTIVE).as_deref() == Some("Active")
                        && section_of(&mixed, "related", &third).as_deref() == Some("Active"),
                    "final list must show an active child and an active side chat: {mixed}"
                );
                // Children: active + settled; side chats: settled + new; the
                // archived side chat and children are not counted.
                anyhow::ensure!(mixed["relatedCounts"][PARENT] == 4, "final combined count: {mixed}");
                record.insert("finalMixed".into(), mixed);
                Ok(serde_json::Value::Object(record))
            }
            .await;
            match result {
                Ok(record) => {
                    let written = std::fs::write(output.join("readback.json"), serde_json::to_vec_pretty(&record).unwrap_or_default())
                        .and_then(|_| {
                            std::fs::write(
                                output.join("related-chats.pass"),
                                "PASS: headless production Shell with real pointer input; one Related chats list shows active child and side chats together with the combined count; click opens without dragging; pin drag reorders pins (optimistic + engine-applied bounds); a pin dragged over unpinned rows clamps to the end of the pins; an unpinned drag into the pins clamps below them, switches to Manual sort and keeps every unpinned task; manual order survives new activity; an outside drop is a no-op; Child chats and Side chats render sidebar task rows in Pinned/Active/Settled/Archived sections; hover Pin/Settle/Archive applied by the engine for children and forks alike (no device-local marks); forks from New side chat open as Side tabs under their source with their own chat titles; Back shows the surface menu and keeps the tabs\n",
                            )
                        });
                    if let Err(error) = written {
                        *failure_for_ui.lock().unwrap() = Some(error.to_string());
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                        // AppKit's quit can end the process with status 0 before `run`
                        // returns: leave with the failure status now.
                        std::process::exit(1);
                    }
                }
                Err(error) => {
                    eprintln!("Related chats fixture failed: {error:#}");
                    let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                    let _ = capture(window, cx, output.join("failure.png"));
                    *failure_for_ui.lock().unwrap() = Some(error.to_string());
                    // AppKit's quit can end the process with status 0 before `run`
                    // returns: leave with the failure status now.
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
