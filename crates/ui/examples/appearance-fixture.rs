//! Isolated end-to-end rendering of the production Appearance page.
//! Uses default settings and no account, provider, or user profile data.

use gpui::{
    AppContext, AsyncApp, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use luna_ui::{appearance, icons, settings, theme, theme_library, typography};
use std::{path::PathBuf, time::Duration};

struct AppearanceRoot {
    page: Entity<settings::appearance::AppearancePage>,
}

impl Render for AppearanceRoot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::Theme::of(cx);
        div()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .child(self.page.clone())
    }
}

fn capture(
    window: gpui::AnyWindowHandle,
    cx: &mut AsyncApp,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok::<(), anyhow::Error>(())
    })??;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    std::fs::create_dir_all(&output)?;
    let profile = tempfile::tempdir()?;
    let data = profile.path().join("ui");
    std::fs::create_dir_all(&data)?;
    let capture_output = output.clone();

    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::default();
            settings::init(settings.clone(), data.clone(), cx);
            let fonts = typography::register_fonts(cx);
            typography::init(
                settings.ui_font_family,
                settings.ui_font_size,
                settings.terminal_font_family,
                settings.terminal_font_size,
                settings.code_font_family,
                settings.code_font_size,
                fonts,
                cx,
            );
            theme_library::init(data, cx);
            appearance::init(
                appearance::AppearanceMode::Light,
                settings.theme_selection,
                settings.accent,
                settings.surface,
                cx,
            );
            let overview_page = cx.new(settings::appearance::AppearancePage::new);
            let overview_root_page = overview_page.clone();
            let overview = cx
                .open_window(
                    WindowOptions {
                        window_background: gpui::WindowBackgroundAppearance::Opaque,
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(20.0), px(20.0)),
                            size(px(1100.0), px(1850.0)),
                        ))),
                        show: false,
                        focus: false,
                        ..Default::default()
                    },
                    move |_, cx| {
                        cx.new(|_| AppearanceRoot {
                            page: overview_root_page,
                        })
                    },
                )
                .unwrap();
            let controls_page = cx.new(settings::appearance::AppearancePage::new);
            let controls_root_page = controls_page.clone();
            let controls = cx
                .open_window(
                    WindowOptions {
                        window_background: gpui::WindowBackgroundAppearance::Opaque,
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(20.0), px(20.0)),
                            size(px(1100.0), px(900.0)),
                        ))),
                        show: false,
                        focus: false,
                        ..Default::default()
                    },
                    move |_, cx| {
                        cx.new(|_| AppearanceRoot {
                            page: controls_root_page,
                        })
                    },
                )
                .unwrap();
            cx.activate(true);
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                let result = (|| -> anyhow::Result<()> {
                    capture(
                        overview.into(),
                        cx,
                        &capture_output.join("appearance-light-overview.png"),
                    )?;
                    controls_page.update(cx, |page, cx| page.fixture_scroll_to_bottom(cx));
                    capture(
                        controls.into(),
                        cx,
                        &capture_output.join("appearance-light-controls.png"),
                    )?;
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark, cx));
                    capture(
                        controls.into(),
                        cx,
                        &capture_output.join("appearance-dark-controls.png"),
                    )?;
                    std::fs::write(
                        capture_output.join("result.txt"),
                        "Rendered the production Appearance page with isolated default settings. Captures: light overview, light controls, dark controls. No user profile or provider data was used.\n",
                    )?;
                    Ok(())
                })();
                if let Err(error) = result {
                    eprintln!("appearance fixture failed: {error:#}");
                    std::process::exit(1);
                }
                let _ = overview.update(cx, |_, window, _| window.remove_window());
                let _ = controls.update(cx, |_, window, _| window.remove_window());
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    Ok(())
}
