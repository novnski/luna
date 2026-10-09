//! Discuss with AI (A6): the temporary authoring conversation, shown with the
//! main chat's own surfaces — the [`Transcript`] (markdown, tool cards, the
//! working trailer) over the discussion's entries and the [`Composer`]
//! (attachments, `@` files, `$` skills, `/` commands, the model pill and the
//! access lock) bound to no chat ([`ChatBinding::Detached`]): its sends,
//! Stop and assistant settings come here and go to the engine's authoring
//! session. A compact header names the project and the temporary nature; a
//! read-only proposed-ticket card (then the saved receipt with Open ticket)
//! sits above the composer; Close keeps an unfinished discussion as a
//! resumable Board draft and Discard drops it. There is no editable title or
//! description anywhere in this mode: the user revises the proposal by
//! talking to the assistant, and the engine owns every rule (authority,
//! exclusion from chat surfaces, runtime stop, cleanup).
//!
//! Entry points: [`AuthoringPane::new`] (a new discussion, used by the New
//! issue chooser) and [`AuthoringPane::resume`] (Resume draft);
//! [`BoardPage::open_authoring`] mounts either in the Board page, and
//! [`AuthoringDrafts`] lists the resumable drafts for the page.
//!
//! [`ChatBinding::Detached`]: crate::chat_binding::ChatBinding::Detached
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FontWeight, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, Task, WeakEntity, Window, div,
    prelude::*, px,
};
use luna_doc::SessionMessageEntry;
use luna_proto::{
    AuthoringDiscussionProfile, AuthoringProposal, AuthoringState, BoardAuthoringListRequest,
    BoardAuthoringPage, BoardAuthoringSendRequest, BoardAuthoringSessionRequest,
    BoardAuthoringStartRequest, BoardExecutionProfile, BoardReadRequest, BoardReviewProfile,
    BoardScope, HarnessId, PermissionMode, ReasoningLevel, TicketAuthoringSession, WorkItem,
};

use super::api::{BoardApi, BoardCallError};
use super::model::BoardModel;
use super::page::BoardPage;
use crate::button::{ButtonKind, ControlSize};
use crate::composer::{Composer, ComposerEvent, DetachedMessage};
use crate::icons;
use crate::pickers::DraftConfig;
use crate::settings::widgets;
use crate::theme::Theme;
use crate::transcript::Transcript;

/// A watch that ended while the discussion is open is retried this often.
const WATCH_RETRY: Duration = Duration::from_secs(2);
/// The proposal card shows this much of the brief.
const SUMMARY_CHARS: usize = 360;
/// The composer's prompt in this mode.
const PLACEHOLDER: &str = "Describe what should change, attach screenshots, or mention files…";

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

/// The authoring pane for one temporary session.
pub struct AuthoringPane {
    pub(crate) model: Entity<BoardModel>,
    pub(crate) project_id: String,
    pub(crate) session_id: Option<String>,
    /// Minted once per pane: a retried start returns the same session.
    request_id: String,
    session: Option<TicketAuthoringSession>,
    entries: Vec<SessionMessageEntry>,
    working: bool,
    effective_access: Option<PermissionMode>,
    status: PaneStatus,
    notice: Option<String>,
    /// A message is uploading or being recorded.
    sending: bool,
    /// A Close/Discard/settings change is in flight.
    busy: bool,
    tickets: HashMap<String, WorkItem>,
    loading_tickets: HashSet<String>,
    /// The main chat's composer, bound to no chat: its owner sends.
    composer: Entity<Composer>,
    /// The main chat's transcript over this discussion's entries (from the
    /// first engine answer on).
    transcript: Option<Entity<Transcript>>,
    /// The discussion profile last shown in the composer's pickers.
    shown_profile: Option<AuthoringDiscussionProfile>,
    focus_pending: bool,
    composer_width: Rc<Cell<f32>>,
    composer_height: Rc<Cell<f32>>,
    last_layout: (f32, f32),
    watch_generation: u64,
    start_task: Option<Task<()>>,
    watch_task: Option<Task<()>>,
    send_task: Option<Task<()>>,
    op_task: Option<Task<()>>,
    ticket_tasks: Vec<Task<()>>,
    #[cfg(feature = "appshots-fixture")]
    probe: std::rc::Rc<std::cell::RefCell<HashMap<String, gpui::Bounds<gpui::Pixels>>>>,
    /// QA: the send path's recent steps (focus, submit, send, frames).
    #[cfg(feature = "appshots-fixture")]
    trace: std::collections::VecDeque<String>,
    _composer: [Subscription; 2],
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
        let state = model.read(cx).state().clone();
        let binding = crate::chat_binding::ChatBinding::Detached {
            space_id: project_id.clone(),
        };
        let composer = cx.new(|cx| {
            let mut composer = Composer::new_bound(state, binding, cx);
            composer.set_placeholder(PLACEHOLDER, cx);
            composer
        });
        let events = cx.subscribe(&composer, |pane: &mut Self, _, event, cx| match event {
            ComposerEvent::DetachedSend(message) => {
                pane.trace("composer submitted");
                pane.send(message.clone(), cx);
            }
            ComposerEvent::DetachedStop => pane.stop(cx),
            _ => {}
        });
        let pickers = composer.read(cx).pickers().clone();
        let settings = cx.observe(&pickers, |pane: &mut Self, _, cx| {
            pane.on_pickers_changed(cx)
        });
        // The discussion's rows live in the app state only while it is open.
        cx.on_release(|pane: &mut Self, cx| pane.release_rows(cx))
            .detach();
        Self {
            model,
            project_id,
            session_id,
            request_id: format!("authoring-{}", uuid::Uuid::new_v4().simple()),
            session: None,
            entries: Vec::new(),
            working: false,
            effective_access: None,
            status: PaneStatus::Starting,
            notice: None,
            sending: false,
            busy: false,
            tickets: HashMap::new(),
            loading_tickets: HashSet::new(),
            composer,
            transcript: None,
            shown_profile: None,
            focus_pending: true,
            composer_width: Rc::new(Cell::new(0.0)),
            composer_height: Rc::new(Cell::new(0.0)),
            last_layout: (0.0, 0.0),
            watch_generation: 0,
            start_task: None,
            watch_task: None,
            send_task: None,
            op_task: None,
            ticket_tasks: Vec::new(),
            #[cfg(feature = "appshots-fixture")]
            probe: Default::default(),
            #[cfg(feature = "appshots-fixture")]
            trace: Default::default(),
            _composer: [events, settings],
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
                self.mount_transcript(&id, cx);
                self.sync_pickers(cx);
                self.start_watch(id, cx);
            }
            Err(error) => {
                self.trace(format!("start refused: {}", error.message()));
                self.status = PaneStatus::Error(error.message());
            }
        }
        cx.notify();
    }

    /// The main transcript over this session's rows (pushed by the watch).
    fn mount_transcript(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.transcript.is_some() {
            return;
        }
        let state = self.model.read(cx).state().clone();
        let id = session_id.to_owned();
        state.update(cx, |state, cx| {
            state.set_subagent_snapshot(id.clone(), Vec::new());
            cx.notify();
        });
        self.transcript = Some(cx.new(|cx| Transcript::for_doc(state, id, true, cx)));
    }

    /// Drop this discussion's rows from the app state (pane closed).
    fn release_rows(&mut self, cx: &mut App) {
        if let Some(id) = self.session_id.clone() {
            let state = self.model.read(cx).state().clone();
            state.update(cx, |state, cx| {
                state.unwatch_subagent_doc(&id);
                cx.notify();
            });
        }
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
        let id = page.session.id.clone();
        self.session = Some(page.session);
        self.status = PaneStatus::Ready;
        for item in created {
            self.load_ticket(item, cx);
        }
        // The transcript renders the engine's rows, like a chat's.
        self.mount_transcript(&id, cx);
        let state = self.model.read(cx).state().clone();
        let entries = self.entries.clone();
        state.update(cx, |state, cx| {
            state.set_subagent_snapshot(id, entries);
            cx.notify();
        });
        let running = self.working || self.sending;
        self.composer.update(cx, |composer, cx| {
            composer.set_detached_running(running, cx)
        });
        self.sync_pickers(cx);
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

    // ---- the assistant's settings: the composer's pickers ----

    fn draft_of(profile: &AuthoringDiscussionProfile) -> DraftConfig {
        DraftConfig {
            harness: profile.harness,
            provider_account_id: profile.provider_account_id.clone(),
            model: profile.model.clone(),
            reasoning: profile.reasoning,
            model_options: profile.model_options.clone(),
            permission_mode: profile.permission_mode,
            ..DraftConfig::default()
        }
    }

    /// Show the session's discussion profile in the composer's pickers.
    fn sync_pickers(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.session.as_ref().map(|s| s.discussion.clone()) else {
            return;
        };
        if self.shown_profile.as_ref() == Some(&profile) {
            return;
        }
        self.shown_profile = Some(profile.clone());
        let pickers = self.composer.read(cx).pickers().clone();
        pickers.update(cx, |pickers, cx| {
            pickers.set_detached_draft(Self::draft_of(&profile), false, cx)
        });
    }

    /// A pick in the composer's pickers changes the discussion assistant
    /// through the engine (validated there; never the ticket's profile).
    fn on_pickers_changed(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = self.shown_profile.clone() else {
            return;
        };
        let draft = self.composer.read(cx).pickers().read(cx).draft().clone();
        if draft == Self::draft_of(&shown) {
            return;
        }
        let next = AuthoringDiscussionProfile {
            harness: draft.harness,
            provider_account_id: draft.provider_account_id,
            model: draft.model,
            reasoning: draft.reasoning,
            model_options: draft.model_options,
            permission_mode: draft.permission_mode,
        };
        self.shown_profile = Some(next.clone());
        self.change_discussion(next, cx);
    }

    // ---- messages ----

    /// Send the composer's message (text, `@`/`$` selections, images) as the
    /// discussion's next user message. Images upload to the project's host
    /// first, as for a chat; a refusal hands everything back to the composer.
    pub fn send(&mut self, message: DetachedMessage, cx: &mut Context<Self>) {
        if message.text.trim().is_empty()
            && message.attachments.is_empty()
            && message.input_items.is_empty()
        {
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
            self.composer
                .update(cx, |composer, cx| composer.restore_detached(message, cx));
            return;
        }
        let state = self.model.read(cx).state().clone();
        let (Some(session_id), Some(api), Some(engine)) = (
            self.session_id.clone(),
            self.api(cx),
            state.read(cx).engine().cloned(),
        ) else {
            self.trace("send skipped: no session or engine connection");
            self.composer
                .update(cx, |composer, cx| composer.restore_detached(message, cx));
            return;
        };
        // Uploads go to the project's host, like a chat's on that host.
        let host = {
            let state = state.read(cx);
            state
                .space_row(&self.project_id)
                .map(|space| space.device_id.clone())
                .filter(|device| state.local_device_id.as_deref() != Some(device.as_str()))
        };
        self.trace(format!(
            "send started ({} bytes, {} images, {} selections)",
            message.text.len(),
            message.attachments.len(),
            message.input_items.len()
        ));
        let message_id = format!("msg-{}", uuid::Uuid::new_v4().simple());
        self.sending = true;
        self.notice = None;
        self.composer
            .update(cx, |composer, cx| composer.set_detached_running(true, cx));
        self.send_task = Some(cx.spawn(async move |this, cx| {
            let mut paths = Vec::new();
            for attachment in &message.attachments {
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
                    Ok(path) => paths.push(path),
                    Err(error) => {
                        tracing::warn!(%error, "authoring attachment upload failed");
                        this.update(cx, |pane, cx| {
                            pane.send_failed(
                                message.clone(),
                                format!("{} couldn't upload; nothing was sent.", attachment.name),
                                cx,
                            )
                        })
                        .ok();
                        return;
                    }
                }
            }
            let request = BoardAuthoringSendRequest {
                session_id,
                message_id,
                text: crate::attachments::with_attachments(message.text.trim(), &paths),
                input_items: message.input_items.clone(),
                attachments: paths,
            };
            let mut result = api.authoring_send(request.clone()).await;
            if matches!(result, Err(BoardCallError::Lost)) {
                // The message id is the no-replay key.
                result = api.authoring_send(request).await;
            }
            this.update(cx, |pane, cx| match result {
                // The watch reports the turn; a reply can be older than a
                // frame that already arrived.
                Ok(session) => {
                    pane.trace(format!("send accepted (version {})", session.version));
                    pane.sending = false;
                    pane.adopt(session);
                    cx.notify();
                }
                Err(error) => {
                    pane.trace(format!("send refused: {}", error.message()));
                    pane.send_failed(message.clone(), error.message(), cx);
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn send_failed(&mut self, message: DetachedMessage, why: String, cx: &mut Context<Self>) {
        self.sending = false;
        self.notice = Some(why);
        let running = self.working;
        self.composer.update(cx, |composer, cx| {
            composer.set_detached_running(running, cx);
            composer.restore_detached(message, cx);
        });
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
        discussion: AuthoringDiscussionProfile,
        cx: &mut Context<Self>,
    ) {
        let (Some(session), Some(api)) = (self.session.clone(), self.api(cx)) else {
            return;
        };
        if self.working || self.busy {
            self.notice = Some("Change the assistant after this reply.".into());
            self.shown_profile = None;
            self.sync_pickers(cx);
            cx.notify();
            return;
        }
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
                // Show what the engine kept (a refused pick snaps back).
                pane.shown_profile = None;
                pane.sync_pickers(cx);
                cx.notify();
            })
            .ok();
        }));
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

/// A one-line label for a recorded tool use (QA readback).
#[cfg(feature = "appshots-fixture")]
fn tool_label(call: &luna_proto::ToolCall) -> String {
    use luna_proto::ToolCall;
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

    /// The compact header: back to the Board, what this is, where, and the
    /// two ways out.
    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let project = self.project_name(cx);
        let enabled = !self.busy && self.session.is_some();
        let saved = self.saved();
        let pill = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(crate::typography::ui_rems(4.0))
            .px(crate::typography::ui_rems(7.0))
            .py(crate::typography::ui_rems(2.0))
            .rounded_full()
            .bg(theme.fill(crate::theme::FillTier::Quaternary))
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.readable_muted(0.85))
            .child(
                icons::icon(icons::CHAT_ROUND_LINE)
                    .size(crate::typography::ui_rems(11.0))
                    .text_color(theme.readable_muted(0.85)),
            )
            .child("Temporary");
        let close = crate::button::button(
            "authoring-close",
            if saved {
                ButtonKind::Primary
            } else {
                ButtonKind::Ghost
            },
            ControlSize::Sm,
            enabled,
            theme,
        )
        .aria_label(if saved {
            "Close discussion"
        } else {
            "Close and keep a Board draft"
        })
        .tooltip(crate::popover::text_tooltip(if saved {
            "The ticket is saved; this discussion ends."
        } else {
            "Keeps this unfinished discussion as a Board draft you can resume."
        }))
        .child(if saved { "Close" } else { "Close (keep draft)" })
        .on_click(cx.listener(|pane, _, _, cx| pane.close(cx)));
        let discard = (!saved).then(|| {
            self.probed(
                "discard",
                crate::button::button(
                    "authoring-discard",
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    enabled,
                    theme,
                )
                .aria_label("Discard discussion")
                .tooltip(crate::popover::text_tooltip(
                    "Drops this discussion; nothing is kept.",
                ))
                .child(crate::button::glyph(
                    icons::TRASH_BIN_MINIMALISTIC,
                    ControlSize::Sm,
                ))
                .child("Discard")
                .on_click(cx.listener(|pane, _, _, cx| pane.discard(cx))),
            )
        });
        div()
            .id("authoring-header")
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(crate::typography::ui_rems(8.0))
            .px(crate::typography::ui_rems(16.0))
            .py(crate::typography::ui_rems(10.0))
            .border_b_1()
            .border_color(theme.separator(false))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(crate::typography::ui_rems(8.0))
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("Discuss a ticket"),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(13.0))
                            .text_color(theme.readable_muted(0.75))
                            .child(SharedString::from(project)),
                    )
                    .child(pill),
            )
            .children(discard)
            .child(self.probed("close", close))
            .into_any_element()
    }

    /// The read-only proposal (until it is saved), compact, above the
    /// composer.
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
            .py(crate::typography::ui_rems(10.0))
            .rounded(px(14.0))
            .border_1()
            .border_color(theme.separator(true))
            .bg(theme.fill(crate::theme::FillTier::Quaternary))
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(4.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(crate::typography::ui_rems(6.0))
                    .child(
                        icons::icon(icons::CLIPBOARD_LIST)
                            .size(crate::typography::ui_rems(13.0))
                            .text_color(theme.accent),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from(title)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.readable_muted(0.7))
                            .child("Proposed · read only"),
                    ),
            )
            .when(!proposal.description.trim().is_empty(), |card| {
                card.child(
                    div()
                        .text_size(crate::typography::ui_rems(12.0))
                        .line_height(crate::typography::ui_rems(17.0))
                        .text_color(theme.readable_muted(0.9))
                        .child(SharedString::from(clip(
                            proposal.description.trim(),
                            SUMMARY_CHARS,
                        ))),
                )
            })
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.readable_muted(0.75))
                    .child(SharedString::from(format!(
                        "Implement: {} · Review: {}",
                        profile_text(&proposal.implementation),
                        review_text(&proposal.review)
                    ))),
            )
            .when(!proposal.unresolved.is_empty(), |card| {
                card.child(
                    div()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.readable_tone(theme.warning, 0.85))
                        .child(SharedString::from(format!(
                            "Still to decide: {}",
                            proposal.unresolved.join(", ")
                        ))),
                )
            });
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
                    .py(crate::typography::ui_rems(10.0))
                    .rounded(px(14.0))
                    .border_1()
                    .border_color(theme.separator(true))
                    .bg(theme.fill(crate::theme::FillTier::Quaternary))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(crate::typography::ui_rems(10.0))
                    .child(
                        icons::icon(icons::CLIPBOARD_LIST)
                            .size(crate::typography::ui_rems(15.0))
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
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(11.5))
                                    .text_color(theme.readable_muted(0.75))
                                    .child("No chat was created; this discussion can close."),
                            ),
                    )
                    .child(self.probed(&format!("open-ticket-{index}"), open));
                self.probed(&format!("receipt-{index}"), card)
            })
            .collect()
    }

    /// Before the first message: what the assistant does, where.
    fn render_intro(&self, theme: &Theme, cx: &App) -> AnyElement {
        let project = self.project_name(cx);
        div()
            .id("authoring-intro")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .pt(crate::typography::ui_rems(56.0))
            .px(crate::typography::ui_rems(24.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(crate::typography::ui_rems(6.0))
            .child(
                icons::icon(icons::CHAT_ROUND_LINE)
                    .size(crate::typography::ui_rems(22.0))
                    .text_color(theme.readable_muted(0.6)),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(15.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("What should the ticket be?"),
            )
            .child(
                div()
                    .max_w(crate::typography::ui_rems(460.0))
                    .text_center()
                    .text_size(crate::typography::ui_rems(12.5))
                    .line_height(crate::typography::ui_rems(18.0))
                    .text_color(theme.readable_muted(0.75))
                    .child(SharedString::from(format!(
                        "The ticket agent explores {project}, can run commands to reproduce the \
                         problem, asks what is missing and saves the ticket when you say so. It \
                         writes the ticket; it does not fix the issue. This discussion never \
                         appears in chats; closing keeps an unfinished one as a Board draft."
                    ))),
            )
            .into_any_element()
    }
}

impl Render for AuthoringPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_pending) {
            let focus = gpui::Focusable::focus_handle(self.composer.read(cx), cx);
            window.focus(&focus, cx);
        }
        let theme = Theme::of(cx).clone();
        let ended = matches!(
            self.state(),
            Some(AuthoringState::Ended | AuthoringState::Discarded)
        );
        // The main view's layout: the transcript spans the body and the
        // composer stack floats over its bottom; the transcript's last row
        // clears the measured stack.
        let width = self.composer_width.get();
        if width > 0.0 {
            self.composer.update(cx, |composer, cx| {
                composer.set_available_width(width.min(crate::composer::COMPOSER_MAX_WIDTH), cx)
            });
        }
        let clearance = if ended {
            0.0
        } else {
            self.composer_height.get()
        };
        let layout = (width, clearance);
        let relayout = (self.last_layout.0 - layout.0).abs() > 0.5
            || (self.last_layout.1 - layout.1).abs() > 0.5;
        self.last_layout = layout;
        if let Some(transcript) = &self.transcript {
            transcript.update(cx, |transcript, cx| {
                transcript.set_bottom_clearance(clearance, cx);
                if relayout {
                    transcript.snap_to_end_if_pinned(cx);
                }
            });
        }
        let header = self.render_header(&theme, cx);
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
            .map(|notice| widgets::warning_strip(&theme, notice).mt_0());
        let saved = self.saved();
        let proposal = self
            .session
            .as_ref()
            .and_then(|s| s.proposal.as_ref())
            .filter(|_| !saved)
            .cloned()
            .map(|proposal| self.render_proposal(&proposal, &theme));
        let receipts = self.render_receipts(&theme, cx);
        let intro = (self.entries.is_empty() && self.status == PaneStatus::Ready)
            .then(|| self.render_intro(&theme, cx));
        let measured = self.composer_width.clone();
        let measured_height = self.composer_height.clone();
        // The composer stack: the proposal or receipt, then the composer
        // (which carries the main column's gutter and bottom margin).
        let gutter = crate::typography::ui_rems(16.0);
        let stack = div()
            .id("authoring-composer-stack")
            .w_full()
            .flex()
            .flex_col()
            .relative()
            .child(
                gpui::canvas(
                    move |bounds, window, _| {
                        let next = f32::from(bounds.size.width);
                        let height = f32::from(bounds.size.height);
                        if (measured.get() - next).abs() > 0.5
                            || (measured_height.get() - height).abs() > 0.5
                        {
                            measured.set(next);
                            measured_height.set(height);
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .when(
                proposal.is_some() || !receipts.is_empty() || notice.is_some(),
                |stack| {
                    stack.child(
                        div()
                            .w_full()
                            .max_w(crate::typography::ui_rems(
                                crate::composer::COMPOSER_MAX_WIDTH,
                            ))
                            .mx_auto()
                            .px(gutter)
                            .pb(crate::typography::ui_rems(8.0))
                            .flex()
                            .flex_col()
                            .gap(crate::typography::ui_rems(8.0))
                            .children(notice)
                            .children(proposal)
                            .children(receipts),
                    )
                },
            )
            .when(!ended, |stack| {
                stack.child(self.probed("composer", self.composer.clone()))
            });
        let body = div()
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .children(self.transcript.clone())
            .children(intro)
            .child(div().absolute().left_0().right_0().bottom_0().child(stack));
        div()
            .id("board-authoring")
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.text)
            // A press here (after the composer focused its input, bubbling)
            // must not let the focusable Board page take focus back.
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                window.prevent_default()
            })
            // Files dropped anywhere on the discussion stage in its composer.
            .on_drop(cx.listener(|pane, paths: &gpui::ExternalPaths, _, cx| {
                let paths = paths.paths().to_vec();
                pane.composer
                    .update(cx, |composer, cx| composer.add_paths(paths, cx));
                cx.stop_propagation();
            }))
            .child(header)
            .children(status.map(|status| {
                div()
                    .px(crate::typography::ui_rems(16.0))
                    .pt(crate::typography::ui_rems(10.0))
                    .child(status)
            }))
            .child(body)
    }
}

#[cfg(feature = "appshots-fixture")]
impl AuthoringPane {
    /// QA: what the pane rendered and where its controls are.
    pub fn fixture_readback(&self, window: &Window, cx: &App) -> serde_json::Value {
        let mut bounds: serde_json::Map<String, serde_json::Value> = self
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
        let composer = self.composer.read(cx).fixture_detached_readback(window, cx);
        if !composer["send"].is_null() {
            bounds.insert("send".into(), composer["send"].clone());
        }
        let entries: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "id": entry.id,
                    "role": entry.role,
                    "text": entry.parts.iter().filter_map(|part| match part {
                        luna_doc::MessagePart::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    }).collect::<Vec<_>>().join("\n"),
                    "tools": entry.parts.iter().filter_map(|part| match part {
                        luna_doc::MessagePart::Tool { call, .. } => Some(tool_label(call)),
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
            "composerText": composer["text"],
            "composerFocused": composer["focused"],
            "sendEnabled": self.can_send() && composer["sendEnabled"] == true,
            "composer": composer,
            "transcriptRows": self.transcript.as_ref().map(|t| t.read(cx).fixture_rows(cx)),
            "pending": serde_json::Value::Null,
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
