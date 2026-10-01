//! Headless GPUI E2E for side chats in the right pane's shared chat layout.
//!
//! Drives the production `Shell` and `SideChatPanel` in a hidden window against
//! an in-process `EngineCore` with a scripted Mock harness, attached through
//! the production engine boundary (real `WatchChats`, sessions, transcripts).
//! The engine advertises `side-chat-task-commands-v1` and publishes the live
//! side chat spaceless in `WatchChats` (contract §5.1), so the pane shows the
//! FULL main composer and live transcript bound to the side chat; the lease
//! is kept by the `RenewSideChat` heartbeat (§5.2).
//!
//! Failure cases: `2026-10-02-pane-chat/evidence/pane-ui/failure-cases.md`.
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example side-chat-pane-fixture
//! Run:   target/debug/examples/side-chat-pane-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes f01..f12 PNG captures, readback.json and side-chat-pane.pass.
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
const SIDE_DRAFT: &str = "SIDE_DRAFT_KEEP: compare with the settings rename";
const SIDE_PENDING: &str = "SIDE_NAV_PENDING: and the UI flag?";

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

fn live(value: &serde_json::Value) -> String {
    value["liveTexts"].to_string()
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
                state.select_chat(Some(PARENT.into()), cx);
                // The production attachment boundary: real WatchChats (which
                // publishes live side chats spaceless), sessions, transcripts.
                state.fixture_attach_engine(handle, cx);
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
                    capture(window, cx, output.join("f00-side-list.png"))?;
                    anyhow::ensure!(list["historyCount"] == 0 && list["pageId"].is_null(), "unexpected side chat list: {list}");
                    let parent_rows = list["parentTranscriptRows"].as_u64().unwrap_or(0);
                    record.insert("list".into(), list);

                    // f01. Start one with the parent's recent context: with the
                    // engine gate on, the full composer binds the side chat.
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("snapshot", "", cx))?;
                    let created = wait_for(window, cx, 200, |r| {
                        r["firstClass"] == true && r["fullComposer"]["boundChat"] == r["pageId"] && r["busy"] == false
                    })
                    .await?;
                    settle(cx, 500).await;
                    capture(window, cx, output.join("f01-side-full-new-dark.png"))?;
                    anyhow::ensure!(created["firstClass"] == true, "engine gate did not enable the full composer: {created}");
                    let side_id = created["pageId"].as_str().unwrap_or_default().to_string();
                    anyhow::ensure!(created["fullComposer"]["boundChat"] == side_id.as_str(), "full composer must bind the side chat: {created}");
                    anyhow::ensure!(created["mainComposer"]["boundChat"] == PARENT, "main composer must stay on the parent: {created}");
                    anyhow::ensure!(ids(&created["paneChats"]) == [side_id.clone()], "one live pane watch for the side chat: {created}");
                    record.insert("created".into(), created);

                    // f02. Send with the full composer: live transcript, parent untouched.
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("send_full", SIDE_QUESTION, cx))?;
                    let replied = wait_for(window, cx, 250, |r| {
                        live(r).contains(SIDE_QUESTION) && live(r).contains(SIDE_ANSWER) && r["fullComposer"]["sending"] == false
                    })
                    .await?;
                    settle(cx, 600).await;
                    capture(window, cx, output.join("f02-side-full-reply-dark.png"))?;
                    anyhow::ensure!(live(&replied).contains(SIDE_ANSWER), "side chat never answered through the full composer: {replied}");
                    anyhow::ensure!(replied["fullComposer"]["input"] == "" && replied["fullComposer"]["failure"].is_null(), "send did not settle cleanly: {replied}");
                    anyhow::ensure!(replied["parentTranscriptRows"].as_u64().unwrap_or(0) == parent_rows, "the side chat changed the parent transcript: {replied}");
                    record.insert("replied".into(), replied);

                    // f03-f04. Draft across Back: Back parks the side chat (still
                    // writable, lease renewed), reopening restores the draft.
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("type_full", SIDE_DRAFT, cx))?;
                    settle(cx, 300).await;
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("back", "", cx))?;
                    let parked = wait_for(window, cx, 100, |r| r["pageId"].is_null() && r["historyCount"] == 1 && r["busy"] == false).await?;
                    settle(cx, 300).await;
                    capture(window, cx, output.join("f03-side-parked-list.png"))?;
                    anyhow::ensure!(ids(&parked["parked"]) == [side_id.clone()], "Back must park the writable side chat: {parked}");
                    anyhow::ensure!(ids(&parked["paneChats"]).is_empty(), "the parked chat must release its pane watches: {parked}");
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("open", "", cx))?;
                    let restored = wait_for(window, cx, 150, |r| r["firstClass"] == true && r["fullComposer"]["boundChat"] == side_id.as_str()).await?;
                    settle(cx, 500).await;
                    capture(window, cx, output.join("f04-side-draft-restored.png"))?;
                    anyhow::ensure!(restored["writable"] == true && restored["fullComposer"]["input"] == SIDE_DRAFT, "draft lost across Back: {restored}");
                    record.insert("parked".into(), parked);
                    record.insert("draftRestored".into(), restored);

                    // f05. A send in flight across Back: exactly one prompt and one reply.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_side_chat("send_full", SIDE_PENDING, cx);
                        shell.fixture_side_chat("back", "", cx);
                    })?;
                    settle(cx, 50).await;
                    let in_flight = readback(window, cx)?;
                    anyhow::ensure!(in_flight["pageId"].is_null() && ids(&in_flight["parked"]) == [side_id.clone()], "Back during the send must park: {in_flight}");
                    settle(cx, 3000).await;
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("open", "", cx))?;
                    let settled = wait_for(window, cx, 250, |r| live(r).contains(SIDE_PENDING) && live(r).matches(SIDE_ANSWER).count() >= 2).await?;
                    settle(cx, 500).await;
                    capture(window, cx, output.join("f05-side-send-across-back.png"))?;
                    let texts: Vec<String> = settled["liveTexts"].as_array().into_iter().flatten().filter_map(|text| text.as_str().map(str::to_string)).collect();
                    anyhow::ensure!(texts.iter().filter(|text| text.contains(SIDE_PENDING)).count() == 1, "the in-flight send must arrive exactly once: {settled}");
                    anyhow::ensure!(texts.iter().filter(|text| text.contains(SIDE_ANSWER)).count() == 2, "expected one reply per question: {settled}");
                    anyhow::ensure!(settled["fullComposer"]["input"] == "" && settled["fullComposer"]["failure"].is_null(), "the in-flight send did not settle cleanly: {settled}");
                    record.insert("sendAcrossBack".into(), settled);

                    // f06. Provider/account changes are refused truthfully.
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("pick_other_provider", "", cx))?;
                    settle(cx, 300).await;
                    let refused = readback(window, cx)?;
                    anyhow::ensure!(refused["pickerRefusal"].as_str().is_some_and(|text| text.contains("keeps its parent's provider and account")), "provider switch must be refused with the engine's reason: {refused}");
                    record.insert("providerRefused".into(), refused["pickerRefusal"].clone());

                    // f07-f10. Approval card through the live watch; light/dark, narrow.
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
                    let approval = wait_for(window, cx, 100, |r| r["fullComposer"]["wizard"] == SIDE_REQUEST).await?;
                    settle(cx, 400).await;
                    capture(window, cx, output.join("f07-side-full-approval-dark.png"))?;
                    anyhow::ensure!(approval["fullComposer"]["wizard"] == SIDE_REQUEST, "approval card missing from the side chat's composer: {approval}");
                    anyhow::ensure!(approval["mainComposer"]["wizard"].is_null(), "the parent composer must not show the side approval: {approval}");
                    record.insert("approval".into(), approval);
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Light, cx));
                    settle(cx, 600).await;
                    capture(window, cx, output.join("f08-side-full-light.png"))?;
                    window.update(cx, |shell, _, cx| shell.fixture_set_right_pane_width(360.0, cx))?;
                    settle(cx, 700).await;
                    capture(window, cx, output.join("f09-side-full-light-narrow.png"))?;
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark, cx));
                    settle(cx, 700).await;
                    capture(window, cx, output.join("f10-side-full-dark-narrow.png"))?;
                    window.update(cx, |shell, _, cx| shell.fixture_set_right_pane_width(520.0, cx))?;

                    // f11. Heartbeat: 35 s idle (> the 30 s lease) keeps it writable
                    // and a later send still works.
                    settle(cx, 35_000).await;
                    let idle = readback(window, cx)?;
                    anyhow::ensure!(idle["writable"] == true && idle["error"].is_null(), "the lease lapsed while the pane was open: {idle}");
                    anyhow::ensure!(idle["lastRenewAgoMs"].as_u64().unwrap_or(u64::MAX) < 15_000, "no recent RenewSideChat heartbeat: {idle}");
                    anyhow::ensure!(ids(&idle["watchedDocs"]) == [side_id.clone()], "the heartbeat must not reopen or drop the live watch: {idle}");
                    record.insert("afterIdle".into(), idle);

                    // f12. Unload from the header: read-only history.
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("unload", "", cx))?;
                    let back = wait_for(window, cx, 150, |r| r["pageId"].is_null() && r["busy"] == false && r["historyStates"].to_string().contains("unloaded")).await?;
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("open", "", cx))?;
                    let reopened = wait_for(window, cx, 100, |r| r["pageId"] == side_id.as_str() && r["busy"] == false).await?;
                    settle(cx, 400).await;
                    capture(window, cx, output.join("f12-side-unloaded-readonly.png"))?;
                    anyhow::ensure!(reopened["writable"] == false && reopened["fullComposer"].is_null(), "an unloaded side chat must be read-only: {reopened}");
                    record.insert("unloaded".into(), back);
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
                                "PASS: headless production Shell on the production engine boundary; engine gate side-chat-task-commands-v1 + spaceless WatchChats row -> the FULL main composer and live transcript bound to the side chat; send answered with the parent untouched; draft and an in-flight send survive Back (parked, lease renewed); provider switch refused with the engine's reason; approval card in light/dark and a 360 px pane; 35 s idle kept writable by the RenewSideChat heartbeat without reopening the watch; Unload gives read-only history\n",
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
