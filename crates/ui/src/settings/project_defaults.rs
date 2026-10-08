//! Settings → Projects. The title bar carries the project picker; the page
//! shows the selected project's identity, its defaults for new chats and its
//! saved commands (a child card keyed to the project and engine that own it).
//! The page owns project selection and scrolling.
use crate::{
    button::{self, ControlSize, Segment},
    composer::{ComposerInput, ComposerInputEvent},
    popover,
    project_commands::ProjectCommandSettings,
    settings::{composer::ComposerDefaults, widgets},
    state::AppState,
    theme::Theme,
    typography::{TextStyle, TypeStyleExt as _, ui_rems},
};
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable as _, KeyDownEvent, SharedString,
    Subscription, Task, Window, div, prelude::*, px,
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
    let selected = project.resolve(
        global.harness,
        harness
            .and_then(|harness| global.model_for(harness))
            .map(|model| model.id.as_str()),
        harness.and_then(|harness| global.permission_by_harness.get(&harness).copied()),
    );
    ResolvedNewTaskDefaults {
        harness: selected.harness,
        model: selected.model,
        permission_mode: selected.permission_mode,
        checkout: selected.checkout,
    }
}

/// Width of the Defaults rows' segmented controls, in points at the 16 px
/// interface size (an interface rem, like every control dimension). One width
/// for the three rows keeps their edges aligned. It leaves "Current checkout"
/// room at every Interface size, sits beside the row text in a full 720 pt
/// column, and still fits the column at a ~900 px window and Interface size
/// 20, where the row wraps it under the text.
const SEGMENT_WIDTH: f32 = 360.0;
/// The title bar's project pop-up button.
const PICKER_WIDTH: f32 = 240.0;
/// The floating project menu (wider than its trigger, for device names).
const MENU_WIDTH: f32 = 320.0;
/// Height budget of a menu's scrolling list before it scrolls.
const MENU_LIST_MAX: f32 = 280.0;
/// Model rows rendered at once; the search narrows the rest.
const MODEL_LIMIT: usize = 50;

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

/// Identity of the project and connection that own an in-flight settings request.
#[derive(Clone, PartialEq, Eq)]
struct ProjectRequestScope {
    project_id: String,
    project_device_id: String,
    engine_generation: u64,
    engine_device_id: Option<String>,
}

impl ProjectRequestScope {
    fn current(state: &AppState, project_id: Option<&str>) -> Option<Self> {
        let space = state.space_row(project_id?)?;
        Some(Self {
            project_id: space.id.clone(),
            project_device_id: space.device_id.clone(),
            engine_generation: state.engine_generation(),
            engine_device_id: state
                .engine()
                .map(|engine| engine.engine_info().device_id.clone()),
        })
    }
}

/// What a painted control was painted for. Every Defaults control re-checks
/// it on delivery, so a frame painted for another project or connection can
/// never act on the current one. The engine epoch is kept even when no
/// project is selected, so an old picker frame cannot cross a connection
/// replacement either.
#[derive(Clone)]
struct Admission {
    engine_generation: u64,
    scope: Option<ProjectRequestScope>,
}

/// One entry of the model menu: `None` is the provider's default model.
struct ModelEntry {
    value: Option<String>,
    label: SharedString,
    caption: Option<SharedString>,
    /// Wears the "New" badge beside the label (recently listed models).
    new: bool,
}

pub struct ProjectsPage {
    state: Entity<AppState>,
    selected_project_id: Option<String>,
    request_scope: Option<ProjectRequestScope>,
    command_card_key: Option<(String, String, u64, Option<String>)>,
    command_card: Option<Entity<ProjectCommandSettings>>,
    scroll: widgets::PageScroll,
    models: popover::Loadable<Vec<Model>>,
    model_search: Entity<ComposerInput>,
    model_menu: popover::Popup<()>,
    model_cursor: Option<usize>,
    /// The model pop-up button, so closing its menu from the keyboard hands
    /// focus back to it instead of dropping it with the search field.
    model_trigger: FocusHandle,
    project_menu: popover::Popup<()>,
    project_cursor: Option<usize>,
    busy: bool,
    error: Option<String>,
    saved: bool,
    task: Option<Task<()>>,
    catalog_task: Option<Task<()>>,
    catalog_generation: u64,
    catalog_harness: Option<HarnessId>,
    _observe: Subscription,
    _catalog_observe: Subscription,
    _search: Subscription,
}

impl ProjectsPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let selected_project_id = Self::default_project(state.read(cx));
        let request_scope =
            ProjectRequestScope::current(state.read(cx), selected_project_id.as_deref());
        let observe = cx.observe(&state, |page: &mut Self, _, cx| {
            // A page opened before the first projects frame adopts the
            // selected (or first) project once rows arrive. A chosen project
            // that later vanishes is not silently replaced.
            if page.selected_project_id.is_none() {
                page.selected_project_id = Self::default_project(page.state.read(cx));
            }
            page.retire_changed_scope(cx);
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
                page.model_menu = Default::default();
                page.model_cursor = None;
            }
            cx.notify();
        });
        let model_search =
            cx.new(|cx| ComposerInput::with_context("Search project models…", "PaletteSearch", cx));
        let search = cx.subscribe(&model_search, |page: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                // The rows under the cursor change with the filter.
                page.model_cursor = None;
                cx.notify();
            }
        });
        let catalog_observe =
            cx.observe_global::<crate::pickers::HarnessCatalogChanged>(|page: &mut Self, cx| {
                // Projects is retained across Settings navigation. Its next
                // model-menu opening must read a refreshed engine catalog.
                page.catalog_generation = page.catalog_generation.wrapping_add(1);
                page.catalog_task = None;
                page.models = popover::Loadable::Idle;
                page.model_menu = Default::default();
                page.model_cursor = None;
                cx.notify();
            });
        Self {
            state,
            selected_project_id,
            request_scope,
            command_card_key: None,
            command_card: None,
            scroll: Default::default(),
            models: popover::Loadable::Idle,
            model_search,
            model_menu: Default::default(),
            model_cursor: None,
            model_trigger: cx.focus_handle().tab_stop(true),
            project_menu: Default::default(),
            project_cursor: None,
            busy: false,
            error: None,
            saved: false,
            task: None,
            catalog_task: None,
            catalog_generation: 0,
            catalog_harness: None,
            _observe: observe,
            _catalog_observe: catalog_observe,
            _search: search,
        }
    }

    fn default_project(state: &AppState) -> Option<String> {
        state
            .selected_space
            .clone()
            .or_else(|| state.spaces_sorted().first().map(|s| s.id.clone()))
    }

    /// Stable composition hook: recreate project-owned child cards only when
    /// this ID changes. Children render sections, not another page/scroll view.
    pub fn selected_project_id(&self) -> Option<&str> {
        self.selected_project_id.as_deref()
    }

    fn current_request_scope(&self, cx: &gpui::App) -> Option<ProjectRequestScope> {
        ProjectRequestScope::current(self.state.read(cx), self.selected_project_id.as_deref())
    }

    fn admission(&self, cx: &gpui::App) -> Admission {
        Admission {
            engine_generation: self.state.read(cx).engine_generation(),
            scope: self.current_request_scope(cx),
        }
    }

    fn admits(&self, admission: &Admission, cx: &gpui::App) -> bool {
        self.state.read(cx).engine_generation() == admission.engine_generation
            && self.current_request_scope(cx) == admission.scope
    }

    /// A retained settings page can outlive its engine. Cancel waits owned by
    /// the retired connection and release their UI state without changing the
    /// selected project, project rows, or any composer draft.
    fn retire_changed_scope(&mut self, cx: &gpui::App) {
        let scope = self.current_request_scope(cx);
        if self.request_scope == scope {
            return;
        }
        self.request_scope = scope;
        self.task = None;
        self.busy = false;
        self.error = None;
        self.saved = false;
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        self.catalog_task = None;
        self.catalog_harness = self.selected(cx).and_then(|space| {
            space
                .execution_defaults
                .provider
                .map(|provider| provider.harness)
        });
        self.models = popover::Loadable::Idle;
        // The model list belonged to the retired scope: drop its menu at once.
        self.model_menu = Default::default();
        self.model_cursor = None;
    }

    fn selected(&self, cx: &gpui::App) -> Option<Space> {
        self.selected_project_id
            .as_deref()
            .and_then(|id| self.state.read(cx).space_row(id))
            .cloned()
    }

    fn load_models(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        self.retire_changed_scope(cx);
        let Some(scope) = self.current_request_scope(cx) else {
            return;
        };
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
                if page.current_request_scope(cx).as_ref() != Some(&scope)
                    || page.catalog_generation != generation
                    || page.selected_project_id.as_deref() != Some(project_id.as_str())
                    || page.selected(cx).and_then(|space| {
                        space
                            .execution_defaults
                            .provider
                            .map(|provider| provider.harness)
                    }) != Some(harness)
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
            self.close_project_menu(cx);
            self.selected_project_id = Some(id);
            self.retire_changed_scope(cx);
            self.model_menu = Default::default();
            self.model_cursor = None;
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
            let retry = matches!(action, Action::RetryModels);
            if self.model_menu.is_open() && !retry {
                self.close_model_menu(cx);
            } else if let Some(provider) = &space.execution_defaults.provider {
                if !self.model_menu.is_open() {
                    self.model_menu.open(());
                    self.model_cursor = None;
                    self.model_search
                        .update(cx, |input, cx| input.set_text("", cx));
                }
                // A catalog already loaded for this provider and scope is
                // reused; reopening must not flash the skeleton again.
                let cached = matches!(self.models, popover::Loadable::Ready(_))
                    && self.catalog_harness == Some(provider.harness);
                if retry || !cached {
                    self.load_models(provider.harness, cx);
                }
            }
            cx.notify();
            return;
        }
        let mut defaults = space.execution_defaults.clone();
        match action {
            Action::Provider(harness) => {
                // Re-choosing the current provider must not drop its pinned model.
                if defaults.provider.as_ref().map(|provider| provider.harness) == harness {
                    return;
                }
                self.catalog_generation = self.catalog_generation.wrapping_add(1);
                self.catalog_task = None;
                self.catalog_harness = harness;
                defaults.provider = harness.map(|harness| ProjectProviderDefault {
                    harness,
                    model: None,
                });
                self.models = popover::Loadable::Idle;
                self.model_menu = Default::default();
                self.model_cursor = None;
            }
            Action::Model(model) => {
                if let Some(provider) = &mut defaults.provider {
                    provider.model = model;
                }
                self.close_model_menu(cx);
            }
            Action::Permission(mode) => defaults.permission_mode = mode,
            Action::Checkout(checkout) => defaults.checkout = checkout,
            Action::Reset => {
                defaults = Default::default();
                self.close_model_menu(cx);
            }
            _ => return,
        }
        // Choosing what is already set is not a write.
        if defaults == space.execution_defaults {
            cx.notify();
            return;
        }
        self.save(space, defaults, cx);
    }

    fn save(&mut self, space: Space, defaults: ProjectExecutionDefaults, cx: &mut Context<Self>) {
        self.retire_changed_scope(cx);
        let Some(scope) = self.current_request_scope(cx) else {
            return;
        };
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
                // Check live identity as well as observer cancellation: the
                // old reply can become runnable before the observer executes.
                if page.current_request_scope(cx).as_ref() != Some(&scope) {
                    return;
                }
                page.busy = false;
                match result {
                    Ok(_) => {
                        // The owning engine acknowledged the targeted commit; WatchSpaces
                        // supplies the authoritative projection.
                        // Apply only this owned field so unrelated live state is kept.
                        page.state.update(cx, |state, cx| {
                            if let Some(row) = state.spaces.iter_mut().find(|row| {
                                row.id == scope.project_id
                                    && row.device_id == scope.project_device_id
                            }) {
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

    fn close_model_menu(&mut self, cx: &mut Context<Self>) {
        self.model_cursor = None;
        if self.model_menu.begin_close() {
            popover::reap_popup(cx, |page| &mut page.model_menu);
        }
    }

    fn close_project_menu(&mut self, cx: &mut Context<Self>) {
        if self.project_menu.begin_close() {
            popover::reap_popup(cx, |page| &mut page.project_menu);
        }
    }

    fn open_project_menu(&mut self, ids: &[String], cx: &mut Context<Self>) {
        if self.busy || ids.is_empty() {
            return;
        }
        self.project_cursor = self
            .selected_project_id
            .as_ref()
            .and_then(|id| ids.iter().position(|candidate| candidate == id));
        self.project_menu.open(());
        cx.notify();
    }

    /// Open the model menu with the search field focused (pointer, keyboard
    /// click or an arrow key on the closed trigger).
    fn open_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model_menu.is_open() {
            return;
        }
        self.action(Action::Models, cx);
        if self.model_menu.is_open() {
            window.focus(&self.model_search.read(cx).focus_handle(cx), cx);
        }
    }

    /// The model menu's rows, in order: the provider default, then the
    /// catalog filtered by the search (at most [`MODEL_LIMIT`]). Also returns
    /// how many catalog models matched.
    fn model_entries(&self, cx: &gpui::App) -> (Vec<ModelEntry>, usize) {
        let mut entries = vec![ModelEntry {
            value: None,
            label: "Provider default".into(),
            caption: Some("Follows the provider's catalog".into()),
            new: false,
        }];
        let mut matched = 0;
        if let popover::Loadable::Ready(list) = &self.models {
            let query = self.model_search.read(cx).text().to_lowercase();
            let filtered = list
                .iter()
                .filter(|model| {
                    format!("{} {}", model.id, model.label)
                        .to_lowercase()
                        .contains(&query)
                })
                .collect::<Vec<_>>();
            matched = filtered.len();
            // Rows name the model alone (user request); a recently listed
            // model wears the "New" badge.
            let now = chrono::Utc::now().timestamp_millis();
            entries.extend(
                filtered
                    .into_iter()
                    .take(MODEL_LIMIT)
                    .map(|model| ModelEntry {
                        value: Some(model.id.clone()),
                        label: model.label.clone().into(),
                        caption: None,
                        new: crate::pickers::model_is_new(model, list, now),
                    }),
            );
        }
        (entries, matched)
    }

    /// Keys from the model menu (the search field lets arrows, Enter and
    /// Escape bubble to the card). Returns whether the key was consumed.
    fn on_model_key(
        &mut self,
        event: &KeyDownEvent,
        admission: &Admission,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.model_menu.is_open() {
            return false;
        }
        let modifiers = event.keystroke.modifiers;
        let (entries, _) = self.model_entries(cx);
        match popover::classify_key(&event.keystroke.key, modifiers.platform, modifiers.control) {
            popover::MenuKey::Up => {
                self.model_cursor = popover::menu_step(self.model_cursor, entries.len(), -1);
            }
            popover::MenuKey::Down => {
                self.model_cursor = popover::menu_step(self.model_cursor, entries.len(), 1);
            }
            popover::MenuKey::Enter => {
                if let Some(entry) = self.model_cursor.and_then(|ix| entries.get(ix))
                    && self.admits(admission, cx)
                {
                    self.action(Action::Model(entry.value.clone()), cx);
                    window.focus(&self.model_trigger, cx);
                }
            }
            popover::MenuKey::Escape => {
                self.close_model_menu(cx);
                window.focus(&self.model_trigger, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }
}

// Rendering.
impl ProjectsPage {
    /// The title bar's project pop-up button and its menu.
    fn project_picker(
        &mut self,
        projects: &[Space],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected(cx);
        let label: SharedString = match &selected {
            Some(space) => space.display_name().to_string().into(),
            None if projects.is_empty() => "No projects".into(),
            None => "Choose a project".into(),
        };
        let enabled = !projects.is_empty() && !self.busy;
        let ids: Vec<String> = projects.iter().map(|space| space.id.clone()).collect();
        let admission = self.admission(cx);
        let mut trigger = button::select_frame(
            "project-defaults-project-picker",
            ControlSize::Md,
            self.project_menu.is_open(),
            enabled,
            theme,
        )
        .relative()
        .w(ui_rems(PICKER_WIDTH))
        .aria_label(format!("Project: {label}"))
        .child(
            button::glyph(crate::icons::FOLDER, ControlSize::Md)
                .text_color(theme.readable_muted(0.85)),
        )
        .child(div().flex_1().min_w_0().truncate().child(label))
        .child(button::select_chevron(theme));
        if enabled {
            let click_ids = ids.clone();
            let click_admission = admission.clone();
            let key_ids = ids.clone();
            trigger = trigger
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|page, _, _, _| page.project_menu.note_trigger_press()),
                )
                .on_click(cx.listener(move |page, event: &gpui::ClickEvent, _, cx| {
                    if event.is_keyboard() {
                        // Enter/Space: commit the highlighted row when open.
                        if page.project_menu.is_open() {
                            let pick = page
                                .project_cursor
                                .and_then(|ix| click_ids.get(ix))
                                .cloned();
                            page.close_project_menu(cx);
                            if let Some(id) = pick
                                && page.admits(&click_admission, cx)
                            {
                                page.action(Action::Project(id), cx);
                            }
                        } else {
                            page.open_project_menu(&click_ids, cx);
                        }
                    } else if page.project_menu.take_press_was_open() {
                        // This press's outside-click already closed the menu.
                        page.close_project_menu(cx);
                    } else {
                        page.open_project_menu(&click_ids, cx);
                    }
                    cx.notify();
                }))
                .on_key_down(cx.listener(move |page, event: &KeyDownEvent, _, cx| {
                    let modifiers = event.keystroke.modifiers;
                    let delta = match popover::classify_key(
                        &event.keystroke.key,
                        modifiers.platform,
                        modifiers.control,
                    ) {
                        popover::MenuKey::Up => -1,
                        popover::MenuKey::Down => 1,
                        popover::MenuKey::Escape if page.project_menu.is_open() => {
                            page.close_project_menu(cx);
                            cx.stop_propagation();
                            cx.notify();
                            return;
                        }
                        _ => return,
                    };
                    if page.project_menu.is_open() {
                        page.project_cursor =
                            popover::menu_step(page.project_cursor, key_ids.len(), delta);
                    } else {
                        page.open_project_menu(&key_ids, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }));
        }
        if self.project_menu.get().is_some() {
            let closing = self.project_menu.closing_since();
            let mut rows = Vec::with_capacity(projects.len());
            for (ix, space) in projects.iter().enumerate() {
                let name = space.display_name().to_string();
                let device = self
                    .state
                    .read(cx)
                    .device_name(&space.device_id)
                    .unwrap_or("This device")
                    .to_string();
                let id = space.id.clone();
                let admission = admission.clone();
                rows.push(
                    popover::menu_row_nav(
                        theme,
                        self.selected_project_id.as_deref() == Some(space.id.as_str()),
                        self.project_cursor == Some(ix),
                    )
                    .id(SharedString::from(format!("project-defaults-project-{ix}")))
                    .role(gpui::Role::Button)
                    .aria_label(format!("Project: {name} · {device}"))
                    .on_mouse_move(cx.listener(move |page, _: &gpui::MouseMoveEvent, _, cx| {
                        if page.project_cursor != Some(ix) {
                            page.project_cursor = Some(ix);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |page, _, _, cx| {
                        cx.stop_propagation();
                        page.close_project_menu(cx);
                        if page.admits(&admission, cx) {
                            page.action(Action::Project(id.clone()), cx);
                        }
                        cx.notify();
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(
                        div()
                            .flex_none()
                            .max_w(ui_rems(120.0))
                            .truncate()
                            .type_style(TextStyle::Caption)
                            .text_color(theme.readable_muted(0.8))
                            .child(device),
                    )
                    .into_any_element(),
                );
            }
            let menu = popover::popover_card(theme)
                .w(ui_rems(MENU_WIDTH))
                .on_mouse_down_out(cx.listener(|page, _, _, cx| {
                    page.close_project_menu(cx);
                    cx.notify();
                }))
                .flex()
                .flex_col()
                .child(popover::menu_heading(theme, "Projects"))
                .child(
                    div()
                        .id("project-defaults-project-list")
                        .max_h(ui_rems(MENU_LIST_MAX))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(popover::MENU_GAP))
                        .children(rows),
                )
                .into_any_element();
            trigger = trigger.child(popover::anchored_menu_below_end(
                "project-defaults-project-menu",
                menu,
                closing,
            ));
        }
        trigger.into_any_element()
    }

    /// The selected project's identity: name, folder, device and Git state.
    /// A single informational row, so the card needs no header.
    fn identity_section(&self, space: &Space, theme: &Theme, cx: &gpui::App) -> gpui::Div {
        let device = self
            .state
            .read(cx)
            .device_name(&space.device_id)
            .unwrap_or("This device")
            .to_string();
        widgets::section_card(theme).child(
            widgets::card_row(theme, true)
                .child(widgets::row_tile(theme, crate::icons::FOLDER))
                .child(widgets::row_text(
                    theme,
                    space.display_name().to_string(),
                    Some(space.path.clone().into()),
                ))
                .child(
                    widgets::row_controls()
                        .child(widgets::badge(theme, device))
                        .child(widgets::badge(
                            theme,
                            if space.git_detected { "Git" } else { "No Git" },
                        )),
                ),
        )
    }

    /// A Defaults row whose choice is a segmented control. Each option's
    /// action is admitted against the frame it was painted in. `probe` names
    /// one segment for the rendered-input tests (see [`segment_probe`]).
    #[allow(clippy::too_many_arguments)]
    fn segmented_row(
        &self,
        id: &'static str,
        title: &'static str,
        description: &'static str,
        options: Vec<(&'static str, SharedString, Action)>,
        selected: usize,
        probe: Option<(usize, &'static str)>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let page = cx.entity().downgrade();
        let admission = self.admission(cx);
        let count = options.len();
        let mut segments = Vec::with_capacity(count);
        let mut actions = Vec::with_capacity(count);
        for (key, label, action) in options {
            segments.push(Segment::new(key, label));
            actions.push(action);
        }
        let control = button::segmented(
            id,
            segments,
            selected,
            SEGMENT_WIDTH,
            ControlSize::Md,
            theme,
            move |ix, _, cx| {
                let Some(action) = actions.get(*ix).cloned() else {
                    return;
                };
                page.update(cx, |page, cx| {
                    if page.admits(&admission, cx) {
                        page.action(action, cx);
                    }
                })
                .ok();
            },
        )
        .aria_label(title)
        // A save in flight holds every choice (`action` ignores input while
        // busy); show it the way a disabled control looks.
        .when(self.busy, |control| control.opacity(0.45));
        widgets::card_row(theme, false)
            .child(widgets::row_text(theme, title, Some(description.into())))
            .child(widgets::row_controls().child(segment_probe(control, count, probe)))
    }

    /// The Model row: a pop-up button over the provider's catalog.
    fn model_row(
        &mut self,
        provider: &ProjectProviderDefault,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let label: SharedString = match provider.model.as_deref() {
            None => "Provider default".into(),
            Some(id) => self
                .models
                .ready()
                .and_then(|models| models.iter().find(|model| model.id == id))
                .map(|model| model.label.clone())
                .unwrap_or_else(|| id.to_string())
                .into(),
        };
        let admission = self.admission(cx);
        let open = self.model_menu.is_open();
        let mut trigger = button::select_frame(
            "project-model-picker",
            ControlSize::Md,
            open,
            !self.busy,
            theme,
        )
        .relative()
        // The segmented rows' width, so the Defaults controls share edges.
        .w(ui_rems(SEGMENT_WIDTH))
        .track_focus(&self.model_trigger)
        .aria_label(format!("Model: {label}"))
        .child(div().flex_1().min_w_0().truncate().child(label))
        .child(button::select_chevron(theme));
        if !self.busy {
            let click_admission = admission.clone();
            trigger = trigger
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|page, _, _, _| page.model_menu.note_trigger_press()),
                )
                .on_click(
                    cx.listener(move |page, event: &gpui::ClickEvent, window, cx| {
                        if !page.admits(&click_admission, cx) {
                            return;
                        }
                        if event.is_keyboard() {
                            if page.model_menu.is_open() {
                                page.close_model_menu(cx);
                            } else {
                                page.open_model_menu(window, cx);
                            }
                        } else if page.model_menu.take_press_was_open() {
                            page.close_model_menu(cx);
                        } else {
                            page.open_model_menu(window, cx);
                        }
                        cx.notify();
                    }),
                )
                // The closed trigger opens on an arrow key; once open, the
                // menu card owns the keys and stops them before they get here.
                .on_key_down(cx.listener(|page, event: &KeyDownEvent, window, cx| {
                    if page.model_menu.is_open() {
                        return;
                    }
                    if matches!(event.keystroke.key.as_str(), "up" | "down") {
                        page.open_model_menu(window, cx);
                        cx.stop_propagation();
                    }
                }));
        }
        if self.model_menu.get().is_some() {
            let closing = self.model_menu.closing_since();
            let menu = self.model_menu_card(provider, &admission, theme, cx);
            trigger = trigger.child(popover::anchored_menu_below_end(
                "project-model-menu",
                menu,
                closing,
            ));
        }
        widgets::card_row(theme, false)
            .child(widgets::row_text(
                theme,
                "Model",
                Some(
                    "Provider default follows the provider's catalog. A pinned model must stay available."
                        .into(),
                ),
            ))
            .child(widgets::row_controls().child(trigger))
    }

    fn model_menu_card(
        &mut self,
        provider: &ProjectProviderDefault,
        admission: &Admission,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (entries, matched) = self.model_entries(cx);
        let mut rows = Vec::with_capacity(entries.len());
        for (ix, entry) in entries.into_iter().enumerate() {
            let selected = provider.model == entry.value;
            let value = entry.value.clone();
            let admission = admission.clone();
            rows.push(
                popover::menu_row_nav(theme, selected, self.model_cursor == Some(ix))
                    .id(SharedString::from(match &entry.value {
                        None => "project-model-default".to_string(),
                        Some(_) => format!("project-model-{}", ix - 1),
                    }))
                    .role(gpui::Role::Button)
                    .aria_label(format!("Model: {}", entry.label))
                    .on_mouse_move(cx.listener(move |page, _: &gpui::MouseMoveEvent, _, cx| {
                        if page.model_cursor != Some(ix) {
                            page.model_cursor = Some(ix);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |page, _, window, cx| {
                        cx.stop_propagation();
                        if page.admits(&admission, cx) {
                            page.action(Action::Model(value.clone()), cx);
                            window.focus(&page.model_trigger, cx);
                        }
                    }))
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
                                    .child(div().min_w_0().truncate().child(entry.label))
                                    .when(entry.new, |line| {
                                        line.child(crate::pickers::new_model_badge(&theme))
                                    }),
                            )
                            .when_some(entry.caption, |column, caption| {
                                column.child(
                                    div()
                                        .truncate()
                                        .type_style(TextStyle::Caption)
                                        .text_color(theme.readable_muted(0.8))
                                        .child(caption),
                                )
                            }),
                    )
                    .into_any_element(),
            );
        }
        let note = |copy: SharedString| {
            div()
                .px(ui_rems(8.0))
                .py(ui_rems(6.0))
                .type_style(TextStyle::Footnote)
                .text_color(theme.readable_muted(0.8))
                .child(copy)
        };
        let mut list = div()
            .id("project-model-list")
            .max_h(ui_rems(MENU_LIST_MAX))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(popover::MENU_GAP))
            .children(rows);
        match &self.models {
            popover::Loadable::Ready(_) if matched == 0 => {
                list = list.child(note(
                    "No available models. Refresh Providers or refine the search.".into(),
                ));
            }
            popover::Loadable::Ready(_) if matched > MODEL_LIMIT => {
                list = list.child(note(
                    format!(
                        "Showing {MODEL_LIMIT} of {matched} models. Search to narrow the list."
                    )
                    .into(),
                ));
            }
            popover::Loadable::Ready(_) => {}
            popover::Loadable::Error(error) => {
                let retry_admission = admission.clone();
                list = list.child(
                    div()
                        .px(ui_rems(4.0))
                        .py(ui_rems(4.0))
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(ui_rems(8.0))
                        .child(widgets::error_strip(theme, error.clone()).mt_0().w_full())
                        .child(
                            button::button(
                                "project-model-retry",
                                button::ButtonKind::Secondary,
                                ControlSize::Sm,
                                !self.busy,
                                theme,
                            )
                            .aria_label("Retry loading models")
                            .child(button::glyph(crate::icons::REFRESH, ControlSize::Sm))
                            .child("Retry")
                            .on_click(cx.listener(
                                move |page, _, _, cx| {
                                    cx.stop_propagation();
                                    if page.admits(&retry_admission, cx) {
                                        page.action(Action::RetryModels, cx);
                                    }
                                },
                            )),
                        ),
                );
            }
            popover::Loadable::Idle | popover::Loadable::Loading => {
                list = list.child(popover::skeleton_menu_rows(
                    "project-model-loading",
                    theme,
                    4,
                    cx.entity_id(),
                    cx,
                ));
            }
        }
        let key_admission = admission.clone();
        popover::popover_card(theme)
            // At least as wide as its trigger, like a native pop-up menu.
            .w(ui_rems(SEGMENT_WIDTH))
            .on_mouse_down_out(cx.listener(|page, _, _, cx| {
                page.close_model_menu(cx);
                cx.notify();
            }))
            .on_key_down(cx.listener(move |page, event: &KeyDownEvent, window, cx| {
                if page.on_model_key(event, &key_admission, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .child(popover::search_input_frame(
                theme,
                self.model_search.clone().into_any_element(),
            ))
            .child(list)
            .into_any_element()
    }

    /// New chat defaults: provider, model, permissions and checkout, with the
    /// reset and save status in the header and errors inside the card.
    fn defaults_section(
        &mut self,
        space: &Space,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let defaults = space.execution_defaults.clone();
        let admission = self.admission(cx);
        let mut actions = Vec::new();
        let status = if self.busy {
            Some("Saving…")
        } else if self.saved {
            Some("Saved")
        } else {
            None
        };
        if let Some(status) = status {
            actions.push(
                div()
                    .type_style(TextStyle::Caption)
                    .text_color(theme.readable_muted(0.8))
                    .child(status)
                    .into_any_element(),
            );
        }
        let can_reset = !self.busy && !defaults.is_inherited();
        actions.push(
            widgets::ghost_action("project-defaults-reset", can_reset, theme)
                .aria_label("Use global defaults for this project")
                .child("Use global defaults")
                .when(can_reset, |el| {
                    el.on_click(cx.listener(move |page, _, _, cx| {
                        if page.admits(&admission, cx) {
                            page.action(Action::Reset, cx);
                        }
                    }))
                })
                .into_any_element(),
        );
        let mut card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "New chat defaults",
            Some(
                "Apply to new chats started in this project from the desktop app. Existing tasks keep their own settings."
                    .into(),
            ),
            actions,
        ));

        // Provider.
        let mut providers: Vec<(&'static str, SharedString, Action)> = vec![
            ("global", "Global".into(), Action::Provider(None)),
            (
                "codex",
                "Codex".into(),
                Action::Provider(Some(HarnessId::Codex)),
            ),
            (
                "claude-code",
                "Claude Code".into(),
                Action::Provider(Some(HarnessId::ClaudeCode)),
            ),
            (
                "opencode",
                "OpenCode".into(),
                Action::Provider(Some(HarnessId::Opencode)),
            ),
        ];
        let current = defaults.provider.as_ref().map(|provider| provider.harness);
        let provider_ix = match current {
            None => 0,
            Some(HarnessId::Codex) => 1,
            Some(HarnessId::ClaudeCode) => 2,
            Some(HarnessId::Opencode) => 3,
            // A legacy provider set elsewhere still shows as the choice.
            Some(other) => {
                providers.push((
                    "other",
                    format!("{other:?}").into(),
                    Action::Provider(Some(other)),
                ));
                providers.len() - 1
            }
        };
        card = card.child(self.segmented_row(
            "project-provider",
            "Provider",
            "Global uses your app-wide provider and remembered model. Choose one to override it here.",
            providers,
            provider_ix,
            None,
            theme,
            cx,
        ));
        if let Some(provider) = &defaults.provider {
            card = card.child(self.model_row(provider, theme, cx));
        }

        // Permissions.
        let mut permissions: Vec<(&'static str, SharedString, Action)> = vec![
            ("global", "Global".into(), Action::Permission(None)),
            (
                "auto",
                "Approve for me".into(),
                Action::Permission(Some(PermissionMode::Auto)),
            ),
            (
                "full",
                "Full access".into(),
                Action::Permission(Some(PermissionMode::Full)),
            ),
        ];
        let permission_ix = match defaults.permission_mode {
            None => 0,
            Some(PermissionMode::Auto) => 1,
            Some(PermissionMode::Full) => 2,
            Some(PermissionMode::Ask) => {
                permissions.push((
                    "ask",
                    "Ask".into(),
                    Action::Permission(Some(PermissionMode::Ask)),
                ));
                3
            }
        };
        card = card.child(self.segmented_row(
            "project-permission",
            "Permissions",
            "Provider capabilities and machine limits still apply. A choice the provider can't honour blocks the new chat.",
            permissions,
            permission_ix,
            None,
            theme,
            cx,
        ));

        // Checkout.
        let checkout_ix = match defaults.checkout {
            None => 0,
            Some(ProjectCheckout::Local) => 1,
            Some(ProjectCheckout::NewWorktree) => 2,
        };
        let mut checkout = self.segmented_row(
            "project-checkout",
            "Checkout",
            "Global uses Current checkout. New worktree uses the Git isolation flow.",
            vec![
                ("global", "Global".into(), Action::Checkout(None)),
                (
                    "current",
                    "Current checkout".into(),
                    Action::Checkout(Some(ProjectCheckout::Local)),
                ),
                (
                    "new-worktree",
                    "New worktree".into(),
                    Action::Checkout(Some(ProjectCheckout::NewWorktree)),
                ),
            ],
            checkout_ix,
            Some((2, "qa-defaults-new-worktree")),
            theme,
            cx,
        );
        if defaults.checkout == Some(ProjectCheckout::NewWorktree) && !space.git_detected {
            checkout = checkout.child(
                widgets::warning_strip(
                    theme,
                    "This folder has no Git checkout. New tasks will require Current checkout.",
                )
                .mt_0()
                .w_full(),
            );
        }
        card = card.child(checkout);
        if let Some(error) = &self.error {
            card = card.child(
                widgets::card_row(theme, false)
                    .child(widgets::error_strip(theme, error.clone()).mt_0().w_full()),
            );
        }
        card
    }

    /// The page body when no project is shown: loading, none yet, or the
    /// chosen project has gone.
    fn placeholder_section(
        &self,
        no_projects: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let state = self.state.read(cx);
        let loading = no_projects && !state.spaces_synced && state.engine().is_some();
        let card = widgets::section_card(theme);
        if loading {
            return card.child(
                div()
                    .px(ui_rems(widgets::ROW_PAD_X))
                    .py(ui_rems(widgets::ROW_PAD_Y))
                    .child(popover::skeleton_rows(
                        "project-defaults-loading",
                        theme,
                        3,
                        cx.entity_id(),
                        cx,
                    )),
            );
        }
        let (title, description) = if no_projects {
            (
                "No projects yet",
                "Add a project to set the defaults and commands its new chats use.",
            )
        } else {
            (
                "Choose a project",
                "Pick a project from the menu above to edit its defaults and commands.",
            )
        };
        card.child(widgets::card_row(theme, true).child(widgets::row_text(
            theme,
            title,
            Some(description.into()),
        )))
    }
}

/// Wraps a segmented control so a rendered-input test can find one segment
/// by `selector`. The kit's segments carry no debug selector of their own,
/// so in test builds an absolutely placed, listener-free box covers segment
/// `index` of `count` (segments share the width equally). It inserts no
/// hitbox, so input still reaches the real segment beneath it, and it does
/// not take part in layout.
fn segment_probe(
    control: gpui::Stateful<gpui::Div>,
    count: usize,
    probe: Option<(usize, &'static str)>,
) -> gpui::Div {
    let count = count.max(1) as f32;
    div().relative().flex_none().child(control).when_some(
        probe.filter(|_| cfg!(test)),
        |wrapper, (index, selector)| {
            wrapper.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(gpui::relative(index as f32 / count))
                    .w(gpui::relative(1.0 / count))
                    .debug_selector(move || selector.into()),
            )
        },
    )
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
        // Commands are device-local. Keep the editor only for this exact
        // project/backend identity; dropping it cancels any stale card request.
        let command_card_key = self.selected(cx).map(|space| {
            let state = self.state.read(cx);
            (
                space.id,
                space.device_id,
                state.engine_generation(),
                state
                    .engine()
                    .map(|engine| engine.engine_info().device_id.clone()),
            )
        });
        if self.command_card_key != command_card_key {
            self.command_card_key = command_card_key.clone();
            self.command_card = command_card_key.map(|(space_id, _, _, _)| {
                cx.new(|cx| ProjectCommandSettings::new(self.state.clone(), space_id, cx))
            });
        }
        let theme = Theme::of(cx).clone();
        let projects = self
            .state
            .read(cx)
            .spaces_sorted()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let picker = self.project_picker(&projects, &theme, cx);
        let mut body = widgets::page_column()
            .child(widgets::page_title_bar(&theme, "Projects", None, vec![picker]))
            .child(widgets::page_subtitle(
                &theme,
                "Choose what new chats in each project start with, and the commands you can run in its checkouts.",
            ));
        if let Some(space) = self.selected(cx) {
            body = body
                .child(self.identity_section(&space, &theme, cx))
                .child(self.defaults_section(&space, &theme, cx));
            if let Some(commands) = &self.command_card {
                body = body.child(commands.clone());
            }
        } else {
            body = body.child(self.placeholder_section(projects.is_empty(), &theme, cx));
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

#[cfg(test)]
#[path = "project_defaults_engine_epoch_tests.rs"]
mod engine_epoch_tests;

#[cfg(test)]
#[path = "project_defaults_button_admission_tests.rs"]
mod button_admission_tests;
