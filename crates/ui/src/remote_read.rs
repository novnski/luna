//! Exact-target, read-only remote task window. It consumes the same typed RPC
//! readback as the CLI and never mounts the full workspace watch/UI surface.

use gpui::{
    App, AppContext as _, Bounds, Context, Render, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use serde::{Deserialize, Serialize};

use crate::theme::Theme;

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
    first_render_reported: bool,
}

fn qa_startup_event(event: &str) {
    if std::env::var_os("LUNA_L30_QA_ROOT").is_some() {
        eprintln!("{event} pid={}", std::process::id());
    }
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
            first_render_reported: false,
        }
    }

    fn control(
        &self,
        label: &'static str,
        command: RemoteReadCommand,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement {
        let theme = Theme::of(cx);
        div()
            .id(label)
            .role(gpui::Role::Button)
            .aria_label(label)
            .px_4()
            .py_2()
            .rounded(px(Theme::CONTROL_RADIUS))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .text_color(theme.text)
            .text_size(crate::typography::ui_rems(13.0))
            .cursor_pointer()
            .hover(|style| style.bg(theme.surface_raised_hover))
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        window.set_rem_size(px(crate::typography::font_size(cx).pixels()));
        let theme = Theme::of(cx);
        if !self.first_render_reported {
            self.first_render_reported = true;
            qa_startup_event("L30_VIEW_FIRST_RENDER");
        }
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
            .id("remote-read-root")
            .role(gpui::Role::Document)
            .size_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .bg(theme.glass())
            .font_family(theme.font_sans.clone())
            .text_size(crate::typography::ui_rems(13.0))
            .text_color(theme.text)
            .child(
                div()
                    .id("remote-read-title")
                    .role(gpui::Role::Heading)
                    .aria_label("LUNA Remote task read")
                    .text_size(crate::typography::ui_rems(20.0))
                    .child("LUNA · Remote task read"),
            )
            .child(
                div()
                    .id("remote-read-description")
                    .role(gpui::Role::Label)
                    .aria_label("Read-only identity and bounded receipt page")
                    .text_color(theme.text_muted)
                    .child("Read-only identity and bounded receipt page"),
            )
            .child(
                div()
                    .id("remote-read-status")
                    .role(gpui::Role::Label)
                    .aria_label(status)
                    .text_color(theme.text_muted)
                    .child(status),
            )
            .child(
                div()
                    .id("remote-read-details")
                    .role(gpui::Role::Label)
                    .aria_label(details.clone())
                    .flex_1()
                    .child(details),
            )
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
    data_dir: std::path::PathBuf,
) {
    let app = gpui_platform::application().with_assets(crate::icons::Assets);
    app.run(move |cx: &mut App| {
        qa_startup_event("L30_VIEW_APP_RUN_ENTERED");
        gpui_tokio::init(cx);
        gpui_base::init(cx);
        crate::init_appearance(&data_dir, cx);
        let exit_on_quit = commands.clone();
        cx.on_app_quit(move |_| {
            let _ = exit_on_quit.send(RemoteReadCommand::Exit);
            async {}
        })
        .detach();
        cx.open_window(
            WindowOptions {
                window_background: Theme::of(cx).window_background_appearance(),
                titlebar: Some(TitlebarOptions {
                    title: Some("LUNA Remote Task Read".into()),
                    ..Default::default()
                }),
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
                crate::appearance::observe_window(window, cx).detach();
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
        qa_startup_event("L30_VIEW_WINDOW_OPENED");
    });
}
