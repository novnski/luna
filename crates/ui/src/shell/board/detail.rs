//! Ticket detail drawer (A9) and the Board's overlay host.
//!
//! [`BoardSurfaces`] is the one overlay the Board page mounts: the New
//! ticket chooser, Write manually or one ticket's [`BoardDetail`] drawer.
//! Discuss with AI leaves through [`BoardSurfacesEvent::DiscussWithAi`] to
//! A6's conversation, which the Board page mounts in place of its body, and
//! From GitHub issue through [`BoardSurfacesEvent::OpenGitHub`] to A7's
//! picker, which the Shell hosts. The page (A8)
//! calls its entry points:
//!
//! * [`BoardSurfaces::open_detail`] — card click / Enter on a card.
//! * [`BoardSurfaces::open_new_ticket`] — the New ticket button (`None`
//!   project: the chooser asks for one; never guessed from All projects).
//!
//! Navigation leaves through [`BoardSurfacesEvent`]: the Shell subscribes in
//! `Shell::board_page` and routes Go to chat, Needs attention and writeback
//! requests to the existing chat, attention and GitHub surfaces.
//!
//! The drawer renders what the engine derived (`BoardRead`): key and title
//! (rename through `BoardUpdate`, which also renames the working chat), the
//! editable brief with expected-revision conflicts, Send update to chat while
//! a run is active, requested/resolved/observed settings, the working chat
//! and its location, the same Start/Continue/Stop moves as drag, dispatch
//! history, review, evidence, the source issue, blockers and archive. It
//! never computes phase, authority or permitted actions itself.
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable as _, IntoElement,
    KeyDownEvent, ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Task,
    WeakEntity, Window, div, prelude::*, px,
};
use luna_proto::{
    BOARD_TITLE_MAX_CHARS, BoardAction, BoardAdmissionReason, BoardArchiveRequest,
    BoardAttentionKind, BoardChatRole, BoardColumn, BoardContextMode, BoardControlAction,
    BoardControlRequest, BoardDestination, BoardError, BoardExecutionOptions,
    BoardExecutionOptionsRequest, BoardFollowUp, BoardItemChanges, BoardItemDetail,
    BoardMoveEffect, BoardMoveRequest, BoardMutationReceipt, BoardNavigationRequest,
    BoardNavigationTarget, BoardPhase, BoardReadRequest, BoardReceiptRequest,
    BoardResolveSettingsRequest, BoardResolvedSettings, BoardReviewStartRequest,
    BoardSendUpdateRequest, CrossTaskSendMode, IssueLinkRole, WorkItemView,
};

use super::api::{BoardApi, BoardCallError};
use super::evidence::{self, EvidenceContext};
use super::github_picker::{WritebackChoice, writeback_choices};
use super::manual_form::{ChooserEvent, ManualFormEvent, ManualTicketForm, NewTicketChooser};
use super::model::{BoardModel, BoardModelEvent};
use super::settings_controls::{
    ExecutionSettingsDraft, ExecutionSettingsEditor, describe_implementation, describe_resolved,
    describe_review, harness_label, model_label,
};
use crate::button::{self, ButtonKind, ControlSize};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::settings::widgets;
#[cfg(feature = "appshots-fixture")]
use crate::shell::Route;
use crate::shell::Shell;
use crate::state::AppState;
use crate::theme::{FillTier, Theme};
use crate::typography::ui_rems;

/// Widest the drawer grows; a narrower page gives it the full width.
const DRAWER_WIDTH: f32 = 600.0;
/// Bound for each detail list read with the ticket.
const HISTORY_LIMIT: u32 = 50;
/// Characters of a concurrent brief shown in a conflict.
const CONFLICT_PREVIEW: usize = 600;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// A fresh client idempotency key (`[A-Za-z0-9_.:-]`). Reuse it verbatim
/// after a lost answer; mint a new one for a deliberately different action.
pub(crate) fn new_request_id(kind: &str) -> String {
    format!("ui-{kind}-{}", uuid::Uuid::new_v4().simple())
}

/// "Oct 9, 14:32" in local time.
pub(crate) fn format_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|at| {
            at.with_timezone(&chrono::Local)
                .format("%b %-d, %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "an unknown time".into())
}

/// Records what the Board surfaces painted (control bounds, key texts,
/// chips) for the native QA fixture. A no-op outside fixture builds.
#[derive(Clone, Default)]
pub struct BoardProbe {
    #[cfg(feature = "appshots-fixture")]
    frame: std::rc::Rc<std::cell::RefCell<ProbeFrame>>,
}

#[cfg(feature = "appshots-fixture")]
#[derive(Default)]
struct ProbeFrame {
    bounds: std::collections::BTreeMap<String, [f32; 4]>,
    texts: std::collections::BTreeMap<String, String>,
    chips: std::collections::BTreeMap<String, (String, bool)>,
}

impl BoardProbe {
    /// Forget the last frame (the overlay host calls it before rendering).
    pub fn clear(&self) {
        #[cfg(feature = "appshots-fixture")]
        {
            *self.frame.borrow_mut() = ProbeFrame::default();
        }
    }

    /// A layout-neutral child that records its parent's painted bounds.
    pub fn bounds_el(&self, name: impl Into<String>) -> Option<AnyElement> {
        #[cfg(feature = "appshots-fixture")]
        {
            let frame = self.frame.clone();
            let name = name.into();
            Some(
                gpui::canvas(
                    move |bounds, _, _| {
                        frame.borrow_mut().bounds.insert(
                            name,
                            [
                                f32::from(bounds.origin.x),
                                f32::from(bounds.origin.y),
                                f32::from(bounds.size.width),
                                f32::from(bounds.size.height),
                            ],
                        );
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
                .into_any_element(),
            )
        }
        #[cfg(not(feature = "appshots-fixture"))]
        {
            let _ = name;
            None
        }
    }

    /// A text the surface showed this frame.
    pub fn text(&self, key: impl Into<String>, value: impl Into<String>) {
        #[cfg(feature = "appshots-fixture")]
        {
            self.frame
                .borrow_mut()
                .texts
                .insert(key.into(), value.into());
        }
        #[cfg(not(feature = "appshots-fixture"))]
        {
            let _ = (key, value);
        }
    }

    /// A choice chip the surface showed this frame.
    pub fn chip(&self, id: &str, label: &str, selected: bool) {
        #[cfg(feature = "appshots-fixture")]
        {
            self.frame
                .borrow_mut()
                .chips
                .insert(id.to_owned(), (label.to_owned(), selected));
        }
        #[cfg(not(feature = "appshots-fixture"))]
        {
            let _ = (id, label, selected);
        }
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn json(&self) -> serde_json::Value {
        let frame = self.frame.borrow();
        serde_json::json!({
            "bounds": frame.bounds,
            "texts": frame.texts,
            "chips": frame
                .chips
                .iter()
                .map(|(id, (label, selected))| {
                    (id.clone(), serde_json::json!({ "label": label, "selected": selected }))
                })
                .collect::<serde_json::Map<_, _>>(),
        })
    }
}

fn attention_label(kind: BoardAttentionKind) -> &'static str {
    match kind {
        BoardAttentionKind::Question => "Question waiting",
        BoardAttentionKind::Approval => "Approval needed",
        BoardAttentionKind::ProviderFailure => "Provider failed",
        BoardAttentionKind::AuthRequired => "Sign-in required",
        BoardAttentionKind::HostUnavailable => "Host unavailable",
        BoardAttentionKind::CompatibilityBlocked => "Not compatible",
        BoardAttentionKind::SettingsUnresolved => "Settings unresolved",
        BoardAttentionKind::ModelUnavailable => "Model unavailable",
        BoardAttentionKind::ReviewBlocked => "Review still blocked after the repair rounds",
        BoardAttentionKind::ReviewUnavailable => "Reviewer unavailable",
        BoardAttentionKind::StopUnknown => "Stop not confirmed",
        BoardAttentionKind::DeliveryUnknown => "Delivery not confirmed",
        BoardAttentionKind::DependencyBlocked => "Blocked by a dependency",
        BoardAttentionKind::CleanupFailed => "Reviewer cleanup failed",
        BoardAttentionKind::AgentBlocked => "Agent reported a blocker",
        BoardAttentionKind::Other => "Needs attention",
    }
}

fn admission_text(reason: &BoardAdmissionReason) -> String {
    match reason {
        BoardAdmissionReason::ProjectCapacity { limit } => {
            format!("the project already runs {limit} tickets")
        }
        BoardAdmissionReason::HostCapacity { limit } => {
            format!("this host is at its limit of {limit} runs")
        }
        BoardAdmissionReason::ProviderCapacity { harness, limit } => {
            format!(
                "{} is at its limit of {limit} runs",
                harness_label(*harness)
            )
        }
        BoardAdmissionReason::Budget { detail } => format!("budget: {detail}"),
        BoardAdmissionReason::Dependency { blocked_by } => {
            format!("blocked by {} open ticket(s)", blocked_by.len())
        }
        BoardAdmissionReason::CheckoutBusy { .. } => {
            "its checkout is busy with another chat".into()
        }
        BoardAdmissionReason::WaitingForStop {
            execution_generation,
        } => format!("waiting for run {execution_generation} to stop"),
        BoardAdmissionReason::HostUnavailable { .. } => "the project's host is unavailable".into(),
        BoardAdmissionReason::SettingsUnresolved { fields } => {
            format!("settings to resolve first: {}", fields.join(", "))
        }
    }
}

fn effect_text(effect: &BoardMoveEffect, column: BoardColumn) -> String {
    match effect {
        BoardMoveEffect::None => format!("Moved to {}.", column.label()),
        BoardMoveEffect::FirstStart { .. } => {
            "Started: the brief was sent once to the ticket's new working chat.".into()
        }
        BoardMoveEffect::Continue { .. } => {
            "Continuing in the same chat from where it stopped.".into()
        }
        BoardMoveEffect::Queued { reason } => {
            format!("In Ready, waiting: {}.", admission_text(reason))
        }
        BoardMoveEffect::CancelledUnsent => {
            "Moved to Backlog; the launch that was not sent yet was cancelled.".into()
        }
        BoardMoveEffect::StopRequested {
            execution_generation,
        } => format!(
            "Stop requested for run {execution_generation}. The card shows Stopping until it is \
             confirmed."
        ),
        BoardMoveEffect::Coalesced => "Merged with a move already in progress.".into(),
        BoardMoveEffect::Reconciling => {
            "Checking an earlier attempt's outcome before doing anything.".into()
        }
    }
}

/// A run is under way: the brief it was sent stays frozen until an
/// explicit update.
fn run_active(view: &WorkItemView) -> bool {
    matches!(
        view.phase,
        BoardPhase::Starting
            | BoardPhase::Working
            | BoardPhase::Checking
            | BoardPhase::Reviewing
            | BoardPhase::Fixing
    )
}

/// The newest brief revision any instruction delivered to the working chat.
fn last_sent_brief_revision(detail: &BoardItemDetail) -> Option<u32> {
    let working = detail
        .view
        .working_chat
        .as_ref()
        .map(|chat| chat.task_id.as_str());
    detail
        .launches
        .iter()
        .filter(|launch| working.is_none_or(|task| launch.task_id == task))
        .map(|launch| launch.brief_revision)
        .max()
}

fn send_update_offered(detail: &BoardItemDetail) -> bool {
    let view = &detail.view;
    view.actions.contains(&BoardAction::SendUpdate)
        && run_active(view)
        && last_sent_brief_revision(detail).is_some_and(|sent| view.item.brief_revision > sent)
}

// ---------------------------------------------------------------------------
// Overlay host
// ---------------------------------------------------------------------------

/// What the overlay shows.
pub enum BoardSurface {
    Chooser(Entity<NewTicketChooser>),
    Manual(Entity<ManualTicketForm>),
    Detail(Entity<BoardDetail>),
}

impl BoardSurface {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Chooser(_) => "chooser",
            Self::Manual(_) => "manual",
            Self::Detail(_) => "detail",
        }
    }
}

/// Requests the Board surfaces hand to the rest of the app.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardSurfacesEvent {
    /// Open the existing chat (main chat, or the main parent's workbench with
    /// the exact side chat). Never relaunches, resends or reparents.
    GoToChat(BoardNavigationTarget),
    /// Open the existing approval/question/error UI for this item.
    OpenAttention {
        attention_id: Option<String>,
        task_id: Option<String>,
    },
    /// Open the GitHub writeback dialog (A7) for this issue reference and
    /// the write the person chose.
    Writeback {
        work_item_id: String,
        issue_ref_id: String,
        display: String,
        choice: WritebackChoice,
    },
    /// From GitHub issue (A7's picker, hosted by the Shell).
    OpenGitHub { project_id: String },
    /// Discuss with AI (A6's conversation, mounted by the Board page).
    DiscussWithAi { project_id: String },
}

/// The Board's overlay: one surface at a time in a right-hand drawer.
pub struct BoardSurfaces {
    state: Entity<AppState>,
    model: Entity<BoardModel>,
    probe: BoardProbe,
    surface: Option<BoardSurface>,
    focus: FocusHandle,
    /// Give the keyboard back to the open surface on the next frame.
    refocus: bool,
    _surface: Vec<Subscription>,
}

impl EventEmitter<BoardSurfacesEvent> for BoardSurfaces {}

impl BoardSurfaces {
    pub fn new(state: Entity<AppState>, model: Entity<BoardModel>, cx: &mut Context<Self>) -> Self {
        Self {
            state,
            model,
            probe: BoardProbe::default(),
            surface: None,
            focus: cx.focus_handle(),
            refocus: false,
            _surface: Vec::new(),
        }
    }

    /// An overlay above the drawer (A7's writeback dialog or issue picker)
    /// closed and took the focused element with it: return the keyboard to
    /// the open surface so Escape and Tab reach it again.
    pub fn restore_focus(&mut self, cx: &mut Context<Self>) {
        if self.surface.is_some() {
            self.refocus = true;
            cx.notify();
        }
    }

    /// The element that owns the keyboard for the open surface.
    fn surface_focus(&self, cx: &App) -> FocusHandle {
        match &self.surface {
            Some(BoardSurface::Detail(detail)) => detail.read(cx).focus.clone(),
            Some(BoardSurface::Chooser(chooser)) => chooser.read(cx).focus_handle(),
            Some(BoardSurface::Manual(form)) => form.read(cx).title_focus(cx),
            None => self.focus.clone(),
        }
    }

    // Read by the Board page (A8), e.g. to keep a card selected.
    #[allow(dead_code)]
    pub fn surface(&self) -> Option<&BoardSurface> {
        self.surface.as_ref()
    }

    #[allow(dead_code)]
    pub fn is_open(&self) -> bool {
        self.surface.is_some()
    }

    /// The ticket whose drawer is open (the page keeps its card selected).
    #[allow(dead_code)]
    pub fn detail_work_item_id(&self, cx: &App) -> Option<String> {
        match &self.surface {
            Some(BoardSurface::Detail(detail)) => Some(detail.read(cx).work_item_id.clone()),
            _ => None,
        }
    }

    fn set_surface(
        &mut self,
        surface: BoardSurface,
        subscriptions: Vec<Subscription>,
        cx: &mut Context<Self>,
    ) {
        self.surface = Some(surface);
        self._surface = subscriptions;
        cx.notify();
    }

    /// Open one ticket's detail drawer (by id or key).
    pub fn open_detail(
        &mut self,
        work_item_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(BoardSurface::Detail(detail)) = &self.surface
            && detail.read(cx).work_item_id == work_item_id
        {
            let focus = detail.read(cx).focus.clone();
            detail.update(cx, |detail, cx| detail.reload(cx));
            window.focus(&focus, cx);
            return;
        }
        let model = self.model.clone();
        let state = self.state.clone();
        let probe = self.probe.clone();
        let detail = cx.new(|cx| BoardDetail::new(model, state, work_item_id, probe, cx));
        let subscription = cx.subscribe_in(
            &detail,
            window,
            |this, _, event: &BoardDetailEvent, window, cx| match event {
                BoardDetailEvent::Close => this.close(window, cx),
                BoardDetailEvent::OpenTicket(id) => this.open_detail(id.clone(), window, cx),
                BoardDetailEvent::GoToChat(target) => {
                    cx.emit(BoardSurfacesEvent::GoToChat(target.clone()))
                }
                BoardDetailEvent::OpenAttention {
                    attention_id,
                    task_id,
                } => cx.emit(BoardSurfacesEvent::OpenAttention {
                    attention_id: attention_id.clone(),
                    task_id: task_id.clone(),
                }),
                BoardDetailEvent::Writeback {
                    work_item_id,
                    issue_ref_id,
                    display,
                    choice,
                } => cx.emit(BoardSurfacesEvent::Writeback {
                    work_item_id: work_item_id.clone(),
                    issue_ref_id: issue_ref_id.clone(),
                    display: display.clone(),
                    choice: choice.clone(),
                }),
            },
        );
        let focus = detail.read(cx).focus.clone();
        self.set_surface(BoardSurface::Detail(detail), vec![subscription], cx);
        window.focus(&focus, cx);
    }

    /// Open the New ticket chooser. `None` (All projects) asks for the
    /// project explicitly.
    pub fn open_new_ticket(
        &mut self,
        project_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        let probe = self.probe.clone();
        let chooser = cx.new(|cx| NewTicketChooser::new(state, project_id, probe, cx));
        let subscription = cx.subscribe_in(
            &chooser,
            window,
            |this, _, event: &ChooserEvent, window, cx| match event {
                ChooserEvent::WriteManually { project_id } => {
                    this.open_manual_form(project_id.clone(), window, cx)
                }
                ChooserEvent::DiscussWithAi { project_id } => {
                    this.open_authoring(project_id.clone(), window, cx)
                }
                ChooserEvent::FromGitHub { project_id } => {
                    this.open_github(project_id.clone(), window, cx)
                }
                ChooserEvent::Close => this.close(window, cx),
            },
        );
        let focus = chooser.read(cx).focus_handle();
        self.set_surface(BoardSurface::Chooser(chooser), vec![subscription], cx);
        window.focus(&focus, cx);
    }

    /// Write manually for one project.
    pub fn open_manual_form(
        &mut self,
        project_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let model = self.model.clone();
        let state = self.state.clone();
        let probe = self.probe.clone();
        let form = cx.new(|cx| {
            ManualTicketForm::new(model, state, Some(project_id.clone()), probe, window, cx)
        });
        let back_project = project_id.clone();
        let subscription = cx.subscribe_in(
            &form,
            window,
            move |this, _, event: &ManualFormEvent, window, cx| match event {
                ManualFormEvent::OpenTicket(id) => this.open_detail(id.clone(), window, cx),
                ManualFormEvent::Back => {
                    this.open_new_ticket(Some(back_project.clone()), window, cx)
                }
                ManualFormEvent::Close => this.close(window, cx),
            },
        );
        let focus = form.read(cx).title_focus(cx);
        self.set_surface(BoardSurface::Manual(form), vec![subscription], cx);
        window.focus(&focus, cx);
    }

    /// Discuss with AI: A6's temporary conversation, which the Board page
    /// mounts in place of its body (it has no title/description fields; the
    /// conversation drafts the ticket). The drawer closes.
    pub fn open_authoring(
        &mut self,
        project_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close(window, cx);
        cx.emit(BoardSurfacesEvent::DiscussWithAi { project_id });
    }

    /// From a GitHub issue: A7's picker, which the Shell hosts (one host for
    /// its import/open-card events); the drawer closes.
    pub fn open_github(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.close(window, cx);
        cx.emit(BoardSurfacesEvent::OpenGitHub { project_id });
    }

    /// Close whatever is open.
    pub fn close(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.surface = None;
        self._surface.clear();
        cx.notify();
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_readback(&self, window: &Window, cx: &App) -> serde_json::Value {
        let surface = self.surface.as_ref().map(BoardSurface::key);
        let state = match &self.surface {
            Some(BoardSurface::Chooser(chooser)) => chooser.read(cx).fixture_state(),
            Some(BoardSurface::Manual(form)) => form.read(cx).fixture_state(window, cx),
            Some(BoardSurface::Detail(detail)) => detail.read(cx).fixture_state(window, cx),
            None => serde_json::Value::Null,
        };
        serde_json::json!({
            "surface": surface,
            "state": state,
            "probe": self.probe.json(),
        })
    }
}

impl Render for BoardSurfaces {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.probe.clear();
        if std::mem::take(&mut self.refocus) && self.surface.is_some() {
            let focus = self.surface_focus(cx);
            window.defer(cx, move |window, cx| window.focus(&focus, cx));
        }
        let Some(surface) = &self.surface else {
            return div().id("board-overlay-closed").into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let content = match surface {
            BoardSurface::Chooser(entity) => entity.clone().into_any_element(),
            BoardSurface::Manual(entity) => entity.clone().into_any_element(),
            BoardSurface::Detail(entity) => entity.clone().into_any_element(),
        };
        self.probe.text("board-surface", surface.key());
        div()
            .id("board-overlay")
            .absolute()
            .inset_0()
            .flex()
            .flex_row()
            .justify_end()
            .child(
                div()
                    .id("board-scrim")
                    .absolute()
                    .inset_0()
                    .bg(theme.scrim())
                    .occlude()
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.close(window, cx)),
                    ),
            )
            .child(
                div()
                    .id("board-drawer")
                    .track_focus(&self.focus)
                    .relative()
                    .h_full()
                    .w_full()
                    .max_w(ui_rems(DRAWER_WIDTH))
                    .bg(theme.bg)
                    .border_l_1()
                    .border_color(theme.border)
                    .occlude()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .text_color(theme.text)
                    // Escape closes the drawer (an open inline edit takes it
                    // first); Tab walks controls through the Shell's handler.
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        let key = &event.keystroke;
                        if key.key == "escape"
                            && !key.modifiers.control
                            && !key.modifiers.alt
                            && !key.modifiers.platform
                        {
                            this.close(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(div().flex_1().min_h_0().w_full().child(content))
                    .children(self.probe.bounds_el("board-drawer")),
            )
            .children(self.probe.bounds_el("board-overlay"))
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// The detail drawer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum BoardDetailEvent {
    Close,
    OpenTicket(String),
    GoToChat(BoardNavigationTarget),
    OpenAttention {
        attention_id: Option<String>,
        task_id: Option<String>,
    },
    Writeback {
        work_item_id: String,
        issue_ref_id: String,
        display: String,
        choice: WritebackChoice,
    },
}

enum Loaded {
    Loading,
    Ready(Box<BoardItemDetail>),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditTarget {
    Title,
    Brief,
    Settings,
}

impl EditTarget {
    fn key(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Brief => "brief",
            Self::Settings => "settings",
        }
    }
}

/// One in-place edit of the ticket against the revision it started from.
struct Edit {
    base_revision: u64,
    saving: bool,
    error: Option<String>,
    /// The engine refused a stale write; this is the current ticket.
    conflict: Option<Box<WorkItemView>>,
    /// Reused after a lost answer.
    request_id: Option<String>,
    input: Option<Entity<ComposerInput>>,
    editor: Option<Entity<ExecutionSettingsEditor>>,
    _subscription: Option<Subscription>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeTone {
    Info,
    Success,
    Warning,
    Error,
}

impl NoticeTone {
    fn key(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

struct Notice {
    tone: NoticeTone,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailAction {
    Start,
    Continue,
    MoveToReady,
    MoveToBacklog,
    Stop,
    Reopen,
    Retry { adopt_latest: bool },
    Review,
    Archive,
    Unarchive,
}

impl DetailAction {
    fn key(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Continue => "continue",
            Self::MoveToReady => "move-ready",
            Self::MoveToBacklog => "move-backlog",
            Self::Stop => "stop",
            Self::Reopen => "reopen",
            Self::Retry {
                adopt_latest: false,
            } => "retry-sent",
            Self::Retry { adopt_latest: true } => "retry-latest",
            Self::Review => "review",
            Self::Archive => "archive",
            Self::Unarchive => "unarchive",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Continue => "Continue",
            Self::MoveToReady => "Move to Ready",
            Self::MoveToBacklog => "Move to Backlog",
            Self::Stop => "Stop · Move to Backlog",
            Self::Reopen => "Reopen · Move to Backlog",
            Self::Retry {
                adopt_latest: false,
            } => "Retry with the sent brief",
            Self::Retry { adopt_latest: true } => "Retry with the latest brief",
            Self::Review => "Review now",
            Self::Archive => "Archive",
            Self::Unarchive => "Unarchive",
        }
    }
}

/// The drawer for one ticket.
pub struct BoardDetail {
    pub(crate) model: Entity<BoardModel>,
    pub(crate) work_item_id: String,
    state: Entity<AppState>,
    probe: BoardProbe,
    focus: FocusHandle,
    scroll: ScrollHandle,
    loaded: Loaded,
    options: Option<BoardExecutionOptions>,
    load_task: Option<Task<()>>,
    options_task: Option<Task<()>>,
    /// A change arrived while a read was in flight: read again after it.
    dirty: bool,
    title_edit: Option<Edit>,
    brief_edit: Option<Edit>,
    settings_edit: Option<Edit>,
    /// The action in flight and its request id (reused after a lost answer).
    pending_action: Option<(DetailAction, String)>,
    action_task: Option<Task<()>>,
    pending_send: Option<String>,
    send_task: Option<Task<()>>,
    notice: Option<Notice>,
    preview: Option<Result<BoardResolvedSettings, String>>,
    preview_task: Option<Task<()>>,
    expanded_launch: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<BoardDetailEvent> for BoardDetail {}

impl BoardDetail {
    pub fn new(
        model: Entity<BoardModel>,
        state: Entity<AppState>,
        work_item_id: String,
        probe: BoardProbe,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&model, |this, _, event: &BoardModelEvent, cx| {
            let relevant = match event {
                BoardModelEvent::Changed => true,
                BoardModelEvent::Events(events) => events
                    .iter()
                    .any(|event| event.work_item_id == this.work_item_id),
            };
            if relevant {
                this.reload(cx);
            }
        });
        let mut detail = Self {
            model,
            work_item_id,
            state,
            probe,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            loaded: Loaded::Loading,
            options: None,
            load_task: None,
            options_task: None,
            dirty: false,
            title_edit: None,
            brief_edit: None,
            settings_edit: None,
            pending_action: None,
            action_task: None,
            pending_send: None,
            send_task: None,
            notice: None,
            preview: None,
            preview_task: None,
            expanded_launch: None,
            _subscriptions: vec![subscription],
        };
        detail.reload(cx);
        detail
    }

    fn api(&self, cx: &App) -> Option<BoardApi> {
        self.model.read(cx).api(cx)
    }

    fn ready(&self) -> Option<&BoardItemDetail> {
        match &self.loaded {
            Loaded::Ready(detail) => Some(detail),
            _ => None,
        }
    }

    fn chat_title(&self, task_id: &str, cx: &App) -> Option<String> {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == task_id)
            .and_then(|chat| chat.title.clone())
    }

    fn set_notice(&mut self, tone: NoticeTone, text: impl Into<String>) {
        self.notice = Some(Notice {
            tone,
            text: text.into(),
        });
    }

    // ---- loading ----

    /// Re-read the ticket (coalesced while a read is in flight).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.load_task.is_some() {
            self.dirty = true;
            return;
        }
        let Some(api) = self.api(cx) else {
            if self.ready().is_none() {
                self.loaded = Loaded::Error("Engine not connected.".into());
            }
            cx.notify();
            return;
        };
        let request = BoardReadRequest {
            work_item_id: self.work_item_id.clone(),
            project_id: None,
            history_limit: Some(HISTORY_LIMIT),
        };
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = api.read(request).await;
            this.update(cx, |detail, cx| detail.install(result, cx))
                .ok();
        }));
    }

    fn install(&mut self, result: Result<BoardItemDetail, BoardCallError>, cx: &mut Context<Self>) {
        self.load_task = None;
        match result {
            Ok(detail) => {
                // A key opens the ticket by id from here on.
                self.work_item_id = detail.view.item.id.clone();
                if self.options.is_none() && self.options_task.is_none() {
                    self.load_options(detail.view.item.project_id.clone(), cx);
                }
                self.loaded = Loaded::Ready(Box::new(detail));
            }
            Err(error) => {
                let message = match error.board() {
                    Some(BoardError::NotFound { .. }) => "This ticket no longer exists.".into(),
                    _ => error.message(),
                };
                if self.ready().is_some()
                    && !matches!(error.board(), Some(BoardError::NotFound { .. }))
                {
                    // Keep what is shown; say it may be stale.
                    self.set_notice(
                        NoticeTone::Warning,
                        format!("Couldn't refresh this ticket: {message}"),
                    );
                } else {
                    self.loaded = Loaded::Error(message);
                }
            }
        }
        cx.notify();
        if std::mem::take(&mut self.dirty) {
            self.reload(cx);
        }
    }

    fn load_options(&mut self, project_id: String, cx: &mut Context<Self>) {
        let Some(api) = self.api(cx) else {
            return;
        };
        let request = BoardExecutionOptionsRequest {
            project_id,
            harness: None,
            query: None,
            offset: 0,
            limit: None,
        };
        self.options_task = Some(cx.spawn(async move |this, cx| {
            // Labels only: without the catalog the drawer shows raw ids.
            let result = api.execution_options(request).await;
            this.update(cx, |detail, cx| {
                if let Ok(options) = result {
                    detail.options = Some(options);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Show a committed record at once, then re-read the derived view.
    fn apply_receipt(&mut self, receipt: &BoardMutationReceipt, cx: &mut Context<Self>) {
        if let Loaded::Ready(detail) = &mut self.loaded
            && detail.view.item.revision <= receipt.item.revision
        {
            detail.view.item = receipt.item.clone();
        }
        self.model
            .update(cx, |model, cx| model.apply_receipt(receipt, cx));
        self.reload(cx);
    }

    // ---- edits ----

    fn edit_mut(&mut self, target: EditTarget) -> &mut Option<Edit> {
        match target {
            EditTarget::Title => &mut self.title_edit,
            EditTarget::Brief => &mut self.brief_edit,
            EditTarget::Settings => &mut self.settings_edit,
        }
    }

    fn begin_text_edit(&mut self, target: EditTarget, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = self.ready() else {
            return;
        };
        let item = &detail.view.item;
        let (text, placeholder) = match target {
            EditTarget::Title => (item.title.clone(), "Ticket title"),
            _ => (
                item.description.clone(),
                "What should change, and how will we know it works?",
            ),
        };
        let base_revision = item.revision;
        let single = target == EditTarget::Title;
        let input = cx.new(|cx| {
            let mut input = ComposerInput::new(placeholder, cx);
            if single {
                input = input
                    .with_single_line()
                    .with_accessibility_role(gpui::Role::TextInput);
            } else {
                input = input.with_accessibility_role(gpui::Role::MultilineTextInput);
            }
            input.set_text(text, cx);
            input
        });
        let subscription =
            cx.subscribe_in(&input, window, move |this, input, event, window, cx| {
                if *event == ComposerInputEvent::Submitted {
                    if single {
                        this.save_edit(target, cx);
                    } else {
                        // The brief is multi-line: Enter inserts a line break.
                        input.update(cx, |input, cx| {
                            gpui::EntityInputHandler::replace_text_in_range(
                                input, None, "\n", window, cx,
                            )
                        });
                    }
                }
            });
        let focus = input.read(cx).focus_handle(cx);
        *self.edit_mut(target) = Some(Edit {
            base_revision,
            saving: false,
            error: None,
            conflict: None,
            request_id: None,
            input: Some(input),
            editor: None,
            _subscription: Some(subscription),
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    fn begin_settings_edit(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self.ready() else {
            return;
        };
        let item = &detail.view.item;
        let draft = ExecutionSettingsDraft {
            implementation: item.implementation.clone(),
            review: item.review.clone(),
        };
        let project = item.project_id.clone();
        let base_revision = item.revision;
        let model = self.model.clone();
        let state = self.state.clone();
        let probe = self.probe.clone();
        let editor = cx.new(|cx| {
            ExecutionSettingsEditor::new(model, state, project, draft, "detail", probe, cx)
        });
        self.settings_edit = Some(Edit {
            base_revision,
            saving: false,
            error: None,
            conflict: None,
            request_id: None,
            input: None,
            editor: Some(editor),
            _subscription: None,
        });
        cx.notify();
    }

    fn cancel_edit(&mut self, target: EditTarget, window: &mut Window, cx: &mut Context<Self>) {
        *self.edit_mut(target) = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The changes an edit would save, or a validation error.
    fn edit_changes(&self, target: EditTarget, cx: &App) -> Result<BoardItemChanges, String> {
        let edit = match target {
            EditTarget::Title => self.title_edit.as_ref(),
            EditTarget::Brief => self.brief_edit.as_ref(),
            EditTarget::Settings => self.settings_edit.as_ref(),
        }
        .ok_or_else(|| "Nothing to save.".to_owned())?;
        match target {
            EditTarget::Title => {
                let title = edit
                    .input
                    .as_ref()
                    .map(|input| input.read(cx).text().trim().to_owned())
                    .unwrap_or_default();
                if title.is_empty() {
                    return Err("A title is required.".into());
                }
                if title.chars().count() > BOARD_TITLE_MAX_CHARS {
                    return Err(format!(
                        "Keep the title under {BOARD_TITLE_MAX_CHARS} characters."
                    ));
                }
                Ok(BoardItemChanges {
                    title: Some(title),
                    ..Default::default()
                })
            }
            EditTarget::Brief => Ok(BoardItemChanges {
                description: Some(
                    edit.input
                        .as_ref()
                        .map(|input| input.read(cx).text().trim_end().to_owned())
                        .unwrap_or_default(),
                ),
                ..Default::default()
            }),
            EditTarget::Settings => {
                let draft = edit
                    .editor
                    .as_ref()
                    .map(|editor| editor.read(cx).draft())
                    .unwrap_or_default();
                Ok(BoardItemChanges {
                    implementation: Some(draft.implementation),
                    review: Some(draft.review),
                    ..Default::default()
                })
            }
        }
    }

    /// Save an edit against the revision it started from.
    fn save_edit(&mut self, target: EditTarget, cx: &mut Context<Self>) {
        let changes = self.edit_changes(target, cx);
        let Some(api) = self.api(cx) else {
            return;
        };
        let work_item_id = self.work_item_id.clone();
        let Some(edit) = self.edit_mut(target).as_mut() else {
            return;
        };
        if edit.saving {
            return;
        }
        let changes = match changes {
            Ok(changes) => changes,
            Err(error) => {
                edit.error = Some(error);
                cx.notify();
                return;
            }
        };
        edit.error = None;
        edit.saving = true;
        let request_id = edit
            .request_id
            .get_or_insert_with(|| new_request_id(target.key()))
            .clone();
        let request = luna_proto::BoardUpdateRequest {
            request_id: request_id.clone(),
            work_item_id,
            expected_revision: edit.base_revision,
            changes,
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match api.update(request).await {
                Err(BoardCallError::Lost) => reconcile(&api, request_id).await,
                other => other,
            };
            this.update(cx, |detail, cx| detail.finish_edit(target, result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_edit(
        &mut self,
        target: EditTarget,
        result: Result<BoardMutationReceipt, BoardCallError>,
        cx: &mut Context<Self>,
    ) {
        let active_run = self.ready().is_some_and(|detail| run_active(&detail.view));
        let has_chat = self
            .ready()
            .is_some_and(|detail| detail.view.working_chat.is_some());
        let sent = self.ready().and_then(last_sent_brief_revision);
        let Some(edit) = self.edit_mut(target).as_mut() else {
            return;
        };
        edit.saving = false;
        match result {
            Ok(receipt) => {
                *self.edit_mut(target) = None;
                let text = match target {
                    EditTarget::Title if has_chat => {
                        "Renamed. The working chat's title follows the ticket.".to_owned()
                    }
                    EditTarget::Title => "Renamed.".to_owned(),
                    EditTarget::Brief => match (active_run, sent) {
                        (true, Some(sent)) => format!(
                            "Brief saved as revision {}. The running chat still has revision \
                             {sent}; send the update when you are ready.",
                            receipt.item.brief_revision
                        ),
                        (false, Some(_)) => format!(
                            "Brief saved as revision {}. The next continuation includes it.",
                            receipt.item.brief_revision
                        ),
                        _ => format!(
                            "Brief saved as revision {}. The first start sends it.",
                            receipt.item.brief_revision
                        ),
                    },
                    EditTarget::Settings if active_run => {
                        "Settings saved. They apply to the next launch; the running chat keeps \
                         its settings."
                            .to_owned()
                    }
                    EditTarget::Settings => {
                        "Settings saved. They apply when the ticket next starts.".to_owned()
                    }
                };
                self.set_notice(NoticeTone::Success, text);
                self.apply_receipt(&receipt, cx);
            }
            Err(BoardCallError::Board(BoardError::StaleRevision { current, .. })) => {
                edit.request_id = None;
                edit.conflict = Some(current);
            }
            Err(BoardCallError::Lost) => {
                edit.error = Some(
                    "LUNA couldn't confirm the change and found no record of it. Save again; it \
                     is safe to retry."
                        .into(),
                );
            }
            Err(error) => {
                edit.request_id = None;
                edit.error = Some(error.message());
            }
        }
        cx.notify();
    }

    /// Conflict: save mine over the current revision (a deliberate new write).
    fn keep_mine(&mut self, target: EditTarget, cx: &mut Context<Self>) {
        if let Some(edit) = self.edit_mut(target).as_mut()
            && let Some(current) = edit.conflict.take()
        {
            edit.base_revision = current.item.revision;
            edit.request_id = None;
        }
        self.save_edit(target, cx);
    }

    /// Conflict: discard mine and continue from the current version.
    fn use_latest(&mut self, target: EditTarget, cx: &mut Context<Self>) {
        let Some(edit) = self.edit_mut(target).as_mut() else {
            return;
        };
        let Some(current) = edit.conflict.take() else {
            return;
        };
        edit.base_revision = current.item.revision;
        edit.request_id = None;
        let item = current.item.clone();
        match target {
            EditTarget::Title | EditTarget::Brief => {
                if let Some(input) = edit.input.clone() {
                    let text = if target == EditTarget::Title {
                        item.title.clone()
                    } else {
                        item.description.clone()
                    };
                    input.update(cx, |input, cx| input.set_text(text, cx));
                }
            }
            EditTarget::Settings => {
                if let Some(editor) = edit.editor.clone() {
                    editor.update(cx, |editor, cx| {
                        editor.set_draft(
                            ExecutionSettingsDraft {
                                implementation: item.implementation.clone(),
                                review: item.review.clone(),
                            },
                            cx,
                        )
                    });
                }
            }
        }
        if let Loaded::Ready(detail) = &mut self.loaded {
            detail.view = *current;
        }
        cx.notify();
    }

    // ---- actions ----

    fn permitted_actions(view: &WorkItemView) -> Vec<(DetailAction, ButtonKind)> {
        let has = |action: BoardAction| view.actions.contains(&action);
        let mut actions = Vec::new();
        if has(BoardAction::Start) {
            actions.push((DetailAction::Start, ButtonKind::Primary));
        } else if has(BoardAction::Continue) {
            actions.push((DetailAction::Continue, ButtonKind::Primary));
        } else if has(BoardAction::MoveToReady) {
            actions.push((DetailAction::MoveToReady, ButtonKind::Primary));
        }
        if has(BoardAction::MoveToBacklog) {
            actions.push((
                if has(BoardAction::Stop) {
                    DetailAction::Stop
                } else {
                    DetailAction::MoveToBacklog
                },
                ButtonKind::Secondary,
            ));
        }
        if has(BoardAction::Reopen) {
            actions.push((DetailAction::Reopen, ButtonKind::Secondary));
        }
        if has(BoardAction::Retry) {
            actions.push((
                DetailAction::Retry {
                    adopt_latest: false,
                },
                ButtonKind::Secondary,
            ));
            actions.push((
                DetailAction::Retry { adopt_latest: true },
                ButtonKind::Secondary,
            ));
        }
        if has(BoardAction::Review) {
            actions.push((DetailAction::Review, ButtonKind::Secondary));
        }
        actions
    }

    /// Run one action through the same engine operation as drag, menus and
    /// agents. A lost answer is reconciled by its request id.
    fn run_action(&mut self, action: DetailAction, cx: &mut Context<Self>) {
        if self.action_task.is_some() {
            return;
        }
        let Some(api) = self.api(cx) else {
            self.set_notice(NoticeTone::Error, "Engine not connected.");
            cx.notify();
            return;
        };
        let Some(detail) = self.ready() else {
            return;
        };
        let revision = detail.view.item.revision;
        let work_item_id = detail.view.item.id.clone();
        let request_id = match &self.pending_action {
            Some((pending, id)) if *pending == action => id.clone(),
            _ => new_request_id(action.key()),
        };
        self.pending_action = Some((action, request_id.clone()));
        self.set_notice(NoticeTone::Info, format!("{}…", action.label()));
        cx.notify();
        let move_to = |to: BoardColumn| BoardMoveRequest {
            request_id: request_id.clone(),
            work_item_id: work_item_id.clone(),
            to,
            expected_revision: None,
            after_id: None,
            before_id: None,
            destination: BoardDestination::Auto,
            context: BoardContextMode::Fresh,
            execution: None,
            follow_up: BoardFollowUp::None,
        };
        enum Call {
            Move(BoardMoveRequest),
            Control(BoardControlRequest),
            Review(BoardReviewStartRequest),
            Archive(BoardArchiveRequest),
        }
        let call = match action {
            DetailAction::Start | DetailAction::Continue | DetailAction::MoveToReady => {
                Call::Move(move_to(BoardColumn::Ready))
            }
            DetailAction::MoveToBacklog | DetailAction::Stop | DetailAction::Reopen => {
                Call::Move(move_to(BoardColumn::Backlog))
            }
            DetailAction::Retry { adopt_latest } => Call::Control(BoardControlRequest {
                request_id: request_id.clone(),
                work_item_id: work_item_id.clone(),
                action: BoardControlAction::Retry,
                expected_revision: None,
                adopt_latest_brief: adopt_latest,
            }),
            DetailAction::Review => Call::Review(BoardReviewStartRequest {
                request_id: request_id.clone(),
                work_item_id: work_item_id.clone(),
                execution_generation: None,
                profile: None,
                // A user-requested review: the engine captures the current
                // result itself (A5's implementer reports stay empty).
                checks: Vec::new(),
                summary: None,
                evidence: Vec::new(),
            }),
            DetailAction::Archive | DetailAction::Unarchive => Call::Archive(BoardArchiveRequest {
                request_id: request_id.clone(),
                work_item_id: work_item_id.clone(),
                archived: action == DetailAction::Archive,
                expected_revision: Some(revision),
            }),
        };
        self.action_task = Some(cx.spawn(async move |this, cx| {
            // (receipt to show, effect text)
            let result: Result<(Option<BoardMutationReceipt>, String), BoardCallError> = match call
            {
                Call::Move(request) => api.move_item(request).await.map(|receipt| {
                    let text = effect_text(&receipt.effect, receipt.mutation.column);
                    (Some(receipt.mutation), text)
                }),
                Call::Control(request) => api.control(request).await.map(|receipt| {
                    let text = effect_text(&receipt.effect, receipt.mutation.column);
                    (Some(receipt.mutation), text)
                }),
                Call::Review(request) => api.review_start(request).await.map(|receipt| {
                    (
                        None,
                        format!(
                            "Review started (round {}) in a temporary reviewer side chat.",
                            receipt.run.round
                        ),
                    )
                }),
                Call::Archive(request) => {
                    let archived = request.archived;
                    api.archive(request).await.map(|receipt| {
                        let text = if archived {
                            "Archived. Work, links and history are kept; it never stops work or \
                             touches a GitHub issue."
                        } else {
                            "Unarchived; the card is back in its column."
                        };
                        (Some(receipt), text.to_owned())
                    })
                }
            };
            let result = match result {
                Err(BoardCallError::Lost) => {
                    reconcile(&api, request_id.clone()).await.map(|receipt| {
                        (
                            Some(receipt),
                            "Applied (confirmed by its receipt after a lost answer).".to_owned(),
                        )
                    })
                }
                other => other,
            };
            this.update(cx, |detail, cx| detail.finish_action(action, result, cx))
                .ok();
        }));
    }

    fn finish_action(
        &mut self,
        action: DetailAction,
        result: Result<(Option<BoardMutationReceipt>, String), BoardCallError>,
        cx: &mut Context<Self>,
    ) {
        self.action_task = None;
        match result {
            Ok((receipt, text)) => {
                self.pending_action = None;
                self.set_notice(NoticeTone::Success, text);
                match receipt {
                    Some(receipt) => self.apply_receipt(&receipt, cx),
                    None => self.reload(cx),
                }
            }
            Err(BoardCallError::Lost) => {
                // Keep the request id: pressing again reconciles, never repeats.
                self.set_notice(
                    NoticeTone::Warning,
                    format!(
                        "LUNA couldn't confirm “{}” and found no record of it. Press it again; \
                         it is safe to retry.",
                        action.label()
                    ),
                );
            }
            Err(error) => {
                self.pending_action = None;
                self.set_notice(
                    NoticeTone::Error,
                    format!("{} failed: {}", action.label(), error.message()),
                );
                self.reload(cx);
            }
        }
        cx.notify();
    }

    /// "Send update to chat": the brief delta through the ordinary
    /// queue/steer path. The original message is never edited.
    fn send_update(&mut self, mode: CrossTaskSendMode, cx: &mut Context<Self>) {
        if self.send_task.is_some() {
            return;
        }
        let Some(api) = self.api(cx) else {
            return;
        };
        let Some((work_item_id, revision)) = self
            .ready()
            .map(|detail| (detail.view.item.id.clone(), detail.view.item.revision))
        else {
            return;
        };
        let request_id = self
            .pending_send
            .get_or_insert_with(|| new_request_id("send-update"))
            .clone();
        let request = BoardSendUpdateRequest {
            request_id: request_id.clone(),
            work_item_id,
            expected_revision: revision,
            mode,
        };
        self.set_notice(NoticeTone::Info, "Sending the brief update…");
        cx.notify();
        self.send_task = Some(cx.spawn(async move |this, cx| {
            let result = match api.send_update(request).await {
                Ok(receipt) => Ok((
                    receipt.mutation.clone(),
                    format!(
                        "Update sent to the working chat: brief revision {} → {} ({}).",
                        receipt.from_brief_revision,
                        receipt.to_brief_revision,
                        match mode {
                            CrossTaskSendMode::Steer => "steering the current turn",
                            CrossTaskSendMode::Auto => "steered or queued",
                            CrossTaskSendMode::Queue => "queued for its next turn",
                        }
                    ),
                )),
                Err(BoardCallError::Lost) => reconcile(&api, request_id).await.map(|receipt| {
                    (
                        receipt,
                        "Update sent (confirmed by its receipt after a lost answer).".to_owned(),
                    )
                }),
                Err(error) => Err(error),
            };
            this.update(cx, |detail, cx| {
                detail.send_task = None;
                match result {
                    Ok((receipt, text)) => {
                        detail.pending_send = None;
                        detail.set_notice(NoticeTone::Success, text);
                        detail.apply_receipt(&receipt, cx);
                    }
                    Err(BoardCallError::Lost) => detail.set_notice(
                        NoticeTone::Warning,
                        "LUNA couldn't confirm the update. Send again; it is safe to retry.",
                    ),
                    Err(error) => {
                        detail.pending_send = None;
                        detail.set_notice(
                            NoticeTone::Error,
                            format!("The update was not sent: {}", error.message()),
                        );
                        detail.reload(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn preview_settings(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api(cx) else {
            return;
        };
        let request = BoardResolveSettingsRequest {
            work_item_id: self.work_item_id.clone(),
            destination: BoardDestination::Auto,
            execution: None,
            caller_task_id: None,
        };
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            let result = api
                .resolve_settings(request)
                .await
                .map_err(|error| error.message());
            this.update(cx, |detail, cx| {
                detail.preview_task = None;
                detail.preview = Some(result);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ---- navigation ----

    fn navigate(
        &mut self,
        request: BoardNavigationRequest,
        fallback: BoardNavigationTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(api) = self.api(cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let target = match api.navigation(request).await {
                Ok(target) => target,
                // Older engines: the engine-derived working chat summary.
                Err(_) => fallback,
            };
            this.update(cx, |detail, cx| match &target {
                BoardNavigationTarget::NoChat { .. } => {
                    detail.set_notice(
                        NoticeTone::Info,
                        "This ticket has no working chat yet. Start it to create one.",
                    );
                    cx.notify();
                }
                _ => cx.emit(BoardDetailEvent::GoToChat(target.clone())),
            })
            .ok();
        })
        .detach();
    }

    fn go_to_chat(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self.ready() else {
            return;
        };
        let fallback = match &detail.view.working_chat {
            None => BoardNavigationTarget::NoChat {
                work_item_id: detail.view.item.id.clone(),
            },
            Some(chat) => match (&chat.role, &chat.main_parent_id) {
                (BoardChatRole::Side, Some(main)) if chat.parent_available => {
                    BoardNavigationTarget::SideChat {
                        main_task_id: main.clone(),
                        side_task_id: chat.task_id.clone(),
                    }
                }
                (BoardChatRole::Side, main) => BoardNavigationTarget::Direct {
                    task_id: chat.task_id.clone(),
                    original_parent_id: main.clone(),
                },
                (BoardChatRole::Main, _) => BoardNavigationTarget::MainChat {
                    task_id: chat.task_id.clone(),
                },
            },
        };
        let request = BoardNavigationRequest {
            work_item_id: Some(detail.view.item.id.clone()),
            task_id: None,
        };
        self.navigate(request, fallback, cx);
    }

    /// Review details: the reviewer chat (archived ones open read-only).
    pub(crate) fn open_review_details(&mut self, reviewer_task_id: String, cx: &mut Context<Self>) {
        let fallback = BoardNavigationTarget::Direct {
            task_id: reviewer_task_id.clone(),
            original_parent_id: None,
        };
        let request = BoardNavigationRequest {
            work_item_id: None,
            task_id: Some(reviewer_task_id),
        };
        self.navigate(request, fallback, cx);
    }

    pub(crate) fn toggle_launch(&mut self, launch_id: String, cx: &mut Context<Self>) {
        if self.expanded_launch.as_deref() == Some(launch_id.as_str()) {
            self.expanded_launch = None;
        } else {
            self.expanded_launch = Some(launch_id);
        }
        cx.notify();
    }

    // ---- rendering ----

    fn button(
        &self,
        id: impl Into<String>,
        label: impl Into<SharedString>,
        kind: ButtonKind,
        enabled: bool,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let id = id.into();
        let label = label.into();
        button::button(id.clone(), kind, ControlSize::Sm, enabled, theme)
            .aria_label(label.clone())
            .child(label)
            .children(self.probe.bounds_el(id))
    }

    fn text(&self, key: impl Into<String>, copy: impl Into<String>) -> SharedString {
        let copy = copy.into();
        self.probe.text(key, copy.clone());
        copy.into()
    }

    fn render_header(
        &self,
        detail: &BoardItemDetail,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = &detail.view;
        let item = &view.item;
        let status = self.text(
            "detail-status",
            format!(
                "{} · {} · {}{}",
                item.key,
                item.column.label(),
                view.phase.label(),
                if item.archived { " · Archived" } else { "" }
            ),
        );
        let mut column = div().w_full().flex().flex_col().gap(ui_rems(6.0)).child(
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(ui_rems(8.0))
                .child(
                    div()
                        .min_w_0()
                        .text_size(ui_rems(12.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.readable_muted(0.9))
                        .child(status),
                )
                .child(
                    button::icon_button(
                        "detail-close",
                        crate::icons::CLOSE,
                        ControlSize::Md,
                        true,
                        theme,
                    )
                    .aria_label("Close ticket details")
                    .children(self.probe.bounds_el("detail-close"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(BoardDetailEvent::Close))),
                ),
        );
        match &self.title_edit {
            Some(edit) => {
                let input = edit.input.clone();
                let mut editor = div()
                    .id("detail-title-editor")
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(ui_rems(6.0))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            this.cancel_edit(EditTarget::Title, window, cx);
                            cx.stop_propagation();
                        }
                    }));
                if let Some(input) = input {
                    editor = editor.child(
                        crate::popover::dialog_field(input.into_any_element())
                            .id("detail-title-input")
                            .relative()
                            .children(self.probe.bounds_el("detail-title-input")),
                    );
                }
                editor = editor
                    .children(self.conflict_banner(EditTarget::Title, edit, theme, cx))
                    .children(edit.error.as_ref().map(|error| {
                        self.probe.text("detail-title-error", error.clone());
                        widgets::error_strip(theme, error.clone()).mt_0()
                    }))
                    .child(
                        widgets::row_block()
                            .child(
                                self.button(
                                    "detail-title-save",
                                    if edit.saving {
                                        "Saving…"
                                    } else {
                                        "Save title"
                                    },
                                    ButtonKind::Primary,
                                    !edit.saving,
                                    theme,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| this.save_edit(EditTarget::Title, cx),
                                )),
                            )
                            .child(
                                self.button(
                                    "detail-title-cancel",
                                    "Cancel",
                                    ButtonKind::Ghost,
                                    true,
                                    theme,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.cancel_edit(EditTarget::Title, window, cx)
                                    },
                                )),
                            )
                            .child(widgets::row_description(
                                theme,
                                if view.working_chat.is_some() {
                                    "Renaming the ticket renames its working chat too."
                                } else {
                                    "Enter saves; Escape cancels."
                                },
                            )),
                    );
                column = column.child(editor);
            }
            None => {
                let title = self.text("detail-title", item.title.clone());
                column = column.child(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(ui_rems(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(crate::typography::TextStyle::Title2.size())
                                .line_height(crate::typography::TextStyle::Title2.line_height())
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(title),
                        )
                        .when(view.actions.contains(&BoardAction::Edit), |row| {
                            row.child(
                                self.button(
                                    "detail-rename",
                                    "Rename",
                                    ButtonKind::Ghost,
                                    true,
                                    theme,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.begin_text_edit(EditTarget::Title, window, cx)
                                    },
                                )),
                            )
                        }),
                );
            }
        }
        column.into_any_element()
    }

    fn conflict_banner(
        &self,
        target: EditTarget,
        edit: &Edit,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let current = edit.conflict.as_ref()?;
        let what = match target {
            EditTarget::Title => current.item.title.clone(),
            EditTarget::Brief => current
                .item
                .description
                .chars()
                .take(CONFLICT_PREVIEW)
                .collect(),
            EditTarget::Settings => format!(
                "Implementation: {} · Review: {}",
                describe_implementation(&current.item.implementation, self.options.as_ref()),
                describe_review(&current.item.review, self.options.as_ref())
            ),
        };
        let message = self.text(
            format!("detail-conflict-{}", target.key()),
            format!(
                "{} changed since you started editing (you started from revision {}; it is now \
                 revision {}). Your edit is kept. Choose which version to keep.",
                current.item.key, edit.base_revision, current.item.revision
            ),
        );
        let latest = self.text(format!("detail-conflict-{}-latest", target.key()), what);
        Some(
            div()
                .id(SharedString::from(format!(
                    "detail-conflict-{}",
                    target.key()
                )))
                .w_full()
                .flex()
                .flex_col()
                .gap(ui_rems(6.0))
                .child(widgets::warning_strip(theme, message).mt_0())
                .child(
                    div()
                        .w_full()
                        .px(ui_rems(10.0))
                        .py(ui_rems(8.0))
                        .rounded(px(8.0))
                        .bg(theme.fill(FillTier::Quaternary))
                        .flex()
                        .flex_col()
                        .gap(ui_rems(2.0))
                        .child(widgets::row_title(theme, "Current version"))
                        .child(widgets::row_description(theme, latest)),
                )
                .child(
                    widgets::row_block()
                        .child(
                            self.button(
                                format!("detail-conflict-keep-{}", target.key()),
                                "Keep mine (save over the current version)",
                                ButtonKind::Primary,
                                !edit.saving,
                                theme,
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.keep_mine(target, cx)),
                            ),
                        )
                        .child(
                            self.button(
                                format!("detail-conflict-latest-{}", target.key()),
                                "Use the current version",
                                ButtonKind::Secondary,
                                !edit.saving,
                                theme,
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.use_latest(target, cx)),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_notice(&self, theme: &Theme) -> Option<AnyElement> {
        let notice = self.notice.as_ref()?;
        self.probe.text("detail-notice", notice.text.clone());
        self.probe.text("detail-notice-tone", notice.tone.key());
        Some(match notice.tone {
            NoticeTone::Error => widgets::error_strip(theme, notice.text.clone())
                .mt_0()
                .into_any_element(),
            NoticeTone::Warning => widgets::warning_strip(theme, notice.text.clone())
                .mt_0()
                .into_any_element(),
            NoticeTone::Info | NoticeTone::Success => div()
                .w_full()
                .px(ui_rems(12.0))
                .py(ui_rems(8.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.fill(FillTier::Quaternary))
                .flex()
                .flex_row()
                .items_start()
                .gap(ui_rems(8.0))
                .child(
                    crate::icons::icon(if notice.tone == NoticeTone::Success {
                        crate::icons::CHECK
                    } else {
                        crate::icons::INFO_CIRCLE
                    })
                    .size(ui_rems(14.0))
                    .flex_none()
                    .text_color(theme.readable_muted(0.9)),
                )
                .child(widgets::row_description(theme, notice.text.clone()).text_color(theme.text))
                .into_any_element(),
        })
    }

    fn render_attention(
        &self,
        detail: &BoardItemDetail,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        for (index, reason) in detail.view.attention.iter().enumerate() {
            let copy = self.text(
                format!("detail-attention-{index}"),
                format!(
                    "Needs attention · {}: {}",
                    attention_label(reason.kind),
                    reason.detail
                ),
            );
            let mut row = div()
                .w_full()
                .flex()
                .flex_col()
                .gap(ui_rems(6.0))
                .child(widgets::warning_strip(theme, copy).mt_0());
            if reason.task_id.is_some() || reason.attention_id.is_some() {
                let attention_id = reason.attention_id.clone();
                let task_id = reason.task_id.clone();
                row = row.child(
                    widgets::row_block().child(
                        self.button(
                            format!("detail-attention-open-{index}"),
                            "Open where it is waiting",
                            ButtonKind::Secondary,
                            true,
                            theme,
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(BoardDetailEvent::OpenAttention {
                                attention_id: attention_id.clone(),
                                task_id: task_id.clone(),
                            })
                        })),
                    ),
                );
            }
            rows.push(row.into_any_element());
        }
        if let Some(reason) = &detail.view.queue_reason {
            let copy = self.text(
                "detail-queue",
                format!("Waiting: {}.", admission_text(reason)),
            );
            rows.push(widgets::row_description(theme, copy).into_any_element());
        }
        rows
    }

    fn render_actions(
        &self,
        detail: &BoardItemDetail,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.action_task.is_some();
        let actions = Self::permitted_actions(&detail.view);
        self.probe.text(
            "detail-actions",
            actions
                .iter()
                .map(|(action, _)| action.key())
                .collect::<Vec<_>>()
                .join(","),
        );
        let mut row = widgets::row_block();
        for (action, kind) in actions {
            let mut button = self.button(
                format!("detail-action-{}", action.key()),
                action.label(),
                kind,
                !busy,
                theme,
            );
            if action == DetailAction::Start || action == DetailAction::Continue {
                button = button.child(
                    button::glyph(crate::icons::PLAY, ControlSize::Sm).text_color(theme.on_solid),
                );
            }
            if !busy {
                button =
                    button.on_click(cx.listener(move |this, _, _, cx| this.run_action(action, cx)));
            }
            row = row.child(button);
        }
        let column = detail.view.item.column;
        let hint = match column {
            BoardColumn::Backlog if detail.view.working_chat.is_some() => {
                "Continue moves it to Ready and resumes the same chat; the brief is not sent again."
            }
            BoardColumn::Backlog => {
                "Start moves it to Ready: the first start creates its chat and sends the brief once."
            }
            BoardColumn::Ready => "Ready runs it. Moving it to Backlog uses the chat's Stop.",
            BoardColumn::Done => "Done is set by LUNA when the checks and review pass.",
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(6.0))
            .child(row)
            .child(widgets::row_description(theme, hint))
            .into_any_element()
    }

    fn render_working_chat(
        &self,
        detail: &BoardItemDetail,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = &detail.view;
        let mut card = evidence::section(theme, "detail-working-chat", "Working chat", None);
        match &view.working_chat {
            None => {
                let copy = self.text(
                    "detail-working-chat-location",
                    "No working chat yet. A first start from the Board creates a new main chat.",
                );
                card = card.child(
                    widgets::card_row(theme, false)
                        .child(widgets::row_tile(theme, crate::icons::CHAT_LINE))
                        .child(widgets::row_text(theme, "Not started", Some(copy))),
                );
            }
            Some(chat) => {
                let title = self
                    .chat_title(&chat.task_id, cx)
                    .unwrap_or_else(|| detail.view.item.title.clone());
                let parent_title = chat
                    .main_parent_title
                    .clone()
                    .or_else(|| {
                        chat.main_parent_id
                            .as_ref()
                            .and_then(|id| self.chat_title(id, cx))
                    })
                    .unwrap_or_else(|| "its main chat".into());
                let location = match chat.role {
                    BoardChatRole::Main => "Main chat".to_owned(),
                    BoardChatRole::Side if chat.parent_available => {
                        format!("Side chat in {parent_title}")
                    }
                    BoardChatRole::Side => format!(
                        "Side chat in {parent_title} (that chat is unavailable, so this opens \
                         directly; its parent never changes)"
                    ),
                };
                let location = self.text("detail-working-chat-location", location);
                let observed = match (chat.harness, &chat.model) {
                    (Some(harness), Some(model)) => format!(
                        "Running with {} · {}",
                        harness_label(harness),
                        model_label(self.options.as_ref(), harness, model)
                    ),
                    (Some(harness), None) => format!("Running with {}", harness_label(harness)),
                    _ => "Provider and model not observed yet".into(),
                };
                let mut row = widgets::card_row(theme, false)
                    .child(widgets::row_tile(
                        theme,
                        if chat.role == BoardChatRole::Side {
                            crate::icons::FORK
                        } else {
                            crate::icons::CHAT_LINE
                        },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(ui_rems(widgets::ROW_TEXT_MIN))
                            .flex()
                            .flex_col()
                            .gap(ui_rems(2.0))
                            .child(widgets::row_title(theme, title))
                            .child(widgets::row_description(theme, location))
                            .child(widgets::row_description(theme, observed)),
                    );
                if view.actions.contains(&BoardAction::GoToChat) {
                    row = row.child(
                        widgets::row_controls().child(
                            self.button(
                                "detail-go-to-chat",
                                "Go to chat",
                                ButtonKind::Secondary,
                                true,
                                theme,
                            )
                            .child(button::glyph(crate::icons::ARROW_UP_RIGHT, ControlSize::Sm))
                            .on_click(cx.listener(|this, _, _, cx| this.go_to_chat(cx))),
                        ),
                    );
                }
                card = card.child(row);
            }
        }
        card.into_any_element()
    }

    fn render_brief(
        &self,
        detail: &BoardItemDetail,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = &detail.view;
        let item = &view.item;
        let sent = last_sent_brief_revision(detail);
        let active = run_active(view);
        let revision = self.text(
            "detail-brief-revision",
            format!(
                "Brief revision {}{}",
                item.brief_revision,
                match sent {
                    Some(sent) if sent == item.brief_revision => {
                        " · the working chat has this revision".to_owned()
                    }
                    Some(sent) => format!(" · the working chat has revision {sent}"),
                    None => " · not sent yet".to_owned(),
                }
            ),
        );
        let mut card = evidence::section(theme, "detail-brief", "Brief", Some(revision));
        match &self.brief_edit {
            Some(edit) => {
                let note = match (active, sent) {
                    (true, Some(sent)) => format!(
                        "A run is active. Saving changes the ticket's brief for future work; the \
                         running chat keeps revision {sent} until you send the update."
                    ),
                    (false, Some(_)) => {
                        "Saved changes reach the chat with the next continuation or an update you \
                         send."
                            .to_owned()
                    }
                    _ => "Saved changes are what the first start sends.".to_owned(),
                };
                let note = self.text("detail-brief-note", note);
                let mut editor = div()
                    .id("detail-brief-editor")
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(ui_rems(8.0))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            this.cancel_edit(EditTarget::Brief, window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(widgets::row_description(theme, note));
                if let Some(input) = edit.input.clone() {
                    let focus = input.read(cx).focus_handle(cx);
                    editor = editor.child(
                        crate::popover::dialog_field(input.into_any_element())
                            .id("detail-brief-input")
                            .relative()
                            .cursor_text()
                            .min_h(ui_rems(160.0))
                            .text_size(ui_rems(13.0))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |_, _, window, cx| window.focus(&focus, cx)),
                            )
                            .children(self.probe.bounds_el("detail-brief-input")),
                    );
                }
                editor = editor
                    .children(self.conflict_banner(EditTarget::Brief, edit, theme, cx))
                    .children(edit.error.as_ref().map(|error| {
                        self.probe.text("detail-brief-error", error.clone());
                        widgets::error_strip(theme, error.clone()).mt_0()
                    }))
                    .child(
                        widgets::row_block()
                            .child(
                                self.button(
                                    "detail-brief-save",
                                    if edit.saving {
                                        "Saving…"
                                    } else {
                                        "Save brief"
                                    },
                                    ButtonKind::Primary,
                                    !edit.saving && edit.conflict.is_none(),
                                    theme,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| this.save_edit(EditTarget::Brief, cx),
                                )),
                            )
                            .child(
                                self.button(
                                    "detail-brief-cancel",
                                    "Cancel",
                                    ButtonKind::Ghost,
                                    true,
                                    theme,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.cancel_edit(EditTarget::Brief, window, cx)
                                    },
                                )),
                            ),
                    );
                card = card.child(widgets::card_row(theme, false).child(editor));
            }
            None => {
                let body = if item.description.trim().is_empty() {
                    "No description yet.".to_owned()
                } else {
                    item.description.clone()
                };
                let body = self.text("detail-brief-text", body);
                card = card.child(
                    widgets::card_row(theme, false).child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_size(ui_rems(13.0))
                            .line_height(ui_rems(19.0))
                            .text_color(theme.text)
                            .child(body),
                    ),
                );
                let mut controls = widgets::row_block();
                if view.actions.contains(&BoardAction::Edit) {
                    controls = controls.child(
                        self.button(
                            "detail-edit-brief",
                            "Edit brief",
                            ButtonKind::Secondary,
                            true,
                            theme,
                        )
                        .child(button::glyph(crate::icons::PEN, ControlSize::Sm))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.begin_text_edit(EditTarget::Brief, window, cx)
                        })),
                    );
                }
                let offered = send_update_offered(detail);
                self.probe
                    .text("detail-send-update-offered", offered.to_string());
                if offered {
                    let sending = self.send_task.is_some();
                    controls = controls
                        .child(
                            self.button(
                                "detail-send-update",
                                "Send update to chat",
                                ButtonKind::Primary,
                                !sending,
                                theme,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.send_update(CrossTaskSendMode::Queue, cx)
                            })),
                        )
                        .child(
                            self.button(
                                "detail-send-update-steer",
                                "Steer now",
                                ButtonKind::Secondary,
                                !sending,
                                theme,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.send_update(CrossTaskSendMode::Steer, cx)
                            })),
                        );
                }
                let hint = if offered {
                    Some(format!(
                        "Brief revision {} is not in the running chat yet. Sending adds a message; \
                         the original instruction is never edited.",
                        item.brief_revision
                    ))
                } else if !active && sent.is_some_and(|sent| item.brief_revision > sent) {
                    Some("The next continuation includes the latest brief.".to_owned())
                } else {
                    None
                };
                card = card.child(
                    widgets::card_row(theme, false).child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(ui_rems(6.0))
                            .child(controls)
                            .children(hint.map(|hint| {
                                let hint = self.text("detail-brief-hint", hint);
                                widgets::row_description(theme, hint)
                            })),
                    ),
                );
            }
        }
        card.into_any_element()
    }

    fn render_settings(
        &self,
        detail: &BoardItemDetail,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = &detail.view;
        let item = &view.item;
        let options = self.options.as_ref();
        let mut card = evidence::section(
            theme,
            "detail-settings",
            "Execution settings",
            Some("Requested on the ticket, resolved for dispatch, and observed running.".into()),
        );
        if let Some(edit) = &self.settings_edit {
            if let Some(editor) = edit.editor.clone() {
                card =
                    card.child(widgets::card_row(theme, false).child(div().w_full().child(editor)));
            }
            let note = if run_active(view) {
                "Changes apply to the next launch; the running chat keeps its settings."
            } else {
                "Changes apply when the ticket next starts."
            };
            let mut block = div()
                .w_full()
                .flex()
                .flex_col()
                .gap(ui_rems(6.0))
                .children(self.conflict_banner(EditTarget::Settings, edit, theme, cx))
                .children(edit.error.as_ref().map(|error| {
                    self.probe.text("detail-settings-error", error.clone());
                    widgets::error_strip(theme, error.clone()).mt_0()
                }))
                .child(widgets::row_description(theme, note));
            block = block.child(
                widgets::row_block()
                    .child(
                        self.button(
                            "detail-settings-save",
                            if edit.saving {
                                "Saving…"
                            } else {
                                "Save settings"
                            },
                            ButtonKind::Primary,
                            !edit.saving && edit.conflict.is_none(),
                            theme,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.save_edit(EditTarget::Settings, cx)),
                        ),
                    )
                    .child(
                        self.button(
                            "detail-settings-cancel",
                            "Cancel",
                            ButtonKind::Ghost,
                            true,
                            theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.cancel_edit(EditTarget::Settings, window, cx)
                        })),
                    ),
            );
            return card
                .child(widgets::card_row(theme, false).child(block))
                .into_any_element();
        }
        // Requested.
        let requested = self.text(
            "detail-settings-requested",
            describe_implementation(&item.implementation, options),
        );
        let requested_review = self.text(
            "detail-settings-requested-review",
            describe_review(&item.review, options),
        );
        card = card.child(
            widgets::card_row(theme, false)
                .child(
                    div()
                        .flex_1()
                        .min_w(ui_rems(widgets::ROW_TEXT_MIN))
                        .flex()
                        .flex_col()
                        .gap(ui_rems(2.0))
                        .child(widgets::row_title(theme, "Requested"))
                        .child(widgets::row_description(
                            theme,
                            format!("Implementation: {requested}"),
                        ))
                        .child(widgets::row_description(
                            theme,
                            format!("Review: {requested_review}"),
                        )),
                )
                .when(view.actions.contains(&BoardAction::Edit), |row| {
                    row.child(
                        widgets::row_controls().child(
                            self.button(
                                "detail-settings-edit",
                                "Change",
                                ButtonKind::Ghost,
                                true,
                                theme,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.begin_settings_edit(cx))),
                        ),
                    )
                }),
        );
        // Resolved for dispatch (frozen in the newest launch snapshot).
        let latest = detail
            .launches
            .iter()
            .max_by_key(|launch| launch.created_at_ms);
        let resolved = match latest {
            Some(launch) => {
                let sources: Vec<String> = launch
                    .resolved
                    .sources
                    .iter()
                    .map(|source| {
                        format!(
                            "{} from {}",
                            source.field,
                            super::settings_controls::origin_label(source.source)
                        )
                    })
                    .collect();
                format!(
                    "{} · frozen at the {} on {}{}",
                    describe_resolved(&launch.resolved, options, true),
                    evidence::instruction_kind_label(launch.kind).to_lowercase(),
                    format_time(launch.created_at_ms),
                    if sources.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", sources.join(", "))
                    }
                )
            }
            None => {
                "Not resolved yet: Start resolves the settings and freezes them for the launch."
                    .into()
            }
        };
        let resolved = self.text("detail-settings-resolved", resolved);
        let mut resolved_row = widgets::card_row(theme, false).child(
            div()
                .flex_1()
                .min_w(ui_rems(widgets::ROW_TEXT_MIN))
                .flex()
                .flex_col()
                .gap(ui_rems(2.0))
                .child(widgets::row_title(theme, "Resolved for dispatch"))
                .child(widgets::row_description(theme, resolved)),
        );
        if latest.is_none() {
            resolved_row = resolved_row.child(
                widgets::row_controls().child(
                    self.button(
                        "detail-settings-preview",
                        if self.preview_task.is_some() {
                            "Checking…"
                        } else {
                            "Preview what Start would use"
                        },
                        ButtonKind::Ghost,
                        self.preview_task.is_none(),
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.preview_settings(cx))),
                ),
            );
        }
        card = card.child(resolved_row);
        if latest.is_none()
            && let Some(preview) = &self.preview
        {
            let copy = match preview {
                Ok(preview) => {
                    let mut copy = format!(
                        "Start would use {}; review: {}.",
                        describe_resolved(&preview.implementation, options, true),
                        if preview.review_fallback_to_implementation {
                            "the implementation model in a separate reviewer chat".to_owned()
                        } else {
                            describe_resolved(&preview.review, options, false)
                        }
                    );
                    if !preview.unresolved.is_empty() {
                        copy.push_str(&format!(
                            " Still to resolve: {}.",
                            preview.unresolved.join(", ")
                        ));
                    }
                    copy
                }
                Err(error) => format!("Preview unavailable: {error}"),
            };
            let copy = self.text("detail-settings-preview-result", copy);
            card = card.child(
                widgets::card_row(theme, false)
                    .child(div().w_full().child(widgets::row_description(theme, copy))),
            );
        }
        // Observed running.
        let observed = match (&view.working_chat, view.phase) {
            (_, BoardPhase::Unknown) => {
                "Unknown: the host or its runtime can't be observed right now.".to_owned()
            }
            (Some(chat), _) => match (chat.harness, &chat.model) {
                (Some(harness), Some(model)) => format!(
                    "{} · {} (reported by the working chat; reasoning and access are not reported)",
                    harness_label(harness),
                    model_label(options, harness, model)
                ),
                _ => "Not observed yet: the working chat has not reported its model.".to_owned(),
            },
            (None, _) => "Not observed: nothing has run yet.".to_owned(),
        };
        let observed = self.text("detail-settings-observed", observed);
        card = card.child(
            widgets::card_row(theme, false).child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(ui_rems(2.0))
                    .child(widgets::row_title(theme, "Observed running"))
                    .child(widgets::row_description(theme, observed)),
            ),
        );
        card.into_any_element()
    }

    fn render_issues(
        &self,
        detail: &BoardItemDetail,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if detail.issues.is_empty() {
            return None;
        }
        let mut card = evidence::section(
            theme,
            "detail-issues",
            "Source issue",
            Some(
                "GitHub state is separate from the Board column. A local rename never changes the \
                 issue, and nothing is written back without your explicit confirmation."
                    .into(),
            ),
        );
        for (index, issue) in detail.issues.iter().enumerate() {
            let summary = issue.summary();
            let heading = self.text(
                format!("detail-issue-{index}"),
                format!(
                    "{} · {}{}",
                    summary.display,
                    match issue.role {
                        IssueLinkRole::Source => "source",
                        IssueLinkRole::Related => "related",
                    },
                    if issue.active { "" } else { " · unlinked" }
                ),
            );
            let meta = self.text(
                format!("detail-issue-{index}-meta"),
                format!(
                    "{}GitHub state: {} · fetched {}",
                    issue
                        .remote_title
                        .as_ref()
                        .map(|title| format!("“{title}” · "))
                        .unwrap_or_default(),
                    issue.remote_state.as_deref().unwrap_or("unknown"),
                    format_time(issue.fetched_at_ms)
                ),
            );
            let url = issue.canonical_url.clone();
            let mut controls = widgets::row_controls().child(
                button::button(
                    format!("detail-issue-{index}-open"),
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label("Open on GitHub")
                .child(button::glyph(crate::icons::ARROW_UP_RIGHT, ControlSize::Sm))
                .child("Open on GitHub")
                .on_click(move |_, _, cx: &mut App| cx.open_url(&url)),
            );
            // One explicit write per offered choice (A7's PB-3 contract); each
            // opens A7's confirmation dialog, nothing is written from here.
            for (choice_index, choice) in
                writeback_choices(detail, &issue.id).into_iter().enumerate()
            {
                let label = match &choice {
                    WritebackChoice::Comment { .. } => "Comment on GitHub…",
                    WritebackChoice::AddLabel => "Add label…",
                    WritebackChoice::RemoveLabel { .. } => "Remove label…",
                    WritebackChoice::Close => "Close issue…",
                };
                let id = if choice_index == 0 {
                    format!("detail-issue-{index}-writeback")
                } else {
                    format!("detail-issue-{index}-writeback-{choice_index}")
                };
                let work_item_id = detail.view.item.id.clone();
                let issue_ref_id = issue.id.clone();
                let display = summary.display.clone();
                controls = controls.child(
                    self.button(id, label, ButtonKind::Secondary, true, theme)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(BoardDetailEvent::Writeback {
                                work_item_id: work_item_id.clone(),
                                issue_ref_id: issue_ref_id.clone(),
                                display: display.clone(),
                                choice: choice.clone(),
                            })
                        })),
                );
            }
            card = card.child(
                widgets::card_row(theme, false)
                    .child(widgets::row_tile(theme, crate::icons::GLOBAL))
                    .child(widgets::row_text(theme, heading, Some(meta)))
                    .child(controls),
            );
        }
        Some(card.into_any_element())
    }

    fn render_dependencies(
        &self,
        detail: &BoardItemDetail,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if detail.dependencies.is_empty() {
            return None;
        }
        let mut card = evidence::section(
            theme,
            "detail-dependencies",
            "Blocked by",
            Some("Shown for planning: a cleared blocker never starts work by itself.".into()),
        );
        let model = self.model.read(cx);
        for (index, dependency) in detail.dependencies.iter().enumerate() {
            let open = detail.view.blocked_by.contains(&dependency.blocked_by_id);
            let (key, title) = model
                .item(&dependency.blocked_by_id)
                .map(|view| (view.item.key.clone(), view.item.title.clone()))
                .unwrap_or_else(|| ("A ticket".into(), dependency.blocked_by_id.clone()));
            let copy = self.text(
                format!("detail-blocker-{index}"),
                format!(
                    "{key} · {title} · {}",
                    if open {
                        "still open (blocks this ticket)"
                    } else {
                        "done or archived (no longer blocks)"
                    }
                ),
            );
            let id = dependency.blocked_by_id.clone();
            card = card.child(
                widgets::card_row(theme, false)
                    .child(widgets::row_tile(
                        theme,
                        if open {
                            crate::icons::LOCK_KEYHOLE
                        } else {
                            crate::icons::LOCK_KEYHOLE_UNLOCKED
                        },
                    ))
                    .child(widgets::row_text(theme, copy, None))
                    .child(
                        widgets::row_controls().child(
                            self.button(
                                format!("detail-blocker-{index}-open"),
                                "Open",
                                ButtonKind::Ghost,
                                true,
                                theme,
                            )
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.emit(BoardDetailEvent::OpenTicket(id.clone()))
                                },
                            )),
                        ),
                    ),
            );
        }
        Some(card.into_any_element())
    }

    fn render_archive(
        &self,
        detail: &BoardItemDetail,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let view = &detail.view;
        let action = if view.actions.contains(&BoardAction::Unarchive) {
            DetailAction::Unarchive
        } else if view.actions.contains(&BoardAction::Archive) {
            DetailAction::Archive
        } else {
            return None;
        };
        let busy = self.action_task.is_some();
        Some(
            widgets::card_row(theme, false)
                .child(widgets::row_text(
                    theme,
                    if action == DetailAction::Archive {
                        "Archive"
                    } else {
                        "Archived"
                    },
                    Some(
                        "Archiving hides the card and keeps its links and history. It never \
                         stops work or touches a GitHub issue."
                            .into(),
                    ),
                ))
                .child(
                    widgets::row_controls().child(
                        self.button(
                            format!("detail-action-{}", action.key()),
                            action.label(),
                            ButtonKind::Ghost,
                            !busy,
                            theme,
                        )
                        .child(button::glyph(
                            if action == DetailAction::Archive {
                                crate::icons::ARCHIVE_MINIMALISTIC
                            } else {
                                crate::icons::ARCHIVE_UP_MINIMALISTIC
                            },
                            ControlSize::Sm,
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| this.run_action(action, cx))),
                    ),
                )
                .into_any_element(),
        )
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_state(&self, window: &Window, cx: &App) -> serde_json::Value {
        let edit_state = |edit: &Option<Edit>| match edit {
            None => serde_json::Value::Null,
            Some(edit) => serde_json::json!({
                "baseRevision": edit.base_revision,
                "saving": edit.saving,
                "error": edit.error,
                "conflictRevision": edit.conflict.as_ref().map(|current| current.item.revision),
                "text": edit.input.as_ref().map(|input| input.read(cx).text().to_owned()),
                "focused": edit.input.as_ref().map(|input| input.read(cx).focus_handle(cx).is_focused(window)),
                "settings": edit.editor.as_ref().map(|editor| editor.read(cx).fixture_state()),
            }),
        };
        let (loaded, detail) = match &self.loaded {
            Loaded::Loading => ("loading", serde_json::Value::Null),
            Loaded::Error(error) => ("error", serde_json::json!({ "error": error })),
            Loaded::Ready(detail) => (
                "ready",
                serde_json::json!({
                    "item": detail.view.item,
                    "phase": detail.view.phase,
                    "actions": detail.view.actions,
                    "workingChat": detail.view.working_chat,
                    "runActive": run_active(&detail.view),
                    "sendUpdateOffered": send_update_offered(detail),
                    "lastSentBriefRevision": last_sent_brief_revision(detail),
                    "dispatch": evidence::DispatchCounts::of(&detail.launches).json(),
                    "launches": detail.launches.len(),
                    "reviewRuns": detail.review_runs.len(),
                    "outcomes": detail.outcomes.len(),
                }),
            ),
        };
        serde_json::json!({
            "workItemId": self.work_item_id,
            "loaded": loaded,
            "detail": detail,
            "focused": self.focus.is_focused(window),
            "edits": {
                "title": edit_state(&self.title_edit),
                "brief": edit_state(&self.brief_edit),
                "settings": edit_state(&self.settings_edit),
            },
            "actionInFlight": self.action_task.is_some(),
            "pendingAction": self.pending_action.as_ref().map(|(action, _)| action.key()),
            "notice": self.notice.as_ref().map(|notice| serde_json::json!({
                "tone": notice.tone.key(),
                "text": notice.text,
            })),
            "optionsLoaded": self.options.is_some(),
        })
    }
}

/// Reconcile a lost mutation by its original request id.
async fn reconcile(
    api: &BoardApi,
    request_id: String,
) -> Result<BoardMutationReceipt, BoardCallError> {
    match api.receipt(BoardReceiptRequest { request_id }).await {
        Ok(Some(receipt)) => Ok(receipt),
        Ok(None) | Err(_) => Err(BoardCallError::Lost),
    }
}

impl Render for BoardDetail {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        // The root tracks focus (a press anywhere focuses the drawer, so
        // Escape reaches it); the header stays put and only the body scrolls.
        let root = div()
            .id("board-detail")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col();
        let detail = match &self.loaded {
            Loaded::Loading => {
                self.probe.text("detail-loading", "Loading the ticket…");
                return root
                    .child(
                        div()
                            .p(ui_rems(20.0))
                            .child(widgets::row_description(&theme, "Loading the ticket…")),
                    )
                    .into_any_element();
            }
            Loaded::Error(error) => {
                self.probe.text("detail-error", error.clone());
                return root
                    .child(
                        div()
                            .p(ui_rems(20.0))
                            .flex()
                            .flex_col()
                            .gap(ui_rems(12.0))
                            .child(widgets::error_strip(&theme, error.clone()).mt_0())
                            .child(
                                self.button(
                                    "detail-close",
                                    "Close",
                                    ButtonKind::Secondary,
                                    true,
                                    &theme,
                                )
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(BoardDetailEvent::Close)),
                                ),
                            ),
                    )
                    .into_any_element();
            }
            Loaded::Ready(detail) => detail.as_ref().clone(),
        };
        let weak: WeakEntity<Self> = cx.entity().downgrade();
        // Chat titles from the local chat list, for actors and launches.
        let titles: std::collections::HashMap<String, String> = self
            .state
            .read(cx)
            .chats
            .iter()
            .filter_map(|chat| chat.title.clone().map(|title| (chat.id.clone(), title)))
            .collect();
        let lookup = |task_id: &str| titles.get(task_id).cloned();
        let ecx = EvidenceContext {
            theme: &theme,
            probe: &self.probe,
            this: weak,
            options: self.options.as_ref(),
            chat_title: &lookup,
        };
        let review = evidence::render_review(&detail, &ecx);
        let outcome = evidence::render_evidence(&detail, &ecx);
        let dispatch = evidence::render_dispatch(&detail, self.expanded_launch.as_deref(), &ecx);
        let history = evidence::render_history(&detail, &ecx);
        // Pinned header: key, status, title (or its rename editor) and Close
        // stay reachable however far the body is scrolled, in any width.
        let header = div()
            .id("detail-header")
            .relative()
            .flex_none()
            .w_full()
            .px(ui_rems(20.0))
            .pt(ui_rems(14.0))
            .pb(ui_rems(10.0))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.bg)
            .child(self.render_header(&detail, &theme, cx))
            .children(self.probe.bounds_el("detail-header"));
        let body = div()
            .w_full()
            .px(ui_rems(20.0))
            .pt(ui_rems(12.0))
            .pb(ui_rems(48.0))
            .flex()
            .flex_col()
            .gap(ui_rems(10.0))
            .children(self.render_notice(&theme))
            .children(self.render_attention(&detail, &theme, cx))
            .child(self.render_actions(&detail, &theme, cx))
            .child(self.render_working_chat(&detail, &theme, cx))
            .child(self.render_brief(&detail, &theme, cx))
            .child(self.render_settings(&detail, &theme, cx))
            .child(review)
            .child(outcome)
            .child(dispatch)
            .child(history)
            .children(self.render_issues(&detail, &theme, cx))
            .children(self.render_dependencies(&detail, &theme, cx))
            .children(self.render_archive(&detail, &theme, cx))
            .children(self.probe.bounds_el("detail-body"));
        // The scroller fills a non-scrolling frame, whose bounds are the
        // visible body area (the probe there never moves with the content).
        let scroller = div()
            .id("detail-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(body);
        root.child(header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(scroller)
                    .children(self.probe.bounds_el("detail-scroll")),
            )
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// App navigation for Board surfaces
// ---------------------------------------------------------------------------

impl Shell {
    /// After an overlay above the Board drawer closes (A7's writeback dialog
    /// or issue picker), give the keyboard back to the open drawer.
    pub(in crate::shell) fn board_restore_drawer_focus(&mut self, cx: &mut Context<Self>) {
        if let Some(page) = &self.board_page {
            let surfaces = page.read(cx).surfaces().clone();
            surfaces.update(cx, |surfaces, cx| surfaces.restore_focus(cx));
        }
    }

    /// Route a Board surface request to the existing app surfaces: Go to
    /// chat (A10's `board_open_navigation`), the existing attention UI, A6's
    /// conversation, and A7's GitHub picker and writeback dialog.
    pub(in crate::shell) fn on_board_surface_event(
        &mut self,
        event: &BoardSurfacesEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            BoardSurfacesEvent::GoToChat(target) => self.board_open_navigation(target.clone(), cx),
            BoardSurfacesEvent::OpenAttention {
                task_id: Some(task_id),
                ..
            } => {
                // The chat holds the existing approval/question/error UI; the
                // Activity path opens it the way the Activity center does.
                self.open_activity_chat(task_id.clone(), cx);
            }
            BoardSurfacesEvent::OpenAttention { task_id: None, .. } => {}
            BoardSurfacesEvent::Writeback {
                work_item_id,
                issue_ref_id,
                display,
                choice,
            } => self.open_board_issue_writeback(
                work_item_id.clone(),
                issue_ref_id.clone(),
                display.clone(),
                choice.clone(),
                cx,
            ),
            BoardSurfacesEvent::OpenGitHub { project_id } => {
                self.open_board_github_picker(project_id.clone(), cx)
            }
            BoardSurfacesEvent::DiscussWithAi { project_id } => {
                let page = self.board_page(cx);
                page.update(cx, |page, cx| {
                    page.open_authoring(project_id.clone(), None, cx);
                });
            }
        }
    }

    /// QA: open the Board page (as its sidebar row does).
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_board_detail_open_board(&mut self, cx: &mut Context<Self>) {
        self.open_sidebar_page(crate::shell::SidebarPage::Board, cx);
    }

    /// QA: the Shell's route as a short string (`chat`, `page:board`, …).
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_board_detail_route(&self) -> String {
        match self.route {
            Route::Chat => "chat".into(),
            Route::Page(page) => format!("page:{}", page.key()),
            Route::Settings(_) => "settings".into(),
        }
    }
}
