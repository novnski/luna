use super::pane_chat::{
    PaneChatChrome, PaneChatEvent, PaneChatView, PaneComposers, PaneHeaderActions, PaneIdentity,
};
use super::*;
use crate::state::EngineHandle;
use luna_engine::side_chats::{SideChatCreated, SideChatList, SideChatPage};
use serde_json::{Value, json};
use std::rc::Rc;

fn state_label(state: &str) -> &str {
    match state {
        "active" => "Writable",
        "preparing" => "Preparing",
        "creationFailed" => "Creation incomplete",
        "closing" => "Saving",
        "closeFailed" => "Unload incomplete",
        "unloaded" => "Unloaded",
        "expired" => "Expired",
        "recovered" => "Recovered after restart",
        "deleted" => "Deleted",
        _ => "Unavailable",
    }
}

#[derive(Clone)]
enum PanelAction {
    New,
    NewSnapshot,
    Open(String),
    Older,
    Latest,
    MoreHistory,
    Send,
    Unload,
    Refresh,
    Answer(String),
    Approve(luna_proto::ApprovalDecision),
    RevealOrigin,
    /// Leave the open side chat: unload a writable one (it becomes read-only
    /// history), or just return to the list from a read-only page.
    Back,
    /// Stop only the side chat's current turn; the lease stays writable.
    Stop,
}

pub(super) enum SideChatEvent {
    RevealOrigin {
        parent: String,
        message_id: String,
        engine_generation: u64,
    },
    ResultToParent {
        parent: String,
        engine_generation: u64,
        text: String,
    },
}

impl gpui::EventEmitter<SideChatEvent> for SideChatPanel {}

/// Whether this side chat can use the full bound composer (contract §5.1):
/// the engine advertises `side-chat-task-commands-v1` (side chats accept the
/// ordinary task commands, watches, drafts and per-chat RPCs while leased)
/// and `WatchChats` publishes the live side chat's row (without a project,
/// so it stays out of the sidebar and can't become the main chat). Otherwise
/// side chats use the leased side-chat RPCs and the input says what they
/// support.
pub(super) fn side_chat_first_class(state: &AppState, side_id: &str) -> bool {
    state.engine().is_some_and(|engine| {
        engine
            .engine_info()
            .supports(luna_proto::capabilities::SIDE_CHAT_TASK_COMMANDS_V1)
    }) && state
        .chats
        .iter()
        .any(|chat| chat.id == side_id && chat.space_id.is_none())
}

/// The lease lapses after 30 s without a renewal; renew well inside that.
const LEASE_HEARTBEAT: Duration = Duration::from_secs(10);

pub(super) struct SideChatPanel {
    state: Entity<AppState>,
    /// Shared per-chat composers (first-class side chats bind one).
    composers: Entity<PaneComposers>,
    /// The shared full chat view, when this side chat is first-class.
    full_view: Option<(Entity<PaneChatView>, Subscription)>,
    /// First-class side chats left with Back: still writable, their leases
    /// kept alive by the heartbeat so the draft or a send in flight can
    /// finish and the chat can be reopened. Unloaded on panel release.
    parked: std::collections::HashMap<String, String>,
    last_renew: std::time::Instant,
    renew_task: Option<Task<()>>,
    links: crate::markdown::render::LinkUi,
    engine: Option<EngineHandle>,
    parent: String,
    engine_generation: u64,
    parent_title: String,
    page: Option<SideChatPage>,
    history: Option<SideChatList>,
    lease: Option<String>,
    transcript: Option<Entity<Transcript>>,
    transcript_events: Option<Subscription>,
    origin: Option<(String, String)>,
    input: Entity<ComposerInput>,
    before: Option<i64>,
    answers: std::collections::HashMap<String, Vec<String>>,
    busy: bool,
    operation_label: Option<&'static str>,
    focus_pending: bool,
    snapshot_alternative: bool,
    error: Option<String>,
    _events: Subscription,
    task: Option<Task<()>>,
    _ticker: Task<()>,
}

impl SideChatPanel {
    pub(super) fn new(
        state: Entity<AppState>,
        composers: Entity<PaneComposers>,
        links: crate::markdown::render::LinkUi,
        parent: String,
        origin: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) -> Self {
        let engine = state.read(cx).engine().cloned();
        let engine_generation = state.read(cx).engine_generation();
        let parent_title = state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == parent)
            .and_then(|chat| chat.title.clone())
            .unwrap_or_else(|| "Conversation".into());
        let input = cx.new(|cx| ComposerInput::new("Ask in this side chat…", cx));
        let events = cx.subscribe(&input, |this, _, event, cx| {
            if matches!(
                event,
                ComposerInputEvent::Submitted | ComposerInputEvent::ModifierSubmitted
            ) {
                this.action(PanelAction::Send, cx);
            }
        });
        cx.on_release(|this, cx| {
            if let Some(page) = &this.page {
                this.state
                    .update(cx, |state, _| state.unwatch_subagent_doc(&page.meta.id));
            }
        })
        .detach();
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if this
                    .update(cx, |panel, cx| {
                        // A first-class side chat streams through its live
                        // doc watch; reading pages would replace (and stop)
                        // that watch. Its lease only needs the heartbeat.
                        if panel.full_mode(cx) || !panel.parked.is_empty() {
                            if panel.last_renew.elapsed() >= LEASE_HEARTBEAT {
                                panel.renew_leases(cx);
                            }
                        } else if panel.lease.is_some() && !panel.busy {
                            panel.action(PanelAction::Refresh, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut this = Self {
            state,
            composers,
            full_view: None,
            parked: Default::default(),
            last_renew: std::time::Instant::now(),
            renew_task: None,
            links,
            engine,
            parent,
            engine_generation,
            parent_title,
            page: None,
            history: None,
            lease: None,
            transcript: None,
            transcript_events: None,
            origin,
            input,
            before: None,
            answers: Default::default(),
            busy: false,
            operation_label: None,
            focus_pending: false,
            snapshot_alternative: false,
            error: None,
            _events: events,
            task: None,
            _ticker: ticker,
        };
        this.action(
            if this.origin.is_some() {
                PanelAction::New
            } else {
                PanelAction::Refresh
            },
            cx,
        );
        this
    }

    /// Shell owns the asynchronous readback, so a failed unload stays visible
    /// even after the side view and its subscriptions have been released.
    pub(super) fn is_current(&self, cx: &App) -> bool {
        self.state.read(cx).engine_generation() == self.engine_generation
    }

    /// The open page is a writable, first-class side chat.
    fn full_mode(&self, cx: &App) -> bool {
        self.lease.is_some()
            && self.page.as_ref().is_some_and(|page| {
                page.meta.state == "active"
                    && side_chat_first_class(self.state.read(cx), &page.meta.id)
            })
    }

    /// Heartbeat: `RenewSideChat` for the open and every parked lease. Never
    /// reads pages, so live doc watches are untouched.
    fn renew_leases(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        if !self.is_current(cx) || self.renew_task.is_some() {
            return;
        }
        let mut leases: Vec<(String, String)> = self
            .parked
            .iter()
            .map(|(id, lease)| (id.clone(), lease.clone()))
            .collect();
        if let (Some(page), Some(lease)) = (&self.page, &self.lease) {
            leases.push((page.meta.id.clone(), lease.clone()));
        }
        if leases.is_empty() {
            return;
        }
        self.last_renew = std::time::Instant::now();
        self.renew_task = Some(cx.spawn(async move |this, cx| {
            let calls = leases.clone();
            let results = Tokio::spawn(cx, async move {
                let mut results = Vec::new();
                for (id, lease) in calls {
                    let result = tokio::time::timeout(
                        Duration::from_secs(10),
                        engine
                            .client()
                            .call(methods::RENEW_SIDE_CHAT, json!({"id": id, "lease": lease})),
                    )
                    .await
                    .map_err(|_| luna_rpc::RpcError::Failed("Lease renewal timed out".into()))
                    .and_then(|result| result);
                    results.push((id, result));
                }
                results
            })
            .await
            .unwrap_or_default();
            this.update(cx, |panel, cx| {
                panel.renew_task = None;
                for (id, result) in results {
                    let open = panel.page.as_ref().is_some_and(|page| page.meta.id == id);
                    match result {
                        Ok(value) => {
                            if open
                                && let Ok(meta) = serde_json::from_value::<
                                    luna_engine::side_chats::SideChatMeta,
                                >(value)
                                && let Some(page) = panel.page.as_mut()
                            {
                                page.meta = meta;
                            }
                        }
                        Err(error) => {
                            if open {
                                // The engine made it read-only; show the
                                // saved history and why.
                                panel.lease = None;
                                panel.full_view = None;
                                panel.error = Some(error.to_string());
                                panel.action(PanelAction::Refresh, cx);
                            } else {
                                panel.parked.remove(&id);
                            }
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Parked side chats to unload when the panel goes away.
    pub(super) fn parked_release_requests(&self) -> Vec<(EngineHandle, String, String)> {
        let Some(engine) = self.engine.clone() else {
            return Vec::new();
        };
        self.parked
            .iter()
            .map(|(id, lease)| (engine.clone(), id.clone(), lease.clone()))
            .collect()
    }

    pub(super) fn release_request(&self) -> Option<(EngineHandle, String, String)> {
        Some((
            self.engine.clone()?,
            self.page.as_ref()?.meta.id.clone(),
            self.lease.clone()?,
        ))
    }

    fn pending_question(&self) -> Option<(String, Vec<luna_proto::UserInputQuestion>)> {
        self.page
            .as_ref()?
            .entries
            .iter()
            .rev()
            .flat_map(|entry| entry.parts.iter().rev())
            .find_map(|part| match part {
                luna_doc::MessagePart::Input {
                    request_id,
                    questions,
                    resolved: false,
                    ..
                } => Some((request_id.clone(), questions.clone())),
                _ => None,
            })
    }

    fn action(&mut self, action: PanelAction, cx: &mut Context<Self>) {
        if matches!(action, PanelAction::Back) {
            if !self.is_current(cx) {
                return;
            }
            // A first-class side chat is parked, not unloaded: its composer
            // (draft, send in flight, receipt check) is cached per chat and
            // the heartbeat keeps the lease, so reopening continues it.
            if self.full_mode(cx)
                && let (Some(page), Some(lease)) = (self.page.take(), self.lease.take())
            {
                self.parked.insert(page.meta.id.clone(), lease);
                self.full_view = None;
                self.transcript = None;
                self.transcript_events = None;
                self.before = None;
                self.answers.clear();
                // A page read in flight must not reopen the parked chat.
                self.task = None;
                self.busy = false;
                self.operation_label = None;
                self.action(PanelAction::Refresh, cx);
                return;
            }
            // A send or unload in flight finishes first: navigation never
            // drops an in-flight side chat request or its readback.
            if self.busy {
                return;
            }
            if self.lease.is_some() {
                self.action(PanelAction::Unload, cx);
                return;
            }
            if let Some(page) = self.page.take() {
                self.state
                    .update(cx, |state, _| state.unwatch_subagent_doc(&page.meta.id));
            }
            self.transcript = None;
            self.transcript_events = None;
            self.before = None;
            self.answers.clear();
            self.action(PanelAction::Refresh, cx);
            return;
        }
        if matches!(action, PanelAction::RevealOrigin) {
            if self.is_current(cx)
                && let Some(message_id) = self
                    .page
                    .as_ref()
                    .and_then(|page| page.meta.origin_message_id.clone())
            {
                cx.emit(SideChatEvent::RevealOrigin {
                    parent: self.parent.clone(),
                    message_id,
                    engine_generation: self.engine_generation,
                });
            }
            return;
        }
        if self.busy || !self.is_current(cx) {
            return;
        }
        let Some(engine) = self.engine.clone() else {
            self.error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let action = if matches!(action, PanelAction::Send) && self.pending_question().is_some() {
            PanelAction::Answer(self.input.read(cx).text().to_string())
        } else {
            action
        };
        let offer_snapshot = matches!(&action, PanelAction::New);
        let (method, params) = match action.clone() {
            PanelAction::RevealOrigin | PanelAction::Back => return,
            PanelAction::Answer(_) | PanelAction::Approve(_) => {
                let Some((request_id, questions)) = self.pending_question() else {
                    return;
                };
                let Some(page) = &self.page else {
                    return;
                };
                if self.lease.is_none() || page.meta.state != "active" {
                    return;
                }
                let decision = if let PanelAction::Approve(decision) = action {
                    Some(decision)
                } else {
                    None
                };
                if decision.is_none() {
                    if questions.iter().any(|question| question.approval.is_some()) {
                        return;
                    }
                    let PanelAction::Answer(answer) = action else {
                        return;
                    };
                    if answer.trim().is_empty() {
                        return;
                    }
                    if let Some(question) = questions
                        .iter()
                        .find(|question| !self.answers.contains_key(&question.id))
                    {
                        self.answers.insert(question.id.clone(), vec![answer]);
                    }
                    self.input.update(cx, |input, cx| input.set_text("", cx));
                    if self.answers.len() < questions.len() {
                        cx.notify();
                        return;
                    }
                }
                (
                    if questions.iter().any(|question| {
                        question
                            .approval
                            .as_ref()
                            .is_some_and(|approval| approval.is_luna_cross_task())
                    }) {
                        "LunaSideLocalApproval"
                    } else {
                        methods::ANSWER_SIDE_CHAT
                    },
                    json!({"id":page.meta.id,"lease":self.lease,"requestId":request_id,"decision":decision,
                    "answers":questions.iter().filter_map(|question| self.answers.get(&question.id).map(|labels| luna_proto::UserInputAnswer { question_id:question.id.clone(), labels:labels.clone() })).collect::<Vec<_>>() }),
                )
            }
            PanelAction::New | PanelAction::NewSnapshot => {
                if self.page.is_some() {
                    return;
                }
                (
                    methods::CREATE_SIDE_CHAT,
                    json!({"parentId":self.parent,"requestId":uuid::Uuid::new_v4().to_string(),"originMessageId":self.origin.as_ref().map(|origin| &origin.0),"anchorText":self.origin.as_ref().map(|origin| &origin.1),"forceSnapshot":matches!(action, PanelAction::NewSnapshot)}),
                )
            }
            PanelAction::Open(id) => {
                self.before = None;
                // Reopening a parked side chat resumes its lease.
                self.lease = self.parked.remove(&id);
                (methods::READ_SIDE_CHAT, json!({"id":id,"lease":self.lease}))
            }
            PanelAction::Send => {
                let Some(page) = self
                    .page
                    .as_ref()
                    .filter(|page| page.meta.state == "active" && !page.working)
                else {
                    return;
                };
                if self.lease.is_none() || self.input.read(cx).is_empty() {
                    return;
                }
                (
                    methods::SEND_SIDE_CHAT,
                    json!({"id":page.meta.id,"lease":self.lease,"messageId":uuid::Uuid::new_v4().to_string(),"text":self.input.read(cx).text()}),
                )
            }
            PanelAction::Stop => {
                let Some(page) = self.page.as_ref().filter(|page| page.working) else {
                    return;
                };
                if self.lease.is_none() {
                    return;
                }
                (
                    methods::STOP_SIDE_CHAT,
                    json!({"id":page.meta.id,"lease":self.lease}),
                )
            }
            PanelAction::Unload => {
                let Some(page) = &self.page else {
                    return;
                };
                (
                    methods::UNLOAD_SIDE_CHAT,
                    json!({"id":page.meta.id,"lease":self.lease}),
                )
            }
            PanelAction::MoreHistory => (
                methods::LIST_SIDE_CHATS,
                json!({"parentId":self.parent,"before":self.history.as_ref().and_then(|history| history.before)}),
            ),
            PanelAction::Older | PanelAction::Latest | PanelAction::Refresh => {
                if let Some(page) = &self.page {
                    match action {
                        PanelAction::Older => self.before = page.before,
                        PanelAction::Latest => self.before = None,
                        _ => {}
                    }
                    (
                        methods::READ_SIDE_CHAT,
                        json!({"id":page.meta.id,"lease":self.lease,"before":self.before}),
                    )
                } else {
                    (methods::LIST_SIDE_CHATS, json!({"parentId":self.parent}))
                }
            }
        };
        let submitted_text = matches!(
            method,
            methods::SEND_SIDE_CHAT | methods::ANSWER_SIDE_CHAT | "LunaSideLocalApproval"
        )
        .then(|| self.input.read(cx).text().to_string());
        self.operation_label = match method {
            methods::CREATE_SIDE_CHAT => Some("Creating side chat…"),
            methods::SEND_SIDE_CHAT => Some("Sending…"),
            methods::UNLOAD_SIDE_CHAT => Some("Stopping work and saving…"),
            methods::STOP_SIDE_CHAT => Some("Stopping the side chat's turn…"),
            methods::LIST_SIDE_CHATS if self.history.is_none() => Some("Loading saved side chats…"),
            _ => None,
        };
        self.busy = true;
        self.error = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = Tokio::spawn(cx, async move {
                tokio::time::timeout(Duration::from_secs(45), async move {
                let value = if method == "LunaSideLocalApproval" {
                    let id = params["id"].as_str().unwrap_or_default();
                    let page: SideChatPage = engine
                        .client()
                        .call_as(
                            methods::READ_SIDE_CHAT,
                            json!({"id":id,"lease":params["lease"]}),
                        )
                        .await?;
                    if page.meta.state != "active" {
                        return Err(luna_rpc::RpcError::Failed(
                            "This side chat is read-only".into(),
                        ));
                    }
                    let decision: luna_proto::ApprovalDecision =
                        serde_json::from_value(params["decision"].clone())
                            .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))?;
                    let outcome = engine
                        .respond_luna_local_ui_approval(
                            id,
                            params["requestId"].as_str().unwrap_or_default(),
                            decision,
                        )
                        .await
                        .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))?;
                    if !matches!(
                        outcome,
                        luna_engine::sessions::LunaApprovalOutcome::AllowedOnce
                            | luna_engine::sessions::LunaApprovalOutcome::Denied
                    ) {
                        return Err(luna_rpc::RpcError::Failed(format!(
                            "Approval was not applied: {outcome:?}"
                        )));
                    }
                    json!({"resolved":true})
                } else {
                    engine.client().call(method, params).await?
                };
                if method == methods::CREATE_SIDE_CHAT {
                    let created: SideChatCreated = serde_json::from_value(value)
                        .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))?;
                    let page = engine
                        .client()
                        .call(
                            methods::READ_SIDE_CHAT,
                            json!({"id":created.meta.id,"lease":created.lease}),
                        )
                        .await?;
                    Ok(json!({"created":created,"page":page}))
                } else {
                    Ok::<Value, luna_rpc::RpcError>(value)
                }
                }).await.map_err(|_| luna_rpc::RpcError::Failed("Side chat response timed out. The outcome is unknown; inspect saved history before trying another send.".into()))?
            })
            .await;
            this.update(cx, |panel, cx| {
                if !panel.is_current(cx) {
                    return;
                }
                panel.busy = false;
                panel.operation_label = None;
                let value = match result {
                    Ok(Ok(value)) => value,
                    Ok(Err(error)) => {
                        panel.error = Some(error.to_string());
                        panel.snapshot_alternative = offer_snapshot;
                        cx.notify();
                        return;
                    }
                    Err(error) => {
                        panel.error = Some(error.to_string());
                        panel.snapshot_alternative = offer_snapshot;
                        cx.notify();
                        return;
                    }
                };
                let applied: anyhow::Result<()> = (|| {
                    match method {
                        methods::CREATE_SIDE_CHAT => {
                            let created: SideChatCreated =
                                serde_json::from_value(value["created"].clone())?;
                            panel.lease = created.lease;
                            panel.snapshot_alternative = false;
                            panel.focus_pending = panel.lease.is_some();
                            panel.apply_page(serde_json::from_value(value["page"].clone())?, cx);
                        }
                        methods::READ_SIDE_CHAT => {
                            panel.apply_page(serde_json::from_value(value)?, cx)
                        }
                        methods::LIST_SIDE_CHATS => {
                            panel.history = Some(serde_json::from_value(value)?)
                        }
                        methods::STOP_SIDE_CHAT => {
                            panel.before = None;
                            panel.action(PanelAction::Refresh, cx);
                        }
                        "LunaSideLocalApproval"
                        | methods::ANSWER_SIDE_CHAT
                        | methods::SEND_SIDE_CHAT => {
                            panel.answers.clear();
                            if submitted_text.as_deref().is_some_and(|text| panel.input.read(cx).text() == text) {
                                panel.input.update(cx, |input, cx| input.set_text("", cx));
                            }
                            panel.before = None;
                            panel.action(PanelAction::Refresh, cx);
                        }
                        methods::UNLOAD_SIDE_CHAT => {
                            if let Some(page) = panel.page.take() {
                                panel.state.update(cx, |state, _| {
                                    state.unwatch_subagent_doc(&page.meta.id)
                                });
                            }
                            panel.transcript = None;
                            panel.transcript_events = None;
                            panel.full_view = None;
                            panel.origin = None;
                            panel.lease = None;
                            panel.input.update(cx, |input, cx| input.set_text("", cx));
                            panel.action(PanelAction::Refresh, cx);
                        }
                        _ => {}
                    }
                    Ok(())
                })();
                if let Err(error) = applied {
                    panel.error = Some(error.to_string());
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn apply_page(&mut self, page: SideChatPage, cx: &mut Context<Self>) {
        let changed = self
            .page
            .as_ref()
            .is_none_or(|old| old.meta.id != page.meta.id || old.entries != page.entries);
        if self
            .page
            .as_ref()
            .is_none_or(|old| old.meta.id != page.meta.id)
        {
            if let Some(old) = &self.page {
                self.state
                    .update(cx, |state, _| state.unwatch_subagent_doc(&old.meta.id));
            }
            self.transcript_events = None;
            self.transcript = Some(cx.new(|cx| {
                Transcript::for_doc(self.state.clone(), page.meta.id.clone(), false, cx)
            }));
        }
        if self.transcript_events.is_none()
            && let Some(transcript) = &self.transcript
        {
            transcript.update(cx, |view, _| view.allow_side_chat_handoff());
            self.transcript_events = Some(cx.subscribe(transcript, |panel, _, event, cx| {
                if let TranscriptEvent::SideChatResult {
                    side_chat_id,
                    message_id,
                    text,
                } = event
                    && panel.is_current(cx)
                    && panel.page.as_ref().is_some_and(|page| {
                        page.meta.id == *side_chat_id
                            && page.entries.iter().any(|entry| entry.id == *message_id)
                    })
                {
                    cx.emit(SideChatEvent::ResultToParent {
                        parent: panel.parent.clone(),
                        engine_generation: panel.engine_generation,
                        text: text.clone(),
                    });
                }
            }));
        }
        // A first-class side chat's live doc watch owns its rows; a page
        // snapshot would replace (and stop) that watch.
        let live = self.state.read(cx).pane_chat(&page.meta.id).is_some();
        if changed && !live {
            self.state.update(cx, |state, cx| {
                state.set_subagent_snapshot(page.meta.id.clone(), page.entries.clone());
                cx.notify();
            });
        }
        if !matches!(
            page.meta.state.as_str(),
            "active" | "closing" | "closeFailed"
        ) {
            self.lease = None;
        }
        self.page = Some(page);
    }

    /// The open side chat in the shared pane chat layout: identity header,
    /// the main transcript, and a composer-styled input. Side chats run on
    /// the engine's leased side-chat RPCs, so the composer offers exactly
    /// what they support (text and approvals) and says so.
    fn render_page(
        &mut self,
        page: SideChatPage,
        window: &mut Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let writable = page.meta.state == "active" && self.lease.is_some();
        if writable && side_chat_first_class(self.state.read(cx), &page.meta.id) {
            return self.render_full_view(&page, theme, cx);
        }
        self.full_view = None;
        if writable && self.focus_pending {
            window.focus(&self.input.focus_handle(cx), cx);
            self.focus_pending = false;
        }
        let context = if page.meta.context_mode == "nativeFork" {
            "Native fork"
        } else {
            "Context snapshot"
        };
        let provider = super::child_chats::provider_label(page.meta.config.harness);
        let model = page
            .meta
            .config
            .model
            .clone()
            .filter(|model| !model.is_empty())
            .unwrap_or_else(|| "Provider default".into());
        let status = if writable {
            let minutes = ((page.meta.expires_at_ms - Utc::now().timestamp_millis()).max(0)
                + 59_999)
                / 60_000;
            (
                SharedString::from(format!("Writable · {minutes} min left")),
                theme.success,
                false,
            )
        } else {
            (
                SharedString::from(format!("Read-only · {}", state_label(&page.meta.state))),
                theme.readable_muted(0.6),
                false,
            )
        };
        // Like a main chat, a side chat is named by its first question; the
        // engine's stored title only repeats "Side chat · <parent>", which
        // the identity badge and parent link already show.
        let title = page
            .entries
            .iter()
            .find(|entry| entry.role == luna_doc::MessageRole::User)
            .and_then(|entry| {
                entry.parts.iter().find_map(|part| match part {
                    luna_doc::MessagePart::Text { text, .. } => Some(transcript::single_line(text)),
                    _ => None,
                })
            })
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "New side chat".into());
        let chrome = PaneChatChrome {
            identity: PaneIdentity::Side,
            title: title.into(),
            parent_title: transcript::single_line(&self.parent_title).into(),
            status: Some(status),
            meta: Some(format!("{provider} · {model} · {context}").into()),
        };
        let has_origin = page.meta.origin_message_id.is_some();
        let header = super::pane_chat::render_pane_header(
            &chrome,
            PaneHeaderActions {
                back_label: "Side chats",
                back: Rc::new(|panel: &mut Self, cx| panel.action(PanelAction::Back, cx)),
                open_main: None,
                show_parent: has_origin.then(|| {
                    Rc::new(|panel: &mut Self, cx: &mut Context<Self>| {
                        panel.action(PanelAction::RevealOrigin, cx)
                    }) as Rc<dyn Fn(&mut Self, &mut Context<Self>)>
                }),
                extra: Vec::new(),
            },
            theme,
            cx,
        );

        let mut notices: Vec<AnyElement> = Vec::new();
        if let Some(label) = self.operation_label {
            notices.push(
                div()
                    .px(px(16.0))
                    .pt(px(8.0))
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_muted)
                    .child(label)
                    .into_any_element(),
            );
        }
        if let Some(error) = self.error.clone() {
            notices.push(
                div()
                    .px(px(12.0))
                    .pt(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(crate::settings::widgets::error_strip(theme, error))
                    .child(self.button(
                        "side-refresh",
                        "Refresh saved state",
                        PanelAction::Refresh,
                        cx,
                    ))
                    .into_any_element(),
            );
        }
        if let Some(quote) = page.meta.anchor_text.clone() {
            notices.push(
                div()
                    .mx(px(16.0))
                    .mt(px(10.0))
                    .max_h(px(72.0))
                    .overflow_hidden()
                    .border_l_2()
                    .border_color(theme.border_strong)
                    .pl(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(quote)
                    .into_any_element(),
            );
        }
        if let Some(detail) = page.meta.detail.clone() {
            notices.push(
                div()
                    .px(px(16.0))
                    .pt(px(8.0))
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_muted)
                    .child(detail)
                    .into_any_element(),
            );
        }
        if page.before.is_some() || self.before.is_some() {
            notices.push(
                div()
                    .px(px(12.0))
                    .pt(px(8.0))
                    .flex()
                    .gap(px(6.0))
                    .when(page.before.is_some(), |el| {
                        el.child(self.button(
                            "side-older",
                            "Older messages",
                            PanelAction::Older,
                            cx,
                        ))
                    })
                    .when(self.before.is_some(), |el| {
                        el.child(self.button(
                            "side-latest",
                            "Latest messages",
                            PanelAction::Latest,
                            cx,
                        ))
                    })
                    .into_any_element(),
            );
        }

        let transcript: AnyElement = match &self.transcript {
            Some(transcript) if !page.entries.is_empty() => transcript.clone().into_any_element(),
            _ => div()
                .p(px(16.0))
                .text_color(theme.text_muted)
                .child(if writable {
                    "Ask a question here. Your parent conversation stays unchanged."
                } else {
                    "This side chat has no saved messages."
                })
                .into_any_element(),
        };

        let composer = if writable {
            self.render_side_composer(&page, &model, provider, theme, cx)
        } else {
            div()
                .id("side-read-only")
                .w_full()
                .flex_none()
                .px(px(16.0))
                .py(px(12.0))
                .border_t_1()
                .border_color(theme.border)
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child(format!(
                    "Read-only · {} · saved transcript. Start a new side chat to continue.",
                    state_label(&page.meta.state)
                ))
                .into_any_element()
        };
        div()
            .id("side-chat-page")
            .size_full()
            .flex()
            .flex_col()
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text)
            .child(header)
            .child(super::pane_chat::render_pane_body(
                notices, transcript, false, composer, theme,
            ))
            .into_any_element()
    }

    /// A first-class side chat: the same full view as a child chat (main
    /// transcript + main composer bound to the side chat), with the side
    /// chat's lifetime as its status and Back unloading it.
    fn render_full_view(
        &mut self,
        page: &SideChatPage,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stale = self.full_view.as_ref().is_none_or(|(view, _)| {
            view.read(cx).composer().read(cx).readback(cx)["boundChat"] != page.meta.id.as_str()
        });
        if stale {
            let composer = self
                .composers
                .update(cx, |composers, cx| composers.composer(&page.meta.id, cx));
            let state = self.state.clone();
            let side = page.meta.id.clone();
            let parent = self.parent.clone();
            let links = self.links.clone();
            let view = cx.new(|cx| {
                PaneChatView::new(
                    state,
                    side,
                    PaneIdentity::Side,
                    parent,
                    "Side chats",
                    composer,
                    links,
                    cx,
                )
            });
            let events = cx.subscribe(&view, |panel, _, event, cx| match event {
                PaneChatEvent::Back => panel.action(PanelAction::Back, cx),
                PaneChatEvent::Unload => panel.action(PanelAction::Unload, cx),
                PaneChatEvent::ShowParent(_) => panel.action(PanelAction::RevealOrigin, cx),
                PaneChatEvent::OpenAsMain(_) => {}
            });
            // "Use in parent" on a side chat answer, as in the leased view.
            let transcript = view.read(cx).transcript().clone();
            transcript.update(cx, |view, _| view.allow_side_chat_handoff());
            self.transcript_events = Some(cx.subscribe(&transcript, |panel, _, event, cx| {
                if let TranscriptEvent::SideChatResult {
                    side_chat_id,
                    message_id,
                    text,
                } = event
                    && panel.is_current(cx)
                    && panel
                        .page
                        .as_ref()
                        .is_some_and(|page| page.meta.id == *side_chat_id)
                    && panel
                        .state
                        .read(cx)
                        .sub_transcript(side_chat_id)
                        .iter()
                        .any(|entry| entry.id == *message_id)
                {
                    cx.emit(SideChatEvent::ResultToParent {
                        parent: panel.parent.clone(),
                        engine_generation: panel.engine_generation,
                        text: text.clone(),
                    });
                }
            }));
            self.transcript = None;
            self.full_view = Some((view, events));
        }
        let minutes =
            ((page.meta.expires_at_ms - Utc::now().timestamp_millis()).max(0) + 59_999) / 60_000;
        let status = (
            SharedString::from(match self.operation_label {
                Some(label) => label.to_string(),
                None => format!("Writable · {minutes} min left"),
            }),
            theme.success,
        );
        let (view, _) = self.full_view.as_ref().expect("full view just created");
        let view = view.clone();
        view.update(cx, |view, cx| view.set_status(Some(status), cx));
        view.into_any_element()
    }

    /// The side chat's input in the main composer's pill chrome. Text and
    /// approvals only: the leased side-chat RPCs carry no attachments,
    /// queue/steer, model change or goals, and the composer says so.
    fn render_side_composer(
        &mut self,
        page: &SideChatPage,
        model: &str,
        provider: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let working = page.working;
        let status: &'static str = match page.status {
            Some(luna_proto::SessionStatus::AwaitingInput) => "Waiting for input",
            Some(luna_proto::SessionStatus::AwaitingApproval) => "Approval needed",
            Some(luna_proto::SessionStatus::Errored) => "Provider error",
            // Only user-visible operations; the 2 s history poll stays quiet.
            _ if self.operation_label.is_some() => self.operation_label.unwrap_or_default(),
            _ if working => "Working…",
            _ => "",
        };
        let blocked = self.busy || working || self.input.read(cx).is_empty();
        let question_card = self.render_side_question(theme, cx);
        let muted = theme.text_muted;
        let (brand, brand_color) = crate::pickers::harness_brand_icon(page.meta.config.harness);
        let chip = |icon_path: &'static str, tint: Option<gpui::Hsla>, label: String| {
            div()
                .h(px(28.0))
                .min_w_0()
                .px(px(6.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .rounded(px(8.0))
                .text_size(crate::typography::ui_rems(12.5))
                .text_color(muted)
                .child(
                    icon(icon_path)
                        .size(px(14.0))
                        .flex_none()
                        .text_color(tint.unwrap_or(muted)),
                )
                .child(div().min_w_0().truncate().child(label))
        };
        let stop = working.then(|| {
            div()
                .id("side-stop")
                .role(gpui::Role::Button)
                .aria_label("Stop the side chat's current turn")
                .tab_index(0)
                .size(px(28.0))
                .flex_none()
                .rounded_full()
                .bg(theme.text)
                .flex()
                .items_center()
                .justify_center()
                .when(self.busy, |el| el.opacity(0.35))
                .when(!self.busy, |el| {
                    el.cursor_pointer()
                        .hover(|style| style.opacity(0.85))
                        .on_click(
                            cx.listener(|panel, _, _, cx| panel.action(PanelAction::Stop, cx)),
                        )
                })
                .child(div().size(px(11.0)).rounded(px(3.0)).bg(theme.bg))
        });
        let send = div()
            .id("side-send")
            .role(gpui::Role::Button)
            .aria_label("Send to side chat")
            .tab_index(0)
            .size(px(28.0))
            .flex_none()
            .rounded_full()
            .bg(theme.text)
            .flex()
            .items_center()
            .justify_center()
            .when(blocked, |el| el.opacity(0.35))
            .when(!blocked, |el| {
                el.cursor_pointer()
                    .hover(|style| style.opacity(0.85))
                    .on_click(cx.listener(|panel, _, _, cx| panel.action(PanelAction::Send, cx)))
            })
            .child(icon(icons::ARROW_UP).size(px(14.0)).text_color(theme.bg));
        let pill = crate::composer::pane_composer_surface(theme)
            .id("side-composer-surface")
            .w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .min_h(px(44.0))
                    .max_h(px(160.0))
                    .px(px(16.0))
                    .pt(px(12.0))
                    .pb(px(4.0))
                    .overflow_hidden()
                    .child(self.input.clone()),
            )
            .child(
                div()
                    .h(px(46.0))
                    .px(px(12.0))
                    .pb(px(10.0))
                    .pt(px(2.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(chip(brand, brand_color, format!("{provider} · {model}")))
                            .child(chip(
                                icons::KEY_MINIMALISTIC,
                                None,
                                "Parent's access".into(),
                            )),
                    )
                    .when(!status.is_empty(), |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(crate::typography::ui_rems(11.5))
                                .text_color(theme.readable_muted(0.6))
                                .child(status),
                        )
                    })
                    .map(|el| match stop {
                        Some(stop) => el.child(stop),
                        None => el.child(send),
                    }),
            );
        div()
            .id("side-composer")
            .w_full()
            .flex_none()
            .px(px(12.0))
            .pt(px(4.0))
            .pb(px(10.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .children(question_card)
            .child(pill)
            .child(
                div()
                    .px(px(4.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.readable_muted(0.55))
                    .child("Side chat · text and approvals. It inherits the parent's checkout, model and access; attachments, queue and goals belong to full chats."),
            )
            .into_any_element()
    }

    /// A pending question or approval in the composer's card chrome.
    fn render_side_question(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (_, questions) = self.pending_question()?;
        let question = questions
            .iter()
            .find(|question| !self.answers.contains_key(&question.id))?
            .clone();
        let mut card = crate::composer::pane_composer_surface(theme)
            .id("side-question-card")
            .w_full()
            .px(px(16.0))
            .py(px(12.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.readable_muted(0.65))
                    .child(question.header.to_uppercase()),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(13.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(question.question.clone()),
            );
        if let Some(approval) = &question.approval {
            let (label, value) = crate::composer::approval_expiry_row(approval);
            card = card.child(
                div()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_muted)
                    .child(format!("{label}: {value}")),
            );
        }
        let mut buttons = div().flex().flex_wrap().gap(px(6.0)).pt(px(4.0));
        if let Some(details) = question
            .approval
            .as_ref()
            .and_then(|approval| approval.details.as_ref())
        {
            for (label, value) in details.display_rows() {
                card = card.child(
                    div()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.text_muted)
                        .child(format!("{label}: {value}")),
                );
            }
            for (index, choice) in details.choices.iter().enumerate() {
                buttons = buttons.child(self.button(
                    format!("side-approval-{index}"),
                    choice.label.clone(),
                    PanelAction::Approve(luna_proto::ApprovalDecision::ProviderChoice {
                        index: index as u32,
                    }),
                    cx,
                ));
            }
        } else if question.approval.is_some() {
            buttons = buttons
                .child(self.button(
                    "side-allow",
                    "Allow once",
                    PanelAction::Approve(luna_proto::ApprovalDecision::AllowOnce),
                    cx,
                ))
                .child(self.button(
                    "side-deny",
                    "Deny",
                    PanelAction::Approve(luna_proto::ApprovalDecision::Deny),
                    cx,
                ));
        } else {
            for (index, option) in question.options.iter().enumerate() {
                buttons = buttons.child(self.button(
                    format!("side-answer-{index}"),
                    option.clone(),
                    PanelAction::Answer(option.clone()),
                    cx,
                ));
            }
        }
        Some(card.child(buttons).into_any_element())
    }

    fn button(
        &self,
        id: impl Into<SharedString>,
        title: impl Into<SharedString>,
        action: PanelAction,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let title = title.into();
        let key_action = action.clone();
        div()
            .id(id.into())
            .role(gpui::Role::Button)
            .aria_label(title.clone())
            .tab_index(0)
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .text_color(theme.text)
            .text_size(crate::typography::ui_rems(12.0))
            .hover(move |style| style.bg(theme.surface_raised))
            .focus_visible(move |style| style.border_color(theme.border_strong))
            .when(self.busy, |style| style.opacity(0.5))
            .child(title)
            .on_click(cx.listener(move |panel, _, _, cx| panel.action(action.clone(), cx)))
            .on_key_down(
                cx.listener(move |panel, event: &gpui::KeyDownEvent, _, cx| {
                    if is_settings_nav_activation(&event.keystroke.key, event.is_held) {
                        panel.action(key_action.clone(), cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .into_any_element()
    }
}

impl Render for SideChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        if let Some(page) = self.page.clone() {
            return self.render_page(page, window, &theme, cx);
        }
        let mut root = div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text)
            .child(
                crate::surface_chrome::toolbar(&theme).child(
                    div()
                        .truncate()
                        .child(format!("Side chats · {}", self.parent_title)),
                ),
            );
        root = root.child(
            div()
                .h(px(24.0))
                .flex_none()
                .px(px(16.0))
                .text_color(theme.text_muted)
                .text_size(crate::typography::ui_rems(11.0))
                .child(self.operation_label.unwrap_or("")),
        );
        if let Some(error) = self.error.clone() {
            root = root
                .child(
                    div()
                        .px(px(16.0))
                        .child(crate::settings::widgets::error_strip(&theme, error)),
                )
                .child(self.button(
                    "side-refresh",
                    "Refresh saved state",
                    PanelAction::Refresh,
                    cx,
                ));
        }
        root = root.child(div().p(px(16.0)).flex().flex_col().gap(px(12.0))
            .child(self.button("side-new", "Start side chat", PanelAction::New, cx))
            .when(self.snapshot_alternative, |el| el.child(self.button("side-new-snapshot", "Start with recent context", PanelAction::NewSnapshot, cx)))
            .child(self.button("side-refresh-history", "Refresh history", PanelAction::Refresh, cx))
            .child(div().text_color(theme.text_muted).child("A separate conversation with this parent's context. Writable for two hours; saved history remains available."))
            .child(div().text_color(theme.text_muted).child("Previous side chats · open to read")));
        let mut list = div()
            .id("side-history")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(16.0))
            .flex()
            .flex_col()
            .gap(px(8.0));
        if let Some(history) = &self.history {
            if history.items.is_empty() {
                list = list.child(
                    div()
                        .text_color(theme.text_muted)
                        .child("No previous side chats"),
                );
            }
            for meta in &history.items {
                let stamp = chrono::DateTime::from_timestamp_millis(meta.created_at_ms)
                    .map(|date| {
                        date.with_timezone(&chrono::Local)
                            .format("%b %d · %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default();
                list = list.child(self.button(
                    format!("side-history-{}", meta.id),
                    format!(
                        "{stamp} · {} · {} messages",
                        state_label(&meta.state),
                        meta.message_count
                    ),
                    PanelAction::Open(meta.id.clone()),
                    cx,
                ));
            }
            if history.before.is_some() {
                list = list.child(self.button(
                    "side-more-history",
                    "Earlier side chats",
                    PanelAction::MoreHistory,
                    cx,
                ));
            }
        } else {
            list = list.child("Loading saved side chats…");
        }
        root = root.child(list);
        root.into_any_element()
    }
}

impl Shell {
    pub(super) fn release_side_chat(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.side_chat_panel.take() else {
            return;
        };
        self.side_chat_events = None;
        let request = panel.read(cx).release_request();
        // Parked first-class side chats end with the panel, quietly: their
        // leases would lapse within 30 s anyway, this just saves promptly.
        for (engine, id, lease) in panel.read(cx).parked_release_requests() {
            cx.spawn(async move |_, cx| {
                let _ = Tokio::spawn(cx, async move {
                    tokio::time::timeout(
                        Duration::from_secs(20),
                        engine
                            .client()
                            .call(methods::UNLOAD_SIDE_CHAT, json!({"id": id, "lease": lease})),
                    )
                    .await
                })
                .await;
            })
            .detach();
        }
        drop(panel);
        if let Some((engine, id, lease)) = request {
            let progress = SidebarNotice::status("Stopping side chat and saving its transcript…");
            let notice_id = progress.id;
            self.sidebar_notice = Some(progress);
            cx.spawn(async move |this, cx| {
                let result = Tokio::spawn(cx, async move {
                    tokio::time::timeout(Duration::from_secs(20), engine.client().call(methods::UNLOAD_SIDE_CHAT, json!({"id":id,"lease":lease}))).await
                        .map_err(|_| luna_rpc::RpcError::Failed("Unload response timed out; the view lease remains the cancellation backstop.".into()))?
                }).await;
                this.update(cx, |shell, cx| {
                    if shell.sidebar_notice.as_ref().is_none_or(|notice| notice.id != notice_id) {
                        return;
                    }
                    shell.sidebar_notice = match result {
                        Ok(Ok(_)) => None,
                        Ok(Err(error)) => Some(format!("Side chat unload incomplete: {error}. Inspect Previous side chats.").into()),
                        Err(error) => Some(format!("Side chat unload readback unavailable: {error}. The view lease will stop abandoned work.").into()),
                    };
                    cx.notify();
                }).ok();
            }).detach();
        }
    }

    pub(super) fn create_side_chat_panel(
        &mut self,
        origin: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let links = Self::session_links(Some(self.active_chat.clone()), cx);
        let panel = cx.new(|cx| {
            SideChatPanel::new(
                self.state.clone(),
                self.pane_composers.clone(),
                links,
                self.active_chat.clone(),
                origin,
                cx,
            )
        });
        self.side_chat_events = Some(cx.subscribe(&panel, |shell, _, event, cx| {
            let (parent, engine_generation, text) = match event {
                SideChatEvent::RevealOrigin {
                    parent,
                    message_id,
                    engine_generation,
                } => {
                    if shell.active_chat == *parent
                        && shell.state.read(cx).engine_generation() == *engine_generation
                    {
                        if shell.right_pane_expanded {
                            shell.toggle_right_pane_expand(cx);
                        }
                        shell.transcript.update(cx, |transcript, cx| {
                            transcript.navigate_to_message(parent.clone(), message_id.clone(), cx)
                        });
                    }
                    return;
                }
                SideChatEvent::ResultToParent {
                    parent,
                    engine_generation,
                    text,
                } => (parent, engine_generation, text),
            };
            if shell.active_chat != *parent
                || shell.state.read(cx).engine_generation() != *engine_generation
            {
                return;
            }
            let inserted = shell.composer.update(cx, |composer, cx| {
                composer.insert_side_chat_result(parent, text, *engine_generation, cx)
            });
            shell.sidebar_notice = Some(SidebarNotice::status(if inserted {
                "Added to the parent draft. Review it and press Send."
            } else {
                "Parent draft is loading or being submitted. Try again when it is ready."
            }));
            cx.notify();
        }));
        self.side_chat_panel = Some(panel);
    }

    pub(super) fn render_side_chats(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.side_chat_panel.is_none() {
            self.create_side_chat_panel(None, cx);
        }
        self.side_chat_panel
            .as_ref()
            .unwrap()
            .clone()
            .into_any_element()
    }
}

/// Headless QA drives the production side chat panel through these hooks.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_open_side_chats(&mut self, cx: &mut Context<Self>) {
        self.add_work_surface(RightSurface::SideChats, cx);
        if self.side_chat_panel.is_none() {
            self.create_side_chat_panel(None, cx);
        }
    }

    /// `snapshot` (start with recent context), `new`, `type`, `send`, `back`,
    /// `stop`, `refresh`, or `open` (the first saved side chat).
    pub fn fixture_side_chat(&mut self, action: &str, text: &str, cx: &mut Context<Self>) {
        let Some(panel) = self.side_chat_panel.clone() else {
            return;
        };
        panel.update(cx, |panel, cx| {
            let action = match action {
                "snapshot" => PanelAction::NewSnapshot,
                "new" => PanelAction::New,
                "back" => PanelAction::Back,
                "stop" => PanelAction::Stop,
                "refresh" => PanelAction::Refresh,
                "open" => match panel
                    .history
                    .as_ref()
                    .and_then(|history| history.items.first())
                {
                    Some(meta) => PanelAction::Open(meta.id.clone()),
                    None => return,
                },
                "type_full" | "send_full" => {
                    let Some(composer) = panel
                        .full_view
                        .as_ref()
                        .map(|(view, _)| view.read(cx).composer().clone())
                    else {
                        return;
                    };
                    let input = composer.read(cx).input.clone();
                    input.update(cx, |input, cx| {
                        input.set_text(text, cx);
                        if action == "send_full" {
                            cx.emit(crate::composer::ComposerInputEvent::Submitted);
                        }
                    });
                    return;
                }
                "pick_other_provider" => {
                    let Some(composer) = panel
                        .full_view
                        .as_ref()
                        .map(|(view, _)| view.read(cx).composer().clone())
                    else {
                        return;
                    };
                    let pickers = composer.read(cx).pickers().clone();
                    pickers.update(cx, |pickers, cx| {
                        pickers.fixture_pick_harness(HarnessId::Codex, cx)
                    });
                    return;
                }
                "unload" => PanelAction::Unload,
                "type" | "send" => {
                    let input = panel.input.clone();
                    input.update(cx, |input, cx| input.set_text(text, cx));
                    if action == "type" {
                        return;
                    }
                    PanelAction::Send
                }
                _ => return,
            };
            panel.action(action, cx);
        });
    }

    pub fn fixture_side_chat_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        let Some(panel) = self.side_chat_panel.as_ref().map(|panel| panel.read(cx)) else {
            return serde_json::Value::Null;
        };
        let page = panel.page.as_ref();
        serde_json::json!({
            "pageId": page.map(|page| page.meta.id.clone()),
            "state": page.map(|page| page.meta.state.clone()),
            "contextMode": page.map(|page| page.meta.context_mode.clone()),
            "writable": page.is_some_and(|page| page.meta.state == "active") && panel.lease.is_some(),
            "working": page.map(|page| page.working),
            "busy": panel.busy,
            "error": panel.error,
            "input": panel.input.read(cx).text().to_string(),
            "question": panel.pending_question().map(|(id, _)| id),
            "historyCount": panel.history.as_ref().map(|history| history.items.len()),
            "historyStates": panel.history.as_ref().map(|history| history.items.iter().map(|meta| meta.state.clone()).collect::<Vec<_>>()),
            "texts": page.map(|page| page.entries.iter().flat_map(|entry| entry.parts.iter()).filter_map(|part| match part {
                luna_doc::MessagePart::Text { text, .. } => Some(text.clone()),
                _ => None,
            }).collect::<Vec<_>>()),
            "parentTranscriptRows": self.state.read(cx).transcript.len(),
            "mainComposer": self.composer.read(cx).readback(cx),
            "firstClass": page.is_some_and(|page| side_chat_first_class(self.state.read(cx), &page.meta.id)),
            "parked": panel.parked.keys().cloned().collect::<Vec<_>>(),
            "lastRenewAgoMs": panel.last_renew.elapsed().as_millis() as u64,
            "pickerRefusal": panel.full_view.as_ref().and_then(|(view, _)| view.read(cx).composer().read(cx).pickers().read(cx).fixture_refusals()),
            "liveTexts": page.map(|page| self.state.read(cx).sub_transcript(&page.meta.id).iter().flat_map(|entry| entry.parts.iter()).filter_map(|part| match part {
                luna_doc::MessagePart::Text { text, .. } => Some(text.clone()),
                _ => None,
            }).collect::<Vec<_>>()),
            "paneChats": self.state.read(cx).pane_chat_ids(),
            "watchedDocs": self.state.read(cx).fixture_sub_watch_ids(),
            "fullComposer": panel.full_view.as_ref().map(|(view, _)| view.read(cx).composer().read(cx).readback(cx)),
            "paneTranscript": panel.full_view.as_ref().map(|(view, _)| view.read(cx).transcript().read(cx).scroll_readback()),
        })
    }
}
