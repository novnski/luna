//! New ticket (A9): the chooser between the two distinct creation
//! experiences (Write manually / Discuss with AI, plus From GitHub issue),
//! and the Write manually form: title, description (acceptance criteria are a
//! section of it, not a mandatory field) and the implementation/review
//! settings. Save to Backlog calls `BoardApi::create`: it creates no chat and
//! starts nothing, and its receipt stays visible. Discuss with AI opens the
//! temporary authoring pane (A6), which has no title/description fields.
use gpui::{
    AnyElement, App, Context, Entity, EntityInputHandler as _, EventEmitter, FocusHandle,
    Focusable as _, IntoElement, ParentElement, Render, ScrollHandle, SharedString, Styled,
    Subscription, Task, Window, div, prelude::*, px,
};
use luna_proto::{
    BOARD_DESCRIPTION_MAX_BYTES, BOARD_TITLE_MAX_CHARS, BoardCreateRequest, BoardError,
    BoardItemOrigin, BoardMutationReceipt, BoardPlacement, BoardReceiptRequest, BoardReviewProfile,
    BoardScope,
};

use super::super::project_picker::{
    ProjectPicker, ProjectPickerEvent, ProjectPickerIds, ProjectPickerOptions, ProjectPickerRow,
    ProjectSource,
};
use super::super::sidebar_projects::{project_tile, project_tint};
use super::api::BoardCallError;
use super::detail::{BoardProbe, new_request_id};
use super::model::BoardModel;
use super::page::ticket_hints;
use super::settings_controls::{
    ExecutionSettingsDraft, ExecutionSettingsEditor, SettingsEditorEvent, SettingsRow, Which,
    describe_implementation, describe_review,
};
use crate::AccessibleButton as _;
use crate::button::{self, ButtonKind, ControlSize};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::popover;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::{FillTier, Theme};
use crate::typography::ui_rems;

/// Generate title is offered once the brief has this many characters.
const TITLE_FROM_BRIEF_CHARS: usize = 100;

/// The section "Add acceptance criteria" appends to the description.
const CRITERIA_SECTION: &str = "## Acceptance criteria\n- ";

// ---------------------------------------------------------------------------
// Chooser
// ---------------------------------------------------------------------------

/// What the chooser asks its host to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChooserEvent {
    WriteManually { project_id: String },
    DiscussWithAi { project_id: String },
    FromGitHub { project_id: String },
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChooserKind {
    Manual,
    Ai,
    GitHub,
}

/// New ticket: two distinct experiences (and the GitHub import), never a
/// form beside a conversation. The ticket's project is the Board's: the
/// sidebar's project when it names one (shown, never changeable here), and
/// only under All projects a choice in the shared project picker, which
/// never guesses one.
pub struct NewTicketChooser {
    state: Entity<AppState>,
    model: Entity<BoardModel>,
    probe: BoardProbe,
    /// The choice under All projects (kept, as a preselection, when the
    /// scope narrows and widens again).
    project_id: Option<String>,
    picker: popover::Popup<ChooserPicker>,
    error: Option<String>,
    focus: FocusHandle,
}

/// The open project picker and its subscription.
struct ChooserPicker {
    picker: Entity<ProjectPicker>,
    _events: Subscription,
}

impl EventEmitter<ChooserEvent> for NewTicketChooser {}

impl NewTicketChooser {
    pub fn new(
        state: Entity<AppState>,
        model: Entity<BoardModel>,
        preselect: Option<String>,
        probe: BoardProbe,
        cx: &mut Context<Self>,
    ) -> Self {
        let bound = model.read(cx).scope().project_id().map(str::to_owned);
        Self {
            state,
            model,
            probe,
            project_id: bound.or(preselect),
            picker: Default::default(),
            error: None,
            focus: cx.focus_handle(),
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The Board's project, when it shows one: every new ticket goes there.
    fn bound(&self, cx: &App) -> Option<String> {
        self.model.read(cx).scope().project_id().map(str::to_owned)
    }

    /// The project the ticket will belong to.
    fn chosen(&self, cx: &App) -> Option<String> {
        self.bound(cx).or_else(|| self.project_id.clone())
    }

    /// The Board's scope changed under the open chooser: a project scope
    /// takes over the choice (and closes the picker).
    pub fn follow_scope(&mut self, scope: &BoardScope, cx: &mut Context<Self>) {
        if let Some(project_id) = scope.project_id() {
            self.project_id = Some(project_id.to_owned());
            self.error = None;
            self.close_picker(cx);
        }
        cx.notify();
    }

    fn choose(&mut self, kind: ChooserKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_id) = self.chosen(cx) else {
            self.error = Some("Choose a project for this ticket first.".into());
            self.open_picker(window, cx);
            cx.notify();
            return;
        };
        cx.emit(match kind {
            ChooserKind::Manual => ChooserEvent::WriteManually { project_id },
            ChooserKind::Ai => ChooserEvent::DiscussWithAi { project_id },
            ChooserKind::GitHub => ChooserEvent::FromGitHub { project_id },
        });
    }

    fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bound(cx).is_some() || self.picker.is_open() {
            return;
        }
        let state = self.state.clone();
        let hints = ticket_hints(self.state.read(cx), self.model.read(cx));
        let options = ProjectPickerOptions {
            source: ProjectSource::ThisHost,
            selected: self.project_id.clone(),
            all_row: false,
            footer: false,
            context_menu: false,
            ids: ProjectPickerIds::BOARD,
            width: px(popover::MENU_W_L),
        };
        let picker = cx.new(|cx| {
            let mut picker = ProjectPicker::new(state, options, cx);
            picker.set_hints(hints);
            picker
        });
        let events = cx.subscribe(&picker, |this: &mut Self, _, event, cx| match event {
            ProjectPickerEvent::Chose(ProjectPickerRow::Space(project_id)) => {
                this.project_id = Some(project_id.clone());
                this.error = None;
                this.close_picker(cx);
            }
            ProjectPickerEvent::Chose(_) | ProjectPickerEvent::Dismissed => this.close_picker(cx),
            ProjectPickerEvent::ContextMenu { .. } => {}
        });
        let focus = picker.read(cx).search_focus(cx);
        self.picker.open(ChooserPicker {
            picker,
            _events: events,
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    fn close_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.begin_close() {
            popover::reap_popup(cx, |chooser: &mut Self| &mut chooser.picker);
            cx.notify();
        }
    }

    /// The project's tile and name (`None`: nothing chosen yet).
    fn project_identity(&self, project_id: &str, theme: &Theme, cx: &App) -> (AnyElement, String) {
        let state = self.state.read(cx);
        let name = state
            .space_row(project_id)
            .map(|space| space.display_name().to_owned())
            .unwrap_or_else(|| "Unknown project".into());
        let tile = project_tile(project_tint(state, project_id), &name, 20.0, theme);
        (tile.into_any_element(), name)
    }

    /// Under All projects: the dropdown that opens the project picker.
    fn render_project_select(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let open = self.picker.is_open();
        let chosen = self.project_id.clone();
        let label = chosen
            .as_deref()
            .map(|id| self.project_identity(id, theme, cx));
        let text = label
            .as_ref()
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| "Choose a project".into());
        self.probe.text("board-chooser-project", text.clone());
        let mut trigger =
            button::select_frame("board-chooser-project", ControlSize::Md, open, true, theme)
                .aria_label(SharedString::from(format!("Project: {text}")))
                .relative()
                .min_w(ui_rems(200.0))
                .max_w(ui_rems(280.0))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, _| this.picker.note_trigger_press()),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.picker.take_press_was_open() {
                        return;
                    }
                    this.open_picker(window, cx);
                }));
        trigger = match label {
            Some((tile, _)) => trigger.child(tile),
            None => trigger.child(button::glyph(crate::icons::FOLDER, ControlSize::Md)),
        };
        trigger = trigger
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(chosen.is_none(), |el| {
                        el.text_color(theme.readable_muted(0.85))
                    })
                    .child(SharedString::from(text)),
            )
            .child(button::select_chevron(theme))
            .children(self.probe.bounds_el("board-chooser-project"));
        if let Some(open) = self.picker.get() {
            let closing = self.picker.closing_since();
            trigger = trigger.child(popover::anchored_menu_below(
                "board-chooser-project-menu",
                open.picker.clone().into_any_element(),
                closing,
            ));
        }
        trigger.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn option_card(
        &self,
        id: &'static str,
        icon: &'static str,
        title: &'static str,
        body: &'static str,
        kind: ChooserKind,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.probe.text(id, title);
        div()
            .id(id)
            .accessible_button(title, theme.focus_ring())
            .relative()
            .w_full()
            .px(ui_rems(14.0))
            .py(ui_rems(12.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.separator(true))
            .bg(theme.fill(FillTier::Quaternary))
            .hover(|style| style.bg(theme.fill(FillTier::Tertiary)))
            .cursor_pointer()
            .flex()
            .flex_row()
            .items_start()
            .gap(ui_rems(12.0))
            .on_click(cx.listener(move |this, _, window, cx| this.choose(kind, window, cx)))
            .child(widgets::row_tile(theme, icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(ui_rems(2.0))
                    .child(widgets::row_title(theme, title))
                    .child(widgets::row_description(theme, body)),
            )
            .children(self.probe.bounds_el(id))
            .into_any_element()
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_state(&self, cx: &App) -> serde_json::Value {
        serde_json::json!({
            "projectId": self.chosen(cx),
            "bound": self.bound(cx),
            "error": self.error,
            "picker": self.picker.as_open().map(|open| open.picker.read(cx).fixture_readback(cx)),
        })
    }
}

impl Render for NewTicketChooser {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut root = div()
            .id("board-chooser")
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .px(ui_rems(20.0))
            .py(ui_rems(18.0))
            .flex()
            .flex_col()
            .gap(ui_rems(12.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(ui_rems(8.0))
                    .child(widgets::page_header(&theme, "New ticket", None))
                    .child(
                        button::icon_button(
                            "board-chooser-close",
                            crate::icons::CLOSE,
                            ControlSize::Md,
                            true,
                            &theme,
                        )
                        .aria_label("Close")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ChooserEvent::Close)))
                        .children(self.probe.bounds_el("board-chooser-close")),
                    ),
            )
            .child(widgets::page_subtitle(
                &theme,
                "A ticket describes what should be achieved. Saving one starts nothing; moving \
                 it to Ready starts the work.",
            ));
        let project = match self.bound(cx) {
            // The sidebar names the project: every ticket made here is its.
            Some(project_id) => {
                let (tile, name) = self.project_identity(&project_id, &theme, cx);
                self.probe.text("board-chooser-project", name.clone());
                div()
                    .id("board-chooser-project")
                    .aria_label(SharedString::from(format!("Project: {name}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(8.0))
                    .text_size(ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tile)
                    .child(div().min_w_0().truncate().child(SharedString::from(name)))
                    .children(self.probe.bounds_el("board-chooser-project"))
                    .into_any_element()
            }
            None => self.render_project_select(&theme, cx),
        };
        let hint = if self.bound(cx).is_some() {
            "The project selected in the sidebar. Choose All projects there to make a ticket \
             for another project."
        } else {
            "Each ticket belongs to one project on this Mac."
        };
        root = root.child(
            widgets::section_card(&theme).mt_0().child(
                widgets::card_row(&theme, true)
                    .child(widgets::row_text(&theme, "Project", Some(hint.into())))
                    .child(project),
            ),
        );
        if let Some(error) = &self.error {
            self.probe.text("board-chooser-error", error.clone());
            root = root.child(widgets::error_strip(&theme, error.clone()).mt_0());
        }
        root.child(self.option_card(
            "board-chooser-manual",
            crate::icons::PEN_NEW_SQUARE,
            "Write manually",
            "Write the title and brief yourself and choose how it will run. Saving creates no \
             chat.",
            ChooserKind::Manual,
            &theme,
            cx,
        ))
        .child(self.option_card(
            "board-chooser-ai",
            crate::icons::CHAT_ROUND_LINE,
            "Discuss with AI",
            "A temporary, project-aware conversation works out the ticket with you. It never \
             appears among your chats; closing it keeps only a Board draft.",
            ChooserKind::Ai,
            &theme,
            cx,
        ))
        .child(self.option_card(
            "board-chooser-github",
            crate::icons::FILE_SEARCH,
            "From a GitHub issue",
            "Import an issue from this project's repository as a ticket.",
            ChooserKind::GitHub,
            &theme,
            cx,
        ))
    }
}

// ---------------------------------------------------------------------------
// Write manually
// ---------------------------------------------------------------------------

/// What the form asks its host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualFormEvent {
    /// Open the saved ticket's detail drawer.
    OpenTicket(String),
    /// Back to the chooser.
    Back,
    Close,
}

enum SavePhase {
    Editing,
    Saving,
    /// The answer was lost; reconciling by the original request id.
    Reconciling,
    Saved(Box<BoardMutationReceipt>),
}

impl SavePhase {
    #[cfg(feature = "appshots-fixture")]
    fn key(&self) -> &'static str {
        match self {
            Self::Editing => "editing",
            Self::Saving => "saving",
            Self::Reconciling => "reconciling",
            Self::Saved(_) => "saved",
        }
    }
}

#[derive(Default)]
struct FieldErrors {
    project: Option<String>,
    title: Option<String>,
    description: Option<String>,
    settings: Option<String>,
    general: Option<String>,
}

impl FieldErrors {
    fn any(&self) -> bool {
        self.project.is_some()
            || self.title.is_some()
            || self.description.is_some()
            || self.settings.is_some()
            || self.general.is_some()
    }
}

/// The manual ticket form for one project.
pub struct ManualTicketForm {
    pub(crate) model: Entity<BoardModel>,
    pub(crate) project_id: Option<String>,
    state: Entity<AppState>,
    probe: BoardProbe,
    title: Entity<ComposerInput>,
    /// The brief: the chat composer in form mode (attachments and dropped
    /// images, `@` files, `$` skills, `/` commands; Enter breaks the line),
    /// whose pickers are the ticket's agent pickers.
    description: Entity<crate::composer::Composer>,
    /// Images already uploaded for this form (attachment id → upload path),
    /// so a retried save sends the same paths and never duplicates.
    uploaded: std::collections::HashMap<String, String>,
    /// The brief composer's measured width (it lays out like a pane's).
    composer_width: std::rc::Rc<std::cell::Cell<f32>>,
    /// Drafting a title from the brief (the model that names chats).
    title_task: Option<Task<()>>,
    settings: Option<Entity<ExecutionSettingsEditor>>,
    /// The editor's rows: the agent inside the brief's card, the reviewer
    /// below it.
    settings_rows: Option<(Entity<SettingsRow>, Entity<SettingsRow>)>,
    phase: SavePhase,
    /// Reused verbatim after a lost answer, so a retry can never create a
    /// second ticket.
    pending_request: Option<String>,
    errors: FieldErrors,
    scroll: ScrollHandle,
    save_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ManualFormEvent> for ManualTicketForm {}

impl ManualTicketForm {
    pub fn new(
        model: Entity<BoardModel>,
        state: Entity<AppState>,
        project_id: Option<String>,
        probe: BoardProbe,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = cx.new(|cx| {
            ComposerInput::new("Ticket title, e.g. “Retry failed uploads”", cx)
                .with_single_line()
                .with_text_metrics(15.0, 21.0)
                .with_accessibility_role(gpui::Role::TextInput)
        });
        let binding = crate::chat_binding::ChatBinding::Detached {
            space_id: project_id.clone().unwrap_or_default(),
        };
        let description = cx.new(|cx| {
            let mut composer = crate::composer::Composer::new_bound(state.clone(), binding, cx);
            composer.set_placeholder(
                "Describe the outcome, the context and how to verify it…",
                cx,
            );
            composer.set_detached_form(cx);
            composer
        });
        let mut subscriptions = vec![
            cx.subscribe_in(&title, window, |this, _, event, window, cx| match event {
                // Enter in the title moves on to the brief.
                ComposerInputEvent::Submitted => {
                    let focus = gpui::Focusable::focus_handle(this.description.read(cx), cx);
                    window.focus(&focus, cx);
                }
                ComposerInputEvent::Edited if this.errors.title.take().is_some() => cx.notify(),
                _ => {}
            }),
            // An edit clears the brief's error and may enable Generate title.
            cx.observe(&description, |this, _, cx| {
                this.errors.description = None;
                cx.notify();
            }),
        ];
        let agent_pickers = description.read(cx).pickers().clone();
        let settings = project_id.clone().map(|project| {
            let editor = cx.new(|cx| {
                ExecutionSettingsEditor::new(
                    model.clone(),
                    state.clone(),
                    project,
                    ExecutionSettingsDraft::default(),
                    "manual",
                    probe.clone(),
                    Some(agent_pickers.clone()),
                    cx,
                )
            });
            subscriptions.push(
                cx.subscribe(&editor, |this, _, _: &SettingsEditorEvent, cx| {
                    if this.errors.settings.take().is_some() {
                        cx.notify();
                    }
                }),
            );
            editor
        });
        let settings_rows = settings.as_ref().map(|editor| {
            (
                cx.new(|cx| SettingsRow::new(editor.clone(), Which::Implementation, true, cx)),
                cx.new(|cx| SettingsRow::new(editor.clone(), Which::Review, true, cx)),
            )
        });
        Self {
            model,
            project_id,
            state,
            probe,
            title,
            description,
            uploaded: Default::default(),
            composer_width: Default::default(),
            title_task: None,
            settings,
            settings_rows,
            phase: SavePhase::Editing,
            pending_request: None,
            errors: FieldErrors::default(),
            scroll: ScrollHandle::new(),
            save_task: None,
            _subscriptions: subscriptions,
        }
    }

    /// The title's focus handle (the form's first field).
    pub fn title_focus(&self, cx: &App) -> FocusHandle {
        self.title.read(cx).focus_handle(cx)
    }

    /// Focus the title.
    pub fn focus_title(&self, window: &mut Window, cx: &mut App) {
        let focus = self.title_focus(cx);
        window.focus(&focus, cx);
    }

    fn validate(&mut self, title: &str, description: &str) -> bool {
        self.errors = FieldErrors::default();
        if self.project_id.is_none() {
            self.errors.project = Some("Choose a project for this ticket.".into());
        }
        if title.is_empty() {
            self.errors.title =
                Some("A title is required: a short outcome such as “Retry failed uploads”.".into());
        } else if title.contains('\n') {
            self.errors.title = Some("Keep the title on one line.".into());
        } else if title.chars().count() > BOARD_TITLE_MAX_CHARS {
            self.errors.title = Some(format!(
                "Keep the title under {BOARD_TITLE_MAX_CHARS} characters."
            ));
        }
        if description.len() > BOARD_DESCRIPTION_MAX_BYTES {
            self.errors.description = Some(format!(
                "The description is too long ({} KiB at most).",
                BOARD_DESCRIPTION_MAX_BYTES / 1024
            ));
        }
        !self.errors.any()
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        // One save at a time; a saved form shows its receipt instead.
        if !matches!(self.phase, SavePhase::Editing) {
            return;
        }
        let title = self.title.read(cx).text().trim().to_owned();
        let typed = self.description.read(cx).detached_text(cx);
        if !self.validate(&title, typed.trim_end()) {
            cx.notify();
            return;
        }
        let Some(api) = self.model.read(cx).api(cx) else {
            self.errors.general = Some("Engine not connected. Nothing was saved.".into());
            cx.notify();
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.errors.general = Some("Engine not connected. Nothing was saved.".into());
            cx.notify();
            return;
        };
        // The brief as the composer holds it: text (skills in the agent's own
        // syntax, `@` files as links), `$` selections and staged images.
        let message = self
            .description
            .update(cx, |composer, cx| composer.take_detached(cx));
        let description = message.text.clone();
        // Images upload to the project's host, like a chat message's there.
        let host = {
            let state = self.state.read(cx);
            self.project_id
                .as_ref()
                .and_then(|id| state.space_row(id))
                .map(|space| space.device_id.clone())
                .filter(|device| state.local_device_id.as_deref() != Some(device.as_str()))
        };
        let uploaded = self.uploaded.clone();
        let draft = self
            .settings
            .as_ref()
            .map(|editor| editor.read(cx).draft())
            .unwrap_or_default();
        let request_id = self
            .pending_request
            .get_or_insert_with(|| new_request_id("create"))
            .clone();
        let request = BoardCreateRequest {
            request_id: request_id.clone(),
            project_id: self.project_id.clone(),
            title,
            description,
            implementation: (!draft.implementation.is_inherited()).then_some(draft.implementation),
            review: (draft.review != BoardReviewProfile::default()).then_some(draft.review),
            completion: None,
            labels: Vec::new(),
            priority: None,
            context_refs: Vec::new(),
            attachments: Vec::new(),
            input_items: message.input_items.clone(),
            origin: Some(BoardItemOrigin::Manual),
            placement: BoardPlacement::Top,
        };
        self.phase = SavePhase::Saving;
        cx.notify();
        self.save_task = Some(cx.spawn(async move |this, cx| {
            let mut request = request;
            let mut paths = Vec::new();
            for attachment in &message.attachments {
                if let Some(path) = uploaded.get(&attachment.id) {
                    paths.push(path.clone());
                    continue;
                }
                let upload_id = uuid::Uuid::new_v4().to_string();
                match crate::attachments::upload_attachment(
                    &engine,
                    cx.background_executor(),
                    host.as_deref(),
                    &upload_id,
                    attachment,
                    None,
                )
                .await
                {
                    Ok(path) => {
                        let id = attachment.id.clone();
                        let kept = path.clone();
                        this.update(cx, |form, _| {
                            form.uploaded.insert(id, kept);
                        })
                        .ok();
                        paths.push(path);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "ticket attachment upload failed");
                        let name = attachment.name.clone();
                        this.update(cx, |form, cx| {
                            form.phase = SavePhase::Editing;
                            form.errors.description =
                                Some(format!("{name} couldn't upload; nothing was saved."));
                            form.description.update(cx, |composer, cx| {
                                composer.restore_detached(message.clone(), cx)
                            });
                            cx.notify();
                        })
                        .ok();
                        return;
                    }
                }
            }
            request.attachments = paths;
            let result = match api.create(request).await {
                Err(BoardCallError::Lost) => {
                    this.update(cx, |form, cx| {
                        form.phase = SavePhase::Reconciling;
                        cx.notify();
                    })
                    .ok();
                    match api
                        .receipt(BoardReceiptRequest {
                            request_id: request_id.clone(),
                        })
                        .await
                    {
                        Ok(Some(receipt)) => Ok(receipt),
                        Ok(None) | Err(_) => Err(BoardCallError::Lost),
                    }
                }
                other => other,
            };
            this.update(cx, |form, cx| {
                // Not saved: the brief, its selections and images go back.
                if result.is_err() {
                    form.description.update(cx, |composer, cx| {
                        composer.restore_detached(message.clone(), cx)
                    });
                }
                form.finish_save(result, cx)
            })
            .ok();
        }));
    }

    fn finish_save(
        &mut self,
        result: Result<BoardMutationReceipt, BoardCallError>,
        cx: &mut Context<Self>,
    ) {
        self.save_task = None;
        match result {
            Ok(receipt) => {
                self.pending_request = None;
                self.model
                    .update(cx, |model, cx| model.apply_receipt(&receipt, cx));
                self.phase = SavePhase::Saved(Box::new(receipt));
            }
            Err(BoardCallError::Lost) => {
                // Keep the request id: saving again reconciles, never
                // duplicates.
                self.phase = SavePhase::Editing;
                self.errors.general = Some(
                    "LUNA couldn't confirm the save, and no ticket was found for it. Save again; \
                     it is safe to retry."
                        .into(),
                );
            }
            Err(error) => {
                self.phase = SavePhase::Editing;
                self.pending_request = None;
                self.apply_error(error);
            }
        }
        cx.notify();
    }

    /// Put an engine refusal next to the field it concerns.
    fn apply_error(&mut self, error: BoardCallError) {
        let Some(board) = error.board().cloned() else {
            self.errors.general = Some(format!("Not saved: {}", error.message()));
            return;
        };
        match &board {
            BoardError::InvalidInput { field, reason } => {
                let message = reason.clone();
                let field = field.to_ascii_lowercase();
                if field.contains("title") {
                    self.errors.title = Some(message);
                } else if field.contains("description") {
                    self.errors.description = Some(message);
                } else if field.contains("implementation")
                    || field.contains("review")
                    || field.contains("model")
                    || field.contains("reasoning")
                    || field.contains("permission")
                {
                    self.errors.settings = Some(format!("{field}: {message}"));
                } else if field.contains("project") {
                    self.errors.project = Some(message);
                } else {
                    self.errors.general = Some(format!("{field}: {message}"));
                }
            }
            BoardError::UnavailableModel {
                model, supported, ..
            } => {
                self.errors.settings = Some(if supported.is_empty() {
                    format!("{model} is not available on this host.")
                } else {
                    format!(
                        "{model} is not available on this host. Available: {}.",
                        supported.join(", ")
                    )
                });
            }
            BoardError::UnsupportedEffort {
                model,
                effort,
                supported,
            } => {
                let supported: Vec<&str> = supported
                    .iter()
                    .map(|level| crate::pickers::reasoning_label(*level))
                    .collect();
                self.errors.settings = Some(format!(
                    "{model} does not support {} reasoning. Supported: {}.",
                    crate::pickers::reasoning_label(*effort),
                    if supported.is_empty() {
                        "none".into()
                    } else {
                        supported.join(", ")
                    }
                ));
            }
            BoardError::AmbiguousProject | BoardError::ProjectNotFound { .. } => {
                self.errors.project = Some(board.message());
            }
            _ => self.errors.general = Some(format!("Not saved: {}", board.message())),
        }
    }

    /// Start over for another ticket (same project and settings).
    fn write_another(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.phase = SavePhase::Editing;
        self.errors = FieldErrors::default();
        self.title.update(cx, |input, cx| input.set_text("", cx));
        self.description.update(cx, |composer, cx| {
            composer.set_detached_text(String::new(), cx)
        });
        self.uploaded.clear();
        self.focus_title(window, cx);
        cx.notify();
    }

    fn add_criteria(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.description.read(cx).detached_text(cx);
        if current.contains("## Acceptance criteria") {
            return;
        }
        let text = if current.trim().is_empty() {
            CRITERIA_SECTION.to_owned()
        } else {
            format!("{}\n\n{CRITERIA_SECTION}", current.trim_end())
        };
        self.description
            .update(cx, |composer, cx| composer.set_detached_text(text, cx));
        let focus = gpui::Focusable::focus_handle(self.description.read(cx), cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Generate title is offered once the brief says enough to name.
    fn can_generate_title(&self, cx: &App) -> bool {
        self.title_task.is_none()
            && self.project_id.is_some()
            && self
                .description
                .read(cx)
                .detached_text(cx)
                .trim()
                .chars()
                .count()
                >= TITLE_FROM_BRIEF_CHARS
    }

    /// Draft the title from the brief with the device's title model (the
    /// one that names chats). It replaces the title field; nothing is saved.
    fn generate_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_generate_title(cx) {
            return;
        }
        let (Some(project_id), Some(api)) = (self.project_id.clone(), self.model.read(cx).api(cx))
        else {
            return;
        };
        let description = self.description.read(cx).detached_text(cx);
        // The ticket's agent harness, when no title model is configured.
        let harness = self
            .description
            .read(cx)
            .pickers()
            .read(cx)
            .resolved(cx)
            .harness;
        let request = luna_proto::BoardGenerateTitleRequest {
            project_id,
            description,
            harness,
        };
        self.errors.title = None;
        let focus = self.title_focus(cx);
        window.focus(&focus, cx);
        self.title_task = Some(cx.spawn(async move |this, cx| {
            let result = api.generate_title(request).await;
            this.update(cx, |form, cx| {
                form.title_task = None;
                match result {
                    Ok(generated) => form
                        .title
                        .update(cx, |input, cx| input.set_text(generated.title, cx)),
                    Err(error) => {
                        form.errors.title =
                            Some(format!("Couldn't draft a title: {}", error.message()))
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn field_error(&self, key: &str, error: &Option<String>, theme: &Theme) -> Option<AnyElement> {
        let error = error.as_ref()?;
        self.probe
            .text(format!("manual-error-{key}"), error.clone());
        Some(
            widgets::error_strip(theme, error.clone())
                .mt(ui_rems(6.0))
                .into_any_element(),
        )
    }

    /// A borderless field inside the brief's card: the input paints its own
    /// lines; a press anywhere in the field's box focuses it (bubbling, after
    /// the input's own press handling), and the focusable drawer and Shell
    /// must not take focus back.
    fn card_field(
        &self,
        id: &'static str,
        input: &Entity<ComposerInput>,
        min_height: Option<f32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus = input.read(cx).focus_handle(cx);
        div()
            .id(id)
            .relative()
            .w_full()
            .cursor_text()
            .px(ui_rems(16.0))
            .when_some(min_height, |frame, height| frame.min_h(ui_rems(height)))
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                window.focus(&focus, cx);
                window.prevent_default();
            })
            .child(input.clone())
            .children(self.probe.bounds_el(id))
            .into_any_element()
    }

    fn render_receipt(
        &self,
        receipt: &BoardMutationReceipt,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let options = self
            .settings
            .as_ref()
            .and_then(|editor| editor.read(cx).options().cloned());
        let title = format!("{} saved to Backlog", receipt.key);
        let chat = "No chat was created and nothing started. Move it to Ready (or press Start in \
                    its details) when an agent should work on it."
            .to_owned();
        let implementation = format!(
            "Implementation: {}",
            describe_implementation(&receipt.item.implementation, options.as_ref())
        );
        let review = format!(
            "Review: {}",
            describe_review(&receipt.item.review, options.as_ref())
        );
        self.probe.text("manual-receipt-title", title.clone());
        self.probe.text("manual-receipt-chat", chat.clone());
        self.probe
            .text("manual-receipt-implementation", implementation.clone());
        self.probe.text("manual-receipt-review", review.clone());
        let work_item_id = receipt.work_item_id.clone();
        widgets::section_card(theme)
            .id("manual-receipt")
            .mt_0()
            .child(
                widgets::card_row(theme, true)
                    .child(widgets::row_tile(theme, crate::icons::CHECK))
                    .child(widgets::row_text(theme, title, Some(chat.into()))),
            )
            .child(
                widgets::card_row(theme, false).child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(ui_rems(4.0))
                        .child(widgets::row_description(theme, implementation))
                        .child(widgets::row_description(theme, review)),
                ),
            )
            .child(
                widgets::card_row(theme, false).child(
                    widgets::row_block()
                        .child(
                            button::button(
                                "manual-open-ticket",
                                ButtonKind::Primary,
                                ControlSize::Md,
                                true,
                                theme,
                            )
                            .aria_label("Open ticket")
                            .child("Open ticket")
                            .children(self.probe.bounds_el("manual-open-ticket"))
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.emit(ManualFormEvent::OpenTicket(work_item_id.clone()))
                                },
                            )),
                        )
                        .child(
                            button::button(
                                "manual-write-another",
                                ButtonKind::Secondary,
                                ControlSize::Md,
                                true,
                                theme,
                            )
                            .aria_label("Write another ticket")
                            .child("Write another")
                            .children(self.probe.bounds_el("manual-write-another"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.write_another(window, cx)),
                            ),
                        )
                        .child(
                            button::button(
                                "manual-done",
                                ButtonKind::Ghost,
                                ControlSize::Md,
                                true,
                                theme,
                            )
                            .aria_label("Close")
                            .child("Close")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ManualFormEvent::Close))),
                        ),
                ),
            )
            .into_any_element()
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_state(&self, window: &Window, cx: &App) -> serde_json::Value {
        let focused = if self.title.read(cx).focus_handle(cx).is_focused(window) {
            Some("title")
        } else if gpui::Focusable::focus_handle(self.description.read(cx), cx).is_focused(window) {
            Some("description")
        } else {
            None
        };
        serde_json::json!({
            "projectId": self.project_id,
            "phase": self.phase.key(),
            "title": self.title.read(cx).text(),
            "description": self.description.read(cx).detached_text(cx),
            "descriptionComposer": self.description.read(cx).fixture_detached_readback(window, cx),
            "generateTitle": if self.title_task.is_some() {
                "generating"
            } else if self.can_generate_title(cx) {
                "ready"
            } else {
                "waiting"
            },
            "focused": focused,
            "pendingRequest": self.pending_request,
            "errors": {
                "project": self.errors.project,
                "title": self.errors.title,
                "description": self.errors.description,
                "settings": self.errors.settings,
                "general": self.errors.general,
            },
            "receipt": match &self.phase {
                SavePhase::Saved(receipt) => serde_json::to_value(receipt.as_ref()).unwrap_or_default(),
                _ => serde_json::Value::Null,
            },
            "settings": self.settings.as_ref().map(|editor| editor.read(cx).fixture_state(cx)),
        })
    }
}

impl Render for ManualTicketForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let project_name = self
            .project_id
            .as_ref()
            .and_then(|id| self.state.read(cx).space_row(id))
            .map(|space| space.display_name().to_owned());
        let header = div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(ui_rems(8.0))
            .child(
                button::button(
                    "manual-back",
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    true,
                    &theme,
                )
                .aria_label("Back to New ticket")
                .child(button::glyph(crate::icons::ARROW_LEFT, ControlSize::Sm))
                .child("New ticket")
                .children(self.probe.bounds_el("manual-back"))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ManualFormEvent::Back))),
            )
            .child(
                button::icon_button(
                    "manual-close",
                    crate::icons::CLOSE,
                    ControlSize::Md,
                    true,
                    &theme,
                )
                .aria_label("Close")
                .children(self.probe.bounds_el("manual-close"))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ManualFormEvent::Close))),
            );
        let title = match &project_name {
            Some(name) => format!("New ticket in {name}"),
            None => "New ticket".to_owned(),
        };
        let mut body = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(10.0))
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui_rems(2.0))
                    .child(
                        div()
                            .text_size(ui_rems(17.0))
                            .line_height(ui_rems(22.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(12.0))
                            .text_color(theme.readable_muted(0.75))
                            .child(
                                "Write the brief; saving puts it in Backlog and starts nothing.",
                            ),
                    ),
            );
        if let Some(error) = self.field_error("project", &self.errors.project, &theme) {
            body = body.child(error);
        }
        if let SavePhase::Saved(receipt) = &self.phase {
            let receipt = receipt.as_ref().clone();
            body = body.child(self.render_receipt(&receipt, &theme, cx));
        } else {
            let saving = matches!(self.phase, SavePhase::Saving | SavePhase::Reconciling);
            // The title in a composer-style field; the brief is the chat
            // composer itself (its controls row carries the agent's pickers).
            let gutter = ui_rems(16.0);
            let title_field = self.card_field("manual-title", &self.title.clone(), None, cx);
            let can_generate = self.can_generate_title(cx);
            let generating = self.title_task.is_some();
            self.probe.text(
                "manual-generate-title-state",
                if generating {
                    "generating"
                } else if can_generate {
                    "ready"
                } else {
                    "waiting"
                },
            );
            let generate = button::button(
                "manual-generate-title",
                ButtonKind::Ghost,
                ControlSize::Sm,
                can_generate,
                &theme,
            )
            .aria_label("Generate a title from the description")
            .tooltip(crate::popover::text_tooltip(
                if can_generate || generating {
                    "Draft a title from the description, with the model that names your chats."
                } else {
                    "Write a little more description first (about 100 characters)."
                },
            ))
            .child(button::glyph(crate::icons::MAGIC_WAND_3, ControlSize::Sm))
            .child(if generating {
                "Generating…"
            } else {
                "Generate"
            })
            .children(self.probe.bounds_el("manual-generate-title"))
            .when(can_generate, |button| {
                button.on_click(cx.listener(|this, _, window, cx| this.generate_title(window, cx)))
            });
            let title_card = div().w_full().px(gutter).child(
                crate::composer::pane_composer_surface(&theme)
                    .id("manual-title-card")
                    .w_full()
                    .py(ui_rems(8.0))
                    .pr(ui_rems(8.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(6.0))
                    .child(div().flex_1().min_w_0().py(ui_rems(4.0)).child(title_field))
                    .child(div().flex_none().child(generate)),
            );
            let width = self.composer_width.get();
            if width > 0.0 {
                self.description.update(cx, |composer, cx| {
                    composer.set_available_width(width.min(crate::composer::COMPOSER_MAX_WIDTH), cx)
                });
            }
            let measured = self.composer_width.clone();
            let brief = div()
                .id("manual-description")
                .relative()
                .w_full()
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
                // After the composer focused its input (bubbling), the
                // focusable drawer and Shell must not take focus back.
                .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                    window.prevent_default()
                })
                // Files dropped on the brief are staged in it, like a chat's.
                .on_drop(cx.listener(|form, paths: &gpui::ExternalPaths, _, cx| {
                    let paths = paths.paths().to_vec();
                    form.description
                        .update(cx, |composer, cx| composer.add_paths(paths, cx));
                    cx.stop_propagation();
                }))
                .child(self.description.clone())
                .children(self.probe.bounds_el("manual-description"));
            let title_error = self
                .field_error("title", &self.errors.title, &theme)
                .map(|error| div().px(gutter).child(error));
            let brief_error = self
                .field_error("description", &self.errors.description, &theme)
                .map(|error| div().px(gutter).child(error));
            let card = div()
                .w_full()
                .flex()
                .flex_col()
                .gap(ui_rems(8.0))
                .child(title_card)
                .children(title_error)
                .child(brief)
                .children(brief_error)
                .when_some(
                    self.settings_rows.as_ref().map(|(row, _)| row.clone()),
                    |card, row| card.child(div().px(ui_rems(22.0)).child(row)),
                );
            body = body
                .child(card)
                .when_some(
                    self.settings_rows.as_ref().map(|(_, row)| row.clone()),
                    |body, row| body.child(div().px(ui_rems(22.0)).child(row)),
                )
                .children(self.field_error("settings", &self.errors.settings, &theme))
                .children(self.field_error("general", &self.errors.general, &theme));
            let status = match self.phase {
                SavePhase::Saving => "Saving…",
                SavePhase::Reconciling => "Checking whether the ticket was saved…",
                _ => "Saving creates no chat and starts nothing.",
            };
            self.probe.text("manual-status", status);
            body = body.child(
                div()
                    .w_full()
                    .mt(ui_rems(4.0))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(ui_rems(8.0))
                    .child(
                        button::button(
                            "manual-add-criteria",
                            ButtonKind::Ghost,
                            ControlSize::Sm,
                            true,
                            &theme,
                        )
                        .aria_label("Add an Acceptance criteria section to the description")
                        .child(button::glyph(crate::icons::CHECKLIST, ControlSize::Sm))
                        .child("Acceptance criteria")
                        .children(self.probe.bounds_el("manual-add-criteria"))
                        .on_click(cx.listener(|this, _, window, cx| this.add_criteria(window, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(ui_rems(11.5))
                            .text_color(theme.readable_muted(0.7))
                            .child(status),
                    )
                    .child(
                        button::button(
                            "manual-save",
                            ButtonKind::Primary,
                            ControlSize::Md,
                            !saving,
                            &theme,
                        )
                        .aria_label("Save to Backlog")
                        .child("Save to Backlog")
                        .children(self.probe.bounds_el("manual-save"))
                        .when(!saving, |button| {
                            button.on_click(cx.listener(|this, _, _, cx| this.save(cx)))
                        }),
                    ),
            );
        }
        div()
            .id("manual-form")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .w_full()
                    .px(ui_rems(20.0))
                    .pt(ui_rems(14.0))
                    .pb(ui_rems(40.0))
                    .child(body),
            )
    }
}
