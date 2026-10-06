//! Explicit transfer dialogs (slice 2), previewed and applied by the host:
//! Send findings back (a fork's conversation context stored for its lineage
//! source's next message) and Start new task with context (an independent
//! ordinary task created with a captured copy, atomically, on apply).
//! Conversation context only: no files, branches or Git changes move, and
//! nothing starts or interrupts a turn anywhere.

use super::*;
use crate::context_transfer::{ApplyOutcome, ApplyState, PreviewState, TransferFlow};

/// Which explicit transfer the dialog reviews.
#[derive(Debug, Clone, PartialEq)]
enum DialogMode {
    /// A fork's findings to its lineage source.
    Findings,
    /// A new ordinary task (the reserved `targetTaskId`) with a copy of the
    /// source's context, created only when applied; `config` is shown.
    NewTask { config: String },
}

pub(super) struct TransferDialog {
    mode: DialogMode,
    flow: TransferFlow,
    /// The transfer's source chat title.
    fork_title: String,
    /// The destination's title (the source chat for findings).
    source_title: String,
    focus: FocusHandle,
    focus_pending: bool,
    task: Option<Task<()>>,
    /// QA: report the next applied send as unknown (a lost answer).
    #[cfg(feature = "appshots-fixture")]
    lose_apply: bool,
}

impl Shell {
    /// Where `fork`'s findings may go now: its lineage source, while both
    /// chats exist and their host offers explicit transfers. The same answer
    /// gates every menu entry and the dialog.
    pub(super) fn findings_destination(&self, fork: &str, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let source = super::side_chats::fork_source(state, fork)?.to_string();
        let exists = |id: &str| state.chats.iter().any(|chat| chat.id == id);
        (exists(fork)
            && exists(&source)
            && crate::context_transfer::supported(state, fork)
            && crate::context_transfer::supported(state, &source))
        .then_some(source)
    }

    fn chat_title_or_untitled(&self, id: &str, cx: &App) -> String {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .and_then(|chat| chat.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Untitled chat".into())
    }

    /// "Send findings to {source}…": ask the host for the fork's findings
    /// preview (read-only).
    /// An open transfer whose apply is in flight or unknown is never replaced
    /// (its command id stays for Check status); close it first.
    fn transfer_dialog_busy(&mut self, cx: &mut Context<Self>) -> bool {
        let busy = self
            .transfer_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.flow.busy());
        if busy {
            self.sidebar_notice = Some(
                "A context transfer is still being saved or checked. Check its status or close it first."
                    .into(),
            );
            cx.notify();
        }
        busy
    }

    pub(super) fn open_findings_dialog(&mut self, fork: String, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        if self.transfer_dialog_busy(cx) {
            return;
        }
        let Some(source) = self.findings_destination(&fork, cx) else {
            self.sidebar_notice = Some("This chat has no source to send findings to.".into());
            cx.notify();
            return;
        };
        // The source's own draft sizes the budget (it is never sent here).
        let prompt_text = self
            .workbench_composer(&source, cx)
            .map(|composer| composer.read(cx).input.read(cx).text().to_string())
            .unwrap_or_default();
        let request = luna_proto::ContextTransferRequest {
            kind: luna_proto::ContextTransferKind::ForkMerge,
            source_task_id: fork.clone(),
            target_task_id: source.clone(),
            target_config: None,
            new_target: None,
            message_ids: Vec::new(),
            prompt_text,
        };
        let generation = self.state.read(cx).engine_generation();
        self.transfer_dialog = Some(TransferDialog {
            mode: DialogMode::Findings,
            flow: TransferFlow::new(request, generation),
            fork_title: self.chat_title_or_untitled(&fork, cx),
            source_title: self.chat_title_or_untitled(&source, cx),
            focus: cx.focus_handle(),
            focus_pending: true,
            task: None,
            #[cfg(feature = "appshots-fixture")]
            lose_apply: false,
        });
        self.refresh_findings_preview(cx);
        cx.notify();
    }

    /// Whether `source` can start a new task with a copy of its context
    /// (the chat exists, has a configuration, and its host offers transfers).
    pub(super) fn new_task_with_context_available(&self, source: &str, cx: &App) -> bool {
        let state = self.state.read(cx);
        state
            .chats
            .iter()
            .any(|chat| chat.id == source && chat.config.is_some())
            && crate::context_transfer::supported(state, source)
    }

    /// "Start new task with context…": reserve the new task's id for this
    /// dialog and ask the host for a preview into it. Nothing is created
    /// until the reviewed preview is applied; the source and its draft stay
    /// as they are.
    pub(super) fn open_new_task_with_context(&mut self, source: String, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        if self.transfer_dialog_busy(cx) {
            return;
        }
        let config = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == source)
            .and_then(|chat| chat.config.clone());
        let (Some(config), true) = (config, self.new_task_with_context_available(&source, cx))
        else {
            self.sidebar_notice = Some("This chat can't start a new task with its context.".into());
            cx.notify();
            return;
        };
        let config_line = [
            Some(super::child_chats::provider_label(config.harness).to_string()),
            config.model.clone(),
            config
                .reasoning
                .map(|level| crate::pickers::reasoning_label(level).to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let request = luna_proto::ContextTransferRequest {
            kind: luna_proto::ContextTransferKind::PortableFork,
            source_task_id: source.clone(),
            // Reserved for this dialog: refreshes and Check status keep it.
            target_task_id: uuid::Uuid::new_v4().to_string(),
            target_config: None,
            new_target: Some(luna_proto::ContextTransferNewTarget {
                config,
                title: None,
            }),
            message_ids: Vec::new(),
            prompt_text: String::new(),
        };
        let generation = self.state.read(cx).engine_generation();
        let source_title = self.chat_title_or_untitled(&source, cx);
        self.transfer_dialog = Some(TransferDialog {
            mode: DialogMode::NewTask {
                config: config_line,
            },
            flow: TransferFlow::new(request, generation),
            fork_title: source_title,
            source_title: "New task".into(),
            focus: cx.focus_handle(),
            focus_pending: true,
            task: None,
            #[cfg(feature = "appshots-fixture")]
            lose_apply: false,
        });
        self.refresh_findings_preview(cx);
        cx.notify();
    }

    /// The dialog still describes the same chats and connection.
    fn findings_current(&self, cx: &App) -> bool {
        self.transfer_dialog.as_ref().is_some_and(|dialog| {
            dialog.flow.engine_generation == self.state.read(cx).engine_generation()
                && match dialog.mode {
                    DialogMode::Findings => {
                        self.findings_destination(&dialog.flow.request.source_task_id, cx)
                            .as_deref()
                            == Some(dialog.flow.request.target_task_id.as_str())
                    }
                    DialogMode::NewTask { .. } => self
                        .new_task_with_context_available(&dialog.flow.request.source_task_id, cx),
                }
        })
    }

    fn refresh_findings_preview(&mut self, cx: &mut Context<Self>) {
        let (engine, generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(dialog) = self.transfer_dialog.as_mut() else {
            return;
        };
        // Refusing here (not just hiding Refresh) keeps an in-flight or
        // unknown apply's command id and blocks a second apply.
        if dialog.flow.busy() {
            return;
        }
        let Some(engine) = engine else {
            dialog.flow.preview = PreviewState::Error("Engine not connected".into());
            return;
        };
        let attempts = dialog.flow.apply_attempts;
        let show_text = dialog.flow.show_text;
        dialog.flow = TransferFlow::new(dialog.flow.request.clone(), generation);
        dialog.flow.apply_attempts = attempts;
        dialog.flow.show_text = show_text;
        let request = dialog.flow.request.clone();
        dialog.task = Some(cx.spawn(async move |this, cx| {
            let result = crate::context_transfer::request_preview(engine, request.clone()).await;
            this.update(cx, |shell, cx| {
                let current = shell.findings_current(cx);
                if let Some(dialog) = shell.transfer_dialog.as_mut()
                    && current
                    && dialog.flow.request == request
                    && dialog.flow.engine_generation == generation
                {
                    dialog.flow.install_preview(result);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// The dialog's primary action (button, QA alike): send the reviewed
    /// findings, or re-read an unknown outcome's receipt.
    fn findings_primary(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.transfer_dialog
                .as_ref()
                .map(|dialog| &dialog.flow.apply),
            Some(ApplyState::Unknown { .. })
        ) {
            self.check_findings_status(cx);
        } else {
            self.apply_findings(cx);
        }
    }

    fn apply_findings(&mut self, cx: &mut Context<Self>) {
        let current = self.findings_current(cx);
        let engine = self.state.read(cx).engine().cloned();
        let Some(dialog) = self.transfer_dialog.as_mut() else {
            return;
        };
        if !current {
            dialog.flow.preview = PreviewState::Stale(
                "The chats or the connection changed. Refresh the preview.".into(),
            );
            cx.notify();
            return;
        }
        let Some(engine) = engine else {
            return;
        };
        if !dialog.flow.can_apply() {
            return;
        }
        let Some(preview) = dialog.flow.ready().cloned() else {
            return;
        };
        let command_id = uuid::Uuid::new_v4().to_string();
        dialog.flow.apply = ApplyState::Applying {
            command_id: command_id.clone(),
        };
        dialog.flow.apply_attempts += 1;
        let generation = dialog.flow.engine_generation;
        let preview_id = preview.preview_id.clone();
        let target = dialog.flow.request.target_task_id.clone();
        dialog.task = Some(cx.spawn(async move |this, cx| {
            let outcome =
                crate::context_transfer::apply_preview(engine, preview, command_id.clone()).await;
            this.update(cx, |shell, cx| {
                shell.settle_findings(&preview_id, &target, generation, command_id, outcome, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn settle_findings(
        &mut self,
        preview_id: &str,
        target: &str,
        generation: u64,
        command_id: String,
        outcome: ApplyOutcome,
        cx: &mut Context<Self>,
    ) {
        // Only on the connection that sent it, for the dialog and target it
        // was sent for: a late answer from an earlier engine or profile
        // never settles, navigates or notifies here.
        let current_generation = self.state.read(cx).engine_generation();
        let Some(dialog) = self.transfer_dialog.as_mut().filter(|dialog| {
            current_generation == generation
                && dialog.flow.engine_generation == generation
                && dialog.flow.request.target_task_id == target
                && dialog
                    .flow
                    .ready()
                    .is_some_and(|preview| preview.preview_id == preview_id)
        }) else {
            return;
        };
        #[cfg(feature = "appshots-fixture")]
        let outcome =
            if std::mem::take(&mut dialog.lose_apply) && matches!(outcome, ApplyOutcome::Applied) {
                ApplyOutcome::Unknown
            } else {
                outcome
            };
        dialog.flow.install_apply(command_id, outcome);
        if dialog.flow.apply == ApplyState::Applied {
            let (mode, from, to, target) = (
                dialog.mode.clone(),
                dialog.fork_title.clone(),
                dialog.source_title.clone(),
                dialog.flow.request.target_task_id.clone(),
            );
            self.transfer_dialog = None;
            match mode {
                DialogMode::Findings => {
                    self.sidebar_notice = Some(SidebarNotice::transient_status(format!(
                        "Findings saved for {to}. They're added to your next message there."
                    )));
                }
                DialogMode::NewTask { .. } => {
                    // Open exactly the task the host created for this id.
                    self.open_chat(target, cx);
                    self.sidebar_notice = Some(SidebarNotice::transient_status(format!(
                        "Started a new task with context from {from}. It's added to your first message."
                    )));
                }
            }
        }
        cx.notify();
    }

    fn check_findings_status(&mut self, cx: &mut Context<Self>) {
        let engine = self.state.read(cx).engine().cloned();
        let Some(dialog) = self.transfer_dialog.as_mut() else {
            return;
        };
        let Some(engine) = engine else {
            return;
        };
        let ApplyState::Unknown {
            command_id,
            checking,
        } = &mut dialog.flow.apply
        else {
            return;
        };
        if *checking {
            return;
        }
        *checking = true;
        let command_id = command_id.clone();
        let generation = dialog.flow.engine_generation;
        let target = dialog.flow.request.target_task_id.clone();
        let preview_id = dialog
            .flow
            .ready()
            .map(|preview| preview.preview_id.clone());
        dialog.task = Some(cx.spawn(async move |this, cx| {
            let outcome = crate::context_transfer::check_apply(engine, command_id.clone()).await;
            this.update(cx, |shell, cx| {
                // A late answer from an earlier connection changes nothing.
                if shell.state.read(cx).engine_generation() != generation {
                    return;
                }
                match (outcome, preview_id) {
                    (Some(outcome), Some(preview_id)) => shell.settle_findings(
                        &preview_id,
                        &target,
                        generation,
                        command_id,
                        outcome,
                        cx,
                    ),
                    _ => {
                        if let Some(ApplyState::Unknown { checking, .. }) = shell
                            .transfer_dialog
                            .as_mut()
                            .filter(|dialog| {
                                dialog.flow.engine_generation == generation
                                    && dialog.flow.request.target_task_id == target
                            })
                            .map(|dialog| &mut dialog.flow.apply)
                        {
                            *checking = false;
                        }
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn close_transfer_dialog(&mut self, cx: &mut Context<Self>) {
        // An unknown outcome stays visible in the receipts; closing never
        // resends it.
        if self.transfer_dialog.take().is_some() {
            cx.notify();
        }
    }

    /// The dialog card, while open.
    pub(super) fn render_findings_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.findings_current(cx)
            && let Some(dialog) = self.transfer_dialog.as_mut()
            && !matches!(dialog.flow.preview, PreviewState::Stale(_))
            && !matches!(dialog.flow.apply, ApplyState::Unknown { .. })
        {
            dialog.flow.preview = PreviewState::Stale(
                "The chats or the connection changed. Refresh the preview.".into(),
            );
        }
        let current = self.findings_current(cx);
        let dialog = self.transfer_dialog.as_mut()?;
        if std::mem::take(&mut dialog.focus_pending) {
            window.focus(&dialog.focus, cx);
        }
        let dialog = self.transfer_dialog.as_ref()?;
        let theme = Theme::of(cx).clone();
        let flow = &dialog.flow;
        let toggle = cx.listener(|this, _, _, cx| {
            if let Some(dialog) = this.transfer_dialog.as_mut() {
                dialog.flow.show_text = !dialog.flow.show_text;
            }
            cx.notify();
        });
        let refresh = matches!(
            flow.preview,
            PreviewState::Stale(_) | PreviewState::Error(_)
        ) && !matches!(flow.apply, ApplyState::Unknown { .. });
        let (primary_label, primary_enabled) = match (&flow.apply, &dialog.mode) {
            (ApplyState::Unknown { checking, .. }, _) => ("Check status", !checking),
            (ApplyState::Applying { .. }, DialogMode::Findings) => ("Sending…", false),
            (ApplyState::Applying { .. }, DialogMode::NewTask { .. }) => ("Creating…", false),
            (_, DialogMode::Findings) => ("Send findings", current && flow.can_apply()),
            (_, DialogMode::NewTask { .. }) => ("Create task", current && flow.can_apply()),
        };
        let (title, body) = match &dialog.mode {
            DialogMode::Findings => (
                format!("Send findings to {}", dialog.source_title),
                format!(
                    "From {}. Conversation context only: files, branches and Git changes are not merged. Nothing is sent to {} now; the findings are added to your next message there.",
                    dialog.fork_title, dialog.source_title
                ),
            ),
            DialogMode::NewTask { config } => (
                "Start new task with context".to_string(),
                format!(
                    "Creates a new task on {config}, in {from}'s project. It carries a copy of {from}'s recent visible messages, added to your first message there. Nothing is sent now; {from} and its draft are unchanged. Files and Git changes are not copied.",
                    from = dialog.fork_title
                ),
            ),
        };
        let card = popover::dialog_card(&theme)
            .id("findings-dialog")
            .track_focus(&dialog.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close_transfer_dialog(cx);
                    cx.stop_propagation();
                }
            }))
            .child(popover::dialog_title(&theme, &title))
            .child(
                div()
                    .mt(crate::typography::ui_rems(6.0))
                    .child(popover::dialog_body(&theme, body)),
            )
            .child(
                div()
                    .id("findings-preview")
                    .mt(crate::typography::ui_rems(10.0))
                    .max_h(crate::typography::ui_rems(380.0))
                    .overflow_y_scroll()
                    .child(crate::context_transfer::render_preview_body(
                        &theme, flow, toggle,
                    )),
            )
            .children(
                crate::context_transfer::apply_line(&theme, flow)
                    .map(|line| line.mt(crate::typography::ui_rems(6.0))),
            )
            .when(refresh, |card| {
                card.child(
                    div()
                        .id("findings-refresh")
                        .mt(crate::typography::ui_rems(6.0))
                        .accessible_button("Refresh the preview", theme.accent)
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.accent)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.refresh_findings_preview(cx);
                            cx.notify();
                        }))
                        .child("Refresh the preview"),
                )
            })
            .child(
                div()
                    .mt(crate::typography::ui_rems(14.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(crate::typography::ui_rems(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "findings-cancel")
                            .id("findings-cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.close_transfer_dialog(cx))),
                    )
                    .child({
                        let button = popover::btn_primary(&theme, primary_label)
                            .id("findings-send")
                            .aria_label(primary_label);
                        if primary_enabled {
                            button
                                .on_click(cx.listener(|this, _, _, cx| this.findings_primary(cx)))
                                .into_any_element()
                        } else {
                            div().opacity(0.5).child(button).into_any_element()
                        }
                    }),
            )
            .into_any_element();
        Some(card)
    }
}

/// Headless QA drives the dialog through the same paths as its controls.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_findings_offered(&self, fork: &str, cx: &App) -> Option<String> {
        self.findings_destination(fork, cx)
    }
    pub fn fixture_open_findings(&mut self, fork: &str, cx: &mut Context<Self>) {
        self.open_findings_dialog(fork.to_string(), cx);
    }
    pub fn fixture_new_task_with_context_offered(&self, source: &str, cx: &App) -> bool {
        self.new_task_with_context_available(source, cx)
    }
    pub fn fixture_open_new_task_with_context(&mut self, source: &str, cx: &mut Context<Self>) {
        self.open_new_task_with_context(source.to_string(), cx);
    }
    pub fn fixture_close_transfer_dialog(&mut self, cx: &mut Context<Self>) {
        self.close_transfer_dialog(cx);
    }
    pub fn fixture_findings_primary(&mut self, cx: &mut Context<Self>) {
        self.findings_primary(cx);
    }
    pub fn fixture_findings_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_findings_preview(cx);
    }
    pub fn fixture_findings_lose_next_apply(&mut self) {
        if let Some(dialog) = self.transfer_dialog.as_mut() {
            dialog.lose_apply = true;
        }
    }
    /// `task_id`'s context transfer receipts as its views read them, each
    /// with the line the context tray shows.
    pub fn fixture_transfer_receipts(&self, task_id: &str, cx: &App) -> serde_json::Value {
        let state = self.state.read(cx);
        let binding = crate::chat_binding::ChatBinding::Chat(task_id.to_string());
        serde_json::Value::Array(
            binding
                .context_control(state)
                .context_transfers
                .iter()
                .map(|receipt| {
                    serde_json::json!({
                        "id": receipt.id,
                        "kind": receipt.kind,
                        "status": receipt.status,
                        "source": receipt.source.task_id,
                        "target": receipt.target.task_id,
                        "included": receipt.included_message_ids.len(),
                        "line": crate::context_transfer::receipt_line(state, receipt, task_id),
                    })
                })
                .collect(),
        )
    }
    /// A chat as the views see it: whether it exists, is selected, is an
    /// agent child or a context fork, its provider, and how many chats exist.
    pub fn fixture_chat_lineage(&self, id: &str, cx: &App) -> serde_json::Value {
        let state = self.state.read(cx);
        let chat = state.chats.iter().find(|chat| chat.id == id);
        serde_json::json!({
            "exists": chat.is_some(),
            "selected": state.selected_chat.as_deref() == Some(id),
            "childThread": chat.and_then(|chat| chat.child_thread.as_ref()).is_some(),
            "forkSource": state.fork_source_rows.get(id),
            "harness": chat.and_then(|chat| chat.config.as_ref()).map(|config| config.harness),
            "chatCount": state.chats.len(),
        })
    }
    pub fn fixture_findings_readback(&self, cx: &App) -> serde_json::Value {
        match &self.transfer_dialog {
            None => serde_json::json!({"open": false}),
            Some(dialog) => serde_json::json!({
                "open": true,
                "mode": match &dialog.mode {
                    DialogMode::Findings => "findings",
                    DialogMode::NewTask { .. } => "newTask",
                },
                "config": match &dialog.mode {
                    DialogMode::NewTask { config } => Some(config.clone()),
                    DialogMode::Findings => None,
                },
                "current": self.findings_current(cx),
                "forkTitle": dialog.fork_title,
                "sourceTitle": dialog.source_title,
                "flow": dialog.flow.readback(),
            }),
        }
    }
}
