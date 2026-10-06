//! One provider's defaults for fresh chats, rendered as rows inside that
//! provider's card on Settings → Providers.
//!
//! What the engine stores (`GetProviderDefaults` / `SetProviderDefaults`,
//! [`ProviderDefaultsSnapshot`]): one device-owned map of provider →
//! `{model, reasoning, modelOptions}` under a single revision. A save names
//! one provider and must carry the current revision, so each card edits only
//! its own provider's entry and reads the revision from the shared
//! [`AppState::provider_defaults`] (the freshest snapshot the app has seen):
//! saving Codex never leaves the Claude card holding a stale revision. The
//! engine refuses a save for a provider that is not installed and enabled, so
//! a disabled provider's card shows no defaults.
//!
//! The engine keeps no "default provider". A new chat starts on the
//! composer's remembered provider (`composer-defaults.json`,
//! [`ComposerDefaults::harness`]) unless its project names one;
//! [`save_new_chat_provider`] writes that same field for the page's
//! "New chats" row.

use std::path::Path;

use gpui::{
    AnyElement, App, Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use luna_proto::{
    HarnessId, Model, ProviderDefaultsSnapshot, ProviderExecutionDefault, ReasoningLevel,
};
use luna_rpc::methods;

use crate::button::{self, ControlSize, Segment};
use crate::popover::{self, Loadable};
use crate::settings::composer::ComposerDefaults;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;
use crate::typography::ui_rems;

/// Set (with the new value) whenever Settings changes the provider new chats
/// start with, so a composer that already holds its own copy of the sticky
/// defaults can adopt it.
pub struct NewChatProviderChanged(pub HarnessId);

impl gpui::Global for NewChatProviderChanged {}

/// The provider new chats start with when their project names none: the
/// composer's remembered pick.
pub fn new_chat_provider(data_dir: &Path) -> Option<HarnessId> {
    ComposerDefaults::load(data_dir).harness
}

/// Write the provider new chats start with — the same field a provider pick
/// on the new-chat canvas writes — and announce it.
pub fn save_new_chat_provider(
    data_dir: &Path,
    harness: HarnessId,
    cx: &mut App,
) -> std::io::Result<()> {
    let mut defaults = ComposerDefaults::load(data_dir);
    defaults.harness = Some(harness);
    defaults.save(data_dir)?;
    cx.set_global(NewChatProviderChanged(harness));
    Ok(())
}

/// Which pop-up menu of the card is open.
#[derive(Clone, PartialEq, Eq)]
enum Menu {
    Model,
    Reasoning,
    Option(String),
}

#[derive(Clone)]
enum Action {
    Model(Option<String>),
    Reasoning(Option<ReasoningLevel>),
    Option(String, String),
    Reset,
}

/// One row of a pop-up menu.
struct MenuItem {
    key: String,
    label: SharedString,
    detail: Option<SharedString>,
    /// Wears the "New" badge beside the label (recently listed models).
    new: bool,
    active: bool,
    action: Action,
}

/// Width of the model and long-list pop-up buttons.
const SELECT_WIDTH: f32 = 220.0;
/// Width of their menus.
const MENU_WIDTH: f32 = 260.0;
/// Choices up to this count are a segmented control; more are a pop-up.
const SEGMENTED_MAX: usize = 5;

/// A segmented control's width for `labels`: equal segments sized for the
/// longest label at the Md type size, in points at the default interface size.
pub(crate) fn segmented_width(labels: &[SharedString]) -> f32 {
    let widest = labels
        .iter()
        .map(|label| label.chars().count())
        .max()
        .unwrap_or(4) as f32;
    (widest * 6.6 + 24.0).max(56.0) * labels.len().max(1) as f32
}

pub struct ProviderDefaultsPanel {
    state: Entity<AppState>,
    provider: HarnessId,
    name: &'static str,
    /// Stable id fragment (the CLI name).
    key: &'static str,
    /// The provider's model catalog. Idle until the page asks for it (the
    /// provider is enabled), kept visible while a refresh runs.
    models: Loadable<Vec<Model>>,
    /// Set by the page when the catalog and saved defaults should be reread
    /// the next time the card is shown.
    stale: bool,
    menu: popover::Popup<Menu>,
    /// The selection being saved, shown until the engine answers
    /// (`Some(None)` = reset to the provider's defaults).
    pending: Option<Option<ProviderExecutionDefault>>,
    error: Option<String>,
    generation: u64,
    load_task: Option<Task<()>>,
    save_task: Option<Task<()>>,
    _observe: Subscription,
}

impl ProviderDefaultsPanel {
    pub fn new(
        state: Entity<AppState>,
        provider: HarnessId,
        name: &'static str,
        key: &'static str,
        cx: &mut Context<Self>,
    ) -> Self {
        let generation = state.read(cx).engine_generation();
        let observe = cx.observe(&state, |this: &mut Self, _, cx| {
            if this.generation != this.state.read(cx).engine_generation() {
                // A different connection: drop everything from the old one
                // and reread if the card was showing defaults.
                let was_shown = !matches!(this.models, Loadable::Idle);
                this.load_task = None;
                this.save_task = None;
                this.pending = None;
                this.error = None;
                this.models = Loadable::Idle;
                this.close_menu(cx);
                this.generation = this.state.read(cx).engine_generation();
                if was_shown {
                    this.load(cx);
                }
            }
            cx.notify();
        });
        Self {
            state,
            provider,
            name,
            key,
            models: Loadable::Idle,
            stale: false,
            menu: popover::Popup::default(),
            pending: None,
            error: None,
            generation,
            load_task: None,
            save_task: None,
            _observe: observe,
        }
    }

    pub fn provider(&self) -> HarnessId {
        self.provider
    }

    /// Reread the catalog and saved defaults the next time [`Self::ensure_loaded`]
    /// runs (page visit, Refresh, a login or account change, a CLI update).
    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// Load when never loaded or marked stale. The page calls this for the
    /// providers whose cards show defaults.
    pub fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if self.stale || matches!(self.models, Loadable::Idle) {
            self.load(cx);
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.stale = false;
        self.generation = self.state.read(cx).engine_generation();
        let generation = self.generation;
        let provider = self.provider;
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.models = Loadable::Error("Connect to this device to load agent defaults.".into());
            cx.notify();
            return;
        };
        if !matches!(self.models, Loadable::Ready(_)) {
            self.models = Loadable::Loading;
        }
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let defaults = engine
                .client()
                .call(methods::GET_PROVIDER_DEFAULTS, serde_json::json!({}))
                .await
                .map_err(|e| e.to_string())
                .and_then(|value| {
                    serde_json::from_value::<ProviderDefaultsSnapshot>(value)
                        .map_err(|e| e.to_string())
                });
            let models = engine
                .client()
                .call(
                    methods::LIST_MODELS,
                    serde_json::json!({ "harness": provider }),
                )
                .await
                .map_err(|e| e.to_string())
                .and_then(|value| {
                    serde_json::from_value::<Vec<Model>>(value).map_err(|e| e.to_string())
                });
            let _ = this.update(cx, |this, cx| {
                if this.state.read(cx).engine_generation() != generation {
                    return;
                }
                match defaults {
                    Ok(snapshot) => {
                        this.error = None;
                        apply_snapshot(&this.state, snapshot, cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("Saved defaults could not load: {error}"));
                    }
                }
                this.models = match models {
                    Ok(models) => Loadable::Ready(models),
                    Err(error) => Loadable::Error(error),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// What this card shows: the selection being saved, else the provider's
    /// entry in the shared snapshot.
    fn selection(&self, cx: &App) -> ProviderExecutionDefault {
        match &self.pending {
            Some(pending) => pending.clone().unwrap_or_default(),
            None => self
                .state
                .read(cx)
                .provider_defaults
                .providers
                .get(&self.provider)
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn has_override(&self, cx: &App) -> bool {
        match &self.pending {
            Some(pending) => pending.is_some(),
            None => self
                .state
                .read(cx)
                .provider_defaults
                .providers
                .contains_key(&self.provider),
        }
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.begin_close() {
            popover::reap_popup(cx, |panel: &mut Self| &mut panel.menu);
            cx.notify();
        }
    }

    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            return;
        }
        let (revision, loaded) = {
            let state = self.state.read(cx);
            (
                state.provider_defaults.revision,
                state.provider_defaults_loaded,
            )
        };
        // The revision every save must match is only known once a snapshot
        // has loaded.
        if !loaded {
            return;
        }
        let mut current = self.selection(cx);
        let defaults = match action {
            Action::Model(model) => {
                current.model = model;
                current.reasoning = None;
                current.model_options.clear();
                Some(current)
            }
            Action::Reasoning(reasoning) => {
                current.reasoning = reasoning;
                Some(current)
            }
            Action::Option(key, value) => {
                current.model_options.insert(key, serde_json::json!(value));
                Some(current)
            }
            Action::Reset => None,
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let provider = self.provider;
        let generation = self.state.read(cx).engine_generation();
        self.pending = Some(defaults.clone());
        self.error = None;
        self.save_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SET_PROVIDER_DEFAULTS,
                    serde_json::json!({
                        "expectedRevision": revision,
                        "provider": provider,
                        "defaults": defaults,
                    }),
                )
                .await
                .map_err(|e| e.to_string())
                .and_then(|value| {
                    serde_json::from_value::<ProviderDefaultsSnapshot>(value)
                        .map_err(|e| e.to_string())
                });
            let _ = this.update(cx, |this, cx| {
                if this.state.read(cx).engine_generation() != generation {
                    return;
                }
                this.pending = None;
                match result {
                    Ok(snapshot) => apply_snapshot(&this.state, snapshot, cx),
                    Err(error) => {
                        this.error = Some(error);
                        // A stale revision (another card, an agent tool) or a
                        // catalog change: reread both so the next pick saves
                        // against what the engine holds now.
                        this.load(cx);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// A pop-up button with its menu, right-aligned under the trigger.
    #[allow(clippy::too_many_arguments)]
    fn select_menu(
        &self,
        menu: Menu,
        id: SharedString,
        label: SharedString,
        aria: SharedString,
        items: Vec<MenuItem>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.menu.as_open() == Some(&menu);
        let enabled = self.pending.is_none();
        let press_menu = menu.clone();
        let click_menu = menu.clone();
        let mut trigger = button::select(id.clone(), label, ControlSize::Md, open, enabled, theme)
            .aria_label(aria)
            .relative()
            .w(ui_rems(SELECT_WIDTH))
            .max_w_full()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, _| {
                    this.menu
                        .note_trigger_press_matching(|open| *open == press_menu);
                }),
            )
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if this.pending.is_some() {
                    return;
                }
                // GPUI's keyboard click has no mouse-down press note, so the
                // press check would always reopen; toggle instead.
                let showing = this.menu.as_open() == Some(&click_menu);
                if event.is_keyboard() {
                    if showing {
                        this.close_menu(cx);
                    } else {
                        this.menu.open(click_menu.clone());
                    }
                } else if this.menu.take_press_was_open() {
                    this.close_menu(cx);
                } else {
                    this.menu.open(click_menu.clone());
                }
                cx.notify();
            }))
            .on_key_down(crate::swallow_activation_key);
        if self.menu.get() == Some(&menu) {
            let closing = self.menu.closing_since();
            let rows: Vec<AnyElement> = items
                .into_iter()
                .map(|item| {
                    let row_id = SharedString::from(format!("{id}-row-{}", item.key));
                    let action = item.action.clone();
                    popover::menu_row(theme, item.active, row_id.clone())
                        .id(row_id)
                        .role(gpui::Role::Button)
                        .aria_label(item.label.clone())
                        .aria_toggled(if item.active {
                            gpui::Toggled::True
                        } else {
                            gpui::Toggled::False
                        })
                        .tab_index(0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            this.act(action.clone(), cx);
                        }))
                        .on_key_down(crate::swallow_activation_key)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .items_center()
                                        .gap(ui_rems(6.0))
                                        .child(div().min_w_0().truncate().child(item.label))
                                        .when(item.new, |line| {
                                            line.child(crate::pickers::new_model_badge(theme))
                                        }),
                                )
                                .when_some(item.detail, |column, detail| {
                                    column.child(
                                        div()
                                            .truncate()
                                            .text_size(ui_rems(widgets::ROW_DESCRIPTION_SIZE))
                                            .line_height(ui_rems(16.0))
                                            .text_color(theme.readable_muted(0.8))
                                            .child(detail),
                                    )
                                }),
                        )
                        .when(item.active, |row| {
                            row.child(
                                crate::icons::icon(crate::icons::CHECK)
                                    .size(ui_rems(14.0))
                                    .flex_none()
                                    .text_color(theme.selected_row_icon(theme.accent)),
                            )
                        })
                        .into_any_element()
                })
                .collect();
            let card = popover::popover_card(theme)
                .w(ui_rems(MENU_WIDTH))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)))
                .flex()
                .flex_col()
                .child(
                    // Long catalogs (OpenCode lists many models) scroll
                    // inside the card instead of running off the window.
                    div()
                        .id(SharedString::from(format!("{id}-list")))
                        .max_h(ui_rems(320.0))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .children(rows),
                )
                .into_any_element();
            trigger = trigger.child(popover::anchored_menu_below_end(
                SharedString::from(format!("{id}-menu")),
                card,
                closing,
            ));
        }
        trigger.into_any_element()
    }

    /// A choice row: a segmented control for a few options, a pop-up button
    /// with a menu for more.
    #[allow(clippy::too_many_arguments)]
    fn choice_row(
        &self,
        menu: Menu,
        id: SharedString,
        title: SharedString,
        description: Option<SharedString>,
        options: Vec<(SharedString, Action)>,
        selected: usize,
        select_label: SharedString,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let control = if options.len() <= SEGMENTED_MAX {
            let labels: Vec<SharedString> =
                options.iter().map(|(label, _)| label.clone()).collect();
            let width = segmented_width(&labels);
            let actions: Vec<Action> = options.iter().map(|(_, action)| action.clone()).collect();
            let generation = self.generation;
            button::segmented(
                id,
                options
                    .iter()
                    .enumerate()
                    .map(|(ix, (label, _))| Segment::new(ix.to_string(), label.clone()))
                    .collect(),
                selected,
                width,
                ControlSize::Md,
                theme,
                cx.listener(move |this, ix: &usize, _, cx| {
                    if this.state.read(cx).engine_generation() != generation {
                        return;
                    }
                    if let Some(action) = actions.get(*ix) {
                        this.act(action.clone(), cx);
                    }
                }),
            )
            .when(self.pending.is_some(), |el| el.opacity(0.6))
            .into_any_element()
        } else {
            let items = options
                .into_iter()
                .enumerate()
                .map(|(ix, (label, action))| MenuItem {
                    key: ix.to_string(),
                    label,
                    detail: None,
                    new: false,
                    active: ix == selected,
                    action,
                })
                .collect();
            self.select_menu(menu, id, select_label, title.clone(), items, theme, cx)
        };
        widgets::card_row(theme, false)
            .child(widgets::row_text(theme, title, description))
            .child(widgets::row_controls().child(control))
            .into_any_element()
    }

    /// A strip (error or warning) inside the card, between hairlines.
    fn strip_row(theme: &Theme, strip: gpui::Div) -> AnyElement {
        div()
            .w_full()
            .px(ui_rems(widgets::ROW_PAD_X))
            .py(ui_rems(10.0))
            .border_t_1()
            .border_color(theme.border)
            .child(strip.mt(ui_rems(0.0)))
            .into_any_element()
    }

    /// An error strip with a trailing Retry that rereads the catalog and the
    /// saved defaults.
    fn retry_strip(
        &self,
        message: String,
        id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        Self::strip_row(
            theme,
            widgets::error_strip(theme, message)
                .child(div().flex_1())
                .child(
                    button::button(
                        format!("defaults-{id}-retry-{}", self.key),
                        button::ButtonKind::Secondary,
                        ControlSize::Sm,
                        !matches!(self.models, Loadable::Loading),
                        theme,
                    )
                    .aria_label(format!("Reload {} defaults", self.name))
                    .child("Retry")
                    .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                ),
        )
    }
}

/// Adopt a snapshot the engine returned if it is at least as new as the one
/// the app holds.
fn apply_snapshot(state: &Entity<AppState>, snapshot: ProviderDefaultsSnapshot, cx: &mut App) {
    state.update(cx, |state, cx| {
        if snapshot.revision >= state.provider_defaults.revision {
            state.provider_defaults = snapshot;
        }
        state.provider_defaults_loaded = true;
        state.provider_defaults_error = None;
        cx.notify();
    });
}

impl Render for ProviderDefaultsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let name = self.name;
        let key = self.key;
        let defaults = self.selection(cx);
        let saving = self.pending.is_some();
        let can_reset =
            self.has_override(cx) && !saving && self.state.read(cx).provider_defaults_loaded;
        let mut rows: Vec<AnyElement> = Vec::new();

        // The group's own header row: what these settings do, and the reset.
        rows.push(
            widgets::card_row(&theme, false)
                .child(widgets::row_text(
                    &theme,
                    "Defaults for new chats",
                    Some(SharedString::from(format!(
                        "Used by you and your agents when starting a fresh {name} chat. A \
                         model picked in the composer or set by the project wins. Existing \
                         chats keep their settings."
                    ))),
                ))
                .child(
                    widgets::row_controls().child(
                        widgets::ghost_action(format!("defaults-reset-{key}"), can_reset, &theme)
                            .aria_label(format!("Use {name} provider defaults"))
                            .child("Use provider defaults")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.act(Action::Reset, cx);
                            })),
                    ),
                )
                .into_any_element(),
        );

        if let Some(error) = self.error.clone() {
            rows.push(self.retry_strip(error, "save", &theme, cx));
        }

        match self.models.clone() {
            Loadable::Idle | Loadable::Loading => rows.push(
                widgets::card_row(&theme, false)
                    .child(div().w_full().child(popover::skeleton_rows(
                        "provider-defaults-loading",
                        &theme,
                        2,
                        cx.entity_id(),
                        cx,
                    )))
                    .into_any_element(),
            ),
            Loadable::Error(message) => {
                rows.push(self.retry_strip(
                    format!("{name} models could not load: {message}"),
                    "models",
                    &theme,
                    cx,
                ));
            }
            Loadable::Ready(models) => {
                let saved_missing = defaults
                    .model
                    .clone()
                    .filter(|id| !models.iter().any(|model| &model.id == id));
                let selected = match defaults.model.as_deref() {
                    Some(id) => models.iter().find(|model| model.id == id),
                    None => models.first(),
                };
                if let Some(id) = &saved_missing {
                    rows.push(Self::strip_row(
                        &theme,
                        widgets::warning_strip(
                            &theme,
                            format!(
                                "The saved default model \u{201C}{id}\u{201D} is no longer \
                                 offered. New {name} chats can't start until you choose \
                                 another model or use the provider defaults."
                            ),
                        ),
                    ));
                }

                if models.is_empty() {
                    rows.push(
                        widgets::card_row(&theme, false)
                            .child(widgets::row_text(
                                &theme,
                                "Model",
                                Some(SharedString::from(format!(
                                    "{name} offers no models right now. Check its sign-in, then \
                                     refresh."
                                ))),
                            ))
                            .into_any_element(),
                    );
                } else {
                    // Model: an open-ended list, so always a pop-up.
                    let first_label = models.first().map(|model| model.label.clone());
                    let label: SharedString = match (&defaults.model, selected) {
                        (None, _) => first_label
                            .as_ref()
                            .map(|label| format!("Default ({label})"))
                            .unwrap_or_else(|| "Provider default".into())
                            .into(),
                        (Some(_), Some(model)) => model.label.clone().into(),
                        (Some(id), None) => format!("{id} (unavailable)").into(),
                    };
                    let mut items = vec![MenuItem {
                        key: "default".into(),
                        label: "Provider default".into(),
                        detail: first_label.map(SharedString::from),
                        new: false,
                        active: defaults.model.is_none(),
                        action: Action::Model(None),
                    }];
                    // Rows name the model alone (user request); a recently
                    // listed model wears the "New" badge.
                    let now = chrono::Utc::now().timestamp_millis();
                    items.extend(models.iter().map(|model| MenuItem {
                        key: model.id.clone(),
                        label: model.label.clone().into(),
                        detail: None,
                        new: crate::pickers::model_is_new(model, &models, now),
                        active: defaults.model.as_deref() == Some(model.id.as_str()),
                        action: Action::Model(Some(model.id.clone())),
                    }));
                    rows.push(
                        widgets::card_row(&theme, false)
                            .child(widgets::row_text(&theme, "Model", None))
                            .child(widgets::row_controls().child(self.select_menu(
                                Menu::Model,
                                SharedString::from(format!("defaults-model-{key}")),
                                label,
                                SharedString::from(format!("{name} default model")),
                                items,
                                &theme,
                                cx,
                            )))
                            .into_any_element(),
                    );
                }

                if let Some(model) = selected.cloned() {
                    let ladder = model.reasoning_levels.clone();
                    if !ladder.is_empty() {
                        let fallback = luna_proto::default_reasoning_level(&ladder)
                            .map(crate::pickers::reasoning_label)
                            .unwrap_or("none");
                        let saved = defaults.reasoning;
                        let off_ladder = saved.filter(|level| !ladder.contains(level));
                        let selected_ix = saved
                            .and_then(|level| ladder.iter().position(|l| *l == level))
                            .map_or(0, |ix| ix + 1);
                        let segmented = ladder.len() < SEGMENTED_MAX;
                        let mut options: Vec<(SharedString, Action)> = vec![(
                            if segmented {
                                "Default".into()
                            } else {
                                format!("Default ({fallback})").into()
                            },
                            Action::Reasoning(None),
                        )];
                        options.extend(ladder.iter().map(|level| {
                            (
                                SharedString::from(crate::pickers::reasoning_label(*level)),
                                Action::Reasoning(Some(*level)),
                            )
                        }));
                        let select_label = options
                            .get(selected_ix)
                            .map(|(label, _)| label.clone())
                            .unwrap_or_else(|| "Default".into());
                        let description = match off_ladder {
                            Some(level) => format!(
                                "The saved level, {}, isn't offered by this model; Default \
                                 ({fallback}) applies.",
                                crate::pickers::reasoning_label(level)
                            ),
                            None if segmented => format!("Default uses {fallback} for this model."),
                            None => "How long the model thinks before answering.".to_string(),
                        };
                        rows.push(self.choice_row(
                            Menu::Reasoning,
                            SharedString::from(format!("defaults-reasoning-{key}")),
                            "Reasoning".into(),
                            Some(description.into()),
                            options,
                            selected_ix,
                            select_label,
                            &theme,
                            cx,
                        ));
                    }
                    for option in &model.options {
                        let chosen = defaults
                            .model_options
                            .get(&option.id)
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(&option.default_choice)
                            .to_string();
                        let selected_ix = option
                            .choices
                            .iter()
                            .position(|choice| choice.id == chosen)
                            .or_else(|| {
                                option
                                    .choices
                                    .iter()
                                    .position(|choice| choice.id == option.default_choice)
                            })
                            .unwrap_or(0);
                        let options: Vec<(SharedString, Action)> = option
                            .choices
                            .iter()
                            .map(|choice| {
                                (
                                    SharedString::from(choice.label.clone()),
                                    Action::Option(option.id.clone(), choice.id.clone()),
                                )
                            })
                            .collect();
                        if options.is_empty() {
                            continue;
                        }
                        let select_label = options
                            .get(selected_ix)
                            .map(|(label, _)| label.clone())
                            .unwrap_or_default();
                        rows.push(self.choice_row(
                            Menu::Option(option.id.clone()),
                            SharedString::from(format!("defaults-option-{key}-{}", option.id)),
                            option.label.clone().into(),
                            None,
                            options,
                            selected_ix,
                            select_label,
                            &theme,
                            cx,
                        ));
                    }
                }
            }
        }

        div().w_full().flex().flex_col().children(rows)
    }
}
