//! Headless screenshots of the compact Usage card and optional local settings.

use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowOptions, px, size};
use luna_proto::{ChatConfig, HarnessId, PermissionMode, SandboxLevel, Session, SessionStatus};
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
    let appearance_audit = std::env::args().nth(3).as_deref() == Some("appearance-audit");
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
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project)?;
    core.workspace.create_space(
        "usage-project",
        &core.device_id,
        project.to_str().unwrap(),
        Some("Usage fixture project".into()),
        false,
    )?;
    core.workspace.create_chat(
        "usage-fixture",
        Some("usage-project"),
        Some(&core.device_id),
        None,
        None,
    )?;
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
    let spaces = core.workspace.read_spaces()?;
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
                state.spaces = spaces;
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
                    window.update(cx, |shell, _, _| {
                        assert!(!shell.fixture_usage_card_is_open());
                    })?;
                    capture(
                        window.into(),
                        cx,
                        &capture_output.join("usage-startup-closed.png"),
                    )?;
                    // Exercise the footer action through the rendered window.
                    // The 600x500 fixture places Usage at (101, 474).
                    click(window.into(), cx, 101.0, 474.0)?;
                    window.update(cx, |shell, _, _| {
                        assert!(shell.fixture_usage_card_is_open());
                    })?;
                    capture(window.into(), cx, &capture_output.join("usage-codex.png"))?;
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
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Light, cx));
                    capture(
                        window.into(),
                        cx,
                        &capture_output.join("usage-light-opencode.png"),
                    )?;
                    click(window.into(), cx, 29.0, 347.0)?;
                    capture(
                        window.into(),
                        cx,
                        &capture_output.join("usage-light-codex.png"),
                    )?;
                    if appearance_audit {
                        let mut results = Vec::new();
                        for (variant, mode) in [
                            ("luna-dark", appearance::AppearanceMode::Dark),
                            ("catppuccin-mocha", appearance::AppearanceMode::Dark),
                            ("nord", appearance::AppearanceMode::Dark),
                            ("luna-light", appearance::AppearanceMode::Light),
                            ("catppuccin-latte", appearance::AppearanceMode::Light),
                        ] {
                            for surface in [
                                luna_theme::SurfacePreference::Opaque,
                                luna_theme::SurfacePreference::Frosted,
                            ] {
                                window.update(cx, |_, window, cx| {
                                    appearance::set_mode(mode, cx);
                                    let resolved = theme::Theme::of(cx).appearance;
                                    appearance::set_theme(resolved, variant, cx);
                                    appearance::set_surface(surface, cx);
                                    appearance::set_accent(
                                        luna_theme::AccentSelection::Preset(luna_theme::AccentPreset::Pink),
                                        cx,
                                    );
                                    typography::set_family(typography::UiFontFamily::System, cx);
                                    typography::set_font_size(typography::UiFontSize::ALL[6], window, cx);
                                    typography::set_code_font_size(18.0, cx);
                                    typography::set_terminal_font_size(16.0, cx);
                                    settings::flush(cx);
                                    assert_eq!(settings::UiSettings::load(&data), settings::current(cx));
                                })?;
                                let name = format!("appearance-{variant}-{surface:?}-20px.png");
                                let file = capture_output.join(&name);
                                capture(window.into(), cx, &file)?;
                                let expected = window.update(cx, |shell, _, cx| {
                                    assert!(shell.fixture_usage_card_is_open());
                                    assert_eq!(theme::Theme::of(cx).font_sans, ".SystemUIFont");
                                    assert_eq!(theme::Theme::of(cx).code_font_size, 18.0);
                                    assert_eq!(theme::Theme::of(cx).terminal_font_size, 16.0);
                                    gpui::Rgba::from(theme::Theme::of(cx).surface)
                                })?;
                                if surface == luna_theme::SurfacePreference::Opaque {
                                    let pixels = image::open(&file)?.to_rgb8();
                                    let scale = pixels.width() as f32 / 600.0;
                                    let actual = pixels.get_pixel(
                                        ((sidebar_width - 14.0) * scale) as u32,
                                        (400.0 * scale) as u32,
                                    ).0;
                                    let expected = [expected.r, expected.g, expected.b]
                                        .map(|v| (v * 255.0).round() as u8);
                                    anyhow::ensure!(actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) <= 3),
                                        "Usage card ignores {variant}: actual {actual:?}, expected {expected:?}");
                                }
                                results.push(name);
                            }
                        }
                        window.update(cx, |_, window, cx| {
                            typography::set_font_size(typography::UiFontSize::default(), window, cx);
                            typography::set_family(typography::UiFontFamily::Geist, cx);
                            appearance::set_theme(theme::Appearance::Dark, "luna-dark", cx);
                            appearance::set_theme(theme::Appearance::Light, "luna-light", cx);
                            appearance::set_surface(luna_theme::SurfacePreference::ThemeDefault, cx);
                        })?;
                        std::fs::write(capture_output.join("appearance-matrix.json"),
                            serde_json::to_vec_pretty(&results)?)?;
                    }
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark, cx));
                    click(window.into(), cx, 101.0, 474.0)?;
                    window.update(cx, |shell, _, _| {
                        assert!(!shell.fixture_usage_card_is_open());
                    })?;
                    state.update(cx, |state, cx| {
                        let chat = state
                            .chats
                            .iter_mut()
                            .find(|chat| chat.id == "usage-fixture")
                            .unwrap();
                        chat.config = Some(ChatConfig {
                            harness: HarnessId::Codex,
                            provider_account_id: None,
                            model: None,
                            reasoning: None,
                            model_options: Default::default(),
                            sandbox: SandboxLevel::ReadOnly,
                            permission_mode: PermissionMode::Ask,
                            effective_permission_mode: PermissionMode::Ask,
                            permission_ceiling_source: None,
                            permission_degradation_reason: None,
                            permission_audit: Vec::new(),
                        });
                        state.sessions = vec![Session {
                            last_completed_turn: None,
                            chat_id: chat.id.clone(),
                            device_id: chat.device_id.clone(),
                            status: SessionStatus::Working,
                            started_at: Some(chrono::Utc::now()),
                            updated_at: chrono::Utc::now(),
                            recovery: None,
                        }];
                        cx.notify();
                    });
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_provider_usage(cx);
                        shell.fixture_add_compact_usage_providers(cx);
                        shell.fixture_poll_usage(cx);
                        assert!(
                            shell.fixture_usage_card_is_open(),
                            "low quota on a working Codex task should open usage"
                        );
                    })?;
                    capture(
                        window.into(),
                        cx,
                        &capture_output.join("usage-low-auto-open.png"),
                    )?;
                    click(window.into(), cx, 101.0, 474.0)?;
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_poll_usage(cx);
                        assert!(
                            !shell.fixture_usage_card_is_open(),
                            "dismissing a low-usage alert must stay dismissed"
                        );
                    })?;
                    capture(
                        window.into(),
                        cx,
                        &capture_output.join("usage-low-dismissed.png"),
                    )?;
                    if local_settings {
                        click(window.into(), cx, 29.0, 474.0)?;
                        capture(
                            window.into(),
                            cx,
                            &capture_output.join("settings-local.png"),
                        )?;
                    }
                    std::fs::write(capture_output.join("result.txt"), "PASS: rendered Usage actions, quota alert/dismissal, appearance matrix (when requested), saved preference readback.\n")?;
                    Ok(())
                })();
                if let Err(error) = result {
                    let message = error.to_string();
                    std::fs::write(capture_output.join("failure.txt"), &message).ok();
                    eprintln!("usage fixture failed: {message}");
                    *failure_for_ui.lock().unwrap() = Some(message);
                    std::process::exit(1);
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
        "usage-startup-closed.png",
        "usage-closed.png",
        "usage-reopened.png",
        "usage-opencode.png",
        "usage-light-opencode.png",
        "usage-light-codex.png",
        "usage-low-auto-open.png",
        "usage-low-dismissed.png",
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
