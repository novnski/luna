//! Headless GPUI E2E for side chats in the right pane's shared chat layout.
//!
//! Drives the production `Shell` and `SideChatPanel` in a hidden window against
//! an in-process `EngineCore` with a scripted Mock harness. Side chats keep the
//! engine's leased side-chat RPCs (`CreateSideChat` / `SendSideChat` /
//! `ReadSideChat` / `StopSideChat` / `UnloadSideChat`, contract §5); the pane
//! renders them with the shared identity header, the main transcript and a
//! composer-styled input that offers exactly what side chats support.
//!
//! Failure cases: `2026-10-02-pane-chat/evidence/pane-ui/failure-cases.md`.
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example side-chat-pane-fixture
//! Run:   target/debug/examples/side-chat-pane-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes s01..s09 PNG captures, readback.json and side-chat-pane.pass.
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
use luna_proto::{AgentEvent, ChatConfig, DoneStatus, HarnessId, PermissionMode, SandboxLevel};
use luna_ui::*;

const PROJECT: &str = "side-pane-project";
const PARENT: &str = "d0d0d0d0-0000-4000-8000-000000000001";
const OTHER: &str = "d0d0d0d0-0000-4000-8000-000000000002";
const SIDE_QUESTION: &str = "Is the rollout order safe if the cache migration lands first?";
const SIDE_ANSWER: &str =
    "Yes. The cache migration is additive, so landing it first only warms the new keys early.";
const SIDE_REQUEST: &str = "side-fixture-approval";

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

fn approval_entry(device: &str, at: i64) -> SessionMessageEntry {
    SessionMessageEntry {
        id: "side-approval".into(),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Input {
            id: "side-approval-part".into(),
            request_id: SIDE_REQUEST.into(),
            questions: vec![luna_proto::UserInputQuestion {
                id: "approve".into(),
                header: "Approval".into(),
                question: "Run the read-only migration dry run?".into(),
                options: vec!["Allow once".into(), "Deny".into()],
                multi_select: false,
                approval: Some(luna_proto::ApprovalRequest {
                    provider_request_id: SIDE_REQUEST.into(),
                    provider: HarnessId::Mock,
                    turn_id: None,
                    machine_name: "Studio Mac".into(),
                    action_kind: luna_proto::ApprovalActionKind::Command,
                    summary: "cargo run --bin migrate -- --dry-run".into(),
                    requested_scope: luna_proto::PermissionScope::Workspace,
                    expires_at_ms: 0,
                    supports_task_allowance: false,
                    details: None,
                }),
                mcp: None,
                input: None,
            }],
            resolved: false,
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
    window.update(cx, |shell, _, cx| shell.fixture_side_chat_readback(cx))
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn texts(value: &serde_json::Value) -> String {
    value["texts"].to_string()
}

async fn wait_for(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    tries: usize,
    done: impl Fn(&serde_json::Value) -> bool,
) -> anyhow::Result<serde_json::Value> {
    let mut last = serde_json::Value::Null;
    for _ in 0..tries {
        settle(cx, 100).await;
        last = readback(window, cx)?;
        if done(&last) {
            break;
        }
    }
    Ok(last)
}

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: side-chat-pane-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["readback.json", "side-chat-pane.pass", "failure.txt"] {
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
                text: SIDE_ANSWER.into(),
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
            EngineProfile::development(&engine_data, "side-pane-fixture", "side-pane-fixture"),
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
    auth.dev_user_id = "side-pane-fixture".into();
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
        for (id, title, prompt, answer, minutes) in [
            (PARENT, "Plan the v0.5 release", "Plan the v0.5 release rollout: cache migration, settings rename, then the UI flag.", "Proposed order: cache migration, settings rename, UI flag. Each step is reversible.", 3),
            (OTHER, "Polish onboarding copy", "Tighten the onboarding copy.", "Shortened the three onboarding cards.", 45),
        ] {
            core.workspace.create_chat(
                id,
                Some(PROJECT),
                Some(&device),
                Some(config()),
                Some(project.to_string_lossy().into_owned()),
            )?;
            core.workspace.rename_chat(id, title)?;
            let at = now - TimeDelta::minutes(minutes);
            let at_ms = at.timestamp_millis();
            let doc = core.doc_host.open(id)?;
            doc.doc().push_message(&text_entry(&format!("{id}-prompt"), MessageRole::User, prompt, &device, at_ms - 60_000))?;
            doc.doc().push_message(&text_entry(&format!("{id}-answer"), MessageRole::Assistant, answer, &device, at_ms))?;
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
    let docs = core.doc_host.clone();
    let engine_rt = runtime.handle().clone();
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

                    // s01. The side chats surface: no saved side chats yet.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_open_right_pane(cx);
                        shell.fixture_open_side_chats(cx);
                    })?;
                    let list = wait_for(window, cx, 60, |r| r["historyCount"].is_number()).await?;
                    capture(window, cx, output.join("s01-side-list.png"))?;
                    anyhow::ensure!(list["historyCount"] == 0 && list["pageId"].is_null(), "unexpected side chat list: {list}");
                    let parent_rows = list["parentTranscriptRows"].as_u64().unwrap_or(0);
                    record.insert("list".into(), list);

                    // s02. Start one with the parent's recent context.
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("snapshot", "", cx))?;
                    let created = wait_for(window, cx, 150, |r| r["writable"] == true && r["busy"] == false).await?;
                    settle(cx, 400).await;
                    capture(window, cx, output.join("s02-side-new-dark.png"))?;
                    anyhow::ensure!(created["writable"] == true, "side chat not writable: {created}");
                    anyhow::ensure!(created["contextMode"] == "contextSnapshot", "Mock has no native fork; expected a context snapshot: {created}");
                    let side_id = created["pageId"].as_str().unwrap_or_default().to_string();
                    record.insert("created".into(), created);

                    // s03. Ask a question: the side chat answers, the parent is untouched.
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("send", SIDE_QUESTION, cx))?;
                    let replied = wait_for(window, cx, 200, |r| {
                        texts(r).contains(SIDE_QUESTION) && texts(r).contains(SIDE_ANSWER) && r["working"] == false && r["busy"] == false
                    })
                    .await?;
                    settle(cx, 500).await;
                    capture(window, cx, output.join("s03-side-reply-dark.png"))?;
                    anyhow::ensure!(texts(&replied).contains(SIDE_ANSWER), "side chat never answered: {replied}");
                    anyhow::ensure!(replied["input"] == "", "a sent question must clear the input: {replied}");
                    anyhow::ensure!(replied["error"].is_null(), "side send failed: {replied}");
                    anyhow::ensure!(replied["parentTranscriptRows"].as_u64().unwrap_or(0) == parent_rows, "the side chat changed the parent transcript: {replied}");
                    anyhow::ensure!(replied["mainComposer"]["boundChat"] == PARENT, "main composer must stay on the parent: {replied}");
                    record.insert("replied".into(), replied);

                    // s04. An approval raised in the side chat renders in the
                    // side composer's card chrome.
                    let device_id = device.clone();
                    let target = side_id.clone();
                    let docs_for_task = docs.clone();
                    engine_rt
                        .spawn(async move {
                            let doc = docs_for_task.open(&target)?;
                            doc.doc().push_message(&approval_entry(&device_id, Utc::now().timestamp_millis()))?;
                            anyhow::Ok(())
                        })
                        .await??;
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("refresh", "", cx))?;
                    let approval = wait_for(window, cx, 100, |r| r["question"] == SIDE_REQUEST).await?;
                    settle(cx, 400).await;
                    capture(window, cx, output.join("s04-side-approval-dark.png"))?;
                    anyhow::ensure!(approval["question"] == SIDE_REQUEST, "side approval card missing: {approval}");
                    record.insert("approval".into(), approval);

                    // s05-s07. Light theme and a narrow pane.
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("type", "Draft follow-up about the UI flag", cx))?;
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Light, cx));
                    settle(cx, 600).await;
                    capture(window, cx, output.join("s05-side-approval-light.png"))?;
                    window.update(cx, |shell, _, cx| shell.fixture_set_right_pane_width(360.0, cx))?;
                    settle(cx, 700).await;
                    capture(window, cx, output.join("s06-side-light-narrow.png"))?;
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark, cx));
                    settle(cx, 700).await;
                    capture(window, cx, output.join("s07-side-dark-narrow.png"))?;
                    window.update(cx, |shell, _, cx| shell.fixture_set_right_pane_width(520.0, cx))?;
                    settle(cx, 300).await;

                    // s08. Back unloads: the side chat becomes saved, read-only history.
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("back", "", cx))?;
                    let back = wait_for(window, cx, 150, |r| r["pageId"].is_null() && r["historyCount"] == 1 && r["busy"] == false).await?;
                    settle(cx, 300).await;
                    capture(window, cx, output.join("s08-side-back-list.png"))?;
                    anyhow::ensure!(back["pageId"].is_null() && back["historyCount"] == 1, "Back did not unload into history: {back}");
                    record.insert("back".into(), back);

                    // s09. Reopening saved history is read-only, in the same layout.
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("open", "", cx))?;
                    let reopened = wait_for(window, cx, 100, |r| r["pageId"] == side_id.as_str() && r["busy"] == false).await?;
                    settle(cx, 400).await;
                    capture(window, cx, output.join("s09-side-readonly.png"))?;
                    anyhow::ensure!(reopened["writable"] == false, "saved side chat must be read-only: {reopened}");
                    anyhow::ensure!(texts(&reopened).contains(SIDE_ANSWER), "saved history lost the reply: {reopened}");
                    record.insert("reopened".into(), reopened);
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
                                output.join("side-chat-pane.pass"),
                                "PASS: headless production Shell; side chat in the shared pane layout (identity header, parent link, main transcript, composer-styled input); context-snapshot creation, real side send answered by the Mock harness with the parent untouched; side approval card; light/dark and a 360 px pane; Back unloads to read-only history that reopens in the same layout\n",
                            )
                        });
                        if let Err(error) = written {
                            *failure_for_ui.lock().unwrap() = Some(error.to_string());
                        }
                    }
                    Err(error) => {
                        eprintln!("Side chat pane fixture failed: {error:#}");
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
        eprintln!("{error}");
        std::process::exit(1);
    }
    Ok(())
}
