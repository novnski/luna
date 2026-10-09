//! Discuss with AI (A6): the temporary authoring conversation — a message
//! stream and one composer with the discussion assistant's harness, model,
//! reasoning and effective access; a read-only proposed-ticket summary; the
//! saved receipt with Open ticket; Close (keeps an unfinished discussion as
//! a resumable Board draft) and Discard. There is no editable title or
//! description anywhere in this mode: the user revises the proposal by
//! talking to the assistant, and the engine owns every rule (authority,
//! exclusion from chat surfaces, runtime stop, cleanup).
//!
//! Entry points: [`AuthoringPane::new`] (a new discussion, used by the New
//! issue chooser) and [`AuthoringPane::resume`] (Resume draft);
//! [`BoardPage::open_authoring`] mounts either in the Board page, and
//! [`AuthoringDrafts`] lists the resumable drafts for the page.
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, FontWeight,
    IntoElement, ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Task,
    WeakEntity, Window, div, prelude::*, px,
};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::registry::HarnessDescriptor;
use luna_proto::{
    AuthoringDiscussionProfile, AuthoringProposal, AuthoringState, BoardAuthoringListRequest,
    BoardAuthoringPage, BoardAuthoringSendRequest, BoardAuthoringSessionRequest,
    BoardAuthoringStartRequest, BoardExecutionProfile, BoardReadRequest, BoardReviewProfile,
    BoardScope, HarnessId, PermissionMode, ReasoningLevel, TicketAuthoringSession, ToolCall,
    WorkItem,
};
use luna_rpc::methods;

use super::api::{BoardApi, BoardCallError};
use super::model::BoardModel;
use super::page::BoardPage;
use crate::button::{ButtonKind, ControlSize};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons;
use crate::settings::widgets;
use crate::theme::{FillTier, Theme};

/// A watch that ended while the discussion is open is retried this often.
const WATCH_RETRY: Duration = Duration::from_secs(2);
/// The proposal summary shows this much of the brief.
const SUMMARY_CHARS: usize = 900;

/// What the pane tells its host.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthoringPaneEvent {
    /// Closed; `draft_kept` when an unfinished discussion stays a Board draft.
    Closed { draft_kept: bool },
    /// The draft was discarded (or a saved discussion ended).
    Discarded,
    /// Open the saved ticket's normal Board view.
    OpenTicket { work_item_id: String },
}

#[derive(Debug, Clone, PartialEq)]
enum PaneStatus {
    Starting,
    Ready,
    Error(String),
}

/// The discussion-assistant control whose choices are open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    Harness,
    Model,
    Reasoning,
    Access,
}

/// The authoring pane for one temporary session.
pub struct AuthoringPane {
    pub(crate) model: Entity<BoardModel>,
    pub(crate) project_id: String,
    pub(crate) session_id: Option<String>,
    /// Minted once per pane: a retried start returns the same session.
    request_id: String,
    session: Option<TicketAuthoringSession>,
    entries: Vec<SessionMessageEntry>,
    /// The message being sent, shown until the engine records it.
    pending: Option<(String, String)>,
    working: bool,
    effective_access: Option<PermissionMode>,
    status: PaneStatus,
    notice: Option<String>,
    sending: bool,
    /// A Close/Discard/settings change is in flight.
    busy: bool,
    tickets: HashMap<String, WorkItem>,
    loading_tickets: HashSet<String>,
    input: Entity<ComposerInput>,
    focus_pending: bool,
    menu: Option<Menu>,
    harnesses: Vec<HarnessDescriptor>,
    models: Vec<luna_proto::Model>,
    models_for: Option<HarnessId>,
    scroll: ScrollHandle,
    watch_generation: u64,
    start_task: Option<Task<()>>,
    watch_task: Option<Task<()>>,
    send_task: Option<Task<()>>,
    op_task: Option<Task<()>>,
    catalog_task: Option<Task<()>>,
    ticket_tasks: Vec<Task<()>>,
    #[cfg(feature = "appshots-fixture")]
    probe: std::rc::Rc<std::cell::RefCell<HashMap<String, gpui::Bounds<gpui::Pixels>>>>,
    /// QA: the send path's recent steps (focus, submit, send, frames).
    #[cfg(feature = "appshots-fixture")]
    trace: std::collections::VecDeque<String>,
    _input: Subscription,
}

impl EventEmitter<AuthoringPaneEvent> for AuthoringPane {}

impl AuthoringPane {
    /// A new discussion in `project_id` (the New issue chooser's Discuss
    /// with AI). The engine session starts at once; nothing runs until the
    /// first message.
    pub fn new(model: Entity<BoardModel>, project_id: String, cx: &mut Context<Self>) -> Self {
        let mut pane = Self::build(model, project_id, None, cx);
        pane.begin(cx);
        pane
    }

    /// Reopen a Board draft (or an open discussion) in its own project with
    /// its recorded discussion and settings; the engine re-validates access.
    pub fn resume(
        model: Entity<BoardModel>,
        project_id: String,
        session_id: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self::build(model, project_id, Some(session_id), cx);
        pane.begin(cx);
        pane
    }

    fn build(
        model: Entity<BoardModel>,
        project_id: String,
        session_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            ComposerInput::new(
                "Explain the issue, add details, or choose execution settings…",
                cx,
            )
            .with_accessibility_role(gpui::Role::TextInput)
        });
        let events = cx.subscribe(&input, |pane: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                pane.trace("composer submitted");
                pane.send(cx);
            }
        });
        Self {
            model,
            project_id,
            session_id,
            request_id: format!("authoring-{}", uuid::Uuid::new_v4().simple()),
            session: None,
            entries: Vec::new(),
            pending: None,
            working: false,
            effective_access: None,
            status: PaneStatus::Starting,
            notice: None,
            sending: false,
            busy: false,
            tickets: HashMap::new(),
            loading_tickets: HashSet::new(),
            input,
            focus_pending: true,
            menu: None,
            harnesses: Vec::new(),
            models: Vec::new(),
            models_for: None,
            scroll: ScrollHandle::new(),
            watch_generation: 0,
            start_task: None,
            watch_task: None,
            send_task: None,
            op_task: None,
            catalog_task: None,
            ticket_tasks: Vec::new(),
            #[cfg(feature = "appshots-fixture")]
            probe: Default::default(),
            #[cfg(feature = "appshots-fixture")]
            trace: Default::default(),
            _input: events,
        }
    }

    fn api(&self, cx: &App) -> Option<BoardApi> {
        self.model.read(cx).api(cx)
    }

    /// QA: record one step of the send path (bounded; fixture builds only).
    #[cfg(feature = "appshots-fixture")]
    fn trace(&mut self, step: impl Into<String>) {
        if self.trace.len() >= 60 {
            self.trace.pop_front();
        }
        self.trace.push_back(step.into());
    }

    #[cfg(not(feature = "appshots-fixture"))]
    fn trace(&mut self, _step: impl Into<String>) {}

    /// The session this pane shows (after the engine answered).
    #[allow(dead_code)] // for the New issue chooser (A9) and QA hooks
    pub fn session(&self) -> Option<&TicketAuthoringSession> {
        self.session.as_ref()
    }

    fn begin(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api(cx) else {
            self.status = PaneStatus::Error("Engine not connected.".into());
            return;
        };
        let request = BoardAuthoringStartRequest {
            request_id: self.request_id.clone(),
            project_id: self.project_id.clone(),
            resume_session_id: self.session_id.clone(),
            discussion: None,
        };
        self.start_task = Some(cx.spawn(async move |this, cx| {
            let mut result = api.authoring_start(request.clone()).await;
            if matches!(result, Err(BoardCallError::Lost)) {
                // The same request id returns the same session.
                result = api.authoring_start(request).await;
            }
            this.update(cx, |pane, cx| pane.started(result, cx)).ok();
        }));
        self.load_harnesses(cx);
    }

    fn started(
        &mut self,
        result: Result<TicketAuthoringSession, BoardCallError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(session) => {
                self.session_id = Some(session.id.clone());
                let id = session.id.clone();
                self.session = Some(session);
                self.status = PaneStatus::Ready;
                self.start_watch(id, cx);
            }
            Err(error) => {
                self.trace(format!("start refused: {}", error.message()));
                self.status = PaneStatus::Error(error.message());
            }
        }
        cx.notify();
    }

    fn start_watch(&mut self, session_id: String, cx: &mut Context<Self>) {
        let Some(api) = self.api(cx) else {
            return;
        };
        self.watch_generation += 1;
        let generation = self.watch_generation;
        self.watch_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let request = BoardAuthoringSessionRequest {
                    session_id: session_id.clone(),
                };
                match api.authoring_watch(request).await {
                    Ok(mut frames) => {
                        while let Some(value) = frames.recv().await {
                            let Ok(page) = serde_json::from_value::<BoardAuthoringPage>(value)
                            else {
                                tracing::warn!("dropping malformed authoring frame");
                                continue;
                            };
                            let ended = matches!(
                                page.session.state,
                                AuthoringState::Ended | AuthoringState::Discarded
                            );
                            if this
                                .update(cx, |pane, cx| pane.apply_page(page, cx))
                                .is_err()
                                || ended
                            {
                                return;
                            }
                        }
                    }
                    Err(BoardCallError::Unsupported) => return,
                    Err(_) => {}
                }
                cx.background_executor().timer(WATCH_RETRY).await;
                if this
                    .update(cx, |pane, _| pane.watch_generation == generation)
                    .ok()
                    != Some(true)
                {
                    return;
                }
            }
        }));
    }

    fn apply_page(&mut self, page: BoardAuthoringPage, cx: &mut Context<Self>) {
        self.entries = page
            .entries
            .into_iter()
            .filter_map(|entry| serde_json::from_value(entry).ok())
            .collect();
        if self
            .pending
            .as_ref()
            .is_some_and(|(id, _)| self.entries.iter().any(|entry| &entry.id == id))
        {
            self.pending = None;
        }
        self.working = page.working;
        self.effective_access = page.effective_access;
        let frame = format!(
            "frame: {:?}, {} entries, working {}",
            page.session.state,
            self.entries.len(),
            page.working
        );
        self.trace(frame);
        let created = page.session.created_item_ids.clone();
        self.session = Some(page.session);
        self.status = PaneStatus::Ready;
        for id in created {
            self.load_ticket(id, cx);
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// The saved receipt: the ticket's committed key and column.
    fn load_ticket(&mut self, work_item_id: String, cx: &mut Context<Self>) {
        if self.tickets.contains_key(&work_item_id)
            || !self.loading_tickets.insert(work_item_id.clone())
        {
            return;
        }
        let Some(api) = self.api(cx) else {
            return;
        };
        let request = BoardReadRequest {
            work_item_id: work_item_id.clone(),
            project_id: Some(self.project_id.clone()),
            history_limit: Some(1),
        };
        self.ticket_tasks.push(cx.spawn(async move |this, cx| {
            let result = api.read(request).await;
            this.update(cx, |pane, cx| {
                pane.loading_tickets.remove(&work_item_id);
                if let Ok(detail) = result {
                    pane.tickets.insert(work_item_id, detail.view.item);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn load_harnesses(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.model.read(cx).state().read(cx).engine().cloned() else {
            return;
        };
        self.catalog_task = Some(cx.spawn(async move |this, cx| {
            let listed = engine
                .client()
                .call(methods::LIST_HARNESSES, serde_json::json!({}))
                .await
                .ok()
                .and_then(|value| serde_json::from_value::<Vec<HarnessDescriptor>>(value).ok())
                .unwrap_or_default();
            this.update(cx, |pane, cx| {
                pane.harnesses = listed
                    .into_iter()
                    .filter(|descriptor| descriptor.installed && descriptor.enabled != Some(false))
                    .collect();
                cx.notify();
            })
            .ok();
        }));
    }

    fn load_models(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        if self.models_for == Some(harness) {
            return;
        }
        let Some(engine) = self.model.read(cx).state().read(cx).engine().cloned() else {
            return;
        };
        self.models_for = Some(harness);
        self.models.clear();
        self.catalog_task = Some(cx.spawn(async move |this, cx| {
            let listed = engine
                .client()
                .call(
                    methods::LIST_MODELS,
                    serde_json::json!({ "harness": harness }),
                )
                .await
                .ok()
                .and_then(|value| serde_json::from_value::<Vec<luna_proto::Model>>(value).ok())
                .unwrap_or_default();
            this.update(cx, |pane, cx| {
                if pane.models_for == Some(harness) {
                    pane.models = listed;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Take a session from an RPC reply unless a newer watch frame already
    /// replaced it (versions only grow while the session is open).
    fn adopt(&mut self, session: TicketAuthoringSession) {
        if self
            .session
            .as_ref()
            .is_none_or(|current| session.version >= current.version)
        {
            self.session = Some(session);
        }
    }

    fn state(&self) -> Option<AuthoringState> {
        self.session.as_ref().map(|session| session.state)
    }

    fn saved(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| !session.created_item_ids.is_empty())
    }

    fn can_send(&self) -> bool {
        matches!(
            self.state(),
            Some(AuthoringState::Active | AuthoringState::Saved)
        ) && !self.working
            && !self.sending
            && !self.busy
    }

    /// Send the composer's text as the next user message.
    pub fn send(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().trim().to_owned();
        if text.is_empty() {
            self.trace("send skipped: the composer is empty");
            return;
        }
        if !self.can_send() {
            let reason = format!(
                "send skipped: state {:?}, working {}, sending {}, busy {}",
                self.state(),
                self.working,
                self.sending,
                self.busy
            );
            self.trace(reason);
            return;
        }
        let (Some(session_id), Some(api)) = (self.session_id.clone(), self.api(cx)) else {
            self.trace("send skipped: no session or engine connection");
            return;
        };
        self.trace(format!("send started ({} bytes)", text.len()));
        let message_id = format!("msg-{}", uuid::Uuid::new_v4().simple());
        self.pending = Some((message_id.clone(), text.clone()));
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.sending = true;
        self.notice = None;
        self.menu = None;
        let request = BoardAuthoringSendRequest {
            session_id,
            message_id,
            text: text.clone(),
        };
        self.send_task = Some(cx.spawn(async move |this, cx| {
            let mut result = api.authoring_send(request.clone()).await;
            if matches!(result, Err(BoardCallError::Lost)) {
                // The message id is the no-replay key.
                result = api.authoring_send(request).await;
            }
            this.update(cx, |pane, cx| {
                pane.sending = false;
                match result {
                    // The watch reports the turn; a reply can be older
                    // than a frame that already arrived.
                    Ok(session) => {
                        pane.trace(format!("send accepted (version {})", session.version));
                        pane.adopt(session);
                    }
                    Err(error) => {
                        pane.trace(format!("send refused: {}", error.message()));
                        pane.pending = None;
                        pane.notice = Some(error.message());
                        pane.input.update(cx, |input, cx| input.set_text(text, cx));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let (Some(session_id), Some(api)) = (self.session_id.clone(), self.api(cx)) else {
            return;
        };
        self.op_task = Some(cx.spawn(async move |_, _| {
            let _ = api
                .authoring_interrupt(BoardAuthoringSessionRequest { session_id })
                .await;
        }));
    }

    /// Close: an unfinished discussion stays a resumable Board draft; a
    /// saved one ends. The engine stops the runtime either way.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.finish(false, cx);
    }

    /// Discard the draft (the engine keeps only exclusion metadata).
    pub fn discard(&mut self, cx: &mut Context<Self>) {
        self.finish(true, cx);
    }

    fn finish(&mut self, discard: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(session_id) = self.session_id.clone() else {
            // Nothing was created yet: there is nothing to keep.
            cx.emit(AuthoringPaneEvent::Closed { draft_kept: false });
            return;
        };
        let Some(api) = self.api(cx) else {
            self.notice = Some("Engine not connected.".into());
            cx.notify();
            return;
        };
        self.busy = true;
        self.menu = None;
        cx.notify();
        self.op_task = Some(cx.spawn(async move |this, cx| {
            let request = BoardAuthoringSessionRequest { session_id };
            let result = if discard {
                api.authoring_discard(request).await
            } else {
                api.authoring_close(request).await
            };
            this.update(cx, |pane, cx| {
                pane.busy = false;
                match result {
                    Ok(session) => {
                        let state = session.state;
                        pane.session = Some(session);
                        pane.watch_generation += 1;
                        pane.watch_task = None;
                        cx.emit(match state {
                            AuthoringState::Discarded => AuthoringPaneEvent::Discarded,
                            AuthoringState::Draft => {
                                AuthoringPaneEvent::Closed { draft_kept: true }
                            }
                            _ if discard => AuthoringPaneEvent::Discarded,
                            _ => AuthoringPaneEvent::Closed { draft_kept: false },
                        });
                    }
                    Err(error) => pane.notice = Some(error.message()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn open_ticket(&mut self, work_item_id: String, cx: &mut Context<Self>) {
        cx.emit(AuthoringPaneEvent::OpenTicket { work_item_id });
    }

    /// Apply a discussion-assistant change through the engine (validated
    /// there; never the ticket's implementation profile).
    fn change_discussion(
        &mut self,
        change: impl FnOnce(&mut AuthoringDiscussionProfile),
        cx: &mut Context<Self>,
    ) {
        let (Some(session), Some(api)) = (self.session.clone(), self.api(cx)) else {
            return;
        };
        if self.working || self.busy {
            self.notice = Some("Change the assistant after this reply.".into());
            cx.notify();
            return;
        }
        let mut discussion = session.discussion.clone();
        change(&mut discussion);
        self.menu = None;
        self.busy = true;
        let request = BoardAuthoringStartRequest {
            request_id: format!("authoring-settings-{}", uuid::Uuid::new_v4().simple()),
            project_id: self.project_id.clone(),
            resume_session_id: Some(session.id.clone()),
            discussion: Some(discussion),
        };
        self.op_task = Some(cx.spawn(async move |this, cx| {
            let result = api.authoring_start(request).await;
            this.update(cx, |pane, cx| {
                pane.busy = false;
                match result {
                    Ok(session) => {
                        pane.adopt(session);
                        pane.notice = None;
                    }
                    Err(error) => pane.notice = Some(error.message()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = (self.menu != Some(menu)).then_some(menu);
        if self.menu == Some(Menu::Model)
            && let Some(harness) = self.session.as_ref().and_then(|s| s.discussion.harness)
        {
            self.load_models(harness, cx);
        }
        cx.notify();
    }

    fn project_name(&self, cx: &App) -> String {
        self.model
            .read(cx)
            .state()
            .read(cx)
            .space_row(&self.project_id)
            .map(|space| space.display_name().to_owned())
            .unwrap_or_else(|| "this project".into())
    }

    fn harness_label(&self, harness: Option<HarnessId>) -> String {
        match harness {
            Some(harness) => self
                .harnesses
                .iter()
                .find(|descriptor| descriptor.id == harness)
                .map(|descriptor| descriptor.name.clone())
                .unwrap_or_else(|| harness_name(harness).into()),
            None => "Choose an assistant".into(),
        }
    }
}

fn harness_name(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::ClaudeCode => "Claude Code",
        HarnessId::Codex => "Codex",
        HarnessId::Opencode => "OpenCode",
        _ => "Provider",
    }
}

fn access_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Limited access",
        PermissionMode::Auto => "Approve for me",
        PermissionMode::Full => "Full access",
    }
}

fn reasoning_label(level: ReasoningLevel) -> String {
    let text = serde_json::to_value(level)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Default".into(),
    }
}

/// A one-line label for a recorded tool use in the transcript.
fn tool_label(call: &ToolCall) -> String {
    let short = |path: &str| {
        path.rsplit(['/', '\\'])
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("/")
    };
    match call {
        ToolCall::ReadFile { path } => format!("Read {}", short(path)),
        ToolCall::Search { pattern, .. } => format!("Searched for {pattern}"),
        ToolCall::Glob { pattern } => format!("Listed {pattern}"),
        ToolCall::Exec { command } => {
            format!("Ran {}", command.chars().take(60).collect::<String>())
        }
        ToolCall::WriteFile { path, .. } | ToolCall::EditFile { path, .. } => {
            format!("Edit {}", short(path))
        }
        ToolCall::Mcp { tool, .. } => match tool.as_str() {
            "board_create" => "Saved the ticket".into(),
            "board_draft_checkpoint" => "Updated the proposed ticket".into(),
            "board_execution_options" => "Checked execution options".into(),
            "board_start" => "Started the ticket".into(),
            "board_read" | "board_list" => "Read the board".into(),
            other => other.replace('_', " "),
        },
        ToolCall::WebFetch { url, .. } => format!("Fetched {url}"),
        ToolCall::WebSearch { query } => format!("Searched the web for {query}"),
        ToolCall::Unknown { name, .. } => name.clone(),
        _ => "Used a tool".into(),
    }
}

fn profile_text(implementation: &BoardExecutionProfile) -> String {
    if implementation.deferred {
        return "settings left for later (chosen before it runs)".into();
    }
    if implementation.is_inherited() {
        return "project defaults".into();
    }
    let mut parts = Vec::new();
    if let Some(harness) = implementation.harness {
        parts.push(harness_name(harness).to_owned());
    }
    if let Some(model) = &implementation.model {
        parts.push(model.clone());
    }
    if let Some(reasoning) = implementation.reasoning {
        parts.push(reasoning_label(reasoning));
    }
    if let Some(mode) = implementation.permission_mode {
        parts.push(access_label(mode).to_owned());
    }
    if parts.is_empty() {
        "project defaults".into()
    } else {
        parts.join(" · ")
    }
}

fn review_text(review: &BoardReviewProfile) -> String {
    if review.disabled {
        return "no automatic review".into();
    }
    let mut parts = Vec::new();
    if let Some(harness) = review.harness {
        parts.push(harness_name(harness).to_owned());
    }
    if let Some(model) = &review.model {
        parts.push(model.clone());
    }
    if let Some(reasoning) = review.reasoning {
        parts.push(reasoning_label(reasoning));
    }
    if parts.is_empty() {
        "project default reviewer".into()
    } else {
        parts.join(" · ")
    }
}

fn clip(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(chars).collect();
    clipped.push('…');
    clipped
}

impl AuthoringPane {
    #[cfg(feature = "appshots-fixture")]
    fn probed(&self, label: &str, element: impl IntoElement) -> AnyElement {
        let probe = self.probe.clone();
        let label = label.to_owned();
        div()
            .child(element)
            .on_children_prepainted(move |children, _, _| {
                if let Some(bounds) = children.first() {
                    probe.borrow_mut().insert(label.clone(), *bounds);
                }
            })
            .into_any_element()
    }

    #[cfg(not(feature = "appshots-fixture"))]
    fn probed(&self, _label: &str, element: impl IntoElement) -> AnyElement {
        element.into_any_element()
    }

    fn render_entry(&self, entry: &SessionMessageEntry, theme: &Theme) -> AnyElement {
        let (who, user) = match entry.role {
            MessageRole::User => ("You", true),
            MessageRole::Assistant => ("Ticket assistant", false),
            MessageRole::System => ("LUNA", false),
        };
        let mut body = div().flex().flex_col().gap(crate::typography::ui_rems(4.0));
        for part in &entry.parts {
            match part {
                MessagePart::Text { text, .. } if !text.trim().is_empty() => {
                    body = body.child(
                        div()
                            .min_w_0()
                            .text_size(crate::typography::ui_rems(13.0))
                            .line_height(crate::typography::ui_rems(19.0))
                            .text_color(theme.text)
                            .child(SharedString::from(text.trim().to_owned())),
                    );
                }
                MessagePart::Tool { call, is_error, .. } => {
                    body = body.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(crate::typography::ui_rems(6.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.readable_muted(0.8))
                            .child(
                                icons::icon(icons::CHAT_LINE)
                                    .size(crate::typography::ui_rems(12.0))
                                    .text_color(theme.readable_muted(0.7)),
                            )
                            .child(SharedString::from(if *is_error {
                                format!("{} (refused)", tool_label(call))
                            } else {
                                tool_label(call)
                            })),
                    );
                }
                MessagePart::AsyncQuestion { questions, .. } => {
                    for question in questions {
                        let options = question
                            .options
                            .as_ref()
                            .map(|options| options.join(" · "))
                            .unwrap_or_default();
                        body = body.child(
                            div()
                                .px(crate::typography::ui_rems(10.0))
                                .py(crate::typography::ui_rems(8.0))
                                .rounded(px(8.0))
                                .border_1()
                                .border_color(theme.separator(false))
                                .flex()
                                .flex_col()
                                .gap(crate::typography::ui_rems(2.0))
                                .child(
                                    div()
                                        .text_size(crate::typography::ui_rems(13.0))
                                        .text_color(theme.text)
                                        .child(SharedString::from(question.title.clone())),
                                )
                                .when(!options.is_empty(), |card| {
                                    card.child(widgets::row_description(theme, options))
                                })
                                .child(widgets::row_description(
                                    theme,
                                    "Answer in your next message.",
                                )),
                        );
                    }
                }
                MessagePart::Error { message, .. } => {
                    body = body.child(
                        div()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.readable_tone(theme.warning, 0.9))
                            .child(SharedString::from(message.clone())),
                    );
                }
                _ => {}
            }
        }
        if entry.status == Some(MessageStatus::Streaming) {
            body = body.child(widgets::row_description(theme, "…"));
        }
        div()
            .id(SharedString::from(format!("authoring-entry-{}", entry.id)))
            .w_full()
            .flex()
            .flex_col()
            .when(user, |row| row.items_end())
            .child(
                div()
                    .max_w(crate::typography::ui_rems(560.0))
                    .px(crate::typography::ui_rems(12.0))
                    .py(crate::typography::ui_rems(10.0))
                    .rounded(px(10.0))
                    .when(user, |bubble| bubble.bg(theme.fill(FillTier::Quaternary)))
                    .flex()
                    .flex_col()
                    .gap(crate::typography::ui_rems(4.0))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.readable_muted(0.75))
                            .child(who),
                    )
                    .child(body),
            )
            .into_any_element()
    }

    fn render_proposal(&self, proposal: &AuthoringProposal, theme: &Theme) -> AnyElement {
        let title = if proposal.title.trim().is_empty() {
            "Untitled ticket".to_owned()
        } else {
            proposal.title.clone()
        };
        let card = div()
            .id("authoring-proposal")
            .w_full()
            .px(crate::typography::ui_rems(14.0))
            .py(crate::typography::ui_rems(12.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.accent.opacity(0.35))
            .bg(theme.accent_wash)
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(6.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(crate::typography::ui_rems(6.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.readable_muted(0.85))
                    .child(
                        icons::icon(icons::CLIPBOARD_LIST)
                            .size(crate::typography::ui_rems(12.0))
                            .text_color(theme.readable_muted(0.85)),
                    )
                    .child("Proposed ticket · read only"),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(15.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from(title)),
            )
            .when(!proposal.description.trim().is_empty(), |card| {
                card.child(
                    div()
                        .text_size(crate::typography::ui_rems(12.5))
                        .line_height(crate::typography::ui_rems(18.0))
                        .text_color(theme.readable_muted(0.95))
                        .child(SharedString::from(clip(
                            proposal.description.trim(),
                            SUMMARY_CHARS,
                        ))),
                )
            })
            .child(widgets::row_description(
                theme,
                format!(
                    "Implement: {} · Review: {}",
                    profile_text(&proposal.implementation),
                    review_text(&proposal.review)
                ),
            ))
            .when(!proposal.unresolved.is_empty(), |card| {
                card.child(widgets::row_description(
                    theme,
                    format!("Still to decide: {}", proposal.unresolved.join(", ")),
                ))
            })
            .child(widgets::row_description(
                theme,
                "Tell the assistant what to change.",
            ));
        self.probed("proposal", card)
    }

    fn render_receipts(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        session
            .created_item_ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let ticket = self.tickets.get(id);
                let headline = match ticket {
                    Some(ticket) => format!("{} saved to {}", ticket.key, ticket.column.label()),
                    None => "Ticket saved to the Board".into(),
                };
                let open_id = id.clone();
                let open = crate::button::button(
                    SharedString::from(format!("authoring-open-ticket-{index}")),
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label("Open ticket")
                .child("Open ticket")
                .on_click(cx.listener(move |pane, _, _, cx| pane.open_ticket(open_id.clone(), cx)));
                let card = div()
                    .id(SharedString::from(format!("authoring-receipt-{index}")))
                    .w_full()
                    .px(crate::typography::ui_rems(14.0))
                    .py(crate::typography::ui_rems(12.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(theme.separator(true))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(crate::typography::ui_rems(12.0))
                    .child(
                        icons::icon(icons::CLIPBOARD_LIST)
                            .size(crate::typography::ui_rems(16.0))
                            .text_color(theme.readable_muted(0.9)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(SharedString::from(headline)),
                            )
                            .child(widgets::row_description(
                                theme,
                                "No working chat was created. This temporary discussion can now close.",
                            )),
                    )
                    .child(self.probed(&format!("open-ticket-{index}"), open));
                self.probed(&format!("receipt-{index}"), card)
            })
            .collect()
    }

    fn render_assistant_controls(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(session) = &self.session else {
            return div().into_any_element();
        };
        let discussion = &session.discussion;
        let enabled = !self.working && !self.busy;
        let chip = |id: &'static str, label: String, menu: Menu, cx: &mut Context<Self>| {
            let open = self.menu == Some(menu);
            self.probed(
                id,
                crate::button::select(id, label, ControlSize::Sm, open, enabled, theme)
                    .aria_label(match menu {
                        Menu::Harness => "Discussion assistant provider",
                        Menu::Model => "Discussion assistant model",
                        Menu::Reasoning => "Discussion assistant reasoning",
                        Menu::Access => "Discussion assistant access",
                    })
                    .on_click(cx.listener(move |pane, _, _, cx| pane.toggle_menu(menu, cx))),
            )
        };
        let access = self
            .effective_access
            .or(discussion.permission_mode)
            .unwrap_or(PermissionMode::Ask);
        let mut access_text = access_label(access).to_owned();
        if let Some(requested) = discussion.permission_mode
            && requested != access
        {
            access_text = format!("{access_text} (asked for {})", access_label(requested));
        }
        let mut row = div()
            .id("authoring-assistant")
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(crate::typography::ui_rems(6.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.readable_muted(0.85))
                    .child("Discussion assistant"),
            );
        row = row.child(chip(
            "authoring-harness",
            self.harness_label(discussion.harness),
            Menu::Harness,
            cx,
        ));
        row = row.child(chip(
            "authoring-model",
            discussion
                .model
                .clone()
                .unwrap_or_else(|| "Default model".into()),
            Menu::Model,
            cx,
        ));
        row = row.child(chip(
            "authoring-reasoning",
            discussion
                .reasoning
                .map(reasoning_label)
                .unwrap_or_else(|| "Default reasoning".into()),
            Menu::Reasoning,
            cx,
        ));
        row = row.child(chip("authoring-access", access_text, Menu::Access, cx));
        let mut column = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(6.0))
            .child(row);
        if let Some(menu) = self.menu {
            column = column.child(self.render_menu(menu, discussion, theme, cx));
        }
        column
            .child(widgets::row_description(
                theme,
                "These settings affect this temporary discussion, not the future ticket worker.",
            ))
            .into_any_element()
    }

    fn render_menu(
        &self,
        menu: Menu,
        discussion: &AuthoringDiscussionProfile,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut choices: Vec<AnyElement> = Vec::new();
        let mut push = |id: String,
                        label: String,
                        selected: bool,
                        element: Box<dyn Fn(&mut AuthoringDiscussionProfile)>,
                        cx: &mut Context<Self>| {
            let element = std::rc::Rc::new(element);
            choices.push(
                self.probed(
                    &id,
                    crate::button::chip(
                        SharedString::from(id.clone()),
                        selected,
                        ControlSize::Sm,
                        true,
                        theme,
                    )
                    .child(SharedString::from(label))
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        let element = element.clone();
                        pane.change_discussion(move |profile| element(profile), cx);
                    })),
                ),
            );
        };
        match menu {
            Menu::Harness => {
                for descriptor in &self.harnesses {
                    let harness = descriptor.id;
                    push(
                        format!(
                            "authoring-harness-{}",
                            harness_name(harness).replace(' ', "-").to_lowercase()
                        ),
                        descriptor.name.clone(),
                        discussion.harness == Some(harness),
                        Box::new(move |profile| {
                            if profile.harness != Some(harness) {
                                profile.harness = Some(harness);
                                profile.model = None;
                                profile.reasoning = None;
                                profile.model_options.clear();
                            }
                        }),
                        cx,
                    );
                }
            }
            Menu::Model => {
                push(
                    "authoring-model-default".into(),
                    "Default model".into(),
                    discussion.model.is_none(),
                    Box::new(|profile| profile.model = None),
                    cx,
                );
                for model in &self.models {
                    let id = model.id.clone();
                    push(
                        format!("authoring-model-{}", model.id),
                        model.label.clone(),
                        discussion.model.as_deref() == Some(model.id.as_str()),
                        Box::new(move |profile| profile.model = Some(id.clone())),
                        cx,
                    );
                }
            }
            Menu::Reasoning => {
                let levels = discussion
                    .harness
                    .and_then(|harness| {
                        self.harnesses
                            .iter()
                            .find(|descriptor| descriptor.id == harness)
                            .map(|descriptor| descriptor.reasoning_levels.clone())
                    })
                    .unwrap_or_default();
                push(
                    "authoring-reasoning-default".into(),
                    "Default reasoning".into(),
                    discussion.reasoning.is_none(),
                    Box::new(|profile| profile.reasoning = None),
                    cx,
                );
                for level in levels {
                    push(
                        format!(
                            "authoring-reasoning-{}",
                            reasoning_label(level).to_lowercase()
                        ),
                        reasoning_label(level),
                        discussion.reasoning == Some(level),
                        Box::new(move |profile| profile.reasoning = Some(level)),
                        cx,
                    );
                }
            }
            Menu::Access => {
                for mode in [
                    PermissionMode::Ask,
                    PermissionMode::Auto,
                    PermissionMode::Full,
                ] {
                    push(
                        format!(
                            "authoring-access-{}",
                            access_label(mode).replace(' ', "-").to_lowercase()
                        ),
                        access_label(mode).into(),
                        discussion.permission_mode.unwrap_or(PermissionMode::Ask) == mode,
                        Box::new(move |profile| profile.permission_mode = Some(mode)),
                        cx,
                    );
                }
            }
        }
        div()
            .id("authoring-menu")
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(crate::typography::ui_rems(6.0))
            .children(choices)
            .into_any_element()
    }

    fn render_composer(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let enabled = self.can_send();
        let action = if self.working {
            crate::button::button(
                "authoring-stop",
                ButtonKind::Secondary,
                ControlSize::Md,
                true,
                theme,
            )
            .aria_label("Stop the temporary assistant")
            .child(crate::button::glyph(icons::STOP, ControlSize::Md))
            .child("Stop")
            .on_click(cx.listener(|pane, _, _, cx| pane.stop(cx)))
        } else {
            crate::button::button(
                "authoring-send",
                ButtonKind::Primary,
                ControlSize::Md,
                enabled,
                theme,
            )
            .aria_label("Send to temporary assistant")
            .child(
                crate::button::glyph(icons::ARROW_UP, ControlSize::Md).text_color(theme.on_solid),
            )
            .on_click(cx.listener(|pane, _, _, cx| pane.send(cx)))
        };
        let frame = crate::popover::dialog_field(self.input.clone().into_any_element())
            .px(crate::typography::ui_rems(10.0))
            .py(crate::typography::ui_rems(8.0))
            .rounded(px(10.0))
            .border_color(theme.separator(true))
            .bg(theme.fill(FillTier::Quaternary))
            .text_size(crate::typography::ui_rems(13.0))
            .min_h(crate::typography::ui_rems(64.0))
            .cursor(gpui::CursorStyle::IBeam)
            // The input is one line at the top of this taller box and blurs
            // itself on any press outside its own bounds: a press anywhere
            // in the box (bubbling, after that blur) puts focus back in it.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|pane, _, window, cx| {
                    window.focus(&pane.input.focus_handle(cx), cx);
                    // Like GPUI's own focus transfer: a focusable ancestor
                    // (the Shell) must not take focus back during bubbling.
                    window.prevent_default();
                    pane.trace("composer box pressed: input focused");
                }),
            );
        div()
            .id("authoring-composer")
            .w_full()
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(8.0))
            .child(self.probed("composer", frame))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(crate::typography::ui_rems(8.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(crate::typography::ui_rems(6.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.readable_muted(0.8))
                            .child(
                                icons::icon(icons::FOLDER)
                                    .size(crate::typography::ui_rems(12.0))
                                    .text_color(theme.readable_muted(0.8)),
                            )
                            .child("Project context available"),
                    )
                    .child(div().flex_1())
                    .child(self.probed(if self.working { "stop" } else { "send" }, action)),
            )
            .into_any_element()
    }

    fn render_actions(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let enabled = !self.busy && self.session.is_some();
        let mut row = div()
            .id("authoring-actions")
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(crate::typography::ui_rems(8.0));
        if self.saved() {
            row = row.child(
                self.probed(
                    "close",
                    crate::button::button(
                        "authoring-close",
                        ButtonKind::Primary,
                        ControlSize::Md,
                        enabled,
                        theme,
                    )
                    .aria_label("Close discussion")
                    .child("Close discussion")
                    .on_click(cx.listener(|pane, _, _, cx| pane.close(cx))),
                ),
            );
        } else {
            row = row
                .child(
                    self.probed(
                        "discard",
                        crate::button::button(
                            "authoring-discard",
                            ButtonKind::Danger,
                            ControlSize::Md,
                            enabled,
                            theme,
                        )
                        .aria_label("Discard discussion")
                        .child("Discard discussion")
                        .on_click(cx.listener(|pane, _, _, cx| pane.discard(cx))),
                    ),
                )
                .child(
                    self.probed(
                        "close",
                        crate::button::button(
                            "authoring-close",
                            ButtonKind::Secondary,
                            ControlSize::Md,
                            enabled,
                            theme,
                        )
                        .aria_label("Close and keep a Board draft")
                        .child("Close (keep draft)")
                        .on_click(cx.listener(|pane, _, _, cx| pane.close(cx))),
                    ),
                );
        }
        row.into_any_element()
    }
}

impl Render for AuthoringPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_pending) {
            window.focus(&self.input.focus_handle(cx), cx);
        }
        let theme = Theme::of(cx).clone();
        let project = self.project_name(cx);
        let mut messages = div()
            .id("authoring-messages")
            .w_full()
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(10.0))
            .child(
                div()
                    .px(crate::typography::ui_rems(12.0))
                    .flex()
                    .flex_col()
                    .gap(crate::typography::ui_rems(2.0))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.readable_muted(0.75))
                            .child("Ticket assistant"),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(13.0))
                            .text_color(theme.text)
                            .child(SharedString::from(format!(
                                "Tell me what should change in {project}. I can look at the project, ask what is missing and save the ticket when you say so."
                            ))),
                    ),
            );
        for entry in &self.entries {
            messages = messages.child(self.render_entry(entry, &theme));
        }
        if let Some((id, text)) = &self.pending
            && !self.entries.iter().any(|entry| &entry.id == id)
        {
            let echo = SessionMessageEntry {
                id: id.clone(),
                role: MessageRole::User,
                parts: vec![MessagePart::Text {
                    id: "t0".into(),
                    text: text.clone(),
                }],
                created_at: 0,
                device_id: String::new(),
                status: Some(MessageStatus::Streaming),
                continuation_of: None,
            };
            messages = messages.child(self.render_entry(&echo, &theme));
        }
        let saved = self.saved();
        if let Some(proposal) = self.session.as_ref().and_then(|s| s.proposal.as_ref())
            && !saved
        {
            messages = messages.child(self.render_proposal(proposal, &theme));
        }
        messages = messages.children(self.render_receipts(&theme, cx));
        let status = match &self.status {
            PaneStatus::Starting => {
                Some(widgets::row_description(&theme, "Opening the discussion…"))
            }
            PaneStatus::Error(error) => Some(widgets::error_strip(&theme, error.clone())),
            PaneStatus::Ready => None,
        };
        let notice = self
            .notice
            .clone()
            .or_else(|| self.session.as_ref().and_then(|s| s.last_error.clone()))
            .map(|notice| widgets::warning_strip(&theme, notice));
        let ended = matches!(
            self.state(),
            Some(AuthoringState::Ended | AuthoringState::Discarded)
        );
        let tag = |label: String, glyph: &'static str| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(crate::typography::ui_rems(5.0))
                .px(crate::typography::ui_rems(8.0))
                .py(crate::typography::ui_rems(3.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme.separator(false))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.readable_muted(0.9))
                .child(
                    icons::icon(glyph)
                        .size(crate::typography::ui_rems(12.0))
                        .text_color(theme.readable_muted(0.9)),
                )
                .child(SharedString::from(label))
        };
        div()
            .id("board-authoring")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .w_full()
                    .max_w(crate::typography::ui_rems(760.0))
                    .mx_auto()
                    .px(crate::typography::ui_rems(24.0))
                    .pt(crate::typography::ui_rems(28.0))
                    .pb(crate::typography::ui_rems(48.0))
                    .flex()
                    .flex_col()
                    .gap(crate::typography::ui_rems(14.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(widgets::page_title_bar(&theme, "Discuss the ticket", None, vec![]))
                            .child(widgets::page_subtitle(
                                &theme,
                                "A temporary, project-aware conversation. The assistant writes the ticket; you change it by talking.",
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(crate::typography::ui_rems(6.0))
                            .child(tag("Temporary discussion".into(), icons::CHAT_ROUND_LINE))
                            .child(tag(format!("{project} attached"), icons::FOLDER)),
                    )
                    .children(status)
                    .child(messages)
                    .children(notice)
                    .when(!ended, |page| {
                        page.child(self.render_assistant_controls(&theme, cx))
                            .child(self.render_composer(&theme, cx))
                            .child(widgets::row_description(
                                &theme,
                                "This discussion never appears in chats, Settled or Archive. Closing keeps only an unfinished Board draft.",
                            ))
                            .child(self.render_actions(&theme, cx))
                    }),
            )
    }
}

#[cfg(feature = "appshots-fixture")]
impl AuthoringPane {
    /// QA: what the pane rendered and where its controls are.
    pub fn fixture_readback(&self, window: &Window, cx: &App) -> serde_json::Value {
        let bounds: serde_json::Map<String, serde_json::Value> = self
            .probe
            .borrow()
            .iter()
            .map(|(label, bounds)| {
                (
                    label.clone(),
                    serde_json::json!({
                        "x": f32::from(bounds.origin.x),
                        "y": f32::from(bounds.origin.y),
                        "w": f32::from(bounds.size.width),
                        "h": f32::from(bounds.size.height),
                    }),
                )
            })
            .collect();
        let entries: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "id": entry.id,
                    "role": entry.role,
                    "text": entry.parts.iter().filter_map(|part| match part {
                        MessagePart::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    }).collect::<Vec<_>>().join("\n"),
                    "tools": entry.parts.iter().filter_map(|part| match part {
                        MessagePart::Tool { call, .. } => Some(tool_label(call)),
                        _ => None,
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::json!({
            "sessionId": self.session_id,
            "state": self.state(),
            "working": self.working,
            "sending": self.sending,
            "busy": self.busy,
            "status": format!("{:?}", self.status),
            "notice": self.notice,
            "entries": entries,
            "composerText": self.input.read(cx).text(),
            "composerFocused": self.input.read(cx).focus_handle(cx).is_focused(window),
            "sendEnabled": self.can_send() && !self.input.read(cx).text().trim().is_empty(),
            "pending": self.pending.as_ref().map(|(id, text)| serde_json::json!({"id": id, "text": text})),
            "trace": self.trace.iter().collect::<Vec<_>>(),
            // The only text input in this mode is the composer: no title or
            // description field exists to report.
            "textInputs": ["composer"],
            "proposal": self.session.as_ref().and_then(|s| s.proposal.as_ref()).map(|p| serde_json::json!({
                "title": p.title,
                "implementation": p.implementation,
                "review": p.review,
                "unresolved": p.unresolved,
                "readOnly": true,
                "rendered": !self.saved(),
            })),
            "receipts": self.session.as_ref().map(|s| s.created_item_ids.iter().map(|id| serde_json::json!({
                "workItemId": id,
                "key": self.tickets.get(id).map(|t| t.key.clone()),
                "column": self.tickets.get(id).map(|t| t.column),
            })).collect::<Vec<_>>()),
            "discussion": self.session.as_ref().map(|s| &s.discussion),
            "effectiveAccess": self.effective_access,
            "bounds": bounds,
        })
    }
}

/// A mounted pane and its event subscription (owned by the Board page).
pub struct AuthoringMount {
    pub(super) pane: Entity<AuthoringPane>,
    pub(super) _events: Subscription,
}

impl BoardPage {
    /// Mount Discuss with AI in the page: a new discussion in `project_id`,
    /// or `resume` a Board draft. Used by the New issue chooser and the
    /// drafts list.
    pub fn open_authoring(
        &mut self,
        project_id: String,
        resume: Option<String>,
        cx: &mut Context<Self>,
    ) -> Entity<AuthoringPane> {
        let model = self.model().clone();
        let pane = cx.new(|cx| match resume {
            Some(session_id) => AuthoringPane::resume(model, project_id, session_id, cx),
            None => AuthoringPane::new(model, project_id, cx),
        });
        let events = cx.subscribe(&pane, |page: &mut Self, _, event, cx| match event {
            AuthoringPaneEvent::Closed { .. } | AuthoringPaneEvent::Discarded => {
                page.authoring = None;
                match page.authoring_then_open.take() {
                    // Open ticket: the conversation ended; show the ticket.
                    Some(work_item_id) => {
                        page.refresh_drafts(cx);
                        page.model().update(cx, |model, cx| model.refresh(cx));
                        page.show_ticket(work_item_id, cx);
                    }
                    None => page.authoring_closed(cx),
                }
                cx.notify();
            }
            AuthoringPaneEvent::OpenTicket { work_item_id } => {
                page.authoring_open_ticket(work_item_id.clone(), cx);
            }
        });
        self.authoring_then_open = None;
        self.authoring = Some(AuthoringMount {
            pane: pane.clone(),
            _events: events,
        });
        cx.notify();
        pane
    }

    /// The open Discuss with AI pane, if any.
    #[allow(dead_code)] // for the New issue chooser (A9) and QA hooks
    pub fn authoring_pane(&self) -> Option<Entity<AuthoringPane>> {
        self.authoring.as_ref().map(|mount| mount.pane.clone())
    }

    /// The resumable drafts list shown on the page.
    #[allow(dead_code)] // for the Board page (A8) and QA hooks
    pub fn authoring_drafts(&self) -> Entity<AuthoringDrafts> {
        self.drafts.clone()
    }

    /// Open ticket from the receipt: close the conversation (a saved
    /// discussion ends and the engine stops its runtime), then open the
    /// ticket's detail drawer once the close is confirmed. The pane stays
    /// mounted until then so its close request is not dropped.
    fn authoring_open_ticket(&mut self, work_item_id: String, cx: &mut Context<Self>) {
        let Some(pane) = self.authoring_pane() else {
            self.show_ticket(work_item_id, cx);
            return;
        };
        self.authoring_then_open = Some(work_item_id);
        pane.update(cx, |pane, cx| pane.close(cx));
    }
}

/// The resumable Board drafts (and open discussions) of the page's scope,
/// with the Discuss with AI entry. Drafts never appear in chat lists.
pub struct AuthoringDrafts {
    model: Entity<BoardModel>,
    page: WeakEntity<BoardPage>,
    sessions: Vec<TicketAuthoringSession>,
    error: Option<String>,
    load_task: Option<Task<()>>,
    op_task: Option<Task<()>>,
    scope: BoardScope,
    #[cfg(feature = "appshots-fixture")]
    probe: std::rc::Rc<std::cell::RefCell<HashMap<String, gpui::Bounds<gpui::Pixels>>>>,
    _model: Subscription,
}

impl AuthoringDrafts {
    pub fn new(
        model: Entity<BoardModel>,
        page: WeakEntity<BoardPage>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observer = cx.observe(&model, |drafts: &mut Self, model, cx| {
            if model.read(cx).scope() != &drafts.scope {
                drafts.scope = model.read(cx).scope().clone();
                drafts.refresh(cx);
            }
        });
        let scope = model.read(cx).scope().clone();
        let mut drafts = Self {
            model,
            page,
            sessions: Vec::new(),
            error: None,
            load_task: None,
            op_task: None,
            scope,
            #[cfg(feature = "appshots-fixture")]
            probe: Default::default(),
            _model: observer,
        };
        drafts.refresh(cx);
        drafts
    }

    /// The listed drafts and open discussions.
    #[allow(dead_code)] // for the Board page (A8)
    pub fn sessions(&self) -> &[TicketAuthoringSession] {
        &self.sessions
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.model.read(cx).api(cx) else {
            return;
        };
        let request = BoardAuthoringListRequest {
            project_id: self.scope.project_id().map(str::to_owned),
        };
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = api.authoring_list(request).await;
            this.update(cx, |drafts, cx| {
                match result {
                    Ok(list) => {
                        drafts.sessions = list.sessions;
                        drafts.error = None;
                    }
                    Err(BoardCallError::Unsupported) => drafts.sessions.clear(),
                    Err(error) => drafts.error = Some(error.message()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Discuss with AI in the page's project.
    pub fn discuss(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.scope.project_id().map(str::to_owned) else {
            return;
        };
        self.page
            .update(cx, |page, cx| {
                page.open_authoring(project_id, None, cx);
            })
            .ok();
    }

    pub fn resume(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.iter().find(|s| s.id == session_id).cloned() else {
            return;
        };
        self.page
            .update(cx, |page, cx| {
                page.open_authoring(session.project_id.clone(), Some(session.id.clone()), cx);
            })
            .ok();
    }

    pub fn discard(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let Some(api) = self.model.read(cx).api(cx) else {
            return;
        };
        let request = BoardAuthoringSessionRequest {
            session_id: session_id.to_owned(),
        };
        self.op_task = Some(cx.spawn(async move |this, cx| {
            let result = api.authoring_discard(request).await;
            this.update(cx, |drafts, cx| {
                if let Err(error) = result {
                    drafts.error = Some(error.message());
                }
                drafts.refresh(cx);
            })
            .ok();
        }));
    }

    #[cfg(feature = "appshots-fixture")]
    fn probed(&self, label: &str, element: impl IntoElement) -> AnyElement {
        let probe = self.probe.clone();
        let label = label.to_owned();
        div()
            .child(element)
            .on_children_prepainted(move |children, _, _| {
                if let Some(bounds) = children.first() {
                    probe.borrow_mut().insert(label.clone(), *bounds);
                }
            })
            .into_any_element()
    }

    #[cfg(not(feature = "appshots-fixture"))]
    fn probed(&self, _label: &str, element: impl IntoElement) -> AnyElement {
        element.into_any_element()
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_readback(&self) -> serde_json::Value {
        let bounds: serde_json::Map<String, serde_json::Value> = self
            .probe
            .borrow()
            .iter()
            .map(|(label, bounds)| {
                (
                    label.clone(),
                    serde_json::json!({
                        "x": f32::from(bounds.origin.x),
                        "y": f32::from(bounds.origin.y),
                        "w": f32::from(bounds.size.width),
                        "h": f32::from(bounds.size.height),
                    }),
                )
            })
            .collect();
        serde_json::json!({
            "drafts": self.sessions.iter().map(|session| serde_json::json!({
                "id": session.id,
                "state": session.state,
                "title": session.proposal.as_ref().map(|p| p.title.clone()),
            })).collect::<Vec<_>>(),
            "error": self.error,
            "bounds": bounds,
        })
    }
}

impl Render for AuthoringDrafts {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let project = self.scope.project_id().is_some();
        let discuss = crate::button::button(
            "board-discuss-with-ai",
            ButtonKind::Secondary,
            ControlSize::Md,
            project,
            &theme,
        )
        .aria_label("Discuss a new ticket with AI")
        .child(crate::button::glyph(
            icons::CHAT_ROUND_LINE,
            ControlSize::Md,
        ))
        .child("Discuss with AI")
        .on_click(cx.listener(|drafts, _, _, cx| drafts.discuss(cx)));
        let description: SharedString = if project {
            "A temporary, project-aware conversation that writes the ticket with you. Unfinished discussions stay here as Board drafts, never as chats."
                .into()
        } else {
            "Choose a project to discuss a new ticket with AI.".into()
        };
        let mut card =
            widgets::section_card(&theme)
                .id("board-drafts")
                .child(widgets::section_header(
                    &theme,
                    "Board drafts",
                    Some(description),
                    vec![self.probed("discuss", discuss)],
                ));
        for (index, session) in self.sessions.iter().enumerate() {
            let title = session
                .proposal
                .as_ref()
                .map(|proposal| proposal.title.trim().to_owned())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| "Untitled discussion".into());
            let state = match session.state {
                AuthoringState::Draft => "Draft",
                AuthoringState::Saved => "Saved, still open",
                _ => "Open",
            };
            let messages = session.message_count;
            let resume_id = session.id.clone();
            let discard_id = session.id.clone();
            let resume = crate::button::button(
                SharedString::from(format!("board-draft-resume-{index}")),
                ButtonKind::Secondary,
                ControlSize::Sm,
                true,
                &theme,
            )
            .aria_label("Resume draft")
            .child("Resume")
            .on_click(cx.listener(move |drafts, _, _, cx| drafts.resume(&resume_id, cx)));
            let discard = crate::button::button(
                SharedString::from(format!("board-draft-discard-{index}")),
                ButtonKind::Ghost,
                ControlSize::Sm,
                true,
                &theme,
            )
            .aria_label("Discard draft")
            .child("Discard")
            .on_click(cx.listener(move |drafts, _, _, cx| drafts.discard(&discard_id, cx)));
            card = card.child(
                widgets::card_row(&theme, index == 0)
                    .child(widgets::row_text(
                        &theme,
                        title,
                        Some(SharedString::from(format!(
                            "{state} · {messages} message{}",
                            if messages == 1 { "" } else { "s" }
                        ))),
                    ))
                    .child(
                        widgets::row_controls()
                            .gap(crate::typography::ui_rems(6.0))
                            .child(self.probed(&format!("resume-{index}"), resume))
                            .child(self.probed(&format!("discard-{index}"), discard)),
                    ),
            );
        }
        if let Some(error) = &self.error {
            card = card.child(widgets::error_strip(&theme, error.clone()));
        }
        card
    }
}
