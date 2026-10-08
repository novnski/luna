//! The Board page (A8): header and toolbar, Backlog / Ready / Done columns
//! (or the List view, also chosen automatically at narrow widths), drag and
//! keyboard moves, card menus and the empty state. The detail drawer, New
//! ticket chooser and Write manually live in the one overlay host the page
//! mounts, [`BoardSurfaces`] (A9). Discuss with AI is A6's conversation,
//! mounted in place of the board body ([`BoardPage::open_authoring`]), with
//! A6's Board drafts list; From GitHub issue opens A7's picker through the
//! Shell.
//!
//! Every move is one engine operation: a drop or a keyboard move between
//! columns calls `BoardMove` once (no confirmation; the engine resolves
//! settings and reports the resulting phase or an actionable error), a drop
//! within a column calls `BoardRank` only, and dropping on Done calls
//! nothing (Done is engine-owned). What the page shows comes from the
//! engine's read model; the page persists presentation only
//! (`UiSettings.board`).
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable as _, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels, Point,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Task,
    Window, div, prelude::FluentBuilder, px,
};
use luna_proto::{
    BoardAdmissionReason, BoardArchiveRequest, BoardColumn, BoardConvertChatRequest, BoardError,
    BoardMoveEffect, BoardMoveReceipt, BoardMoveRequest, BoardNavigationRequest,
    BoardOutcomesRequest, BoardRankRequest, BoardReceiptRequest, BoardScope, BoardSourceKind,
    HarnessId, OutcomeKind, WorkItem, WorkItemView,
};

use super::api::BoardCallError;
use super::detail::BoardSurfaces;
use super::model::{BoardListFilters, BoardModel, BoardModelEvent, BoardStatus};
use crate::settings::{BoardUiPrefs, BoardViewMode, widgets};
use crate::state::AppState;
use crate::theme::Theme;
use crate::typography::ui_rems;

/// Below this page width (interface points at the default size) the Board
/// shows the List view whatever the saved preference says.
pub(super) const NARROW_WIDTH: f32 = 760.0;
/// Search edits settle this long before the engine is asked again.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(220);
/// Scroll offsets are saved once scrolling pauses.
const SCROLL_PERSIST_DELAY: Duration = Duration::from_millis(450);
/// PR/check evidence is read for at most this many changed cards per pass.
const EVIDENCE_BATCH: usize = 24;
/// Model updates a pending focus request waits for its card (a reload in
/// flight after a scope or filter change).
const FOCUS_PATIENCE: u8 = 3;

/// What the page tells the Shell.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardPageEvent {
    /// Presentation preferences changed; persist them (`UiSettings.board`).
    PrefsChanged(BoardUiPrefs),
    /// Go to chat: A10's one path (`Shell::board_go_to_chat`) resolves the
    /// ticket's canonical chat and reveals it.
    GoToChat(BoardNavigationRequest),
    /// From GitHub issue for one project (A7's picker, hosted by the Shell).
    OpenGitHubPicker { project_id: String },
}

impl EventEmitter<BoardPageEvent> for BoardPage {}

/// Requests any Board surface holding the [`BoardModel`] can make of the
/// page that hosts it (the detail drawer, the New issue form, the GitHub
/// picker, the authoring pane): `model.update(cx, |_, cx|
/// cx.emit(BoardPageRequest::CloseOverlay))`.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardPageRequest {
    OpenDetail {
        work_item_id: String,
    },
    /// The New issue chooser (Write manually / Discuss with AI / From
    /// GitHub issue).
    OpenNewIssue,
    OpenGitHubIssue {
        project_id: Option<String>,
    },
    DiscussWithAi {
        project_id: Option<String>,
        resume_session_id: Option<String>,
    },
    AddExistingChat,
    CloseOverlay,
    /// Select (and focus) a ticket, e.g. one just saved.
    Select {
        work_item_id: String,
    },
    OpenChat {
        work_item_id: String,
    },
}

impl EventEmitter<BoardPageRequest> for BoardModel {}

/// The drag payload of a card.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoardCardDrag {
    pub work_item_id: String,
    pub project_id: String,
    pub from: BoardColumn,
    pub key: String,
    pub title: String,
}

/// Where a dragged card would land: the column and its same-project
/// neighbours (rank is per project and column).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct DropTarget {
    pub column: BoardColumn,
    /// Insertion slot among the column's other visible cards.
    pub index: usize,
    pub after_id: Option<String>,
    pub before_id: Option<String>,
}

/// PR and check evidence for one card, from its current outcomes.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CardEvidence {
    /// The `last_activity_ms` this was read at.
    pub as_of: i64,
    pub pull_request: Option<String>,
    pub checks_total: usize,
    pub checks_passed: usize,
    pub checks_failed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoticeTone {
    Info,
    Warning,
    Error,
}

/// The result of the last action: a receipt summary or an actionable error.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct BoardNotice {
    pub tone: NoticeTone,
    pub text: SharedString,
    pub work_item_id: Option<String>,
}

/// Which project a creation action needs when the Board shows All projects
/// (creation never guesses a project).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectIntent {
    GitHub,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum BoardMenu {
    Project,
    Filters,
    ChooseProject(ProjectIntent),
    AddChat {
        project_id: Option<String>,
    },
    /// A card's actions, at the pointer or (keyboard) under the card.
    Card {
        work_item_id: String,
        at: Option<Point<Pixels>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CardAction {
    Open,
    MoveToReady,
    MoveToBacklog,
    MoveUp,
    MoveDown,
    OpenChat,
    Archive,
    Unarchive,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum MenuAction {
    Scope(BoardScope),
    ToggleAttention,
    Provider(Option<HarnessId>),
    Source(Option<BoardSourceKind>),
    ToggleArchived,
    ClearFilters,
    Project(ProjectIntent, String),
    AddChat(String),
    Card(CardAction, String),
}

/// One row of a Board menu.
#[derive(Debug, Clone)]
pub(super) struct MenuEntry {
    pub key: String,
    pub label: String,
    pub hint: Option<String>,
    pub glyph: Option<&'static str>,
    pub checked: bool,
    pub enabled: bool,
    pub heading: Option<&'static str>,
    pub action: MenuAction,
}

impl MenuEntry {
    pub(super) fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        action: MenuAction,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            hint: None,
            glyph: None,
            checked: false,
            enabled: true,
            heading: None,
            action,
        }
    }
}

/// A surface change asked for where no window is at hand (async results,
/// model requests); applied on the next frame.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum SurfaceRequest {
    Detail(String),
    NewTicket(Option<String>),
    Manual(String),
    Close,
}

/// Painted bounds from the last frame (window coordinates): the drop logic
/// and the keyboard reveal read them; the QA probe reports them.
#[derive(Default)]
pub(super) struct BoardGeometry {
    pub page: Option<Bounds<Pixels>>,
    /// The window width when `page` was measured.
    pub viewport_width: Pixels,
    pub columns: Vec<(BoardColumn, Bounds<Pixels>)>,
    /// Card (Board view) or row (List view) bounds per column, in order.
    pub cards: HashMap<BoardColumn, Vec<(String, Bounds<Pixels>)>>,
    /// Labelled controls (toolbar, empty state) for the QA probe.
    pub controls: HashMap<String, Bounds<Pixels>>,
}

impl BoardGeometry {
    pub(super) fn card_bounds(&self, id: &str) -> Option<Bounds<Pixels>> {
        self.cards
            .values()
            .flatten()
            .find(|(card, _)| card == id)
            .map(|(_, bounds)| *bounds)
    }
}

pub struct BoardPage {
    pub(super) state: Entity<AppState>,
    pub(super) model: Entity<BoardModel>,
    pub(super) prefs: BoardUiPrefs,
    /// The prefs last handed to the Shell.
    persisted: BoardUiPrefs,
    pub(super) scroll: ScrollHandle,
    pub(super) search: Entity<crate::composer::ComposerInput>,
    pub(super) focus: FocusHandle,
    pub(super) menu_focus: FocusHandle,
    pub(super) card_focus: HashMap<String, FocusHandle>,
    pub(super) selected: Option<String>,
    pub(super) menu: crate::popover::Popup<BoardMenu>,
    pub(super) menu_cursor: Option<usize>,
    /// Archived tickets are listed (session only; the default board hides
    /// them).
    pub(super) show_archived: bool,
    pub(super) dragging: Option<String>,
    pub(super) drag_over: Option<DropTarget>,
    pub(super) geometry: Rc<RefCell<BoardGeometry>>,
    /// The overlay host (A9): detail drawer, New ticket chooser, Write
    /// manually, Discuss with AI.
    pub(super) surfaces: Entity<BoardSurfaces>,
    /// What the host showed last (focus returns to the card on close).
    shown_surface: Option<&'static str>,
    pending_surface: Option<SurfaceRequest>,
    pub(super) notice: Option<BoardNotice>,
    /// The latest status announcement (an accessibility status element;
    /// never moves focus).
    pub(super) announcement: SharedString,
    phases: HashMap<String, (BoardColumn, luna_proto::BoardPhase)>,
    /// The first live read seeded `phases` (nothing to announce before it).
    phases_seeded: bool,
    /// Discuss with AI (A6): the open temporary discussion, if any. It
    /// replaces the board body while open.
    pub(super) authoring: Option<super::authoring_pane::AuthoringMount>,
    /// Resumable Board drafts and the Discuss with AI entry (A6).
    pub(super) drafts: Entity<super::authoring_pane::AuthoringDrafts>,
    /// A6's Open ticket: the ticket to show once its conversation closed.
    pub(super) authoring_then_open: Option<String>,
    pub(super) evidence: HashMap<String, CardEvidence>,
    evidence_supported: bool,
    evidence_dirty: bool,
    evidence_task: Option<Task<()>>,
    search_task: Option<Task<()>>,
    scroll_task: Option<Task<()>>,
    /// Mutations in flight (moves, ranks, archives, conversions).
    pub(super) pending: usize,
    /// Move/rank/archive RPCs this page issued, in order (QA readback).
    pub(super) issued: Vec<(String, String)>,
    restore_scroll: Option<f32>,
    /// Focus this card once it renders; the count is how many more model
    /// updates may pass before the request lapses (a reload in flight).
    pending_focus: Option<(String, u8)>,
    pending_menu_focus: bool,
    /// Scroll the focused card into view on the next frame.
    pending_reveal: Option<String>,
    /// The sidebar project filter last applied, so the Board follows a
    /// sidebar change but keeps its own explicit choice otherwise.
    seen_sidebar_filter: Option<String>,
    /// Whether the last frame was narrow enough for the List view.
    pub(super) narrow: bool,
    _subscriptions: Vec<Subscription>,
}

/// A request id the engine accepts (`[A-Za-z0-9_.:-]{1,128}`).
pub(super) fn request_id(operation: &str) -> String {
    format!("ui-{operation}-{}", uuid::Uuid::new_v4().simple())
}

impl BoardPage {
    pub fn new(state: Entity<AppState>, scope: BoardScope, cx: &mut Context<Self>) -> Self {
        let saved = crate::settings::current(cx).board;
        let mut prefs = saved.clone();
        prefs.project_id = scope.project_id().map(str::to_owned);
        let filters = BoardListFilters {
            text: prefs.search.clone(),
            attention_only: prefs.attention_only,
            provider: prefs.provider,
            source: prefs.source,
            archived: luna_proto::BoardArchivedFilter::Exclude,
        };
        let model =
            cx.new(|cx| BoardModel::with_filters(state.clone(), scope.clone(), filters, cx));
        let search = cx.new(|cx| {
            let mut input = crate::composer::ComposerInput::with_context(
                "Search tickets…",
                "PaletteSearch",
                cx,
            )
            .with_text_metrics(12.0, 16.0)
            .with_single_line()
            .with_accessibility_role(gpui::Role::SearchInput);
            if !prefs.search.is_empty() {
                input.set_text(prefs.search.clone(), cx);
            }
            input
        });
        let observer = cx.observe(&model, |_, _, cx| cx.notify());
        let surfaces = cx.new(|cx| BoardSurfaces::new(state.clone(), model.clone(), cx));
        let page_handle = cx.entity().downgrade();
        let drafts = cx
            .new(|cx| super::authoring_pane::AuthoringDrafts::new(model.clone(), page_handle, cx));
        let surfaces_observer = cx.observe(&surfaces, |page, _, cx| page.on_surfaces_changed(cx));
        let model_events =
            cx.subscribe(&model, |page, _, event: &BoardModelEvent, cx| match event {
                BoardModelEvent::Changed => page.on_model_changed(cx),
                BoardModelEvent::Events(_) => page.refresh_drafts(cx),
            });
        let requests = cx.subscribe(&model, |page, _, request: &BoardPageRequest, cx| {
            page.on_request(request.clone(), cx)
        });
        let search_events = cx.subscribe(
            &search,
            |page, input, event: &crate::composer::ComposerInputEvent, cx| {
                if matches!(event, crate::composer::ComposerInputEvent::Edited) {
                    let text = input.read(cx).text().to_owned();
                    page.on_search_edited(text, cx);
                }
            },
        );
        let restore_scroll = (prefs.scroll_y > 0.0).then_some(prefs.scroll_y);
        let selected = prefs.selected_item.clone();
        let page = Self {
            state,
            model,
            persisted: saved,
            prefs,
            scroll: ScrollHandle::new(),
            search,
            focus: cx.focus_handle(),
            menu_focus: cx.focus_handle(),
            card_focus: HashMap::new(),
            pending_focus: selected.clone().map(|id| (id, FOCUS_PATIENCE)),
            selected,
            menu: crate::popover::Popup::default(),
            menu_cursor: None,
            show_archived: false,
            dragging: None,
            drag_over: None,
            geometry: Rc::new(RefCell::new(BoardGeometry::default())),
            surfaces,
            shown_surface: None,
            pending_surface: None,
            notice: None,
            announcement: SharedString::default(),
            phases: HashMap::new(),
            phases_seeded: false,
            authoring: None,
            drafts,
            authoring_then_open: None,
            evidence: HashMap::new(),
            evidence_supported: true,
            evidence_dirty: false,
            evidence_task: None,
            search_task: None,
            scroll_task: None,
            pending: 0,
            issued: Vec::new(),
            restore_scroll,
            pending_menu_focus: false,
            pending_reveal: None,
            seen_sidebar_filter: None,
            narrow: false,
            _subscriptions: vec![
                observer,
                surfaces_observer,
                model_events,
                requests,
                search_events,
            ],
        };
        page
    }

    /// Hand the current preferences to a newly subscribed Shell (the scope
    /// may differ from the saved one when the Board opened from a project).
    pub fn sync_prefs(&mut self, cx: &mut Context<Self>) {
        self.persisted = self.prefs.clone();
        cx.emit(BoardPageEvent::PrefsChanged(self.prefs.clone()));
    }

    /// The data layer shared by every Board surface on this page.
    pub fn model(&self) -> &Entity<BoardModel> {
        &self.model
    }

    // ---- preferences ------------------------------------------------------

    /// Hand changed presentation preferences to the Shell, which saves them.
    fn persist(&mut self, cx: &mut Context<Self>) {
        if self.prefs != self.persisted {
            self.persisted = self.prefs.clone();
            cx.emit(BoardPageEvent::PrefsChanged(self.prefs.clone()));
        }
    }

    pub(super) fn effective_view(&self) -> BoardViewMode {
        if self.narrow {
            BoardViewMode::List
        } else {
            self.prefs.view
        }
    }

    pub(super) fn set_view(&mut self, view: BoardViewMode, cx: &mut Context<Self>) {
        if self.prefs.view != view {
            self.prefs.view = view;
            self.persist(cx);
            cx.notify();
        }
    }

    // ---- scope ------------------------------------------------------------

    pub(super) fn scope(&self, cx: &App) -> BoardScope {
        self.model.read(cx).scope().clone()
    }

    /// Switch project scope (explicit choice): reload, remember it.
    pub fn set_scope(&mut self, scope: BoardScope, cx: &mut Context<Self>) {
        self.prefs.project_id = scope.project_id().map(str::to_owned);
        self.model
            .update(cx, |model, cx| model.set_scope(scope, cx));
        self.drag_over = None;
        self.persist(cx);
        cx.notify();
    }

    /// Follow an explicit change of the sidebar's project filter (opening the
    /// Board from a project). Unchanged, or cleared to All, the Board keeps
    /// its own scope: switching pages never changes the project.
    pub fn follow_sidebar_filter(&mut self, filter: Option<String>, cx: &mut Context<Self>) {
        if self.seen_sidebar_filter == filter {
            return;
        }
        self.seen_sidebar_filter = filter.clone();
        if let Some(project_id) = filter
            && self.scope(cx).project_id() != Some(project_id.as_str())
        {
            self.set_scope(BoardScope::Project { project_id }, cx);
        }
    }

    /// Record the sidebar filter the page was created under.
    pub fn note_sidebar_filter(&mut self, filter: Option<String>) {
        self.seen_sidebar_filter = filter;
    }

    /// Projects this host owns (the only boards it can manage), sorted.
    pub(super) fn local_projects(&self, cx: &App) -> Vec<(String, String)> {
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

    pub(super) fn project_name(&self, project_id: &str, cx: &App) -> String {
        self.state
            .read(cx)
            .space_row(project_id)
            .map(|space| space.display_name().to_owned())
            .unwrap_or_else(|| "Unknown project".into())
    }

    pub(super) fn scope_label(&self, cx: &App) -> String {
        match self.scope(cx) {
            BoardScope::AllProjects => "All projects".into(),
            BoardScope::Project { project_id } => self.project_name(&project_id, cx),
        }
    }

    /// The project a card names: only in All projects, where tickets of
    /// several projects share a column.
    pub(super) fn project_label_for(&self, item: &WorkItem, cx: &App) -> Option<String> {
        matches!(self.scope(cx), BoardScope::AllProjects)
            .then(|| self.project_name(&item.project_id, cx))
    }

    // ---- filters ----------------------------------------------------------

    pub(super) fn filters(&self, cx: &App) -> BoardListFilters {
        self.model.read(cx).filters().clone()
    }

    pub(super) fn update_filters(
        &mut self,
        change: impl FnOnce(&mut BoardListFilters),
        cx: &mut Context<Self>,
    ) {
        let mut filters = self.filters(cx);
        change(&mut filters);
        self.prefs.search = filters.text.clone();
        self.prefs.attention_only = filters.attention_only;
        self.prefs.provider = filters.provider;
        self.prefs.source = filters.source;
        self.model
            .update(cx, |model, cx| model.set_filters(filters, cx));
        self.persist(cx);
        cx.notify();
    }

    pub(super) fn active_filter_count(&self, cx: &App) -> usize {
        let filters = self.filters(cx);
        [
            filters.provider.is_some(),
            filters.source.is_some(),
            filters.archived != luna_proto::BoardArchivedFilter::Exclude,
        ]
        .into_iter()
        .filter(|on| *on)
        .count()
    }

    pub(super) fn any_filter(&self, cx: &App) -> bool {
        let filters = self.filters(cx);
        !filters.text.trim().is_empty()
            || filters.attention_only
            || self.active_filter_count(cx) > 0
    }

    fn on_search_edited(&mut self, text: String, cx: &mut Context<Self>) {
        self.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            this.update(cx, |page, cx| {
                page.search_task = None;
                if page.filters(cx).text != text {
                    page.update_filters(|filters| filters.text = text, cx);
                }
            })
            .ok();
        }));
    }

    pub(super) fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.set_text("", cx));
        self.search_task = None;
        self.show_archived = false;
        self.update_filters(
            |filters| {
                *filters = BoardListFilters::default();
            },
            cx,
        );
    }

    // ---- selection and focus ---------------------------------------------

    pub(super) fn card_focus(&mut self, id: &str, cx: &mut Context<Self>) -> FocusHandle {
        self.card_focus
            .entry(id.to_owned())
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// Select a card (and focus it when `focus`), remembering the choice.
    pub(super) fn select(
        &mut self,
        id: &str,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.as_deref() != Some(id) {
            self.selected = Some(id.to_owned());
            self.prefs.selected_item = Some(id.to_owned());
            self.persist(cx);
        }
        if focus {
            let handle = self.card_focus(id, cx);
            window.focus(&handle, cx);
            self.pending_reveal = Some(id.to_owned());
        }
        cx.notify();
    }

    /// The visible cards in navigation order, grouped by column. List mode
    /// reads it as one sequence.
    pub(super) fn visible_columns(&self, cx: &App) -> Vec<(BoardColumn, Vec<String>)> {
        let model = self.model.read(cx);
        BoardColumn::ALL
            .into_iter()
            .map(|column| {
                (
                    column,
                    model
                        .column(column)
                        .map(|view| view.item.id.clone())
                        .collect(),
                )
            })
            .collect()
    }

    /// Where a card sits: (column index, row index).
    fn locate(columns: &[(BoardColumn, Vec<String>)], id: &str) -> Option<(usize, usize)> {
        columns.iter().enumerate().find_map(|(column, (_, ids))| {
            ids.iter()
                .position(|card| card == id)
                .map(|row| (column, row))
        })
    }

    /// The card a navigation key moves to, or `None` to stay.
    fn neighbour(&self, id: &str, key: &str, cx: &App) -> Option<String> {
        let columns = self.visible_columns(cx);
        if self.effective_view() == BoardViewMode::List {
            let flat: Vec<&String> = columns.iter().flat_map(|(_, ids)| ids).collect();
            let at = flat.iter().position(|card| *card == id)?;
            let next = match key {
                "up" => at.checked_sub(1)?,
                "down" => at + 1,
                "home" => 0,
                "end" => flat.len().checked_sub(1)?,
                _ => return None,
            };
            return flat.get(next).map(|id| (*id).clone());
        }
        let (column, row) = Self::locate(&columns, id)?;
        match key {
            "up" => columns[column].1.get(row.checked_sub(1)?).cloned(),
            "down" => columns[column].1.get(row + 1).cloned(),
            "home" => columns[column].1.first().cloned(),
            "end" => columns[column].1.last().cloned(),
            "left" | "right" => {
                let step: isize = if key == "left" { -1 } else { 1 };
                let mut at = column as isize + step;
                while at >= 0 && (at as usize) < columns.len() {
                    let ids = &columns[at as usize].1;
                    if !ids.is_empty() {
                        return ids.get(row.min(ids.len() - 1)).cloned();
                    }
                    at += step;
                }
                None
            }
            _ => None,
        }
    }

    /// A focused card's keys. Returns whether the key was handled.
    pub(super) fn on_card_key(
        &mut self,
        id: &str,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keystroke = &event.keystroke;
        let mods = keystroke.modifiers;
        let key = keystroke.key.as_str();
        let plain = !mods.alt && !mods.shift && !mods.control && !mods.platform;
        if mods.alt && !mods.shift && !mods.control && !mods.platform {
            let action = match key {
                "right" => Some(CardAction::MoveToReady),
                "left" => Some(CardAction::MoveToBacklog),
                "up" => Some(CardAction::MoveUp),
                "down" => Some(CardAction::MoveDown),
                _ => None,
            };
            if let Some(action) = action {
                self.run_card_action(action, id, window, cx);
                return true;
            }
            return false;
        }
        if mods.secondary() && !mods.alt && !mods.shift {
            match key {
                "enter" => {
                    self.run_card_action(CardAction::OpenChat, id, window, cx);
                    return true;
                }
                "backspace" => {
                    let archived = self
                        .model
                        .read(cx)
                        .item(id)
                        .is_some_and(|view| view.item.archived);
                    let action = if archived {
                        CardAction::Unarchive
                    } else {
                        CardAction::Archive
                    };
                    self.run_card_action(action, id, window, cx);
                    return true;
                }
                _ => return false,
            }
        }
        if (mods.shift && key == "f10") || (plain && key == "menu") {
            self.open_card_menu(id, None, window, cx);
            return true;
        }
        if !plain {
            return false;
        }
        match key {
            "enter" | "space" => {
                self.select(id, true, window, cx);
                self.open_detail(id.to_owned(), window, cx);
                true
            }
            "escape" => {
                if self.surface_open(cx) {
                    self.apply_surface(SurfaceRequest::Close, window, cx);
                    true
                } else {
                    false
                }
            }
            "up" | "down" | "left" | "right" | "home" | "end" => {
                if let Some(next) = self.neighbour(id, key, cx) {
                    self.select(&next, true, window, cx);
                }
                true
            }
            _ => false,
        }
    }

    // ---- model events -----------------------------------------------------

    fn on_model_changed(&mut self, cx: &mut Context<Self>) {
        let (live, views): (bool, Vec<WorkItemView>) = {
            let model = self.model.read(cx);
            (
                matches!(model.status(), BoardStatus::Live),
                model.items().to_vec(),
            )
        };
        // Status changes are announced (never by moving focus). The first
        // load only seeds the record.
        let first = !self.phases_seeded;
        let mut changes = Vec::new();
        let mut next = HashMap::with_capacity(views.len());
        for view in &views {
            let now = (view.item.column, view.phase);
            if !first
                && let Some(before) = self.phases.get(&view.item.id)
                && *before != now
            {
                changes.push(format!(
                    "{}: {} · {}",
                    view.item.key,
                    view.item.column.label(),
                    super::card::phase_badge(view).label
                ));
            }
            next.insert(view.item.id.clone(), now);
        }
        // A focus request whose card is still missing waits a few updates
        // (a reload in flight), then lapses.
        if let Some((id, patience)) = &mut self.pending_focus
            && !views.iter().any(|view| &view.item.id == id)
        {
            if *patience == 0 {
                self.pending_focus = None;
            } else {
                *patience -= 1;
            }
        }
        if live {
            self.phases = next;
            self.phases_seeded = true;
            self.card_focus
                .retain(|id, _| views.iter().any(|view| &view.item.id == id));
        }
        if !changes.is_empty() {
            let shown = changes.len().min(3);
            let mut text = changes[..shown].join("; ");
            if changes.len() > shown {
                text.push_str(&format!("; {} more", changes.len() - shown));
            }
            self.announcement = SharedString::from(text);
        }
        if live {
            self.load_evidence(cx);
        }
        cx.notify();
    }

    fn on_request(&mut self, request: BoardPageRequest, cx: &mut Context<Self>) {
        match request {
            BoardPageRequest::OpenDetail { work_item_id } => {
                self.request_surface(SurfaceRequest::Detail(work_item_id), cx)
            }
            BoardPageRequest::OpenNewIssue => {
                let project_id = self.scope(cx).project_id().map(str::to_owned);
                self.request_surface(SurfaceRequest::NewTicket(project_id), cx)
            }
            BoardPageRequest::OpenGitHubIssue { project_id } => self.open_github(project_id, cx),
            BoardPageRequest::DiscussWithAi {
                project_id,
                resume_session_id,
            } => self.request_authoring(project_id, resume_session_id, cx),
            BoardPageRequest::AddExistingChat => self.open_add_chat(cx),
            BoardPageRequest::CloseOverlay => self.request_surface(SurfaceRequest::Close, cx),
            BoardPageRequest::Select { work_item_id } => {
                self.selected = Some(work_item_id.clone());
                self.prefs.selected_item = Some(work_item_id.clone());
                self.pending_focus = Some((work_item_id, FOCUS_PATIENCE));
                self.persist(cx);
                cx.notify();
            }
            BoardPageRequest::OpenChat { work_item_id } => self.open_chat(&work_item_id, cx),
        }
    }

    // ---- evidence and drafts ---------------------------------------------

    /// Read PR/check outcomes for cards whose activity changed. A host that
    /// does not serve outcomes yet shows none (never a guess).
    fn load_evidence(&mut self, cx: &mut Context<Self>) {
        if !self.evidence_supported {
            return;
        }
        if self.evidence_task.is_some() {
            self.evidence_dirty = true;
            return;
        }
        let wanted: Vec<(String, i64)> = self
            .model
            .read(cx)
            .items()
            .iter()
            .filter(|view| view.working_chat.is_some())
            .filter(|view| {
                self.evidence
                    .get(&view.item.id)
                    .map(|evidence| evidence.as_of)
                    != Some(view.last_activity_ms)
            })
            .take(EVIDENCE_BATCH)
            .map(|view| (view.item.id.clone(), view.last_activity_ms))
            .collect();
        let Some(api) = self.model.read(cx).api(cx) else {
            return;
        };
        if wanted.is_empty() {
            return;
        }
        self.evidence_task = Some(cx.spawn(async move |this, cx| {
            let mut read = Vec::new();
            let mut supported = true;
            for (id, as_of) in wanted {
                let request = BoardOutcomesRequest {
                    work_item_id: id.clone(),
                    limit: Some(20),
                };
                match api.outcomes(request).await {
                    Ok(outcomes) => read.push((id, summarize_evidence(as_of, &outcomes))),
                    Err(BoardCallError::Unsupported)
                    | Err(BoardCallError::Board(BoardError::NotImplemented { .. })) => {
                        supported = false;
                        break;
                    }
                    Err(_) => read.push((
                        id,
                        CardEvidence {
                            as_of,
                            ..CardEvidence::default()
                        },
                    )),
                }
            }
            this.update(cx, |page, cx| {
                page.evidence_task = None;
                page.evidence_supported = supported;
                page.evidence.extend(read);
                if std::mem::take(&mut page.evidence_dirty) {
                    page.load_evidence(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Re-read A6's Board drafts (after a journal event that may have
    /// saved or closed a discussion).
    pub(super) fn refresh_drafts(&mut self, cx: &mut Context<Self>) {
        self.drafts.update(cx, |drafts, cx| drafts.refresh(cx));
    }

    // ---- notices ----------------------------------------------------------

    pub(super) fn set_notice(
        &mut self,
        tone: NoticeTone,
        text: impl Into<SharedString>,
        work_item_id: Option<String>,
    ) {
        let text = text.into();
        self.announcement = text.clone();
        self.notice = Some(BoardNotice {
            tone,
            text,
            work_item_id,
        });
    }

    // ---- overlays (A9's host) -----------------------------------------------

    /// The overlay host (detail drawer, New ticket chooser and forms).
    pub fn surfaces(&self) -> &Entity<BoardSurfaces> {
        &self.surfaces
    }

    /// Open one ticket's detail drawer (card click / Enter on a card).
    pub fn open_detail(
        &mut self,
        work_item_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_surface(SurfaceRequest::Detail(work_item_id), window, cx);
    }

    /// Open the New ticket chooser (`None`, All projects: it asks for the
    /// project; never guessed).
    pub fn open_new_ticket(
        &mut self,
        project_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_surface(SurfaceRequest::NewTicket(project_id), window, cx);
    }

    /// Queue a surface change for the next frame (no window at hand).
    pub(super) fn request_surface(&mut self, request: SurfaceRequest, cx: &mut Context<Self>) {
        self.pending_surface = Some(request);
        cx.notify();
    }

    pub(super) fn apply_surface(
        &mut self,
        request: SurfaceRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_surface = None;
        if let SurfaceRequest::Detail(id) = &request
            && self.selected.as_deref() != Some(id.as_str())
        {
            self.selected = Some(id.clone());
            self.prefs.selected_item = Some(id.clone());
            self.persist(cx);
        }
        self.surfaces.update(cx, |surfaces, cx| match request {
            SurfaceRequest::Detail(id) => surfaces.open_detail(id, window, cx),
            SurfaceRequest::NewTicket(project_id) => {
                surfaces.open_new_ticket(project_id, window, cx)
            }
            SurfaceRequest::Manual(project_id) => surfaces.open_manual_form(project_id, window, cx),
            SurfaceRequest::Close => surfaces.close(window, cx),
        });
        cx.notify();
    }

    pub(super) fn surface_open(&self, cx: &App) -> bool {
        self.surfaces.read(cx).is_open()
    }

    /// The host opened, switched or closed a surface: keep the drawer's
    /// ticket selected and return focus to the card on close.
    fn on_surfaces_changed(&mut self, cx: &mut Context<Self>) {
        let (shown, detail) = {
            let surfaces = self.surfaces.read(cx);
            (
                surfaces.surface().map(|surface| surface.key()),
                surfaces.detail_work_item_id(cx),
            )
        };
        if let Some(id) = detail
            && self.selected.as_deref() != Some(id.as_str())
        {
            self.selected = Some(id.clone());
            self.prefs.selected_item = Some(id);
            self.persist(cx);
        }
        let before = std::mem::replace(&mut self.shown_surface, shown);
        if before.is_some()
            && shown.is_none()
            && let Some(id) = self.selected.clone()
        {
            self.pending_focus = Some((id, FOCUS_PATIENCE));
        }
        cx.notify();
    }

    /// Write manually or Discuss with AI straight away when the Board shows
    /// one project; under All projects the chooser asks for the project.
    pub(super) fn open_create(
        &mut self,
        authoring: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = match self.scope(cx).project_id().map(str::to_owned) {
            Some(project_id) if authoring => {
                self.open_authoring(project_id, None, cx);
                return;
            }
            Some(project_id) => SurfaceRequest::Manual(project_id),
            None => SurfaceRequest::NewTicket(None),
        };
        self.apply_surface(request, window, cx);
    }

    /// From GitHub issue: A7's picker for one project (asked for under All
    /// projects), hosted by the Shell.
    pub(super) fn open_github(&mut self, project_id: Option<String>, cx: &mut Context<Self>) {
        let Some(project_id) =
            project_id.or_else(|| self.scope(cx).project_id().map(str::to_owned))
        else {
            self.open_menu(BoardMenu::ChooseProject(ProjectIntent::GitHub), false, cx);
            return;
        };
        cx.emit(BoardPageEvent::OpenGitHubPicker { project_id });
    }

    /// Discuss with AI (or resume a Board draft) from a model request: A6's
    /// conversation for a known project, else the chooser asks for one.
    fn request_authoring(
        &mut self,
        project_id: Option<String>,
        resume_session_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let draft_project = resume_session_id.as_ref().and_then(|session| {
            self.drafts
                .read(cx)
                .sessions()
                .iter()
                .find(|draft| &draft.id == session)
                .map(|draft| draft.project_id.clone())
        });
        match project_id
            .or(draft_project)
            .or_else(|| self.scope(cx).project_id().map(str::to_owned))
        {
            Some(project_id) => {
                self.open_authoring(project_id, resume_session_id, cx);
            }
            None => self.request_surface(SurfaceRequest::NewTicket(None), cx),
        }
    }

    /// A6's conversation closed (draft kept, discarded or ended): focus
    /// returns to the selected card.
    pub(super) fn authoring_closed(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected.clone() {
            self.pending_focus = Some((id, FOCUS_PATIENCE));
        }
        self.refresh_drafts(cx);
        cx.notify();
    }

    /// Open a ticket in its detail drawer from outside the page (a chat's
    /// ticket badge, A6's receipt): selected, then opened on the next frame.
    pub fn show_ticket(&mut self, work_item_id: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(work_item_id.as_str()) {
            self.selected = Some(work_item_id.clone());
            self.prefs.selected_item = Some(work_item_id.clone());
            self.persist(cx);
        }
        self.request_surface(SurfaceRequest::Detail(work_item_id), cx);
    }

    /// The selected ticket (QA readbacks).
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_selected(&self) -> Option<String> {
        self.selected.clone()
    }

    pub(super) fn open_add_chat(&mut self, cx: &mut Context<Self>) {
        let project_id = self.scope(cx).project_id().map(str::to_owned);
        self.open_menu(BoardMenu::AddChat { project_id }, false, cx);
    }

    // ---- menus --------------------------------------------------------------

    pub(super) fn open_menu(&mut self, menu: BoardMenu, keyboard: bool, cx: &mut Context<Self>) {
        let entries = self.menu_entries(&menu, cx);
        self.menu_cursor = if keyboard {
            entries.iter().position(|entry| entry.enabled)
        } else {
            None
        };
        self.menu.open(menu);
        self.pending_menu_focus = true;
        cx.notify();
    }

    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        // Keyboard focus returns to the card the menu acted on (or the
        // selected card for toolbar menus), never to the window.
        let card = match self.menu.as_open() {
            Some(BoardMenu::Card { work_item_id, .. }) => Some(work_item_id.clone()),
            Some(_) => self.selected.clone(),
            None => None,
        };
        if self.menu.begin_close() {
            crate::popover::reap_popup(cx, |page: &mut Self| &mut page.menu);
        }
        self.menu_cursor = None;
        if let Some(id) = card {
            self.pending_focus = Some((id, FOCUS_PATIENCE));
        }
        cx.notify();
    }

    pub(super) fn open_card_menu(
        &mut self,
        id: &str,
        at: Option<Point<Pixels>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_menu(
            BoardMenu::Card {
                work_item_id: id.to_owned(),
                at,
            },
            at.is_none(),
            cx,
        );
    }

    /// The rows of a menu (shared by rendering and keyboard activation).
    pub(super) fn menu_entries(&self, menu: &BoardMenu, cx: &App) -> Vec<MenuEntry> {
        match menu {
            BoardMenu::Project => self.project_menu_entries(cx),
            BoardMenu::Filters => self.filter_menu_entries(cx),
            BoardMenu::ChooseProject(intent) => self
                .local_projects(cx)
                .into_iter()
                .enumerate()
                .map(|(ix, (id, name))| {
                    let mut entry = MenuEntry::new(
                        format!("choose-{id}"),
                        name,
                        MenuAction::Project(*intent, id),
                    );
                    entry.glyph = Some(crate::icons::FOLDER);
                    if ix == 0 {
                        entry.heading = Some("Choose a project");
                    }
                    entry
                })
                .collect(),
            BoardMenu::AddChat { project_id } => self.add_chat_entries(project_id.as_deref(), cx),
            BoardMenu::Card { work_item_id, .. } => self.card_menu_entries(work_item_id, cx),
        }
    }

    fn add_chat_entries(&self, project_id: Option<&str>, cx: &App) -> Vec<MenuEntry> {
        let state = self.state.read(cx);
        let projects: Vec<String> = match project_id {
            Some(project) => vec![project.to_owned()],
            None => self
                .local_projects(cx)
                .into_iter()
                .map(|(id, _)| id)
                .collect(),
        };
        let mut chats: Vec<&luna_proto::Chat> = state
            .chats
            .iter()
            .filter(|chat| !chat.archived)
            .filter(|chat| {
                chat.space_id
                    .as_deref()
                    .is_some_and(|space| projects.iter().any(|project| project == space))
            })
            .collect();
        chats.sort_by(|left, right| {
            right
                .last_message_at
                .unwrap_or(right.created_at)
                .cmp(&left.last_message_at.unwrap_or(left.created_at))
        });
        let mut entries: Vec<MenuEntry> = chats
            .into_iter()
            .take(12)
            .enumerate()
            .map(|(ix, chat)| {
                let mut entry = MenuEntry::new(
                    format!("add-chat-{}", chat.id),
                    chat.title
                        .clone()
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or_else(|| "Untitled chat".into()),
                    MenuAction::AddChat(chat.id.clone()),
                );
                entry.glyph = Some(crate::icons::CHAT_LINE);
                if ix == 0 {
                    entry.heading = Some("Add a chat as a ticket");
                }
                entry
            })
            .collect();
        if entries.is_empty() {
            let mut entry = MenuEntry::new(
                "add-chat-none",
                "No chats in this project",
                MenuAction::ClearFilters,
            );
            entry.enabled = false;
            entry.heading = Some("Add a chat as a ticket");
            entries.push(entry);
        }
        entries
    }

    fn card_menu_entries(&self, id: &str, cx: &App) -> Vec<MenuEntry> {
        let Some(view) = self.model.read(cx).item(id) else {
            return Vec::new();
        };
        let allowed = |action: luna_proto::BoardAction| view.actions.contains(&action);
        let column = view.item.column;
        let archived = view.item.archived;
        let mut entries = Vec::new();
        let mut open = MenuEntry::new(
            "card-open",
            "Open ticket",
            MenuAction::Card(CardAction::Open, id.to_owned()),
        );
        open.hint = Some("↩".into());
        open.glyph = Some(crate::icons::TICKET);
        entries.push(open);
        let mut ready = MenuEntry::new(
            "card-ready",
            if view.working_chat.is_some() {
                "Move to Ready (continue)"
            } else {
                "Move to Ready (start)"
            },
            MenuAction::Card(CardAction::MoveToReady, id.to_owned()),
        );
        ready.hint = Some("⌥→".into());
        ready.glyph = Some(crate::icons::PLAY);
        ready.enabled = !archived
            && column != BoardColumn::Ready
            && allowed(luna_proto::BoardAction::MoveToReady);
        ready.heading = Some("Move");
        entries.push(ready);
        let mut backlog = MenuEntry::new(
            "card-backlog",
            match column {
                BoardColumn::Done => "Move to Backlog (reopen)",
                _ if view.working_chat.is_some() => "Move to Backlog (stop)",
                _ => "Move to Backlog",
            },
            MenuAction::Card(CardAction::MoveToBacklog, id.to_owned()),
        );
        backlog.hint = Some("⌥←".into());
        backlog.glyph = Some(crate::icons::STOP_CIRCLE);
        backlog.enabled = !archived
            && column != BoardColumn::Backlog
            && (allowed(luna_proto::BoardAction::MoveToBacklog)
                || allowed(luna_proto::BoardAction::Reopen));
        entries.push(backlog);
        let mut up = MenuEntry::new(
            "card-up",
            "Move up",
            MenuAction::Card(CardAction::MoveUp, id.to_owned()),
        );
        up.hint = Some("⌥↑".into());
        up.glyph = Some(crate::icons::ARROW_UP);
        up.enabled = !archived && self.rank_neighbours(id, -1, cx).is_some();
        entries.push(up);
        let mut down = MenuEntry::new(
            "card-down",
            "Move down",
            MenuAction::Card(CardAction::MoveDown, id.to_owned()),
        );
        down.hint = Some("⌥↓".into());
        down.glyph = Some(crate::icons::ARROW_DOWN);
        down.enabled = !archived && self.rank_neighbours(id, 1, cx).is_some();
        entries.push(down);
        let mut chat = MenuEntry::new(
            "card-chat",
            "Open chat",
            MenuAction::Card(CardAction::OpenChat, id.to_owned()),
        );
        chat.hint = Some("⌘↩".into());
        chat.glyph = Some(crate::icons::CHAT_LINE);
        chat.enabled = view.working_chat.is_some() || allowed(luna_proto::BoardAction::GoToChat);
        chat.heading = Some("Ticket");
        entries.push(chat);
        let mut archive = if archived {
            MenuEntry::new(
                "card-unarchive",
                "Unarchive",
                MenuAction::Card(CardAction::Unarchive, id.to_owned()),
            )
        } else {
            MenuEntry::new(
                "card-archive",
                "Archive",
                MenuAction::Card(CardAction::Archive, id.to_owned()),
            )
        };
        archive.hint = Some("⌘⌫".into());
        archive.glyph = Some(crate::icons::ARCHIVE_MINIMALISTIC);
        entries.push(archive);
        entries
    }

    pub(super) fn run_menu_action(
        &mut self,
        action: MenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_menu(cx);
        match action {
            MenuAction::Scope(scope) => self.set_scope(scope, cx),
            MenuAction::ToggleAttention => self.update_filters(
                |filters| filters.attention_only = !filters.attention_only,
                cx,
            ),
            MenuAction::Provider(provider) => {
                self.update_filters(|filters| filters.provider = provider, cx)
            }
            MenuAction::Source(source) => {
                self.update_filters(|filters| filters.source = source, cx)
            }
            MenuAction::ToggleArchived => {
                self.show_archived = !self.show_archived;
                let archived = if self.show_archived {
                    luna_proto::BoardArchivedFilter::Include
                } else {
                    luna_proto::BoardArchivedFilter::Exclude
                };
                self.update_filters(|filters| filters.archived = archived, cx)
            }
            MenuAction::ClearFilters => self.clear_filters(cx),
            MenuAction::Project(intent, project_id) => match intent {
                ProjectIntent::GitHub => self.open_github(Some(project_id), cx),
            },
            MenuAction::AddChat(task_id) => self.convert_chat(task_id, cx),
            MenuAction::Card(action, id) => self.run_card_action(action, &id, window, cx),
        }
    }

    /// Up/Down/Enter/Escape inside an open menu.
    pub(super) fn on_menu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(menu) = self.menu.as_open().cloned() else {
            return false;
        };
        let entries = self.menu_entries(&menu, cx);
        match event.keystroke.key.as_str() {
            "up" | "down" => {
                let delta = if event.keystroke.key == "up" { -1 } else { 1 };
                self.menu_cursor =
                    crate::popover::menu_step_where(self.menu_cursor, entries.len(), delta, |ix| {
                        entries[ix].enabled
                    });
                cx.notify();
                true
            }
            "enter" | "space" => {
                if let Some(entry) = self.menu_cursor.and_then(|ix| entries.get(ix))
                    && entry.enabled
                {
                    let action = entry.action.clone();
                    self.run_menu_action(action, window, cx);
                }
                true
            }
            "escape" => {
                self.close_menu(cx);
                true
            }
            "tab" => {
                self.close_menu(cx);
                false
            }
            _ => false,
        }
    }

    // ---- card actions -------------------------------------------------------

    pub(super) fn run_card_action(
        &mut self,
        action: CardAction,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.model.read(cx).item(id).cloned() else {
            return;
        };
        let key = view.item.key.clone();
        match action {
            CardAction::Open => {
                self.select(id, true, window, cx);
                self.open_detail(id.to_owned(), window, cx);
            }
            CardAction::MoveToReady
                if !view.actions.is_empty()
                    && view.item.column != BoardColumn::Ready
                    && !view.actions.contains(&luna_proto::BoardAction::MoveToReady) =>
            {
                self.set_notice(
                    NoticeTone::Info,
                    format!(
                        "{key} can't move to Ready from {}.",
                        view.item.column.label()
                    ),
                    Some(id.to_owned()),
                );
            }
            CardAction::MoveToBacklog
                if !view.actions.is_empty()
                    && view.item.column != BoardColumn::Backlog
                    && !view.actions.iter().any(|action| {
                        matches!(
                            action,
                            luna_proto::BoardAction::MoveToBacklog
                                | luna_proto::BoardAction::Reopen
                        )
                    }) =>
            {
                self.set_notice(
                    NoticeTone::Info,
                    format!("{key} can't move to Backlog right now."),
                    Some(id.to_owned()),
                );
            }
            CardAction::MoveToReady => {
                if view.item.column == BoardColumn::Ready {
                    self.set_notice(
                        NoticeTone::Info,
                        format!("{key} is already in Ready."),
                        None,
                    );
                } else {
                    self.move_item(id, BoardColumn::Ready, None, None, cx);
                }
            }
            CardAction::MoveToBacklog => {
                if view.item.column == BoardColumn::Backlog {
                    self.set_notice(
                        NoticeTone::Info,
                        format!("{key} is already in Backlog."),
                        None,
                    );
                } else {
                    self.move_item(id, BoardColumn::Backlog, None, None, cx);
                }
            }
            CardAction::MoveUp | CardAction::MoveDown => {
                let delta = if action == CardAction::MoveUp { -1 } else { 1 };
                if let Some((after, before)) = self.rank_neighbours(id, delta, cx) {
                    self.rank_item(id, after, before, cx);
                }
            }
            CardAction::OpenChat => self.open_chat(id, cx),
            CardAction::Archive => self.archive_item(id, true, cx),
            CardAction::Unarchive => self.archive_item(id, false, cx),
        }
        cx.notify();
    }

    /// The (after, before) neighbours that move a card one place up
    /// (`delta < 0`) or down among its project's cards in its column.
    pub(super) fn rank_neighbours(
        &self,
        id: &str,
        delta: isize,
        cx: &App,
    ) -> Option<(Option<String>, Option<String>)> {
        let model = self.model.read(cx);
        let view = model.item(id)?;
        let peers: Vec<&str> = model
            .column(view.item.column)
            .filter(|peer| peer.item.project_id == view.item.project_id)
            .map(|peer| peer.item.id.as_str())
            .collect();
        let at = peers.iter().position(|peer| *peer == id)?;
        if delta < 0 {
            let above = at.checked_sub(1)?;
            Some((
                above.checked_sub(1).map(|ix| peers[ix].to_owned()),
                Some(peers[above].to_owned()),
            ))
        } else {
            let below = peers.get(at + 1)?;
            Some((
                Some((*below).to_owned()),
                peers.get(at + 2).map(|peer| (*peer).to_owned()),
            ))
        }
    }

    /// The card's current same-project neighbours (above, below).
    fn current_neighbours(&self, id: &str, cx: &App) -> (Option<String>, Option<String>) {
        let model = self.model.read(cx);
        let Some(view) = model.item(id) else {
            return (None, None);
        };
        let peers: Vec<&str> = model
            .column(view.item.column)
            .filter(|peer| peer.item.project_id == view.item.project_id)
            .map(|peer| peer.item.id.as_str())
            .collect();
        let at = peers.iter().position(|peer| *peer == id).unwrap_or(0);
        (
            at.checked_sub(1).map(|ix| peers[ix].to_owned()),
            peers.get(at + 1).map(|peer| (*peer).to_owned()),
        )
    }

    /// The one engine transition for drag, keyboard and menu moves. No
    /// confirmation: the engine resolves settings and answers with the
    /// resulting phase, or an error the notice routes to the ticket.
    pub(super) fn move_item(
        &mut self,
        id: &str,
        to: BoardColumn,
        after_id: Option<String>,
        before_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(api) = self.model.read(cx).api(cx) else {
            self.set_notice(NoticeTone::Error, "Engine not connected.", None);
            return;
        };
        let Some(key) = self
            .model
            .read(cx)
            .item(id)
            .map(|view| view.item.key.clone())
        else {
            return;
        };
        let request = BoardMoveRequest {
            request_id: request_id("move"),
            work_item_id: id.to_owned(),
            to,
            expected_revision: None,
            after_id,
            before_id,
            destination: Default::default(),
            context: Default::default(),
            execution: None,
            follow_up: Default::default(),
        };
        self.issued.push(("move".into(), id.to_owned()));
        self.pending += 1;
        let id = id.to_owned();
        cx.spawn(async move |this, cx| {
            let request_id = request.request_id.clone();
            let result = api.move_item(request).await;
            // A lost answer is reconciled by its request id, never resent.
            let reconciled = match &result {
                Err(BoardCallError::Lost) => {
                    Some(api.receipt(BoardReceiptRequest { request_id }).await)
                }
                _ => None,
            };
            this.update(cx, |page, cx| {
                page.pending -= 1;
                page.finish_move(&id, &key, to, result, reconciled, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_move(
        &mut self,
        id: &str,
        key: &str,
        to: BoardColumn,
        result: Result<BoardMoveReceipt, BoardCallError>,
        reconciled: Option<Result<Option<luna_proto::BoardMutationReceipt>, BoardCallError>>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(receipt) => {
                self.model
                    .update(cx, |model, cx| model.apply_move(&receipt, cx));
                let phase = self
                    .model
                    .read(cx)
                    .item(id)
                    .map(|view| super::card::phase_badge(view).label)
                    .unwrap_or_else(|| receipt.phase.label().to_owned());
                let column = receipt.mutation.column.label();
                let (tone, text) = match &receipt.effect {
                    BoardMoveEffect::Queued {
                        reason: BoardAdmissionReason::SettingsUnresolved { fields },
                    } => (
                        NoticeTone::Warning,
                        format!(
                            "{key} is in {column} but waits for settings: {}. Choose them in the ticket.",
                            fields.join(", ")
                        ),
                    ),
                    BoardMoveEffect::Queued { reason } => (
                        NoticeTone::Info,
                        format!("{key} → {column} · {phase} ({})", queue_text(reason)),
                    ),
                    BoardMoveEffect::FirstStart { .. } => (
                        NoticeTone::Info,
                        format!("{key} → {column} · started a new chat · {phase}"),
                    ),
                    BoardMoveEffect::Continue { .. } => (
                        NoticeTone::Info,
                        format!("{key} → {column} · continuing its chat · {phase}"),
                    ),
                    BoardMoveEffect::StopRequested { .. } => (
                        NoticeTone::Info,
                        format!("{key} → {column} · stop requested · {phase}"),
                    ),
                    BoardMoveEffect::CancelledUnsent => (
                        NoticeTone::Info,
                        format!("{key} → {column} · the unsent start was cancelled · {phase}"),
                    ),
                    BoardMoveEffect::Coalesced => (
                        NoticeTone::Info,
                        format!(
                            "{key} → {column} · joined the change already in progress · {phase}"
                        ),
                    ),
                    BoardMoveEffect::Reconciling => (
                        NoticeTone::Warning,
                        format!("{key} → {column} · confirming an earlier attempt first · {phase}"),
                    ),
                    BoardMoveEffect::None => {
                        (NoticeTone::Info, format!("{key} → {column} · {phase}"))
                    }
                };
                let unresolved = matches!(
                    receipt.effect,
                    BoardMoveEffect::Queued {
                        reason: BoardAdmissionReason::SettingsUnresolved { .. }
                    }
                );
                self.set_notice(tone, text, Some(id.to_owned()));
                if unresolved {
                    self.request_surface(SurfaceRequest::Detail(id.to_owned()), cx);
                }
            }
            Err(BoardCallError::Lost) => match reconciled {
                Some(Ok(Some(mutation))) => {
                    self.model.update(cx, |model, cx| {
                        model.apply_committed(&mutation.item, None, cx)
                    });
                    self.set_notice(
                        NoticeTone::Info,
                        format!("{key} → {}. Confirming its state…", mutation.column.label()),
                        Some(id.to_owned()),
                    );
                }
                _ => {
                    self.set_notice(
                        NoticeTone::Warning,
                        format!(
                            "Couldn't confirm moving {key} to {}. The board shows the engine's current state.",
                            to.label()
                        ),
                        Some(id.to_owned()),
                    );
                    self.model.update(cx, |model, cx| model.refresh(cx));
                }
            },
            Err(error) => {
                let open_ticket = matches!(
                    error.board(),
                    Some(
                        BoardError::UnavailableModel { .. }
                            | BoardError::UnsupportedEffort { .. }
                            | BoardError::UnavailableProvider { .. }
                            | BoardError::InvalidInput { .. }
                            | BoardError::InvalidDestination { .. }
                            | BoardError::AmbiguousProject
                    )
                );
                let suffix = if open_ticket {
                    " Opened the ticket to fix its settings."
                } else {
                    ""
                };
                self.set_notice(
                    NoticeTone::Error,
                    format!("{key} was not moved: {}{suffix}", error.message()),
                    Some(id.to_owned()),
                );
                if open_ticket {
                    self.request_surface(SurfaceRequest::Detail(id.to_owned()), cx);
                }
                self.model.update(cx, |model, cx| model.refresh(cx));
            }
        }
        cx.notify();
    }

    /// Reorder only: one `BoardRank`, never a move, start or stop.
    pub(super) fn rank_item(
        &mut self,
        id: &str,
        after_id: Option<String>,
        before_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(api) = self.model.read(cx).api(cx) else {
            self.set_notice(NoticeTone::Error, "Engine not connected.", None);
            return;
        };
        let request = BoardRankRequest {
            request_id: request_id("rank"),
            work_item_id: id.to_owned(),
            after_id,
            before_id,
        };
        self.issued.push(("rank".into(), id.to_owned()));
        self.pending += 1;
        cx.spawn(async move |this, cx| {
            let result = api.rank(request).await;
            this.update(cx, |page, cx| {
                page.pending -= 1;
                match result {
                    Ok(receipt) => {
                        page.model.update(cx, |model, cx| {
                            model.apply_committed(&receipt.item, None, cx)
                        });
                        page.announcement =
                            SharedString::from(format!("{} reordered", receipt.key));
                    }
                    Err(error) => {
                        page.set_notice(NoticeTone::Error, error.message(), None);
                        page.model.update(cx, |model, cx| model.refresh(cx));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Archive hides the card (history and links kept; work is not stopped).
    pub(super) fn archive_item(&mut self, id: &str, archived: bool, cx: &mut Context<Self>) {
        let Some(api) = self.model.read(cx).api(cx) else {
            self.set_notice(NoticeTone::Error, "Engine not connected.", None);
            return;
        };
        let request = BoardArchiveRequest {
            request_id: request_id("archive"),
            work_item_id: id.to_owned(),
            archived,
            expected_revision: None,
        };
        self.issued.push(("archive".into(), id.to_owned()));
        self.pending += 1;
        let id = id.to_owned();
        cx.spawn(async move |this, cx| {
            let result = api.archive(request).await;
            this.update(cx, |page, cx| {
                page.pending -= 1;
                match result {
                    Ok(receipt) => {
                        page.model
                            .update(cx, |model, cx| model.apply_receipt(&receipt, cx));
                        let text = if archived {
                            format!(
                                "{} archived. Its history is kept and running work was not stopped.",
                                receipt.key
                            )
                        } else {
                            format!("{} restored to {}.", receipt.key, receipt.column.label())
                        };
                        page.set_notice(NoticeTone::Info, text, Some(id));
                    }
                    Err(error) => page.set_notice(NoticeTone::Error, error.message(), Some(id)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Go to chat through the Shell's one path (A10), which resolves the
    /// canonical location (main chat, or the parent workbench with the exact
    /// side chat) and handles an unavailable parent.
    pub(super) fn open_chat(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(view) = self.model.read(cx).item(id).cloned() else {
            return;
        };
        if view.working_chat.is_none() && !view.actions.contains(&luna_proto::BoardAction::GoToChat)
        {
            self.set_notice(
                NoticeTone::Info,
                format!(
                    "{} has no chat yet. Move it to Ready to start one.",
                    view.item.key
                ),
                Some(id.to_owned()),
            );
            return;
        }
        cx.emit(BoardPageEvent::GoToChat(BoardNavigationRequest {
            work_item_id: Some(id.to_owned()),
            task_id: None,
        }));
    }

    /// Add existing chat: convert it in place (same chat, nothing resent).
    fn convert_chat(&mut self, task_id: String, cx: &mut Context<Self>) {
        let Some(api) = self.model.read(cx).api(cx) else {
            return;
        };
        let request = BoardConvertChatRequest {
            request_id: request_id("convert"),
            task_id,
            title: None,
            description: String::new(),
            implementation: None,
            review: None,
            completion: None,
        };
        self.pending += 1;
        cx.spawn(async move |this, cx| {
            let result = api.convert_chat(request).await;
            this.update(cx, |page, cx| {
                page.pending -= 1;
                match result {
                    Ok(receipt) => {
                        let id = receipt.mutation.work_item_id.clone();
                        page.set_notice(
                            NoticeTone::Info,
                            format!(
                                "Added the chat as {}. It keeps its place and history; nothing was resent.",
                                receipt.mutation.key
                            ),
                            Some(id.clone()),
                        );
                        page.selected = Some(id.clone());
                        page.prefs.selected_item = Some(id.clone());
                        page.persist(cx);
                        page.model.update(cx, |model, cx| model.refresh(cx));
                        page.request_surface(SurfaceRequest::Detail(id), cx);
                    }
                    Err(error) => page.set_notice(NoticeTone::Error, error.message(), None),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- drag and drop ------------------------------------------------------

    /// Where a card dragged to `position` would land.
    pub(super) fn drop_target_at(
        &self,
        position: Point<Pixels>,
        drag: &BoardCardDrag,
        cx: &App,
    ) -> Option<DropTarget> {
        let geometry = self.geometry.borrow();
        let column = geometry
            .columns
            .iter()
            .find(|(_, bounds)| position.x >= bounds.left() && position.x <= bounds.right())
            .map(|(column, _)| *column)?;
        let model = self.model.read(cx);
        let same_project = |id: &str| {
            model
                .item(id)
                .is_some_and(|view| view.item.project_id == drag.project_id)
        };
        let cards: Vec<&(String, Bounds<Pixels>)> = geometry
            .cards
            .get(&column)
            .into_iter()
            .flatten()
            .filter(|(id, _)| *id != drag.work_item_id)
            .collect();
        let index = cards
            .iter()
            .filter(|(_, bounds)| bounds.center().y < position.y)
            .count();
        let after_id = cards[..index]
            .iter()
            .rev()
            .find(|(id, _)| same_project(id))
            .map(|(id, _)| id.clone());
        let before_id = cards[index..]
            .iter()
            .find(|(id, _)| same_project(id))
            .map(|(id, _)| id.clone());
        Some(DropTarget {
            column,
            index,
            after_id,
            before_id,
        })
    }

    pub(super) fn on_drag_over(
        &mut self,
        position: Point<Pixels>,
        drag: &BoardCardDrag,
        cx: &mut Context<Self>,
    ) {
        if self.dragging.as_deref() != Some(drag.work_item_id.as_str()) {
            self.dragging = Some(drag.work_item_id.clone());
            cx.notify();
        }
        let target = self.drop_target_at(position, drag, cx);
        if target != self.drag_over {
            self.drag_over = target;
            cx.notify();
        }
    }

    /// A card was dropped on `column`.
    pub(super) fn drop_card(
        &mut self,
        drag: &BoardCardDrag,
        column: BoardColumn,
        cx: &mut Context<Self>,
    ) {
        let target = self
            .drag_over
            .take()
            .filter(|target| target.column == column);
        self.dragging = None;
        if column == BoardColumn::Done && drag.from != BoardColumn::Done {
            // Done is engine-owned: a drop never manufactures a review.
            self.set_notice(
                NoticeTone::Warning,
                format!(
                    "{} was not moved. Done is set by LUNA when checks and the review pass; \
                     dropping here does not start a review.",
                    drag.key
                ),
                Some(drag.work_item_id.clone()),
            );
            cx.notify();
            return;
        }
        let (after_id, before_id) = match &target {
            Some(target) => (target.after_id.clone(), target.before_id.clone()),
            None => (None, None),
        };
        if column == drag.from {
            if target.is_none()
                || (after_id.clone(), before_id.clone())
                    == self.current_neighbours(&drag.work_item_id, cx)
            {
                cx.notify();
                return;
            }
            self.rank_item(&drag.work_item_id, after_id, before_id, cx);
        } else {
            self.move_item(&drag.work_item_id, column, after_id, before_id, cx);
        }
        self.selected = Some(drag.work_item_id.clone());
        self.prefs.selected_item = Some(drag.work_item_id.clone());
        self.persist(cx);
        cx.notify();
    }

    // ---- frame bookkeeping ----------------------------------------------------

    /// Restore scroll, focus and reveal requests once the content exists,
    /// and save the scroll offset after scrolling pauses.
    fn sync_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.has_active_drag() && (self.dragging.is_some() || self.drag_over.is_some()) {
            self.dragging = None;
            self.drag_over = None;
        }
        let live = matches!(self.model.read(cx).status(), BoardStatus::Live);
        if live && let Some(offset) = self.restore_scroll.take() {
            self.scroll.set_offset(gpui::point(px(0.0), px(-offset)));
        }
        if live
            && !self.menu.is_open()
            && let Some((id, _)) = &self.pending_focus
            && self.model.read(cx).item(id).is_some()
        {
            let id = id.clone();
            self.pending_focus = None;
            let handle = self.card_focus(&id, cx);
            window.focus(&handle, cx);
            self.pending_reveal = Some(id);
        }
        // Surface changes queued without a window (the host focuses the
        // drawer; focus returns to the card when it closes).
        if let Some(request) = self.pending_surface.take() {
            self.apply_surface(request, window, cx);
        }
        if self.pending_menu_focus && self.menu.is_open() {
            self.pending_menu_focus = false;
            window.focus(&self.menu_focus, cx);
        }
        if let Some(id) = self.pending_reveal.take() {
            let geometry = self.geometry.borrow();
            let viewport = self.scroll.bounds();
            if let Some(card) = geometry.card_bounds(&id)
                && viewport.size.height > px(0.0)
            {
                let offset = self.scroll.offset();
                let top_gap = card.top() - viewport.top();
                let bottom_gap = card.bottom() - viewport.bottom();
                let shift = if top_gap < px(0.0) {
                    top_gap - px(12.0)
                } else if bottom_gap > px(0.0) {
                    bottom_gap + px(12.0)
                } else {
                    px(0.0)
                };
                if shift != px(0.0) {
                    let max = self.scroll.max_offset().y;
                    let y = (-offset.y + shift).clamp(px(0.0), max.max(px(0.0)));
                    self.scroll.set_offset(gpui::point(offset.x, -y));
                    cx.notify();
                }
            }
        }
        let offset = (-f32::from(self.scroll.offset().y)).max(0.0);
        if live && (offset - self.prefs.scroll_y).abs() > 1.0 && self.scroll_task.is_none() {
            self.scroll_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SCROLL_PERSIST_DELAY).await;
                this.update(cx, |page, cx| {
                    page.scroll_task = None;
                    page.prefs.scroll_y = (-f32::from(page.scroll.offset().y)).max(0.0);
                    page.persist(cx);
                })
                .ok();
            }));
        }
    }
}

fn queue_text(reason: &BoardAdmissionReason) -> String {
    match reason {
        BoardAdmissionReason::ProjectCapacity { limit } => {
            format!("waiting for project capacity ({limit} at a time)")
        }
        BoardAdmissionReason::HostCapacity { limit } => {
            format!("waiting for host capacity ({limit} at a time)")
        }
        BoardAdmissionReason::ProviderCapacity { limit, .. } => {
            format!("waiting for provider capacity ({limit} at a time)")
        }
        BoardAdmissionReason::Budget { detail } => format!("budget: {detail}"),
        BoardAdmissionReason::Dependency { blocked_by } => {
            format!("blocked by {}", blocked_by.join(", "))
        }
        BoardAdmissionReason::CheckoutBusy { .. } => "its checkout is busy".into(),
        BoardAdmissionReason::WaitingForStop { .. } => {
            "waiting for the previous run to stop".into()
        }
        BoardAdmissionReason::HostUnavailable { .. } => "the host is offline".into(),
        BoardAdmissionReason::SettingsUnresolved { fields } => {
            format!("settings needed: {}", fields.join(", "))
        }
    }
}

/// PR and check counts from current (non-historical) outcomes.
fn summarize_evidence(as_of: i64, outcomes: &luna_proto::BoardOutcomes) -> CardEvidence {
    let mut evidence = CardEvidence {
        as_of,
        ..CardEvidence::default()
    };
    for outcome in outcomes
        .outcomes
        .iter()
        .filter(|outcome| !outcome.historical)
    {
        match outcome.kind {
            OutcomeKind::PullRequest if evidence.pull_request.is_none() => {
                let mut label = outcome.label.clone();
                if let Some(state) = &outcome.state {
                    label.push_str(" · ");
                    label.push_str(state);
                }
                evidence.pull_request = Some(label);
            }
            OutcomeKind::Check => {
                evidence.checks_total += 1;
                match outcome.state.as_deref() {
                    Some("passed" | "success" | "succeeded") => evidence.checks_passed += 1,
                    Some("failed" | "failure" | "error") => evidence.checks_failed += 1,
                    _ => {}
                }
            }
            _ => {}
        }
    }
    evidence
}

impl Render for BoardPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        self.sync_frame(window, cx);
        if let Some(mount) = &self.authoring {
            // Discuss with AI (A6) takes the page while open: the
            // conversation needs the room, and the board is one Close away.
            return div()
                .id("board-page")
                .size_full()
                .child(mount.pane.clone())
                .into_any_element();
        }
        // The List view takes over below the narrow width: the last measured
        // page width, adjusted by how much the window changed since (so a
        // window resize switches in the same frame).
        let narrow_px = NARROW_WIDTH / 16.0 * f32::from(window.rem_size());
        let viewport = window.viewport_size().width;
        let width = {
            let geometry = self.geometry.borrow();
            geometry
                .page
                .map(|bounds| f32::from(bounds.size.width + (viewport - geometry.viewport_width)))
        };
        self.narrow = width.is_some_and(|width| width < narrow_px);

        let header = self.render_header(&theme, window, cx);
        let notice = self.render_notice(&theme, cx);
        let body = self.render_body(&theme, window, cx);
        let card_menu = self.render_card_menu(&theme, cx);
        let status = div()
            .id("board-status-announcer")
            .role(gpui::Role::Status)
            .aria_label(self.announcement.clone())
            .absolute()
            .bottom_0()
            .left_0()
            .size(px(1.0))
            .overflow_hidden();

        let geometry = self.geometry.clone();
        let entity = cx.entity_id();
        let was_narrow = self.narrow;
        div()
            .on_children_prepainted(move |bounds, window, _| {
                let page = bounds.first().copied();
                {
                    let mut geometry = geometry.borrow_mut();
                    geometry.page = page;
                    geometry.viewport_width = window.viewport_size().width;
                }
                // The estimate missed (the sidebar or a pane changed width):
                // render again with the measured width.
                let narrow = page.is_some_and(|page| f32::from(page.size.width) < narrow_px);
                if narrow != was_narrow {
                    window.on_next_frame(move |_, cx| cx.notify(entity));
                }
            })
            .id("board-page")
            .key_context("BoardPage")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let mods = event.keystroke.modifiers;
                if event.keystroke.key == "escape" && this.surface_open(cx) {
                    this.apply_surface(SurfaceRequest::Close, window, cx);
                    cx.stop_propagation();
                } else if mods.secondary() && event.keystroke.key == "f" {
                    let focus = this.search.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                    cx.stop_propagation();
                }
            }))
            .on_drag_move::<BoardCardDrag>(cx.listener(
                |this, event: &gpui::DragMoveEvent<BoardCardDrag>, _, cx| {
                    let drag = event.drag(cx).clone();
                    this.on_drag_over(event.event.position, &drag, cx);
                },
            ))
            .child(
                div()
                    .id("board-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .w_full()
                            .px(ui_rems(24.0))
                            .pt(ui_rems(24.0))
                            .pb(ui_rems(48.0))
                            .flex()
                            .flex_col()
                            .gap(ui_rems(14.0))
                            .child(header)
                            .children(notice)
                            .child(self.drafts.clone())
                            .child(body),
                    ),
            )
            // The overlay host sits outside the scroller so the drawer never
            // scrolls with the board.
            .child(self.surfaces.clone())
            .children(card_menu)
            .child(status)
            .into_any_element()
    }
}

impl BoardPage {
    fn render_notice(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let status_text = match self.model.read(cx).status() {
            BoardStatus::Live => None,
            BoardStatus::Loading => Some((NoticeTone::Info, "Loading tickets…".to_owned())),
            BoardStatus::Disconnected(reason) | BoardStatus::Error(reason) => {
                Some((NoticeTone::Error, reason.clone()))
            }
            BoardStatus::Unsupported => Some((
                NoticeTone::Error,
                "This host does not support the Board yet.".to_owned(),
            )),
        };
        let notice = self.notice.clone();
        if status_text.is_none() && notice.is_none() {
            return None;
        }
        let mut column = div().flex().flex_col().gap(ui_rems(6.0));
        if let Some((tone, text)) = status_text {
            column = column.child(notice_strip("board-status", theme, tone, text));
        }
        if let Some(notice) = notice {
            let open = notice.work_item_id.clone().filter(|id| {
                self.model.read(cx).item(id).is_some()
                    && self.surfaces.read(cx).detail_work_item_id(cx).as_deref()
                        != Some(id.as_str())
            });
            column = column.child(
                notice_strip("board-notice", theme, notice.tone, notice.text.to_string())
                    .when_some(open, |strip, id| {
                        strip.child(
                            crate::button::button(
                                "board-notice-open",
                                crate::button::ButtonKind::Ghost,
                                crate::button::ControlSize::Sm,
                                true,
                                theme,
                            )
                            .aria_label("Open ticket")
                            .child("Open ticket")
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.select(&id, true, window, cx);
                                    this.open_detail(id.clone(), window, cx);
                                },
                            )),
                        )
                    })
                    .child(
                        crate::button::icon_button(
                            "board-notice-dismiss",
                            crate::icons::CLOSE,
                            crate::button::ControlSize::Sm,
                            true,
                            theme,
                        )
                        .aria_label("Dismiss")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notice = None;
                            cx.notify();
                        })),
                    ),
            );
        }
        Some(column.into_any_element())
    }

    /// Columns, the List view, or the empty state.
    fn render_body(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (count, live) = {
            let model = self.model.read(cx);
            (
                model.items().len(),
                matches!(model.status(), BoardStatus::Live),
            )
        };
        if count == 0 && live {
            return if self.any_filter(cx) {
                self.render_no_matches(theme, cx)
            } else {
                self.render_empty(theme, cx)
            };
        }
        match self.effective_view() {
            BoardViewMode::Board => self.render_columns(theme, window, cx),
            BoardViewMode::List => self.render_list(theme, window, cx),
        }
    }

    fn render_no_matches(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("board-no-matches")
            .flex()
            .flex_col()
            .items_start()
            .gap(ui_rems(8.0))
            .py(ui_rems(24.0))
            .child(widgets::page_subtitle(
                theme,
                "No tickets match these filters.",
            ))
            .child(
                crate::button::button(
                    "board-clear-filters",
                    crate::button::ButtonKind::Secondary,
                    crate::button::ControlSize::Md,
                    true,
                    theme,
                )
                .aria_label("Clear filters")
                .child("Clear filters")
                .on_click(cx.listener(|this, _, _, cx| this.clear_filters(cx))),
            )
            .into_any_element()
    }

    /// The empty state: what a ticket is and the four ways to make one. A
    /// local ticket needs no GitHub access.
    fn render_empty(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let options: [(&str, &str, &str, &'static str); 4] = [
            (
                "board-empty-manual",
                "Write manually",
                "Title, brief and settings. Saved to Backlog; no chat, no GitHub needed.",
                crate::icons::PEN,
            ),
            (
                "board-empty-discuss",
                "Discuss with AI",
                "Shape the ticket in a temporary conversation that never becomes a chat.",
                crate::icons::CHAT_ROUND_LINE,
            ),
            (
                "board-empty-github",
                "From GitHub issue",
                "Import an issue as a ticket. Optional.",
                crate::icons::INBOX_IN,
            ),
            (
                "board-empty-chat",
                "Add existing chat",
                "Turn a chat into a ticket in place; nothing is resent.",
                crate::icons::CHAT_LINE,
            ),
        ];
        let geometry = self.geometry.clone();
        let labels: Vec<String> = options.iter().map(|(id, ..)| (*id).to_owned()).collect();
        let tiles: Vec<AnyElement> = options
            .into_iter()
            .map(|(id, title, description, glyph)| {
                div()
                    .id(SharedString::from(id))
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(format!("{title}: {description}")))
                    .tab_index(0)
                    .flex_1()
                    .min_w(ui_rems(180.0))
                    .flex()
                    .flex_col()
                    .gap(ui_rems(6.0))
                    .p(ui_rems(14.0))
                    .rounded(px(crate::theme::radius::LG))
                    .border_1()
                    .border_color(theme.separator(true))
                    .bg(theme.fill(crate::theme::FillTier::Quaternary))
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme.accent.opacity(0.45)))
                    .focus_visible({
                        let ring = theme.focus_ring();
                        move |style| crate::theme::focus_ring(style, ring)
                    })
                    .on_key_down(crate::swallow_activation_key)
                    .on_click(cx.listener(move |this, _, window, cx| match id {
                        "board-empty-manual" => this.open_create(false, window, cx),
                        "board-empty-discuss" => this.open_create(true, window, cx),
                        "board-empty-github" => this.open_github(None, cx),
                        _ => this.open_add_chat(cx),
                    }))
                    .child(
                        crate::icons::icon(glyph)
                            .size(ui_rems(18.0))
                            .text_color(theme.readable_muted(0.9)),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(13.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(12.0))
                            .line_height(ui_rems(17.0))
                            .text_color(theme.readable_muted(0.85))
                            .child(description),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .id("board-empty")
            .flex()
            .flex_col()
            .gap(ui_rems(14.0))
            .py(ui_rems(16.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui_rems(4.0))
                    .child(
                        div()
                            .text_size(ui_rems(15.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("No tickets yet"),
                    )
                    .child(widgets::page_subtitle(
                        theme,
                        "A ticket describes an outcome. Saving one creates no chat; moving it to \
                         Ready starts an agent and moving it back stops it.",
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(ui_rems(10.0))
                    .on_children_prepainted(move |bounds, _, _| {
                        let mut geometry = geometry.borrow_mut();
                        for (label, bounds) in labels.iter().zip(bounds) {
                            geometry.controls.insert(label.clone(), bounds);
                        }
                    })
                    .children(tiles),
            )
            .into_any_element()
    }

    /// A Board menu card: rows with one shared keyboard/pointer highlight.
    pub(super) fn render_menu_card(
        &mut self,
        entries: Vec<MenuEntry>,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let cursor = self.menu_cursor;
        let mut rows: Vec<AnyElement> = Vec::new();
        for (ix, entry) in entries.into_iter().enumerate() {
            if let Some(heading) = entry.heading {
                if ix > 0 {
                    rows.push(crate::popover::menu_separator().into_any_element());
                }
                rows.push(crate::popover::menu_heading(theme, heading).into_any_element());
            }
            let enabled = entry.enabled;
            let action = entry.action.clone();
            let label = entry.label.clone();
            rows.push(
                crate::popover::menu_row_nav(theme, entry.checked, cursor == Some(ix))
                    .id(SharedString::from(format!("board-menu-{}", entry.key)))
                    .role(gpui::Role::MenuItem)
                    .aria_label(SharedString::from(label.clone()))
                    .when(!enabled, |row| row.opacity(0.45).cursor_default())
                    .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                        if this.menu_cursor != Some(ix) {
                            this.menu_cursor = Some(ix);
                            cx.notify();
                        }
                    }))
                    .when(enabled, |row| {
                        row.on_click(cx.listener(move |this, _, window, cx| {
                            this.run_menu_action(action.clone(), window, cx);
                        }))
                    })
                    .when_some(entry.glyph, |row, glyph| {
                        row.child(
                            crate::icons::icon(glyph)
                                .size(ui_rems(14.0))
                                .flex_none()
                                .text_color(theme.readable_muted(0.9)),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(label)),
                    )
                    .when_some(entry.hint, |row, hint| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(ui_rems(11.0))
                                .text_color(theme.readable_muted(0.7))
                                .child(SharedString::from(hint)),
                        )
                    })
                    .into_any_element(),
            );
        }
        crate::popover::popover_card(theme)
            .id("board-menu")
            .role(gpui::Role::Menu)
            .track_focus(&self.menu_focus)
            .w(ui_rems(width))
            .max_h(ui_rems(420.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.on_menu_key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .children(rows)
    }

    /// The card context menu, at the pointer or under the focused card.
    fn render_card_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(BoardMenu::Card { work_item_id, at }) = self.menu.get().cloned() else {
            return None;
        };
        let position = at.or_else(|| {
            self.geometry
                .borrow()
                .card_bounds(&work_item_id)
                .map(|bounds| gpui::point(bounds.left() + px(12.0), bounds.bottom()))
        })?;
        let entries = self.menu_entries(
            &BoardMenu::Card {
                work_item_id: work_item_id.clone(),
                at,
            },
            cx,
        );
        let closing = self.menu.closing_since();
        let card = self.render_menu_card(entries, 250.0, theme, cx);
        Some(crate::popover::menu_at(
            SharedString::from(format!("board-card-actions-{work_item_id}")),
            position,
            card.into_any_element(),
            closing,
        ))
    }
}

/// A one-line notice strip in the page's tone.
fn notice_strip(
    id: &'static str,
    theme: &Theme,
    tone: NoticeTone,
    text: String,
) -> gpui::Stateful<gpui::Div> {
    let (color, glyph) = match tone {
        NoticeTone::Info => (theme.readable_muted(0.95), crate::icons::INFO_CIRCLE),
        NoticeTone::Warning => (
            theme.readable_tone(theme.warning, 0.95),
            crate::icons::DANGER_TRIANGLE,
        ),
        NoticeTone::Error => (
            theme.readable_tone(theme.danger, 0.95),
            crate::icons::DANGER_CIRCLE,
        ),
    };
    let wash = match tone {
        NoticeTone::Info => theme.fill(crate::theme::FillTier::Quaternary),
        NoticeTone::Warning => theme.warning.opacity(0.07),
        NoticeTone::Error => theme.danger.opacity(0.07),
    };
    div()
        .id(id)
        .role(gpui::Role::Status)
        .aria_label(SharedString::from(text.clone()))
        .flex()
        .flex_row()
        .items_center()
        .gap(ui_rems(8.0))
        .px(ui_rems(12.0))
        .py(ui_rems(6.0))
        .rounded(px(crate::theme::radius::MD))
        .bg(wash)
        .text_size(ui_rems(12.0))
        .line_height(ui_rems(16.0))
        .text_color(color)
        .child(
            crate::icons::icon(glyph)
                .size(ui_rems(14.0))
                .flex_none()
                .text_color(color),
        )
        .child(div().flex_1().min_w_0().child(SharedString::from(text)))
}

/// QA: what the page shows and where (the board-page fixture's readback).
#[cfg(feature = "appshots-fixture")]
impl BoardPage {
    pub fn fixture_readback(&self, window: &Window, cx: &App) -> serde_json::Value {
        let rect = |bounds: Bounds<Pixels>| {
            serde_json::json!({
                "x": f32::from(bounds.origin.x),
                "y": f32::from(bounds.origin.y),
                "w": f32::from(bounds.size.width),
                "h": f32::from(bounds.size.height),
            })
        };
        let model = self.model.read(cx);
        let geometry = self.geometry.borrow();
        let focused = self
            .card_focus
            .iter()
            .find(|(_, handle)| handle.is_focused(window))
            .map(|(id, _)| id.clone());
        let cards: Vec<serde_json::Value> = model
            .items()
            .iter()
            .map(|view| {
                let badge = super::card::phase_badge(view);
                serde_json::json!({
                    "id": view.item.id,
                    "key": view.item.key,
                    "title": view.item.title,
                    "project": view.item.project_id,
                    "column": view.item.column.as_str(),
                    "rank": view.item.rank,
                    "phase": format!("{:?}", view.phase),
                    "badge": badge.label,
                    "tone": badge.tone.as_str(),
                    "attention": super::card::attention_text(&view.attention),
                    "label": super::card::card_label(view, self.project_label_for(&view.item, cx).as_deref()),
                    "archived": view.item.archived,
                    "bounds": geometry.card_bounds(&view.item.id).map(rect),
                })
            })
            .collect();
        let columns: Vec<serde_json::Value> = BoardColumn::ALL
            .into_iter()
            .map(|column| {
                serde_json::json!({
                    "column": column.as_str(),
                    "label": column.label(),
                    "help": column.help(),
                    "count": model.column(column).count(),
                    "ids": model.column(column).map(|view| view.item.id.clone()).collect::<Vec<_>>(),
                    "bounds": geometry
                        .columns
                        .iter()
                        .find(|(c, _)| *c == column)
                        .map(|(_, bounds)| rect(*bounds)),
                })
            })
            .collect();
        serde_json::json!({
            "status": format!("{:?}", model.status()),
            "scope": model.scope(),
            "scopeLabel": self.scope_label(cx),
            "filters": {
                "text": model.filters().text,
                "attentionOnly": model.filters().attention_only,
                "provider": model.filters().provider,
                "source": model.filters().source,
                "archived": model.filters().archived,
            },
            "view": format!("{:?}", self.prefs.view),
            "effectiveView": format!("{:?}", self.effective_view()),
            "narrow": self.narrow,
            "prefs": self.prefs,
            "selected": self.selected,
            "focused": focused,
            "pageFocused": self.focus.contains_focused(window, cx),
            "menu": self.menu.get().map(|menu| format!("{menu:?}")),
            "menuEntries": self.menu.get().map(|menu| self.menu_entries(menu, cx)
                .into_iter()
                .map(|entry| serde_json::json!({"key": entry.key, "label": entry.label, "enabled": entry.enabled}))
                .collect::<Vec<_>>()),
            "overlay": self.surfaces.read(cx).surface().map(|surface| surface.key()),
            "overlayItem": self.surfaces.read(cx).detail_work_item_id(cx),
            "notice": self.notice.as_ref().map(|notice| notice.text.to_string()),
            "announcement": self.announcement.to_string(),
            "drafts": self.drafts.read(cx).sessions().iter().map(|draft| draft.id.clone()).collect::<Vec<_>>(),
            "authoring": self.authoring.is_some(),
            "pending": self.pending,
            "issued": self.issued,
            "dragOver": self.drag_over.as_ref().map(|target| serde_json::json!({
                "column": target.column.as_str(),
                "index": target.index,
                "after": target.after_id,
                "before": target.before_id,
            })),
            "scroll": {
                "offset": -f32::from(self.scroll.offset().y),
                "max": f32::from(self.scroll.max_offset().y),
                "viewport": rect(self.scroll.bounds()),
            },
            "page": geometry.page.map(rect),
            "columns": columns,
            "cards": cards,
            "controls": geometry.controls.iter().map(|(label, bounds)| (label.clone(), rect(*bounds))).collect::<serde_json::Map<_, _>>(),
            "emptyState": model.items().is_empty() && matches!(model.status(), BoardStatus::Live) && !self.any_filter(cx),
        })
    }
}
