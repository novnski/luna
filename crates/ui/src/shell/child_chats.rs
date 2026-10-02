//! Child chats in the right pane.
//!
//! Persistent child chats (`Chat.child_thread`) belong to the conversation
//! that started them. Instead of an indented sidebar tree, the parent's right
//! pane lists them ([`RightSurface::ChildChats`]) and drills into one child as
//! a full chat: the shared pane chat view ([`super::pane_chat`]) with the main
//! transcript and the main composer bound to the child.
//!
//! One open child holds exactly one set of pane watches
//! (`AppState::watch_pane_chat`: doc messages, queue and native tasks, keyed
//! by the child's chat id). Back, closing the tab or pane, switching chats and
//! engine replacement release them. The child's composer is cached by the
//! shell ([`super::pane_chat::PaneComposers`]), so its draft and any send in
//! flight survive all of those.

use super::pane_chat::{PaneChatEvent, PaneChatView, PaneComposers, PaneIdentity};
use super::*;
use crate::markdown::render::LinkUi;
use chrono::DateTime;
use luna_proto::{Chat, ChatIndicator};

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
    pub(super) fn color(self, theme: &Theme) -> gpui::Hsla {
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

pub(super) fn provider_label(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::Codex => "Codex",
        HarnessId::ClaudeCode => "Claude Code",
        HarnessId::Opencode => "OpenCode",
        HarnessId::Mock => "Mock",
        _ => "Provider",
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
                let provider = provider_label(config.harness);
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

pub(super) enum ChildChatEvent {
    /// Promote this child to the main conversation view.
    OpenAsMain(String),
    /// Bring the parent conversation (the main view) forward.
    ShowParent(String),
    /// A child opened: route its transcript's history/subagent events.
    Opened(Entity<Transcript>),
}

impl gpui::EventEmitter<ChildChatEvent> for ChildChatPanel {}

struct OpenChild {
    id: String,
    view: Entity<PaneChatView>,
    _events: Subscription,
}

pub(super) struct ChildChatPanel {
    state: Entity<AppState>,
    composers: Entity<PaneComposers>,
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
    pub(super) fn open_chat_id(&self) -> Option<&str> {
        self.open.as_ref().map(|open| open.id.as_str())
    }

    pub(super) fn reveal_message(&mut self, id: String, message: String, cx: &mut Context<Self>) {
        self.open_child(id.clone(), cx);
        if let Some(open) = &self.open {
            open.view
                .read(cx)
                .transcript()
                .clone()
                .update(cx, |view, cx| {
                    view.navigate_to_message(id, message, cx);
                });
        }
    }

    pub(super) fn new(
        state: Entity<AppState>,
        composers: Entity<PaneComposers>,
        parent: String,
        links: LinkUi,
        cx: &mut Context<Self>,
    ) -> Self {
        let engine_generation = state.read(cx).engine_generation();
        // Rows, statuses and the open transcript all derive from AppState.
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        cx.on_release(|panel, cx| {
            if let Some(open) = panel.open.take() {
                panel.state.update(cx, |state, _| {
                    state.unwatch_pane_chat(&open.id);
                });
            }
        })
        .detach();
        Self {
            state,
            composers,
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

    /// A replaced engine invalidates the watch and any in-flight send.
    pub(super) fn is_current(&self, cx: &App) -> bool {
        self.state.read(cx).engine_generation() == self.engine_generation
    }

    pub(super) fn open_child(&mut self, id: String, cx: &mut Context<Self>) {
        if self.open.as_ref().is_some_and(|open| open.id == id) {
            return;
        }
        self.close_open(cx);
        let composer = self
            .composers
            .update(cx, |composers, cx| composers.composer(&id, cx));
        let state = self.state.clone();
        let mut links = self.links.clone();
        links.source_session = Some(id.clone());
        let parent = self.parent.clone();
        let view_id = id.clone();
        let view = cx.new(|cx| {
            PaneChatView::new(
                state,
                view_id,
                PaneIdentity::Child,
                parent,
                "Child chats",
                composer,
                links,
                cx,
            )
        });
        let events = cx.subscribe(&view, |panel, _, event, cx| match event {
            PaneChatEvent::Back => panel.back(cx),
            PaneChatEvent::OpenAsMain(id) => cx.emit(ChildChatEvent::OpenAsMain(id.clone())),
            PaneChatEvent::ShowParent(id) => cx.emit(ChildChatEvent::ShowParent(id.clone())),
            PaneChatEvent::Unload => {}
        });
        let transcript = view.read(cx).transcript().clone();
        self.open = Some(OpenChild {
            id,
            view,
            _events: events,
        });
        cx.emit(ChildChatEvent::Opened(transcript));
        cx.notify();
    }

    fn close_open(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = self.open.take() {
            self.state.update(cx, |state, cx| {
                state.unwatch_pane_chat(&open.id);
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

    fn render_open(&self) -> AnyElement {
        match &self.open {
            Some(open) => open.view.clone().into_any_element(),
            None => Empty.into_any_element(),
        }
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
            div().size_full().child(self.render_open())
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
        self.pane_transcript_events = None;
        // Dropping the last handle runs the panel's release hook, which
        // cancels its doc watch.
        if self.child_chat_panel.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn ensure_child_chat_panel(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<ChildChatPanel> {
        if let Some(panel) = &self.child_chat_panel
            && panel.read(cx).parent() == self.active_chat
            && panel.read(cx).is_current(cx)
        {
            return panel.clone();
        }
        self.release_child_chat_panel(cx);
        let links = Self::session_links(Some(self.active_chat.clone()), cx);
        let composers = self.pane_composers.clone();
        let panel = cx.new(|cx| {
            ChildChatPanel::new(
                self.state.clone(),
                composers,
                self.active_chat.clone(),
                links,
                cx,
            )
        });
        self.child_chat_events = Some(cx.subscribe(&panel, |shell, _, event, cx| match event {
            ChildChatEvent::OpenAsMain(id) => shell.open_pane_chat_as_main(id.clone(), cx),
            ChildChatEvent::ShowParent(id) => shell.show_pane_parent(id.clone(), cx),
            ChildChatEvent::Opened(transcript) => {
                shell.pane_transcript_events =
                    Some(cx.subscribe(transcript, Self::on_transcript_event));
            }
        }));
        self.child_chat_panel = Some(panel.clone());
        panel
    }

    /// Promote a pane chat to the main view. Release first: the pane's watch
    /// and the main transcript watch share the doc id. The pane composer's
    /// draft is made durable before the main composer loads it, so the text
    /// moves with the chat instead of two composers editing one draft.
    pub(super) fn open_pane_chat_as_main(&mut self, id: String, cx: &mut Context<Self>) {
        self.release_child_chat_panel(cx);
        self.pending_open_main = None;
        let Some(composer) = self.pane_composers.read(cx).get(&id) else {
            self.open_chat(id, cx);
            return;
        };
        if composer.update(cx, |composer, cx| composer.prepare_close(cx)) {
            self.finish_open_pane_chat_as_main(id, cx);
            return;
        }
        let owner_chat = self.active_chat.clone();
        let owner_generation = self.state.read(cx).engine_generation();
        self.pending_open_main = Some(cx.subscribe(&composer, move |shell, _, event, cx| {
            if matches!(event, ComposerEvent::DraftFlushCompleted) {
                shell.pending_open_main = None;
                if shell.active_chat != owner_chat
                    || shell.state.read(cx).engine_generation() != owner_generation
                {
                    return;
                }
                shell.finish_open_pane_chat_as_main(id.clone(), cx);
            }
        }));
    }

    fn finish_open_pane_chat_as_main(&mut self, id: String, cx: &mut Context<Self>) {
        // A composer with a send or receipt check in flight keeps owning it;
        // only an idle one is dropped so the main view reloads the draft.
        let idle = self
            .pane_composers
            .read(cx)
            .get(&id)
            .is_some_and(|composer| !composer.read(cx).has_work_in_flight());
        if idle {
            self.pane_composers
                .update(cx, |composers, _| composers.remove(&id));
        }
        self.composer
            .update(cx, |composer, _| composer.forget_cached_draft(&id));
        self.open_chat(id, cx);
    }

    /// The parent link: bring the parent conversation forward in the main view.
    pub(super) fn show_pane_parent(&mut self, parent: String, cx: &mut Context<Self>) {
        if self.right_pane_expanded {
            self.toggle_right_pane_expand(cx);
        }
        if self.active_chat != parent {
            self.open_chat(parent, cx);
        }
        cx.notify();
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

    fn fixture_open_child_composer(&self, cx: &App) -> Option<Entity<Composer>> {
        let panel = self.child_chat_panel.as_ref()?.read(cx);
        Some(panel.open.as_ref()?.view.read(cx).composer().clone())
    }

    /// Type into the open child's composer (a draft; nothing is sent).
    pub fn fixture_child_chat_type(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(composer) = self.fixture_open_child_composer(cx) {
            let input = composer.read(cx).input.clone();
            input.update(cx, |input, cx| input.set_text(text, cx));
        }
    }

    /// Type and press Enter in the open child's composer: the main composer's
    /// own submit path, bound to the child.
    pub fn fixture_child_chat_send(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(composer) = self.fixture_open_child_composer(cx) {
            let input = composer.read(cx).input.clone();
            input.update(cx, |input, cx| {
                input.set_text(text, cx);
                cx.emit(crate::composer::ComposerInputEvent::Submitted);
            });
        }
    }

    pub fn fixture_child_chat_open_main(&mut self, cx: &mut Context<Self>) {
        let id = self
            .child_chat_panel
            .as_ref()
            .and_then(|panel| panel.read(cx).open.as_ref().map(|open| open.id.clone()));
        if let Some(id) = id {
            self.open_pane_chat_as_main(id, cx);
        }
    }

    pub fn fixture_main_composer_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        self.composer.read(cx).readback(cx)
    }

    pub fn fixture_set_right_pane_width(&mut self, width: f32, cx: &mut Context<Self>) {
        self.settings.right_pane_width = width;
        cx.notify();
    }

    pub fn fixture_engine(&self, cx: &Context<Self>) -> Option<crate::state::EngineHandle> {
        self.state.read(cx).engine().cloned()
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
            "openChild": open.map(|open| open.id.clone()),
            "openComposer": open.map(|open| open.view.read(cx).composer().read(cx).readback(cx)),
            "openTranscriptScroll": open.map(|open| open.view.read(cx).transcript().read(cx).scroll_readback()),
            "openNotice": open.and_then(|open| open.view.read(cx).composer().read(cx).failure.as_ref().map(|text| serde_json::json!({"error": true, "text": text.to_string()}))),
            "openSending": open.map(|open| open.view.read(cx).composer().read(cx).is_sending()),
            "openInput": open.map(|open| open.view.read(cx).composer().read(cx).input.read(cx).text().to_string()),
            "openEchoes": open.map(|open| state.echoes_for(&open.id).len()),
            "openTranscriptUserIds": open.map(|open| {
                state
                    .sub_transcript(&open.id)
                    .iter()
                    .filter(|entry| entry.role == luna_doc::MessageRole::User)
                    .map(|entry| entry.id.clone())
                    .collect::<Vec<_>>()
            }),
            "mainComposer": self.composer.read(cx).readback(cx),
            "paneComposerStates": self
                .pane_composers
                .read(cx)
                .ids()
                .into_iter()
                .filter_map(|id| {
                    let composer = self.pane_composers.read(cx).get(&id)?;
                    Some((id, composer.read(cx).readback(cx)))
                })
                .collect::<serde_json::Map<_, _>>(),
            "parentTranscriptRows": state.transcript.len(),
            "parentTranscriptText": state
                .transcript
                .iter()
                .flat_map(|entry| entry.parts.iter())
                .filter_map(|part| match part {
                    luna_doc::MessagePart::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            "openSendPending": open.map(|open| state.send_pending(&open.id, now)),
            "paneChats": state.pane_chat_ids(),
            "paneComposers": self.pane_composers.read(cx).ids(),
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
