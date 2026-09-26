//! Settings → Projects. The container owns project selection and scrolling;
//! sibling project features can compose a child card beneath execution defaults.
use crate::{
    composer::{ComposerInput, ComposerInputEvent},
    popover,
    settings::{composer::ComposerDefaults, widgets},
    state::AppState,
    theme::Theme,
};
use gpui::{
    AnyElement, Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use luna_proto::{
    HarnessId, Model, PermissionMode, ProjectCheckout, ProjectExecutionDefaults,
    ProjectProviderDefault, Space,
};
use luna_rpc::methods;

/// One fallback resolver shared by every desktop new-task entry point.
/// An explicit provider with no model means its catalog default, not the last
/// remembered model. The global checkout behavior is Current checkout.
pub struct ResolvedNewTaskDefaults<'a> {
    pub harness: Option<HarnessId>,
    pub model: Option<&'a str>,
    pub permission_mode: Option<PermissionMode>,
    pub checkout: ProjectCheckout,
}

pub fn resolve_new_task_defaults<'a>(
    project: &'a ProjectExecutionDefaults,
    global: &'a ComposerDefaults,
) -> ResolvedNewTaskDefaults<'a> {
    let harness = project
        .provider
        .as_ref()
        .map(|provider| provider.harness)
        .or(global.harness);
    let model = match &project.provider {
        Some(provider) => provider.model.as_deref(),
        None => harness
            .and_then(|harness| global.model_for(harness))
            .map(|model| model.id.as_str()),
    };
    ResolvedNewTaskDefaults {
        harness,
        model,
        permission_mode: project.permission_mode.or_else(|| {
            harness.and_then(|harness| global.permission_by_harness.get(&harness).copied())
        }),
        checkout: project.checkout.unwrap_or(ProjectCheckout::Local),
    }
}

#[derive(Clone)]
enum Action {
    Project(String),
    Provider(Option<HarnessId>),
    Model(Option<String>),
    Permission(Option<PermissionMode>),
    Checkout(Option<ProjectCheckout>),
    Models,
    RetryModels,
    Reset,
}

pub struct ProjectsPage {
    state: Entity<AppState>,
    selected_project_id: Option<String>,
    scroll: widgets::PageScroll,
    models: popover::Loadable<Vec<Model>>,
    model_search: Entity<ComposerInput>,
    models_open: bool,
    busy: bool,
    error: Option<String>,
    saved: bool,
    task: Option<Task<()>>,
    catalog_task: Option<Task<()>>,
    catalog_generation: u64,
    catalog_harness: Option<HarnessId>,
    _observe: Subscription,
    _search: Subscription,
}

impl ProjectsPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let selected_project_id = state
            .read(cx)
            .selected_space
            .clone()
            .or_else(|| state.read(cx).spaces_sorted().first().map(|s| s.id.clone()));
        let observe = cx.observe(&state, |page: &mut Self, _, cx| {
            let harness = page.selected(cx).and_then(|space| {
                space
                    .execution_defaults
                    .provider
                    .map(|provider| provider.harness)
            });
            if harness != page.catalog_harness {
                page.catalog_generation = page.catalog_generation.wrapping_add(1);
                page.catalog_task = None;
                page.catalog_harness = harness;
                page.models = popover::Loadable::Idle;
                page.models_open = false;
            }
            cx.notify();
        });
        let model_search =
            cx.new(|cx| ComposerInput::with_context("Search models…", "PaletteSearch", cx));
        let search = cx.subscribe(&model_search, |_, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                cx.notify();
            }
        });
        Self {
            state,
            selected_project_id,
            scroll: Default::default(),
            models: popover::Loadable::Idle,
            model_search,
            models_open: false,
            busy: false,
            error: None,
            saved: false,
            task: None,
            catalog_task: None,
            catalog_generation: 0,
            catalog_harness: None,
            _observe: observe,
            _search: search,
        }
    }

    /// Stable composition hook: recreate project-owned child cards only when
    /// this ID changes. Children render sections, not another page/scroll view.
    pub fn selected_project_id(&self) -> Option<&str> {
        self.selected_project_id.as_deref()
    }

    fn selected(&self, cx: &gpui::App) -> Option<Space> {
        self.selected_project_id
            .as_deref()
            .and_then(|id| self.state.read(cx).space_row(id))
            .cloned()
    }

    fn load_models(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        let Some(space) = self.selected(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.models =
                popover::Loadable::Error("Connect to the project's device to load models.".into());
            return;
        };
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        let generation = self.catalog_generation;
        self.catalog_harness = Some(harness);
        self.models = popover::Loadable::Loading;
        let project_id = space.id;
        let params = serde_json::json!({"harness":harness,"targetDeviceId":space.device_id});
        self.catalog_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::LIST_MODELS, params).await;
            this.update(cx, |page, cx| {
                if page.catalog_generation != generation
                    || page.selected_project_id.as_deref() != Some(project_id.as_str())
                {
                    return;
                }
                page.models = match result {
                    Ok(value) => serde_json::from_value(value)
                        .map(popover::Loadable::Ready)
                        .unwrap_or_else(|error| popover::Loadable::Error(error.to_string())),
                    Err(error) => popover::Loadable::Error(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn action(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Action::Project(id) = action {
            self.selected_project_id = Some(id);
            self.catalog_generation = self.catalog_generation.wrapping_add(1);
            self.catalog_task = None;
            self.catalog_harness = None;
            self.models = popover::Loadable::Idle;
            self.models_open = false;
            self.error = None;
            self.saved = false;
            self.model_search
                .update(cx, |input, cx| input.set_text("", cx));
            cx.notify();
            return;
        }
        let Some(space) = self.selected(cx) else {
            return;
        };
        if matches!(action, Action::Models | Action::RetryModels) {
            self.models_open = matches!(action, Action::RetryModels) || !self.models_open;
            if self.models_open
                && let Some(provider) = &space.execution_defaults.provider
            {
                self.load_models(provider.harness, cx);
            }
            cx.notify();
            return;
        }
        let mut defaults = space.execution_defaults.clone();
        match action {
            Action::Provider(harness) => {
                self.catalog_generation = self.catalog_generation.wrapping_add(1);
                self.catalog_task = None;
                self.catalog_harness = harness;
                defaults.provider = harness.map(|harness| ProjectProviderDefault {
                    harness,
                    model: None,
                });
                self.models = popover::Loadable::Idle;
                self.models_open = false;
            }
            Action::Model(model) => {
                if let Some(provider) = &mut defaults.provider {
                    provider.model = model;
                }
                self.models_open = false;
            }
            Action::Permission(mode) => defaults.permission_mode = mode,
            Action::Checkout(checkout) => defaults.checkout = checkout,
            Action::Reset => {
                defaults = Default::default();
                self.models_open = false;
            }
            _ => return,
        }
        self.save(space, defaults, cx);
    }

    fn save(&mut self, space: Space, defaults: ProjectExecutionDefaults, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some("Connect to the project's device before saving defaults.".into());
            cx.notify();
            return;
        };
        self.busy = true;
        self.error = None;
        self.saved = false;
        let params = serde_json::json!({"op":"setProjectExecutionDefaults","spaceId":space.id,"targetDeviceId":space.device_id,"defaults":defaults,"expectedDefaults":space.execution_defaults});
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::MUTATE, params).await;
            this.update(cx, |page, cx| {
                page.busy = false;
                match result {
                    Ok(_) => {
                        // The owning engine acknowledged the targeted commit; WatchSpaces
                        // supplies the authoritative projection.
                        // Apply only this owned field so unrelated live state is kept.
                        page.state.update(cx, |state, cx| {
                            if let Some(row) =
                                state.spaces.iter_mut().find(|row| row.id == space.id)
                            {
                                row.execution_defaults = defaults;
                            }
                            cx.notify();
                        });
                        page.saved = true;
                    }
                    Err(error) => page.error = Some(format!("Defaults were not saved: {error}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn choice(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        selected: bool,
        action: Action,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let border = theme.border;
        let accent = theme.accent;
        widgets::ghost_action(theme)
            .id(id.into())
            .role(gpui::Role::Button)
            .tab_index(0)
            .aria_selected(selected)
            .border_1()
            .border_color(if selected { accent } else { border })
            .text_color(if selected {
                theme.text
            } else {
                theme.text_muted
            })
            .when(self.busy, |el| el.opacity(0.5))
            .focus_visible(move |style| style.border_color(accent))
            .hover(|style| widgets::ghost_hover(theme, style))
            .child(label.into())
            .on_click(cx.listener({
                let action = action.clone();
                move |page, _, _, cx| page.action(action.clone(), cx)
            }))
            .on_key_down(cx.listener(move |page, event: &gpui::KeyDownEvent, _, cx| {
                if !event.is_held && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    page.action(action.clone(), cx);
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
    }

    fn choices() -> gpui::Div {
        div().flex().flex_wrap().gap(px(8.0)).mt(px(8.0))
    }
    fn row(theme: &Theme, title: &str, copy: &str, first: bool, choices: gpui::Div) -> gpui::Div {
        widgets::card_row(theme, first).child(
            div()
                .w_full()
                .child(widgets::row_title(theme, title))
                .child(widgets::page_subtitle(theme, copy))
                .child(choices),
        )
    }
}

impl popover::ScrollRailHost for ProjectsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }
    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for ProjectsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let projects = self
            .state
            .read(cx)
            .spaces_sorted()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut body = widgets::page_column()
            .child(widgets::page_header(&theme, "Projects", None))
            .child(widgets::page_subtitle(
                &theme,
                "Defaults apply to new chats created in the desktop app. Existing tasks keep their own settings.",
            ));
        let mut project_choices = Self::choices();
        for (index, space) in projects.iter().enumerate() {
            project_choices = project_choices.child(self.choice(
                ("project-defaults-project", index),
                format!(
                    "{} · {}",
                    space.display_name(),
                    self.state.read(cx).device_name(&space.device_id).unwrap_or("This device")
                ),
                self.selected_project_id.as_deref() == Some(&space.id),
                Action::Project(space.id.clone()),
                &theme,
                cx,
            ));
        }
        body = body.child(project_choices);
        if let Some(space) = self.selected(cx) {
            let defaults = &space.execution_defaults;
            body = body.child(widgets::page_subtitle(&theme, space.path.clone()));
            let mut providers = Self::choices().child(self.choice(
                "project-provider-inherit",
                "Inherit global default",
                defaults.provider.is_none(),
                Action::Provider(None),
                &theme,
                cx,
            ));
            for (index, (harness, label)) in [
                (HarnessId::Codex, "Codex"),
                (HarnessId::ClaudeCode, "Claude Code"),
                (HarnessId::Opencode, "OpenCode"),
            ]
            .into_iter()
            .enumerate()
            {
                providers = providers.child(
                    self.choice(
                        ("project-provider", index),
                        label,
                        defaults
                            .provider
                            .as_ref()
                            .is_some_and(|p| p.harness == harness),
                        Action::Provider(Some(harness)),
                        &theme,
                        cx,
                    ),
                );
            }
            let mut card = widgets::section_card(&theme).child(Self::row(
                &theme,
                "Provider and model",
                "Inherit the global provider and remembered model, or set a project override.",
                true,
                providers,
            ));
            if let Some(provider) = &defaults.provider {
                let label = provider
                    .model
                    .as_deref()
                    .unwrap_or("Provider default model");
                let mut choices = Self::choices().child(self.choice(
                    "project-model-picker",
                    label,
                    self.models_open,
                    Action::Models,
                    &theme,
                    cx,
                ));
                if provider.model.is_some() {
                    choices = choices.child(self.choice(
                        "project-model-default",
                        "Use provider default",
                        false,
                        Action::Model(None),
                        &theme,
                        cx,
                    ));
                }
                let mut row = Self::row(
                    &theme,
                    "Model",
                    "Provider default follows this provider's catalog. A pinned model must remain available.",
                    false,
                    choices,
                );
                if self.models_open {
                    let mut models = div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(self.model_search.clone());
                    match &self.models {
                        popover::Loadable::Ready(list) => {
                            let query = self.model_search.read(cx).text().to_lowercase();
                            let filtered = list
                                .iter()
                                .filter(|m| {
                                    format!("{} {}", m.id, m.label)
                                        .to_lowercase()
                                        .contains(&query)
                                })
                                .collect::<Vec<_>>();
                            if filtered.is_empty() {
                                models = models.child(widgets::page_subtitle(
                                    &theme,
                                    "No available models. Refresh Providers or refine the search.",
                                ));
                            }
                            for (index, model) in filtered.iter().take(50).enumerate() {
                                models = models.child(self.choice(
                                    ("project-model", index),
                                    format!("{} · {}", model.label, model.id),
                                    provider.model.as_deref() == Some(model.id.as_str()),
                                    Action::Model(Some(model.id.clone())),
                                    &theme,
                                    cx,
                                ));
                            }
                            if filtered.len() > 50 {
                                models = models.child(widgets::page_subtitle(
                                    &theme,
                                    "Showing 50 models. Search to narrow the list.",
                                ));
                            }
                        }
                        popover::Loadable::Error(error) => {
                            models = models
                                .child(widgets::error_strip(&theme, error.clone()))
                                .child(self.choice(
                                    "project-model-retry",
                                    "Retry",
                                    false,
                                    Action::RetryModels,
                                    &theme,
                                    cx,
                                ));
                        }
                        _ => {
                            models = models.child(widgets::page_subtitle(
                                &theme,
                                "Loading models from the project's device…",
                            ));
                        }
                    }
                    row = row.flex_col().items_start().child(models);
                }
                card = card.child(row);
            }
            let mut permissions = Self::choices();
            for (index, (value, label)) in [
                (None, "Inherit global default"),
                (Some(PermissionMode::Auto), "Approve for me"),
                (Some(PermissionMode::Full), "Full access"),
            ]
            .into_iter()
            .enumerate()
            {
                permissions = permissions.child(self.choice(
                    ("project-permission", index),
                    label,
                    defaults.permission_mode == value,
                    Action::Permission(value),
                    &theme,
                    cx,
                ));
            }
            card=card.child(Self::row(&theme,"Permissions","Provider capabilities and machine limits still apply. Unsupported choices block the new task.",false,permissions));
            let mut checkout = Self::choices();
            for (index, (value, label)) in [
                (None, "Inherit global default"),
                (Some(ProjectCheckout::Local), "Current checkout"),
                (Some(ProjectCheckout::NewWorktree), "New worktree"),
            ]
            .into_iter()
            .enumerate()
            {
                checkout = checkout.child(self.choice(
                    ("project-checkout", index),
                    label,
                    defaults.checkout == value,
                    Action::Checkout(value),
                    &theme,
                    cx,
                ));
            }
            card=card.child(Self::row(&theme,"Checkout","Global default uses Current checkout. New worktree uses the existing Git isolation flow.",false,checkout));
            body = body.child(card);
            if defaults.checkout == Some(ProjectCheckout::NewWorktree) && !space.git_detected {
                body = body.child(widgets::warning_strip(
                    &theme,
                    "This folder has no Git checkout. New tasks will require Current checkout.",
                ));
            }
            body = body.child(Self::choices().child(self.choice(
                "project-defaults-reset",
                "Inherit all global defaults",
                defaults.is_inherited(),
                Action::Reset,
                &theme,
                cx,
            )));
            if self.busy {
                body = body.child(widgets::page_subtitle(&theme, "Saving…"));
            } else if self.saved {
                body = body.child(widgets::page_subtitle(&theme, "Saved for new tasks."));
            }
            // ProjectCommandSettings child card is composed here by its owner.
        } else {
            body = body.child(widgets::page_subtitle(
                &theme,
                "Add a project to configure its execution defaults.",
            ));
        }
        if let Some(error) = &self.error {
            body = body.child(widgets::error_strip(&theme, error.clone()));
        }
        let rail = popover::rail(self, "project-defaults-scrollbar", &theme, cx);
        div()
            .size_full()
            .relative()
            .child(
                div()
                    .id("project-defaults-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .on_hover(cx.listener(|page, hovered: &bool, _, cx| {
                        if page.scroll.set_list_hovered(*hovered) {
                            cx.notify();
                        }
                    }))
                    .child(body),
            )
            .children(rail)
    }
}
