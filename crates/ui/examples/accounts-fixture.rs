//! Interactive native Accounts QA fixture. Every provider path, credential
//! store, and account snapshot is synthetic; the fixture never calls an
//! upstream provider or the macOS Keychain.

use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use luna_proto::{
    AgentAccount, AgentAccountWarning, AgentAccountsSnapshot, AgentAuthKind,
    AgentAuthenticationState, HarnessId,
};
use luna_ui::*;
use std::sync::Arc;

fn unused_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn account(id: &str, label: &str, email: &str, active: bool) -> AgentAccount {
    AgentAccount {
        id: id.into(),
        harness: HarnessId::Codex,
        label: Some(label.into()),
        email: Some(email.into()),
        plan_label: Some("ChatGPT Plus".into()),
        active,
        authentication: AgentAuthenticationState::Authenticated,
        usage_windows: vec![],
        display_name: Some(format!("Provider {label}")),
        organization: None,
        auth_kind: Some(AgentAuthKind::Oauth),
        switchable: !active,
        saved_at: Some(1_788_900_000_000),
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("ui");
    let engine_data = temp.path().join("engine");
    let provider_root = temp.path().join("providers");
    let claude_dir = provider_root.join("claude");
    let codex_home = provider_root.join("codex");
    std::fs::create_dir_all(&claude_dir)?;
    std::fs::create_dir_all(&codex_home)?;
    std::fs::create_dir_all(&data)?;
    std::fs::write(
        claude_dir.join(".credentials.json"),
        serde_json::json!({"claudeAiOauth":{"accessToken":"fixture-only"}}).to_string(),
    )?;
    let claude_config = provider_root.join("claude.json");
    std::fs::write(
        &claude_config,
        serde_json::json!({
            "oauthAccount":{"accountUuid":"fixture-claude", "emailAddress":"claude@example.test"}
        })
        .to_string(),
    )?;
    std::fs::write(
        codex_home.join("auth.json"),
        serde_json::json!({"OPENAI_API_KEY":"fixture-only"}).to_string(),
    )?;

    let runtime = tokio::runtime::Runtime::new()?;
    let registry = Arc::new(luna_engine::HarnessRegistry::new());
    let profile =
        luna_engine::EngineProfile::development(&engine_data, "fixture-org", "fixture-user");
    let core = runtime.block_on(async {
        luna_engine::EngineCore::assemble_with_profile_and_agent_accounts_config(
            profile,
            registry,
            HarnessId::Mock,
            None,
            luna_engine::AgentAccountsConfig {
                data_dir: engine_data.clone(),
                claude_config_dir: claude_dir,
                claude_config_file: claude_config,
                codex_home,
                cursor_sdk_auth_file: provider_root.join("cursor/auth.json"),
            },
        )
    })?;
    let mut auth_config = luna_engine::AuthConfig::new("", &engine_data);
    auth_config.use_in_memory_credentials_for_tests();
    auth_config.dev_user_id = "fixture-user".into();
    core.set_auth(luna_engine::Auth::new(auth_config));
    core.workspace
        .create_chat("fixture-chat", None, Some(&core.device_id), None, None)?;
    let ipc_port = unused_port();
    let _ipc = runtime.block_on(luna_engine::serve_ipc(ipc_port, core.rpc_service()))?;
    let boot = EngineBootConfig {
        data_dir: data.clone(),
        ipc_port,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let handle = runtime.block_on(state::EngineHandle::bootstrap(boot.clone()))?;
    let chats = core.workspace.read_chats()?;
    let device = core.device_id.clone();
    let snapshot = AgentAccountsSnapshot {
        accounts: vec![
            AgentAccount {
                id: "claude-native".into(),
                harness: HarnessId::ClaudeCode,
                label: None,
                email: Some("claude@example.test".into()),
                plan_label: Some("Max".into()),
                active: true,
                authentication: AgentAuthenticationState::Authenticated,
                usage_windows: vec![],
                display_name: Some("Synthetic Claude".into()),
                organization: None,
                auth_kind: Some(AgentAuthKind::Oauth),
                switchable: false,
                saved_at: None,
            },
            account("named-work", "Work", "work@example.test", true),
            account("named-personal", "Personal", "personal@example.test", false),
        ],
        warnings: vec![AgentAccountWarning {
            harness: HarnessId::Codex,
            message: "Synthetic fixture: no provider or Keychain access is enabled.".into(),
        }],
    };

    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::default();
            settings::init(settings.clone(), data.clone(), cx);
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
            theme_library::init(data.clone(), cx);
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
            let state = cx.new(|_| {
                let mut state = state::AppState::new();
                state.fixture_attachment_engine(handle);
                state.connection = luna_proto::view::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
                state.local_device_id = Some(device.clone());
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": device,
                        "name": "Synthetic Mac",
                        "platform": "macos",
                        "lastSeenAt": null
                    }))
                    .unwrap(),
                ];
                state.chats = chats;
                state.selected_chat = Some("fixture-chat".into());
                state.auto_selected = true;
                state.chats_synced = true;
                state.spaces_synced = true;
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
            window
                .update(cx, |shell, _, cx| {
                    shell.fixture_accounts_settings(snapshot, cx);
                })
                .unwrap();
            state.update(cx, |_, cx| cx.notify());
            cx.activate(true);
        });
    Ok(())
}
