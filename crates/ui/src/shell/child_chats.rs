//! Child chats in the right pane.
//!
//! Persistent child chats (`Chat.child_thread`) belong to the conversation
//! that started them. Instead of an indented sidebar tree, the parent's right
//! pane lists them ([`RightSurface::ChildChats`]) and drills into one child's
//! live transcript with a reply composer, then back to the list: the Side
//! chats interaction over full persistent chats.
//!
//! One open child holds exactly one `WatchDocMessages` subscription
//! (`AppState::watch_subagent_doc`, keyed by the child's chat id). Back,
//! closing the tab or pane, switching chats and engine replacement release it.

use super::*;
use crate::markdown::render::LinkUi;
use crate::state::EngineHandle;
use chrono::DateTime;
use luna_proto::{Chat, ChatIndicator, CommandReceiptStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildChatStatus {
    NeedsApproval,
    NeedsInput,
    Running,
    Failed,
    Interrupted,
    Completed,
    Idle,
    Archived,
}

impl ChildChatStatus {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::NeedsApproval => "Needs approval",
            Self::NeedsInput => "Needs input",
            Self::Running => "Running",
            Self::Failed => "Failed",
            Self::Interrupted => "Interrupted",
            Self::Completed => "Completed",
            Self::Idle => "Idle",
            Self::Archived => "Archived",
        }
    }

    /// List order: attention, then live work, then everything else by
    /// recency; archived rows last.
    fn rank(self) -> u8 {
        match self {
            Self::NeedsApproval | Self::NeedsInput => 0,
            Self::Running => 1,
            Self::Archived => 3,
            _ => 2,
        }
    }

    /// The sidebar's status palette, so a child reads the same in both places.
    fn color(self, theme: &Theme) -> gpui::Hsla {
        let indicator = match self {
            Self::NeedsApproval => ChatIndicator::AwaitingApproval,
            Self::NeedsInput => ChatIndicator::AwaitingInput,
            Self::Running => ChatIndicator::Working,
            Self::Failed => ChatIndicator::Errored,
            Self::Completed => ChatIndicator::Completed,
            Self::Interrupted | Self::Idle | Self::Archived => {
                return theme.readable_muted(0.55);
            }
        };
        let color = spaces::status_dot_color(indicator, theme);
        // The sidebar dims status words; a list row is roomier and keeps
        // them legible.
        gpui::Hsla {
            a: color.a.max(0.85),
            ..color
        }
    }
}

pub(super) struct ChildChatRow {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) status: ChildChatStatus,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) detail: Option<String>,
    pub(super) model: Option<String>,
    pub(super) children: usize,
}

fn is_child_of(chat: &Chat, parent: &str) -> bool {
    chat.id != parent
        && chat
            .child_thread
            .as_ref()
            .is_some_and(|child| child.parent_task_id == parent)
}

/// Non-archived direct child chats per parent id (sidebar chip, picker badge).
pub(super) fn direct_child_counts(chats: &[Chat]) -> std::collections::HashMap<String, usize> {
    let mut counts = std::collections::HashMap::new();
    for chat in chats.iter().filter(|chat| !chat.archived) {
        if let Some(child) = &chat.child_thread
            && child.parent_task_id != chat.id
        {
            *counts.entry(child.parent_task_id.clone()).or_default() += 1;
        }
    }
    counts
}

/// Live session state wins; otherwise the newest recorded child outcome, then
/// the seen marker. Outcomes are compacted after delivery, so a reported child
/// can read Idle.
pub(super) fn child_chat_status(
    state: &AppState,
    chat: &Chat,
    now: DateTime<Utc>,
) -> ChildChatStatus {
    if chat.archived {
        return ChildChatStatus::Archived;
    }
    if state.send_pending(&chat.id, now) {
        return ChildChatStatus::Running;
    }
    match luna_proto::view::effective_indicator(state.session_for(&chat.id), now) {
        Indicator::Working => return ChildChatStatus::Running,
        Indicator::AwaitingApproval => return ChildChatStatus::NeedsApproval,
        Indicator::AwaitingInput => return ChildChatStatus::NeedsInput,
        Indicator::Errored => return ChildChatStatus::Failed,
        Indicator::None => {}
    }
    let latest = chat.child_thread.as_ref().and_then(|child| {
        child
            .outcomes
            .iter()
            .max_by_key(|outcome| outcome.created_at_ms)
    });
    match latest.map(|outcome| outcome.outcome.as_str()) {
        Some("failed" | "errored") => ChildChatStatus::Failed,
        Some("interrupted") => ChildChatStatus::Interrupted,
        Some("turn-completed") => ChildChatStatus::Completed,
        _ if chat.unseen() => ChildChatStatus::Completed,
        _ => ChildChatStatus::Idle,
    }
}

fn first_line(text: &str) -> Option<String> {
    let line = transcript::single_line(text);
    let line = line.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// This parent's direct children: attention and running first, then the rest
/// by recency, archived last.
pub(super) fn child_chat_rows(
    state: &AppState,
    parent: &str,
    now: DateTime<Utc>,
) -> Vec<ChildChatRow> {
    let counts = direct_child_counts(&state.chats);
    let mut rows: Vec<ChildChatRow> = state
        .chats
        .iter()
        .filter(|chat| is_child_of(chat, parent))
        .map(|chat| {
            let child = chat.child_thread.as_ref();
            let detail = child
                .and_then(|child| {
                    child
                        .outcomes
                        .iter()
                        .max_by_key(|outcome| outcome.created_at_ms)
                        .and_then(|outcome| first_line(&outcome.summary))
                        .or_else(|| first_line(&child.initial_prompt))
                })
                .filter(|line| Some(line.as_str()) != chat.title.as_deref());
            let model = chat.config.as_ref().map(|config| {
                let provider = match config.harness {
                    HarnessId::Codex => "Codex",
                    HarnessId::ClaudeCode => "Claude Code",
                    HarnessId::Opencode => "OpenCode",
                    HarnessId::Mock => "Mock",
                    _ => "Provider",
                };
                match config.model.as_deref().filter(|model| !model.is_empty()) {
                    Some(model) => format!("{provider} · {model}"),
                    None => provider.to_string(),
                }
            });
            ChildChatRow {
                id: chat.id.clone(),
                title: transcript::single_line(chat.title.as_deref().unwrap_or("New child chat")),
                status: child_chat_status(state, chat, now),
                updated_at: chat.last_message_at.unwrap_or(chat.created_at),
                detail,
                model,
                children: counts.get(&chat.id).copied().unwrap_or(0),
            }
        })
        .collect();
    rows.sort_by(|left, right| {
        left.status
            .rank()
            .cmp(&right.status.rank())
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    rows
}

/// Engine, host device and live-run flag for a reply, or why it cannot be
/// sent (`None` = silently ignored, e.g. archived).
fn prepare_reply(
    state: &AppState,
    id: &str,
    now: DateTime<Utc>,
) -> Result<(EngineHandle, String, bool), Option<&'static str>> {
    let chat = state
        .chats
        .iter()
        .find(|chat| chat.id == id)
        .ok_or(Some("This child chat is no longer available."))?;
    if chat.archived {
        return Err(None);
    }
    let engine = state
        .engine()
        .cloned()
        .ok_or(Some("Engine not connected"))?;
    let capability = luna_proto::capabilities::MESSAGE_QUEUE_V1;
    if !engine.engine_info().supports(capability) || !state.chat_host_supports(id, capability) {
        return Err(Some(
            "Update this child chat's engine to reply from here, or open it as the main chat.",
        ));
    }
    let running = child_chat_status(state, chat, now) == ChildChatStatus::Running;
    Ok((engine, chat.device_id.clone(), running))
}

pub(super) enum ChildChatEvent {
    /// Promote this child to the main conversation view.
    OpenAsMain(String),
}

impl gpui::EventEmitter<ChildChatEvent> for ChildChatPanel {}

struct OpenChild {
    id: String,
    transcript: Entity<Transcript>,
    input: Entity<ComposerInput>,
    _input_events: Subscription,
    sending: bool,
    /// `(is_error, text)` for the composer's status line.
    notice: Option<(bool, String)>,
    focus_pending: bool,
    _send: Option<Task<()>>,
}

pub(super) struct ChildChatPanel {
    state: Entity<AppState>,
    parent: String,
    engine_generation: u64,
    links: LinkUi,
    open: Option<OpenChild>,
    focus: FocusHandle,
    /// After Back the open view's controls are gone; park focus on the
    /// panel so Tab continues into the list.
    focus_root_pending: bool,
    _observe: Subscription,
}

impl ChildChatPanel {
    pub(super) fn new(
        state: Entity<AppState>,
        parent: String,
        links: LinkUi,
        cx: &mut Context<Self>,
    ) -> Self {
        let engine_generation = state.read(cx).engine_generation();
        // Rows, statuses and the open transcript all derive from AppState.
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        cx.on_release(|panel, cx| {
            if let Some(open) = panel.open.take() {
                panel
                    .state
                    .update(cx, |state, _| state.unwatch_subagent_doc(&open.id));
            }
        })
        .detach();
        Self {
            state,
            parent,
            engine_generation,
            links,
            open: None,
            focus: cx.focus_handle(),
            focus_root_pending: false,
            _observe: observe,
        }
    }

    pub(super) fn parent(&self) -> &str {
        &self.parent
    }

    pub(super) fn open_child_id(&self) -> Option<&str> {
        self.open.as_ref().map(|open| open.id.as_str())
    }

    /// A replaced engine invalidates the watch and any in-flight send.
    pub(super) fn is_current(&self, cx: &App) -> bool {
        self.state.read(cx).engine_generation() == self.engine_generation
    }

    pub(super) fn open_child(&mut self, id: String, cx: &mut Context<Self>) {
        if self.open.as_ref().is_some_and(|open| open.id == id) {
            return;
        }
        self.close_open(cx);
        let state = self.state.clone();
        state.update(cx, |state, cx| {
            state.watch_subagent_doc(id.clone(), cx);
            // Reading it here is reading it: clear the unseen marker exactly
            // as opening it as the main chat would.
            state.mark_chat_seen(&id, cx);
        });
        let transcript = cx.new(|cx| Transcript::for_doc(state.clone(), id.clone(), true, cx));
        let mut links = self.links.clone();
        links.source_session = Some(id.clone());
        transcript.update(cx, |transcript, _| {
            transcript.set_workspace_link_handler(links)
        });
        let input = cx.new(|cx| ComposerInput::new("Reply to this child chat…", cx));
        let input_events = cx.subscribe(&input, |panel, _, event, cx| {
            if matches!(
                event,
                ComposerInputEvent::Submitted | ComposerInputEvent::ModifierSubmitted
            ) {
                panel.send(cx);
            }
        });
        self.open = Some(OpenChild {
            id,
            transcript,
            input,
            _input_events: input_events,
            sending: false,
            notice: None,
            focus_pending: true,
            _send: None,
        });
        cx.notify();
    }

    fn close_open(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = self.open.take() {
            self.state.update(cx, |state, cx| {
                state.unwatch_subagent_doc(&open.id);
                state.mark_chat_seen(&open.id, cx);
                cx.notify();
            });
        }
    }

    pub(super) fn back(&mut self, cx: &mut Context<Self>) {
        if self.open.is_none() {
            return;
        }
        self.close_open(cx);
        self.focus_root_pending = true;
        cx.notify();
    }

    /// Queue the reply on the child exactly as the engine starts a child: a
    /// held queue row on the child's host, delivered now when it is idle or
    /// when its current turn ends. The child's own execution settings apply.
    pub(super) fn send(&mut self, cx: &mut Context<Self>) {
        if !self.is_current(cx) {
            return;
        }
        let Some((id, text)) = self.open.as_ref().and_then(|open| {
            let text = open.input.read(cx).text().trim().to_string();
            (!open.sending && !text.is_empty()).then(|| (open.id.clone(), text))
        }) else {
            return;
        };
        let prepared = prepare_reply(self.state.read(cx), &id, Utc::now());
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let (engine, host, running) = match prepared {
            Ok(prepared) => prepared,
            Err(None) => return,
            Err(Some(error)) => {
                open.notice = Some((true, error.into()));
                cx.notify();
                return;
            }
        };
        let chat_id = id;
        let message_id = uuid::Uuid::new_v4().to_string();
        let intent = serde_json::json!({
            "kind": "queueMessage",
            "messageId": message_id,
            "text": text,
            "attachments": [],
            "inputItems": [],
            "holdForTurnEnd": true,
        });
        open.sending = true;
        open.notice = None;
        let task_chat = chat_id.clone();
        open._send = Some(cx.spawn(async move |this, cx| {
            let result = Tokio::spawn(cx, async move {
                tokio::time::timeout(
                    Duration::from_secs(45),
                    crate::task_commands::execute_receipt_on_with_id(
                        &engine,
                        &task_chat,
                        Some(&host),
                        message_id,
                        intent,
                    ),
                )
                .await
                .map_err(|_| {
                    luna_rpc::RpcError::Failed(
                        "Reply timed out. Delivery is unknown; check the transcript before sending again."
                            .into(),
                    )
                })?
            })
            .await;
            this.update(cx, |panel, cx| {
                if !panel.is_current(cx) {
                    return;
                }
                let Some(open) = panel.open.as_mut().filter(|open| open.id == chat_id) else {
                    return;
                };
                open.sending = false;
                let delivered = match result {
                    Ok(Ok(receipt)) => match receipt.status {
                        CommandReceiptStatus::Applied => Ok(if running {
                            "Queued. It is delivered when the current turn ends."
                        } else {
                            "Sent."
                        }),
                        CommandReceiptStatus::Pending => {
                            Ok("Sent. Waiting for the host to confirm delivery.")
                        }
                        _ => Err(crate::task_commands::failure_message(
                            &receipt,
                            "The child chat rejected this reply.",
                        )),
                    },
                    Ok(Err(error)) => Err(format!("Delivery is unknown: {error}")),
                    Err(error) => Err(format!("Delivery is unknown: {error}")),
                };
                match delivered {
                    Ok(notice) => {
                        if open.input.read(cx).text().trim() == text {
                            open.input.update(cx, |input, cx| input.set_text("", cx));
                        }
                        open.notice = Some((false, notice.into()));
                    }
                    Err(error) => open.notice = Some((true, error)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn control(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let label = label.into();
        let border_strong = theme.border_strong;
        let hover = crate::theme::ink(0.06);
        div()
            .id(id.into())
            .role(gpui::Role::Button)
            .aria_label(label)
            .tab_index(0)
            .h(px(crate::surface_chrome::CONTROL_SIZE))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.0))
            .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
            .border_1()
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(border_strong))
    }

    fn status_glyph(
        &self,
        key: String,
        status: ChildChatStatus,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if status == ChildChatStatus::Running {
            loaders::mini_glyph_spinner(key, 2.0, theme.glyph, cx.entity_id(), cx)
                .into_any_element()
        } else {
            div()
                .size(px(6.0))
                .flex_none()
                .rounded_full()
                .bg(status.color(theme))
                .into_any_element()
        }
    }

    fn render_row(&self, row: ChildChatRow, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let now = Utc::now();
        let time_ago = format_time_ago(row.updated_at, now);
        let label = row.status.label();
        let accessible = format!(
            "{}, {label}, updated {time_ago}. Open child chat",
            row.title
        );
        let meta = [
            Some(label.to_string()),
            row.model.clone(),
            (row.children > 0).then(|| {
                format!(
                    "{} child chat{}",
                    row.children,
                    if row.children == 1 { "" } else { "s" }
                )
            }),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let status_color = row.status.color(theme);
        let open_id = row.id.clone();
        let key_id = row.id.clone();
        let border_strong = theme.border_strong;
        let hover = crate::theme::ink(0.05);
        let muted = theme.text_muted;
        div()
            .id(SharedString::from(format!("child-chat-row-{}", row.id)))
            .role(gpui::Role::Button)
            .aria_label(accessible)
            .tab_index(0)
            .w_full()
            .flex_none()
            .px(px(12.0))
            .py(px(9.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(crate::theme::ink(0.02))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .cursor_pointer()
            .hover(move |style| style.bg(hover).border_color(border_strong))
            .focus_visible(move |style| style.border_color(border_strong))
            .on_click(cx.listener(move |panel, _, _, cx| panel.open_child(open_id.clone(), cx)))
            .on_key_down(
                cx.listener(move |panel, event: &gpui::KeyDownEvent, _, cx| {
                    if is_settings_nav_activation(&event.keystroke.key, event.is_held) {
                        panel.open_child(key_id.clone(), cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().w(px(12.0)).flex_none().flex().justify_center().child(
                        self.status_glyph(
                            format!("child-chat-row-glyph-{}", row.id),
                            row.status,
                            theme,
                            cx,
                        ),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(13.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(row.title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.readable_muted(0.6))
                            .child(time_ago),
                    )
                    .child(
                        icon(icons::ALT_ARROW_RIGHT)
                            .size(px(12.0))
                            .flex_none()
                            .text_color(muted.opacity(0.7)),
                    ),
            )
            .child(
                div()
                    .pl(px(20.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(status_color)
                            .child(meta[0].clone()),
                    )
                    .when(meta.len() > 1, |el| {
                        el.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.readable_muted(0.6))
                                .child(format!("· {}", meta[1..].join(" · "))),
                        )
                    }),
            )
            .when_some(row.detail, |el, detail| {
                el.child(
                    div()
                        .pl(px(20.0))
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.readable_muted(0.72))
                        .child(detail),
                )
            })
            .into_any_element()
    }

    fn render_list(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let (parent_title, rows) = {
            let state = self.state.read(cx);
            let title = state
                .chats
                .iter()
                .find(|chat| chat.id == self.parent)
                .and_then(|chat| chat.title.clone())
                .unwrap_or_else(|| "Conversation".into());
            (
                transcript::single_line(&title),
                child_chat_rows(state, &self.parent, Utc::now()),
            )
        };
        let mut list = div()
            .id("child-chats-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(12.0))
            .pb(px(12.0))
            .flex()
            .flex_col()
            .gap(px(6.0));
        if rows.is_empty() {
            list = list.child(
                div()
                    .p(px(8.0))
                    .text_color(theme.text_muted)
                    .child("No child chats yet. Use “New child chat…” in a chat's menu, or let an agent start one."),
            );
        }
        // Rows arrive rank-sorted, so each section is one contiguous run.
        let mut sections: Vec<(u8, Vec<ChildChatRow>)> = Vec::new();
        for row in rows {
            let current = section_of(row.status);
            match sections.last_mut() {
                Some((section, items)) if *section == current => items.push(row),
                _ => sections.push((current, vec![row])),
            }
        }
        for (section, items) in sections {
            let title = match section {
                0 => "Active",
                1 => "Recent",
                _ => "Archived",
            };
            list = list.child(
                div()
                    .pt(px(8.0))
                    .pb(px(2.0))
                    .px(px(4.0))
                    .flex()
                    .gap(px(6.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.readable_muted(0.6))
                    .child(title)
                    .child(
                        div()
                            .text_color(theme.readable_muted(0.45))
                            .child(items.len().to_string()),
                    ),
            );
            for row in items {
                list = list.child(self.render_row(row, theme, cx));
            }
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                crate::surface_chrome::toolbar(theme).child(
                    div()
                        .px(px(4.0))
                        .min_w_0()
                        .truncate()
                        .child(format!("Child chats · {parent_title}")),
                ),
            )
            .child(
                div()
                    .px(px(16.0))
                    .pt(px(10.0))
                    .pb(px(4.0))
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.readable_muted(0.7))
                    .child("Chats started from this conversation. Open one to follow it and reply here; each keeps its own history."),
            )
            .child(list)
    }

    fn render_open(
        &mut self,
        window: &mut Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let Some(open) = self.open.as_mut() else {
            return div();
        };
        if std::mem::take(&mut open.focus_pending) {
            window.focus(&open.input.focus_handle(cx), cx);
        }
        let id = open.id.clone();
        let transcript = open.transcript.clone();
        let input = open.input.clone();
        let sending = open.sending;
        let notice = open.notice.clone();
        let (row, loading) = {
            let state = self.state.read(cx);
            let row = child_chat_rows(state, &self.parent, Utc::now())
                .into_iter()
                .find(|row| row.id == id);
            (row, state.sub_transcript(&id).is_empty())
        };
        let title = row
            .as_ref()
            .map(|row| row.title.clone())
            .unwrap_or_else(|| "Child chat".into());
        let status = row
            .as_ref()
            .map(|row| row.status)
            .unwrap_or(ChildChatStatus::Idle);
        let muted = theme.text_muted;
        let back = self
            .control("child-chat-back", "Back to child chats", theme)
            .pl(px(4.0))
            .pr(px(8.0))
            .text_color(muted)
            .tooltip(|_, cx| {
                cx.new(|_| SurfaceTabTooltip {
                    text: "Back to child chats (Esc)".into(),
                })
                .into()
            })
            .tooltip_show_delay(Duration::from_millis(350))
            .on_click(cx.listener(|panel, _, _, cx| panel.back(cx)))
            .on_key_down(cx.listener(|panel, event: &gpui::KeyDownEvent, _, cx| {
                if is_settings_nav_activation(&event.keystroke.key, event.is_held) {
                    panel.back(cx);
                    cx.stop_propagation();
                }
            }))
            .child(icon(icons::ALT_ARROW_LEFT).size(px(14.0)).text_color(muted))
            .child("Child chats");
        let promote_id = id.clone();
        let promote_key_id = id.clone();
        let open_main = self
            .control("child-chat-open-main", "Open as main chat", theme)
            .w(px(crate::surface_chrome::CONTROL_SIZE))
            .justify_center()
            .tooltip(|_, cx| {
                cx.new(|_| SurfaceTabTooltip {
                    text: "Open as main chat".into(),
                })
                .into()
            })
            .tooltip_show_delay(Duration::from_millis(350))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(ChildChatEvent::OpenAsMain(promote_id.clone()))
            }))
            .on_key_down(cx.listener(move |_, event: &gpui::KeyDownEvent, _, cx| {
                if is_settings_nav_activation(&event.keystroke.key, event.is_held) {
                    cx.emit(ChildChatEvent::OpenAsMain(promote_key_id.clone()));
                    cx.stop_propagation();
                }
            }))
            .child(
                icon(icons::ARROW_UP_RIGHT)
                    .size(px(crate::surface_chrome::ICON_SIZE))
                    .text_color(muted),
            );
        let toolbar = crate::surface_chrome::toolbar(theme)
            .child(back)
            .child(
                div()
                    .flex_none()
                    .text_color(theme.readable_muted(0.4))
                    .child("/"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .px(px(2.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(title),
            )
            .child(open_main);
        let meta = [
            row.as_ref().and_then(|row| row.model.clone()),
            row.as_ref()
                .map(|row| format!("updated {}", format_time_ago(row.updated_at, Utc::now()))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let status_strip = div()
            .px(px(16.0))
            .py(px(8.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .text_size(crate::typography::ui_rems(11.5))
            .child(self.status_glyph(format!("child-chat-open-glyph-{id}"), status, theme, cx))
            .child(
                div()
                    .flex_none()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(status.color(theme))
                    .child(status.label()),
            )
            .when(!meta.is_empty(), |el| {
                el.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.readable_muted(0.6))
                        .child(format!("· {meta}")),
                )
            });
        let attention = match status {
            ChildChatStatus::NeedsApproval => Some("This child chat is waiting for an approval."),
            ChildChatStatus::NeedsInput => Some("This child chat is asking you a question."),
            _ => None,
        };
        let attention_id = id.clone();
        let attention_key_id = id.clone();
        let mut root = div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(status_strip)
            .when_some(attention, |el, text| {
                el.child(
                    div()
                        .mx(px(12.0))
                        .mt(px(10.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(px(8.0))
                        .bg(theme.warning.opacity(0.08))
                        .border_1()
                        .border_color(theme.warning.opacity(0.25))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(div().flex_1().min_w_0().child(text))
                        .child(
                            self.control("child-chat-respond", "Open to respond", theme)
                                .px(px(8.0))
                                .border_color(theme.border)
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(ChildChatEvent::OpenAsMain(attention_id.clone()))
                                }))
                                .on_key_down(cx.listener(
                                    move |_, event: &gpui::KeyDownEvent, _, cx| {
                                        if is_settings_nav_activation(
                                            &event.keystroke.key,
                                            event.is_held,
                                        ) {
                                            cx.emit(ChildChatEvent::OpenAsMain(
                                                attention_key_id.clone(),
                                            ));
                                            cx.stop_propagation();
                                        }
                                    },
                                ))
                                .child("Open to respond"),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(transcript)
                    .when(loading, |el| {
                        el.child(
                            div()
                                .absolute()
                                .top(px(16.0))
                                .left(px(16.0))
                                .text_color(theme.text_muted)
                                .child("Loading messages…"),
                        )
                    }),
            );
        if status == ChildChatStatus::Archived {
            root = root.child(
                div()
                    .px(px(16.0))
                    .py(px(12.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .text_color(theme.text_muted)
                    .child("Archived · read-only. Unarchive it to reply."),
            );
        } else {
            let has_text = !input.read(cx).is_empty();
            let hint: (bool, String) = notice.unwrap_or_else(|| {
                (
                    false,
                    if sending {
                        "Sending…".into()
                    } else if status == ChildChatStatus::Running {
                        "Replies queue until the current turn ends".into()
                    } else {
                        "Replies go to this child chat, not the parent".into()
                    },
                )
            });
            let border_strong = theme.border_strong;
            root = root.child(
                div()
                    .p(px(12.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .min_h(px(48.0))
                            .max_h(px(160.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(crate::theme::ink(0.02))
                            .p(px(10.0))
                            .child(input),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(crate::typography::ui_rems(11.5))
                                    .text_color(if hint.0 {
                                        theme.danger
                                    } else {
                                        theme.text_muted
                                    })
                                    .child(hint.1),
                            )
                            .child(
                                div()
                                    .id("child-chat-send")
                                    .role(gpui::Role::Button)
                                    .aria_label("Send reply to child chat")
                                    .tab_index(0)
                                    .flex_none()
                                    .px(px(10.0))
                                    .py(px(6.0))
                                    .rounded(px(6.0))
                                    .border_1()
                                    .border_color(theme.border)
                                    .cursor_pointer()
                                    .text_color(theme.text)
                                    .text_size(crate::typography::ui_rems(12.0))
                                    .hover(move |style| style.border_color(border_strong))
                                    .focus_visible(move |style| style.border_color(border_strong))
                                    .when(sending || !has_text, |style| style.opacity(0.5))
                                    .on_click(cx.listener(|panel, _, _, cx| panel.send(cx)))
                                    .on_key_down(cx.listener(
                                        |panel, event: &gpui::KeyDownEvent, _, cx| {
                                            if is_settings_nav_activation(
                                                &event.keystroke.key,
                                                event.is_held,
                                            ) {
                                                panel.send(cx);
                                                cx.stop_propagation();
                                            }
                                        },
                                    ))
                                    .child(if sending { "Sending…" } else { "Send" }),
                            ),
                    ),
            );
        }
        root
    }
}

/// List sections: 0 = needs you or running, 1 = everything else, 2 = archived.
fn section_of(status: ChildChatStatus) -> u8 {
    match status.rank() {
        0 | 1 => 0,
        2 => 1,
        _ => 2,
    }
}

impl Render for ChildChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        if std::mem::take(&mut self.focus_root_pending) {
            window.focus(&self.focus, cx);
        }
        let body = if self.open.is_some() {
            self.render_open(window, &theme, cx)
        } else {
            self.render_list(&theme, cx)
        };
        div()
            .id("child-chats-panel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text)
            .on_key_down(cx.listener(|panel, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && panel.open.is_some() {
                    panel.back(cx);
                    cx.stop_propagation();
                }
            }))
            .child(body)
    }
}

impl Shell {
    pub(super) fn release_child_chat_panel(&mut self, cx: &mut Context<Self>) {
        self.child_chat_events = None;
        // Dropping the last handle runs the panel's release hook, which
        // cancels its doc watch.
        if self.child_chat_panel.take().is_some() {
            cx.notify();
        }
    }

    fn ensure_child_chat_panel(&mut self, cx: &mut Context<Self>) -> Entity<ChildChatPanel> {
        if let Some(panel) = &self.child_chat_panel
            && panel.read(cx).parent() == self.active_chat
            && panel.read(cx).is_current(cx)
        {
            return panel.clone();
        }
        self.release_child_chat_panel(cx);
        let links = Self::session_links(Some(self.active_chat.clone()), cx);
        let panel = cx
            .new(|cx| ChildChatPanel::new(self.state.clone(), self.active_chat.clone(), links, cx));
        self.child_chat_events = Some(cx.subscribe(&panel, |shell, _, event, cx| match event {
            ChildChatEvent::OpenAsMain(id) => {
                // Release first: the pane's watch and the main transcript
                // watch share the child's doc id.
                shell.release_child_chat_panel(cx);
                shell.open_chat(id.clone(), cx);
            }
        }));
        self.child_chat_panel = Some(panel.clone());
        panel
    }

    pub(super) fn render_child_chats(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_child_chat_panel(cx).into_any_element()
    }

    /// The current chat's non-archived child count (picker badge).
    pub(super) fn active_child_chat_count(&self, cx: &App) -> (usize, bool) {
        if self.active_chat.is_empty() {
            return (0, false);
        }
        let chats = &self.state.read(cx).chats;
        let mut active = 0;
        let mut any = false;
        for chat in chats
            .iter()
            .filter(|chat| is_child_of(chat, &self.active_chat))
        {
            any = true;
            if !chat.archived {
                active += 1;
            }
        }
        (active, any)
    }

    /// Open `parent`'s Child chats surface, selecting the parent first when
    /// needed (sidebar count chip). Pre-seeding the parent's panel state lets
    /// the chat switch restore straight into the list.
    pub(super) fn open_child_chats_for(&mut self, parent: String, cx: &mut Context<Self>) {
        if parent == self.active_chat {
            self.add_work_surface(RightSurface::ChildChats, cx);
            return;
        }
        push_unique_right_surface(
            self.right_tabs.entry(parent.clone()).or_default(),
            RightSurface::ChildChats,
        );
        self.panels.update(&parent, |panel| {
            panel.changes_open = true;
            panel.right_active = RightSurface::ChildChats;
        });
        self.open_chat(parent, cx);
    }
}

/// Native visual QA drives the production panel through these hooks.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_open_right_pane(&mut self, cx: &mut Context<Self>) {
        if !self.right_pane_open(cx) {
            self.toggle_right_pane(cx);
        }
        let key = self.panel_key(cx);
        self.panels
            .update(&key, |panel| panel.right_active = RightSurface::Picker);
        cx.notify();
    }

    pub fn fixture_open_child_chats(&mut self, cx: &mut Context<Self>) {
        self.add_work_surface(RightSurface::ChildChats, cx);
    }

    pub fn fixture_open_child_chats_for(&mut self, parent: String, cx: &mut Context<Self>) {
        self.open_child_chats_for(parent, cx);
    }

    pub fn fixture_child_chat_open(&mut self, id: String, cx: &mut Context<Self>) {
        let panel = self.ensure_child_chat_panel(cx);
        panel.update(cx, |panel, cx| panel.open_child(id, cx));
    }

    pub fn fixture_child_chat_back(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.child_chat_panel.clone() {
            panel.update(cx, |panel, cx| panel.back(cx));
        }
    }

    pub fn fixture_child_chat_send(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(panel) = self.child_chat_panel.clone() {
            panel.update(cx, |panel, cx| {
                if let Some(open) = &panel.open {
                    open.input.update(cx, |input, cx| input.set_text(text, cx));
                }
                panel.send(cx);
            });
        }
    }

    pub fn fixture_close_right_pane(&mut self, cx: &mut Context<Self>) {
        if self.right_pane_open(cx) {
            self.toggle_right_pane(cx);
        }
    }

    pub fn fixture_child_chats_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        let state = self.state.read(cx);
        let now = Utc::now();
        let panel = self.child_chat_panel.as_ref().map(|panel| panel.read(cx));
        let open = panel.and_then(|panel| panel.open.as_ref());
        let (badge, any) = self.active_child_chat_count(cx);
        serde_json::json!({
            "activeChat": self.active_chat,
            "rightPaneOpen": self.right_pane_open(cx),
            "rightActive": format!("{:?}", self.resolved_right_active(cx)),
            "pickerBadge": badge,
            "pickerCardVisible": any,
            "panelParent": panel.map(|panel| panel.parent().to_string()),
            "openChild": panel.and_then(|panel| panel.open_child_id().map(str::to_string)),
            "openNotice": open.and_then(|open| open.notice.as_ref().map(|(error, text)| serde_json::json!({"error": error, "text": text}))),
            "openSending": open.map(|open| open.sending),
            "openInput": open.map(|open| open.input.read(cx).text().to_string()),
            "watchedDocs": state.fixture_sub_watch_ids(),
            "openTranscriptRows": open.map(|open| state.sub_transcript(&open.id).len()),
            "openTranscriptText": open.map(|open| {
                state
                    .sub_transcript(&open.id)
                    .iter()
                    .flat_map(|entry| entry.parts.iter())
                    .filter_map(|part| match part {
                        luna_doc::MessagePart::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            }),
            "rows": child_chat_rows(state, &self.active_chat, now)
                .into_iter()
                .map(|row| serde_json::json!({
                    "id": row.id,
                    "title": row.title,
                    "status": row.status.label(),
                    "children": row.children,
                }))
                .collect::<Vec<_>>(),
            "sidebarOrder": self.sidebar_visible_order(cx),
            "collapsedChildChats": self.settings.collapsed_child_chats,
        })
    }
}
