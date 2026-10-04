//! Headless GPUI E2E for a persistent fork (side thread) as an ordinary
//! thread in the chat workbench.
//!
//! A side chat starts as a fork (here: from recent context) of its source
//! and is then an ordinary thread: published with its real project and its
//! source as a link, opened as a Side tab under that source, with its own
//! cached composer and live transcript. Closing the tab, closing the right
//! sidebar, switching chats and an engine re-attach never end it; there is
//! no End, lease, expiry, heartbeat, unload or read-only "ended" state.
//!
//! Drives the production `Shell` (side chat creator, workbench, pane views)
//! in a hidden window against an in-process `EngineCore` with a scripted Mock
//! harness, attached through the production engine boundary (real
//! `WatchChats`, sessions, transcripts, workbench layout).
//!
//! Failure cases this run must catch (written before the migration):
//! 1. New side chat never opens as a Side tab under its exact source, the
//!    fork is published without its real project, or the creator keeps its
//!    retry id after a completed create.
//! 2. The fork's tab has no composer of its own, shares Main's (bound chat),
//!    or its header shows an End, lease, countdown, expiry, unload or
//!    read-only state.
//! 3. A send from the fork's composer is not answered, or changes the
//!    source's transcript.
//! 4. Closing the fork's tab loses its draft, or an in-flight send arrives
//!    twice (or never) after close and reopen.
//! 5. The fork's provider picker refuses with the old side-chat lock instead
//!    of taking the ordinary provider-switch path of an existing chat.
//! 6. An approval request in the fork shows in Main's composer, or not in
//!    the fork's (dark, light and a narrow canvas).
//! 7. Idling past the old 30 s lease, closing the right sidebar, switching
//!    chats and an engine re-attach end, unload or unbind the fork (its tab
//!    loses its composer, or its published state leaves `active`).
//! 8. Archive (the ordinary operation that replaced End) keeps a composer, or
//!    loses the saved history; unarchive does not restore the composer.
//! 9. The fork cannot become the workbench Main (its own composer as Main's).
//!
//! Retired with the side panel (no hook exists for them, and the behavior is
//! gone, so they are not claimed): the panel's list/page readback (pageId,
//! historyCount, parked), RenewSideChat timing, the right-pane transcript
//! clearance above the approval card (the tab now uses the workbench pane
//! view; its composer geometry is covered by `composer-parity-fixture`), and
//! End's read-only history.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example side-chat-pane-fixture
//! Run:   target/debug/examples/side-chat-pane-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes f00..f16 PNG captures, readback.json and side-chat-pane.pass; a
//! failure writes failure.txt and exits nonzero.
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

/// Everything the run asserts about `fork` (empty before one exists): the
/// creator, the workbench, the fork's tab header and both composers.
fn readback(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    fork: &str,
) -> anyhow::Result<serde_json::Value> {
    window.update(cx, |shell, _, cx| {
        let (main, pane) = shell.fixture_composer_readbacks(fork, cx);
        serde_json::json!({
            "creator": shell.fixture_side_chat_readback(cx),
            "workbench": shell.fixture_workbench_readback(cx),
            "related": shell.fixture_related_readback(cx),
            "strip": shell.fixture_workbench_strip(fork, cx),
            "hasComposer": shell.fixture_workbench_tab_has_composer(fork, cx),
            "draft": shell.fixture_workbench_draft(fork, cx),
            "forkTexts": shell.fixture_workbench_user_texts(fork, cx),
            "parentTexts": shell.fixture_workbench_user_texts(PARENT, cx),
            "mainComposer": main,
            "forkComposer": pane,
        })
    })
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn panes(node: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    if node["kind"] == "pane" {
        out.push(node.clone());
    } else {
        panes(&node["first"], out);
        panes(&node["second"], out);
    }
}

/// `chat`'s tab in the workbench readback.
fn tab(readback: &serde_json::Value, chat: &str) -> Option<serde_json::Value> {
    let mut all = Vec::new();
    panes(&readback["workbench"]["root"], &mut all);
    all.into_iter()
        .flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default())
        .find(|tab| tab["chatId"] == chat)
}

/// The first Side tab in the workbench readback.
fn side_tab(readback: &serde_json::Value) -> Option<String> {
    let mut all = Vec::new();
    panes(&readback["workbench"]["root"], &mut all);
    all.into_iter()
        .flat_map(|pane| pane["tabs"].as_array().cloned().unwrap_or_default())
        .find(|tab| tab["role"] == "Side")
        .and_then(|tab| tab["chatId"].as_str().map(str::to_string))
}

/// The fork's tab is an ordinary thread's: its own composer bound to it, a
/// fork identity linking its source, and no separate lifecycle anywhere in
/// its header.
fn ordinary(readback: &serde_json::Value, fork: &str, when: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        readback["hasComposer"] == true && readback["forkComposer"]["boundChat"] == fork,
        "{when}: the fork's tab renders its own composer: {readback}"
    );
    anyhow::ensure!(
        readback["mainComposer"]["boundChat"] != fork,
        "{when}: Main's composer is not the fork's: {readback}"
    );
    let strip = &readback["strip"];
    anyhow::ensure!(
        strip["identity"] == "Side chat" && strip["parentLinked"] == true,
        "{when}: the header names the fork's source as a link: {strip}"
    );
    let words = strip["status"].as_str().unwrap_or_default().to_lowercase();
    for lifecycle in [
        "ended",
        "read-only",
        "writable",
        "expire",
        "min left",
        "lease",
        "unload",
    ] {
        anyhow::ensure!(
            !words.contains(lifecycle),
            "{when}: no separate fork lifecycle ({lifecycle:?}) in the header: {strip}"
        );
    }
    Ok(())
}

/// Assistant answers to `prompt` in `chat`'s view (completed ones).
fn answers(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    chat: &str,
    prompt: &str,
) -> anyhow::Result<usize> {
    let after = window.update(cx, |shell, _, cx| {
        shell.fixture_workbench_turn_after(chat, prompt, cx)
    })?;
    Ok(after
        .iter()
        // Count this turn only; later user turns have their own replies.
        .take_while(|entry| entry["role"] != "User")
        .filter(|entry| {
            entry["role"] == "Assistant"
                && entry["status"] == "Complete"
                && entry["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(SIDE_ANSWER))
        })
        .count())
}

async fn wait_for(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    fork: &str,
    tries: usize,
    what: &str,
    done: impl Fn(&serde_json::Value) -> bool,
) -> anyhow::Result<serde_json::Value> {
    let mut last = serde_json::Value::Null;
    for _ in 0..tries {
        settle(cx, 100).await;
        last = readback(window, cx, fork)?;
        if done(&last) {
            return Ok(last);
        }
    }
    anyhow::bail!("{what}: {last}")
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
            let handle_for_task = handle.clone();
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
            let state_for_task = state.clone();
            cx.spawn(async move |cx| {
                let result: anyhow::Result<serde_json::Value> = async {
                    let mut record = serde_json::Map::new();
                    settle(cx, 900).await;

                    // f00. Related chats with no forks yet; the workbench is
                    // Main's.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_open_right_pane(cx);
                        shell.fixture_open_side_chats(cx);
                    })?;
                    let empty = wait_for(window, cx, "", 60, "the workbench loads with Main", |r| {
                        r["workbench"]["loaded"] == true && r["workbench"]["main"] == PARENT && r["creator"]["busy"] == false
                    })
                    .await?;
                    capture(window, cx, output.join("f00-side-list.png"))?;
                    anyhow::ensure!(empty["related"]["relatedTrees"].get(PARENT).is_none() && side_tab(&empty).is_none(), "no forks yet: {empty}");
                    let parent_texts = empty["parentTexts"].clone();
                    record.insert("empty".into(), empty);

                    // f01. Start one from recent context: it opens as a Side
                    // tab under its source, with its own composer.
                    window.update(cx, |shell, _, cx| shell.fixture_side_chat("snapshot", "", cx))?;
                    let opened = wait_for(window, cx, "", 200, "New side chat opens as a Side tab", |r| side_tab(r).is_some()).await?;
                    let side_id = side_tab(&opened).unwrap_or_default();
                    let created = wait_for(window, cx, &side_id, 100, "the fork's composer mounts", |r| r["forkComposer"]["boundChat"] == side_id.as_str()).await?;
                    settle(cx, 500).await;
                    capture(window, cx, output.join("f01-side-full-new-dark.png"))?;
                    let fork_tab = tab(&created, &side_id).unwrap_or_default();
                    anyhow::ensure!(fork_tab["sideParentId"] == PARENT && fork_tab["parent"] == PARENT && fork_tab["role"] == "Side", "the Side tab keeps its exact source: {fork_tab}");
                    anyhow::ensure!(created["creator"]["pendingCreate"].is_null() && created["creator"]["error"].is_null(), "a completed create drops its retry id: {created}");
                    let row = cx.update(|cx| state_for_task.read(cx).chats.iter().find(|chat| chat.id == side_id).cloned());
                    anyhow::ensure!(row.as_ref().is_some_and(|row| row.space_id.as_deref() == Some(PROJECT)), "the fork is published with its real project: {row:?}");
                    ordinary(&created, &side_id, "a new fork")?;
                    anyhow::ensure!(created["mainComposer"]["boundChat"] == PARENT, "Main's composer stays on the source: {created}");
                    record.insert("created".into(), created);

                    // f02. Send from the fork's own composer: answered, the
                    // source's transcript untouched.
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_type(&side_id, SIDE_QUESTION, true, cx))?;
                    for _ in 0..250 {
                        settle(cx, 100).await;
                        if answers(window, cx, &side_id, SIDE_QUESTION)? == 1 {
                            break;
                        }
                    }
                    settle(cx, 600).await;
                    capture(window, cx, output.join("f02-side-full-reply-dark.png"))?;
                    let replied = readback(window, cx, &side_id)?;
                    anyhow::ensure!(answers(window, cx, &side_id, SIDE_QUESTION)? == 1, "the fork answered its own send: {replied}");
                    anyhow::ensure!(replied["forkComposer"]["input"] == "" && replied["forkComposer"]["failure"].is_null(), "the send settled cleanly: {replied}");
                    anyhow::ensure!(replied["parentTexts"] == parent_texts, "the fork changed the source's transcript: {replied}");
                    record.insert("replied".into(), replied);

                    // f03-f04. A draft across closing and reopening the tab:
                    // Close is not End.
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_type(&side_id, SIDE_DRAFT, false, cx))?;
                    settle(cx, 600).await;
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_close(&side_id, cx))?;
                    let closed = wait_for(window, cx, &side_id, 100, "the fork's tab closed", |r| tab(r, &side_id).is_none()).await?;
                    capture(window, cx, output.join("f03-side-back-list.png"))?;
                    anyhow::ensure!(closed["related"]["relatedTrees"][PARENT].to_string().contains(side_id.as_str()), "a closed fork stays in its source's library: {closed}");
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_open(&side_id, cx))?;
                    let restored = wait_for(window, cx, &side_id, 150, "the fork reopens with its draft", |r| {
                        tab(r, &side_id).is_some() && r["draft"] == SIDE_DRAFT
                    })
                    .await?;
                    settle(cx, 500).await;
                    capture(window, cx, output.join("f04-side-draft-restored.png"))?;
                    anyhow::ensure!(tab(&restored, &side_id).is_some_and(|tab| tab["role"] == "Side" && tab["parent"] == PARENT), "reopened as a Side tab under its source: {restored}");
                    ordinary(&restored, &side_id, "after close and reopen")?;
                    record.insert("closed".into(), closed);
                    record.insert("draftRestored".into(), restored);

                    // f05. A send in flight across closing the tab: exactly
                    // one prompt and one reply.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_workbench_type(&side_id, SIDE_PENDING, true, cx);
                        shell.fixture_workbench_close(&side_id, cx);
                    })?;
                    settle(cx, 3000).await;
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_open(&side_id, cx))?;
                    for _ in 0..250 {
                        settle(cx, 100).await;
                        if answers(window, cx, &side_id, SIDE_PENDING)? >= 1 {
                            break;
                        }
                    }
                    settle(cx, 500).await;
                    capture(window, cx, output.join("f05-side-send-across-back.png"))?;
                    let settled = readback(window, cx, &side_id)?;
                    let prompts = settled["forkTexts"].as_array().into_iter().flatten().filter(|text| text.as_str().is_some_and(|text| text.contains(SIDE_PENDING))).count();
                    anyhow::ensure!(prompts == 1, "the in-flight send arrives exactly once: {settled}");
                    anyhow::ensure!(answers(window, cx, &side_id, SIDE_PENDING)? == 1, "one reply to the in-flight send: {settled}");
                    anyhow::ensure!(settled["forkComposer"]["input"] == "" && settled["forkComposer"]["failure"].is_null(), "the in-flight send settled cleanly: {settled}");
                    record.insert("sendAcrossClose".into(), settled);

                    // f06. Provider: the fork's own settings, through the
                    // ordinary path of an existing chat (no side-chat lock).
                    let refusal = window.update(cx, |shell, _, cx| {
                        let composer = shell.fixture_workbench_composer(&side_id, cx)?;
                        let pickers = composer.read(cx).pickers().clone();
                        pickers.update(cx, |pickers, cx| pickers.fixture_pick_harness(HarnessId::Codex, cx));
                        let refusal = pickers.read(cx).fixture_refusals();
                        pickers.update(cx, |pickers, cx| pickers.fixture_close(cx));
                        Some(refusal)
                    })?;
                    let refusal = refusal.ok_or_else(|| anyhow::anyhow!("the fork's composer must exist for its provider picker"))?;
                    anyhow::ensure!(!refusal.as_deref().is_some_and(|text| text.contains("parent's provider")), "the fork's provider is its own, not locked to its source: {refusal:?}");
                    record.insert("providerRefusal".into(), serde_json::json!(refusal));

                    // f07-f10. An approval card through the fork's live watch,
                    // in its own composer only; light/dark, narrow canvas.
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
                    let approval = wait_for(window, cx, &side_id, 100, "the approval card in the fork's composer", |r| r["forkComposer"]["wizard"] == SIDE_REQUEST).await?;
                    settle(cx, 400).await;
                    capture(window, cx, output.join("f07-side-full-approval-dark.png"))?;
                    anyhow::ensure!(approval["mainComposer"]["wizard"].is_null(), "Main's composer must not show the fork's approval: {approval}");
                    record.insert("approval".into(), approval);
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Light, cx));
                    settle(cx, 600).await;
                    capture(window, cx, output.join("f08-side-full-light.png"))?;
                    window.update(cx, |shell, _, cx| shell.fixture_set_right_pane_width(760.0, cx))?;
                    settle(cx, 700).await;
                    capture(window, cx, output.join("f09-side-full-light-narrow.png"))?;
                    let narrow = readback(window, cx, &side_id)?;
                    anyhow::ensure!(narrow["forkComposer"]["wizard"] == SIDE_REQUEST && narrow["forkComposer"]["boundChat"] == side_id.as_str(), "the narrow canvas keeps the fork's approval card: {narrow}");
                    record.insert("narrowApproval".into(), narrow);
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark, cx));
                    settle(cx, 700).await;
                    capture(window, cx, output.join("f10-side-full-dark-narrow.png"))?;
                    window.update(cx, |shell, _, cx| shell.fixture_set_right_pane_width(520.0, cx))?;

                    // f11. Idle past the old 30 s lease: still an ordinary,
                    // active thread.
                    settle(cx, 35_000).await;
                    let idle = readback(window, cx, &side_id)?;
                    ordinary(&idle, &side_id, "after 35 s idle")?;
                    let published = |cx: &mut AsyncApp| cx.update(|cx| state_for_task.read(cx).side_chat_rows.get(&side_id).and_then(|row| row.state.clone()));
                    anyhow::ensure!(published(cx).as_deref() == Some("active"), "an idle fork stays active: {:?}", published(cx));
                    record.insert("afterIdle".into(), idle);

                    // f12. Closing the right sidebar (the creator goes with
                    // it) leaves the fork's tab alone.
                    window.update(cx, |shell, _, cx| shell.fixture_close_right_pane(cx))?;
                    settle(cx, 600).await;
                    let sidebar_closed = readback(window, cx, &side_id)?;
                    ordinary(&sidebar_closed, &side_id, "with the right sidebar closed")?;
                    capture(window, cx, output.join("f12-side-reopened-after-close.png"))?;
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_open_right_pane(cx);
                        shell.fixture_open_side_chats(cx);
                    })?;
                    record.insert("rightSidebarClosed".into(), sidebar_closed);

                    // f13. Switch chats and back: the fork's tab is still in
                    // the source's workbench, with its composer.
                    cx.update(|cx| state_for_task.update(cx, |state, cx| state.select_chat(Some(OTHER.into()), cx)));
                    settle(cx, 600).await;
                    cx.update(|cx| state_for_task.update(cx, |state, cx| state.select_chat(Some(PARENT.into()), cx)));
                    let switched = wait_for(window, cx, &side_id, 150, "the fork's tab after switching back", |r| {
                        r["workbench"]["main"] == PARENT && r["workbench"]["loaded"] == true && tab(r, &side_id).is_some() && r["hasComposer"] == true
                    })
                    .await?;
                    ordinary(&switched, &side_id, "after switching chats")?;
                    record.insert("afterSwitch".into(), switched);

                    // f14. An engine re-attach (new generation, fresh watches)
                    // keeps it an active thread.
                    let reattach = handle_for_task.clone();
                    cx.update(|cx| state_for_task.update(cx, |state, cx| state.fixture_attach_engine(reattach, cx)));
                    settle(cx, 1500).await;
                    let reattached = wait_for(window, cx, &side_id, 200, "the fork's tab after a re-attach", |r| {
                        r["workbench"]["loaded"] == true && tab(r, &side_id).is_some() && r["hasComposer"] == true
                    })
                    .await?;
                    settle(cx, 400).await;
                    capture(window, cx, output.join("f14-side-after-reattach.png"))?;
                    ordinary(&reattached, &side_id, "after a re-attach")?;
                    anyhow::ensure!(published(cx).as_deref() == Some("active"), "a re-attach keeps the fork active: {:?}", published(cx));
                    record.insert("afterReattach".into(), reattached);

                    // f15. Archive, the ordinary operation (no End): the tab
                    // turns read-only with its history; unarchive restores it.
                    window.update(cx, |shell, _, cx| shell.fixture_chat_action(&side_id, "archive", cx))?;
                    let archived = wait_for(window, cx, &side_id, 200, "the fork archived", |r| r["hasComposer"] == false && r["strip"]["archived"] == true).await?;
                    settle(cx, 600).await;
                    capture(window, cx, output.join("f15-side-archived.png"))?;
                    let texts = archived["forkTexts"].to_string();
                    anyhow::ensure!(texts.contains(SIDE_QUESTION) && texts.contains(SIDE_PENDING), "an archived fork keeps its history: {archived}");
                    anyhow::ensure!(answers(window, cx, &side_id, SIDE_QUESTION)? == 1, "its answers stay too: {archived}");
                    window.update(cx, |shell, _, cx| shell.fixture_chat_action(&side_id, "unarchive", cx))?;
                    let unarchived = wait_for(window, cx, &side_id, 200, "the fork unarchived", |r| r["hasComposer"] == true && r["strip"]["archived"] == false).await?;
                    ordinary(&unarchived, &side_id, "after unarchive")?;
                    record.insert("archived".into(), archived);
                    record.insert("unarchived".into(), unarchived);

                    // f16. The fork as its own workbench Main: Main's composer
                    // binds it; then back to the source.
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_open_as_main(&side_id, cx))?;
                    let as_main = wait_for(window, cx, &side_id, 150, "the fork as Main", |r| {
                        r["workbench"]["main"] == side_id.as_str() && r["mainComposer"]["boundChat"] == side_id.as_str()
                    })
                    .await?;
                    settle(cx, 500).await;
                    capture(window, cx, output.join("f16-side-ended-reopened.png"))?;
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_open_as_main(PARENT, cx))?;
                    wait_for(window, cx, &side_id, 150, "back to the source's workbench", |r| r["workbench"]["main"] == PARENT && r["workbench"]["loaded"] == true).await?;
                    record.insert("forkAsMain".into(), as_main);
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
                                "PASS: headless production Shell on the production engine boundary; New side chat (recent context) opens as a Side tab under its exact source, published with its real project, with its own composer and no End/lease/expiry/read-only header; its send is answered with the source untouched; its draft and an in-flight send survive closing and reopening the tab (one prompt, one reply); its provider picker takes the ordinary path (no side-chat lock); an approval card shows in the fork's composer only, in dark, light and a narrow canvas; 35 s idle, closing the right sidebar, switching chats and an engine re-attach keep it an active thread with its composer; archive turns it read-only with its history and unarchive restores the composer; it opens as its own workbench Main. Retired with the side panel and not claimed: panel list/page readback, lease renewal timing, right-pane transcript clearance, End history\n",
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
                        eprintln!("Side chat pane fixture failed: {error:#}");
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
