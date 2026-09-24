//! Exact-target, read-only remote task window. It consumes the same typed RPC
//! readback as the CLI and never mounts the full workspace watch/UI surface.

use gpui::{
    App, AppContext as _, Bounds, Context, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskReadback {
    pub machine: String,
    pub environment: String,
    pub project: String,
    pub task: String,
    pub receipt_page_count: usize,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "camelCase")]
pub enum RemoteReadState {
    Connecting,
    Connected(RemoteTaskReadback),
    Disconnected(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteReadCommand {
    Disconnect,
    Reconnect,
    Exit,
}

struct RemoteReadWindow {
    state: RemoteReadState,
    commands: tokio::sync::mpsc::UnboundedSender<RemoteReadCommand>,
}

impl RemoteReadWindow {
    fn new(
        mut state: tokio::sync::watch::Receiver<RemoteReadState>,
        commands: tokio::sync::mpsc::UnboundedSender<RemoteReadCommand>,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = state.borrow_and_update().clone();
        cx.spawn(async move |this, cx| {
            while state.changed().await.is_ok() {
                let next = state.borrow_and_update().clone();
                if this
                    .update(cx, |this, cx| {
                        this.state = next;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            state: initial,
            commands,
        }
    }

    fn control(
        &self,
        label: &'static str,
        command: RemoteReadCommand,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement {
        div()
            .id(label)
            .px_4()
            .py_2()
            .rounded_md()
            .bg(rgb(0x26303b))
            .text_color(rgb(0xf4f6f8))
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(|style| style.border_2().border_color(rgb(0x8ab4ef)))
            .child(label)
            .on_click(cx.listener(move |this, _, _, _| {
                let _ = this.commands.send(command);
            }))
            .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                if !event.is_held && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    let _ = this.commands.send(command);
                }
            }))
    }
}

impl Render for RemoteReadWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let (status, details) = match &self.state {
            RemoteReadState::Connecting => (
                "Connecting",
                "Opening authenticated SSH transport…".to_owned(),
            ),
            RemoteReadState::Disconnected(reason) => ("Disconnected", reason.clone()),
            RemoteReadState::Connected(readback) => (
                "Connected",
                format!(
                    "Machine: {}\nEnvironment: {}\nProject: {}\nTask: {}\nReceipts in first page: {}\n{}",
                    readback.machine,
                    readback.environment,
                    readback.project,
                    readback.task,
                    readback.receipt_page_count,
                    readback.summary,
                ),
            ),
        };
        div()
            .size_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .bg(rgb(0x11151b))
            .text_color(rgb(0xf4f6f8))
            .child(div().text_size(px(20.)).child("LUNA · Remote task read"))
            .child(
                div()
                    .text_color(rgb(0x9fb3c8))
                    .child("Read-only identity and bounded receipt page"),
            )
            .child(div().text_color(rgb(0x9fb3c8)).child(status))
            .child(div().flex_1().child(details))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(self.control("Disconnect", RemoteReadCommand::Disconnect, cx))
                    .child(self.control("Reconnect", RemoteReadCommand::Reconnect, cx)),
            )
    }
}

/// Show the read-only task view. The transport worker owns SSH and publishes
/// truthful connection state; closing the window requests worker exit.
pub fn run_remote_read_view(
    state: tokio::sync::watch::Receiver<RemoteReadState>,
    commands: tokio::sync::mpsc::UnboundedSender<RemoteReadCommand>,
) {
    let app = gpui_platform::application().with_assets(crate::icons::Assets);
    app.run(move |cx: &mut App| {
        gpui_tokio::init(cx);
        let exit_on_quit = commands.clone();
        cx.on_app_quit(move |_| {
            let _ = exit_on_quit.send(RemoteReadCommand::Exit);
            async {}
        })
        .detach();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(700.), px(420.)),
                    cx,
                ))),
                window_min_size: Some(size(px(540.), px(320.))),
                app_id: Some("luna-remote-read".into()),
                ..Default::default()
            },
            |window, cx| {
                let exit_on_close = commands.clone();
                window.on_window_should_close(cx, move |_, cx| {
                    let _ = exit_on_close.send(RemoteReadCommand::Exit);
                    cx.quit();
                    true
                });
                cx.new(|cx| RemoteReadWindow::new(state, commands, cx))
            },
        )
        .expect("open LUNA remote read window");
    });
}
