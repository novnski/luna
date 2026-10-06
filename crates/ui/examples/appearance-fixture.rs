//! Isolated end-to-end rendering of the production Appearance page.
//! Uses default settings and no account, provider, or user profile data.

use gpui::{
    AppContext, AsyncApp, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use luna_ui::{appearance, icons, settings, theme, theme_library, typography};
use std::{path::PathBuf, time::Duration};

struct ToolDetailsRoot {
    details: Vec<luna_ui::transcript::ToolDetail>,
}

impl Render for ToolDetailsRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
        let theme = theme::Theme::of(cx);
        div()
            .size_full()
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .text_size(typography::ui_rems(13.0))
            .child("Production tool output, thought and inline diff")
            .children(self.details.iter().map(|detail| {
                luna_ui::transcript::fixture_detail_panel(
                    detail,
                    theme,
                    typography::font_size(cx).pixels() / 16.0,
                )
            }))
            .child(luna_ui::transcript::fixture_review_card(theme, cx))
    }
}

struct AppearanceRoot {
    page: Entity<settings::appearance::AppearancePage>,
}

impl Render for AppearanceRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
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

    gpui_platform::headless()
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
            theme_library::init(data.clone(), cx);
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
            let file = luna_ui::changes::parse_patch("diff --git a/demo.rs b/demo.rs\n--- a/demo.rs\n+++ b/demo.rs\n@@ -1,2 +1,2 @@\n-old_value\n+new_value\n context\n\\ No newline at end of file\n").remove(0);
            let details = vec![
                luna_ui::transcript::ToolDetail::Output { lines: vec!["cargo check --locked".into(), "    Checking luna-ui".into(), "    Finished successfully".into()], truncated_by: 2, raw_text: "cargo check --locked".into() },
                luna_ui::transcript::ToolDetail::Thought { lines: vec![vec![luna_ui::markdown::parser::InlineRun { text: "Thought text follows the interface font.".into(), style: Default::default() }]], truncated_by: 1 , source: None, live: false },
                luna_ui::transcript::ToolDetail::Diff { file: std::sync::Arc::new(file), old_text: None, new_text: None },
            ];
            let tools = cx.open_window(WindowOptions {
                window_background: gpui::WindowBackgroundAppearance::Opaque,
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(gpui::point(px(20.0), px(20.0)), size(px(1100.0), px(900.0))))),
                show: false, focus: false, ..Default::default()
            }, |_, cx| cx.new(|_| ToolDetailsRoot { details })).unwrap();
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
                    capture(tools.into(), cx, &capture_output.join("tools-default.png"))?;
                    for (name, mode, variant, surface, font, size) in [
                        ("dark-opaque-system-20", appearance::AppearanceMode::Dark, "catppuccin-mocha", luna_theme::SurfacePreference::Opaque, typography::UiFontFamily::System, typography::UiFontSize::ALL[6]),
                        ("light-frosted-geist-12", appearance::AppearanceMode::Light, "catppuccin-latte", luna_theme::SurfacePreference::Frosted, typography::UiFontFamily::Geist, typography::UiFontSize::ALL[0]),
                    ] {
                        overview.update(cx, |_, window, cx| {
                            appearance::set_mode(mode, cx);
                            appearance::set_theme(theme::Theme::of(cx).appearance, variant, cx);
                            appearance::set_surface(surface, cx);
                            appearance::set_accent(luna_theme::AccentSelection::Preset(luna_theme::AccentPreset::Green), cx);
                            typography::set_family(font, cx);
                            typography::set_font_size(size, window, cx);
                            typography::set_code_font_size(18.0, cx);
                            typography::set_code_family(typography::UiFontFamily::Geist, cx);
                            typography::set_terminal_font_size(16.0, cx);
                            settings::flush(cx);
                            assert_eq!(settings::UiSettings::load(&data), settings::current(cx));
                        })?;
                        capture(overview.into(), cx, &capture_output.join(format!("appearance-{name}-overview.png")))?;
                        capture(controls.into(), cx, &capture_output.join(format!("appearance-{name}-controls.png")))?;
                        // Reload through the same startup path used by the app.
                        cx.update(|cx| {
                            let saved = settings::UiSettings::load(&data);
                            settings::init(saved.clone(), data.clone(), cx);
                            typography::init(saved.ui_font_family, saved.ui_font_size,
                                saved.terminal_font_family, saved.terminal_font_size,
                                saved.code_font_family, saved.code_font_size,
                                typography::availability(cx).clone(), cx);
                            appearance::init(saved.appearance, saved.theme_selection, saved.accent, saved.surface, cx);
                            assert_eq!(theme::Theme::of(cx).variant_id.as_ref(), variant);
                            assert_eq!(theme::Theme::of(cx).code_font_size, 18.0);
                            assert_eq!(theme::Theme::of(cx).ui_font_size, size.pixels());
                            assert_eq!(theme::Theme::of(cx).font_mono.as_ref(), "Geist");
                            assert_eq!(theme::Theme::of(cx).terminal_font_size, 16.0);
                        });
                        cx.update(|cx| std::fs::write(capture_output.join(format!("preferences-{name}.json")), serde_json::to_vec_pretty(&settings::current(cx)).unwrap()).unwrap());
                        capture(controls.into(), cx, &capture_output.join(format!("appearance-{name}-reloaded.png")))?;
                        for code_size in [8.0, 18.0, 32.0] {
                            cx.update(|cx| typography::set_code_font_size(code_size, cx));
                            capture(tools.into(), cx, &capture_output.join(format!("tools-{name}-{code_size}px.png")))?;
                        }
                    }
                    std::fs::write(
                        capture_output.join("result.txt"),
                        "PASS: Production Appearance page, default light/dark and Catppuccin light/dark; opaque/frosted; interface families and 12/20px sizes; code 18px and terminal 16px; immediate disk readback and startup reload. No user profile or provider data was used.\n",
                    )?;
                    Ok(())
                })();
                if let Err(error) = result {
                    eprintln!("appearance fixture failed: {error:#}");
                    std::process::exit(1);
                }
                let _ = overview.update(cx, |_, window, _| window.remove_window());
                let _ = controls.update(cx, |_, window, _| window.remove_window());
                let _ = tools.update(cx, |_, window, _| window.remove_window());
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    Ok(())
}
