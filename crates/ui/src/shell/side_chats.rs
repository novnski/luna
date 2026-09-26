use super::*;
use crate::state::EngineHandle;
use luna_engine::side_chats::{SideChatCreated, SideChatList, SideChatPage};
use serde_json::{Value, json};

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

pub(super) struct SideChatPanel {
    state: Entity<AppState>,
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
                        if panel.lease.is_some() && !panel.busy {
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
            PanelAction::RevealOrigin => return,
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
                (methods::READ_SIDE_CHAT, json!({"id":id}))
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
        if changed {
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
                        .py(px(10.0))
                        .text_color(theme.text)
                        .child(error),
                )
                .child(self.button(
                    "side-refresh",
                    "Refresh saved state",
                    PanelAction::Refresh,
                    cx,
                ));
        }
        if let Some(page) = self.page.clone() {
            let writable = page.meta.state == "active" && self.lease.is_some();
            if writable && self.focus_pending {
                window.focus(&self.input.focus_handle(cx), cx);
                self.focus_pending = false;
            }
            let context = if page.meta.context_mode == "nativeFork" {
                "Native fork · parent context retained"
            } else {
                "Context snapshot · up to 32 recent messages"
            };
            let lifetime = if writable {
                let minutes = ((page.meta.expires_at_ms - Utc::now().timestamp_millis()).max(0)
                    + 59_999)
                    / 60_000;
                format!("Writable for {minutes} more min · leaving this view makes it read-only")
            } else {
                format!(
                    "Read-only · {} · saved transcript",
                    state_label(&page.meta.state)
                )
            };
            root = root.child(
                div()
                    .px(px(16.0))
                    .py(px(10.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(div().text_color(theme.text_muted).child(context))
                    .child(div().text_color(theme.text_muted).child(format!(
                            "{:?} · {} · inherited account",
                            page.meta.config.harness,
                            page.meta
                                .config
                                .model
                                .as_deref()
                                .unwrap_or("Provider default")
                        )))
                    .when_some(page.meta.anchor_text.clone(), |el, quote| {
                        el.child(
                            div()
                                .max_h(px(92.0))
                                .overflow_hidden()
                                .border_l_1()
                                .border_color(theme.border_strong)
                                .pl(px(10.0))
                                .text_color(theme.text_muted)
                                .child(quote),
                        )
                    })
                    .child(div().text_color(theme.text_muted).child(lifetime))
                    .when(page.meta.origin_message_id.is_some(), |el| {
                        el.child(self.button(
                            "side-origin",
                            "Show original message",
                            PanelAction::RevealOrigin,
                            cx,
                        ))
                    })
                    .when_some(page.meta.detail, |el, detail| el.child(detail))
                    .child(
                        div()
                            .flex()
                            .gap(px(6.0))
                            .child(self.button(
                                "side-unload",
                                if self.busy { "Saving…" } else { "Unload" },
                                PanelAction::Unload,
                                cx,
                            ))
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
                            }),
                    ),
            );
            if let Some(transcript) = &self.transcript {
                root = root.child(div().flex_1().min_h_0().child(transcript.clone()));
            }
            if page.entries.is_empty() {
                root = root.child(div().p(px(16.0)).text_color(theme.text_muted).child(
                    if writable {
                        "Ask a question here. Your parent conversation stays unchanged."
                    } else {
                        "This side chat has no saved messages."
                    },
                ));
            }
            if writable {
                if let Some((_, questions)) = self.pending_question()
                    && let Some(question) = questions
                        .iter()
                        .find(|question| !self.answers.contains_key(&question.id))
                {
                    let mut card = div()
                        .px(px(16.0))
                        .py(px(10.0))
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(question.header.clone())
                        .child(question.question.clone());
                    if question.approval.is_some() {
                        card = card
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
                            card = card.child(self.button(
                                format!("side-answer-{index}"),
                                option.clone(),
                                PanelAction::Answer(option.clone()),
                                cx,
                            ));
                        }
                    }
                    root = root.child(card);
                }
                root = root.child(
                    div()
                        .p(px(12.0))
                        .border_t_1()
                        .border_color(theme.border)
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(
                            div()
                                .min_h(px(48.0))
                                .max_h(px(160.0))
                                .rounded(px(8.0))
                                .border_1()
                                .border_color(theme.border)
                                .p(px(10.0))
                                .child(self.input.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .child(div().text_color(theme.text_muted).child(
                                    match page.status {
                                        Some(luna_proto::SessionStatus::AwaitingInput) => {
                                            "Waiting for input"
                                        }
                                        Some(luna_proto::SessionStatus::AwaitingApproval) => {
                                            "Approval needed"
                                        }
                                        Some(luna_proto::SessionStatus::Errored) => {
                                            "Provider error"
                                        }
                                        _ if page.working => "Working…",
                                        _ => "Ready",
                                    },
                                ))
                                .child(self.button("side-send", "Send", PanelAction::Send, cx)),
                        ),
                );
            }
        } else {
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
        }
        root
    }
}

impl Shell {
    pub(super) fn release_side_chat(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.side_chat_panel.take() else {
            return;
        };
        self.side_chat_events = None;
        let request = panel.read(cx).release_request();
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
        let panel = cx
            .new(|cx| SideChatPanel::new(self.state.clone(), self.active_chat.clone(), origin, cx));
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
