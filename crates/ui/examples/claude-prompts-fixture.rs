//! Headless GPUI fixture for the CC-03 Claude permission/dialog prompts.
//!
//! Renders the production transcript working trailer and composer question
//! panel in a hidden window, seeded with the questions the real Claude adapter
//! produced in the harness E2E (`LUNA_CC03_EVIDENCE_DIR`). It never
//! starts an engine, contacts a provider, touches LUNA Dev or its data, or
//! opens a visible window.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example claude-prompts-fixture
//! Run:   target/debug/examples/claude-prompts-fixture ABSOLUTE_EVIDENCE_DIR ADAPTER_ARTIFACT_DIR
//! Writes seven PNG captures, claude-prompts.json and a pass receipt.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gpui::{
    AppContext, Bounds, Context, Entity, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, size,
};
use luna_ui::*;

struct Root {
    composer: Entity<composer::Composer>,
    transcript: Entity<transcript::Transcript>,
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
        let theme = theme::Theme::of(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .p(px(24.0))
            .child(div().flex_1().min_h_0().child(self.transcript.clone()))
            .child(self.composer.clone())
    }
}

fn main() -> anyhow::Result<()> {
    let usage = || {
        anyhow::anyhow!("usage: claude-prompts-fixture ABSOLUTE_EVIDENCE_DIR ADAPTER_ARTIFACT_DIR")
    };
    let output = PathBuf::from(std::env::args().nth(1).ok_or_else(usage)?);
    let artifacts = PathBuf::from(std::env::args().nth(2).ok_or_else(usage)?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["claude-prompts.json", "claude-prompts.pass", "failure.txt"] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let profile = tempfile::tempdir()?;
    let data = profile.path().join("ui");
    std::fs::create_dir_all(&data)?;
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
            let transcript = cx.new(|cx| transcript::Transcript::new(state.clone(), cx));
            let view = composer.clone();
            let window: gpui::AnyWindowHandle = match cx.open_window(
                WindowOptions {
                    window_background: gpui::WindowBackgroundAppearance::Opaque,
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(20.0), px(20.0)),
                        size(px(900.0), px(760.0)),
                    ))),
                    show: false,
                    focus: false,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Root { composer: view, transcript }),
            ) {
                Ok(window) => window.into(),
                Err(error) => {
                    eprintln!("Claude prompts fixture failed: open window: {error:#}");
                    let _ = std::fs::write(output.join("failure.txt"), format!("open window: {error:#}\n"));
                    drop(profile);
                    // AppKit termination from inside `run` exits 0; fail explicitly.
                    std::process::exit(1);
                }
            };
            cx.spawn(async move |cx| {
                let result: anyhow::Result<()> = async {
                    let evidence = composer::run_claude_prompts_e2e(
                        composer, state, window, &output, &artifacts, cx,
                    )
                    .await?;
                    std::fs::write(
                        output.join("claude-prompts.json"),
                        serde_json::to_vec_pretty(&evidence)?,
                    )?;
                    std::fs::write(
                        output.join("claude-prompts.pass"),
                        "PASS: headless GPUI CC-03 Claude prompts; questions from the Claude adapter E2E artifacts, no provider proof\n",
                    )?;
                    Ok(())
                }
                .await;
                let _ = window.update(cx, |_, window, _| window.remove_window());
                if let Err(error) = result {
                    eprintln!("Claude prompts fixture failed: {error:#}");
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
