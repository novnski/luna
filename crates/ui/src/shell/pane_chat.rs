//! The right pane's shared chat view.
//!
//! Side chats and child chats open in the right pane as another main chat:
//! the main [`Transcript`] (tool rendering, markdown, history actions) bound
//! to the chat's doc, and the main [`Composer`] bound to the same chat id
//! (queue/steer/Stop, approvals and questions, goals, model/effort/access,
//! context usage, attachments, `@` and `/` pickers), with the main column's
//! gutter so both pills share one geometry. A child chat's compact header
//! carries its identity, status, parent and navigation; a side chat inherits
//! all of that from the main chat beside it, so its header is one row (Back,
//! title, Close) with the details in the title's tooltip.
//!
//! Composers are cached per chat in [`PaneComposers`], owned by the shell, so
//! drafts, pending submissions and their receipt watches belong to the chat,
//! not to whichever pane happens to show it: Back, closing the pane or
//! switching the main chat never drops a send in flight.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use super::*;
use crate::chat_binding::ChatBinding;
use crate::markdown::render::LinkUi;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaneIdentity {
    Side,
    Child,
    /// An ordinary chat the user listed beside the host chat: it has no
    /// parent, so the pane names its host plainly and links nowhere.
    Alongside,
}

impl PaneIdentity {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Side => "Side chat",
            Self::Child => "New thread",
            Self::Alongside => "Alongside",
        }
    }
}

/// Per-chat pane composers. Profile-local: a runtime replacement drops every
/// cached composer (each also guards its own writes by engine generation).
pub(super) struct PaneComposers {
    state: Entity<AppState>,
    generation: u64,
    composers: HashMap<String, Entity<Composer>>,
    /// Wakes observers (the shell's quit flow) when a pane draft flush lands.
    events: HashMap<String, Subscription>,
    /// Least recently opened first; bounds the idle composers kept alive.
    order: Vec<String>,
}

/// Idle cached pane composers beyond this are dropped (their drafts are
/// durable); composers with unsaved text or work in flight are always kept.
const PANE_COMPOSER_CACHE: usize = 8;

impl PaneComposers {
    pub(super) fn new(state: Entity<AppState>, cx: &App) -> Self {
        let generation = state.read(cx).engine_generation();
        Self {
            state,
            generation,
            composers: HashMap::new(),
            events: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn sync_generation(&mut self, cx: &App) {
        let generation = self.state.read(cx).engine_generation();
        if generation != self.generation {
            self.generation = generation;
            self.composers.clear();
            self.events.clear();
            self.order.clear();
        }
    }

    /// The cached composer bound to `chat_id`, created on first use.
    pub(super) fn composer(&mut self, chat_id: &str, cx: &mut Context<Self>) -> Entity<Composer> {
        self.sync_generation(cx);
        self.order.retain(|id| id != chat_id);
        self.order.push(chat_id.to_string());
        if let Some(composer) = self.composers.get(chat_id) {
            return composer.clone();
        }
        let state = self.state.clone();
        let binding = ChatBinding::Chat(chat_id.to_string());
        let composer = cx.new(|cx| Composer::new_bound(state, binding, cx));
        let events = cx.subscribe(&composer, |_, _, event, cx| {
            if matches!(event, ComposerEvent::DraftFlushCompleted) {
                cx.notify();
            }
        });
        self.events.insert(chat_id.to_string(), events);
        self.composers.insert(chat_id.to_string(), composer.clone());
        self.evict_idle(chat_id, cx);
        composer
    }

    fn evict_idle(&mut self, keep: &str, cx: &App) {
        while self.composers.len() > PANE_COMPOSER_CACHE {
            let victim = self.order.iter().find(|id| {
                id.as_str() != keep
                    && self.composers.get(*id).is_some_and(|composer| {
                        let composer = composer.read(cx);
                        composer.draft_flush_complete() && !composer.has_work_in_flight()
                    })
            });
            let Some(victim) = victim.cloned() else {
                return;
            };
            self.remove(&victim);
        }
    }

    pub(super) fn get(&self, chat_id: &str) -> Option<Entity<Composer>> {
        self.composers.get(chat_id).cloned()
    }

    pub(super) fn remove(&mut self, chat_id: &str) -> Option<Entity<Composer>> {
        self.events.remove(chat_id);
        self.order.retain(|id| id != chat_id);
        self.composers.remove(chat_id)
    }

    /// Flush every cached pane draft before quitting. `true` when all are
    /// already durable; otherwise each dirty composer emits
    /// `ComposerEvent::DraftFlushCompleted` when its save settles.
    pub(super) fn prepare_close(&mut self, cx: &mut Context<Self>) -> bool {
        let mut ready = true;
        for composer in self.composers.values() {
            ready &= composer.update(cx, |composer, cx| composer.prepare_close(cx));
        }
        ready
    }

    pub(super) fn flush_complete(&self, cx: &App) -> bool {
        self.composers
            .values()
            .all(|composer| composer.read(cx).draft_flush_complete())
    }

    #[cfg(feature = "appshots-fixture")]
    pub(super) fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.composers.keys().cloned().collect();
        ids.sort();
        ids
    }
}

/// What the header shows. Built by the owning panel each frame.
pub(super) struct PaneChatChrome {
    pub identity: PaneIdentity,
    pub title: SharedString,
    pub parent_title: SharedString,
    /// Status word and its colour (child status, side chat lifetime).
    pub status: Option<(SharedString, gpui::Hsla, bool)>,
    /// Provider/model and similar quiet facts.
    pub meta: Option<SharedString>,
}

type PaneAction<V> = Rc<dyn Fn(&mut V, &mut Context<V>)>;

/// Navigation affordances of the header; `None` hides a control.
pub(super) struct PaneHeaderActions<V> {
    pub back_label: &'static str,
    pub back: PaneAction<V>,
    pub open_main: Option<PaneAction<V>>,
    pub show_parent: Option<PaneAction<V>>,
    /// Extra trailing controls (e.g. side chat Unload).
    pub extra: Vec<AnyElement>,
}

fn header_control<V: 'static>(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    theme: &Theme,
    action: PaneAction<V>,
    cx: &mut Context<V>,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    let id: SharedString = id.into();
    div()
        .id(id.clone())
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
        .hover_wash(id)
        .focus_visible({
            let ring = theme.focus_ring();
            move |style| crate::theme::focus_ring(style, ring)
        })
        .on_click(cx.listener(move |view, _, _, cx| action(view, cx)))
        .on_key_down(crate::swallow_activation_key)
}

/// Compact identity header: Back · title · identity badge · Open as main,
/// then one quiet line with status, parent link and provider facts. Side
/// chats get the single-row [`render_side_header`] instead.
pub(super) fn render_pane_header<V: 'static>(
    chrome: &PaneChatChrome,
    actions: PaneHeaderActions<V>,
    theme: &Theme,
    cx: &mut Context<V>,
) -> gpui::Div {
    if chrome.identity == PaneIdentity::Side {
        return render_side_header(chrome, actions, theme, cx);
    }
    let muted = theme.text_muted;
    let back_tooltip: SharedString =
        format!("Back to {} (Esc)", actions.back_label.to_lowercase()).into();
    let back = header_control(
        "pane-chat-back",
        format!("Back to {}", actions.back_label.to_lowercase()),
        theme,
        actions.back,
        cx,
    )
    .pl(px(4.0))
    .pr(px(8.0))
    .text_color(muted)
    .tooltip(move |_, cx| {
        cx.new(|_| SurfaceTabTooltip {
            text: back_tooltip.clone(),
        })
        .into()
    })
    .tooltip_show_delay(crate::popover::TOOLTIP_DELAY)
    .child(icon(icons::ALT_ARROW_LEFT).size(px(14.0)).text_color(muted))
    .child(actions.back_label);
    let badge = div()
        .id("pane-chat-identity")
        .role(gpui::Role::Label)
        .aria_label(chrome.identity.label())
        .flex_none()
        .px(px(6.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.border)
        .bg(crate::theme::ink(0.04))
        .text_size(crate::typography::ui_rems(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.readable_muted(0.75))
        .child(chrome.identity.label());
    let mut toolbar = crate::surface_chrome::toolbar(theme)
        .child(back)
        .child(
            div()
                .flex_none()
                .text_color(theme.decorative_muted(0.4))
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
                .child(chrome.title.clone()),
        )
        .child(badge)
        .children(actions.extra);
    if let Some(open_main) = actions.open_main {
        toolbar = toolbar.child(
            header_control(
                "pane-chat-open-main",
                "Open as main chat",
                theme,
                open_main,
                cx,
            )
            .w(px(crate::surface_chrome::CONTROL_SIZE))
            .justify_center()
            .tooltip(|_, cx| {
                cx.new(|_| SurfaceTabTooltip {
                    text: "Open as main chat".into(),
                })
                .into()
            })
            .tooltip_show_delay(crate::popover::TOOLTIP_DELAY)
            .child(
                icon(icons::ARROW_UP_RIGHT)
                    .size(px(crate::surface_chrome::ICON_SIZE))
                    .text_color(muted),
            ),
        );
    }
    let parent_label: SharedString = match chrome.identity {
        PaneIdentity::Alongside => format!("beside {}", chrome.parent_title).into(),
        _ => format!("from {}", chrome.parent_title).into(),
    };
    let parent = match actions.show_parent {
        Some(show_parent) => header_control(
            "pane-chat-parent",
            format!("Parent chat: {}", chrome.parent_title),
            theme,
            show_parent,
            cx,
        )
        .h(px(18.0))
        .px(px(4.0))
        .min_w_0()
        .text_color(theme.readable_muted(0.7))
        .child(
            icon(icons::ARROW_UP_RIGHT)
                .size(px(11.0))
                .flex_none()
                .text_color(theme.readable_muted(0.6)),
        )
        .child(div().min_w_0().truncate().child(parent_label))
        .into_any_element(),
        None => div()
            .min_w_0()
            .truncate()
            .text_color(theme.readable_muted(0.7))
            .child(parent_label)
            .into_any_element(),
    };
    let strip = div()
        .w_full()
        .flex_none()
        .px(px(12.0))
        .py(px(5.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .border_b_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(11.5))
        .min_w_0()
        .when_some(chrome.status.clone(), |el, (label, color, _)| {
            el.child(div().size(px(6.0)).flex_none().rounded_full().bg(color))
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(color)
                        .child(label),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.decorative_muted(0.4))
                        .child("·"),
                )
        })
        .child(div().min_w_0().child(parent))
        .when_some(chrome.meta.clone(), |el, meta| {
            el.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.readable_muted(0.6))
                    .child(format!("· {meta}")),
            )
        });
    div()
        .w_full()
        .flex_none()
        .flex()
        .flex_col()
        .child(toolbar)
        .child(strip)
}

/// A side chat inherits its checkout, access and provider from the main chat
/// beside it, so its header is one quiet row (Codex's side tab): Back to the
/// list, the chat's name, then the trailing controls (Close). The lease
/// lifetime, parent and provider facts live in the title's tooltip, and the
/// title reveals where the chat started. Only a state other than the live
/// lease ("Writable · N min left") — saving, read-only — stays visible.
fn render_side_header<V: 'static>(
    chrome: &PaneChatChrome,
    actions: PaneHeaderActions<V>,
    theme: &Theme,
    cx: &mut Context<V>,
) -> gpui::Div {
    let muted = theme.text_muted;
    let back = header_control(
        "pane-chat-back",
        format!("Back to {}", actions.back_label.to_lowercase()),
        theme,
        actions.back,
        cx,
    )
    .w(px(crate::surface_chrome::CONTROL_SIZE))
    .justify_center()
    .tooltip(move |_, cx| {
        crate::pickers::ControlTooltip::view("All related chats", Some("Esc".into()), cx)
    })
    .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
    .child(
        icon(icons::ALT_ARROW_LEFT)
            .size(px(crate::surface_chrome::ICON_SIZE))
            .text_color(muted),
    );
    let live = chrome
        .status
        .as_ref()
        .is_none_or(|(label, _, _)| label.starts_with("Writable") || label.as_ref() == "Active");
    let facts: Vec<String> = chrome
        .status
        .iter()
        .map(|(label, _, _)| label.to_string())
        .chain(std::iter::once(format!("From {}", chrome.parent_title)))
        .chain(chrome.meta.iter().map(|meta| meta.to_string()))
        .collect();
    let tooltip_title = chrome.title.clone();
    let tooltip_detail: SharedString = facts.join(" · ").into();
    let title_text = div()
        .min_w_0()
        .truncate()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text)
        .child(chrome.title.clone());
    let title = match actions.show_parent {
        Some(show_parent) => header_control(
            "pane-chat-title",
            format!("{}: show where it started", chrome.title),
            theme,
            show_parent,
            cx,
        )
        .flex_1()
        .min_w_0()
        .px(px(6.0))
        .child(title_text)
        .tooltip(move |_, cx| {
            crate::pickers::ControlTooltip::view(
                tooltip_title.clone(),
                Some(format!("{tooltip_detail}. Click to show where it started.").into()),
                cx,
            )
        })
        .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
        .into_any_element(),
        None => div()
            .id("pane-chat-title")
            .flex_1()
            .min_w_0()
            .px(px(6.0))
            .child(title_text)
            .tooltip(move |_, cx| {
                crate::pickers::ControlTooltip::view(
                    tooltip_title.clone(),
                    Some(tooltip_detail.clone()),
                    cx,
                )
            })
            .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
            .into_any_element(),
    };
    crate::surface_chrome::toolbar(theme)
        .child(back)
        .child(title)
        .when_some(
            chrome.status.clone().filter(|_| !live),
            |row, (label, color, _)| {
                row.child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(color)
                        .child(label),
                )
            },
        )
        .children(actions.extra)
}

pub(super) enum PaneChatEvent {
    Back,
    OpenAsMain(String),
    ShowParent(String),
}

impl gpui::EventEmitter<PaneChatEvent> for PaneChatView {}

/// One full chat in the right pane: the main transcript and the main
/// composer, both bound to `chat_id`.
pub(super) struct PaneChatView {
    state: Entity<AppState>,
    chat_id: String,
    identity: PaneIdentity,
    parent: String,
    back_label: &'static str,
    transcript: Entity<Transcript>,
    composer: Entity<Composer>,
    /// Composer slot width measured at paint, fed back as the composer's
    /// available width so its chips adapt to the pane.
    composer_width: Rc<Cell<f32>>,
    /// Height of the composer stack (approval card, goal bar, queue, pill)
    /// overlaying the transcript bottom; the transcript clears it.
    composer_height: Rc<Cell<f32>>,
    /// The pane width and stack height the transcript last laid out for.
    last_layout: (f32, f32),
    /// A workbench tab: the tab names the chat, so the view keeps only a
    /// slim context strip (parent, status, provider) above the transcript.
    embedded: bool,
    _subscriptions: Vec<Subscription>,
}

impl PaneChatView {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        state: Entity<AppState>,
        chat_id: String,
        identity: PaneIdentity,
        parent: String,
        back_label: &'static str,
        composer: Entity<Composer>,
        links: LinkUi,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(
            state, chat_id, identity, parent, back_label, composer, links, false, true, cx,
        )
    }

    /// A workbench tab's view. Creating it never moves keyboard focus: a
    /// restored layout mounts several at once; the shell focuses one.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_embedded(
        state: Entity<AppState>,
        chat_id: String,
        identity: PaneIdentity,
        parent: String,
        composer: Entity<Composer>,
        links: LinkUi,
        cx: &mut Context<Self>,
    ) -> Self {
        composer.update(cx, |composer, cx| composer.set_frame_footer(true, cx));
        Self::build(
            state,
            chat_id,
            identity,
            parent,
            "Workbench",
            composer,
            links,
            true,
            false,
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        state: Entity<AppState>,
        chat_id: String,
        identity: PaneIdentity,
        parent: String,
        back_label: &'static str,
        composer: Entity<Composer>,
        links: LinkUi,
        embedded: bool,
        focus: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = state.update(cx, |state, cx| {
            let owner = state.watch_pane_chat(chat_id.clone(), cx);
            // Reading it here is reading it: clear the unseen marker exactly
            // as opening it as the main chat would.
            state.mark_chat_seen(&chat_id, cx);
            owner
        });
        let transcript = cx.new(|cx| Transcript::for_pane_chat(state.clone(), chat_id.clone(), cx));
        let mut links = links;
        links.source_session = Some(chat_id.clone());
        transcript.update(cx, |transcript, _| {
            transcript.set_workspace_link_handler(links)
        });
        if focus {
            composer.update(cx, |composer, cx| composer.request_focus(cx));
        }
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.observe(&composer, |_, _, cx| cx.notify()),
        ];
        let release_id = chat_id.clone();
        // Deferred until the view drops: release the watch only if it is
        // still this view's (a side chat leaving this view for its saved
        // history releases it first and installs that history in its place).
        cx.on_release(move |view, cx| {
            view.state
                .update(cx, |state, _| state.release_pane_chat(&release_id, owner));
        })
        .detach();
        Self {
            state,
            chat_id,
            identity,
            parent,
            back_label,
            transcript,
            composer,
            composer_width: Rc::new(Cell::new(0.0)),
            composer_height: Rc::new(Cell::new(0.0)),
            last_layout: (0.0, 0.0),
            embedded,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn chat_id(&self) -> &str {
        &self.chat_id
    }

    /// The composer slot's measured width (its controls lay out within it).
    pub(super) fn composer_slot_width(&self) -> f32 {
        self.composer_width.get()
    }

    /// The identity can change under a live view (a reopened chat, a child
    /// whose row arrived after the tab opened); the view keeps its watch.
    pub(super) fn set_identity(
        &mut self,
        identity: PaneIdentity,
        parent: String,
        cx: &mut Context<Self>,
    ) {
        if self.identity != identity || self.parent != parent {
            self.identity = identity;
            self.parent = parent;
            cx.notify();
        }
    }

    #[cfg(feature = "appshots-fixture")]
    pub(super) fn identity(&self) -> PaneIdentity {
        self.identity
    }

    pub(super) fn transcript(&self) -> &Entity<Transcript> {
        &self.transcript
    }

    pub(super) fn composer(&self) -> &Entity<Composer> {
        &self.composer
    }

    fn chrome(&self, cx: &App) -> (PaneChatChrome, bool, bool) {
        let state = self.state.read(cx);
        let now = Utc::now();
        let chat = state.chats.iter().find(|chat| chat.id == self.chat_id);
        let parent_row = state.chats.iter().find(|chat| chat.id == self.parent);
        let parent_title = parent_row
            .and_then(|chat| chat.title.clone())
            .map(|title| transcript::single_line(&title))
            .unwrap_or_else(|| match self.identity {
                // The source is a link, not a lifetime: a fork outlives it.
                PaneIdentity::Side => "a deleted chat".into(),
                _ => "Parent chat".into(),
            });
        let own_title = chat
            .and_then(|chat| chat.title.clone())
            .map(|title| transcript::single_line(&title))
            .filter(|title| !title.trim().is_empty());
        let title = match (self.identity, own_title) {
            (_, Some(title)) => title,
            // An untitled fork is named by its first question.
            (PaneIdentity::Side, None) => ChatBinding::Chat(self.chat_id.clone())
                .transcript(state)
                .iter()
                .find(|entry| entry.role == luna_doc::MessageRole::User)
                .and_then(|entry| {
                    entry.parts.iter().find_map(|part| match part {
                        luna_doc::MessagePart::Text { text, .. } => {
                            Some(transcript::single_line(text))
                        }
                        _ => None,
                    })
                })
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| "New side chat".into()),
            (identity, None) => identity.label().to_string(),
        };
        let theme = Theme::of(cx);
        let (status, archived) = match (self.identity, chat) {
            (_, Some(chat)) => {
                let status = super::child_chats::child_chat_status(state, chat, now);
                (
                    Some((
                        SharedString::from(status.label()),
                        status.color(theme),
                        status == super::child_chats::ChildChatStatus::Running,
                    )),
                    chat.archived,
                )
            }
            (_, None) => (None, false),
        };
        let meta = chat.and_then(|chat| chat.config.as_ref()).map(|config| {
            let provider = super::child_chats::provider_label(config.harness);
            let mut meta = match config.model.as_deref().filter(|model| !model.is_empty()) {
                Some(model) => format!("{provider} · {model}"),
                None => provider.to_string(),
            };
            // A workbench tab's checkout and branch live in the frame's
            // bottom row while it is focused.
            if !self.embedded
                && let Some(branch) = chat.and_then(|chat| chat.branch.as_deref())
            {
                meta.push_str(&format!(" · {branch}"));
            }
            SharedString::from(meta)
        });
        let loading = !ChatBinding::Chat(self.chat_id.clone()).transcript_replayed(state)
            && state.sub_transcript(&self.chat_id).is_empty();
        (
            PaneChatChrome {
                identity: self.identity,
                title: title.into(),
                parent_title: parent_title.into(),
                status,
                meta,
            },
            archived,
            loading,
        )
    }
}

impl Render for PaneChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Opt-in baseline counters (slice 6): observe only.
        let _perf =
            crate::perf_counters::Timer::start("pane.render", || format!("pane:{}", self.chat_id));
        let theme = Theme::of(cx).clone();
        let (chrome, archived, loading) = self.chrome(cx);
        let open_id = self.chat_id.clone();
        // A deleted source is named, never linked.
        let parent_id = if self
            .state
            .read(cx)
            .chats
            .iter()
            .any(|chat| chat.id == self.parent)
        {
            self.parent.clone()
        } else {
            String::new()
        };
        // A workbench tab has no context row: its tab carries the identity
        // (role glyph, tooltip, context menu) and live status, so the chat
        // starts right under the tabs.
        let header = if self.embedded {
            div()
        } else {
            self.render_full_header(&chrome, open_id, parent_id, &theme, cx)
        };
        self.render_with_header(header, archived, loading, &theme, cx)
    }
}

impl PaneChatView {
    fn render_full_header(
        &mut self,
        chrome: &PaneChatChrome,
        open_id: String,
        parent_id: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        render_pane_header(
            chrome,
            PaneHeaderActions {
                back_label: self.back_label,
                back: Rc::new(|_, cx| cx.emit(PaneChatEvent::Back)),
                // Any thread, a fork included, can become the main chat.
                open_main: Some(Rc::new(move |_: &mut Self, cx: &mut Context<Self>| {
                    cx.emit(PaneChatEvent::OpenAsMain(open_id.clone()))
                })
                    as Rc<dyn Fn(&mut Self, &mut Context<Self>)>),
                // An alongside chat has no parent to show (its host is named
                // as text only), nor does a fork whose source was deleted.
                show_parent: (self.identity != PaneIdentity::Alongside && !parent_id.is_empty())
                    .then(|| {
                        Rc::new(move |_: &mut Self, cx: &mut Context<Self>| {
                            cx.emit(PaneChatEvent::ShowParent(parent_id.clone()))
                        }) as Rc<dyn Fn(&mut Self, &mut Context<Self>)>
                    }),
                extra: Vec::new(),
            },
            theme,
            cx,
        )
    }

    fn render_with_header(
        &mut self,
        header: gpui::Div,
        archived: bool,
        loading: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let width = self.composer_width.get();
        if width > 0.0 {
            self.composer.update(cx, |composer, cx| {
                composer.set_available_width(width.min(crate::composer::COMPOSER_MAX_WIDTH), cx)
            });
        }
        // The main view's layout: the transcript spans the body and the
        // composer stack floats over its bottom, so the transcript's last row
        // clears the measured stack height (approval card included).
        let clearance = if archived {
            0.0
        } else {
            self.composer_height.get()
        };
        let layout = (width, clearance);
        let relayout = (self.last_layout.0 - layout.0).abs() > 0.5
            || (self.last_layout.1 - layout.1).abs() > 0.5;
        self.last_layout = layout;
        self.transcript.update(cx, |transcript, cx| {
            transcript.set_bottom_clearance(clearance, cx);
            // Rewrapping at a new width or a taller stack (approval card)
            // must not leave a pinned view short of its latest message.
            if relayout {
                transcript.snap_to_end_if_pinned(cx);
            }
        });
        let measured = self.composer_width.clone();
        let measured_height = self.composer_height.clone();
        let composer_slot = if archived {
            div()
                .id("pane-chat-read-only")
                .w_full()
                .flex_none()
                .px(px(16.0))
                .py(px(12.0))
                .border_t_1()
                .border_color(theme.border)
                .text_color(theme.text_muted)
                .child("Archived · read-only. Unarchive it to reply.")
                .into_any_element()
        } else {
            div()
                .id("pane-chat-composer")
                .w_full()
                .flex_none()
                .relative()
                // No inset of its own: the composer carries the main
                // column's 16px gutter and bottom margin, so the pane's pill
                // and footer sit exactly level with the main composer's.
                .child(
                    gpui::canvas(
                        move |bounds, window, _| {
                            let next = f32::from(bounds.size.width);
                            let height = f32::from(bounds.size.height);
                            if (measured.get() - next).abs() > 0.5
                                || (measured_height.get() - height).abs() > 0.5
                            {
                                measured.set(next);
                                measured_height.set(height);
                                crate::perf_counters::count("frame.request", "pane_chat#1", 1, 0);
                                window.request_animation_frame();
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .child(self.composer.clone())
                .into_any_element()
        };
        let body = div()
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .child(self.transcript.clone())
            .when(loading, |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(16.0))
                        .left(px(16.0))
                        .text_color(theme.text_muted)
                        .child("Loading messages…"),
                )
            });
        let body = if archived {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .flex_col()
                .child(body)
                .child(composer_slot)
        } else {
            body.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .child(composer_slot),
            )
        };
        // No own background: the pane sits on the shell frost like the main
        // conversation column.
        div()
            .id("pane-chat-view")
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.text)
            // Drops are owned by the pointer: whatever lands on this pane is
            // staged in THIS pane's composer (never Main's, never by keyboard
            // focus), with Main's dropzone semantics. Archived panes have no
            // composer and accept nothing.
            .when(!archived, |pane| {
                pane.on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                    let paths = paths.paths().to_vec();
                    // Opt-in evidence (slice 6): this pane's handler fired.
                    crate::perf_counters::count("drop.external", "pane", paths.len() as u64, 0);
                    this.composer
                        .update(cx, |composer, cx| composer.add_paths(paths, cx));
                    cx.stop_propagation();
                    cx.notify();
                }))
                .on_drop::<crate::files::WorkspacePathDrag>(cx.listener(
                    |this, payload: &crate::files::WorkspacePathDrag, window, cx| {
                        this.composer.update(cx, |composer, cx| {
                            composer.add_workspace_path(
                                &payload.path,
                                payload.is_directory,
                                window,
                                cx,
                            )
                        });
                        cx.stop_propagation();
                        cx.notify();
                    },
                ))
                .on_drop::<super::RightTabDrag>(cx.listener(
                    |this, payload: &super::RightTabDrag, window, cx| {
                        if let Some(path) = &payload.workspace_path {
                            this.composer.update(cx, |composer, cx| {
                                composer.add_workspace_path(
                                    &path.path,
                                    path.is_directory,
                                    window,
                                    cx,
                                )
                            });
                        }
                        cx.stop_propagation();
                        cx.notify();
                    },
                ))
            })
            .child(header)
            .child(body)
    }
}

#[cfg(feature = "appshots-fixture")]
impl PaneChatView {
    /// The view's identity facts (QA): its title, the parent label and
    /// whether it links, its status word and provider facts (in a workbench
    /// tab these live on the tab and its tooltip, not in a row). A fork's
    /// are an ordinary thread's: no End, lease, expiry or read-only state.
    pub(super) fn fixture_strip(&self, cx: &App) -> serde_json::Value {
        let (chrome, archived, loading) = self.chrome(cx);
        let linked = self
            .state
            .read(cx)
            .chats
            .iter()
            .any(|chat| chat.id == self.parent);
        serde_json::json!({
            "identity": chrome.identity.label(),
            "title": chrome.title.to_string(),
            "parentTitle": chrome.parent_title.to_string(),
            "parentLinked": linked && self.identity != PaneIdentity::Alongside,
            "status": chrome.status.as_ref().map(|(label, _, _)| label.to_string()),
            "meta": chrome.meta.as_ref().map(|meta| meta.to_string()),
            "archived": archived,
            "loading": loading,
            // A workbench tab renders no context row (identity is on the tab).
            "metaRow": !self.embedded,
        })
    }
}

#[cfg(feature = "appshots-fixture")]
impl Shell {
    /// Composer-parity fixture: the main composer and the cached pane
    /// composer bound to `pane_chat`, when one exists.
    pub fn fixture_composers(
        &self,
        pane_chat: Option<&str>,
        cx: &App,
    ) -> (Entity<Composer>, Option<Entity<Composer>>) {
        let pane = pane_chat.and_then(|id| self.pane_composers.read(cx).get(id));
        (self.composer.clone(), pane)
    }

    /// The main and pane composers' readbacks (geometry, chips, controls row).
    pub fn fixture_composer_readbacks(
        &self,
        pane_chat: &str,
        cx: &App,
    ) -> (serde_json::Value, serde_json::Value) {
        let (main, pane) = self.fixture_composers(Some(pane_chat), cx);
        (
            main.read(cx).readback(cx),
            pane.map(|pane| pane.read(cx).readback(cx))
                .unwrap_or(serde_json::Value::Null),
        )
    }

    /// A queued Steer row's decision in the Main and pane composers.
    pub fn fixture_steer_resolutions(
        &self,
        pane_chat: &str,
        cx: &App,
    ) -> (serde_json::Value, serde_json::Value) {
        let (main, pane) = self.fixture_composers(Some(pane_chat), cx);
        (
            main.read(cx).fixture_steer_resolution(cx),
            pane.map(|pane| pane.read(cx).fixture_steer_resolution(cx))
                .unwrap_or(serde_json::Value::Null),
        )
    }

    /// Seed (or with `None` hide) a task's execution summary in place of
    /// its engine's, through the accessor every view reads.
    pub fn fixture_seed_execution(
        &mut self,
        task_id: &str,
        summary: Option<luna_proto::TaskExecutionSummary>,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            state.execution_fixture.insert(task_id.to_string(), summary);
            cx.notify();
        });
        cx.notify();
    }

    /// Stop overriding a task's summary (the engine's own applies again).
    pub fn fixture_clear_execution(&mut self, task_id: &str, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.execution_fixture.remove(task_id);
            cx.notify();
        });
        cx.notify();
    }

    /// The engine's own (watch-decoded) summary for `task_id`, ignoring QA
    /// overrides.
    pub fn fixture_watch_execution(&self, task_id: &str, cx: &App) -> serde_json::Value {
        self.state
            .read(cx)
            .execution_rows
            .get(task_id)
            .and_then(|summary| serde_json::to_value(summary).ok())
            .unwrap_or(serde_json::Value::Null)
    }

    /// The chat diagnostics follow right now.
    pub fn fixture_execution_focus(&self, cx: &App) -> Option<String> {
        self.state.read(cx).execution_focus_chat()
    }

    /// Settings → Providers with Advanced (the Execution card) open.
    pub fn fixture_open_execution_card(&mut self, cx: &mut Context<Self>) {
        self.open_settings(SettingsSection::Providers, cx);
        if let Some(page) = self.accounts_page.clone() {
            page.update(cx, |page, cx| page.fixture_open_advanced(cx));
        }
        cx.notify();
    }

    /// The Providers page's scroll viewport and offset, and the Execution
    /// card's and its summary's painted bounds (window coordinates).
    pub fn fixture_execution_geometry(&self, cx: &App) -> serde_json::Value {
        let rect = |bounds: gpui::Bounds<gpui::Pixels>| {
            serde_json::json!({
                "x": f32::from(bounds.origin.x),
                "y": f32::from(bounds.origin.y),
                "w": f32::from(bounds.size.width),
                "h": f32::from(bounds.size.height),
            })
        };
        let Some(page) = self.accounts_page.as_ref() else {
            return serde_json::Value::Null;
        };
        let (viewport, offset, max_offset) = page.read(cx).fixture_scroll_state();
        let measured = page
            .read(cx)
            .fixture_advanced_page()
            .map(|advanced| advanced.read(cx).fixture_execution_bounds())
            .unwrap_or_default();
        serde_json::json!({
            "viewport": rect(viewport),
            "offset": offset,
            "maxOffset": max_offset,
            "card": measured.get("card").copied().map(rect),
            "summary": measured.get("summary").copied().map(rect),
        })
    }

    /// Scroll the Providers page's real scroll container down by `delta`.
    pub fn fixture_scroll_providers(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(page) = self.accounts_page.clone() {
            page.update(cx, |page, cx| page.fixture_scroll_by(delta, cx));
        }
        cx.notify();
    }

    /// The Execution card as rendered (`Null` until its page exists).
    pub fn fixture_execution_card(&self, cx: &App) -> serde_json::Value {
        self.accounts_page
            .as_ref()
            .and_then(|page| page.read(cx).fixture_advanced_page())
            .map(|page| page.read(cx).fixture_execution_readback(cx))
            .unwrap_or(serde_json::Value::Null)
    }

    /// The engine connection generation (bumped on every attach, reconnect
    /// and profile replacement).
    pub fn fixture_engine_generation(&self, cx: &App) -> u64 {
        self.state.read(cx).engine_generation()
    }

    /// Re-attach the same engine as a reconnect would: a new connection
    /// generation, and execution observations dropped until the watch
    /// republishes them.
    pub fn fixture_reconnect_engine(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if let Some(engine) = state.engine().cloned() {
                state.set_test_engine(engine);
            }
            cx.notify();
        });
        cx.notify();
    }

    /// The chats watch ended (as on an engine restart): through the same
    /// invalidation the watch runs.
    pub fn fixture_end_chats_watch(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.invalidate_execution_observations();
            cx.notify();
        });
        cx.notify();
    }

    /// Deliver a contract response for `task_id`, requested under engine
    /// connection `generation`, to the Execution card as a late RPC
    /// completion would (refused for another focus or connection).
    pub fn fixture_deliver_execution_contract(
        &mut self,
        task_id: &str,
        generation: u64,
        view: serde_json::Value,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(page) = self
            .accounts_page
            .as_ref()
            .and_then(|page| page.read(cx).fixture_advanced_page())
        else {
            return false;
        };
        page.update(cx, |page, cx| {
            page.fixture_deliver_contract(task_id, generation, view, cx)
        })
    }
}
