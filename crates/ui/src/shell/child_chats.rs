//! Child chats in the right pane.
//!
//! Persistent child chats (`Chat.child_thread`) belong to the conversation
//! that started them. Instead of an indented sidebar tree, the parent's right
//! pane lists them with its side chats ([`RightSurface::RelatedChats`],
//! [`super::related_chats`]) and drills into one child as
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
pub(crate) enum ChildChatStatus {
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
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NeedsApproval => "Needs approval",
            Self::NeedsInput => "Needs input",
            Self::Running => "Running",
            Self::Failed => "Failed",
            Self::Interrupted => "Interrupted",
            // A child's own turn ended; its owned background work (if any)
            // is the work view's, never implied done here.
            Self::Completed => "Turn completed",
            Self::Idle => "Idle",
            Self::Archived => "Archived",
        }
    }

    /// Readback order: attention, then live work, then everything else by
    /// recency; archived rows last.
    #[cfg(feature = "appshots-fixture")]
    fn rank(self) -> u8 {
        match self {
            Self::NeedsApproval | Self::NeedsInput => 0,
            Self::Running => 1,
            Self::Archived => 3,
            _ => 2,
        }
    }

    /// The sidebar's status palette, so a child reads the same in both places.
    pub(crate) fn color(self, theme: &Theme) -> gpui::Hsla {
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

#[cfg(feature = "appshots-fixture")]
pub(super) struct ChildChatRow {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) status: ChildChatStatus,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) detail: Option<String>,
    pub(super) model: Option<String>,
    pub(super) children: usize,
}

#[cfg(feature = "appshots-fixture")]
fn is_child_of(chat: &Chat, parent: &str) -> bool {
    chat.id != parent
        && chat
            .child_thread
            .as_ref()
            .is_some_and(|child| child.parent_task_id == parent)
}

/// Non-archived direct child chats per parent id (QA readback).
#[cfg(feature = "appshots-fixture")]
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
pub(crate) fn child_chat_status(
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
        // Unread alone never implies a completed turn.
        _ => ChildChatStatus::Idle,
    }
}

pub(crate) fn provider_label(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::Codex => "Codex",
        HarnessId::ClaudeCode => "Claude Code",
        HarnessId::Opencode => "OpenCode",
        HarnessId::Mock => "Mock",
        _ => "Provider",
    }
}

#[cfg(feature = "appshots-fixture")]
fn first_line(text: &str) -> Option<String> {
    let line = transcript::single_line(text);
    let line = line.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// This parent's direct children: attention and running first, then the rest
/// by recency, archived last.
#[cfg(feature = "appshots-fixture")]
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
                title: transcript::single_line(chat.title.as_deref().unwrap_or("New thread")),
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

    /// The open chat's pane identity label (Child chat or Alongside), for QA.
    #[cfg(feature = "appshots-fixture")]
    pub(super) fn open_identity(&self, cx: &App) -> Option<&'static str> {
        self.open
            .as_ref()
            .map(|open| open.view.read(cx).identity().label())
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
        // Its own child, or an ordinary chat listed alongside (no parent).
        let identity = if self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .and_then(super::placement::lineage_parent_id)
            == Some(parent.as_str())
        {
            PaneIdentity::Child
        } else {
            PaneIdentity::Alongside
        };
        let view = cx.new(|cx| {
            PaneChatView::new(
                state,
                view_id,
                identity,
                parent,
                "Related chats",
                composer,
                links,
                cx,
            )
        });
        let events = cx.subscribe(&view, |panel, _, event, cx| match event {
            PaneChatEvent::Back => panel.back(cx),
            PaneChatEvent::OpenAsMain(id) => cx.emit(ChildChatEvent::OpenAsMain(id.clone())),
            PaneChatEvent::ShowParent(id) => cx.emit(ChildChatEvent::ShowParent(id.clone())),
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

    fn render_open(&self) -> AnyElement {
        match &self.open {
            Some(open) => open.view.clone().into_any_element(),
            None => Empty.into_any_element(),
        }
    }
}

impl Render for ChildChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        if std::mem::take(&mut self.focus_root_pending) {
            window.focus(&self.focus, cx);
        }
        // The list itself is the shell's ([`Shell::render_child_chat_list`]):
        // it shares the sidebar's task rows and their actions.
        let body = div().size_full().child(self.render_open());
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
        self.child_chat_observation = None;
        self.child_chat_open_shown = false;
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
        // Opening a child or going Back swaps the list (rendered here) for
        // the panel's chat view; repaint the shell only on that flip.
        self.child_chat_open_shown = false;
        self.child_chat_observation = Some(cx.observe(&panel, |shell, panel, cx| {
            let open = panel.read(cx).open_chat_id().is_some();
            if shell.child_chat_open_shown != open {
                shell.child_chat_open_shown = open;
                cx.notify();
            }
        }));
        self.child_chat_panel = Some(panel.clone());
        panel
    }

    /// Drill into one child in the pane (a list row's click).
    pub(super) fn open_child_chat_in_pane(&mut self, id: String, cx: &mut Context<Self>) {
        self.related_focus = super::related_chats::RelatedKind::Child;
        let panel = self.ensure_child_chat_panel(cx);
        panel.update(cx, |panel, cx| panel.open_child(id, cx));
        cx.notify();
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

    /// The current chat's non-archived child count (QA readback).
    #[cfg(feature = "appshots-fixture")]
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

    /// Open `parent`'s Related chats surface, selecting the parent first when
    /// needed (sidebar count chip). Pre-seeding the parent's panel state lets
    /// the chat switch restore straight into the list.
    pub(super) fn open_related_chats_for(&mut self, parent: String, cx: &mut Context<Self>) {
        if parent == self.active_chat {
            self.add_work_surface(RightSurface::RelatedChats, cx);
            return;
        }
        push_unique_right_surface(
            self.right_tabs.entry(parent.clone()).or_default(),
            RightSurface::RelatedChats,
        );
        self.panels.update(&parent, |panel| {
            panel.changes_open = true;
            panel.right_active = RightSurface::RelatedChats;
            panel.picker_open = false;
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
        self.add_work_surface(RightSurface::RelatedChats, cx);
    }

    pub fn fixture_open_child_chats_for(&mut self, parent: String, cx: &mut Context<Self>) {
        self.open_related_chats_for(parent, cx);
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
                    "model": row.model,
                    "detail": row.detail,
                    "children": row.children,
                }))
                .collect::<Vec<_>>(),
            "sidebarOrder": self.sidebar_visible_order(cx),
            "collapsedChildChats": self.settings.collapsed_child_chats,
        })
    }
}
