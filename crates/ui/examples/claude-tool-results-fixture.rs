//! Headless GPUI rendering of Claude tool results fetched from the host journal.
//!
//! Input: the evidence directory written by the engine E2E
//! `crates/engine/tests/claude_tool_results.rs` (`LUNA_CC05_EVIDENCE`): the
//! exact `FetchToolBlob` texts (envelopes and diffs) after an engine restart.
//! Each one goes through the production lazy-fetch detail builder and detail
//! renderer. Long outputs are shown as consecutive viewport-sized pages of the
//! same production rows. No engine, provider, LUNA Dev or user profile is used.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example claude-tool-results-fixture
//! Run:   target/debug/examples/claude-tool-results-fixture ABSOLUTE_E2E_EVIDENCE_DIR ABSOLUTE_OUTPUT_DIR
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    AppContext, AsyncApp, Bounds, Context, IntoElement, Render, SharedString, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use luna_ui::transcript::ToolDetail;
use luna_ui::{appearance, icons, settings, theme, theme_library, typography};

/// Rows per page: one production output viewport without clipping.
const PAGE_ROWS: usize = 11;

struct Section {
    title: SharedString,
    pages: Vec<ToolDetail>,
}

struct ResultsRoot {
    heading: SharedString,
    sections: Vec<Section>,
}

impl Render for ResultsRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
        let theme = theme::Theme::of(cx);
        let scale = typography::font_size(cx).pixels() / 16.0;
        div()
            .size_full()
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .text_size(typography::ui_rems(13.0))
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child(self.heading.clone()),
            )
            .children(self.sections.iter().map(|section| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(section.title.clone())
                    .children(
                        section.pages.iter().map(|page| {
                            luna_ui::transcript::fixture_detail_panel(page, theme, scale)
                        }),
                    )
            }))
    }
}

fn capture(window: gpui::AnyWindowHandle, cx: &mut AsyncApp, path: &Path) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok::<(), anyhow::Error>(())
    })??;
    Ok(())
}

/// Production detail for one fetched blob, plus its displayed rows.
fn detail(dir: &Path, file: &str, is_diff: bool) -> anyhow::Result<(ToolDetail, Vec<String>)> {
    let text = std::fs::read_to_string(dir.join(file))?;
    let detail = luna_ui::transcript::fixture_blob_detail(&text, is_diff)
        .ok_or_else(|| anyhow::anyhow!("{file}: production detail builder returned nothing"))?;
    let rows = match &detail {
        ToolDetail::Output { lines, .. } => lines.iter().map(|l| l.to_string()).collect(),
        ToolDetail::Diff { file, .. } => vec![format!("diff {} hunks", file.hunks.len())],
        other => anyhow::bail!("{file}: unexpected detail {other:?}"),
    };
    Ok((detail, rows))
}

fn paged(detail: ToolDetail) -> Vec<ToolDetail> {
    match detail {
        ToolDetail::Output {
            lines,
            truncated_by,
            raw_text,
        } => {
            let count = lines.chunks(PAGE_ROWS).count();
            lines
                .chunks(PAGE_ROWS)
                .enumerate()
                .map(|(index, chunk)| ToolDetail::Output {
                    lines: chunk.to_vec(),
                    truncated_by: if index + 1 == count { truncated_by } else { 0 },
                    raw_text: raw_text.clone(),
                })
                .collect()
        }
        other => vec![other],
    }
}

fn require(
    rows: &[String],
    file: &str,
    needles: &[&str],
    forbidden: &[&str],
) -> anyhow::Result<()> {
    let joined = rows.join("\n");
    for needle in needles {
        anyhow::ensure!(joined.contains(needle), "{file}: detail lacks {needle:?}");
    }
    for needle in forbidden {
        anyhow::ensure!(!joined.contains(needle), "{file}: detail shows {needle:?}");
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(args.next().expect("E2E evidence directory"));
    let output = PathBuf::from(args.next().expect("output directory"));
    anyhow::ensure!(
        input.is_absolute() && output.is_absolute(),
        "directories must be absolute"
    );
    std::fs::create_dir_all(&output)?;
    for stale in ["claude-tool-results.pass", "failure.txt", "ui-details.json"] {
        let _ = std::fs::remove_file(output.join(stale));
    }

    // (title, file, is_diff, must show, must not show)
    type Case = (
        &'static str,
        &'static str,
        bool,
        &'static [&'static str],
        &'static [&'static str],
    );
    let rich: &[Case] = &[
        (
            "mcp__fixture__rich · mixed MCP result",
            "toolu_mcp.json",
            false,
            &[
                "Completed · 3 text · 1 image · 1 resource link · structured content",
                "MCP_TEXT_MARKER",
                "Image (image/png)",
                "RESOURCE_TEXT_MARKER",
                "Resource link: report",
                "Structured content",
                "STRUCTURED_MARKER",
                "Arguments",
                "MCP_ARGUMENT_MARKER",
                "Protocol metadata",
            ],
            &["iVBORw0KGgo"],
        ),
        (
            "mcp__computer-use__screenshot · private media",
            "toolu_cu.json",
            false,
            &["Image (image/png) · media not retained"],
            &["SCREENSHOT_SECRET"],
        ),
        (
            "StructuredOutput · schema result",
            "toolu_so.json",
            false,
            &["Structured content", "\"verdict\": \"pass\""],
            &[],
        ),
    ];
    let builtins: &[Case] = &[
        (
            "Bash · stdout, stderr and complete arguments",
            "toolu_bash.json",
            false,
            &[
                "BASH_STDOUT_MARKER",
                "BASH_STDERR_MARKER",
                "Arguments",
                "BASH_DESCRIPTION_ARG",
                "Native result",
            ],
            &["lunaToolOutput"],
        ),
        (
            "Read · pagination arguments",
            "toolu_read.json",
            false,
            &[
                "READ_LINE_TEN",
                "\"offset\": 10",
                "\"limit\": 2",
                "Not retained: file.content (30 bytes)",
            ],
            &[],
        ),
        (
            "Bash · permission denied",
            "toolu_denied.json",
            false,
            &["Not run (permission-rule)", "Not executed: permission-rule"],
            &[],
        ),
        (
            "FutureTool · unknown content block",
            "toolu_future.json",
            false,
            &[
                "FUTURE_TEXT_MARKER",
                "Unsupported content block `future_widget`",
            ],
            &["FUTURE_BLOCK_MARKER"],
        ),
        (
            "mcp__fixture__noop · received empty",
            "toolu_empty.json",
            false,
            &["Completed · empty result"],
            &[],
        ),
    ];
    let diffs: &[Case] = &[
        (
            "Edit · native structuredPatch",
            "toolu_edit.diff.json",
            true,
            &["diff 1 hunks"],
            &[],
        ),
        (
            "Write · created file",
            "toolu_write.diff.json",
            true,
            &["diff 1 hunks"],
            &[],
        ),
        (
            "NotebookEdit · cell source",
            "toolu_nb.diff.json",
            true,
            &["diff 1 hunks"],
            &[],
        ),
        (
            "Subagent Bash · child-owned output",
            "child-toolu_bash.json",
            false,
            &["CHILD_TAIL", "CHILD_DESCRIPTION_ARG"],
            &["BASH_STDOUT_MARKER"],
        ),
    ];
    let mut receipt = serde_json::Map::new();
    let mut groups = Vec::new();
    for (name, cases) in [("rich", rich), ("builtins", builtins), ("diffs", diffs)] {
        let mut sections = Vec::new();
        for (title, file, is_diff, needles, forbidden) in cases {
            let (detail, rows) = detail(&input, file, *is_diff)?;
            require(&rows, file, needles, forbidden)?;
            receipt.insert((*file).into(), serde_json::json!(rows));
            sections.push(Section {
                title: (*title).into(),
                pages: paged(detail),
            });
        }
        groups.push((name, sections));
    }
    std::fs::write(
        output.join("ui-details.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;

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
                appearance::AppearanceMode::Dark,
                settings.theme_selection,
                settings.accent,
                settings.surface,
                cx,
            );
            let mut windows = Vec::new();
            for (name, sections) in groups {
                let height = 120.0
                    + sections
                        .iter()
                        .map(|s| 30.0 + s.pages.len() as f32 * 280.0)
                        .sum::<f32>()
                        .min(2400.0);
                let window = cx
                    .open_window(
                        WindowOptions {
                            window_background: gpui::WindowBackgroundAppearance::Opaque,
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                gpui::point(px(20.0), px(20.0)),
                                size(px(980.0), px(height)),
                            ))),
                            show: false,
                            focus: false,
                            ..Default::default()
                        },
                        move |_, cx| {
                            cx.new(|_| ResultsRoot {
                                heading: format!(
                                    "Claude tool results · {name} · fetched from the host journal after restart (offline 2.1.286-shaped fixture)"
                                )
                                .into(),
                                sections,
                            })
                        },
                    )
                    .unwrap();
                windows.push((name, gpui::AnyWindowHandle::from(window)));
            }
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                let result = (|| -> anyhow::Result<()> {
                    for (name, window) in &windows {
                        capture(
                            *window,
                            cx,
                            &capture_output.join(format!("claude-tool-results-{name}.png")),
                        )?;
                    }
                    std::fs::write(
                        capture_output.join("claude-tool-results.pass"),
                        "PASS: production blob_detail + detail renderer over FetchToolBlob texts from the engine E2E (offline fixture, no provider, no native app)\n",
                    )?;
                    Ok(())
                })();
                if let Err(error) = result {
                    eprintln!("claude tool results fixture failed: {error:#}");
                    let _ = std::fs::write(capture_output.join("failure.txt"), format!("{error:#}\n"));
                    std::process::exit(1);
                }
                for (_, window) in &windows {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                }
                drop(profile);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    Ok(())
}
