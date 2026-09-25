//! Headless screenshots of the compact Usage card and optional local settings.

use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowOptions, px, size};
use luna_proto::HarnessId;
use luna_ui::*;
use std::{path::PathBuf, sync::Arc, time::Duration};

fn capture(
    window: gpui::AnyWindowHandle,
    cx: &mut AsyncApp,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })?
}

fn click(window: gpui::AnyWindowHandle, cx: &mut AsyncApp, x: f32, y: f32) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| {
        window.draw(cx).clear();
        let position = gpui::point(px(x), px(y));
        window.dispatch_event(
            gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                position,
                button: gpui::MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
        window.dispatch_event(
            gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                position,
                button: gpui::MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
    })?;
    Ok(())
}

fn unused_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    let sidebar_width: f32 = std::env::args()
        .nth(2)
        .as_deref()
        .unwrap_or("300")
        .parse()?;
    let local_settings = std::env::args().nth(3).as_deref() == Some("local-settings");
    std::fs::create_dir_all(&output)?;
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("ui");
    let engine_data = temp.path().join("engine");
    std::fs::create_dir_all(&data)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let profile =
        luna_engine::EngineProfile::development(&engine_data, "fixture-org", "fixture-user");
    let provider_root = temp.path().join("providers");
    let core = runtime.block_on(async {
        luna_engine::EngineCore::assemble_with_profile_and_agent_accounts_config(
            profile,
            Arc::new(luna_engine::HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            luna_engine::AgentAccountsConfig {
                data_dir: engine_data.clone(),
                claude_config_dir: provider_root.join("claude"),
                claude_config_file: provider_root.join("claude.json"),
                codex_home: provider_root.join("codex"),
            },
        )
    })?;
    let mut auth_config = luna_engine::AuthConfig::new("", &engine_data);
    auth_config.use_in_memory_credentials_for_tests();
    auth_config.dev_user_id = "fixture-user".into();
    core.set_auth(luna_engine::Auth::new(auth_config));
    core.workspace
        .create_chat("usage-fixture", None, Some(&core.device_id), None, None)?;
    let ipc_port = unused_port();
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
    let device = core.device_id.clone();
    let failure = Arc::new(std::sync::Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
    let capture_output = output.clone();

    gpui_platform::headless()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let mut settings = settings::UiSettings::default();
            settings.sidebar_width = sidebar_width;
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
            let state = cx.new(|_| {
                let mut state = state::AppState::new();
                state.fixture_attachment_engine(handle);
                state.connection = luna_proto::view::ConnectionStatus::Ready;
                state.workspace_scope = Some(if local_settings {
                    luna_proto::WorkspaceScope::Local
                } else {
                    luna_proto::WorkspaceScope::Development
                });
                state.local_device_id = Some(device.clone());
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": device,
                    "name": "Iulian's MacBook Pro 16",
                        "platform": "macos",
                        "lastSeenAt": null
                    }))
                    .unwrap(),
                ];
                state.chats = chats;
                state.selected_chat = Some("usage-fixture".into());
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
                            gpui::point(px(20.), px(20.)),
                            size(px(600.), px(500.)),
                        ))),
                        show: false,
                        focus: false,
                        ..Default::default()
                    },
                    |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
                )
                .unwrap();
            state.update(cx, |_, cx| cx.notify());
            cx.activate(true);
            window
                .update(cx, |shell, _, cx| {
                    shell.fixture_provider_usage(cx);
                    shell.fixture_add_compact_usage_providers(cx);
                })
                .unwrap();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let result = (|| -> anyhow::Result<()> {
                    capture(window.into(), cx, &capture_output.join("usage-codex.png"))?;
                    // Exercise the footer action through the rendered window.
                    // The 600x500 fixture places Usage at (101, 474).
                    click(window.into(), cx, 101.0, 474.0)?;
                    capture(window.into(), cx, &capture_output.join("usage-closed.png"))?;
                    click(window.into(), cx, 101.0, 474.0)?;
                    capture(
                        window.into(),
                        cx,
                        &capture_output.join("usage-reopened.png"),
                    )?;
                    click(window.into(), cx, 61.0, 347.0)?;
                    capture(
                        window.into(),
                        cx,
                        &capture_output.join("usage-opencode.png"),
                    )?;
                    if local_settings {
                        click(window.into(), cx, 29.0, 474.0)?;
                        capture(
                            window.into(),
                            cx,
                            &capture_output.join("settings-local.png"),
                        )?;
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    *failure_for_ui.lock().unwrap() = Some(error.to_string());
                }
                let _ = window.update(cx, |_, window, _| window.remove_window());
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    runtime.block_on(core.shutdown());
    if let Some(error) = failure.lock().unwrap().take() {
        anyhow::bail!(error);
    }
    if [
        "usage-codex.png",
        "usage-closed.png",
        "usage-reopened.png",
        "usage-opencode.png",
    ]
    .iter()
    .any(|name| !output.join(name).is_file())
    {
        anyhow::bail!("usage screenshots were not written");
    }
    if local_settings && !output.join("settings-local.png").is_file() {
        anyhow::bail!("local settings screenshot was not written");
    }
    Ok(())
}
