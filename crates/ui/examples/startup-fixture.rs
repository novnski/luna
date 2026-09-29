//! Rendered startup acceptance using persisted synthetic workspace data and
//! the production engine watches, shell, and NewSession action.
//!
//! Failure cases: settled/archived-only histories reopen a finished task;
//! automatic settling is ignored; a newer finished task hides an active one;
//! an empty workspace never finishes boot; late watches override an explicit
//! new-session choice; explicit navigation to a finished task is overridden.
//!
//! Run twice with the same case directory to check process-restart persistence:
//! target/debug/examples/startup-fixture <evidence-directory> <case> <label>
//! Cases: empty, settled, archived, mixed-finished, auto-settled, active,
//! mixed-active, manual-new, manual-archived. Each run saves PNG + JSON readback.

use std::{path::PathBuf, sync::Arc, time::Duration};

use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_proto::{ChatSettlementOverride, HarnessId, SettlementDisposition};
use luna_ui::*;

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).expect("evidence directory"));
    let case = std::env::args().nth(2).expect("case");
    let label = std::env::args().nth(3).expect("run label");
    anyhow::ensure!(
        matches!(
            case.as_str(),
            "empty"
                | "settled"
                | "archived"
                | "mixed-finished"
                | "auto-settled"
                | "active"
                | "mixed-active"
                | "manual-new"
                | "manual-archived"
        ),
        "unknown startup case"
    );
    anyhow::ensure!(
        label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "unsafe label"
    );
    let data = output.join("ui");
    let engine_data = output.join("engine");
    let project = output.join("project");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&project)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "startup-fixture", "startup-fixture"),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            AgentAccountsConfig {
                data_dir: engine_data.clone(),
                claude_config_dir: output.join("providers/claude"),
                claude_config_file: output.join("providers/claude.json"),
                codex_home: output.join("providers/codex"),
            },
        )
    })?;
    let mut auth = AuthConfig::new("", output.join("auth"));
    auth.dev_user_id = "startup-fixture".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    if core.workspace.read_spaces()?.is_empty() {
        core.workspace.create_space(
            "startup-project",
            &core.device_id,
            project.to_str().unwrap(),
            Some("Startup QA".into()),
            false,
        )?;
        let tasks: &[(&str, &str)] = match case.as_str() {
            "empty" => &[],
            "settled" => &[("settled", "settled")],
            "archived" | "manual-archived" => &[("archived", "archived")],
            "mixed-finished" => &[("archived", "archived"), ("settled", "settled")],
            "auto-settled" => &[("old", "old")],
            "mixed-active" => &[
                ("active", "active"),
                ("settled", "settled"),
                ("archived", "archived"),
            ],
            _ => &[("active", "active")],
        };
        for (index, (id, disposition)) in tasks.iter().enumerate() {
            core.workspace.create_chat(
                id,
                Some("startup-project"),
                Some(&core.device_id),
                None,
                Some(project.to_string_lossy().into_owned()),
            )?;
            core.workspace
                .rename_chat(id, &format!("Startup QA {id}"))?;
            let at = if *disposition == "old" {
                chrono::Utc::now() - chrono::Duration::days(3)
            } else {
                chrono::Utc::now() + chrono::Duration::seconds(index as i64)
            };
            let mut chat = core.workspace.chat(id)?.expect("seeded startup chat");
            chat.last_message_at = Some(at);
            if *disposition == "old" {
                // Automatic settling applies to idle/read tasks, not unread
                // completed turns that still need the user's attention.
                chat.last_seen_at = Some(at + chrono::Duration::seconds(1));
            }
            if *disposition == "archived" {
                chat.archived = true;
            } else if *disposition == "settled" {
                chat.settlement_override = Some(ChatSettlementOverride {
                    disposition: SettlementDisposition::Settled,
                    at: at + chrono::Duration::seconds(1),
                });
            }
            core.workspace.import_chat_row(&chat)?;
        }
    }
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let boot = EngineBootConfig {
        data_dir: data.clone(),
        ipc_port: 59999,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let failure = Arc::new(std::sync::Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
    let device = core.device_id.clone();
    gpui_platform::headless()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let mut settings = settings::UiSettings::load(&data);
            settings.sidebar_auto_settle_after_days = Some(1);
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
                state.local_device_id = Some(device);
                state
            });
            let window: gpui::AnyWindowHandle = cx
                .open_window(
                    WindowOptions {
                        window_background: theme::Theme::of(cx).window_background_appearance(),
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(20.), px(20.)),
                            size(px(1100.), px(800.)),
                        ))),
                        show: false,
                        focus: false,
                        ..Default::default()
                    },
                    |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
                )
                .unwrap()
                .into();
            // Dispatch the actual Cmd+N action before the first workspace watches.
            if case == "manual-new" {
                window
                    .update(cx, |_, window, cx| {
                        window.draw(cx).clear();
                        window.dispatch_action(Box::new(shell::NewSession), cx);
                    })
                    .unwrap();
            } else if case == "manual-archived" {
                state.update(cx, |state, cx| {
                    state.select_chat(Some("archived".into()), cx)
                });
            }
            state.update(cx, |state, cx| state.fixture_attach_engine(handle, cx));
            cx.spawn(async move |cx| {
                let result: anyhow::Result<()> = async {
                    for _ in 0..100 {
                        cx.background_executor()
                            .timer(Duration::from_millis(50))
                            .await;
                        let synced = cx.update(|cx| {
                            let state = state.read(cx);
                            state.chats_synced && state.spaces_synced
                        });
                        if synced {
                            break;
                        }
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(500))
                        .await;
                    let readback = cx.update(|cx| {
                    let state = state.read(cx);
                    serde_json::json!({
                        "case": case, "selectedChat": state.selected_chat,
                        "selectedSpace": state.selected_space, "autoSelected": state.auto_selected,
                        "chatsSynced": state.chats_synced, "spacesSynced": state.spaces_synced,
                        "chats": state.chats,
                    })
                });
                    std::fs::write(
                        output.join(format!("{label}.json")),
                        serde_json::to_string_pretty(&readback)?,
                    )?;
                    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
                        window.draw(cx).clear();
                        window
                            .render_to_image()?
                            .save(output.join(format!("{label}.png")))?;
                        Ok(())
                    })??;
                    anyhow::ensure!(
                        readback["chatsSynced"] == true && readback["spacesSynced"] == true,
                        "workspace watches did not synchronize: {readback}"
                    );
                    let expected = match case.as_str() {
                        "active" | "mixed-active" => Some("active"),
                        "manual-archived" => Some("archived"),
                        _ => None,
                    };
                    anyhow::ensure!(
                        readback["selectedChat"].as_str() == expected,
                        "wrong startup selection; expected {expected:?}: {readback}"
                    );
                    anyhow::ensure!(
                        readback["autoSelected"] == true,
                        "startup decision remained pending: {readback}"
                    );
                    anyhow::ensure!(
                        readback["selectedSpace"] == "startup-project",
                        "new session lost its project: {readback}"
                    );
                    std::fs::write(output.join(format!("{label}.pass")), "PASS\n")?;
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    eprintln!("Startup acceptance failed: {error}");
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
    Ok(())
}
