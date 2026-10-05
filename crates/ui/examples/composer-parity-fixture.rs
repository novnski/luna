//! Headless GPUI E2E for composer parity between the main chat and a side
//! chat (a persistent fork) open beside it in a workbench split, plus the
//! fork's ordinary tab header.
//!
//! Drives the production `Shell` against an in-process `EngineCore` (real
//! `WatchChats`, sessions, transcripts and side chats). The scripted Mock
//! provider carries a Claude-shaped catalog (reasoning ladder, context
//! window, Speed), so the chips and access levels render as they do for a
//! Claude chat. It keeps the Mock id on purpose: the Claude Code id makes the
//! engine probe the real `claude` CLI for its catalog. (The Goal switch is
//! offered only on Codex and Claude Code chats, so it is absent here.)
//!
//! Failure cases this fixture asserts (written before the implementation):
//! 1. The main composer renders one row while the pane renders two, or their
//!    heights differ.
//! 2. The two pills' bottom edges (and so their footers) sit at different y.
//! 3. A chip label is cut: the chosen controls row needs more width than the
//!    row has, at any window or pane width.
//! 4. The access level renders as text instead of its icon (Full access =
//!    open lock, Approve for me = shield check).
//! 5. Fast speed renders as "Fast" text instead of the bolt, or the bolt
//!    shows at Standard.
//! 6. Changing the main chat's reasoning or speed changes the side chat (a
//!    fork follows its own ordinary settings, not its source's).
//! 7. Hovering a main chip lights the side chat's chip (shared hover keys).
//! 8. The model menu still says "Service Tier" / "Fast Mode" or On/Off
//!    instead of Speed: Standard / Fast.
//! 9. The fork's tab header is not the ordinary one-line strip (fork
//!    identity and a link to its source), or shows an End, lease, expiry or
//!    read-only state.
//! 10. Groups never shrink below their 320 px minimum, so the old forced
//!    240 px right pane is replaced by a fork group at that minimum (the
//!    canvas scrolls): its controls row must still fit, yielding whole.
//! 11. Context usage renders under the pill instead of in the controls row
//!    after Goal (Main and fork alike), the row's budget ignores it (a chip
//!    truncates or overlaps), or the pill does not sit lower in the frame
//!    once the slot under it is gone.
//!
//! Execution diagnostics (harness evolution slice 1; see
//! docs/design/harness-evolution/ui-plan.md), written before the
//! implementation:
//! 12. The engine's own WatchChats `execution` for Main is not decoded (schema
//!     1, Main's task, requested provider) or the chips claim a pending change
//!     the engine does not report.
//! 13. While a turn runs, a requested model/reasoning/speed or access level
//!     replaces the chip's label/icon instead of the running one, or the
//!     change shows without its dot and Running / Requested / how it applies.
//! 14. A summary seeded for Main leaks into the fork's composer (routing).
//! 15. A Steer row is offered as native Steer when the engine says
//!     unavailable or after the turn, or its reason is missing; a native
//!     decision is not offered as Steer now.
//! 16. Next-turn clearing (pending emptied, idle) leaves a dot or the old
//!     running labels instead of the requested settings.
//! 17. A parked warm runtime reads as Running, or its loaded settings replace
//!     the requested labels.
//! 18. New-binding copy implies context is carried over automatically.
//! 19. A missing, old-schema or other-task summary is shown or replaced by a
//!     local claim: chips, access and Steer must equal the legacy baseline.
//! 20. The Execution card follows Main instead of the focused fork tab, or
//!     shows the raw task id.
//! 21. A late contract response for the previously focused chat overwrites
//!     the current focus.
//! 22. Adapter capabilities read as verified; long reasons/notes are
//!     truncated or not wrapped in the opaque tooltip and card.
//! 23. Pending rows in the card disagree with the engine's pending list.
//! 24. Once the watch is invalidated (stream end, reconnect, a frame without
//!     `execution`), a cached contract snapshot revives an old Running state.
//! 25. A same-task contract response requested on an earlier engine
//!     connection or profile installs after a reconnect.
//!
//! Context transfers (harness evolution slice 2; see
//! docs/design/harness-evolution/slice-2-ui.md), written before the
//! implementation:
//! 26. Send findings is offered for Main or a chat without a living lineage
//!     source, or targets anything but the fork's source.
//! 27. Opening the dialog or previewing writes a receipt or starts a turn.
//! 28. Applying findings starts or interrupts a turn in the source, changes
//!     its draft, or stores nothing (no Planned receipt from the fork).
//! 29. The source advanced while the dialog was open and the old preview is
//!     sent; refreshing loses the source's draft.
//! 30. A lost apply answer is shown as sent or failed, offers a resend, or
//!     Check status sends a second command; a refresh repeats delivered
//!     findings instead of only new ones.
//! 31. A preview from an earlier engine connection can still be applied.
//! 32. The provider switch preselects a choice, or its primary button (or
//!     Enter) acts before an explicit choice; a preview for another target
//!     provider survives a target change.
//! 33. Continue with context applies a handoff that starts a turn or loses the
//!     receipt; Start fresh carries context.
//! 34. Receipt Details cannot be opened, or omitted-item reads error out.
//! 35. Deleting the fork breaks its source's receipts or names it as alive.
//! 36. Start new task with context creates a task on preview or Cancel,
//!     changes the target id between refreshes, creates an agent child, a
//!     native session or a turn, touches the source's draft, or a lost
//!     answer creates a second task instead of recovering the same id.
//! 37. Details' Read more for a truncated omitted message repeats its prefix,
//!     skips bytes, appends another message's text, ignores the returned
//!     nextTextOffset, splits a UTF-8 character, or labels an excerpt as the
//!     whole message.
//! 38. An apply or Check status answer arriving after the engine connection
//!     changed settles the dialog or panel, opens the created task, or
//!     notifies in the current UI.
//! 39. Refresh while an apply is in flight or unknown replaces the flow,
//!     drops the retained command id, or enables a second apply.
//!
//! The host refuses a second preview from a source whose earlier transfer is
//! still planned; steps first let the destination send (consuming it).
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example composer-parity-fixture
//! Run:   target/debug/examples/composer-parity-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes c01..c11 PNG captures (c11 is the group minimum), e01..e07
//! execution captures, readback.json and composer-parity.pass.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeDelta, Utc};
use futures::StreamExt;
use futures::stream::BoxStream;
use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowHandle, WindowOptions, px, size};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::mock::MockHarness;
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, ChatConfig, DoneStatus, HarnessId, Model, ModelOption, ModelOptionChoice,
    PermissionMode, ReasoningLevel, RunRequest, SandboxLevel, SteeringMode,
};
use luna_ui::*;

const PROJECT: &str = "composer-parity-project";
const PARENT: &str = "c0c0c0c0-0000-4000-8000-000000000001";
const SONNET: &str = "claude-sonnet-5-5";

/// The mock script with a Claude-shaped catalog.
struct ClaudeShaped(MockHarness);

fn select(id: &str, label: &str, choices: &[(&str, &str)], default: &str) -> ModelOption {
    ModelOption {
        id: id.into(),
        label: label.into(),
        choices: choices
            .iter()
            .map(|(id, label)| ModelOptionChoice {
                id: (*id).into(),
                label: (*label).into(),
            })
            .collect(),
        default_choice: default.into(),
    }
}

const LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
];

#[async_trait]
impl Harness for ClaudeShaped {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Mock"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        LADDER
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        let options = || {
            vec![
                select(
                    "contextWindow",
                    "Context Window",
                    &[("200k", "200K"), ("1m", "1M")],
                    "200k",
                ),
                select(
                    "fastMode",
                    "Speed",
                    &[("off", "Standard"), ("on", "Fast")],
                    "off",
                ),
            ]
        };
        Ok(vec![
            Model {
                id: SONNET.into(),
                label: "Sonnet 5.5".into(),
                description: None,
                reasoning_levels: LADDER.to_vec(),
                options: options(),
            },
            Model {
                id: "claude-opus-5-5".into(),
                label: "Opus 5.5".into(),
                description: None,
                reasoning_levels: LADDER.to_vec(),
                options: options(),
            },
        ])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let started = AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: request.model.clone().unwrap_or_else(|| SONNET.into()),
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: request
                .resume
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            assistant_message_id: uuid::Uuid::new_v4().to_string(),
        };
        Ok(futures::stream::once(async move { Ok(started) })
            .chain(self.0.run(request, controls).await?)
            .boxed())
    }
}

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some(SONNET.into()),
        reasoning: Some(ReasoningLevel::Low),
        model_options: Default::default(),
        sandbox: SandboxLevel::DangerFullAccess,
        permission_mode: PermissionMode::Full,
        effective_permission_mode: PermissionMode::Full,
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

/// Lay out and paint one frame (bounds readbacks reflect it).
fn draw(window: WindowHandle<shell::Shell>, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
    })?;
    Ok(())
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn move_mouse(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    x: f32,
    y: f32,
) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                position: gpui::point(px(x), px(y)),
                pressed_button: None,
                modifiers: Default::default(),
            }),
            cx,
        );
        window.refresh();
    })?;
    Ok(())
}

/// Both composers' readbacks (main, pane bound to `side`).
fn composers(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    side: &str,
) -> anyhow::Result<(serde_json::Value, serde_json::Value)> {
    window.update(cx, |shell, _, cx| {
        shell.fixture_composer_readbacks(side, cx)
    })
}

fn panes(node: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    if node["kind"] == "pane" {
        out.push(node.clone());
    } else {
        panes(&node["first"], out);
        panes(&node["second"], out);
    }
}

/// The first Side tab in a workbench readback.
fn side_tab(workbench: &serde_json::Value) -> Option<String> {
    let mut all = Vec::new();
    panes(&workbench["root"], &mut all);
    all.into_iter()
        .flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default())
        .find(|tab| tab["role"] == "Side")
        .and_then(|tab| tab["chatId"].as_str().map(str::to_string))
}

/// `main` and `side` are each the active tab of their own group.
fn active_in_own_groups(workbench: &serde_json::Value, main: &str, side: &str) -> bool {
    let mut all = Vec::new();
    panes(&workbench["root"], &mut all);
    let group = |chat: &str| all.iter().position(|pane| pane["active"] == chat);
    matches!((group(main), group(side)), (Some(a), Some(b)) if a != b)
}

/// The width of the group whose active tab is `chat`.
fn pane_width(workbench: &serde_json::Value, chat: &str) -> Option<f64> {
    let mut all = Vec::new();
    panes(&workbench["root"], &mut all);
    let id = all.iter().find(|pane| pane["active"] == chat)?["id"].clone();
    workbench["paneBounds"]
        .as_array()?
        .iter()
        .find(|entry| entry["id"] == id)?["bounds"]["w"]
        .as_f64()
}

const OPUS: &str = "claude-opus-5-5";

fn exec_settings(
    model: &str,
    reasoning: ReasoningLevel,
    fast: bool,
    permission: PermissionMode,
) -> luna_proto::ExecutionSettings {
    let mut model_options = serde_json::Map::new();
    if fast {
        model_options.insert("fastMode".into(), "on".into());
    }
    luna_proto::ExecutionSettings {
        provider: HarnessId::Mock,
        provider_account_id: None,
        model: Some(model.into()),
        reasoning: Some(reasoning),
        model_options,
        permission_mode: permission,
        sandbox: SandboxLevel::WorkspaceWrite,
    }
}

/// What Main requests after c04 (Sonnet, High, Fast) at Full access.
fn main_requested() -> luna_proto::ExecutionSettings {
    exec_settings(SONNET, ReasoningLevel::High, true, PermissionMode::Full)
}

/// An older runtime still running Opus, Low, Standard at Approve for me.
fn opus_running() -> luna_proto::ExecutionSettings {
    exec_settings(OPUS, ReasoningLevel::Low, false, PermissionMode::Auto)
}

#[allow(clippy::too_many_arguments)]
fn exec_summary(
    task: &str,
    phase: luna_proto::ExecutionPhase,
    requested: luna_proto::ExecutionSettings,
    running: Option<luna_proto::ExecutionSettings>,
    pending: &[luna_proto::ExecutionSettingField],
    transition: (luna_proto::ExecutionTransitionKind, bool, &str),
    steer: (luna_proto::ExecutionSteerKind, Option<&str>),
) -> luna_proto::TaskExecutionSummary {
    luna_proto::TaskExecutionSummary {
        schema_version: luna_proto::EXECUTION_SUMMARY_VERSION,
        task_id: task.into(),
        observed_at_ms: Utc::now().timestamp_millis(),
        phase,
        requested,
        running,
        pending: pending.to_vec(),
        transition: luna_proto::ExecutionTransition {
            kind: transition.0,
            waits_for_turn_end: transition.1,
            reason: transition.2.into(),
        },
        steer: luna_proto::ExecutionSteerDecision {
            kind: steer.0,
            reason: steer.1.map(str::to_string),
        },
        capabilities: None,
        binding_generation: Some(1),
        observation_note: None,
    }
}

/// Model, reasoning, speed and access pending (the full chip difference).
const CHIP_PENDING: [luna_proto::ExecutionSettingField; 4] = [
    luna_proto::ExecutionSettingField::Model,
    luna_proto::ExecutionSettingField::Reasoning,
    luna_proto::ExecutionSettingField::ModelOptions,
    luna_proto::ExecutionSettingField::Permission,
];

fn contract_view(task: &str) -> serde_json::Value {
    contract_view_with(task, None)
}

/// A contract response that also carries an execution snapshot (as an
/// earlier `GetRuntimeContract` may have).
fn contract_view_with(
    task: &str,
    execution: Option<luna_proto::TaskExecutionSummary>,
) -> serde_json::Value {
    serde_json::to_value(luna_proto::RuntimeContractView {
        task_id: task.into(),
        provider: Some(HarnessId::Mock),
        provider_delivery: luna_proto::RuntimeDelivery::Unsupported,
        effective: None,
        unavailable_reason: Some("Fixture: no provider session acknowledged a contract.".into()),
        execution,
    })
    .unwrap_or_default()
}

fn steer_resolutions(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    side: &str,
) -> anyhow::Result<(serde_json::Value, serde_json::Value)> {
    window.update(cx, |shell, _, cx| shell.fixture_steer_resolutions(side, cx))
}

fn tooltip_text(chips: &serde_json::Value, key: &str) -> String {
    chips[key].as_str().unwrap_or_default().to_string()
}

fn f(value: &serde_json::Value) -> f64 {
    value.as_f64().unwrap_or(f64::NAN)
}

/// Failure case 3: the chosen controls row fits its budget.
fn row_fits(name: &str, composer: &serde_json::Value) -> anyhow::Result<()> {
    let row = &composer["controlsRow"];
    let model = match row["modelFit"].as_str() {
        Some("Full") => f(&row["modelFull"]),
        Some("NameOnly") => f(&row["modelNameOnly"]),
        _ => 32.0,
    };
    let goal = row["goal"].as_array().map_or(0.0, |goal| {
        4.0 + if row["goalIconOnly"] == true {
            f(&goal[1])
        } else {
            f(&goal[0])
        }
    });
    // Failure case 11: the context-usage chip shares the row's budget.
    let usage = row["usage"].as_array().map_or(0.0, |usage| {
        4.0 + if row["usageIconOnly"] == true {
            f(&usage[1])
        } else {
            f(&usage[0])
        }
    });
    let need = model + 4.0 + 32.0 + goal + usage;
    anyhow::ensure!(
        need <= f(&row["budget"]) + 0.5,
        "{name} controls row needs {need:.1}px but has {:.1}px: {composer}",
        f(&row["budget"])
    );
    Ok(())
}

/// Failure cases 1-2: identical two-row pills, level bottoms.
fn aligned(main: &serde_json::Value, pane: &serde_json::Value) -> anyhow::Result<()> {
    let (m, p) = (&main["surface"], &pane["surface"]);
    let (mh, ph) = (f(&m["height"]), f(&p["height"]));
    anyhow::ensure!(
        (mh - ph).abs() <= 0.5,
        "pill heights differ: main {mh} pane {ph}"
    );
    anyhow::ensure!(mh >= 100.0, "the main pill is not two-row: {mh}");
    let (mb, pb) = (f(&m["y"]) + mh, f(&p["y"]) + ph);
    anyhow::ensure!(
        (mb - pb).abs() <= 0.5,
        "pill bottoms differ: main {mb} pane {pb}"
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: composer-parity-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["readback.json", "composer-parity.pass", "failure.txt"] {
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
    registry.register(Arc::new(ClaudeShaped(MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: "Yes: the cache migration is additive.".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            },
        ],
    })));
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "composer-parity", "composer-parity"),
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
    auth.dev_user_id = "composer-parity".into();
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
        let client = luna_rpc::memory_client(core.rpc_service());
        client
            .call_as::<serde_json::Value>(
                "Mutate",
                serde_json::json!({"op":"createChat", "chatId":PARENT,
                "spaceId":PROJECT, "cwd":project.to_string_lossy(), "config":config()}),
            )
            .await?;
        core.workspace
            .rename_chat(PARENT, "Plan the v0.5 release")?;
        let at = now - TimeDelta::minutes(3);
        let at_ms = at.timestamp_millis();
        let doc = core.doc_host.open(PARENT)?;
        doc.doc().push_message(&text_entry(
            "parent-prompt",
            MessageRole::User,
            "Plan the v0.5 release rollout: cache migration, settings rename, then the UI flag.",
            &device,
            at_ms - 60_000,
        ))?;
        doc.doc().push_message(&text_entry(
            "parent-answer",
            MessageRole::Assistant,
            "Proposed order: cache migration, settings rename, UI flag. Each step is reversible.",
            &device,
            at_ms,
        ))?;
        let mut chat = core.workspace.chat(PARENT)?.expect("seeded chat");
        chat.created_at = at - TimeDelta::minutes(2);
        chat.last_message_at = Some(at);
        chat.last_seen_at = Some(at + TimeDelta::seconds(1));
        core.workspace.import_chat_row(&chat)?;
        anyhow::Ok(vec![chat])
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
                state.select_chat(Some(PARENT.into()), cx);
                state.fixture_attach_engine(handle, cx);
                state
            });
            let window = cx
                .open_window(
                    WindowOptions {
                        window_background: theme::Theme::of(cx).window_background_appearance(),
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(20.), px(20.)),
                            // Leave room to create two 320px groups with the
                            // rounded viewport inset; later steps shrink them.
                            // The wide baseline includes the in-row context
                            // share; later steps still exercise 320px groups.
                            size(px(1600.), px(900.)),
                        ))),
                        show: false,
                        focus: false,
                        ..Default::default()
                    },
                    |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
                )
                .unwrap();
            cx.spawn(async move |cx| {
                let result: anyhow::Result<serde_json::Value> = async {
                    let mut record = serde_json::Map::new();
                    settle(cx, 900).await;

                    // c01. A fork beside its source in a workbench split: one
                    // geometry. New side chat opens it as a Side tab; the
                    // palette's Move tab to split right puts it in its own
                    // group beside Main.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_open_right_pane(cx);
                        shell.fixture_open_side_chats(cx);
                    })?;
                    for _ in 0..60 {
                        settle(cx, 100).await;
                        let workbench = window.update(cx, |shell, _, cx| shell.fixture_workbench_readback(cx))?;
                        if workbench["loaded"] == true && workbench["main"] == PARENT {
                            break;
                        }
                    }
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("snapshot", "", cx))?;
                    let mut side = String::new();
                    let mut last = serde_json::Value::Null;
                    for _ in 0..200 {
                        settle(cx, 100).await;
                        let workbench = window.update(cx, |shell, _, cx| shell.fixture_workbench_readback(cx))?;
                        if let Some(id) = side_tab(&workbench) {
                            side = id;
                            break;
                        }
                        last = workbench;
                    }
                    anyhow::ensure!(!side.is_empty(), "New side chat never opened as a Side tab: {last}");
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_action("Move tab to split right", cx))?;
                    let mut split = serde_json::Value::Null;
                    for _ in 0..100 {
                        settle(cx, 100).await;
                        split = window.update(cx, |shell, _, cx| shell.fixture_workbench_readback(cx))?;
                        if active_in_own_groups(&split, PARENT, &side) {
                            break;
                        }
                    }
                    anyhow::ensure!(active_in_own_groups(&split, PARENT, &side), "the fork and Main must be active in two groups: {split}");
                    let header = window.update(cx, |shell, _, cx| shell.fixture_workbench_strip(&side, cx))?;
                    let words = header["status"].as_str().unwrap_or_default().to_lowercase();
                    anyhow::ensure!(
                        header["identity"] == "Side chat"
                            && header["parentLinked"] == true
                            && !["ended", "read-only", "writable", "expire", "lease", "unload"].iter().any(|word| words.contains(word)),
                        "the fork's header is the ordinary strip linking its source: {header}"
                    );
                    record.insert("header".into(), header);
                    // Two frames: the controls row fits against measured widths.
                    settle(cx, 600).await;
                    capture(window, cx, output.join("c01-main-side-dark.png"))?;
                    settle(cx, 200).await;
                    let (main, pane) = composers(window, cx, &side)?;
                    aligned(&main, &pane)?;
                    row_fits("main", &main)?;
                    row_fits("pane", &pane)?;
                    for (name, composer) in [("main", &main), ("pane", &pane)] {
                        let chips = &composer["chips"];
                        anyhow::ensure!(chips["model"] == "Sonnet 5.5" && chips["effort"] == "Low", "{name} model chip: {chips}");
                        anyhow::ensure!(chips["fast"] == false, "{name} shows the bolt at Standard: {chips}");
                        anyhow::ensure!(chips["access"] == "Full access" && chips["accessIcon"] == "icons/lock-keyhole-minimalistic-unlocked.svg", "{name} access chip: {chips}");
                    }
                    anyhow::ensure!(main["controlsRow"]["modelFit"] == "Full" && main["controlsRow"]["goalIconOnly"] == false, "the wide main row must show everything: {main}");
                    for (name, composer) in [("main", &main), ("fork", &pane)] {
                        anyhow::ensure!(
                            composer["controlsRow"]["usage"].is_array(),
                            "{name}: context usage sits in the controls row: {composer}"
                        );
                    }
                    anyhow::ensure!(main["controlsRow"]["usageIconOnly"] == false, "the wide main row shows the context share: {main}");
                    record.insert("c01".into(), serde_json::json!({"main": main, "pane": pane}));

                    // c02. Hover the main model chip: only the main chip lights.
                    let surface = &main["surface"];
                    let (sx, sy, sh) = (f(&surface["x"]) as f32, f(&surface["y"]) as f32, f(&surface["height"]) as f32);
                    let row_y = sy + sh - 1.0 - 12.0 - 16.0;
                    let chips_x = sx + 1.0 + 12.0 + 28.0 + 2.0;
                    move_mouse(window, cx, chips_x + 24.0, row_y)?;
                    settle(cx, 400).await;
                    capture(window, cx, output.join("c02-hover-main-model.png"))?;
                    let (main, pane) = composers(window, cx, &side)?;
                    anyhow::ensure!(f(&main["chips"]["hoverModel"]) > 0.9, "the hovered main chip did not light: {main}");
                    anyhow::ensure!(f(&pane["chips"]["hoverModel"]) < 0.01, "the side chat's chip lit with the main one: {pane}");
                    record.insert("c02".into(), serde_json::json!({"main": main["chips"], "pane": pane["chips"]}));

                    // c03. Access tooltip (the icon-only chip names itself).
                    let model_width = f(&main["controlsRow"]["modelFull"]) as f32;
                    move_mouse(window, cx, chips_x + model_width + 4.0 + 16.0, row_y)?;
                    settle(cx, 900).await;
                    capture(window, cx, output.join("c03-access-tooltip.png"))?;
                    let (main, _) = composers(window, cx, &side)?;
                    anyhow::ensure!(f(&main["chips"]["hoverAccess"]) > 0.9, "the access chip was not under the pointer: {main}");
                    move_mouse(window, cx, sx + 200.0, 300.0)?;
                    settle(cx, 400).await;

                    // c04. Main: Fast + High. The side chat keeps its own Standard + Low.
                    window.update(cx, |shell, _, cx| {
                        let (main, _) = shell.fixture_composers(None, cx);
                        let pickers = main.read(cx).pickers().clone();
                        pickers.update(cx, |pickers, cx| {
                            pickers.fixture_pick_option("fastMode", "on", cx);
                            pickers.fixture_pick_reasoning(ReasoningLevel::High, cx);
                        });
                    })?;
                    settle(cx, 800).await;
                    capture(window, cx, output.join("c04-main-fast-high.png"))?;
                    let (main, pane) = composers(window, cx, &side)?;
                    anyhow::ensure!(main["chips"]["fast"] == true && main["chips"]["effort"] == "High", "main did not take Fast + High: {main}");
                    anyhow::ensure!(main["chips"]["model"] == "Sonnet 5.5", "the bolt must replace text, not join the name: {main}");
                    anyhow::ensure!(pane["chips"]["fast"] == false && pane["chips"]["effort"] == "Low", "the side chat followed the main chat: {pane}");
                    anyhow::ensure!(main["chips"]["modelTooltip"].as_str().is_some_and(|t| t.contains("Fast")), "the tooltip must spell the speed out: {main}");
                    aligned(&main, &pane)?;
                    row_fits("main", &main)?;
                    record.insert("c04".into(), serde_json::json!({"main": main["chips"], "pane": pane["chips"]}));

                    // c05-c07. A wider right sidebar narrows both groups; at
                    // 760 px they sit at their 320 px minimum (the canvas
                    // scrolls).
                    let mut widths = serde_json::Map::new();
                    for (label, width) in [("c05-pane-360", 360.0), ("c06-pane-300", 300.0), ("c07-pane-760", 760.0)] {
                        window.update(cx, |shell, _, cx| shell.fixture_set_right_pane_width(width, cx))?;
                        settle(cx, 700).await;
                        capture(window, cx, output.join(format!("{label}.png")))?;
                        settle(cx, 200).await;
                        let (main, pane) = composers(window, cx, &side)?;
                        aligned(&main, &pane)?;
                        row_fits("main", &main)?;
                        row_fits("pane", &pane)?;
                        widths.insert(label.into(), serde_json::json!({"main": main["controlsRow"], "pane": pane["controlsRow"], "mainSurface": main["surface"], "paneSurface": pane["surface"]}));
                    }
                    // c11. Failure case 10: the fork's group at its 320 px
                    // minimum (a very wide right sidebar): the effort, then
                    // the name, yield whole — never an ellipsis.
                    window.update(cx, |shell, _, cx| shell.fixture_set_right_pane_width(900.0, cx))?;
                    settle(cx, 700).await;
                    capture(window, cx, output.join("c11-group-minimum.png"))?;
                    settle(cx, 200).await;
                    let (main, pane) = composers(window, cx, &side)?;
                    aligned(&main, &pane)?;
                    row_fits("pane", &pane)?;
                    let minimum = window.update(cx, |shell, _, cx| shell.fixture_workbench_readback(cx))?;
                    let group = pane_width(&minimum, &side);
                    anyhow::ensure!(group.is_some_and(|width| (319.5..=330.0).contains(&width)), "the fork's group sits at its 320 px minimum: {group:?} in {minimum}");
                    widths.insert("c11-group-minimum".into(), serde_json::json!({"pane": pane["controlsRow"], "paneSurface": pane["surface"], "chips": pane["chips"], "groupWidth": group}));
                    record.insert("widths".into(), serde_json::Value::Object(widths));

                    // c08. Light mode at a typical pane width.
                    window.update(cx, |shell, _, cx| shell.fixture_set_right_pane_width(480.0, cx))?;
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Light, cx));
                    settle(cx, 800).await;
                    capture(window, cx, output.join("c08-main-side-light.png"))?;
                    let (main, pane) = composers(window, cx, &side)?;
                    aligned(&main, &pane)?;
                    row_fits("main", &main)?;
                    row_fits("pane", &pane)?;

                    // c09. The model menu names Speed: Standard / Fast.
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark, cx));
                    window.update(cx, |shell, window, cx| {
                        let (main, _) = shell.fixture_composers(None, cx);
                        let pickers = main.read(cx).pickers().clone();
                        pickers.update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
                    })?;
                    settle(cx, 800).await;
                    capture(window, cx, output.join("c09-model-menu-speed.png"))?;
                    let options = window.update(cx, |shell, _, cx| {
                        let (main, _) = shell.fixture_composers(None, cx);
                        main.read(cx).pickers().read(cx).fixture_setting_groups(cx)
                    })?;
                    anyhow::ensure!(
                        options.iter().any(|(label, choices)| label == "Speed" && choices == &["Standard".to_string(), "Fast".to_string()]),
                        "the model menu must offer Speed: Standard / Fast: {options:?}"
                    );
                    record.insert("modelMenu".into(), serde_json::json!(options));
                    window.update(cx, |shell, _, cx| {
                        let (main, _) = shell.fixture_composers(None, cx);
                        let pickers = main.read(cx).pickers().clone();
                        pickers.update(cx, |pickers, cx| pickers.fixture_close(cx));
                    })?;
                    settle(cx, 500).await;

                    // c10. Typed draft: both pills still share one height.
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_type(&side, "Compare with the settings rename", false, cx))?;
                    settle(cx, 600).await;
                    capture(window, cx, output.join("c10-side-draft.png"))?;
                    let (main, pane) = composers(window, cx, &side)?;
                    aligned(&main, &pane)?;
                    record.insert("final".into(), serde_json::json!({"main": main, "pane": pane}));

                    // ---- Execution diagnostics (failure cases 12-23) ----
                    use luna_proto::{ExecutionPhase as Phase, ExecutionSettingField as Field, ExecutionSteerKind as Steer, ExecutionTransitionKind as Move};
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark, cx));
                    move_mouse(window, cx, 200.0, 300.0)?;
                    settle(cx, 300).await;

                    // e01. Failure case 12: the engine's own summary for Main.
                    let mut watched = serde_json::Value::Null;
                    for _ in 0..60 {
                        watched = window.update(cx, |shell, _, cx| shell.fixture_watch_execution(PARENT, cx))?;
                        if !watched.is_null() {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    anyhow::ensure!(
                        watched["schemaVersion"] == 1
                            && watched["taskId"] == PARENT
                            && watched["requested"]["provider"] == serde_json::to_value(HarnessId::Mock)?,
                        "WatchChats publishes Main's execution summary: {watched}"
                    );
                    let (main, _) = composers(window, cx, &side)?;
                    let engine_model_pending = watched["pending"].as_array().is_some_and(|pending| {
                        pending.iter().any(|field| ["provider", "account", "model", "reasoning", "modelOptions"].contains(&field.as_str().unwrap_or_default()))
                    });
                    let engine_access_pending = watched["pending"].as_array().is_some_and(|pending| pending.iter().any(|field| field == "permission"));
                    anyhow::ensure!(
                        main["chips"]["modelAttention"] == engine_model_pending && main["chips"]["accessAttention"] == engine_access_pending,
                        "the chips' pending dots match the engine's pending list: {watched} vs {}",
                        main["chips"]
                    );
                    record.insert("e01".into(), serde_json::json!({"watched": watched, "chips": main["chips"]}));

                    // The legacy baseline: Main's summary absent (a remote or
                    // older host publishes none).
                    window.update(cx, |shell, _, cx| shell.fixture_seed_execution(PARENT, None, cx))?;
                    settle(cx, 300).await;
                    let (baseline, baseline_pane) = composers(window, cx, &side)?;
                    let (baseline_steer, _) = steer_resolutions(window, cx, &side)?;
                    let legacy = |chips: &serde_json::Value| -> bool {
                        ["model", "effort", "fast", "access", "accessIcon"].iter().all(|key| chips[*key] == baseline["chips"][*key])
                            && chips["modelAttention"] == false
                    };
                    anyhow::ensure!(baseline["chips"]["modelAttention"] == false && baseline_steer["action"] == "Steer now", "without a summary the chips claim nothing and Mock's declared mid-turn steering applies: {} {baseline_steer}", baseline["chips"]);

                    // e02. Failure cases 13-14: a running turn on Opus/Low/
                    // Standard at Approve for me while Sonnet/High/Fast at
                    // Full access is requested.
                    let long_reason = "The running turn keeps the settings it started with; the new model, reasoning, speed and access level apply when the next turn starts on this session.";
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_seed_execution(
                            PARENT,
                            Some(exec_summary(PARENT, Phase::Running, main_requested(), Some(opus_running()), &CHIP_PENDING, (Move::ApplyNextTurn, true, long_reason), (Steer::Unavailable, Some("Pending settings must apply before this chat can be steered")))),
                            cx,
                        )
                    })?;
                    settle(cx, 400).await;
                    let (main, pane) = composers(window, cx, &side)?;
                    let chips = &main["chips"];
                    let model_tip = tooltip_text(chips, "modelTooltip");
                    anyhow::ensure!(
                        chips["model"] == "Opus 5.5" && chips["effort"] == "Low" && chips["fast"] == false && chips["modelAttention"] == true,
                        "the model chip shows the running Opus/Low/Standard with a dot: {chips}"
                    );
                    anyhow::ensure!(
                        model_tip.contains("Running: Opus 5.5 · Low") && model_tip.contains("Requested: Sonnet 5.5 · High") && model_tip.contains("Applies when the next turn starts.") && model_tip.contains(long_reason),
                        "the model tooltip names Running, Requested, how it applies and the engine's reason: {model_tip}"
                    );
                    anyhow::ensure!(
                        chips["access"] == "Approve for me" && chips["accessIcon"] == "icons/shield-check.svg" && chips["accessAttention"] == true
                            && tooltip_text(chips, "accessDetail").contains("Running: Approve for me")
                            && tooltip_text(chips, "accessDetail").contains("Requested: Full access"),
                        "access shows the enforced level while the raise is pending: {chips}"
                    );
                    anyhow::ensure!(!model_tip.contains(PARENT) && !tooltip_text(chips, "accessDetail").contains(PARENT), "no task id in the tooltips");
                    anyhow::ensure!(
                        ["model", "effort", "access", "modelAttention", "accessAttention", "modelTooltip"].iter().all(|key| pane["chips"][*key] == baseline_pane["chips"][*key]),
                        "Main's summary stays out of the fork's composer: {} vs {}",
                        pane["chips"],
                        baseline_pane["chips"]
                    );
                    let surface = &main["surface"];
                    let (sx, sy, sh) = (f(&surface["x"]) as f32, f(&surface["y"]) as f32, f(&surface["height"]) as f32);
                    let row_y = sy + sh - 1.0 - 12.0 - 16.0;
                    let chips_x = sx + 1.0 + 12.0 + 28.0 + 2.0;
                    move_mouse(window, cx, chips_x + 24.0, row_y)?;
                    settle(cx, 900).await;
                    capture(window, cx, output.join("e02-running-model-tooltip.png"))?;
                    let model_width = f(&main["controlsRow"]["modelFull"]) as f32;
                    move_mouse(window, cx, chips_x + model_width + 4.0 + 16.0, row_y)?;
                    settle(cx, 900).await;
                    capture(window, cx, output.join("e02b-running-access-tooltip.png"))?;
                    move_mouse(window, cx, 200.0, 300.0)?;
                    settle(cx, 300).await;
                    record.insert("e02".into(), serde_json::json!({"main": main["chips"], "pane": pane["chips"]}));

                    // e03. Failure case 15: Steer follows the engine.
                    let (main_steer, pane_steer) = steer_resolutions(window, cx, &side)?;
                    anyhow::ensure!(
                        main_steer["action"] == "Send now"
                            && main_steer["tooltip"].as_str().is_some_and(|tip| tip.starts_with("Steer unavailable: Pending settings must apply before this chat can be steered.") && tip.ends_with("Send now interrupts the active response.")),
                        "an unavailable Steer offers only Send now with the engine's reason: {main_steer}"
                    );
                    let pane_watch = window.update(cx, |shell, _, cx| shell.fixture_watch_execution(&side, cx))?;
                    let pane_native = if pane_watch.is_null() { true } else { pane_watch["steer"]["kind"] == "native" };
                    anyhow::ensure!((pane_steer["action"] == "Steer now") == pane_native, "the fork's Steer follows its own engine decision: {pane_steer} {pane_watch}");
                    let mut steer_cases = vec![serde_json::json!({"unavailable": main_steer, "pane": pane_steer})];
                    for (kind, reason, action, lead) in [
                        (Steer::Native, None, "Steer now", "Steer now (without interrupting)"),
                        (Steer::AfterTurn, Some("This provider accepts follow-ups only between turns"), "Send now", "Steering applies only after this turn: This provider accepts follow-ups only between turns."),
                    ] {
                        window.update(cx, |shell, _, cx| {
                            shell.fixture_seed_execution(PARENT, Some(exec_summary(PARENT, Phase::Running, main_requested(), Some(main_requested()), &[], (Move::Reuse, false, ""), (kind, reason))), cx)
                        })?;
                        settle(cx, 200).await;
                        let (resolution, _) = steer_resolutions(window, cx, &side)?;
                        anyhow::ensure!(resolution["action"] == action && resolution["tooltip"].as_str().is_some_and(|tip| tip.starts_with(lead)), "steer {kind:?}: {resolution}");
                        steer_cases.push(resolution);
                    }
                    record.insert("e03".into(), serde_json::json!(steer_cases));

                    // e04. Failure case 16: the next turn applied everything.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_seed_execution(PARENT, Some(exec_summary(PARENT, Phase::Idle, main_requested(), None, &[], (Move::Reuse, false, ""), (Steer::AfterTurn, None))), cx)
                    })?;
                    settle(cx, 300).await;
                    let (main, _) = composers(window, cx, &side)?;
                    anyhow::ensure!(legacy(&main["chips"]) && main["chips"]["accessAttention"] == false, "cleared: requested labels, no dots: {} vs {}", main["chips"], baseline["chips"]);
                    record.insert("e04".into(), main["chips"].clone());

                    // e05. Failure cases 17-18: a parked warm runtime, then a
                    // provider change that needs a new binding.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_seed_execution(PARENT, Some(exec_summary(PARENT, Phase::Warm, main_requested(), Some(opus_running()), &CHIP_PENDING, (Move::RestartAndResume, false, ""), (Steer::AfterTurn, None))), cx)
                    })?;
                    settle(cx, 300).await;
                    let (main, _) = composers(window, cx, &side)?;
                    let chips = &main["chips"];
                    let warm_tip = tooltip_text(chips, "modelTooltip");
                    anyhow::ensure!(
                        chips["model"] == baseline["chips"]["model"] && chips["effort"] == baseline["chips"]["effort"] && chips["modelAttention"] == true
                            && warm_tip.contains("Loaded: Opus 5.5 · Low") && !warm_tip.contains("Running:") && warm_tip.contains("The session restarts and resumes with it."),
                        "a warm runtime reads Loaded and keeps the requested labels: {chips}"
                    );
                    anyhow::ensure!(chips["access"] == "Full access" && tooltip_text(chips, "accessDetail").contains("Loaded: Approve for me"), "warm access shows the requested level and the loaded one: {chips}");
                    let mut provider_change = main_requested();
                    provider_change.provider = HarnessId::Codex;
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_seed_execution(PARENT, Some(exec_summary(PARENT, Phase::Running, provider_change.clone(), Some(main_requested()), &[Field::Provider, Field::Model], (Move::NewBinding, true, ""), (Steer::Unavailable, Some("A provider change needs a new session")))), cx)
                    })?;
                    settle(cx, 300).await;
                    let (main, _) = composers(window, cx, &side)?;
                    let binding_tip = tooltip_text(&main["chips"], "modelTooltip");
                    anyhow::ensure!(
                        binding_tip.contains("After this turn, a new provider session starts. Earlier context is not carried over automatically.")
                            && !binding_tip.to_lowercase().contains("context is carried over")
                            && !binding_tip.to_lowercase().contains("copied"),
                        "a new binding never promises context transfer: {binding_tip}"
                    );
                    record.insert("e05".into(), serde_json::json!({"warm": chips, "newBinding": binding_tip}));

                    // e06. Failure case 19: missing, old-schema and other-task
                    // summaries all read as the legacy baseline.
                    let mut old_schema = exec_summary(PARENT, Phase::Running, main_requested(), Some(opus_running()), &CHIP_PENDING, (Move::ApplyNextTurn, true, ""), (Steer::Unavailable, Some("old")));
                    old_schema.schema_version = luna_proto::EXECUTION_SUMMARY_VERSION + 1;
                    let mut other_task = old_schema.clone();
                    other_task.schema_version = luna_proto::EXECUTION_SUMMARY_VERSION;
                    other_task.task_id = side.clone();
                    let mut fallbacks = Vec::new();
                    for (name, seeded) in [("missing", None), ("oldSchema", Some(old_schema)), ("otherTask", Some(other_task))] {
                        window.update(cx, |shell, _, cx| shell.fixture_seed_execution(PARENT, seeded, cx))?;
                        settle(cx, 300).await;
                        let (main, _) = composers(window, cx, &side)?;
                        let (steer, _) = steer_resolutions(window, cx, &side)?;
                        anyhow::ensure!(legacy(&main["chips"]) && main["chips"]["accessAttention"] == baseline["chips"]["accessAttention"] && main["chips"]["execution"].is_null() && steer == baseline_steer, "{name}: legacy chips and Steer, no claims: {} {steer}", main["chips"]);
                        fallbacks.push(serde_json::json!({"case": name, "chips": main["chips"], "steer": steer}));
                    }
                    record.insert("e06".into(), serde_json::json!(fallbacks));

                    // e07. Failure cases 20-23: the Execution card follows the
                    // focused fork tab, discards a late response for the
                    // previous focus, and labels declarations as such.
                    anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(&side, window, cx))?, "focus the fork's composer");
                    let mut focus = None;
                    for _ in 0..40 {
                        settle(cx, 100).await;
                        focus = window.update(cx, |shell, _, cx| shell.fixture_execution_focus(cx))?;
                        if focus.as_deref() == Some(side.as_str()) {
                            break;
                        }
                    }
                    anyhow::ensure!(focus.as_deref() == Some(side.as_str()), "diagnostics follow the focused fork tab, not Main: {focus:?}");
                    let long_note = "Model, reasoning and speed are the settings this engine admitted for the running turn; the provider does not independently report them back, so they are not confirmed by the provider itself.";
                    let mut fork_summary = exec_summary(&side, Phase::Running, main_requested(), Some(opus_running()), &CHIP_PENDING, (Move::ApplyNextTurn, true, long_reason), (Steer::Unavailable, Some("Pending settings must apply before this chat can be steered")));
                    fork_summary.observation_note = Some(long_note.into());
                    fork_summary.capabilities = Some(luna_proto::ProviderExecutionCapabilities {
                        provider: HarnessId::Mock,
                        steering: SteeringMode::StepBoundary,
                        supports_steering: true,
                        authoritative_prompt_end: true,
                        deterministic_turn_end: true,
                        safe_recovery_continuation: false,
                        native_fork: luna_proto::ProviderForkCapability::None,
                        runtime_delivery: luna_proto::RuntimeDelivery::Unsupported,
                        permissions: luna_proto::ProviderPermissionCapabilities {
                            supported_modes: vec![PermissionMode::Auto, PermissionMode::Full],
                            supports_workspace_sandbox: true,
                            supports_automatic_reviewer: true,
                            supports_live_mode_change: false,
                            supports_task_allowance: false,
                            approval_kinds: Vec::new(),
                            network_control: "provider".into(),
                            known_limitations: Vec::new(),
                        },
                    });
                    window.update(cx, |shell, _, cx| shell.fixture_seed_execution(&side, Some(fork_summary), cx))?;
                    for _ in 0..3 {
                        window.update(cx, |shell, _, cx| shell.fixture_open_execution_card(cx))?;
                        settle(cx, 300).await;
                    }
                    let mut card = serde_json::Value::Null;
                    for _ in 0..40 {
                        card = window.update(cx, |shell, _, cx| shell.fixture_execution_card(cx))?;
                        if card["focus"] == side.as_str() && card["rows"].is_array() {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    let rows = card["rows"].as_array().cloned().unwrap_or_default();
                    let row = |name: &str| rows.iter().find(|row| row["setting"] == name).cloned().unwrap_or_default();
                    anyhow::ensure!(card["focus"] == side.as_str() && card["chatTitle"].as_str().is_some_and(|title| !title.is_empty() && title != side), "the card names the focused fork by title: {card}");
                    anyhow::ensure!(
                        card["runningHeading"] == "Running" && row("Model")["running"] == OPUS && row("Model")["requested"] == SONNET && row("Model")["pending"] == true
                            && row("Access")["running"] == "Approve for me" && row("Access")["requested"] == "Full access" && row("Access")["pending"] == true
                            && row("Provider")["pending"] == false && row("Sandbox")["pending"] == false,
                        "the card's rows and pending marks follow the engine: {card}"
                    );
                    anyhow::ensure!(
                        card["declared"].as_str().is_some_and(|text| text.contains("not verified for this session")) && card["note"] == long_note
                            && card["applies"].as_array().is_some_and(|lines| lines.iter().any(|line| line == long_reason))
                            && card["steering"].as_str().is_some_and(|text| text.starts_with("Unavailable.")),
                        "declarations, note, how it applies and steering: {card}"
                    );
                    let card_text = card.to_string();
                    anyhow::ensure!(!card_text.contains(PARENT) || card["contractTask"] == PARENT, "the card never renders Main's task id as text: {card}");
                    // Scroll the Providers page's real scroll container until
                    // the card is in view, then prove it from painted bounds.
                    draw(window, cx)?;
                    let before = window.update(cx, |shell, _, cx| shell.fixture_execution_geometry(cx))?;
                    anyhow::ensure!(before["card"].is_object() && before["summary"].is_object(), "the card and its summary were painted: {before}");
                    let viewport_h = f(&before["viewport"]["h"]);
                    let card_h = f(&before["card"]["h"]);
                    let delta = f(&before["card"]["y"]) - f(&before["viewport"]["y"]) - 16.0;
                    window.update(cx, |shell, _, cx| shell.fixture_scroll_providers(delta as f32, cx))?;
                    draw(window, cx)?;
                    settle(cx, 200).await;
                    draw(window, cx)?;
                    let scrolled = window.update(cx, |shell, _, cx| shell.fixture_execution_geometry(cx))?;
                    let (top, bottom) = (f(&scrolled["viewport"]["y"]), f(&scrolled["viewport"]["y"]) + f(&scrolled["viewport"]["h"]));
                    let card_top = f(&scrolled["card"]["y"]);
                    anyhow::ensure!(
                        (delta <= 0.5 || f(&scrolled["offset"]) < f(&before["offset"])) && card_top >= top - 0.5 && card_top < bottom,
                        "the real scroll container brought the card's top into view: {before} -> {scrolled}"
                    );
                    capture(window, cx, output.join("e07-execution-card.png"))?;
                    let mut card_views = vec![scrolled.clone()];
                    if card_h <= viewport_h - 16.0 {
                        anyhow::ensure!(
                            card_top + f(&scrolled["card"]["h"]) <= bottom + 0.5,
                            "the whole Execution card is visible: {scrolled}"
                        );
                    } else {
                        // Taller than the viewport: a second capture with the
                        // summary's end (long note, declarations) in view.
                        let summary_bottom = f(&scrolled["summary"]["y"]) + f(&scrolled["summary"]["h"]);
                        window.update(cx, |shell, _, cx| shell.fixture_scroll_providers((summary_bottom - bottom + 16.0) as f32, cx))?;
                        draw(window, cx)?;
                        settle(cx, 200).await;
                        draw(window, cx)?;
                        let lower = window.update(cx, |shell, _, cx| shell.fixture_execution_geometry(cx))?;
                        let lower_bottom = f(&lower["viewport"]["y"]) + f(&lower["viewport"]["h"]);
                        anyhow::ensure!(
                            f(&lower["summary"]["y"]) + f(&lower["summary"]["h"]) <= lower_bottom + 0.5,
                            "the summary's end (note, declarations) is visible: {lower}"
                        );
                        capture(window, cx, output.join("e07b-execution-card-lower.png"))?;
                        card_views.push(lower);
                    }
                    // A late response: focus moves to Main, then the fork's
                    // contract response arrives.
                    window.update(cx, |shell, _, cx| shell.fixture_leave_settings(false, cx))?;
                    settle(cx, 400).await;
                    anyhow::ensure!(window.update(cx, |shell, window, cx| shell.fixture_workbench_focus_input(PARENT, window, cx))?, "focus Main's composer");
                    for _ in 0..40 {
                        settle(cx, 100).await;
                        focus = window.update(cx, |shell, _, cx| shell.fixture_execution_focus(cx))?;
                        if focus.as_deref() == Some(PARENT) {
                            break;
                        }
                    }
                    anyhow::ensure!(focus.as_deref() == Some(PARENT), "focus returned to Main: {focus:?}");
                    window.update(cx, |shell, _, cx| shell.fixture_open_execution_card(cx))?;
                    settle(cx, 600).await;
                    let generation = window.update(cx, |shell, _, cx| shell.fixture_engine_generation(cx))?;
                    let late = window.update(cx, |shell, _, cx| shell.fixture_deliver_execution_contract(&side, generation, contract_view(&side), cx))?;
                    let current = window.update(cx, |shell, _, cx| shell.fixture_deliver_execution_contract(PARENT, generation, contract_view(PARENT), cx))?;
                    let after = window.update(cx, |shell, _, cx| shell.fixture_execution_card(cx))?;
                    anyhow::ensure!(!late && current && after["focus"] == PARENT && after["contractTask"] == PARENT, "a late response for the fork never overwrites Main's card: late={late} current={current} {after}");
                    record.insert("e07".into(), serde_json::json!({"fork": card, "main": after, "geometry": card_views}));

                    // e08. Failure cases 24-25: the card renders execution
                    // only from the live watch. A contract response that
                    // carried a Running snapshot never revives it once the
                    // watch is invalidated; a same-ID response requested on
                    // an earlier engine connection never installs.
                    window.update(cx, |shell, _, cx| shell.fixture_clear_execution(PARENT, cx))?;
                    let stale_running = exec_summary(PARENT, Phase::Running, main_requested(), Some(opus_running()), &CHIP_PENDING, (Move::ApplyNextTurn, true, ""), (Steer::Unavailable, Some("stale")));
                    let generation = window.update(cx, |shell, _, cx| shell.fixture_engine_generation(cx))?;
                    let installed = window.update(cx, |shell, _, cx| shell.fixture_deliver_execution_contract(PARENT, generation, contract_view_with(PARENT, Some(stale_running)), cx))?;
                    anyhow::ensure!(installed, "the current connection's response installs");
                    window.update(cx, |shell, _, cx| shell.fixture_end_chats_watch(cx))?;
                    let invalidated = window.update(cx, |shell, _, cx| shell.fixture_execution_card(cx))?;
                    let (main, _) = composers(window, cx, &side)?;
                    anyhow::ensure!(
                        invalidated["contractExecutionPhase"] == "running"
                            && invalidated["source"] != "contract"
                            && invalidated["phase"] != "Running a turn"
                            && main["chips"]["model"] == baseline["chips"]["model"]
                            && main["chips"]["execution"]["phase"] != "running",
                        "after the watch ends nothing revives the contract's Running snapshot: {invalidated} {}",
                        main["chips"]
                    );
                    window.update(cx, |shell, _, cx| shell.fixture_reconnect_engine(cx))?;
                    let reconnected = window.update(cx, |shell, _, cx| shell.fixture_engine_generation(cx))?;
                    anyhow::ensure!(reconnected != generation, "a reconnect is a new connection generation");
                    let old_connection = window.update(cx, |shell, _, cx| shell.fixture_deliver_execution_contract(PARENT, generation, contract_view(PARENT), cx))?;
                    let new_connection = window.update(cx, |shell, _, cx| shell.fixture_deliver_execution_contract(PARENT, reconnected, contract_view(PARENT), cx))?;
                    let after_reconnect = window.update(cx, |shell, _, cx| shell.fixture_execution_card(cx))?;
                    anyhow::ensure!(
                        !old_connection && new_connection && after_reconnect["contractGeneration"] == reconnected && after_reconnect["contractExecutionPhase"].is_null(),
                        "a same-ID response from the earlier connection is refused: old={old_connection} new={new_connection} {after_reconnect}"
                    );
                    record.insert("e08".into(), serde_json::json!({"invalidated": invalidated, "afterReconnect": after_reconnect}));
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_clear_execution(PARENT, cx);
                        shell.fixture_clear_execution(&side, cx);
                        shell.fixture_leave_settings(false, cx);
                    })?;
                    settle(cx, 300).await;
                    // ---- Context transfers (failure cases 26-35) ----
                    let findings = |cx: &mut AsyncApp| window.update(cx, |shell, _, cx| shell.fixture_findings_readback(cx));
                    let receipts = |cx: &mut AsyncApp, task: &str| {
                        let task = task.to_string();
                        window.update(cx, move |shell, _, cx| shell.fixture_transfer_receipts(&task, cx))
                    };
                    let main_transfer = |cx: &mut AsyncApp| {
                        window.update(cx, |shell, _, cx| {
                            let (main, _) = shell.fixture_composers(None, cx);
                            main.read(cx).pickers().read(cx).fixture_transfer_readback(cx)
                        })
                    };
                    async fn settled_preview(
                        window: WindowHandle<shell::Shell>,
                        cx: &mut AsyncApp,
                        read: impl Fn(&mut AsyncApp) -> anyhow::Result<serde_json::Value>,
                        flow: impl Fn(&serde_json::Value) -> serde_json::Value,
                    ) -> anyhow::Result<serde_json::Value> {
                        let _ = window;
                        let mut last = serde_json::Value::Null;
                        for _ in 0..80 {
                            last = read(cx)?;
                            if flow(&last)["preview"]["state"] != "loading" {
                                return Ok(last);
                            }
                            settle(cx, 100).await;
                        }
                        anyhow::bail!("the host preview never settled: {last}")
                    }

                    // The destination's next explicit send consumes a planned
                    // transfer (the host refuses another preview from the same
                    // source until then).
                    async fn consume(window: WindowHandle<shell::Shell>, cx: &mut AsyncApp, chat: &str, text: &str) -> anyhow::Result<()> {
                        window.update(cx, |shell, _, cx| shell.fixture_workbench_type(chat, text, true, cx))?;
                        let mut last = serde_json::Value::Null;
                        for _ in 0..100 {
                            settle(cx, 100).await;
                            last = window.update(cx, |shell, _, cx| shell.fixture_transfer_receipts(chat, cx))?;
                            if last.as_array().is_some_and(|items| !items.is_empty() && items.iter().all(|item| item["status"] == "consumed")) {
                                return Ok(());
                            }
                        }
                        anyhow::bail!("the explicit send did not consume its context: {last}")
                    }

                    // t01. Failure cases 26-28: the fork gets Send findings;
                    // Main does not. A new fork message gives it findings.
                    let offered_fork = window.update(cx, |shell, _, cx| shell.fixture_findings_offered(&side, cx))?;
                    let offered_main = window.update(cx, |shell, _, cx| shell.fixture_findings_offered(PARENT, cx))?;
                    anyhow::ensure!(offered_fork.as_deref() == Some(PARENT) && offered_main.is_none(), "findings go only from the fork to its lineage source: fork={offered_fork:?} main={offered_main:?}");
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_type(&side, "Fork finding: the cache migration needs a feature flag.", true, cx))?;
                    for _ in 0..60 {
                        if window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(&side, cx))?.iter().any(|text| text.contains("feature flag")) {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    settle(cx, 1500).await;
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_type(PARENT, "Draft kept while findings arrive", false, cx))?;
                    let parent_draft = window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(PARENT, cx))?;
                    let parent_texts = window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(PARENT, cx))?;
                    let receipts_before = receipts(cx, PARENT)?.as_array().map_or(0, Vec::len);
                    window.update(cx, |shell, _, cx| shell.fixture_open_findings(&side, cx))?;
                    let opened = settled_preview(window, cx, findings, |read| read["flow"].clone()).await?;
                    anyhow::ensure!(
                        opened["open"] == true && opened["flow"]["kind"] == "forkMerge" && opened["flow"]["targetTaskId"] == PARENT
                            && opened["flow"]["preview"]["state"] == "ready"
                            && opened["flow"]["preview"]["included"].as_u64().unwrap_or(0) >= 1,
                        "the dialog previews the fork's findings for its source: {opened}"
                    );
                    anyhow::ensure!(receipts(cx, PARENT)?.as_array().map_or(0, Vec::len) == receipts_before, "a preview writes no receipt");
                    capture(window, cx, output.join("t01-findings-dialog.png"))?;
                    window.update(cx, |shell, _, cx| shell.fixture_findings_primary(cx))?;
                    for _ in 0..60 {
                        if findings(cx)?["open"] == false {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    anyhow::ensure!(findings(cx)?["open"] == false, "an applied send closes the dialog: {}", findings(cx)?);
                    settle(cx, 1500).await;
                    let parent_receipts = receipts(cx, PARENT)?;
                    anyhow::ensure!(
                        window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(PARENT, cx))? == parent_texts
                            && window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(PARENT, cx))? == parent_draft
                            && window.update(cx, |shell, _, cx| shell.fixture_watch_execution(PARENT, cx))?["phase"] != "running",
                        "no turn starts in the source and its draft is untouched"
                    );
                    anyhow::ensure!(
                        parent_receipts.as_array().is_some_and(|items| items.iter().any(|item| item["kind"] == "forkMerge" && item["source"] == side.as_str() && item["status"] == "planned" && item["line"].as_str().is_some_and(|line| line.starts_with("Findings from ")))),
                        "the source holds a Planned findings receipt from the fork: {parent_receipts}"
                    );
                    record.insert("t01".into(), serde_json::json!({"dialog": opened, "receipts": parent_receipts}));

                    // t02. Failure cases 29-30: the source advances while the
                    // dialog is open; the stale preview is refused, the draft
                    // stays, a refresh shows only new findings; a lost answer
                    // reads unknown and Check status sends nothing new.
                    consume(window, cx, PARENT, "Parent reads the findings.").await?;
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_type(&side, "Fork finding two: settings rename is safe.", true, cx))?;
                    settle(cx, 2000).await;
                    window.update(cx, |shell, _, cx| shell.fixture_open_findings(&side, cx))?;
                    settled_preview(window, cx, findings, |read| read["flow"].clone()).await?;
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_type(PARENT, "The parent moves on.", true, cx))?;
                    settle(cx, 2000).await;
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_type(PARENT, "Second draft stays", false, cx))?;
                    let second_draft = window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(PARENT, cx))?;
                    window.update(cx, |shell, _, cx| shell.fixture_findings_primary(cx))?;
                    let mut conflicted = serde_json::Value::Null;
                    for _ in 0..60 {
                        conflicted = findings(cx)?;
                        if conflicted["flow"]["preview"]["state"] == "stale" {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    anyhow::ensure!(
                        conflicted["open"] == true && conflicted["flow"]["preview"]["state"] == "stale" && conflicted["flow"]["applyAttempts"] == 1
                            && window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(PARENT, cx))? == second_draft,
                        "the stale preview is refused and the draft is kept: {conflicted}"
                    );
                    capture(window, cx, output.join("t02-findings-stale.png"))?;
                    window.update(cx, |shell, _, cx| shell.fixture_findings_refresh(cx))?;
                    let refreshed = settled_preview(window, cx, findings, |read| read["flow"].clone()).await?;
                    anyhow::ensure!(
                        refreshed["flow"]["preview"]["state"] == "ready" && refreshed["flow"]["preview"]["alreadyDelivered"].as_u64().unwrap_or(0) >= 1,
                        "the refreshed preview leaves already delivered findings out: {refreshed}"
                    );
                    let planned_before = receipts(cx, PARENT)?.as_array().map_or(0, |items| items.iter().filter(|item| item["kind"] == "forkMerge").count());
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_findings_lose_next_apply();
                        shell.fixture_findings_primary(cx);
                    })?;
                    let mut unknown = serde_json::Value::Null;
                    for _ in 0..60 {
                        unknown = findings(cx)?;
                        if unknown["flow"]["apply"]["state"] == "unknown" {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    anyhow::ensure!(unknown["flow"]["apply"]["state"] == "unknown" && unknown["flow"]["applyAttempts"] == 2, "a lost answer reads unknown: {unknown}");
                    capture(window, cx, output.join("t02b-findings-unknown.png"))?;
                    // Failure case 39: Refresh refuses while unknown.
                    window.update(cx, |shell, _, cx| shell.fixture_findings_refresh(cx))?;
                    settle(cx, 300).await;
                    let after_refresh = findings(cx)?;
                    anyhow::ensure!(
                        after_refresh["flow"]["apply"]["state"] == "unknown"
                            && after_refresh["flow"]["apply"]["commandId"] == unknown["flow"]["apply"]["commandId"]
                            && after_refresh["flow"]["applyAttempts"] == 2
                            && after_refresh["flow"]["preview"]["previewId"] == unknown["flow"]["preview"]["previewId"],
                        "Refresh keeps an unknown apply and its command id: {after_refresh}"
                    );
                    window.update(cx, |shell, _, cx| shell.fixture_findings_primary(cx))?;
                    for _ in 0..60 {
                        if findings(cx)?["open"] == false {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    let planned_after = receipts(cx, PARENT)?.as_array().map_or(0, |items| items.iter().filter(|item| item["kind"] == "forkMerge").count());
                    anyhow::ensure!(findings(cx)?["open"] == false && planned_after == planned_before + 1, "Check status settles it without a second send: {planned_before} -> {planned_after}");
                    record.insert("t02".into(), serde_json::json!({"conflicted": conflicted, "refreshed": refreshed, "unknown": unknown}));

                    // t03. Failure case 31: a preview from an earlier engine
                    // connection is never applied.
                    consume(window, cx, PARENT, "Parent reads finding two.").await?;
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_type(&side, "Fork finding three.", true, cx))?;
                    settle(cx, 2000).await;
                    window.update(cx, |shell, _, cx| shell.fixture_open_findings(&side, cx))?;
                    settled_preview(window, cx, findings, |read| read["flow"].clone()).await?;
                    window.update(cx, |shell, _, cx| shell.fixture_reconnect_engine(cx))?;
                    draw(window, cx)?;
                    window.update(cx, |shell, _, cx| shell.fixture_findings_primary(cx))?;
                    settle(cx, 300).await;
                    let reconnected = findings(cx)?;
                    anyhow::ensure!(
                        reconnected["current"] == false && reconnected["flow"]["applyAttempts"] == 0 && reconnected["flow"]["preview"]["state"] == "stale",
                        "an earlier connection's preview is stale and unsent: {reconnected}"
                    );
                    window.update(cx, |shell, _, cx| shell.fixture_findings_refresh(cx))?;
                    let fresh = settled_preview(window, cx, findings, |read| read["flow"].clone()).await?;
                    anyhow::ensure!(fresh["current"] == true && fresh["flow"]["preview"]["state"] == "ready", "a refresh on the new connection is current: {fresh}");
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_findings_primary(cx);
                    })?;
                    for _ in 0..60 {
                        if findings(cx)?["open"] == false {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    record.insert("t03".into(), serde_json::json!({"reconnected": reconnected, "fresh": fresh}));

                    // t03b. Failure case 36: Start new task with context.
                    let lineage = |cx: &mut AsyncApp, id: &str| {
                        let id = id.to_string();
                        window.update(cx, move |shell, _, cx| shell.fixture_chat_lineage(&id, cx))
                    };
                    anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_new_task_with_context_offered(PARENT, cx))?, "Main can start a new task with its context");
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_type(PARENT, "Source draft stays put", false, cx))?;
                    let source_draft = window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(PARENT, cx))?;
                    let chats_before = lineage(cx, PARENT)?["chatCount"].as_u64().unwrap_or(0);
                    window.update(cx, |shell, _, cx| shell.fixture_open_new_task_with_context(PARENT, cx))?;
                    let cancelled_preview = settled_preview(window, cx, findings, |read| read["flow"].clone()).await?;
                    let cancelled_target = cancelled_preview["flow"]["targetTaskId"].as_str().unwrap_or_default().to_string();
                    anyhow::ensure!(
                        cancelled_preview["mode"] == "newTask" && cancelled_preview["flow"]["kind"] == "portableFork"
                            && cancelled_preview["flow"]["preview"]["state"] == "ready" && cancelled_preview["config"].as_str().is_some_and(|config| !config.is_empty())
                            && !cancelled_target.is_empty(),
                        "the dialog previews into a reserved id and shows the config: {cancelled_preview}"
                    );
                    window.update(cx, |shell, _, cx| shell.fixture_findings_refresh(cx))?;
                    let refreshed_new = settled_preview(window, cx, findings, |read| read["flow"].clone()).await?;
                    anyhow::ensure!(refreshed_new["flow"]["targetTaskId"] == cancelled_target.as_str(), "a refresh keeps the reserved id: {refreshed_new}");
                    capture(window, cx, output.join("t03b-new-task-with-context.png"))?;
                    window.update(cx, |shell, _, cx| shell.fixture_close_transfer_dialog(cx))?;
                    settle(cx, 1000).await;
                    anyhow::ensure!(
                        lineage(cx, &cancelled_target)?["exists"] == false && lineage(cx, PARENT)?["chatCount"].as_u64().unwrap_or(0) == chats_before,
                        "preview and Cancel create nothing"
                    );
                    window.update(cx, |shell, _, cx| shell.fixture_open_new_task_with_context(PARENT, cx))?;
                    let creating = settled_preview(window, cx, findings, |read| read["flow"].clone()).await?;
                    let new_task = creating["flow"]["targetTaskId"].as_str().unwrap_or_default().to_string();
                    anyhow::ensure!(!new_task.is_empty() && new_task != cancelled_target, "each dialog reserves its own id: {creating}");
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_findings_lose_next_apply();
                        shell.fixture_findings_primary(cx);
                    })?;
                    let mut lost = serde_json::Value::Null;
                    for _ in 0..60 {
                        lost = findings(cx)?;
                        if lost["flow"]["apply"]["state"] == "unknown" {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    anyhow::ensure!(
                        lost["flow"]["apply"]["state"] == "unknown" && lost["flow"]["applyAttempts"] == 1 && lost["flow"]["targetTaskId"] == new_task.as_str(),
                        "a lost answer reads unknown and keeps the same id: {lost}"
                    );
                    window.update(cx, |shell, _, cx| shell.fixture_findings_primary(cx))?;
                    let mut created = serde_json::Value::Null;
                    for _ in 0..80 {
                        created = lineage(cx, &new_task)?;
                        if findings(cx)?["open"] == false && created["exists"] == true && created["selected"] == true {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    settle(cx, 1000).await;
                    let created = lineage(cx, &new_task)?;
                    let created_exec = window.update(cx, |shell, _, cx| shell.fixture_watch_execution(&new_task, cx))?;
                    let created_receipts = receipts(cx, &new_task)?;
                    anyhow::ensure!(
                        created["exists"] == true && created["selected"] == true && created["childThread"] == false
                            && created["chatCount"].as_u64().unwrap_or(0) == chats_before + 1
                            && created["harness"] == lineage(cx, PARENT)?["harness"],
                        "Check status recovers exactly one ordinary task on the source's config, and opens it: {created}"
                    );
                    anyhow::ensure!(
                        window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(&new_task, cx))?.is_empty()
                            && created_exec["running"].is_null() && created_exec["phase"] != "running",
                        "the new task has no turn and no native session: {created_exec}"
                    );
                    anyhow::ensure!(
                        created_receipts.as_array().is_some_and(|items| items.iter().any(|item| item["kind"] == "portableFork" && item["status"] == "planned" && item["source"] == PARENT)),
                        "the new task holds the Planned context copy from its source: {created_receipts}"
                    );
                    // Its first explicit send takes the captured context.
                    consume(window, cx, &new_task, "First message in the new task.").await?;
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_open_as_main(PARENT, cx))?;
                    for _ in 0..60 {
                        if window.update(cx, |shell, _, cx| shell.fixture_workbench_readback(cx))?["main"] == PARENT {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    settle(cx, 600).await;
                    anyhow::ensure!(window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(PARENT, cx))? == source_draft, "the source's draft is untouched");
                    record.insert("t03b".into(), serde_json::json!({"preview": creating, "lost": lost, "created": created, "receipts": created_receipts}));

                    // t06b. Failure case 37: a long earlier fork message is
                    // left out of an oversized findings transfer; Details reads
                    // it in segments with Read more, each continuing exactly at
                    // the returned nextTextOffset.
                    consume(window, cx, PARENT, "Parent reads finding three.").await?;
                    let earlier: String = std::iter::repeat("naïve café ✓ Ω ").take(2600).collect::<String>() + "END-OF-EARLIER";
                    let latest: String = "Latest finding: ".to_string() + &"z".repeat(110 * 1024);
                    for text in [earlier.clone(), latest] {
                        let side_id = side.clone();
                        window.update(cx, move |shell, _, cx| shell.fixture_workbench_type(&side_id, &text, true, cx))?;
                        settle(cx, 2500).await;
                    }
                    window.update(cx, |shell, _, cx| shell.fixture_open_findings(&side, cx))?;
                    let oversized = settled_preview(window, cx, findings, |read| read["flow"].clone()).await?;
                    anyhow::ensure!(
                        oversized["flow"]["preview"]["state"] == "ready" && oversized["flow"]["preview"]["omitted"].as_u64().unwrap_or(0) >= 1,
                        "the oversized findings leave the earlier message out: {oversized}"
                    );
                    window.update(cx, |shell, _, cx| shell.fixture_findings_primary(cx))?;
                    for _ in 0..60 {
                        if findings(cx)?["open"] == false {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    settle(cx, 800).await;
                    let main_pickers_call = |cx: &mut AsyncApp, f: Box<dyn FnOnce(&mut pickers::Pickers, &mut gpui::Context<pickers::Pickers>)>| {
                        window.update(cx, move |shell, _, cx| {
                            let (main, _) = shell.fixture_composers(None, cx);
                            let pickers = main.read(cx).pickers().clone();
                            pickers.update(cx, f);
                        })
                    };
                    window.update(cx, |shell, window, cx| {
                        let (main, _) = shell.fixture_composers(None, cx);
                        let pickers = main.read(cx).pickers().clone();
                        pickers.update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
                    })?;
                    main_pickers_call(cx, Box::new(|pickers: &mut pickers::Pickers, cx: &mut gpui::Context<pickers::Pickers>| {
                        pickers.fixture_open_tray_transfer(0, cx);
                    }))?;
                    let mut first = serde_json::Value::Null;
                    for _ in 0..60 {
                        first = main_transfer(cx)?["details"].clone();
                        if first["loading"] == false && first["kind"] == "forkMerge" {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    let items = first["items"].as_array().cloned().unwrap_or_default();
                    let index = items
                        .iter()
                        .position(|item| item["available"] == true && item["truncated"] == true && item["nextTextOffset"].is_u64())
                        .ok_or_else(|| anyhow::anyhow!("a truncated available omitted item: {first}"))?;
                    let before = items[index].clone();
                    anyhow::ensure!(
                        before["label"] == "excerpt" && before["textBytes"].as_u64().unwrap_or(0) <= 12_288,
                        "a first segment is bounded and labelled as an excerpt: {before}"
                    );
                    let offset = before["nextTextOffset"].as_u64().unwrap_or(0);
                    let read_more = window.update(cx, move |shell, _, cx| {
                        let (main, _) = shell.fixture_composers(None, cx);
                        let pickers = main.read(cx).pickers().clone();
                        pickers.update(cx, |pickers, cx| pickers.fixture_transfer_read_more(index, cx))
                    })?;
                    anyhow::ensure!(read_more, "Read more is offered for the truncated item");
                    let mut after = serde_json::Value::Null;
                    for _ in 0..60 {
                        after = main_transfer(cx)?["details"]["items"][index].clone();
                        if after["segments"].as_u64().unwrap_or(0) >= 2 || after["error"].is_string() {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    anyhow::ensure!(
                        after["messageId"] == before["messageId"]
                            && after["segments"] == 2
                            && after["segmentStarts"][1] == offset
                            && after["textBytes"].as_u64().unwrap_or(0) > before["textBytes"].as_u64().unwrap_or(0)
                            && after["prefixKept"] == true
                            && (after["truncated"] == false || after["label"] == "excerpt"),
                        "Read more appends exactly the same message from nextTextOffset: {before} -> {after}"
                    );
                    anyhow::ensure!(main_transfer(cx)?["open"] == true, "Details are in the mounted model panel");
                    settle(cx, 300).await;
                    capture(window, cx, output.join("t06b-details-read-more.png"))?;
                    record.insert("t06b".into(), serde_json::json!({"preview": oversized, "before": before, "after": after}));
                    main_pickers_call(cx, Box::new(|pickers: &mut pickers::Pickers, cx: &mut gpui::Context<pickers::Pickers>| {
                        pickers.fixture_close_transfer_details(cx);
                        pickers.fixture_close(cx);
                    }))?;

                    // t03c. Failure cases 38-39 (apply): a new task with the
                    // fork's context is applied, and the engine connection
                    // changes before the answer arrives. The late answer must
                    // not settle the dialog, open the created task or notify;
                    // Refresh refuses while it is in flight.
                    consume(window, cx, PARENT, "Parent reads the long findings.").await?;
                    window.update(cx, |shell, _, cx| shell.fixture_open_new_task_with_context(&side, cx))?;
                    let racing = settled_preview(window, cx, findings, |read| read["flow"].clone()).await?;
                    anyhow::ensure!(racing["flow"]["preview"]["state"] == "ready", "the fork's new-task preview is ready: {racing}");
                    let race_target = racing["flow"]["targetTaskId"].as_str().unwrap_or_default().to_string();
                    let generation_before = window.update(cx, |shell, _, cx| shell.fixture_engine_generation(cx))?;
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_findings_primary(cx);
                        shell.fixture_findings_refresh(cx);
                        shell.fixture_reconnect_engine(cx);
                    })?;
                    settle(cx, 2500).await;
                    let late_apply = findings(cx)?;
                    anyhow::ensure!(
                        window.update(cx, |shell, _, cx| shell.fixture_engine_generation(cx))? != generation_before
                            && late_apply["open"] == true
                            && late_apply["flow"]["apply"]["state"] == "applying"
                            && late_apply["flow"]["applyAttempts"] == 1
                            && late_apply["flow"]["targetTaskId"] == race_target.as_str(),
                        "a late apply answer from the earlier connection settles nothing (and Refresh refused): {late_apply}"
                    );
                    anyhow::ensure!(
                        lineage(cx, PARENT)?["selected"] == true && lineage(cx, &race_target)?["selected"] == false,
                        "the late answer never opens the created task"
                    );
                    window.update(cx, |shell, _, cx| shell.fixture_close_transfer_dialog(cx))?;
                    record.insert("t03c".into(), serde_json::json!({"lateApply": late_apply}));

                    // t04. Failure case 32: the provider switch has no default
                    // choice; its primary acts only after one; a target change
                    // drops the preview.
                    window.update(cx, |shell, window, cx| {
                        let (main, _) = shell.fixture_composers(None, cx);
                        let pickers = main.read(cx).pickers().clone();
                        pickers.update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
                    })?;
                    let pickers_do = |cx: &mut AsyncApp, f: Box<dyn FnOnce(&mut pickers::Pickers, &mut gpui::Context<pickers::Pickers>)>| {
                        window.update(cx, move |shell, _, cx| {
                            let (main, _) = shell.fixture_composers(None, cx);
                            let pickers = main.read(cx).pickers().clone();
                            pickers.update(cx, f);
                        })
                    };
                    pickers_do(cx, Box::new(|pickers: &mut pickers::Pickers, cx: &mut gpui::Context<pickers::Pickers>| pickers.fixture_begin_switch(HarnessId::Opencode, cx)))?;
                    let unchosen = main_transfer(cx)?;
                    let acted = window.update(cx, |shell, _, cx| {
                        let (main, _) = shell.fixture_composers(None, cx);
                        let pickers = main.read(cx).pickers().clone();
                        pickers.update(cx, |pickers, cx| pickers.fixture_switch_primary(cx))
                    })?;
                    anyhow::ensure!(
                        unchosen["supported"] == true && unchosen["choice"].is_null() && unchosen["primary"]["enabled"] == false && !acted,
                        "no choice is preselected and the primary waits for one: {unchosen}"
                    );
                    pickers_do(cx, Box::new(|pickers: &mut pickers::Pickers, cx: &mut gpui::Context<pickers::Pickers>| pickers.fixture_choose_switch(true, cx)))?;
                    let continued = settled_preview(window, cx, main_transfer, |read| read["flow"].clone()).await?;
                    anyhow::ensure!(continued["open"] == true, "The switch panel is mounted");
                    settle(cx, 300).await;
                    capture(window, cx, output.join("t04-provider-switch.png"))?;
                    anyhow::ensure!(
                        continued["choice"] == "Continue" && continued["flow"]["kind"] == "providerHandoff" && continued["flow"]["preview"]["state"] == "ready"
                            && continued["primary"]["label"] == "Switch and continue" && continued["primary"]["enabled"] == true,
                        "Continue shows the host's handoff preview: {continued}"
                    );
                    pickers_do(cx, Box::new(|pickers: &mut pickers::Pickers, cx: &mut gpui::Context<pickers::Pickers>| pickers.fixture_begin_switch(HarnessId::Codex, cx)))?;
                    let retargeted = main_transfer(cx)?;
                    anyhow::ensure!(retargeted["flow"].is_null() && retargeted["choice"].is_null(), "a new target provider drops the old preview and choice: {retargeted}");
                    pickers_do(cx, Box::new(|pickers: &mut pickers::Pickers, cx: &mut gpui::Context<pickers::Pickers>| pickers.fixture_choose_switch(false, cx)))?;
                    let fresh_choice = main_transfer(cx)?;
                    anyhow::ensure!(fresh_choice["primary"]["label"] == "Switch fresh" && fresh_choice["flow"].is_null(), "Start fresh carries no preview: {fresh_choice}");
                    record.insert("t04".into(), serde_json::json!({"unchosen": unchosen, "continued": continued, "fresh": fresh_choice}));

                    // t05. Failure cases 33, 38-39 (Check status): Continue
                    // with context stores the handoff without a turn; its answer
                    // is lost (unknown); Refresh refuses and keeps the command
                    // id; a Check status answer arriving after the connection
                    // changed settles nothing. The panel is then cancelled
                    // (never resent); the host's receipt remains.
                    pickers_do(cx, Box::new(|pickers: &mut pickers::Pickers, cx: &mut gpui::Context<pickers::Pickers>| pickers.fixture_begin_switch(HarnessId::Opencode, cx)))?;
                    pickers_do(cx, Box::new(|pickers: &mut pickers::Pickers, cx: &mut gpui::Context<pickers::Pickers>| pickers.fixture_choose_switch(true, cx)))?;
                    settled_preview(window, cx, main_transfer, |read| read["flow"].clone()).await?;
                    let texts_before_switch = window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(PARENT, cx))?;
                    let switched = window.update(cx, |shell, _, cx| {
                        let (main, _) = shell.fixture_composers(None, cx);
                        let pickers = main.read(cx).pickers().clone();
                        pickers.update(cx, |pickers, cx| {
                            pickers.fixture_lose_next_transfer_apply();
                            pickers.fixture_switch_primary(cx)
                        })
                    })?;
                    anyhow::ensure!(switched, "the enabled primary applies the handoff");
                    let mut lost_switch = serde_json::Value::Null;
                    for _ in 0..60 {
                        lost_switch = main_transfer(cx)?;
                        if lost_switch["flow"]["apply"]["state"] == "unknown" {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    anyhow::ensure!(lost_switch["flow"]["apply"]["state"] == "unknown" && lost_switch["primary"]["label"] == "Check status", "a lost handoff answer reads unknown: {lost_switch}");
                    pickers_do(cx, Box::new(|pickers: &mut pickers::Pickers, cx: &mut gpui::Context<pickers::Pickers>| {
                        pickers.fixture_refresh_transfer(cx);
                        pickers.fixture_begin_switch(HarnessId::Codex, cx);
                    }))?;
                    let kept = main_transfer(cx)?;
                    anyhow::ensure!(
                        kept["flow"]["apply"]["commandId"] == lost_switch["flow"]["apply"]["commandId"]
                            && kept["flow"]["applyAttempts"] == 1
                            && kept["switchTarget"] == lost_switch["switchTarget"],
                        "Refresh and a new target are refused while unknown: {kept}"
                    );
                    window.update(cx, |shell, _, cx| {
                        let (main, _) = shell.fixture_composers(None, cx);
                        let pickers = main.read(cx).pickers().clone();
                        pickers.update(cx, |pickers, cx| {
                            pickers.fixture_switch_primary(cx);
                        });
                        shell.fixture_reconnect_engine(cx);
                    })?;
                    settle(cx, 2500).await;
                    let late_check = main_transfer(cx)?;
                    let main_receipts = receipts(cx, PARENT)?;
                    anyhow::ensure!(
                        late_check["flow"]["apply"]["state"] == "unknown"
                            && late_check["flow"]["apply"]["commandId"] == lost_switch["flow"]["apply"]["commandId"]
                            && late_check["flow"]["applyAttempts"] == 1
                            && late_check["switchTarget"] == lost_switch["switchTarget"],
                        "a late Check status answer from the earlier connection settles nothing: {late_check}"
                    );
                    anyhow::ensure!(
                        main_receipts.as_array().is_some_and(|items| items.iter().any(|item| item["kind"] == "providerHandoff"))
                            && window.update(cx, |shell, _, cx| shell.fixture_workbench_user_texts(PARENT, cx))? == texts_before_switch,
                        "the host stored the handoff with a receipt and no turn started: {main_receipts}"
                    );
                    pickers_do(cx, Box::new(|pickers: &mut pickers::Pickers, cx: &mut gpui::Context<pickers::Pickers>| pickers.fixture_cancel_switch(cx)))?;
                    record.insert("t05".into(), serde_json::json!({"lost": lost_switch, "lateCheck": late_check, "receipts": main_receipts}));

                    // t06. Failure case 34: Details for the newest receipt.
                    let opened_details = window.update(cx, |shell, _, cx| {
                        let (main, _) = shell.fixture_composers(None, cx);
                        let pickers = main.read(cx).pickers().clone();
                        pickers.update(cx, |pickers, cx| pickers.fixture_open_tray_transfer(0, cx))
                    })?;
                    let mut details = serde_json::Value::Null;
                    for _ in 0..40 {
                        details = main_transfer(cx)?["details"].clone();
                        if details["loading"] == false {
                            break;
                        }
                        settle(cx, 100).await;
                    }
                    anyhow::ensure!(opened_details && details["transferId"].is_string() && details["error"].is_null(), "Details open and read without error: {details}");
                    let card_line = {
                        for _ in 0..3 {
                            window.update(cx, |shell, _, cx| shell.fixture_open_execution_card(cx))?;
                            settle(cx, 300).await;
                        }
                        window.update(cx, |shell, _, cx| shell.fixture_execution_card(cx))?["lastTransfer"].clone()
                    };
                    window.update(cx, |shell, _, cx| shell.fixture_leave_settings(false, cx))?;
                    anyhow::ensure!(card_line.is_string(), "the Execution card names the last context transfer: {card_line}");
                    record.insert("t06".into(), serde_json::json!({"details": details, "card": card_line}));

                    // t07. Failure case 35: deleting the fork keeps the
                    // source's receipts and names the fork as deleted.
                    window.update(cx, |shell, _, cx| shell.fixture_chat_action(&side, "delete", cx))?;
                    let mut kept = serde_json::Value::Null;
                    for _ in 0..80 {
                        settle(cx, 150).await;
                        kept = receipts(cx, PARENT)?;
                        let gone = window.update(cx, |shell, _, cx| shell.fixture_findings_offered(&side, cx))?.is_none();
                        if gone && kept.as_array().is_some_and(|items| items.iter().any(|item| item["line"].as_str().is_some_and(|line| line.contains("(deleted)")))) {
                            break;
                        }
                    }
                    anyhow::ensure!(
                        kept.as_array().is_some_and(|items| items.iter().filter(|item| item["kind"] == "forkMerge").count() >= 2
                            && items.iter().any(|item| item["line"].as_str().is_some_and(|line| line.starts_with("Findings from ") && line.contains("(deleted)")))),
                        "the source keeps its findings receipts, naming the fork as deleted: {kept}"
                    );
                    record.insert("t07".into(), kept);
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
                                output.join("composer-parity.pass"),
                                "PASS: headless production Shell on the production engine boundary; a fork from New side chat, moved into a split beside Main, has the ordinary tab header linking its source (no End/lease/expiry/read-only); Main and fork composers share one two-row geometry (equal heights, level bottoms) at 1440 px with 520/360/300/760/480 px right sidebars and the fork group at its 320 px minimum, in dark and light; every controls row fits its measured budget (no truncation); access renders as an icon (Full access = open lock); Fast renders as a bolt beside the model name; Main reasoning/speed changes leave the fork unchanged; hovering a Main chip leaves the fork's chip at rest; the model menu offers Speed: Standard / Fast; execution summaries decode from WatchChats, running settings and access show with Running / Requested / how it applies while a turn runs, clear on the next turn, read Loaded when warm, never promise context transfer on a new binding, stay out of the fork's composer, fall back to legacy chips and Steer when missing/old/other-task, Steer follows the engine's native/after-turn/unavailable decision with its reason, and the Execution card follows the focused fork tab, refuses a late response for the previous focus and labels adapter declarations as unverified; Send findings goes only from a fork to its living source, previews without writing, stores a Planned receipt without starting a turn or touching the draft, refuses a stale or earlier-connection preview, shows a lost answer as unknown and settles it by Check status without resending, refreshes to only new findings; the provider switch has no default choice, drops a preview on a target change, stores a handoff without a turn; Details read without error; receipts survive the fork's deletion; Start new task with context previews into a reserved id kept across refreshes, creates nothing on preview or Cancel, and on apply (a lost answer recovered by Check status) creates exactly one ordinary task on the source's config with a Planned context copy, no child, no native session or turn, opens it, and leaves the source's draft untouched; apply and Check status answers arriving after the engine connection changed settle, open or notify nothing; Refresh and re-targeting refuse while an apply is in flight or unknown, keeping its command id\n",
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
                        eprintln!("Composer parity fixture failed: {error:#}");
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
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
