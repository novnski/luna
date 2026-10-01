//! Self-contained headless GPUI fixture for CC-04 queued image steering.
//!
//! Renders the production composer and queue panel for a live Claude task with
//! a held image row whose intent is Steer, once per provider (Claude Code,
//! Codex, OpenCode), and reads the production primary-action resolution. It
//! never starts an engine, contacts a provider, touches LUNA Dev or its data
//! directory, or opens a visible window.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example queue-image-steer-fixture
//! Run:   target/debug/examples/queue-image-steer-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes queue-steer.json, queue-<provider>.png and a pass receipt.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gpui::{
    AppContext, Bounds, Context, Entity, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, size,
};
use luna_ui::*;

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xB5, 0x1C, 0x0C,
    0x02, 0x00, 0x00, 0x00, 0x0B, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xFC, 0xFF, 0x1F, 0x00,
    0x03, 0x03, 0x02, 0x00, 0xEF, 0xA3, 0x45, 0x99, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
    0xAE, 0x42, 0x60, 0x82,
];

struct Root {
    composer: Entity<composer::Composer>,
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
        let theme = theme::Theme::of(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .p(px(24.0))
            .child(self.composer.clone())
    }
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
        anyhow::anyhow!("usage: queue-image-steer-fixture ABSOLUTE_EVIDENCE_DIR")
    })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["queue-steer.json", "queue-image-steer.pass", "failure.txt"] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let profile = tempfile::tempdir()?;
    let data = profile.path().join("ui");
    std::fs::create_dir_all(&data)?;
    // A real PNG on disk for the queued row's attachment chip.
    let image = profile.path().join("screenshot.png");
    std::fs::write(&image, PNG_1X1)?;
    let image = image.to_string_lossy().into_owned();
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
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
            theme_library::init(data, cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                settings.theme_selection,
                settings.accent,
                settings.surface,
                cx,
            );
            composer::init(cx, settings.composer_send_behavior);
            let state = cx.new(|_| state::AppState::new());
            let composer = cx.new(|cx| composer::Composer::new(state.clone(), cx));
            let view = composer.clone();
            let window: gpui::AnyWindowHandle = match cx.open_window(
                WindowOptions {
                    window_background: gpui::WindowBackgroundAppearance::Opaque,
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(20.0), px(20.0)),
                        size(px(950.0), px(650.0)),
                    ))),
                    show: false,
                    focus: false,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Root { composer: view }),
            ) {
                Ok(window) => window.into(),
                Err(error) => {
                    eprintln!("Queue image steer fixture failed: open window: {error:#}");
                    let _ = std::fs::write(output.join("failure.txt"), format!("open window: {error:#}\n"));
                    drop(profile);
                    // AppKit termination from inside `run` exits 0; fail explicitly.
                    std::process::exit(1);
                }
            };
            cx.spawn(async move |cx| {
                let result: anyhow::Result<()> = async {
                    let evidence = queue::run_queue_image_steer_e2e(
                        composer, state, window, &image, &output, cx,
                    )
                    .await?;
                    std::fs::write(
                        output.join("queue-steer.json"),
                        serde_json::to_vec_pretty(&evidence)?,
                    )?;
                    std::fs::write(
                        output.join("queue-image-steer.pass"),
                        "PASS: headless GPUI queue resolver + panel; synthetic harness catalog, no engine or provider proof\n",
                    )?;
                    Ok(())
                }
                .await;
                let _ = window.update(cx, |_, window, _| window.remove_window());
                if let Err(error) = result {
                    eprintln!("Queue image steer fixture failed: {error:#}");
                    let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                    *failure_for_ui.lock().unwrap() = Some(error.to_string());
                    drop(profile);
                    // AppKit termination from inside `run` exits 0; fail explicitly.
                    std::process::exit(1);
                }
                drop(profile);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    if let Some(error) = failure.lock().unwrap().take() {
        anyhow::bail!(error);
    }
    Ok(())
}
