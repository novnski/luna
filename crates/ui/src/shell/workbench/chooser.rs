//! The group's `+` creation chooser.
//!
//! `+` never creates anything by itself: it opens this anchored card, which
//! names the actual source (the active chat of the group it was opened
//! from), offers the two kinds of conversation and, for a New thread, the
//! providers this device offers. Only the explicit "Create new thread" /
//! "Create side chat" action creates, through the existing safe creation
//! paths (one idempotency key per New thread gesture; the side chat
//! creator's request id). A chooser whose source, Main, engine or layout
//! generation changed while it was open closes without creating.
//!
//! - **New thread**: a fresh conversation from the source. Its context is
//!   not carried over; it may run on any offered provider (with that
//!   provider's own default account).
//! - **Side chat**: carries the source's context, captured now. It stays on
//!   the source's provider and account. A provider without native history
//!   forks, or a source with no provider history yet, is said so before
//!   creating; the recent-visible-messages path is a separate, explicitly
//!   labeled action, offered only when visible messages exist.

use super::*;
use luna_proto::{HarnessId, HistoryCapabilityView, ProviderForkCapability};

/// Which conversation the chooser creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::shell) enum CreationKind {
    NewThread,
    SideChat,
}

/// What the chooser's one primary action does right now. The button, Enter
/// and the create handler all use this single decision, so nothing is ever
/// created that the card does not visibly offer.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::shell) enum ChooserAction {
    /// Nothing can be created yet; the reason is shown.
    Disabled(SharedString),
    NewThread(HarnessId),
    /// A native side chat (the source's provider history).
    SideNative,
    /// The explicitly labeled recent-visible-messages side chat
    /// (`forceSnapshot`).
    SideRecent,
    /// An unknown outcome: check the same request (`snapshot` as sent).
    CheckStatus {
        snapshot: bool,
    },
}

impl ChooserAction {
    pub(in crate::shell) fn label(&self) -> &'static str {
        match self {
            Self::Disabled(_) => "Create",
            Self::NewThread(_) => "Create new thread",
            Self::SideNative => "Create side chat",
            Self::SideRecent => "Create side chat (recent messages)",
            Self::CheckStatus { .. } => "Check status",
        }
    }
}

pub(in crate::shell) struct CreationChooser {
    pub(super) pane_id: String,
    /// The source chat, fixed when the chooser opened.
    pub(super) source: String,
    main: String,
    generation: u64,
    engine_generation: u64,
    pub(super) kind: CreationKind,
    /// The New thread provider (`None` until the catalog offers one).
    pub(super) provider: Option<HarnessId>,
    /// The source's native-history preview (a preview, not permission:
    /// creation errors stay authoritative).
    capability: Option<Result<HistoryCapabilityView, String>>,
    _capability_task: Option<Task<()>>,
    anchor: Point<Pixels>,
    pub(super) focus: FocusHandle,
    focus_pending: bool,
    /// A side chat was asked for: the creator now owns it; the chooser shows
    /// its progress or failure (with Check status / recent context).
    side_requested: bool,
}

impl Shell {
    /// Open the chooser for `pane_id`'s `+` (or the palette / shortcut for
    /// the focused group). Nothing is created here.
    pub(in crate::shell) fn workbench_open_chooser(
        &mut self,
        pane_id: Option<String>,
        kind: CreationKind,
        cx: &mut Context<Self>,
    ) {
        if !self.workbench_ready() {
            self.workbench_unavailable_notice(cx);
            return;
        }
        let layout = self.workbench.layout();
        let pane_id = pane_id.unwrap_or_else(|| layout.focused_pane_id.clone());
        if model::pane(&layout, &pane_id).is_none() {
            return;
        }
        let source = self.workbench_group_source(&pane_id);
        let anchor = self
            .workbench
            .geometry
            .borrow()
            .plus
            .get(&pane_id)
            .map(|bounds| gpui::point(bounds.left(), bounds.bottom() + px(4.0)))
            .unwrap_or_else(|| gpui::point(px(240.0), px(80.0)));
        let provider = self.chooser_default_provider(&source, cx);
        let capability_task = self.chooser_capability_task(source.clone(), cx);
        self.workbench.chooser = Some(CreationChooser {
            pane_id,
            source,
            main: self.workbench.main.clone(),
            generation: self.workbench.generation,
            engine_generation: self.state.read(cx).engine_generation(),
            kind,
            provider,
            capability: None,
            _capability_task: capability_task,
            anchor,
            focus: cx.focus_handle(),
            focus_pending: true,
            side_requested: false,
        });
        cx.notify();
    }

    pub(in crate::shell) fn workbench_close_chooser(&mut self, cx: &mut Context<Self>) {
        if let Some(chooser) = self.workbench.chooser.take() {
            self.workbench_focus_after_chooser(&chooser.pane_id, &chooser.source, cx);
            cx.notify();
        }
    }

    /// Close a chooser whose source went stale, creating nothing. Only when
    /// the card still held the keyboard does it move: to the focused
    /// group's chat (never stolen from where the user went instead).
    fn chooser_close_stale(&mut self, card_focused: bool, cx: &mut Context<Self>) {
        if let Some(chooser) = self.workbench.chooser.take() {
            if card_focused {
                let focused = self.workbench.layout().focused_pane_id.clone();
                self.workbench_focus_group(&focused, Some(&chooser.source), cx);
            }
            cx.notify();
        }
    }

    /// Return the keyboard to where the chooser was opened from: its source
    /// chat while that is still its group's active chat, else that group's
    /// current chat (or the group itself), else the focused group.
    fn workbench_focus_after_chooser(
        &mut self,
        pane_id: &str,
        source: &str,
        cx: &mut Context<Self>,
    ) {
        self.workbench_focus_group(pane_id, Some(source), cx);
    }

    /// The providers this device offers (Main's catalog), in order.
    fn chooser_providers(&self, cx: &App) -> Option<Vec<(HarnessId, String)>> {
        self.composer
            .read(cx)
            .pickers()
            .read(cx)
            .offered_providers()
    }

    /// The source's own provider when offered, else the first offered one.
    fn chooser_default_provider(&self, source: &str, cx: &App) -> Option<HarnessId> {
        let offered = self.chooser_providers(cx)?;
        let own = self.chooser_source_provider(source, cx);
        own.filter(|own| offered.iter().any(|(id, _)| id == own))
            .or_else(|| offered.first().map(|(id, _)| *id))
    }

    fn chooser_source_provider(&self, source: &str, cx: &App) -> Option<HarnessId> {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == source)
            .and_then(|chat| chat.config.as_ref())
            .map(|config| config.harness)
    }

    fn chooser_source_title(&self, source: &str, cx: &App) -> String {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == source)
            .and_then(|chat| chat.title.as_deref())
            .map(transcript::single_line)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Untitled chat".into())
    }

    /// Whether the source shows any visible message (the recent-context path
    /// copies those, so it is offered only when there are some).
    fn chooser_source_has_visible(&self, source: &str, cx: &App) -> bool {
        crate::chat_binding::ChatBinding::Chat(source.to_string())
            .transcript(self.state.read(cx))
            .iter()
            .any(|entry| {
                matches!(
                    entry.role,
                    luna_doc::MessageRole::User | luna_doc::MessageRole::Assistant
                )
            })
    }

    fn chooser_capability_task(&self, source: String, cx: &mut Context<Self>) -> Option<Task<()>> {
        let engine = self.state.read(cx).engine().cloned()?;
        Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<HistoryCapabilityView>(
                    methods::GET_HISTORY_CAPABILITY,
                    serde_json::json!({ "taskId": source }),
                )
                .await
                .map_err(|error| error.to_string());
            this.update(cx, |shell, cx| {
                if let Some(chooser) = shell.workbench.chooser.as_mut()
                    && chooser.source == source
                {
                    chooser.capability = Some(result);
                    cx.notify();
                }
            })
            .ok();
        }))
    }

    /// The chooser still belongs to what is on screen: the same Main, layout
    /// generation and engine, and its group still shows the same source.
    fn chooser_current(&self, cx: &App) -> bool {
        let Some(chooser) = self.workbench.chooser.as_ref() else {
            return false;
        };
        matches!(self.route, Route::Chat)
            && chooser.main == self.workbench.main
            && chooser.generation == self.workbench.generation
            && chooser.engine_generation == self.state.read(cx).engine_generation()
            && self.workbench_ready()
            && self.workbench_group_source(&chooser.pane_id) == chooser.source
    }

    pub(in crate::shell) fn workbench_chooser_kind(
        &mut self,
        kind: CreationKind,
        cx: &mut Context<Self>,
    ) {
        if let Some(chooser) = self.workbench.chooser.as_mut()
            && !chooser.side_requested
        {
            chooser.kind = kind;
            cx.notify();
        }
    }

    pub(in crate::shell) fn workbench_chooser_provider(
        &mut self,
        provider: HarnessId,
        cx: &mut Context<Self>,
    ) {
        let offered = self
            .chooser_providers(cx)
            .is_some_and(|list| list.iter().any(|(id, _)| *id == provider));
        if let Some(chooser) = self.workbench.chooser.as_mut()
            && chooser.kind == CreationKind::NewThread
            && offered
        {
            chooser.provider = Some(provider);
            cx.notify();
        }
    }

    /// Step the New thread provider highlight (arrow keys).
    fn chooser_step_provider(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(list) = self.chooser_providers(cx) else {
            return;
        };
        if list.is_empty() {
            return;
        }
        if let Some(chooser) = self.workbench.chooser.as_mut()
            && chooser.kind == CreationKind::NewThread
        {
            let current = chooser
                .provider
                .and_then(|provider| list.iter().position(|(id, _)| *id == provider))
                .unwrap_or(0) as isize;
            let next = (current + delta).rem_euclid(list.len() as isize) as usize;
            chooser.provider = Some(list[next].0);
            cx.notify();
        }
    }

    /// The chooser's primary action now (see [`ChooserAction`]).
    pub(in crate::shell) fn chooser_action(&self, cx: &App) -> ChooserAction {
        if !matches!(self.route, Route::Chat) {
            return ChooserAction::Disabled(
                "Return to the conversation to create a thread.".into(),
            );
        }
        let Some(chooser) = self.workbench.chooser.as_ref() else {
            return ChooserAction::Disabled("".into());
        };
        if !self.chooser_current(cx) && !chooser.side_requested {
            return ChooserAction::Disabled("This group changed. Open + again.".into());
        }
        match chooser.kind {
            CreationKind::NewThread => {
                if self.workbench.creating_child.is_some() {
                    return ChooserAction::Disabled("Creating a new thread…".into());
                }
                // An earlier New thread whose outcome is unknown blocks any
                // new intent until its status is checked.
                if self.workbench_child_unresolved() {
                    return ChooserAction::Disabled(
                        "An earlier New thread may already exist. Check its status first.".into(),
                    );
                }
                match (self.chooser_providers(cx), chooser.provider) {
                    (None, _) => {
                        ChooserAction::Disabled("Loading the providers this device offers…".into())
                    }
                    (Some(list), _) if list.is_empty() => ChooserAction::Disabled(
                        "No providers are available. Enable a provider in Settings.".into(),
                    ),
                    (Some(_), None) => ChooserAction::Disabled("Choose a provider.".into()),
                    (Some(list), Some(provider)) if list.iter().any(|(id, _)| *id == provider) => {
                        ChooserAction::NewThread(provider)
                    }
                    (Some(_), Some(_)) => ChooserAction::Disabled(
                        "The chosen provider is no longer available. Choose another provider."
                            .into(),
                    ),
                }
            }
            CreationKind::SideChat => {
                let side = self
                    .side_chat_panel
                    .as_ref()
                    .filter(|panel| panel.read(cx).parent_id() == chooser.source)
                    .map(|panel| panel.read(cx).snapshot());
                if chooser.side_requested
                    && let Some(side) = &side
                {
                    if side.busy {
                        return ChooserAction::Disabled("Creating side chat…".into());
                    }
                    if let Some(snapshot) = side.pending_create {
                        return ChooserAction::CheckStatus { snapshot };
                    }
                }
                let other_pending = self.side_chat_panel.as_ref().is_some_and(|panel| {
                    let panel = panel.read(cx);
                    panel.parent_id() != chooser.source
                        && (panel.is_busy() || panel.snapshot().pending_create.is_some())
                });
                if other_pending {
                    return ChooserAction::Disabled(
                        "Another side chat is still being created. Check its status in Related chats first."
                            .into(),
                    );
                }
                if self.chooser_source_provider(&chooser.source, cx).is_none() {
                    return ChooserAction::Disabled(
                        "The source has no provider yet: send a message there first.".into(),
                    );
                }
                let has_visible = self.chooser_source_has_visible(&chooser.source, cx);
                let refused_native = side
                    .as_ref()
                    .is_some_and(|side| side.snapshot_alternative && side.error.is_some());
                match chooser.capability.as_ref() {
                    None => ChooserAction::Disabled(
                        "Checking whether this source can be forked natively…".into(),
                    ),
                    Some(Err(error)) => ChooserAction::Disabled(
                        format!("Couldn't check this source's history: {error}").into(),
                    ),
                    Some(Ok(view)) => {
                        let native = view.capability != ProviderForkCapability::None;
                        if native && !refused_native {
                            ChooserAction::SideNative
                        } else if has_visible {
                            ChooserAction::SideRecent
                        } else {
                            ChooserAction::Disabled(
                                "This source has nothing to carry yet. Send a message there first."
                                    .into(),
                            )
                        }
                    }
                }
            }
        }
    }

    /// Run the chooser's primary action (button, Enter, QA alike); a
    /// disabled action does nothing.
    pub(in crate::shell) fn workbench_chooser_create(&mut self, cx: &mut Context<Self>) {
        if !self.chooser_current(cx)
            && !self
                .workbench
                .chooser
                .as_ref()
                .is_some_and(|chooser| chooser.side_requested)
        {
            // Stale: close without creating (the card had the keyboard).
            self.chooser_close_stale(true, cx);
            return;
        }
        let action = self.chooser_action(cx);
        let Some(chooser) = self.workbench.chooser.as_ref() else {
            return;
        };
        let (pane_id, source) = (chooser.pane_id.clone(), chooser.source.clone());
        let snapshot = match action {
            ChooserAction::Disabled(_) => return,
            ChooserAction::NewThread(provider) => {
                self.workbench.chooser = None;
                self.request_child(Some(pane_id.clone()), false, Some(provider), cx);
                self.workbench_focus_after_chooser(&pane_id, &source, cx);
                return;
            }
            ChooserAction::SideNative => false,
            ChooserAction::SideRecent => true,
            ChooserAction::CheckStatus { snapshot } => snapshot,
        };
        let reuse = self
            .side_chat_panel
            .as_ref()
            .is_some_and(|panel| panel.read(cx).parent_id() == source);
        if !reuse {
            self.release_side_chat(cx);
            self.create_side_chat_panel_for(source, None, cx);
        }
        if let Some(chooser) = self.workbench.chooser.as_mut() {
            chooser.side_requested = true;
        }
        if let Some(panel) = self.side_chat_panel.clone() {
            panel.update(cx, |panel, cx| panel.create(snapshot, cx));
        }
        cx.notify();
    }

    /// Close a chooser whose side chat finished (it opens as a tab), or that
    /// went stale. Called while rendering.
    fn chooser_settle(&mut self, card_focused: bool, cx: &mut Context<Self>) {
        let Some(chooser) = self.workbench.chooser.as_ref() else {
            return;
        };
        if !chooser.side_requested && !self.chooser_current(cx) {
            self.chooser_close_stale(card_focused, cx);
            return;
        }
        if chooser.side_requested {
            let done = self.side_chat_panel.as_ref().is_none_or(|panel| {
                let panel = panel.read(cx);
                let snapshot = panel.snapshot();
                !snapshot.busy && snapshot.error.is_none() && snapshot.pending_create.is_none()
            });
            if done {
                self.workbench.chooser = None;
            }
        }
    }

    fn chooser_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let modifiers = event.keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt {
            return false;
        }
        let kind = self.workbench.chooser.as_ref().map(|chooser| chooser.kind);
        match event.keystroke.key.as_str() {
            "escape" => self.workbench_close_chooser(cx),
            "enter" => self.workbench_chooser_create(cx),
            "n" => self.workbench_chooser_kind(CreationKind::NewThread, cx),
            "s" => self.workbench_chooser_kind(CreationKind::SideChat, cx),
            "up" | "down" if kind == Some(CreationKind::NewThread) => {
                self.chooser_step_provider(if event.keystroke.key == "up" { -1 } else { 1 }, cx);
            }
            digit
                if kind == Some(CreationKind::NewThread)
                    && digit.len() == 1
                    && digit.chars().all(|c| ('1'..='9').contains(&c)) =>
            {
                let index = digit.parse::<usize>().unwrap_or(1) - 1;
                if let Some((provider, _)) = self
                    .chooser_providers(cx)
                    .and_then(|list| list.get(index).cloned())
                {
                    self.workbench_chooser_provider(provider, cx);
                }
            }
            _ => return false,
        }
        true
    }

    /// The anchored chooser card, while open.
    pub(in crate::shell) fn render_creation_chooser(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // A navigation action may open Settings while this card has focus.
        // Dismiss the presentation without cancelling a creator already sent.
        if !matches!(self.route, Route::Chat) {
            self.workbench.chooser = None;
            return None;
        }
        let card_focused = self
            .workbench
            .chooser
            .as_ref()
            .is_some_and(|chooser| chooser.focus.is_focused(window));
        self.chooser_settle(card_focused, cx);
        let claim_focus = std::mem::take(&mut self.workbench.chooser.as_mut()?.focus_pending);
        if claim_focus {
            let focus = self.workbench.chooser.as_ref()?.focus.clone();
            window.focus(&focus, cx);
        }
        let chooser = self.workbench.chooser.as_ref()?;
        let theme = Theme::of(cx).clone();
        let source_title = self.chooser_source_title(&chooser.source, cx);
        let source_provider = self.chooser_source_provider(&chooser.source, cx);
        let providers = self.chooser_providers(cx);
        let has_visible = self.chooser_source_has_visible(&chooser.source, cx);
        let kind = chooser.kind;
        let selected = chooser.provider;
        let anchor = chooser.anchor;
        let focus = chooser.focus.clone();
        let side_requested = chooser.side_requested;
        let capability = chooser.capability.clone();
        let side = self
            .side_chat_panel
            .as_ref()
            .filter(|panel| panel.read(cx).parent_id() == chooser.source)
            .map(|panel| panel.read(cx).snapshot());

        let muted = theme.readable_muted(0.7);
        let card = popover::popover_card(&theme)
            .bg(theme.tooltip_surface())
            .w(px(340.0))
            .min_w_0()
            .whitespace_normal()
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .text_size(crate::typography::ui_rems(12.0));
        // The source, named before anything is created.
        let source_row = div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(muted)
                    .child("From"),
            )
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .truncate()
                    .child(source_title.clone()),
            );
        let kind_option = |id: &'static str,
                           option: CreationKind,
                           title: &'static str,
                           body: &'static str,
                           key: &'static str| {
            let on = kind == option;
            div()
                .id(id)
                .role(gpui::Role::RadioButton)
                .aria_label(SharedString::from(format!("{title}: {body}")))
                .aria_toggled(if on {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .flex_1()
                .min_w_0()
                .p(px(8.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(if on { theme.focus_ring() } else { theme.border })
                .when(on, |el| el.bg(theme.tab_focused_fill()))
                .cursor_pointer()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(muted)
                                .child(key),
                        ),
                )
                .child(
                    div()
                        .min_w_0()
                        .whitespace_normal()
                        .text_size(crate::typography::ui_rems(11.0))
                        .line_height(px(15.0))
                        .text_color(muted)
                        .child(body),
                )
        };
        let kinds = div()
            .flex()
            .flex_row()
            .gap(px(8.0))
            .child(
                kind_option(
                    "wb-chooser-kind-thread",
                    CreationKind::NewThread,
                    "New thread",
                    "Starts fresh. The parent's context is not carried over.",
                    "N",
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.workbench_chooser_kind(CreationKind::NewThread, cx)
                })),
            )
            .child(
                kind_option(
                    "wb-chooser-kind-side",
                    CreationKind::SideChat,
                    "Side chat",
                    "Carries the parent's context, captured now. Later messages are not copied.",
                    "S",
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.workbench_chooser_kind(CreationKind::SideChat, cx)
                })),
            );
        // Providers: any offered one for a New thread; the source's own for a
        // Side chat (its context lives in that provider's session).
        let provider_rows: Vec<AnyElement> = providers
            .clone()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(index, (provider, name))| {
                let (icon_path, tint) = crate::pickers::harness_brand_icon(provider);
                let enabled = match kind {
                    CreationKind::NewThread => true,
                    CreationKind::SideChat => source_provider == Some(provider),
                };
                let chosen = match kind {
                    CreationKind::NewThread => selected == Some(provider),
                    CreationKind::SideChat => source_provider == Some(provider),
                };
                let row = div()
                    .id(SharedString::from(format!(
                        "wb-chooser-provider-{provider:?}"
                    )))
                    .role(gpui::Role::MenuItemRadio)
                    .aria_label(SharedString::from(name.clone()))
                    .aria_toggled(if chosen {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .h(px(30.0))
                    .px(px(8.0))
                    .rounded(px(6.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .when(chosen, |el| el.bg(theme.wash(0.09)))
                    .when(!enabled, |el| el.opacity(0.45))
                    .child(
                        div()
                            .w(px(12.0))
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(muted)
                            .child(if index < 9 && kind == CreationKind::NewThread {
                                format!("{}", index + 1)
                            } else {
                                String::new()
                            }),
                    )
                    .child(
                        icon(icon_path)
                            .size(px(14.0))
                            .flex_none()
                            .text_color(tint.unwrap_or(theme.text_muted)),
                    )
                    .child(div().flex_1().text_color(theme.text).child(name));
                if enabled && kind == CreationKind::NewThread {
                    row.cursor_pointer()
                        .hover(|style| style.bg(crate::theme::wash(0.05)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.workbench_chooser_provider(provider, cx)
                        }))
                        .into_any_element()
                } else {
                    row.into_any_element()
                }
            })
            .collect();
        let provider_note: Option<SharedString> = match kind {
            CreationKind::NewThread => providers
                .is_none()
                .then(|| "Loading the providers this device offers…".into()),
            CreationKind::SideChat => Some(match source_provider {
                Some(provider) => {
                    let name = providers
                        .as_ref()
                        .and_then(|list| list.iter().find(|(id, _)| *id == provider))
                        .map(|(_, name)| name.clone())
                        .unwrap_or_else(|| format!("{provider:?}"));
                    format!("Side chats stay on {name}, the source's provider and account, to keep its context.").into()
                }
                None => "The source has no provider yet: send a message there first.".into(),
            }),
        };
        // What a Side chat would actually carry, before creating it.
        let native = capability
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|view| view.capability != ProviderForkCapability::None);
        let unavailable_reason = capability
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|view| view.unavailable_reason.clone());
        let action = self.chooser_action(cx);
        let recent_context = matches!(
            action,
            ChooserAction::SideRecent | ChooserAction::CheckStatus { snapshot: true }
        ) || side
            .as_ref()
            .is_some_and(|side| side.pending_create == Some(true));
        let side_context: Option<SharedString> = (kind == CreationKind::SideChat).then(|| {
            if recent_context {
                return "Context: recent visible messages only, captured now.".into();
            }
            match native {
                Some(true) => "Context: the source's native provider history, captured now.".into(),
                Some(false) if has_visible => format!(
                    "This source can't be forked natively{}. You can start the side chat from a copy of its recent visible messages instead.",
                    unavailable_reason
                        .as_deref()
                        .map(|reason| format!(" ({reason})"))
                        .unwrap_or_default()
                )
                .into(),
                Some(false) => "This source has no provider history to carry yet. Send a message there first.".into(),
                None => "Checking whether this source can be forked natively…".into(),
            }
        });
        // A side chat in flight or failed (the creator's own state).
        let side_status: Option<(SharedString, bool)> = side
            .as_ref()
            .filter(|_| side_requested)
            .and_then(|snapshot| {
                if snapshot.busy {
                    Some((
                        snapshot.operation.unwrap_or("Creating side chat…").into(),
                        false,
                    ))
                } else {
                    snapshot.error.clone().map(|error| (error.into(), true))
                }
            });

        // Actions: one primary action naming its result, from the same
        // decision Enter and the create handler use.
        // The reason a disabled action can't run, unless a note above
        // already says it.
        let disabled_reason = match &action {
            ChooserAction::Disabled(reason)
                if !reason.is_empty()
                    && provider_note.as_ref() != Some(reason)
                    && side_context.as_ref() != Some(reason) =>
            {
                Some(reason.clone())
            }
            _ => None,
        };
        let primary_label = action.label();
        let primary_enabled = !matches!(action, ChooserAction::Disabled(_));
        let actions = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap(px(8.0))
            .child(
                popover::btn_ghost(&theme, "Cancel", "wb-chooser-cancel")
                    .id("wb-chooser-cancel")
                    .on_click(cx.listener(|this, _, _, cx| this.workbench_close_chooser(cx))),
            )
            .child({
                let button = popover::btn_primary(&theme, primary_label)
                    .id("wb-chooser-create")
                    .aria_label(primary_label);
                if primary_enabled {
                    button
                        .on_click(cx.listener(|this, _, _, cx| this.workbench_chooser_create(cx)))
                        .into_any_element()
                } else {
                    div().opacity(0.5).child(button).into_any_element()
                }
            });
        let card = card
            .id("wb-creation-chooser")
            .role(gpui::Role::Dialog)
            .aria_label(SharedString::from(format!("Create from {source_title}")))
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if this.chooser_key(event, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.workbench_close_chooser(cx)))
            .child(source_row)
            .child(kinds)
            .child(div().flex().flex_col().gap(px(2.0)).children(provider_rows))
            .children(provider_note.map(|note| {
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .line_height(px(15.0))
                    .text_color(muted)
                    .child(note)
            }))
            .children(side_context.map(|note| {
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .line_height(px(15.0))
                    .text_color(muted)
                    .child(note)
            }))
            .children(side_status.map(|(text, failed)| {
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .line_height(px(15.0))
                    .text_color(if failed {
                        theme.readable_tone(theme.danger_muted, 1.0)
                    } else {
                        muted
                    })
                    .child(text)
            }))
            .children(disabled_reason.map(|reason| {
                div()
                    .id("wb-chooser-disabled-reason")
                    .text_size(crate::typography::ui_rems(11.0))
                    .line_height(px(15.0))
                    .text_color(muted)
                    .child(reason)
            }))
            .child(actions);
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(anchor)
                    .anchor(gpui::Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .child(div().occlude().child(card)),
            )
            .priority(1)
            .into_any_element(),
        )
    }
}

/// Headless QA drives the chooser through the same paths as its controls.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    /// Hold the Main composer's real provider-cache state for GUI acceptance.
    pub fn fixture_chooser_hold_providers(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<Vec<luna_engine::HarnessDescriptor>> {
        let pickers = self.composer.read(cx).pickers().clone();
        let loaded = pickers.update(cx, |pickers, cx| pickers.fixture_hold_providers(cx));
        cx.notify();
        loaded
    }

    pub fn fixture_chooser_restore_providers(
        &mut self,
        loaded: Vec<luna_engine::HarnessDescriptor>,
        cx: &mut Context<Self>,
    ) {
        let pickers = self.composer.read(cx).pickers().clone();
        pickers.update(cx, |pickers, cx| {
            pickers.fixture_restore_providers(loaded, cx)
        });
        cx.notify();
    }

    /// Withhold the preview result until the fixture explicitly asks for a
    /// fresh real RPC. No clock race can skip the loading-state assertion.
    pub fn fixture_chooser_hold_capability(&mut self, hold: bool, cx: &mut Context<Self>) {
        if hold {
            if let Some(chooser) = self.workbench.chooser.as_mut() {
                chooser._capability_task = None;
                chooser.capability = None;
            }
        } else if let Some(source) = self
            .workbench
            .chooser
            .as_ref()
            .map(|chooser| chooser.source.clone())
        {
            let task = self.chooser_capability_task(source, cx);
            if let Some(chooser) = self.workbench.chooser.as_mut() {
                chooser._capability_task = task;
            }
        }
        cx.notify();
    }

    /// `+` on `pane_id` (or the focused group), with `kind` preselected.
    pub fn fixture_chooser_open(
        &mut self,
        pane_id: Option<&str>,
        side: bool,
        cx: &mut Context<Self>,
    ) {
        self.workbench_open_chooser(
            pane_id.map(str::to_string),
            if side {
                CreationKind::SideChat
            } else {
                CreationKind::NewThread
            },
            cx,
        );
    }
    pub fn fixture_chooser_kind(&mut self, side: bool, cx: &mut Context<Self>) {
        self.workbench_chooser_kind(
            if side {
                CreationKind::SideChat
            } else {
                CreationKind::NewThread
            },
            cx,
        );
    }
    pub fn fixture_chooser_provider(&mut self, provider: HarnessId, cx: &mut Context<Self>) {
        self.workbench_chooser_provider(provider, cx);
    }
    /// The chooser's primary action, exactly as its button or Enter runs it.
    pub fn fixture_chooser_create(&mut self, cx: &mut Context<Self>) {
        self.workbench_chooser_create(cx);
    }
    pub fn fixture_chooser_close(&mut self, cx: &mut Context<Self>) {
        self.workbench_close_chooser(cx);
    }
    /// The chooser as shown: source, kind, provider, capability preview and
    /// whether it is still current.
    pub fn fixture_chooser_readback(&self, cx: &App) -> serde_json::Value {
        match self.workbench.chooser.as_ref() {
            Some(chooser) => json!({
                "open": true,
                "pane": chooser.pane_id,
                "source": chooser.source,
                "kind": format!("{:?}", chooser.kind),
                "provider": chooser.provider.map(|provider| format!("{provider:?}")),
                "capability": chooser.capability.as_ref().map(|result| match result {
                    Ok(view) => json!({"capability": format!("{:?}", view.capability), "provider": view.provider.map(|p| format!("{p:?}")), "unavailableReason": view.unavailable_reason}),
                    Err(error) => json!({"error": error}),
                }),
                "current": self.chooser_current(cx),
                "sideRequested": chooser.side_requested,
                "action": format!("{:?}", self.chooser_action(cx)),
                "providers": self.chooser_providers(cx).map(|list| list.into_iter().map(|(id, _)| format!("{id:?}")).collect::<Vec<_>>()),
            }),
            None => json!({ "open": false }),
        }
    }
}
