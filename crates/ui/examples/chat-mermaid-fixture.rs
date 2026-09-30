//! Isolated native GPUI transcript/adapter E2E, with PNG + cache readback.
//! Failure scenarios were recorded before implementation in the wave evidence.
//! Synthetic document inputs; this is not a real-provider or full Shell test.
//! No engine, provider home, network calls, worktrees, or user data are used.
use gpui::{
    AppContext, AsyncApp, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_ui::{
    appearance, icons, settings, state::AppState, theme, theme_library, transcript::Transcript,
    typography,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

const CLOSED: &str = "Before the diagram.\n\n```mermaid\nflowchart LR\n    Input --> Render --> Chat\n```\n\nAfter the diagram, **bold** and [link](https://example.com).\n\n```rust\nfn main() { println!(\"ordinary code\"); }\n```\n\n![existing image fallback](relative.png)";

struct Root {
    transcript: Entity<Transcript>,
}
impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
        let theme = theme::Theme::of(cx);
        div()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .child(self.transcript.clone())
    }
}
fn entry(text: &str, streaming: bool) -> SessionMessageEntry {
    SessionMessageEntry {
        id: "mermaid-answer".into(),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Text {
            id: "text".into(),
            text: text.into(),
        }],
        created_at: 1_798_752_000_000,
        device_id: "mermaid-fixture".into(),
        status: Some(if streaming {
            MessageStatus::Streaming
        } else {
            MessageStatus::Complete
        }),
        continuation_of: None,
    }
}
async fn capture(
    window: gpui::AnyWindowHandle,
    transcript: &Entity<Transcript>,
    cx: &mut AsyncApp,
    output: &std::path::Path,
    label: &str,
    allowed: usize,
    rendered: Option<usize>,
    errors: usize,
) -> anyhow::Result<serde_json::Value> {
    let mut readback = serde_json::Value::Null;
    for _ in 0..200 {
        window.update(cx, |_, window, cx| {
            window.draw(cx).clear();
        })?;
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
        readback = cx.update(|cx| transcript.read(cx).fixture_mermaid_readback());
        if readback["allowed"] == allowed
            && readback["pending"] == false
            && rendered.is_none_or(|n| readback["rendered"] == n)
            && readback["errors"]
                .as_array()
                .is_some_and(|e| e.len() == errors)
        {
            break;
        }
    }
    std::fs::write(
        output.join(format!("{label}.json")),
        serde_json::to_vec_pretty(&readback)?,
    )?;
    window.update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window
            .render_to_image()?
            .save(output.join(format!("{label}.png")))?;
        Ok::<(), anyhow::Error>(())
    })??;
    anyhow::ensure!(
        readback["allowed"] == allowed && readback["pending"] == false,
        "{label}: admission/pending mismatch: {readback}"
    );
    anyhow::ensure!(
        rendered.is_none_or(|n| readback["rendered"] == n)
            && readback["errors"]
                .as_array()
                .is_some_and(|e| e.len() == errors),
        "{label}: renderer mismatch: {readback}"
    );
    anyhow::ensure!(
        readback["bytes"].as_u64().unwrap() <= 64 * 1024 * 1024,
        "{label}: exceeded media budget: {readback}"
    );
    Ok(readback)
}
fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).expect("evidence directory"));
    let reopen = std::env::args().nth(2).as_deref() == Some("reopen");
    std::fs::create_dir_all(&output)?;
    let profile = tempfile::tempdir()?;
    let data = profile.path().join("ui");
    std::fs::create_dir_all(&data)?;
    let failure = Arc::new(Mutex::new(None));
    let failure_for_ui = failure.clone();
    gpui_platform::headless().with_assets(icons::Assets).run(move |cx| {
        gpui_tokio::init(cx);
        gpui_base::init(cx);
        let settings = settings::UiSettings::default();
        settings::init(settings.clone(), data.clone(), cx);
        let fonts = typography::register_fonts(cx);
        typography::init(settings.ui_font_family, settings.ui_font_size,
            settings.terminal_font_family, settings.terminal_font_size,
            settings.code_font_family, settings.code_font_size, fonts, cx);
        theme_library::init(data, cx);
        appearance::init(appearance::AppearanceMode::Light, settings.theme_selection,
            settings.accent, settings.surface, cx);
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.selected_chat = Some("mermaid-fixture".into());
            state
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let root_transcript = transcript.clone();
        let window: gpui::AnyWindowHandle = cx.open_window(WindowOptions {
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                gpui::point(px(20.0), px(20.0)), size(px(950.0), px(1400.0))))),
            show: false, focus: false, ..Default::default()
        }, |_, cx| cx.new(|_| Root { transcript: root_transcript })).unwrap().into();
        cx.spawn(async move |cx| {
            let result: anyhow::Result<()> = async {
                if reopen {
                    let entries: Vec<SessionMessageEntry> = serde_json::from_slice(
                        &std::fs::read(output.join("persisted-document.json"))?)?;
                    cx.update(|cx| state.update(cx, |state, cx| {
                        state.apply_transcript(entries); cx.notify();
                    }));
                    capture(window, &transcript, cx, &output, "reopened-light", 1, Some(1), 0).await?;
                } else {
                    let stages = [
                        ("streaming-open", "```mermaid\nflowchart LR\n    Input --> Render", true, 0, 0),
                        ("streaming-closed", CLOSED, true, 1, 1),
                        ("complete-light", CLOSED, false, 1, 1),
                        ("long-fence-open", "````mermaid\r\nflowchart LR\r\n A --> B\r\n```", true, 0, 0),
                        ("long-fence-closed", "````mermaid\r\nflowchart LR\r\n A --> B\r\n````", true, 1, 1),
                    ];
                    for (label, source, streaming, allowed, rendered) in stages {
                        cx.update(|cx| state.update(cx, |state, cx| {
                            state.apply_transcript(vec![entry(source, streaming)]); cx.notify();
                        }));
                        capture(window, &transcript, cx, &output, label, allowed, Some(rendered), 0).await?;
                    }
                    cx.update(|cx| state.update(cx, |state, cx| {
                        state.apply_transcript(vec![entry(CLOSED, false)]); cx.notify();
                    }));
                    let light = capture(window, &transcript, cx, &output, "theme-light", 1, Some(1), 0).await?;
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Dark, cx));
                    let dark = capture(window, &transcript, cx, &output, "theme-dark", 1, Some(1), 0).await?;
                    anyhow::ensure!(dark["style"] != light["style"] && dark["epoch"] != light["epoch"],
                        "appearance did not invalidate the old cache");
                    cx.update(|cx| appearance::set_mode(appearance::AppearanceMode::Light, cx));
                    let nested = "> ```mermaid\n> flowchart LR\n>  Quote --> Diagram\n> ```\n\n- Nested list\n\n  ```MERMAID\n  flowchart LR\n   List --> Diagram\n  ```";
                    cx.update(|cx| state.update(cx, |state, cx| {
                        state.apply_transcript(vec![entry(nested, true)]); cx.notify();
                    }));
                    capture(window, &transcript, cx, &output, "nested-streaming", 2, Some(2), 0).await?;
                    cx.update(|cx| state.update(cx, |state, cx| {
                        state.apply_transcript(vec![entry("```mermaid\nthis is not a diagram\n```", false)]); cx.notify();
                    }));
                    capture(window, &transcript, cx, &output, "malformed-source", 1, Some(0), 1).await?;
                    let oversized = format!("```mermaid\n{}\n```", "x".repeat(16 * 1024 + 1));
                    cx.update(|cx| state.update(cx, |state, cx| {
                        state.apply_transcript(vec![entry(&oversized, false)]); cx.notify();
                    }));
                    capture(window, &transcript, cx, &output, "source-limit", 0, Some(0), 0).await?;
                    let many: String = (0..33).map(|ix| format!("```mermaid\nflowchart LR\n A{ix} --> B{ix}\n```\n\n")).collect();
                    cx.update(|cx| state.update(cx, |state, cx| {
                        state.apply_transcript(vec![entry(&many, false)]); cx.notify();
                    }));
                    let limited = capture(window, &transcript, cx, &output, "count-limit", 32, None, 0).await?;
                    anyhow::ensure!(limited["limitReached"] == true, "33rd diagram was not limited");
                    // Start a render, then attach another chat before waiting on it.
                    cx.update(|cx| state.update(cx, |state, cx| {
                        state.apply_transcript(vec![entry(CLOSED, false)]); cx.notify();
                    }));
                    window.update(cx, |_, window, cx| { window.draw(cx).clear(); })?;
                    cx.update(|cx| state.update(cx, |state, cx| {
                        state.selected_chat = Some("other-fixture-chat".into());
                        state.apply_transcript(vec![entry("Other chat has no diagram.", false)]); cx.notify();
                    }));
                    capture(window, &transcript, cx, &output, "chat-switch", 0, Some(0), 0).await?;
                    std::fs::write(output.join("persisted-document.json"),
                        serde_json::to_vec_pretty(&vec![entry(CLOSED, false)])?)?;
                }
                std::fs::write(output.join(if reopen { "reopen.pass" } else { "fixture.pass" }),
                    "PASS: native GPUI transcript + Mermaid adapter fixture; synthetic inputs, no provider proof\n")?;
                Ok(())
            }.await;
            if let Err(error) = result {
                eprintln!("Chat Mermaid fixture failed: {error:#}");
                *failure_for_ui.lock().unwrap() = Some(error.to_string());
            }
            let _ = window.update(cx, |_, window, _| window.remove_window());
            cx.update(|cx| cx.quit());
        }).detach();
    });
    if let Some(error) = failure.lock().unwrap().take() {
        anyhow::bail!(error);
    }
    Ok(())
}
