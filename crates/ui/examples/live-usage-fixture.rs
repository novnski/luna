//! CC-06 headless screenshots of the Usage page's "Live sessions" section and
//! the stream-observed Claude limit fallback. Data takes the production path:
//! provider accounting reports are recorded in the engine's live ledger (the
//! same call the session loop makes for `AgentEvent::ProviderAccounting`), and
//! the page reads them through the real `GetUsageSnapshot` RPC.
//!
//! The registered Claude harness points at the deterministic CC-06 fake CLI,
//! so the engine's background model refresh never starts a real `claude`.

use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowOptions, px, size};
use luna_proto::{
    ApiRetryObservation, HarnessId, LimitAvailability, ModelAccounting, ProviderAccountingReport,
    RateLimitObservation, RateLimitWindowObservation, ResultAccounting, RuntimeReadback,
    RuntimeReadbackSource, UsageProvider, UsageTokens,
};
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

async fn wait_rows(
    window: gpui::WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    for _ in 0..200 {
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
        if window.update(cx, |shell, _, _| shell.fixture_usage_page_live_rows())? == 2 {
            return Ok(());
        }
    }
    anyhow::bail!("live rows never reached the Usage page")
}

fn unused_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn fake_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-claude-cc06.py")
        .canonicalize()
        .expect("CC-06 fake CLI")
}

fn record(core: &luna_engine::EngineCore, now: i64) {
    let ledger = core.sessions.live_usage();
    let tokens = |uncached, cached, creation, output| UsageTokens {
        uncached_input: uncached,
        cached_input: cached,
        cache_creation: creation,
        output,
        reasoning: 0,
    };
    ledger.record(
        "live-auth",
        &ProviderAccountingReport::Runtime(RuntimeReadback {
            provider: UsageProvider::Claude,
            native_session_id: Some("sess-auth".into()),
            observed_at_ms: now - 60_000,
            source: RuntimeReadbackSource::LiveSwitch,
            requested_model: Some("claude-opus-5-5".into()),
            effective_model: Some("claude-opus-5-5".into()),
            requested_effort: Some("max".into()),
            effective_effort: Some("max".into()),
            matches: true,
            detail: None,
        }),
    );
    ledger.record(
        "live-auth",
        &ProviderAccountingReport::Result(ResultAccounting {
            provider: UsageProvider::Claude,
            native_session_id: Some("sess-auth".into()),
            observed_at_ms: now - 30_000,
            result_index: Some(3),
            origin: Some("human".into()),
            turn_tokens: tokens(1_204, 88_000, 2_400, 3_100),
            cumulative_cost_usd: Some(0.4213),
            models: vec![ModelAccounting {
                model: "claude-opus-5-5".into(),
                canonical_model: Some("claude-opus-5-5".into()),
                tokens: tokens(9_870, 1_120_000, 41_000, 22_500),
                cost_usd: Some(0.4213),
                context_window: Some(200_000),
                max_output_tokens: Some(32_000),
                web_search_requests: Some(0),
            }],
            duration_ms: Some(48_000),
            duration_api_ms: Some(41_000),
            num_turns: Some(6),
        }),
    );
    ledger.record(
        "live-auth",
        &ProviderAccountingReport::RateLimit(RateLimitObservation {
            provider: UsageProvider::Claude,
            native_session_id: Some("sess-auth".into()),
            observed_at_ms: now - 20_000,
            status: "allowed_warning".into(),
            limiting_window: Some("five_hour".into()),
            utilization: Some(0.91),
            resets_at_ms: Some(now + 2 * 3_600_000 + 14 * 60_000),
            windows: vec![
                RateLimitWindowObservation {
                    id: "five_hour".into(),
                    utilization: 0.91,
                    resets_at_ms: Some(now + 2 * 3_600_000 + 14 * 60_000),
                },
                RateLimitWindowObservation {
                    id: "seven_day".into(),
                    utilization: 0.42,
                    resets_at_ms: Some(now + 4 * 86_400_000),
                },
            ],
            overage_status: None,
            using_overage: Some(false),
        }),
    );
    ledger.record(
        "live-docs",
        &ProviderAccountingReport::Runtime(RuntimeReadback {
            provider: UsageProvider::Claude,
            native_session_id: Some("sess-docs".into()),
            observed_at_ms: now - 90_000,
            source: RuntimeReadbackSource::Launch,
            requested_model: Some("claude-haiku-4-5-20251001".into()),
            effective_model: Some("claude-haiku-4-5-20251001".into()),
            requested_effort: Some("high".into()),
            effective_effort: None,
            matches: false,
            detail: Some("effort high requested, none applied".into()),
        }),
    );
    ledger.record(
        "live-docs",
        &ProviderAccountingReport::Result(ResultAccounting {
            provider: UsageProvider::Claude,
            native_session_id: Some("sess-docs".into()),
            observed_at_ms: now - 80_000,
            result_index: Some(0),
            origin: Some("human".into()),
            turn_tokens: tokens(820, 14_000, 1_900, 640),
            cumulative_cost_usd: Some(0.0315),
            models: vec![ModelAccounting {
                model: "claude-haiku-4-5-20251001".into(),
                canonical_model: None,
                tokens: tokens(820, 14_000, 1_900, 640),
                cost_usd: Some(0.0315),
                context_window: Some(200_000),
                max_output_tokens: Some(32_000),
                web_search_requests: None,
            }],
            duration_ms: Some(6_000),
            duration_api_ms: Some(5_100),
            num_turns: Some(1),
        }),
    );
    ledger.record(
        "live-docs",
        &ProviderAccountingReport::ApiRetry(ApiRetryObservation {
            provider: UsageProvider::Claude,
            native_session_id: Some("sess-docs".into()),
            observed_at_ms: now - 70_000,
            attempt: 2,
            max_retries: 10,
            retry_delay_ms: 1_000,
            error_status: Some(529),
            error: Some("overloaded".into()),
        }),
    );
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    std::fs::create_dir_all(&output)?;
    // SAFETY: single-threaded here, before any runtime or engine thread runs.
    unsafe { std::env::set_var("CLAUDE_CODE_EXECUTABLE", fake_claude()) };
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("ui");
    let engine_data = temp.path().join("engine");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&engine_data)?;
    // A fresh price cache keeps the Usage RPC off the network.
    std::fs::write(
        engine_data.join("usage-model-rates.json"),
        serde_json::json!({"fetched_at_ms": chrono::Utc::now().timestamp_millis(),
            "rates": {"claude-opus-5-5": {"inputPerMillion": 5.0, "cachedPerMillion": 0.5,
            "creationPerMillion": 6.25, "outputPerMillion": 25.0}}})
        .to_string(),
    )?;
    let runtime = tokio::runtime::Runtime::new()?;
    let profile =
        luna_engine::EngineProfile::development(&engine_data, "fixture-org", "fixture-user");
    let provider_root = temp.path().join("providers");
    let registry = luna_engine::HarnessRegistry::new();
    registry.register(Arc::new(
        luna_harness::ClaudeHarness::new().with_executable(fake_claude()),
    ));
    let core = runtime.block_on(async {
        luna_engine::EngineCore::assemble_with_profile_and_agent_accounts_config(
            profile,
            Arc::new(registry),
            HarnessId::ClaudeCode,
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
        "live-project",
        &core.device_id,
        project.to_str().unwrap(),
        Some("Live usage fixture".into()),
        false,
    )?;
    for (id, title) in [
        ("live-auth", "Refactor auth flow"),
        ("live-docs", "Draft release notes"),
    ] {
        core.workspace
            .create_chat(id, Some("live-project"), Some(&core.device_id), None, None)?;
        core.workspace.rename_chat(id, title)?;
    }
    record(&core, chrono::Utc::now().timestamp_millis());
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
        default_harness: HarnessId::ClaudeCode,
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
            let state = cx.new(|_| {
                let mut state = state::AppState::new();
                state.fixture_attachment_engine(handle);
                state.connection = luna_proto::view::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
                state.local_device_id = Some(device.clone());
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": device, "name": "Fixture Mac", "platform": "macos",
                        "lastSeenAt": null
                    }))
                    .unwrap(),
                ];
                state.chats = chats;
                state.spaces = spaces;
                state.selected_chat = Some("live-auth".into());
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
                            size(px(1180.), px(760.)),
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
            cx.spawn(async move |cx| {
                let result = async {
                    window.update(cx, |shell, _, cx| shell.fixture_open_usage_page("cost", cx))?;
                    wait_rows(window, cx).await?;
                    capture(window.into(), cx, &capture_output.join("live-usage-cost.png"))?;
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_open_usage_page("limits", cx)
                    })?;
                    let mut limit = None;
                    for _ in 0..200 {
                        cx.background_executor()
                            .timer(Duration::from_millis(50))
                            .await;
                        limit = window.update(cx, |shell, _, _| {
                            shell.fixture_usage_page_claude_limit()
                        })?;
                        if limit
                            .as_ref()
                            .is_some_and(|l| l.availability == LimitAvailability::Unverified)
                        {
                            break;
                        }
                    }
                    let limit = limit.ok_or_else(|| anyhow::anyhow!("no Claude limit row"))?;
                    anyhow::ensure!(
                        limit.availability == LimitAvailability::Unverified
                            && limit.windows.iter().any(|w| w.id == "five_hour"),
                        "stream-observed Claude windows missing: {limit:?}"
                    );
                    capture(
                        window.into(),
                        cx,
                        &capture_output.join("live-usage-limits.png"),
                    )?;
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Light, cx));
                    window.update(cx, |shell, _, cx| shell.fixture_open_usage_page("cost", cx))?;
                    wait_rows(window, cx).await?;
                    capture(
                        window.into(),
                        cx,
                        &capture_output.join("live-usage-cost-light.png"),
                    )?;
                    std::fs::write(
                        capture_output.join("result.txt"),
                        format!(
                            "PASS: 2 live rows via GetUsageSnapshot; Claude limit {:?} with {} windows ({}).\n",
                            limit.availability,
                            limit.windows.len(),
                            limit.detail.unwrap_or_default()
                        ),
                    )?;
                    anyhow::Ok(())
                }
                .await;
                if let Err(error) = result {
                    let message = error.to_string();
                    std::fs::write(capture_output.join("failure.txt"), &message).ok();
                    eprintln!("live usage fixture failed: {message}");
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
    for name in [
        "live-usage-cost.png",
        "live-usage-limits.png",
        "live-usage-cost-light.png",
    ] {
        anyhow::ensure!(output.join(name).is_file(), "{name} was not written");
    }
    Ok(())
}
