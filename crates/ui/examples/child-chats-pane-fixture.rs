//! Headless GPUI E2E for child chats in the right pane.
//!
//! Drives the production `Shell` (sidebar, right-pane picker, Child chats
//! surface, transcript and reply composer) in a hidden window against an
//! in-process `EngineCore` with a scripted Mock harness and a synthetic
//! workspace under the evidence directory. It never contacts a provider,
//! touches LUNA Dev or its data directory, or opens a visible window.
//!
//! The UI rows carry `childThread` lineage; the engine copies deliberately do
//! not, so the engine's child startup sweeper never queues initial prompts
//! into the Mock harness and perturbs the seeded statuses. Live statuses come
//! from synthetic session rows; the reply path is real: the composer queues a
//! `queueMessage` command on the engine, the Mock harness runs the turn and
//! the child's `WatchDocMessages` feed delivers it to the open transcript.
//!
//! Failure cases: `$B/evidence/child-pane/failure-cases.md`.
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example child-chats-pane-fixture
//! Run:   target/debug/examples/child-chats-pane-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes 01..07 PNG captures, readback.json and child-chats-pane.pass.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowHandle, WindowOptions, px, size};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::mock::MockHarness;
use luna_proto::{
    AgentEvent, ChatConfig, ChildThread, ChildThreadOutcome, DoneStatus, HarnessId, PermissionMode,
    SandboxLevel, Session, SessionStatus,
};
use luna_ui::*;

const PROJECT: &str = "child-pane-project";
const PARENT: &str = "c0c0c0c0-0000-4000-8000-000000000001";
const OTHER_A: &str = "c0c0c0c0-0000-4000-8000-000000000002";
const OTHER_B: &str = "c0c0c0c0-0000-4000-8000-000000000003";
const APPROVAL: &str = "c1c1c1c1-0000-4000-8000-000000000001";
const RUNNING: &str = "c1c1c1c1-0000-4000-8000-000000000002";
const FAILED: &str = "c1c1c1c1-0000-4000-8000-000000000003";
const DONE: &str = "c1c1c1c1-0000-4000-8000-000000000004";
const IDLE: &str = "c1c1c1c1-0000-4000-8000-000000000005";
const ARCHIVED: &str = "c1c1c1c1-0000-4000-8000-000000000006";
const GRANDCHILD: &str = "c1c1c1c1-0000-4000-8000-000000000007";
const ORPHAN: &str = "c1c1c1c1-0000-4000-8000-000000000008";
const MISSING_PARENT: &str = "deleted-parent-0000";
const LEGACY_COLLAPSED: &str = "legacy-collapsed-parent";
const REPLY: &str = "Add a short highlights section at the top.";
const MOCK_ANSWER: &str = "Added a highlights section above the full notes.";

struct Spec {
    id: &'static str,
    parent: Option<&'static str>,
    title: &'static str,
    prompt: &'static str,
    answer: &'static str,
    minutes_ago: i64,
    seen: bool,
    archived: bool,
    outcome: Option<(&'static str, &'static str)>,
}

const SPECS: [Spec; 11] = [
    Spec {
        id: PARENT,
        parent: None,
        title: "Plan the v0.5 release",
        prompt: "Plan the v0.5 release. Split the work into child chats so each track keeps its own context.",
        answer: "I started five child chats: the migration review, the flaky sidebar test, the release build failure, an engine change summary and the release notes draft. Their results will appear here as they report back.",
        minutes_ago: 3,
        seen: true,
        archived: false,
        outcome: None,
    },
    Spec {
        id: OTHER_A,
        parent: None,
        title: "Polish onboarding copy",
        prompt: "Tighten the onboarding copy.",
        answer: "Shortened the three onboarding cards.",
        minutes_ago: 45,
        seen: true,
        archived: false,
        outcome: None,
    },
    Spec {
        id: OTHER_B,
        parent: None,
        title: "Fix sidebar hover flicker",
        prompt: "Why does the sidebar hover flicker?",
        answer: "The hover listener toggled twice per frame.",
        minutes_ago: 90,
        seen: true,
        archived: false,
        outcome: None,
    },
    Spec {
        id: APPROVAL,
        parent: Some(PARENT),
        title: "Review the settings migration",
        prompt: "Review the settings migration and apply it to the fixture profile.",
        answer: "The migration renames two keys. I need approval to write the fixture profile.",
        minutes_ago: 4,
        seen: true,
        archived: false,
        outcome: None,
    },
    Spec {
        id: RUNNING,
        parent: Some(PARENT),
        title: "Trace the flaky sidebar test",
        prompt: "Trace why the sidebar ordering test fails about once in twenty runs.",
        answer: "Running the sidebar suite in a loop. Two failures so far, both when two tasks share a timestamp, so the tie-breaker looks like the culprit.",
        minutes_ago: 1,
        seen: true,
        archived: false,
        outcome: None,
    },
    Spec {
        id: FAILED,
        parent: Some(PARENT),
        title: "Reproduce the release build failure",
        prompt: "Reproduce the release build failure on a clean checkout.",
        answer: "cargo build --release failed: linker error in luna-ui.",
        minutes_ago: 26,
        seen: true,
        archived: false,
        outcome: Some((
            "failed",
            "cargo build --release failed: linker error in luna-ui",
        )),
    },
    Spec {
        id: DONE,
        parent: Some(PARENT),
        title: "Summarize engine changes since v0.4",
        prompt: "Summarize engine changes since v0.4.",
        answer: "Three engine changes since v0.4: child chats, queued replies and native review.",
        minutes_ago: 12,
        seen: true,
        archived: false,
        outcome: Some((
            "turn-completed",
            "Three engine changes since v0.4: child chats, queued replies and native review.",
        )),
    },
    Spec {
        id: IDLE,
        parent: Some(PARENT),
        title: "Draft the release notes",
        prompt: "Draft the v0.5 release notes from the merged pull requests.",
        answer: "Drafted the notes: 14 changes grouped into features, fixes and internals.",
        minutes_ago: 120,
        seen: true,
        archived: false,
        outcome: None,
    },
    Spec {
        id: ARCHIVED,
        parent: Some(PARENT),
        title: "Spike: GPU text cache",
        prompt: "Spike a GPU text cache.",
        answer: "Not worth it yet; shaping dominates.",
        minutes_ago: 3 * 24 * 60,
        seen: true,
        archived: true,
        outcome: None,
    },
    Spec {
        id: GRANDCHILD,
        parent: Some(DONE),
        title: "Check the changelog links",
        prompt: "Check every link in the changelog.",
        answer: "All 23 links resolve.",
        minutes_ago: 10,
        seen: true,
        archived: false,
        outcome: None,
    },
    Spec {
        id: ORPHAN,
        parent: Some(MISSING_PARENT),
        title: "Investigate cache eviction",
        prompt: "Investigate the cache eviction spike.",
        answer: "Eviction follows the nightly backup window.",
        minutes_ago: 30,
        seen: true,
        archived: false,
        outcome: None,
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
        initial_prompt: spec.prompt.into(),
        report_results: true,
        continue_parent: false,
        outcomes: spec
            .outcome
            .map(|(outcome, summary)| ChildThreadOutcome {
                id: format!("{}:turn:{outcome}", spec.id),
                turn_id: format!("{}-answer", spec.id),
                outcome: outcome.into(),
                summary: summary.into(),
                created_at_ms: at_ms,
                delivered: true,
            })
            .into_iter()
            .collect(),
        outcome_floor_ms: None,
    })
}

fn capture(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    path: PathBuf,
) -> anyhow::Result<()> {
    // Draw through the untyped handle: a typed update would lease the Shell
    // that the frame is about to render.
    let window = gpui::AnyWindowHandle::from(window);
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })??;
    Ok(())
}

fn readback(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
) -> anyhow::Result<serde_json::Value> {
    window.update(cx, |shell, _, cx| shell.fixture_child_chats_readback(cx))
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

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: child-chats-pane-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["readback.json", "child-chats-pane.pass", "failure.txt"] {
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
            EngineProfile::development(&engine_data, "child-pane-fixture", "child-pane-fixture"),
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
    auth.dev_user_id = "child-pane-fixture".into();
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
    // Doc hosts spawn onto the engine's Tokio runtime.
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
                spec.prompt,
                &device,
                at_ms - 60_000,
            ))?;
            doc.doc().push_message(&text_entry(
                &format!("{}-answer", spec.id),
                MessageRole::Assistant,
                spec.answer,
                &device,
                at_ms,
            ))?;
            let mut chat = core.workspace.chat(spec.id)?.expect("seeded chat");
            chat.created_at = at - TimeDelta::minutes(2);
            chat.last_message_at = Some(at);
            chat.last_seen_at = spec.seen.then(|| at + TimeDelta::seconds(1));
            chat.archived = spec.archived;
            core.workspace.import_chat_row(&chat)?;
            // Lineage only in the UI copy (see the module docs).
            chat.child_thread = child_thread(spec, at_ms);
            ui_chats.push(chat);
        }
        anyhow::Ok(ui_chats)
    })?;
    let spaces = core.workspace.read_spaces()?;
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59998,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let sessions = vec![
        Session {
            chat_id: APPROVAL.into(),
            device_id: device.clone(),
            status: SessionStatus::AwaitingApproval,
            started_at: Some(now - TimeDelta::minutes(5)),
            updated_at: now,
            last_completed_turn: None,
            recovery: None,
        },
        Session {
            chat_id: RUNNING.into(),
            device_id: device.clone(),
            status: SessionStatus::Working,
            started_at: Some(now - TimeDelta::minutes(2)),
            updated_at: now,
            last_completed_turn: None,
            recovery: None,
        },
    ];
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
    gpui_platform::headless()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let mut settings = settings::UiSettings::default();
            settings.collapsed_child_chats = vec![LEGACY_COLLAPSED.into()];
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
                state.sessions = sessions;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.auto_selected = true;
                state.connection = state::ConnectionStatus::Ready;
                // The engine profile is a development workspace: no sign-in gate.
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
                // Real RPC client, no workspace watches: the seeded rows and
                // sessions above stay exactly as written.
                state.fixture_attachment_engine(handle);
                state.select_chat(Some(PARENT.into()), cx);
                state
            });
            let window = cx
                .open_window(
                    WindowOptions {
                        window_background: theme::Theme::of(cx).window_background_appearance(),
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(20.), px(20.)),
                            size(px(1440.), px(900.)),
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

                    // 1. Picker card with the count, sidebar without children.
                    window.update(cx, |shell, _, cx| shell.fixture_open_right_pane(cx))?;
                    settle(cx, 500).await;
                    capture(window, cx, output.join("01-picker-sidebar.png"))?;
                    let picker = readback(window, cx)?;
                    anyhow::ensure!(picker["pickerCardVisible"] == true, "Child chats card missing: {picker}");
                    anyhow::ensure!(picker["pickerBadge"] == 5, "badge must count 5 non-archived children: {picker}");
                    let order = ids(&picker["sidebarOrder"]);
                    for hidden in [APPROVAL, RUNNING, FAILED, DONE, IDLE, ARCHIVED, GRANDCHILD] {
                        anyhow::ensure!(!order.iter().any(|id| id == hidden), "child {hidden} still in the sidebar: {order:?}");
                    }
                    for shown in [PARENT, OTHER_A, OTHER_B, ORPHAN] {
                        anyhow::ensure!(order.iter().any(|id| id == shown), "top-level {shown} missing from the sidebar: {order:?}");
                    }
                    record.insert("picker".into(), picker);

                    // 2. The list: attention + running first, then recency.
                    window.update(cx, |shell, _, cx| shell.fixture_open_child_chats(cx))?;
                    settle(cx, 500).await;
                    capture(window, cx, output.join("02-child-list.png"))?;
                    let list = readback(window, cx)?;
                    let rows: Vec<(String, String)> = list["rows"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|row| (row["id"].as_str().unwrap_or_default().to_string(), row["status"].as_str().unwrap_or_default().to_string()))
                        .collect();
                    let expected = [
                        (APPROVAL, "Needs approval"),
                        (RUNNING, "Running"),
                        (DONE, "Completed"),
                        (FAILED, "Failed"),
                        (IDLE, "Idle"),
                        (ARCHIVED, "Archived"),
                    ];
                    anyhow::ensure!(
                        rows.iter().map(|(id, status)| (id.as_str(), status.as_str())).collect::<Vec<_>>() == expected,
                        "child list order/status mismatch: {rows:?}"
                    );
                    anyhow::ensure!(list["rightActive"] == "ChildChats", "Child chats surface inactive: {list}");
                    anyhow::ensure!(ids(&list["watchedDocs"]).is_empty(), "list view must not watch docs: {list}");
                    record.insert("list".into(), list);

                    // 3. Open a running child: one watch, its transcript.
                    window.update(cx, |shell, _, cx| shell.fixture_child_chat_open(RUNNING.into(), cx))?;
                    window.update(cx, |shell, _, cx| shell.fixture_child_chat_open(RUNNING.into(), cx))?;
                    let mut opened = serde_json::Value::Null;
                    for _ in 0..60 {
                        settle(cx, 100).await;
                        opened = readback(window, cx)?;
                        if opened["openTranscriptRows"].as_u64().unwrap_or(0) >= 2 {
                            break;
                        }
                    }
                    settle(cx, 300).await;
                    capture(window, cx, output.join("03-child-open.png"))?;
                    anyhow::ensure!(opened["openChild"] == RUNNING, "child not open: {opened}");
                    anyhow::ensure!(ids(&opened["watchedDocs"]) == [RUNNING], "expected exactly one watch: {opened}");
                    anyhow::ensure!(opened["openTranscriptRows"].as_u64().unwrap_or(0) >= 2, "child transcript did not load: {opened}");
                    let text = opened["openTranscriptText"].to_string();
                    anyhow::ensure!(text.contains("tie-breaker") && !text.contains("Split the work"), "wrong transcript shown: {opened}");
                    record.insert("open".into(), opened);

                    // 4. Escape from the focused reply input: list again,
                    // watch released (the Back button calls the same path).
                    let escape = gpui::Keystroke::parse("escape")
                        .map_err(|error| anyhow::anyhow!("escape keystroke: {error:?}"))?;
                    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
                        window.dispatch_event(
                            gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                                keystroke: escape,
                                is_held: false,
                                prefer_character_input: false,
                            }),
                            cx,
                        );
                    })?;
                    settle(cx, 400).await;
                    capture(window, cx, output.join("04-child-back.png"))?;
                    let back = readback(window, cx)?;
                    anyhow::ensure!(back["openChild"].is_null(), "Escape did not return to the list: {back}");
                    anyhow::ensure!(ids(&back["watchedDocs"]).is_empty(), "Back leaked the doc watch: {back}");
                    record.insert("back".into(), back);

                    // 5. Open another child and reply through the real queue.
                    window.update(cx, |shell, _, cx| shell.fixture_child_chat_open(IDLE.into(), cx))?;
                    settle(cx, 600).await;
                    window.update(cx, |shell, _, cx| shell.fixture_child_chat_send(REPLY, cx))?;
                    let mut replied = serde_json::Value::Null;
                    for _ in 0..200 {
                        settle(cx, 100).await;
                        replied = readback(window, cx)?;
                        let text = replied["openTranscriptText"].to_string();
                        if text.contains(REPLY) && text.contains(MOCK_ANSWER) {
                            break;
                        }
                    }
                    settle(cx, 400).await;
                    capture(window, cx, output.join("05-child-reply.png"))?;
                    let text = replied["openTranscriptText"].to_string();
                    anyhow::ensure!(replied["openChild"] == IDLE, "second child not open: {replied}");
                    anyhow::ensure!(ids(&replied["watchedDocs"]) == [IDLE], "switching children must leave one watch: {replied}");
                    anyhow::ensure!(replied["openNotice"]["error"] != true, "reply failed: {replied}");
                    anyhow::ensure!(replied["openInput"] == "", "applied reply must clear the input: {replied}");
                    anyhow::ensure!(text.contains(REPLY), "reply never reached the child transcript: {replied}");
                    anyhow::ensure!(text.contains(MOCK_ANSWER), "child never answered the reply: {replied}");
                    record.insert("reply".into(), replied);

                    // 5b. A child waiting on approval offers the main view.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_child_chat_open(APPROVAL.into(), cx)
                    })?;
                    settle(cx, 700).await;
                    capture(window, cx, output.join("05b-child-needs-approval.png"))?;
                    let approval = readback(window, cx)?;
                    anyhow::ensure!(
                        approval["openChild"] == APPROVAL && ids(&approval["watchedDocs"]) == [APPROVAL],
                        "approval child must replace the previous watch: {approval}"
                    );
                    record.insert("approval".into(), approval);

                    // 6. Closing the pane releases the panel and its watch.
                    window.update(cx, |shell, _, cx| shell.fixture_close_right_pane(cx))?;
                    settle(cx, 400).await;
                    let closed = readback(window, cx)?;
                    anyhow::ensure!(closed["panelParent"].is_null() && ids(&closed["watchedDocs"]).is_empty(), "closing the pane leaked the panel or watch: {closed}");
                    record.insert("closed".into(), closed);

                    // 7. A child opened directly stays visible, nested under its parent.
                    cx.update(|cx| state.update(cx, |state, cx| state.select_chat(Some(RUNNING.into()), cx)));
                    settle(cx, 900).await;
                    capture(window, cx, output.join("06-direct-child.png"))?;
                    let direct = readback(window, cx)?;
                    let order = ids(&direct["sidebarOrder"]);
                    let parent_at = order.iter().position(|id| id == PARENT);
                    let child_at = order.iter().position(|id| id == RUNNING);
                    anyhow::ensure!(
                        parent_at.is_some() && child_at == parent_at.map(|at| at + 1),
                        "selected child must follow its parent: {order:?}"
                    );
                    anyhow::ensure!(!order.iter().any(|id| id == APPROVAL || id == IDLE), "siblings must stay folded: {order:?}");
                    record.insert("direct".into(), direct);

                    // 8. The parent's count chip opens its list from another chat.
                    cx.update(|cx| state.update(cx, |state, cx| state.select_chat(Some(OTHER_A.into()), cx)));
                    settle(cx, 400).await;
                    window.update(cx, |shell, _, cx| shell.fixture_open_child_chats_for(PARENT.into(), cx))?;
                    settle(cx, 900).await;
                    capture(window, cx, output.join("07-chip-opens-list.png"))?;
                    let chip = readback(window, cx)?;
                    anyhow::ensure!(chip["activeChat"] == PARENT && chip["rightPaneOpen"] == true && chip["rightActive"] == "ChildChats", "count chip did not open the parent's list: {chip}");
                    anyhow::ensure!(ids(&chip["collapsedChildChats"]) == [LEGACY_COLLAPSED], "legacy setting changed: {chip}");
                    record.insert("chip".into(), chip);
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
                                output.join("child-chats-pane.pass"),
                                "PASS: headless production Shell; child chats folded out of the sidebar, picker card + count, list order/status, one doc watch per open child released on Back/close, real queued reply answered by the Mock harness\n",
                            )
                        });
                        if let Err(error) = written {
                            *failure_for_ui.lock().unwrap() = Some(error.to_string());
                        }
                    }
                    Err(error) => {
                        eprintln!("Child chats pane fixture failed: {error:#}");
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                        *failure_for_ui.lock().unwrap() = Some(error.to_string());
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
        // AppKit termination from inside `run` can exit 0; fail explicitly.
        eprintln!("{error}");
        std::process::exit(1);
    }
    Ok(())
}
