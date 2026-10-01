//! The right pane's shared chat view.
//!
//! Side chats and child chats open in the right pane as another main chat:
//! the main [`Transcript`] (tool rendering, markdown, history actions) bound
//! to the chat's doc, and the main [`Composer`] bound to the same chat id
//! (queue/steer/Stop, approvals and questions, goals, model/effort/access,
//! context usage, attachments, `@` and `/` pickers). A compact header carries
//! the chat's identity ("Side chat" / "Child chat"), its parent and the
//! navigation controls.
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
}

impl PaneIdentity {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Side => "Side chat",
            Self::Child => "Child chat",
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
    let border_strong = theme.border_strong;
    let hover = crate::theme::ink(0.06);
    let key_action = action.clone();
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
        .on_click(cx.listener(move |view, _, _, cx| action(view, cx)))
        .on_key_down(cx.listener(move |view, event: &gpui::KeyDownEvent, _, cx| {
            if is_settings_nav_activation(&event.keystroke.key, event.is_held) {
                key_action(view, cx);
                cx.stop_propagation();
            }
        }))
}

/// Compact identity header: Back · title · identity badge · Open as main,
/// then one quiet line with status, parent link and provider facts.
pub(super) fn render_pane_header<V: 'static>(
    chrome: &PaneChatChrome,
    actions: PaneHeaderActions<V>,
    theme: &Theme,
    cx: &mut Context<V>,
) -> gpui::Div {
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
    .tooltip_show_delay(Duration::from_millis(350))
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
        .text_size(crate::typography::ui_rems(10.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.readable_muted(0.75))
        .child(chrome.identity.label());
    let mut toolbar = crate::surface_chrome::toolbar(theme)
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
            .tooltip_show_delay(Duration::from_millis(350))
            .child(
                icon(icons::ARROW_UP_RIGHT)
                    .size(px(crate::surface_chrome::ICON_SIZE))
                    .text_color(muted),
            ),
        );
    }
    let parent_label: SharedString = format!("from {}", chrome.parent_title).into();
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
                        .text_color(theme.readable_muted(0.4))
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

/// Body below the header: notices, the transcript, then the composer slot
/// (the bound main composer, or a read-only strip).
pub(super) fn render_pane_body(
    notices: Vec<AnyElement>,
    transcript: AnyElement,
    loading: bool,
    composer: AnyElement,
    theme: &Theme,
) -> gpui::Div {
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .children(notices)
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
        )
        .child(composer)
}

pub(super) enum PaneChatEvent {
    Back,
    OpenAsMain(String),
    ShowParent(String),
    /// Side chats: stop work and seal the history (read-only afterwards).
    Unload,
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
    /// Owner-supplied status for chats without a session-derived one (a side
    /// chat's lease lifetime).
    status_override: Option<(SharedString, gpui::Hsla)>,
    _subscriptions: Vec<Subscription>,
}

/// Horizontal inset around the pane composer, matching the main column's
/// composer gutter at narrow widths.
const PANE_COMPOSER_INSET: f32 = 12.0;

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
        state.update(cx, |state, cx| {
            state.watch_pane_chat(chat_id.clone(), cx);
            // Reading it here is reading it: clear the unseen marker exactly
            // as opening it as the main chat would.
            state.mark_chat_seen(&chat_id, cx);
        });
        let transcript = cx.new(|cx| Transcript::for_pane_chat(state.clone(), chat_id.clone(), cx));
        let mut links = links;
        links.source_session = Some(chat_id.clone());
        transcript.update(cx, |transcript, _| {
            transcript.set_workspace_link_handler(links)
        });
        composer.update(cx, |composer, cx| composer.request_focus(cx));
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.observe(&composer, |_, _, cx| cx.notify()),
        ];
        let release_id = chat_id.clone();
        cx.on_release(move |view, cx| {
            view.state
                .update(cx, |state, _| state.unwatch_pane_chat(&release_id));
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
            status_override: None,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn set_status(
        &mut self,
        status: Option<(SharedString, gpui::Hsla)>,
        cx: &mut Context<Self>,
    ) {
        if self.status_override != status {
            self.status_override = status;
            cx.notify();
        }
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
        let parent_title = state
            .chats
            .iter()
            .find(|chat| chat.id == self.parent)
            .and_then(|chat| chat.title.clone())
            .map(|title| transcript::single_line(&title))
            .unwrap_or_else(|| "Parent chat".into());
        let title = match self.identity {
            // Named by its first question, like the leased side view: the
            // engine's row title only repeats "Side chat · <parent>".
            PaneIdentity::Side => ChatBinding::Chat(self.chat_id.clone())
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
            PaneIdentity::Child => chat
                .and_then(|chat| chat.title.clone())
                .map(|title| transcript::single_line(&title))
                .unwrap_or_else(|| self.identity.label().to_string()),
        };
        let theme = Theme::of(cx);
        let (status, archived) = match (self.identity, chat) {
            (PaneIdentity::Child, Some(chat)) => {
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
            _ => (
                self.status_override
                    .clone()
                    .map(|(label, color)| (label, color, false)),
                chat.is_some_and(|chat| chat.archived),
            ),
        };
        let meta = chat.and_then(|chat| chat.config.as_ref()).map(|config| {
            let provider = super::child_chats::provider_label(config.harness);
            let mut meta = match config.model.as_deref().filter(|model| !model.is_empty()) {
                Some(model) => format!("{provider} · {model}"),
                None => provider.to_string(),
            };
            if let Some(branch) = chat.and_then(|chat| chat.branch.as_deref()) {
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
        let theme = Theme::of(cx).clone();
        let (chrome, archived, loading) = self.chrome(cx);
        let open_id = self.chat_id.clone();
        let parent_id = self.parent.clone();
        let header = render_pane_header(
            &chrome,
            PaneHeaderActions {
                back_label: self.back_label,
                // (Side chats: Back parks the side chat; see SideChatPanel.)
                back: Rc::new(|_, cx| cx.emit(PaneChatEvent::Back)),
                // A side chat is never a main chat; a child can be promoted.
                open_main: (self.identity == PaneIdentity::Child).then(|| {
                    Rc::new(move |_: &mut Self, cx: &mut Context<Self>| {
                        cx.emit(PaneChatEvent::OpenAsMain(open_id.clone()))
                    }) as Rc<dyn Fn(&mut Self, &mut Context<Self>)>
                }),
                show_parent: Some(Rc::new(move |_, cx| {
                    cx.emit(PaneChatEvent::ShowParent(parent_id.clone()))
                })),
                extra: if self.identity == PaneIdentity::Side {
                    let border_strong = theme.border_strong;
                    let hover = crate::theme::ink(0.06);
                    vec![
                        div()
                            .id("pane-chat-unload")
                            .role(gpui::Role::Button)
                            .aria_label("Unload side chat: stop work and keep read-only history")
                            .tab_index(0)
                            .h(px(crate::surface_chrome::CONTROL_SIZE))
                            .px(px(8.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
                            .cursor_pointer()
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(theme.text_muted)
                            .hover(move |style| style.bg(hover))
                            .focus_visible(move |style| style.border_color(border_strong))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(PaneChatEvent::Unload)))
                            .child("Unload")
                            .into_any_element(),
                    ]
                } else {
                    Vec::new()
                },
            },
            &theme,
            cx,
        );
        let width = self.composer_width.get();
        if width > 0.0 {
            self.composer.update(cx, |composer, cx| {
                composer.set_available_width(width - 2.0 * PANE_COMPOSER_INSET, cx)
            });
        }
        let measured = self.composer_width.clone();
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
                .px(px(PANE_COMPOSER_INSET))
                .pt(px(4.0))
                .pb(px(PANE_COMPOSER_INSET))
                .child(
                    gpui::canvas(
                        move |bounds, window, _| {
                            let next = f32::from(bounds.size.width);
                            if (measured.get() - next).abs() > 0.5 {
                                measured.set(next);
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
        div()
            .id("pane-chat-view")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .child(header)
            .child(render_pane_body(
                Vec::new(),
                self.transcript.clone().into_any_element(),
                loading,
                composer_slot,
                &theme,
            ))
    }
}
