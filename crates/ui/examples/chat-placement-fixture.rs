//! Headless GPUI E2E for explicit sidebar placement: children and forks begin
//! in Related chats, enter the left sidebar only after a drag, and render as
//! aligned cards. Ordinary chats can still dock alongside and return.
//!
//! Drives the production `Shell` in a hidden window with real pointer input
//! (GPUI hit testing, drag and drop, clicks) against an in-process
//! `EngineCore` with a Mock harness. Rows are located black-box by sweeping
//! the pointer and reading which row reports hover. Child lineage lives in
//! the UI rows only (the engine never starts or sweeps them). Settings are
//! flushed, read back from disk, and loaded into a fresh `Shell` window.
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. A child dragged into the sidebar changes its parent, gets pinned, or is
//!    still listed in its parent's Related chats (a duplicate row).
//! 2. A drop on the Pinned group pins a child, or ignores the pin boundary.
//! 3. A chat docked alongside gains a parent, is labelled Child, stays in the
//!    sidebar, or opens without its own composer.
//! 4. Self, duplicate, ancestor and pinned-child drops change placement; a
//!    dock closes a cycle through docks and parents (A<->B, longer, mixed)
//!    or lands on an archived host; a saved cycle hides chats for good.
//! 5. A child or fork (including pinned/orphaned children) appears in any
//!    sidebar section before a manual drag; selecting one reveals it there.
//! 6. A fork is refused the sidebar or a dock, loses its source when placed,
//!    gains a parent, or cannot return from a dock.
//! 7. A placed child is indented or loses its Child identity; a breadcrumb
//!    names a dock host, cannot open the actual (or archived) parent, or links
//!    a missing parent.
//! 8. Placement does not survive closing the pane or a settings reload, or
//!    leaks to (or is pruned by) another profile on the same device and
//!    scope; an engine with no profile id applies, stores or prunes any.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example chat-placement-fixture
//! Run:   target/debug/examples/chat-placement-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes p01..p12 PNG captures, readback.json and chat-placement.pass; a
//! failure writes failure.txt/failure.png and exits nonzero.
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
use luna_proto::{
    AgentEvent, ChatConfig, ChildThread, DoneStatus, HarnessId, PermissionMode, SandboxLevel,
};
use luna_ui::*;

const PROJECT: &str = "placement-project";
const PARENT: &str = "f0f0f0f0-0000-4000-8000-000000000001";
const OTHER: &str = "f0f0f0f0-0000-4000-8000-000000000002";
const PINNED_TOP: &str = "f0f0f0f0-0000-4000-8000-000000000003";
const ARCH_PARENT: &str = "f0f0f0f0-0000-4000-8000-000000000004";
const THIRD: &str = "f0f0f0f0-0000-4000-8000-000000000005";
const CHILD_A: &str = "f1f1f1f1-0000-4000-8000-000000000001";
const CHILD_B: &str = "f1f1f1f1-0000-4000-8000-000000000002";
const CHILD_PINNED: &str = "f1f1f1f1-0000-4000-8000-000000000003";
const ORPHAN: &str = "f1f1f1f1-0000-4000-8000-000000000004";
const ARCH_CHILD: &str = "f1f1f1f1-0000-4000-8000-000000000005";
const MISSING_PARENT: &str = "deleted-parent-0000";
const PARENT_TITLE: &str =
    "Release plan for the v0.6 cut: cache migration, settings rename and the UI flag rollout";
const SIDE_Q: &str = "Is the cache migration safe to land first?";
const MOCK_ANSWER: &str = "Yes; it only adds keys.";
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

const SPECS: [Spec; 10] = [
    Spec {
        id: PARENT,
        title: PARENT_TITLE,
        minutes_ago: 1,
        pin: None,
        parent: None,
        archived: false,
    },
    Spec {
        id: OTHER,
        title: "Docs cleanup",
        minutes_ago: 4,
        pin: None,
        parent: None,
        archived: false,
    },
    Spec {
        id: PINNED_TOP,
        title: "Roadmap",
        minutes_ago: 10,
        pin: Some("1"),
        parent: None,
        archived: false,
    },
    Spec {
        id: ARCH_PARENT,
        title: "Archived spike",
        minutes_ago: 30,
        pin: None,
        parent: None,
        archived: true,
    },
    Spec {
        id: CHILD_A,
        title: "Benchmark the cache",
        minutes_ago: 2,
        pin: None,
        parent: Some(PARENT),
        archived: false,
    },
    Spec {
        id: CHILD_B,
        title: "Draft release notes",
        minutes_ago: 3,
        pin: None,
        parent: Some(PARENT),
        archived: false,
    },
    Spec {
        id: CHILD_PINNED,
        title: "Pinned audit",
        minutes_ago: 5,
        pin: Some("11"),
        parent: Some(PARENT),
        archived: false,
    },
    Spec {
        id: ORPHAN,
        title: "Investigate eviction",
        minutes_ago: 6,
        pin: None,
        parent: Some(MISSING_PARENT),
        archived: false,
    },
    Spec {
        id: ARCH_CHILD,
        title: "Spike follow-up",
        minutes_ago: 7,
        pin: None,
        parent: Some(ARCH_PARENT),
        archived: false,
    },
    Spec {
        id: THIRD,
        title: "Third chat",
        minutes_ago: 8,
        pin: None,
        parent: None,
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
        work_origin: None,
    })
}

type Window = WindowHandle<shell::Shell>;

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

fn press_at(window: Window, cx: &mut AsyncApp, x: f32, y: f32, down: bool) -> anyhow::Result<()> {
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

async fn click_at(window: Window, cx: &mut AsyncApp, x: f32, y: f32) -> anyhow::Result<()> {
    hover_at(window, cx, x, y, None)?;
    draw(window, cx)?;
    settle(cx, 60).await;
    draw(window, cx)?;
    press_at(window, cx, x, y, true)?;
    press_at(window, cx, x, y, false)?;
    settle(cx, 200).await;
    Ok(())
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn readback(window: Window, cx: &mut AsyncApp) -> anyhow::Result<serde_json::Value> {
    window.update(cx, |shell, _, cx| shell.fixture_related_readback(cx))
}

/// Black-box row geometry: sweep a column and record rows reporting hover.
fn row_spans(
    window: Window,
    cx: &mut AsyncApp,
    x: f32,
    prefix: &str,
) -> anyhow::Result<Vec<(String, f32, f32)>> {
    draw(window, cx)?;
    let mut spans: Vec<(String, f32, f32)> = Vec::new();
    let mut y = 40.0;
    while y < HEIGHT - 40.0 {
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
    hover_at(window, cx, WIDTH / 2.0, HEIGHT - 4.0, None)?;
    Ok(spans)
}

fn center(spans: &[(String, f32, f32)], id: &str) -> anyhow::Result<f32> {
    spans
        .iter()
        .find(|(row, _, _)| row == id)
        .map(|(_, top, bottom)| (top + bottom) / 2.0)
        .ok_or_else(|| anyhow::anyhow!("row {id} not found in {spans:?}"))
}

fn lowest(spans: &[(String, f32, f32)]) -> f32 {
    spans
        .iter()
        .map(|(_, _, bottom)| *bottom)
        .fold(0.0, f32::max)
}

/// Press, travel past the drag threshold, move in steps, release.
async fn drag(
    window: Window,
    cx: &mut AsyncApp,
    from: (f32, f32),
    to: (f32, f32),
) -> anyhow::Result<()> {
    hover_at(window, cx, from.0, from.1, None)?;
    draw(window, cx)?;
    press_at(window, cx, from.0, from.1, true)?;
    hover_at(window, cx, from.0, from.1 + 6.0, Some(MouseButton::Left))?;
    draw(window, cx)?;
    for step in 1..=14 {
        let t = step as f32 / 14.0;
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
    press_at(window, cx, to.0, to.1, false)?;
    settle(cx, 450).await;
    draw(window, cx)?;
    Ok(())
}

/// Select `id` as the main chat and show its Related chats.
async fn focus_related(
    window: Window,
    cx: &mut AsyncApp,
    state: &gpui::Entity<state::AppState>,
    id: &str,
) -> anyhow::Result<()> {
    let id = id.to_string();
    cx.update(|cx| state.update(cx, |state, cx| state.select_chat(Some(id), cx)));
    settle(cx, 400).await;
    window.update(cx, |shell, _, cx| shell.fixture_open_child_chats(cx))?;
    settle(cx, 500).await;
    draw(window, cx)?;
    Ok(())
}

/// Drag a sidebar row onto the selected chat's Related chats; returns the
/// readback after the drop.
async fn drop_on_related(
    window: Window,
    cx: &mut AsyncApp,
    id: &str,
) -> anyhow::Result<serde_json::Value> {
    let sidebar_x = settings::SIDEBAR_DEFAULT / 2.0;
    let left = row_spans(window, cx, sidebar_x, "chat-")?;
    drag(
        window,
        cx,
        (sidebar_x, center(&left, id)?),
        (WIDTH - PANE_WIDTH / 2.0, HEIGHT - 120.0),
    )
    .await?;
    readback(window, cx)
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

fn related_ids(readback: &serde_json::Value) -> Vec<String> {
    readback["related"]["related"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pair| pair[1].as_str().map(str::to_string))
        .collect()
}

fn has(list: &[String], id: &str) -> bool {
    list.iter().any(|item| item == id)
}

/// New side chat through the production creator. The engine's fork row is
/// copied into the UI as the chats watch would publish it (the fixture runs
/// without one), titled through the engine's ordinary rename.
async fn fork(
    window: Window,
    cx: &mut AsyncApp,
    state: &gpui::Entity<state::AppState>,
    workspace: &luna_engine::WorkspaceHost,
    title: &str,
) -> anyhow::Result<String> {
    window.update(cx, |shell, _, cx| shell.fixture_side_chat("new", "", cx))?;
    let mut last = serde_json::Value::Null;
    for _ in 0..150 {
        last = window.update(cx, |shell, _, cx| shell.fixture_side_chat_readback(cx))?;
        anyhow::ensure!(
            last["error"].is_null(),
            "the side chat creator failed: {last}"
        );
        if let Some(id) = last["created"].as_str().map(str::to_string) {
            workspace.rename_chat(&id, title)?;
            let row = workspace
                .chat(&id)?
                .ok_or_else(|| anyhow::anyhow!("the engine has no row for fork {id}"))?;
            anyhow::ensure!(
                row.space_id.is_some(),
                "the fork is published with its real project: {row:?}"
            );
            let source = last["parent"].as_str().unwrap_or_default().to_string();
            cx.update(|cx| {
                state.update(cx, |state, cx| {
                    state.side_chat_rows.insert(
                        id.clone(),
                        state::SideChatRow {
                            parent_id: source,
                            state: Some("active".into()),
                        },
                    );
                    state.chats.push(row);
                    cx.notify();
                })
            });
            settle(cx, 600).await;
            return Ok(id);
        }
        settle(cx, 100).await;
    }
    anyhow::bail!("New side chat never created a fork: {last}")
}

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: chat-placement-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in [
        "readback.json",
        "chat-placement.pass",
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
            EngineProfile::development(&engine_data, "placement-fixture", "placement-fixture"),
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
    auth.dev_user_id = "placement-fixture".into();
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
    let handle_a = handle.clone();
    // Other profiles on the SAME device and scope, told apart only by their
    // profile id (as account profiles under one device root are), and an
    // engine that names no profile (older engines, pre-profile bootstrap).
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
            format!("memory://placement-{label}"),
        )
    };
    let handle_b = profile_handle(Some("fixture-profile-b"), "b");
    let handle_none = profile_handle(None, "none");
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59996,
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
        let open = |cx: &mut gpui::App, state: gpui::Entity<state::AppState>, boot: EngineBootConfig| {
            cx.open_window(
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
                |_, cx| cx.new(|cx| shell::Shell::new(state, boot, cx)),
            )
            .unwrap()
        };
        let mut window = open(cx, state.clone(), boot.clone());
        let ui_state = state.clone();
        cx.spawn(async move |cx| {
            let state = ui_state;
            let result: anyhow::Result<serde_json::Value> = async {
                let mut record = serde_json::Map::new();
                settle(cx, 900).await;
                let sidebar_x = settings::SIDEBAR_DEFAULT / 2.0;
                let pane_x = WIDTH - PANE_WIDTH / 2.0;
                let pane_drop = (pane_x, HEIGHT - 120.0);
                let select = |cx: &mut AsyncApp, id: &str| {
                    let id = id.to_string();
                    cx.update(|cx| state.update(cx, |state, cx| state.select_chat(Some(id), cx)));
                };

                // p01. Every parent-created chat begins outside the sidebar,
                // including pinned and orphaned children. All remain in the
                // parent's Related chats when that parent exists.
                window.update(cx, |shell, _, cx| {
                    shell.fixture_open_right_pane(cx);
                    shell.fixture_open_child_chats(cx);
                })?;
                settle(cx, 800).await;
                capture(window, cx, output.join("p01-initial.png"))?;
                let initial = readback(window, cx)?;
                anyhow::ensure!(
                    ids(&initial["sidebarOrder"]) == [PINNED_TOP, PARENT, OTHER, THIRD],
                    "seeded sidebar: {initial}"
                );
                anyhow::ensure!(ids(&initial["sidebarChildRows"]).is_empty(), "unplaced children must have no sidebar rows: {initial}");
                let related = related_ids(&initial);
                anyhow::ensure!(has(&related, CHILD_A) && has(&related, CHILD_B) && has(&related, CHILD_PINNED), "children list under their parent: {initial}");
                select(cx, CHILD_A);
                settle(cx, 300).await;
                let selected_child = readback(window, cx)?;
                anyhow::ensure!(!has(&ids(&selected_child["sidebarOrder"]), CHILD_A), "opening a child must not reveal it in the sidebar: {selected_child}");
                select(cx, PARENT);
                settle(cx, 300).await;
                let lineage = initial["lineage"].clone();
                record.insert("initial".into(), initial);

                // p02. Child → sidebar (dropped below the rows, no slot):
                // listed there, gone from Related chats, still the parent's
                // child, not pinned, sort unchanged.
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (pane_x, center(&right, CHILD_A)?), (sidebar_x, lowest(&left) + 40.0)).await?;
                capture(window, cx, output.join("p02-child-to-sidebar.png"))?;
                let promoted = readback(window, cx)?;
                anyhow::ensure!(has(&ids(&promoted["sidebarOrder"]), CHILD_A), "promoted child missing from the sidebar: {promoted}");
                anyhow::ensure!(!has(&related_ids(&promoted), CHILD_A), "promoted child still in Related chats: {promoted}");
                anyhow::ensure!(has(&ids(&promoted["sidebarChildRows"]), CHILD_A), "promoted child must be marked: {promoted}");
                anyhow::ensure!(promoted["lineage"] == lineage, "placement changed lineage: {promoted}");
                anyhow::ensure!(promoted["sidebarSort"] == "LastUpdated" && !has(&ids(&promoted["pinnedOrder"]), CHILD_A), "no slot: no sort change, never pinned: {promoted}");
                record.insert("promoted".into(), promoted);

                // p03. Child dropped on the Pinned group: clamped below the
                // pins (never pinned) at the top of the unpinned rows.
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (pane_x, center(&right, CHILD_B)?), (sidebar_x, center(&left, PINNED_TOP)? - 8.0)).await?;
                capture(window, cx, output.join("p03-pin-boundary.png"))?;
                let boundary = readback(window, cx)?;
                let order = ids(&boundary["sidebarOrder"]);
                anyhow::ensure!(order.iter().position(|id| id == CHILD_B) == Some(1), "child must land first below the pins: {boundary}");
                anyhow::ensure!(!has(&ids(&boundary["pinnedOrder"]), CHILD_B) && boundary["sidebarSort"] == "Manual", "a slot drop sorts manually and never pins: {boundary}");
                record.insert("pinBoundary".into(), boundary);

                // Self drop: the selected chat onto its own Related chats.
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                let before = readback(window, cx)?["placements"].clone();
                drag(window, cx, (sidebar_x, center(&left, PARENT)?), pane_drop).await?;
                let self_drop = readback(window, cx)?;
                anyhow::ensure!(self_drop["placements"] == before && self_drop["notice"].as_str().is_some_and(|text| text.contains("this chat")), "self drop must be refused: {self_drop}");

                // p04. Ordinary chat → alongside: gone from the sidebar, listed
                // in Related chats as Alongside, with no parent.
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (sidebar_x, center(&left, OTHER)?), pane_drop).await?;
                capture(window, cx, output.join("p04-docked-alongside.png"))?;
                let docked = readback(window, cx)?;
                anyhow::ensure!(!has(&ids(&docked["sidebarOrder"]), OTHER) && has(&related_ids(&docked), OTHER), "docked chat must move to Related chats: {docked}");
                anyhow::ensure!(docked["placements"]["docked"][OTHER] == PARENT && docked["lineage"][OTHER].is_null(), "docking must not invent a parent: {docked}");
                record.insert("docked".into(), docked);

                // Duplicate drop: the alongside row back onto its own list.
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                drag(window, cx, (pane_x, center(&right, OTHER)?), pane_drop).await?;
                let duplicate = readback(window, cx)?;
                anyhow::ensure!(duplicate["placements"]["docked"].as_object().map(|docked| docked.len()) == Some(1) && has(&related_ids(&duplicate), OTHER), "a duplicate drop must change nothing: {duplicate}");

                // p05. The alongside chat opens with its own composer, as
                // Alongside (never Child), naming no parent.
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                click_at(window, cx, pane_x - 60.0, center(&right, OTHER)?).await?;
                settle(cx, 500).await;
                capture(window, cx, output.join("p05-alongside-open.png"))?;
                let opened = readback(window, cx)?;
                let child = window.update(cx, |shell, _, cx| shell.fixture_child_chats_readback(cx))?;
                let header = window.update(cx, |shell, _, cx| shell.fixture_workbench_strip(OTHER, cx))?;
                anyhow::ensure!(header["identity"] == "Alongside" && header["parentLinked"] == false
                    && child["paneComposerStates"][OTHER]["boundChat"] == OTHER,
                    "alongside chat opens in the workbench as itself: {header} {child}");
                record.insert("alongsideOpened".into(), serde_json::json!({"placement":opened,"header":header,"composer":child["paneComposerStates"][OTHER]}));
                anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_workbench_close(OTHER, cx))?, "close the alongside view without changing placement");
                settle(cx, 300).await;

                // Even a pinned child needs a manual drag into the sidebar;
                // dragging it back removes that placement without unpinning.
                focus_related(window, cx, &state, PARENT).await?;
                let before = readback(window, cx)?["placements"].clone();
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (pane_x, center(&right, CHILD_PINNED)?), (sidebar_x, lowest(&left) + 40.0)).await?;
                let manually_pinned = readback(window, cx)?;
                anyhow::ensure!(has(&ids(&manually_pinned["sidebarOrder"]), CHILD_PINNED) && has(&ids(&manually_pinned["placements"]["promoted"]), CHILD_PINNED), "a pinned child needs explicit placement: {manually_pinned}");
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (sidebar_x, center(&left, CHILD_PINNED)?), pane_drop).await?;
                let pinned_child = readback(window, cx)?;
                anyhow::ensure!(pinned_child["placements"] == before && !has(&ids(&pinned_child["sidebarOrder"]), CHILD_PINNED) && has(&related_ids(&pinned_child), CHILD_PINNED), "a pinned child returns to Related chats: {pinned_child}");

                // A promoted child returns to its parent's Related chats.
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (sidebar_x, center(&left, CHILD_A)?), pane_drop).await?;
                let returned = readback(window, cx)?;
                anyhow::ensure!(!has(&ids(&returned["sidebarOrder"]), CHILD_A) && has(&related_ids(&returned), CHILD_A), "child must return to Related chats: {returned}");
                anyhow::ensure!(ids(&returned["placements"]["promoted"]) == [CHILD_B], "only the other child stays promoted: {returned}");

                // Cycles through docks and parents are refused, unchanged.
                // A <-> B: Other is docked beside Parent; Parent beside Other.
                focus_related(window, cx, &state, OTHER).await?;
                let before = readback(window, cx)?["placements"].clone();
                let two = drop_on_related(window, cx, PARENT).await?;
                anyhow::ensure!(two["placements"] == before && two["notice"].as_str().is_some_and(|text| text.contains("hide both")), "A<->B docking must be refused: {two}");
                // Longer: Third beside Other (allowed), then Parent beside
                // Third (Third -> Other -> Parent) is refused.
                let chained = drop_on_related(window, cx, THIRD).await?;
                anyhow::ensure!(chained["placements"]["docked"][THIRD] == OTHER, "a chain without a cycle is allowed: {chained}");
                focus_related(window, cx, &state, THIRD).await?;
                let before = readback(window, cx)?["placements"].clone();
                let longer = drop_on_related(window, cx, PARENT).await?;
                anyhow::ensure!(longer["placements"] == before, "a longer docking cycle must be refused: {longer}");
                // Undock Third (its row sits in Other's Related chats).
                focus_related(window, cx, &state, OTHER).await?;
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (pane_x, center(&right, THIRD)?), (sidebar_x, lowest(&left) + 40.0)).await?;
                // Mixed: Parent beside Third (allowed); Child A (a child of
                // Parent) then cannot host Third: Child A -> Parent -> Third.
                focus_related(window, cx, &state, THIRD).await?;
                let parent_docked = drop_on_related(window, cx, PARENT).await?;
                anyhow::ensure!(parent_docked["placements"]["docked"][PARENT] == THIRD && parent_docked["placements"]["docked"].get(THIRD).is_none(), "Parent beside Third: {parent_docked}");
                focus_related(window, cx, &state, CHILD_A).await?;
                let before = readback(window, cx)?["placements"].clone();
                let mixed = drop_on_related(window, cx, THIRD).await?;
                anyhow::ensure!(mixed["placements"] == before, "a dock/lineage cycle must be refused: {mixed}");
                capture(window, cx, output.join("p05b-cycle-refused.png"))?;
                // Undock Parent (its row sits in Third's Related chats).
                focus_related(window, cx, &state, THIRD).await?;
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (pane_x, center(&right, PARENT)?), (sidebar_x, lowest(&left) + 40.0)).await?;
                let restored = readback(window, cx)?;
                anyhow::ensure!(restored["placements"]["docked"].as_object().map(|docked| docked.len()) == Some(1) && restored["placements"]["docked"][OTHER] == PARENT, "cycle checks leave only Other beside Parent: {restored}");
                record.insert("cycles".into(), serde_json::json!({ "twoWay": two, "longer": longer, "mixed": mixed }));
                focus_related(window, cx, &state, PARENT).await?;

                // p06. A fork starts in Related chats alone. Dragging it to
                // the sidebar saves placement while retaining the source;
                // docking it beside another chat and returning also works.
                window.update(cx, |shell, _, cx| shell.fixture_open_side_chats(cx))?;
                let side_id = fork(window, cx, &state, &workspace, SIDE_Q).await?;
                focus_related(window, cx, &state, PARENT).await?;
                let listed = readback(window, cx)?;
                anyhow::ensure!(has(&related_ids(&listed), &side_id) && !has(&ids(&listed["sidebarOrder"]), &side_id), "an unplaced fork lists only under its source: {listed}");
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                let before = listed["placements"].clone();
                drag(window, cx, (pane_x, center(&right, &side_id)?), (sidebar_x, lowest(&left) + 40.0)).await?;
                capture(window, cx, output.join("p06-fork-placed.png"))?;
                let placed = readback(window, cx)?;
                anyhow::ensure!(!placed["notice"].as_str().is_some_and(|text| text.contains("stay with")), "a fork is not refused the sidebar: {placed}");
                anyhow::ensure!(placed["placements"]["docked"] == before["docked"] && has(&ids(&placed["placements"]["promoted"]), &side_id), "placing a fork saves explicit promotion: {placed}");
                anyhow::ensure!(has(&ids(&placed["sidebarOrder"]), &side_id) && has(&related_ids(&placed), &side_id), "it stays a sidebar row and under its source: {placed}");
                anyhow::ensure!(placed["lineage"].get(side_id.as_str()).is_none(), "a fork gains no parent: {placed}");
                // Dock it beside Other (allowed: a fork has no lineage), then
                // return it to the sidebar.
                focus_related(window, cx, &state, OTHER).await?;
                let docked = drop_on_related(window, cx, &side_id).await?;
                anyhow::ensure!(docked["placements"]["docked"][side_id.as_str()] == OTHER && has(&related_ids(&docked), &side_id), "a fork docks beside another chat: {docked}");
                capture(window, cx, output.join("p06b-fork-docked.png"))?;
                focus_related(window, cx, &state, PARENT).await?;
                anyhow::ensure!(has(&related_ids(&readback(window, cx)?), &side_id), "a docked fork stays listed under its source");
                focus_related(window, cx, &state, OTHER).await?;
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (pane_x, center(&right, &side_id)?), (sidebar_x, lowest(&left) + 40.0)).await?;
                let undocked = readback(window, cx)?;
                anyhow::ensure!(undocked["placements"]["docked"] == before["docked"], "the fork returns from the dock: {undocked}");
                record.insert("fork".into(), serde_json::json!({ "placed": placed, "docked": docked["placements"], "undocked": undocked["placements"] }));

                // Replay a complete WatchChats frame as on cold load or
                // reconnect. The source map and ordinary chat rows arrive
                // atomically; an explicit placement survives, then removing
                // it leaves the fork only in Related chats.
                let (frame_chats, side_rows, fork_sources) = state.read_with(cx, |state, _| (
                    state.chats.clone(),
                    state.side_chat_rows.clone(),
                    state.fork_source_rows.clone(),
                ));
                cx.update(|cx| state.update(cx, |state, cx| {
                    state.apply_side_chat_rows(side_rows.clone());
                    state.fork_source_rows = fork_sources.clone();
                    state.apply_chats(frame_chats.clone());
                    cx.notify();
                }));
                settle(cx, 300).await;
                let restored_fork = readback(window, cx)?;
                anyhow::ensure!(has(&ids(&restored_fork["sidebarOrder"]), &side_id), "saved fork placement survives a watch replay: {restored_fork}");
                window.update(cx, |shell, _, cx| {
                    shell.fixture_set_chat_placements(serde_json::from_value(before.clone()).expect("saved placements"), cx)
                })?;
                cx.update(|cx| state.update(cx, |state, cx| {
                    state.apply_side_chat_rows(side_rows);
                    state.fork_source_rows = fork_sources;
                    state.apply_chats(frame_chats);
                    cx.notify();
                }));
                settle(cx, 300).await;
                focus_related(window, cx, &state, PARENT).await?;
                let unplaced_fork = readback(window, cx)?;
                anyhow::ensure!(!has(&ids(&unplaced_fork["sidebarOrder"]), &side_id) && has(&related_ids(&unplaced_fork), &side_id), "a cold frame without saved placement keeps the fork in Related chats only: {unplaced_fork}");
                record.insert("watchReplay".into(), serde_json::json!({"placed": restored_fork, "unplaced": unplaced_fork}));

                // p07-p08. A promoted child opened as main: the breadcrumb
                // names its actual parent (long title) and opens it.
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                click_at(window, cx, sidebar_x, center(&left, CHILD_B)?).await?;
                settle(cx, 400).await;
                capture(window, cx, output.join("p07-child-breadcrumb.png"))?;
                anyhow::ensure!(readback(window, cx)?["selected"] == CHILD_B, "opening the promoted child");
                click_at(window, cx, settings::SIDEBAR_DEFAULT + 70.0, 21.0).await?;
                settle(cx, 400).await;
                capture(window, cx, output.join("p08-breadcrumb-to-parent.png"))?;
                let to_parent = readback(window, cx)?;
                anyhow::ensure!(to_parent["selected"] == PARENT, "the breadcrumb must open the actual parent: {to_parent}");

                // p09. A child of an archived parent: marked, and opens it.
                select(cx, ARCH_CHILD);
                settle(cx, 400).await;
                capture(window, cx, output.join("p09-archived-parent-breadcrumb.png"))?;
                click_at(window, cx, settings::SIDEBAR_DEFAULT + 70.0, 21.0).await?;
                settle(cx, 400).await;
                let to_archived = readback(window, cx)?;
                anyhow::ensure!(to_archived["selected"] == ARCH_PARENT, "the breadcrumb must open an archived parent: {to_archived}");
                // An archived chat cannot host: refused before anything changes.
                window.update(cx, |shell, _, cx| shell.fixture_open_child_chats(cx))?;
                settle(cx, 500).await;
                let before = readback(window, cx)?["placements"].clone();
                let archived_host = drop_on_related(window, cx, THIRD).await?;
                anyhow::ensure!(archived_host["placements"] == before && archived_host["notice"].as_str().is_some_and(|text| text.contains("archived")), "an archived host must be refused: {archived_host}");

                // p10. A child whose parent is gone: named, never linked.
                select(cx, ORPHAN);
                settle(cx, 400).await;
                capture(window, cx, output.join("p10-missing-parent.png"))?;
                click_at(window, cx, settings::SIDEBAR_DEFAULT + 70.0, 21.0).await?;
                settle(cx, 300).await;
                let orphan = readback(window, cx)?;
                anyhow::ensure!(orphan["selected"] == ORPHAN && !has(&ids(&orphan["sidebarOrder"]), ORPHAN), "a missing parent must not navigate or reveal a sidebar row: {orphan}");

                // An ancestor cannot sit beside its own child.
                select(cx, CHILD_A);
                settle(cx, 400).await;
                window.update(cx, |shell, _, cx| shell.fixture_open_child_chats(cx))?;
                settle(cx, 400).await;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                let before = readback(window, cx)?["placements"].clone();
                drag(window, cx, (sidebar_x, center(&left, PARENT)?), pane_drop).await?;
                let ancestor = readback(window, cx)?;
                anyhow::ensure!(ancestor["placements"] == before && has(&ids(&ancestor["sidebarOrder"]), PARENT), "an ancestor must stay in the sidebar: {ancestor}");

                // Close and reopen the pane: placement holds.
                select(cx, PARENT);
                settle(cx, 400).await;
                window.update(cx, |shell, _, cx| shell.fixture_close_right_pane(cx))?;
                settle(cx, 400).await;
                window.update(cx, |shell, _, cx| {
                    shell.fixture_open_right_pane(cx);
                    shell.fixture_open_child_chats(cx);
                })?;
                settle(cx, 600).await;
                draw(window, cx)?;
                let reopened = readback(window, cx)?;
                anyhow::ensure!(has(&related_ids(&reopened), OTHER) && !has(&related_ids(&reopened), CHILD_B) && has(&ids(&reopened["sidebarOrder"]), CHILD_B), "placement must survive closing the pane: {reopened}");
                let scope = reopened["placementScope"].as_str().unwrap_or_default().to_string();

                // A saved cycle (an invalid state from an older build or
                // another edit) never hides its chats and is pruned.
                window.update(cx, |shell, _, cx| {
                    shell.fixture_set_chat_placements(
                        settings::ChatPlacements {
                            promoted: vec![CHILD_B.into()],
                            docked: [(OTHER.to_string(), THIRD.to_string()), (THIRD.to_string(), OTHER.to_string())]
                                .into_iter()
                                .collect(),
                        },
                        cx,
                    )
                })?;
                // Any state change runs the shell's placement pruning.
                cx.update(|cx| state.update(cx, |_, cx| cx.notify()));
                settle(cx, 300).await;
                draw(window, cx)?;
                let recovered = readback(window, cx)?;
                let order = ids(&recovered["sidebarOrder"]);
                anyhow::ensure!(has(&order, THIRD) || has(&order, OTHER), "a saved cycle must leave its chats reachable: {recovered}");
                anyhow::ensure!(recovered["placements"]["docked"].as_object().map(|docked| docked.len()) == Some(1), "the cyclic saved dock must be pruned: {recovered}");
                record.insert("recoveredCycle".into(), recovered);
                window.update(cx, |shell, _, cx| {
                    shell.fixture_set_chat_placements(
                        settings::ChatPlacements {
                            promoted: vec![CHILD_B.into()],
                            docked: [(OTHER.to_string(), PARENT.to_string())].into_iter().collect(),
                        },
                        cx,
                    )
                })?;
                settle(cx, 200).await;

                // p11. Persist, read back from disk, reload into a new Shell.
                cx.update(|cx| settings::flush(cx));
                let saved = settings::UiSettings::load(&ui_data);
                let on_disk = saved.chat_placements.get(&scope).cloned().unwrap_or_default();
                anyhow::ensure!(on_disk.promoted == [CHILD_B.to_string()] && on_disk.docked.get(OTHER).map(String::as_str) == Some(PARENT), "placements on disk: {on_disk:?}");
                anyhow::ensure!(saved.chat_placements.len() == 1, "placements are scoped to this profile only: {:?}", saved.chat_placements);
                // A restart: the new Shell exists before the boot selection
                // lands (it adopts the selected chat from that change), so
                // the selection is delivered after it opens, as at boot.
                window.update(cx, |_, window, _| window.remove_window())?;
                cx.update(|cx| state.update(cx, |state, cx| state.select_chat(None, cx)));
                let reload_state = state.clone();
                let reload_boot = boot.clone();
                let reload_dir = ui_data.clone();
                window = cx.update(|cx| {
                    settings::init(saved, reload_dir, cx);
                    open(cx, reload_state, reload_boot)
                });
                settle(cx, 600).await;
                cx.update(|cx| state.update(cx, |state, cx| state.select_chat(Some(PARENT.into()), cx)));
                settle(cx, 600).await;
                anyhow::ensure!(readback(window, cx)?["selected"] == PARENT, "boot selection after the reload");
                window.update(cx, |shell, _, cx| {
                    shell.fixture_open_right_pane(cx);
                    shell.fixture_open_child_chats(cx);
                })?;
                settle(cx, 800).await;
                capture(window, cx, output.join("p11-after-reload.png"))?;
                let reloaded = readback(window, cx)?;
                anyhow::ensure!(!has(&ids(&reloaded["sidebarOrder"]), OTHER) && has(&ids(&reloaded["sidebarOrder"]), CHILD_B), "reloaded sidebar placement: {reloaded}");
                anyhow::ensure!(has(&related_ids(&reloaded), OTHER) && !has(&related_ids(&reloaded), CHILD_B), "reloaded Related chats placement: {reloaded}");
                let a_scope = reloaded["placementScope"].as_str().unwrap_or_default().to_string();
                record.insert("reloaded".into(), reloaded);

                // Two profiles, one device and scope: profile B lists every
                // chat of A except Child B (a stale id for A's placements).
                anyhow::ensure!(!a_scope.is_empty() && a_scope != "fixture-profile-b", "profile A must have its own profile id: {a_scope}");
                let a_saved = cx.update(|cx| settings::current(cx)).chat_placements.get(&a_scope).cloned();
                anyhow::ensure!(a_saved.is_some(), "profile A placements before the switch");
                let chats_a = state.read_with(cx, |state, _| state.chats.clone());
                let switch = |cx: &mut AsyncApp, handle: state::EngineHandle, chats: Vec<luna_proto::Chat>| {
                    cx.update(|cx| {
                        state.update(cx, |state, cx| {
                            state.fixture_attachment_engine(handle);
                            state.chats = chats;
                            cx.notify();
                        })
                    });
                };
                switch(cx, handle_b.clone(), chats_a.iter().filter(|chat| chat.id != CHILD_B).cloned().collect());
                focus_related(window, cx, &state, PARENT).await?;
                let in_b = readback(window, cx)?;
                anyhow::ensure!(in_b["placementScope"] == "fixture-profile-b" && in_b["placements"].is_null(), "profile B starts with no placements: {in_b}");
                anyhow::ensure!(has(&ids(&in_b["sidebarOrder"]), OTHER) && !has(&related_ids(&in_b), OTHER), "profile A's dock must not apply in B: {in_b}");
                let saved = cx.update(|cx| settings::current(cx));
                anyhow::ensure!(saved.chat_placements.get(&a_scope) == a_saved.as_ref(), "B must not prune A's stale ids: {:?}", saved.chat_placements);
                // A placement made in B stays B's.
                let docked_in_b = drop_on_related(window, cx, THIRD).await?;
                anyhow::ensure!(docked_in_b["placements"]["docked"][THIRD] == PARENT, "a dock in profile B: {docked_in_b}");
                let saved = cx.update(|cx| settings::current(cx));
                anyhow::ensure!(saved.chat_placements.get(&a_scope) == a_saved.as_ref() && saved.chat_placements.contains_key("fixture-profile-b"), "each profile keeps its own placements: {:?}", saved.chat_placements);
                capture(window, cx, output.join("p11b-profile-b.png"))?;
                record.insert("profileB".into(), docked_in_b);

                // No profile id: nothing applies, persists or prunes; a move
                // is refused rather than stored under a guessed key.
                switch(cx, handle_none.clone(), chats_a.clone());
                focus_related(window, cx, &state, PARENT).await?;
                let unnamed = readback(window, cx)?;
                anyhow::ensure!(unnamed["placementScope"].is_null() && unnamed["placements"].is_null(), "no profile, no placement: {unnamed}");
                anyhow::ensure!(has(&ids(&unnamed["sidebarOrder"]), OTHER) && !has(&ids(&unnamed["sidebarOrder"]), CHILD_B), "saved placements must not apply without a profile: {unnamed}");
                let before = cx.update(|cx| settings::current(cx)).chat_placements;
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (pane_x, center(&right, CHILD_A)?), (sidebar_x, lowest(&left) + 40.0)).await?;
                let refused_unnamed = readback(window, cx)?;
                anyhow::ensure!(refused_unnamed["notice"].as_str().is_some_and(|text| text.contains("identifies its profile")), "the refusal must say why: {refused_unnamed}");
                anyhow::ensure!(cx.update(|cx| settings::current(cx)).chat_placements == before, "nothing may be stored without a profile");

                // Back to profile A: its placements apply again; B's do not.
                switch(cx, handle_a.clone(), chats_a.clone());
                focus_related(window, cx, &state, PARENT).await?;
                let back_in_a = readback(window, cx)?;
                anyhow::ensure!(back_in_a["placementScope"] == a_scope.as_str(), "profile A again: {back_in_a}");
                anyhow::ensure!(!has(&ids(&back_in_a["sidebarOrder"]), OTHER) && has(&ids(&back_in_a["sidebarOrder"]), CHILD_B) && has(&ids(&back_in_a["sidebarOrder"]), THIRD), "A's placements back, B's dock absent: {back_in_a}");
                record.insert("backInA".into(), back_in_a);

                // p12. Reverse moves after the reload: undock and return.
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (pane_x, center(&right, OTHER)?), (sidebar_x, lowest(&left) + 40.0)).await?;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (sidebar_x, center(&left, CHILD_B)?), pane_drop).await?;
                capture(window, cx, output.join("p12-reversed.png"))?;
                let reversed = readback(window, cx)?;
                anyhow::ensure!(has(&ids(&reversed["sidebarOrder"]), OTHER) && !has(&ids(&reversed["sidebarOrder"]), CHILD_B), "reverse moves: {reversed}");
                anyhow::ensure!(has(&related_ids(&reversed), CHILD_B) && !has(&related_ids(&reversed), OTHER), "reverse moves in Related chats: {reversed}");
                anyhow::ensure!(reversed["placements"].is_null(), "no placements remain: {reversed}");
                anyhow::ensure!(reversed["lineage"] == lineage, "lineage never changes: {reversed}");
                record.insert("reversed".into(), reversed);

                // A generic history fork has `forkSourceTaskId` but no side
                // chat service row. The same default and manual placement
                // apply to it after a complete chats frame.
                cx.update(|cx| state.update(cx, |state, cx| {
                    state.fork_source_rows.insert(THIRD.into(), PARENT.into());
                    let chats = state.chats.clone();
                    state.apply_chats(chats);
                    cx.notify();
                }));
                settle(cx, 300).await;
                let generic_unplaced = readback(window, cx)?;
                anyhow::ensure!(!has(&ids(&generic_unplaced["sidebarOrder"]), THIRD) && has(&related_ids(&generic_unplaced), THIRD), "generic fork starts in Related chats only: {generic_unplaced}");
                let right = row_spans(window, cx, pane_x, "child-pane-chat-")?;
                let left = row_spans(window, cx, sidebar_x, "chat-")?;
                drag(window, cx, (pane_x, center(&right, THIRD)?), (sidebar_x, lowest(&left) + 40.0)).await?;
                let generic_placed = readback(window, cx)?;
                anyhow::ensure!(has(&ids(&generic_placed["sidebarOrder"]), THIRD) && has(&ids(&generic_placed["placements"]["promoted"]), THIRD) && generic_placed["forkSources"][THIRD] == PARENT, "generic fork accepts an explicit sidebar drag: {generic_placed}");
                record.insert("genericFork".into(), serde_json::json!({"unplaced": generic_unplaced, "placed": generic_placed}));
                Ok(serde_json::Value::Object(record))
            }
            .await;
            match result {
                Ok(record) => {
                    let written = std::fs::write(output.join("readback.json"), serde_json::to_vec_pretty(&record).unwrap_or_default())
                        .and_then(|_| {
                            std::fs::write(
                                output.join("chat-placement.pass"),
                                "PASS: headless production Shell with real pointer input; child rows (including pinned and orphaned), service side chats, and generic history forks start outside the sidebar, and selecting a child does not reveal it there; dragging a child or fork into the sidebar saves explicit placement and renders an aligned card, with lineage and source intact; a Pinned-group drop clamps below pins; an ordinary chat docks alongside with its own composer; invalid drops are refused; breadcrumbs preserve actual parent identity; saved placement survives watch replay, pane closure, and settings reload within one profile; reverse moves restore the sidebar and Related chats\n",
                            )
                        });
                    if let Err(error) = written {
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                        // AppKit's quit can end the process with status 0
                        // before `run` returns: leave with the failure now.
                        std::process::exit(1);
                    }
                }
                Err(error) => {
                    eprintln!("Chat placement fixture failed: {error:#}");
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
