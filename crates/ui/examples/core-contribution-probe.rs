//! CORE-00 only: data -> native GPUI composition and unavailable renderer.
//! Not integrated into Shell, and not a public plugin API.
//! Pre-code failure/ownership gate: docs/pending-tasks/extensible-core/core-00/failures.md.
use anyhow::{Result, ensure};
use gpui::{
    AppContext, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb,
    size,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{cell::RefCell, io::Read, path::PathBuf, rc::Rc, time::Instant};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contribution {
    schema: u32,
    id: String,
    label: String,
    kind: String,
}

fn bounded_read(path: &PathBuf) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "declaration too large");
    Ok(bytes)
}

fn load(path: &PathBuf) -> Result<Contribution> {
    let bytes = bounded_read(path)?;
    let value: Contribution = serde_json::from_slice(&bytes)?;
    ensure!(
        value.schema == 1 && value.kind == "sidebar-item",
        "unsupported declaration"
    );
    ensure!(
        value.id.contains('/') && value.id.len() <= 128 && value.label.len() <= 120,
        "invalid declaration"
    );
    Ok(value)
}

struct Host {
    selected_task: String,
    draft: String,
    fallback: String,
    contribution: Option<Contribution>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .bg(rgb(0x17191d))
            .text_color(rgb(0xe8e9ed))
            .text_size(px(16.))
            .font_family("Helvetica")
            .child(
                div()
                    .w(px(220.))
                    .h_full()
                    .p(px(20.))
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .bg(rgb(0x22252b))
                    .child("LUNA host")
                    .child("Chats")
                    .children(
                        self.contribution.iter().map(|entry| {
                            div().text_color(rgb(0x87baff)).child(entry.label.clone())
                        }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .p(px(28.))
                    .flex()
                    .flex_col()
                    .gap(px(24.))
                    .child(format!("Selected: {}", self.selected_task))
                    .child(self.fallback.clone())
                    .child(
                        div()
                            .mt(px(100.))
                            .p(px(16.))
                            .border_1()
                            .border_color(rgb(0x565b67))
                            .child(self.draft.clone()),
                    ),
            )
    }
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<()> {
    anyhow::bail!("CORE-00 native probe currently requires macOS")
}

#[cfg(target_os = "macos")]
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 4,
        "usage: core-contribution-probe DECLARATION DURABLE_CONTENT OUTPUT_DIR"
    );
    let declaration = PathBuf::from(&args[1]);
    let durable = PathBuf::from(&args[2]);
    let output = PathBuf::from(&args[3]);
    std::fs::create_dir(&output)?;
    let original = bounded_read(&durable)?;
    let content: serde_json::Value = serde_json::from_slice(&original)?;
    ensure!(content["schema"] == 1, "unsupported content envelope");
    // Only this host-approved field crosses the renderer boundary. Opaque payload
    // is never sent to the view. This is not yet a production asset permission policy.
    let fallback = content["fallback"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing safe fallback"))?
        .to_owned();
    ensure!(fallback.len() <= 4096, "fallback too large");
    let native = gpui_platform::current_platform(true);
    let platform = gpui::bench_platform(
        Some(Box::new(gpui_platform::current_headless_renderer)),
        native.text_system(),
    );
    let executor = platform.background_executor();
    let handles = Rc::new(RefCell::new(None));
    let capture = handles.clone();
    let app = gpui::Application::with_platform(platform).run_embedded(move |cx| {
        let host = cx.new(|_| Host {
            selected_task: "task-before-plugin".into(),
            draft: "Unsent host draft".into(),
            fallback,
            contribution: None,
        });
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(0.), px(0.)),
                        size(px(900.), px(480.)),
                    ))),
                    ..Default::default()
                },
                |_, _| host.clone(),
            )
            .unwrap();
        *capture.borrow_mut() = Some((host, window));
    });
    let (host, window) = handles.borrow_mut().take().unwrap();
    let dispatcher = executor.dispatcher().as_bench().unwrap();
    let frame = || {
        app.update(|cx| {
            window
                .update(cx, |_, window, cx| window.simulate_next_frame(cx))
                .unwrap()
        });
        dispatcher.run_until_idle();
        app.update(|cx| {
            window
                .update(cx, |_, window, _| window.present_if_needed())
                .unwrap()
        });
    };
    for _ in 0..10 {
        frame();
    }
    let mut samples = Vec::new();
    let mut images = Vec::new();
    for index in 0..43 {
        // Read a separately supplied declaration only AFTER the host starts.
        let contribution = if index % 2 == 1 {
            Some(load(&declaration)?)
        } else {
            None
        };
        let started = Instant::now();
        app.update(|cx| {
            host.update(cx, |host, cx| {
                host.contribution = contribution;
                cx.notify();
            })
        });
        frame();
        samples.push(json!({"index":index,"enabled":index % 2 == 1,"update_to_native_draw_ms":started.elapsed().as_secs_f64()*1000.}));
        app.update(|cx| {
            let host = host.read(cx);
            assert_eq!(host.selected_task, "task-before-plugin");
            assert_eq!(host.draft, "Unsent host draft");
        });
        if index < 3 {
            let path = output.join(format!("phase-{index}.png"));
            app.update(|cx| {
                window
                    .update(cx, |_, window, _| {
                        window.render_to_image().unwrap().save(&path).unwrap()
                    })
                    .unwrap()
            });
            images.push(std::fs::read(path)?);
        }
    }
    ensure!(
        images[0] != images[1],
        "dynamic declaration had no rendered effect"
    );
    ensure!(
        images[0] == images[2],
        "removal did not restore host rendering"
    );
    ensure!(
        std::fs::read(&durable)? == original,
        "opaque durable payload changed"
    );
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&json!({
            "passed":true,"boundary":"GPUI native CoreText/Metal offscreen; prototype composition, not production Shell",
            "durable_sha256":format!("{:x}",Sha256::digest(&original)),
            "declaration_sha256":format!("{:x}",Sha256::digest(std::fs::read(declaration)?)),
            "selection":"task-before-plugin","draft":"Unsent host draft","payload_sent_to_view":false,
            "images":["phase-0.png","phase-1.png","phase-2.png"],"samples":samples
        }))?,
    )?;
    Ok(())
}
