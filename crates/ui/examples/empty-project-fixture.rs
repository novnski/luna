//! Native empty-project screen fixture with isolated synthetic state.
//! Run with `cargo run --locked -p luna-ui --example empty-project-fixture`.
use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, point, px, size};
use luna_ui::*;

fn main() -> anyhow::Result<()> {
    let output = std::env::args().nth(1).map(std::path::PathBuf::from);
    #[cfg(not(feature = "appshots-fixture"))]
    anyhow::ensure!(
        output.is_none(),
        "capture mode requires --features appshots-fixture"
    );
    if let Some(output) = &output {
        std::fs::create_dir_all(output)?;
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let temp = tempfile::tempdir()?;
    let data = temp.path().to_path_buf();
    #[cfg(feature = "appshots-fixture")]
    let application = if output.is_some() {
        gpui_platform::headless()
    } else {
        gpui_platform::application()
    };
    #[cfg(not(feature = "appshots-fixture"))]
    let application = gpui_platform::application();
    application.with_assets(icons::Assets).run(move |cx| {
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
        let appearance = if std::env::var_os("LUNA_PALETTE_LIGHT").is_some() {
            appearance::AppearanceMode::Light
        } else {
            appearance::AppearanceMode::Dark
        };
        appearance::init(
            appearance,
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
        if output.is_none() {
            app_menus::init(cx);
        }

        let state = cx.new(|_| {
            let mut state = state::AppState::new();
            state.connection = luna_proto::view::ConnectionStatus::Ready;
            state.workspace_scope = Some(luna_proto::WorkspaceScope::Local);
            state.local_device_id = Some("local".into());
            state.devices = serde_json::from_value(serde_json::json!([{
                "id": "local", "name": "Studio Mac", "platform": "macos", "lastSeenAt": null
            }]))
            .unwrap();
            state.auto_selected = true;
            state.chats_synced = true;
            state.spaces_synced = true;
            state
        });
        let boot = EngineBootConfig {
            data_dir: data,
            ipc_port: 0,
            edge_url: String::new(),
            edge_token: None,
            org_id: None,
            workos_client_id: None,
            default_harness: HarnessId::ClaudeCode,
        };
        let window = cx
            .open_window(
                WindowOptions {
                    show: output.is_none(),
                    focus: output.is_none(),
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(24.0), px(40.0)),
                        size(px(1100.0), px(800.0)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
            )
            .unwrap();
        state.update(cx, |_, cx| cx.notify());
        #[cfg(feature = "appshots-fixture")]
        if let Some(output) = output {
            cx.spawn(async move |cx| {
                let result = acceptance(window, state, cx, &output).await;
                if let Err(error) = result {
                    eprintln!("empty-project acceptance failed: {error:#}");
                    std::process::exit(1);
                }
                let _ = gpui::AnyWindowHandle::from(window)
                    .update(cx, |_, window, _| window.remove_window());
                cx.update(|cx| cx.quit());
            })
            .detach();
            return;
        }
        cx.activate(true);
    });
    Ok(())
}

#[cfg(feature = "appshots-fixture")]
async fn acceptance(
    window: gpui::WindowHandle<shell::Shell>,
    state: gpui::Entity<state::AppState>,
    cx: &mut gpui::AsyncApp,
    output: &std::path::Path,
) -> anyhow::Result<()> {
    use std::time::Duration;
    async fn pause(cx: &mut gpui::AsyncApp, ms: u64) {
        cx.background_executor()
            .timer(Duration::from_millis(ms))
            .await;
    }
    fn snapshot(
        window: gpui::WindowHandle<shell::Shell>,
        cx: &mut gpui::AsyncApp,
        output: &std::path::Path,
        name: &str,
    ) -> anyhow::Result<()> {
        gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
            window.draw(cx).clear();
            window
                .render_to_image()?
                .save(output.join(format!("{name}.png")))?;
            Ok::<(), anyhow::Error>(())
        })??;
        Ok(())
    }
    fn moon(
        window: gpui::WindowHandle<shell::Shell>,
        cx: &gpui::AsyncApp,
    ) -> anyhow::Result<MoonFixtureState> {
        window
            .read_with(cx, |shell, cx| shell.fixture_welcome_moon_state(cx))?
            .ok_or_else(|| anyhow::anyhow!("welcome moon is missing"))
    }
    fn mouse(
        window: gpui::WindowHandle<shell::Shell>,
        cx: &mut gpui::AsyncApp,
        position: gpui::Point<gpui::Pixels>,
    ) -> anyhow::Result<()> {
        gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers: Default::default(),
                }),
                cx,
            );
        })?;
        Ok(())
    }
    pause(cx, 900).await;
    snapshot(window, cx, output, "dark-rest")?;
    let resting = moon(window, cx)?;
    let bounds = resting
        .bounds
        .ok_or_else(|| anyhow::anyhow!("moon did not prepaint"))?;
    let center = bounds.center();
    anyhow::ensure!(resting.started.is_none(), "moon animates without a hover");
    pause(cx, 160).await;
    anyhow::ensure!(
        moon(window, cx)?.frame_requests == resting.frame_requests,
        "idle moon keeps scheduling frames"
    );
    mouse(window, cx, center)?;
    pause(cx, 300).await;
    snapshot(window, cx, output, "dark-turn")?;
    let turning = moon(window, cx)?;
    anyhow::ensure!(
        turning.started.is_some() && turning.rows != resting.rows,
        "hover did not turn the surface"
    );
    mouse(window, cx, point(px(20.0), px(80.0)))?;
    pause(cx, 140).await;
    snapshot(window, cx, output, "dark-turn-after-leave")?;
    anyhow::ensure!(
        moon(window, cx)?.started == turning.started,
        "pointer leave interrupted the turn"
    );
    mouse(window, cx, center)?;
    pause(cx, 140).await;
    snapshot(window, cx, output, "dark-turn-after-reentry")?;
    anyhow::ensure!(
        moon(window, cx)?.started == turning.started,
        "reentry restarted the turn"
    );
    mouse(window, cx, point(px(20.0), px(80.0)))?;
    pause(cx, 4600).await;
    snapshot(window, cx, output, "dark-settled")?;
    let settled = moon(window, cx)?;
    anyhow::ensure!(
        settled.started.is_none() && settled.rows == resting.rows,
        "moon did not return to its resting surface"
    );
    pause(cx, 180).await;
    anyhow::ensure!(
        moon(window, cx)?.frame_requests == settled.frame_requests,
        "settled moon keeps scheduling frames"
    );
    mouse(window, cx, center)?;
    pause(cx, 200).await;
    snapshot(window, cx, output, "dark-before-reduced")?;
    anyhow::ensure!(
        moon(window, cx)?.started.is_some(),
        "second hover did not start a turn"
    );
    cx.update(|cx| motion::set_reduced_motion(cx, true));
    pause(cx, 160).await;
    snapshot(window, cx, output, "dark-reduced-motion")?;
    let reduced = moon(window, cx)?;
    anyhow::ensure!(
        reduced.started.is_none() && reduced.rows == resting.rows,
        "reduced motion did not park the moon"
    );
    mouse(window, cx, point(px(20.0), px(80.0)))?;
    mouse(window, cx, center)?;
    pause(cx, 160).await;
    anyhow::ensure!(
        moon(window, cx)?.started.is_none()
            && moon(window, cx)?.frame_requests == reduced.frame_requests,
        "reduced-motion hover schedules animation"
    );
    mouse(window, cx, point(px(20.0), px(80.0)))?;
    cx.update(|cx| {
        let saved = settings::current(cx);
        appearance::init(
            appearance::AppearanceMode::Light,
            saved.theme_selection,
            saved.accent,
            saved.surface,
            cx,
        );
    });
    pause(cx, 200).await;
    snapshot(window, cx, output, "light-rest")?;
    anyhow::ensure!(
        moon(window, cx)?.rows != resting.rows,
        "light appearance did not adapt the ink coverage"
    );
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, _| window.resize(size(px(700.0), px(560.0))))?;
    pause(cx, 200).await;
    snapshot(window, cx, output, "light-small")?;
    let small = moon(window, cx)?.bounds.unwrap();
    anyhow::ensure!(
        small.origin.x >= px(0.0)
            && small.origin.y >= px(0.0)
            && small.right() <= px(700.0)
            && small.bottom() <= px(560.0),
        "moon clips in a small window"
    );
    // The primary action sits below the measured moon, heading, and two gaps.
    let button = point(
        small.center().x,
        small.bottom() + px(22.0 + 28.0 + 24.0 + 18.0),
    );
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                position: button,
                button: gpui::MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
        window.dispatch_event(
            gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                position: button,
                button: gpui::MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
    })?;
    pause(cx, 250).await;
    snapshot(window, cx, output, "project-picker")?;
    anyhow::ensure!(
        window.read_with(cx, |shell, _| shell.fixture_welcome_picker_open())?,
        "Add project did not open the picker"
    );
    // A different route must dispose of the moon rather than keep an invisible clock.
    cx.update(|cx| {
        state.update(cx, |state, cx| {
            state.selected_chat = Some("fixture-route".into());
            cx.notify();
        })
    });
    pause(cx, 250).await;
    snapshot(window, cx, output, "route-exit")?;
    anyhow::ensure!(
        window.read_with(cx, |shell, cx| shell
            .fixture_welcome_moon_state(cx)
            .is_none())?,
        "route exit retained the moon"
    );
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "result": "PASS",
            "checks": ["idle parks", "hover rotates surface", "leave completes turn", "reentry preserves clock", "returns to original surface", "settled parks", "second hover", "reduced motion cancels", "reduced hover parks", "light ink adapts", "small window fits", "Add project opens picker", "route exit disposes moon"],
            "animatedFrameRequests": settled.frame_requests,
            "windowSizes": [[1100, 800], [700, 560]],
            "data": "isolated temporary fixture state; no user profile or provider data"
        }))?,
    )?;
    Ok(())
}
