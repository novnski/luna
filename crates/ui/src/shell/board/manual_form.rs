//! New ticket (A9): the chooser between the two distinct creation
//! experiences (Write manually / Discuss with AI, plus From GitHub issue),
//! and the Write manually form: title, description (acceptance criteria are a
//! section of it, not a mandatory field) and the implementation/review
//! settings. Save to Backlog calls `BoardApi::create`: it creates no chat and
//! starts nothing, and its receipt stays visible. Discuss with AI opens the
//! temporary authoring pane (A6), which has no title/description fields.
use gpui::{
    AnyElement, App, Context, Entity, EntityInputHandler as _, EventEmitter, FocusHandle,
    Focusable as _, IntoElement, ParentElement, Render, ScrollHandle, Styled, Subscription, Task,
    Window, div, prelude::*, px,
};
use luna_proto::{
    BOARD_DESCRIPTION_MAX_BYTES, BOARD_TITLE_MAX_CHARS, BoardCreateRequest, BoardError,
    BoardItemOrigin, BoardMutationReceipt, BoardPlacement, BoardReceiptRequest, BoardReviewProfile,
};

use super::api::BoardCallError;
use super::detail::{BoardProbe, new_request_id};
use super::model::BoardModel;
use super::settings_controls::{
    ExecutionSettingsDraft, ExecutionSettingsEditor, SettingsEditorEvent, describe_implementation,
    describe_review,
};
use crate::AccessibleButton as _;
use crate::button::{self, ButtonKind, ControlSize};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::{FillTier, Theme};
use crate::typography::ui_rems;

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
/// form beside a conversation.
pub struct NewTicketChooser {
    state: Entity<AppState>,
    probe: BoardProbe,
    project_id: Option<String>,
    error: Option<String>,
    focus: FocusHandle,
}

impl EventEmitter<ChooserEvent> for NewTicketChooser {}

impl NewTicketChooser {
    pub fn new(
        state: Entity<AppState>,
        project_id: Option<String>,
        probe: BoardProbe,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            state,
            probe,
            project_id,
            error: None,
            focus: cx.focus_handle(),
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Projects this host owns (the Board is owner-local), by name.
    fn projects(&self, cx: &App) -> Vec<(String, String)> {
        let state = self.state.read(cx);
        let local = state.local_device_id.clone();
        state
            .spaces_sorted()
            .into_iter()
            .filter(|space| {
                local
                    .as_deref()
                    .is_none_or(|device| space.device_id == device)
            })
            .map(|space| (space.id.clone(), space.display_name().to_owned()))
            .collect()
    }

    fn choose(&mut self, kind: ChooserKind, cx: &mut Context<Self>) {
        let Some(project_id) = self.project_id.clone() else {
            self.error = Some("Choose a project for this ticket first.".into());
            cx.notify();
            return;
        };
        cx.emit(match kind {
            ChooserKind::Manual => ChooserEvent::WriteManually { project_id },
            ChooserKind::Ai => ChooserEvent::DiscussWithAi { project_id },
            ChooserKind::GitHub => ChooserEvent::FromGitHub { project_id },
        });
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
            .on_click(cx.listener(move |this, _, _, cx| this.choose(kind, cx)))
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
    pub fn fixture_state(&self) -> serde_json::Value {
        serde_json::json!({ "projectId": self.project_id, "error": self.error })
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
        let projects = self.projects(cx);
        let current = self.project_id.clone();
        let named = current
            .as_ref()
            .and_then(|id| projects.iter().find(|(project, _)| project == id))
            .map(|(_, name)| name.clone());
        if current.is_none() || named.is_none() {
            // All projects: the project is chosen explicitly, never guessed.
            let mut chips = Vec::new();
            for (id, name) in projects {
                let selected = current.as_deref() == Some(id.as_str());
                let chip_id = format!("board-chooser-project:{id}");
                self.probe.chip(&chip_id, &name, selected);
                let probe = self.probe.bounds_el(chip_id.clone());
                let mut chip = button::chip(chip_id, selected, ControlSize::Sm, true, &theme)
                    .role(gpui::Role::RadioButton)
                    .aria_label(name.clone());
                if selected {
                    chip = chip.child(button::glyph(crate::icons::CHECK, ControlSize::Sm));
                }
                chips.push(
                    chip.child(name)
                        .children(probe)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.project_id = Some(id.clone());
                            this.error = None;
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            root = root.child(
                widgets::section_card(&theme).mt_0().child(
                    widgets::card_row(&theme, true)
                        .child(widgets::row_text(
                            &theme,
                            "Project",
                            Some("Each ticket belongs to one project on this host.".into()),
                        ))
                        .child(widgets::row_block().children(chips)),
                ),
            );
        } else if let Some(name) = named {
            root = root.child(widgets::row_description(&theme, format!("Project: {name}")));
        }
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
    description: Entity<ComposerInput>,
    settings: Option<Entity<ExecutionSettingsEditor>>,
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
            ComposerInput::new("A clear outcome, e.g. “Retry failed uploads”", cx)
                .with_single_line()
                .with_accessibility_role(gpui::Role::TextInput)
        });
        let description = cx.new(|cx| {
            ComposerInput::new("What should change, and how will we know it works?", cx)
                .with_accessibility_role(gpui::Role::MultilineTextInput)
        });
        let mut subscriptions = vec![
            cx.subscribe_in(&title, window, |this, _, event, window, cx| match event {
                // Enter in the title moves on to the brief.
                ComposerInputEvent::Submitted => {
                    let focus = this.description.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                }
                ComposerInputEvent::Edited if this.errors.title.take().is_some() => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&description, window, |this, input, event, window, cx| {
                match event {
                    // The brief is multi-line: Enter inserts a line break.
                    ComposerInputEvent::Submitted => input.update(cx, |input, cx| {
                        input.replace_text_in_range(None, "\n", window, cx)
                    }),
                    ComposerInputEvent::Edited if this.errors.description.take().is_some() => {
                        cx.notify()
                    }
                    _ => {}
                }
            }),
        ];
        let settings = project_id.clone().map(|project| {
            let editor = cx.new(|cx| {
                ExecutionSettingsEditor::new(
                    model.clone(),
                    state.clone(),
                    project,
                    ExecutionSettingsDraft::default(),
                    "manual",
                    probe.clone(),
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
        Self {
            model,
            project_id,
            state,
            probe,
            title,
            description,
            settings,
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
        let description = self.description.read(cx).text().trim_end().to_owned();
        if !self.validate(&title, &description) {
            cx.notify();
            return;
        }
        let Some(api) = self.model.read(cx).api(cx) else {
            self.errors.general = Some("Engine not connected. Nothing was saved.".into());
            cx.notify();
            return;
        };
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
            origin: Some(BoardItemOrigin::Manual),
            placement: BoardPlacement::Top,
        };
        self.phase = SavePhase::Saving;
        cx.notify();
        self.save_task = Some(cx.spawn(async move |this, cx| {
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
            this.update(cx, |form, cx| form.finish_save(result, cx))
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
        self.description
            .update(cx, |input, cx| input.set_text("", cx));
        self.focus_title(window, cx);
        cx.notify();
    }

    fn add_criteria(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.description.read(cx).text().to_owned();
        if current.contains("## Acceptance criteria") {
            return;
        }
        let text = if current.trim().is_empty() {
            CRITERIA_SECTION.to_owned()
        } else {
            format!("{}\n\n{CRITERIA_SECTION}", current.trim_end())
        };
        self.description
            .update(cx, |input, cx| input.set_text(text, cx));
        let focus = self.description.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
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

    fn input_field(
        &self,
        id: &'static str,
        label: &'static str,
        input: &Entity<ComposerInput>,
        min_height: Option<f32>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus = input.read(cx).focus_handle(cx);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(4.0))
            .child(
                div()
                    .text_size(ui_rems(12.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.readable_muted(0.9))
                    .child(label),
            )
            .child(
                crate::popover::dialog_field(input.clone().into_any_element())
                    .id(id)
                    .relative()
                    .cursor_text()
                    .rounded(px(8.0))
                    .border_color(theme.separator(true))
                    .text_size(ui_rems(13.0))
                    .when_some(min_height, |frame, height| frame.min_h(ui_rems(height)))
                    // A click anywhere in the frame focuses its input.
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |_, _, window, cx| window.focus(&focus, cx)),
                    )
                    .children(self.probe.bounds_el(id)),
            )
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
        } else if self
            .description
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            Some("description")
        } else {
            None
        };
        serde_json::json!({
            "projectId": self.project_id,
            "phase": self.phase.key(),
            "title": self.title.read(cx).text(),
            "description": self.description.read(cx).text(),
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
            "settings": self.settings.as_ref().map(|editor| editor.read(cx).fixture_state()),
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
        let mut body = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(12.0))
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(widgets::page_header(&theme, "Write a ticket", None))
                    .child(widgets::page_subtitle(
                        &theme,
                        "Write the brief and choose how the ticket will run.",
                    )),
            );
        if let Some(name) = project_name {
            body = body.child(widgets::row_description(&theme, format!("Project: {name}")));
        }
        if let Some(error) = self.field_error("project", &self.errors.project, &theme) {
            body = body.child(error);
        }
        if let SavePhase::Saved(receipt) = &self.phase {
            let receipt = receipt.as_ref().clone();
            body = body.child(self.render_receipt(&receipt, &theme, cx));
        } else {
            let saving = matches!(self.phase, SavePhase::Saving | SavePhase::Reconciling);
            body = body
                .child(self.input_field(
                    "manual-title",
                    "Title",
                    &self.title.clone(),
                    None,
                    &theme,
                    cx,
                ))
                .children(self.field_error("title", &self.errors.title, &theme))
                .child(self.input_field(
                    "manual-description",
                    "Description",
                    &self.description.clone(),
                    Some(120.0),
                    &theme,
                    cx,
                ))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_center()
                        .gap(ui_rems(8.0))
                        .child(widgets::row_description(
                            &theme,
                            "Acceptance criteria can be a section of the description.",
                        ))
                        .child(
                            button::button(
                                "manual-add-criteria",
                                ButtonKind::Ghost,
                                ControlSize::Sm,
                                true,
                                &theme,
                            )
                            .aria_label("Add an Acceptance criteria section")
                            .child("Add acceptance criteria")
                            .children(self.probe.bounds_el("manual-add-criteria"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_criteria(window, cx)),
                            ),
                        ),
                )
                .children(self.field_error("description", &self.errors.description, &theme));
            if let Some(editor) = &self.settings {
                body = body.child(editor.clone());
            }
            body = body
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
                    .justify_between()
                    .gap(ui_rems(8.0))
                    .child(widgets::row_description(&theme, status))
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
