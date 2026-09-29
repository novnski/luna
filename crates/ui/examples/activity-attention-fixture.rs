//! Hermetic L15 native QA fixture. It is built here but launched only after
//! parent source review. No native provider, account, auth store, or live home.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowOptions, px, size};
use luna_doc::{
    MessagePart, MessageRole, MessageStatus, QueueDeliveryGate, QueuedMessage, SessionMessageEntry,
};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_proto::{
    ApprovalActionKind, ApprovalRequest, ChatConfig, HarnessId, PermissionMode, PermissionScope,
    SandboxLevel, Session, SessionStatus, UserInputQuestion,
};
use luna_rpc::methods;
use luna_ui::*;

const APPROVAL: &str = "15151515-1515-4151-8151-151515151515";
const INPUT: &str = "25252525-2525-4252-8252-252525252525";
const QUEUE: &str = "35353535-3535-4353-8353-353535353535";
const COMPLETE: &str = "45454545-4545-4454-8454-454545454545";
const TASKS: [&str; 4] = [APPROVAL, INPUT, QUEUE, COMPLETE];
const FIXTURE_ID: &str = "luna-l15-activity-attention-v1";
const BUNDLE_ID: &str = "com.novnski.luna.l15-qa";
const SAFETY: &str = "L15 synthetic fixture only. In-memory auth; fake power; sound and OS notifications disabled; native provider metadata and platform Keychain fallbacks disabled. No live provider/account/private store access.\n";

fn account_config(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    }
}

fn auth_config(root: &Path) -> AuthConfig {
    let mut config = AuthConfig::new("http://127.0.0.1:1", root.join("synthetic-auth"));
    config.dev_user_id = "l15-fixture-user".into();
    config.use_in_memory_credentials_for_tests();
    config
}

fn task_config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some("synthetic-l15-model".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Auto,
        effective_permission_mode: PermissionMode::Auto,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn request_entry(id: &str, approval: bool, device: &str) -> SessionMessageEntry {
    let request = if approval {
        "approval-request"
    } else {
        "input-request"
    };
    SessionMessageEntry {
        id: id.into(),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Input {
            id: request.into(),
            request_id: request.into(),
            questions: vec![UserInputQuestion {
                id: "question".into(),
                header: "Private fixture header".into(),
                question: "Private fixture question".into(),
                options: if approval {
                    // Mirror real approval decision labels without invoking a
                    // provider; the ordinary input case stays intentionally private.
                    vec!["Deny".into(), "Allow once".into(), "Cancel".into()]
                } else {
                    vec!["private option".into()]
                },
                multi_select: false,
                approval: approval.then(|| ApprovalRequest {
                    provider_request_id: request.into(),
                    provider: HarnessId::Mock,
                    turn_id: Some(id.into()),
                    machine_name: "synthetic Mac".into(),
                    action_kind: ApprovalActionKind::Tool,
                    summary: "Private approval detail".into(),
                    requested_scope: PermissionScope::Workspace,
                    expires_at_ms: 1_900_000_000_000,
                    supports_task_allowance: false,
                }),
            }],
            resolved: false,
        }],
        created_at: chrono::Utc::now().timestamp_millis(),
        device_id: device.into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    }
}

async fn seed(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    let existing = core.workspace.read_chats()?;
    if !existing.is_empty() {
        anyhow::ensure!(
            existing.len() == TASKS.len()
                && TASKS
                    .iter()
                    .all(|id| existing.iter().any(|chat| &chat.id == id)),
            "existing synthetic root does not match L15 fixture task identity"
        );
        return Ok(());
    }
    let cwd = root.join("synthetic-project");
    std::fs::create_dir_all(&cwd)?;
    core.workspace.create_space(
        "l15-synthetic-project",
        &core.device_id,
        cwd.to_str().unwrap(),
        Some("Synthetic Activity project".into()),
        false,
    )?;
    for (id, title) in [
        (APPROVAL, "Approval fixture task"),
        (INPUT, "Input fixture task"),
        (QUEUE, "Queued-send fixture task"),
        (COMPLETE, "Completed fixture task"),
    ] {
        core.workspace.create_chat(
            id,
            Some("l15-synthetic-project"),
            None,
            Some(task_config()),
            None,
        )?;
        core.workspace.rename_chat(id, title)?;
    }
    core.doc_host
        .open(APPROVAL)?
        .doc()
        .push_message(&request_entry("approval-turn", true, &core.device_id))?;
    core.doc_host
        .open(INPUT)?
        .doc()
        .push_message(&request_entry("input-turn", false, &core.device_id))?;
    let mut queue = QueuedMessage::new(
        "synthetic-queue-row",
        "Private queued payload never enters Activity",
        &core.device_id,
    );
    queue.delivery_gate = Some(QueueDeliveryGate::ReviewRequired {
        previous_lease_id: "synthetic-lease".into(),
        owner_device_id: core.device_id.clone(),
        since_ms: chrono::Utc::now().timestamp_millis(),
        base_text_hash: "synthetic-hash".into(),
    });
    core.doc_host.open(QUEUE)?.doc().push_queued(&queue)?;
    core.workspace.record_session(&Session {
        chat_id: COMPLETE.into(),
        device_id: core.device_id.clone(),
        status: SessionStatus::Idle,
        started_at: None,
        updated_at: chrono::Utc::now(),
        last_completed_turn: Some("synthetic-completed-turn".into()),
        recovery: None,
    });
    // The optional Codex-style card is transcript metadata, not a live
    // provider resolver. Keep it on the existing completed task so native QA
    // can inspect card + ordinary composer without creating a fifth task or
    // changing the runner's exact-root/task guard.
    core.doc_host
        .open(COMPLETE)?
        .doc()
        .push_message(&SessionMessageEntry {
            id: "synthetic-async-card".into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::AsyncQuestion {
                id: "aq-synthetic-call".into(),
                questions: vec![luna_proto::AsyncUserInputQuestion {
                    title: "Which synthetic sample should I use?".into(),
                    options: Some(vec!["Option A".into(), "Option B".into()]),
                }],
                resolved: false,
            }],
            created_at: chrono::Utc::now().timestamp_millis(),
            device_id: core.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })?;
    core.doc_host.flush_all();
    // The doc-host's last-message projection can land after the seed call.
    // Keep this synthetic read marker ahead of that bounded startup race so
    // the optional card is not mistaken for an unseen completed task.
    core.workspace
        .mark_chat_seen(COMPLETE, chrono::Utc::now() + chrono::Duration::hours(1))?;
    anyhow::ensure!(
        core.workspace
            .read_chats()?
            .iter()
            .any(|chat| chat.id == COMPLETE && !chat.unseen()),
        "completed synthetic task must be idle in Activity filter"
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    let snapshot: luna_proto::AttentionSnapshot = client
        .call_as(
            methods::GET_ATTENTION,
            serde_json::json!({"targetDeviceId": core.device_id}),
        )
        .await?;
    anyhow::ensure!(
        snapshot.unresolved_count == 3,
        "fixture Activity did not seed three needs-attention items"
    );
    anyhow::ensure!(
        snapshot.recent.len() == 1,
        "fixture completion did not seed Recent"
    );
    std::fs::write(
        root.join("FIXTURE.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "fixtureId": FIXTURE_ID,
            "bundleIdentifier": BUNDLE_ID,
            "root": root,
            "tasks": TASKS,
            "needsAttention": 3,
            "recent": 1,
            "defaultTask": APPROVAL,
            "asyncQuestionTask": COMPLETE,
            "idleTask": COMPLETE,
        }))?,
    )?;
    Ok(())
}

async fn capture_activity(
    window: gpui::WindowHandle<shell::Shell>,
    output: PathBuf,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let screenshot_window: gpui::AnyWindowHandle = window.clone().into();
    cx.background_executor().timer(Duration::from_secs(3)).await;
    screenshot_window.update(cx, |_, window, cx| {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
        window.draw(cx).clear();
        window.render_to_image()?.save(output.join("sidebar.png"))?;
        Ok::<_, anyhow::Error>(())
    })??;
    window.update(cx, |shell, window, cx| {
        shell.fixture_open_activity(window, cx)
    })?;
    cx.background_executor().timer(Duration::from_secs(1)).await;
    screenshot_window.update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window
            .render_to_image()?
            .save(output.join("activity.png"))?;
        Ok::<_, anyhow::Error>(())
    })??;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated synthetic root"));
    let capture_dir = std::env::args().nth(2).map(PathBuf::from);
    anyhow::ensure!(
        root.is_absolute() && !root.is_symlink(),
        "root must be absolute and not linked"
    );
    let root = root.canonicalize()?;
    anyhow::ensure!(
        root.to_string_lossy().starts_with("/private/tmp/luna-l15-"),
        "root must remain under a narrow /private/tmp/luna-l15-* path"
    );
    let engine_data = root.join("engine");
    if let Some(output) = &capture_dir {
        anyhow::ensure!(output.is_absolute(), "capture directory must be absolute");
        std::fs::create_dir_all(output)?;
    }
    let ui_data = root.join("ui");
    let temp_data = root.join("tmp");
    for path in [&engine_data, &ui_data, &temp_data] {
        std::fs::create_dir_all(path)?;
    }
    unsafe {
        std::env::set_var("TMPDIR", &temp_data);
        std::env::set_var("TMP", &temp_data);
        std::env::set_var("TEMP", &temp_data);
        std::env::set_var("LUNA_FIXTURE_FAKE_POWER", "1");
        std::env::set_var("LUNA_DISABLE_SOUND", "1");
        std::env::set_var("LUNA_DISABLE_NOTIFICATIONS", "1");
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let accounts = account_config(&root);
    accounts.disable_claude_keychain_fallback()?;
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            EngineProfile::development(&engine_data, "l15-fixture-org", "l15-fixture-user"),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            accounts,
        )
    })?;
    core.set_auth(Auth::new(auth_config(&root)));
    runtime.block_on(seed(&core, &root))?;
    std::fs::write(root.join("SAFETY.txt"), SAFETY)?;
    let ipc_port = std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port();
    let _ipc = runtime.block_on(luna_engine::serve_ipc(
        ipc_port,
        core.rpc_service(),
        &engine_data,
    ))?;
    let boot = EngineBootConfig {
        data_dir: engine_data.clone(),
        ipc_port,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let handle = runtime.block_on(state::EngineHandle::bootstrap(boot.clone()))?;
    let chats = core.workspace.read_chats()?;
    let spaces = core.workspace.read_spaces()?;
    let device = core.device_id.clone();
    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::load(&ui_data);
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
            app_menus::init(cx);
            let state = cx.new(|cx| {
                let mut state = state::AppState::new();
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": device,
                        "name": "Synthetic L15 Mac",
                        "platform": "macos",
                        "lastSeenAt": null
                    }))
                    .unwrap(),
                ];
                state.chats = chats;
                state.spaces = spaces;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.select_chat(Some(APPROVAL.into()), cx);
                state.fixture_attach_engine(handle, cx);
                state
            });
            let window = cx
                .open_window(
                    WindowOptions {
                        window_background: theme::Theme::of(cx).window_background_appearance(),
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(80.), px(80.)),
                            size(px(1100.), px(850.)),
                        ))),
                        ..Default::default()
                    },
                    |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
                )
                .unwrap();
            state.update(cx, |_, cx| cx.notify());
            cx.activate(true);
            if let Some(output) = capture_dir {
                cx.spawn(async move |cx| {
                    if let Err(error) = capture_activity(window, output, cx).await {
                        eprintln!("Activity fixture capture failed: {error:#}");
                    }
                    cx.update(|cx| cx.quit());
                })
                .detach();
            }
        });
    runtime.block_on(core.shutdown());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_fixture_uses_decision_labels_not_an_input_placeholder() {
        let approval = request_entry("approval-turn", true, "synthetic-device");
        let input = request_entry("input-turn", false, "synthetic-device");
        let [MessagePart::Input { questions, .. }] = approval.parts.as_slice() else {
            panic!("approval fixture must have one input request");
        };
        assert!(questions[0].approval.is_some());
        assert_eq!(
            questions[0]
                .options
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["Deny", "Allow once", "Cancel"]
        );
        let [MessagePart::Input { questions, .. }] = input.parts.as_slice() else {
            panic!("ordinary input fixture must have one input request");
        };
        assert!(questions[0].approval.is_none());
        assert_eq!(questions[0].options, vec!["private option".to_string()]);
    }

    #[test]
    fn fixture_accounts_and_auth_are_root_scoped_without_native_credentials() {
        let root = tempfile::tempdir().unwrap();
        let accounts = account_config(root.path());
        assert_eq!(accounts.data_dir, root.path().join("provider-state"));
        assert_eq!(accounts.codex_home, root.path().join("synthetic-codex"));
        assert_eq!(
            accounts.claude_config_dir,
            root.path().join("synthetic-claude")
        );
        let auth = auth_config(root.path());
        assert_eq!(auth.data_dir, root.path().join("synthetic-auth"));
        assert!(
            auth.test_credential_broker()
                .independent_in_memory_handle_for_tests()
                .is_some()
        );
    }
}
