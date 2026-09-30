//! Synthetic native GPUI editor/composer E2E; coordinator owns GUI execution.
use gpui::{AppContext, Bounds, Context, Entity, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px, size};
use luna_ui::*;
use std::path::PathBuf;

struct Root { composer: Entity<composer::Composer> }
impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
        let theme = theme::Theme::of(cx);
        div().size_full().bg(theme.bg).text_color(theme.text).font_family(theme.font_sans.clone())
            .p(px(24.0)).child(self.composer.clone())
    }
}
fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).expect("preserved evidence directory"));
    std::fs::create_dir_all(&output)?;
    let profile = tempfile::tempdir()?;
    let data = profile.path().join("ui");
    std::fs::create_dir_all(&data)?;
    gpui_platform::headless().with_assets(icons::Assets).run(move |cx| {
        gpui_tokio::init(cx);
        gpui_base::init(cx);
        let settings = settings::UiSettings::default();
        settings::init(settings.clone(), data.clone(), cx);
        let fonts = typography::register_fonts(cx);
        typography::init(settings.ui_font_family, settings.ui_font_size, settings.terminal_font_family,
            settings.terminal_font_size, settings.code_font_family, settings.code_font_size, fonts, cx);
        theme_library::init(data, cx);
        appearance::init(appearance::AppearanceMode::Dark, settings.theme_selection, settings.surface, cx);
        let state = cx.new(|_| state::AppState::new());
        let composer = cx.new(|cx| composer::Composer::new(state, cx));
        let view = composer.clone();
        let window = cx.open_window(WindowOptions {
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(gpui::point(px(20.0), px(20.0)), size(px(950.0), px(650.0)))) ,
            show: false, focus: false, ..Default::default()
        }, |_, cx| cx.new(|_| Root { composer: view })).unwrap();
        let result = window.update(cx, |_, window, cx| -> anyhow::Result<()> {
            let evidence = composer::run_integrations_picker_e2e(composer, window, cx)?;
            std::fs::write(output.join("editor-composer.json"), serde_json::to_vec_pretty(&evidence)?)?;
            window.draw(cx).clear();
            window.render_to_image()?.save(output.join("editor-composer.png"))?;
            Ok(())
        }).unwrap();
        if let Err(error) = result {
            std::fs::write(output.join("editor-composer-failure.txt"), error.to_string()).ok();
            std::process::exit(1);
        }
        cx.quit();
    });
    Ok(())
}
