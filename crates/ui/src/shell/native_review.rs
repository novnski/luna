//! Native review picker. Reuses ordinary chat creation and the durable Run command.
//!
//! Codex runs its native `review/start`. Claude Code runs its own
//! `/code-review` (the engine maps the target; custom instructions are an
//! ordinary message) and reports findings through `ReportFindings`. Claude's
//! cloud ultrareview is a separate service lane, shown unavailable until its
//! job, cost and result interface is verified.
use super::*;
use luna_proto::{Chat, NativeReviewRequest, ReviewTarget, RunRequest, TaskCommandRead};
use luna_rpc::RpcError;
use serde_json::json;

pub(super) struct NativeReviewDialog {
    source: Chat,
    /// Claude Code's `/code-review` lane rather than Codex's native review.
    claude: bool,
    origin: HistoryEngineOrigin,
    snapshot: Option<TaskCommandSnapshot>,
    input: Entity<ComposerInput>,
    target_kind: usize,
    separate: bool,
    branches: Vec<String>,
    reference_error: Option<String>,
    error: Option<String>,
    created_task_id: Option<String>,
    sending: bool,
    focus_pending: bool,
    _events: Subscription,
}

impl Shell {
    pub(super) fn open_native_review(&mut self, task_id: String, cx: &mut Context<Self>) {
        if self
            .native_review_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.sending)
        {
            return;
        }
        self.close_chat_menu(cx);
        self.cancel_history_flow(cx);
        let state = self.state.read(cx);
        let Some(source) = state.chats.iter().find(|chat| chat.id == task_id).cloned() else {
            return;
        };
        let Some(harness) = source
            .config
            .as_ref()
            .map(|config| config.harness)
            .filter(|harness| matches!(harness, HarnessId::Codex | HarnessId::ClaudeCode))
        else {
            self.sidebar_notice = Some("Code review needs a Codex or Claude Code chat.".into());
            return;
        };
        let claude = harness == HarnessId::ClaudeCode;
        let Some(engine) = state.engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected.".into());
            return;
        };
        let origin = HistoryEngineOrigin::capture(state);
        let generation = self.history_flow_generation;
        let cwd = source.cwd.clone().unwrap_or_default();
        let host = source.device_id.clone();
        let input =
            cx.new(|cx| ComposerInput::new("Base branch, commit SHA, or review instructions", cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_native_review(cx);
            }
        });
        self.native_review_dialog = Some(NativeReviewDialog {
            source,
            claude,
            origin,
            snapshot: None,
            input,
            target_kind: 0,
            separate: false,
            branches: Vec::new(),
            reference_error: None,
            error: None,
            created_task_id: None,
            sending: false,
            focus_pending: false,
            _events: events,
        });
        self.native_review_task = Some(cx.spawn(async move |this, cx| {
            let snapshot = engine
                .client()
                .call_as::<TaskCommandRead>(
                    methods::GET_TASK_COMMAND_SNAPSHOT,
                    json!({ "taskId": task_id, "targetDeviceId": host }),
                )
                .await;
            this.update(cx, |shell, cx| {
                if generation != shell.history_flow_generation
                    || !origin.is_current(shell.state.read(cx))
                {
                    return;
                }
                let Some(dialog) = shell.native_review_dialog.as_mut() else {
                    return;
                };
                match snapshot {
                    Ok(read) => dialog.snapshot = Some(read.snapshot),
                    Err(error) => {
                        dialog.error = Some(format!("Could not read task binding: {error}"))
                    }
                }
                cx.notify();
            })
            .ok();
            let branches = engine
                .client()
                .call_as::<Vec<String>>(
                    methods::LIST_BRANCHES,
                    json!({ "repoPath": cwd, "targetDeviceId": host }),
                )
                .await;
            this.update(cx, |shell, cx| {
                if generation != shell.history_flow_generation
                    || !origin.is_current(shell.state.read(cx))
                {
                    return;
                }
                let Some(dialog) = shell.native_review_dialog.as_mut() else {
                    return;
                };
                match branches {
                    Ok(branches) => dialog.branches = branches,
                    Err(error) => {
                        dialog.reference_error = Some(format!(
                            "Branch list unavailable: {error}. Enter a reference manually."
                        ))
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn select_native_review_target(&mut self, kind: usize, cx: &mut Context<Self>) {
        let Some(dialog) = self.native_review_dialog.as_mut() else {
            return;
        };
        if dialog.sending {
            return;
        }
        dialog.target_kind = kind;
        dialog.error = None;
        let placeholder = match kind {
            1 => "Base branch (for example main or origin/main)",
            2 => "Commit SHA or Git commit reference",
            _ if dialog.claude => "What Claude should review, and how",
            _ => "Instructions for the native reviewer",
        };
        dialog.input.update(cx, |input, cx| {
            input.set_text("", cx);
            input.set_placeholder(placeholder, cx);
        });
        dialog.focus_pending = kind != 0;
        cx.notify();
    }

    fn submit_native_review(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.native_review_dialog.as_mut() else {
            return;
        };
        if dialog.sending {
            return;
        }
        let text = dialog.input.read(cx).text().trim().to_string();
        let target = match dialog.target_kind {
            0 => ReviewTarget::UncommittedChanges,
            1 => ReviewTarget::BaseBranch { branch: text },
            2 => ReviewTarget::Commit {
                sha: text,
                title: None,
            },
            _ => ReviewTarget::Custom { instructions: text },
        };
        if let Err(error) = target.validate() {
            dialog.error = Some(error);
            cx.notify();
            return;
        }
        let Some(snapshot) = dialog.snapshot.clone() else {
            dialog.error =
                Some("The task binding is still loading. Reopen the picker if it failed.".into());
            cx.notify();
            return;
        };
        let source = dialog.source.clone();
        let origin = dialog.origin;
        let separate = dialog.separate;
        let existing_destination = dialog.created_task_id.clone();
        if separate
            && !self.state.read(cx).spaces.iter().any(|space| {
                source.space_id.as_deref() == Some(space.id.as_str())
                    && space.device_id == source.device_id
            })
        {
            dialog.error = Some("Separate review is unavailable because this task and project use different computers. Choose In this chat.".into());
            cx.notify();
            return;
        }
        if !origin.is_current(self.state.read(cx)) {
            self.cancel_history_flow(cx);
            self.sidebar_notice = Some("Profile changed; reopen the review picker.".into());
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let generation = self.history_flow_generation;
        dialog.sending = true;
        dialog.error = None;
        self.native_review_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let config = source.config.clone().ok_or_else(|| {
                    RpcError::Failed("Review configuration is unavailable".into())
                })?;
                let cwd = source
                    .cwd
                    .clone()
                    .filter(|cwd| !cwd.is_empty())
                    .ok_or_else(|| RpcError::Failed("Review checkout is unavailable".into()))?;
                let destination = if separate {
                    // A separate result uses an ordinary LUNA chat in the same
                    // project/host/checkout/account. It owns a new native thread,
                    // avoiding deprecated provider detached delivery and rebinding.
                    let current = engine
                        .client()
                        .call_as::<TaskCommandRead>(
                            methods::GET_TASK_COMMAND_SNAPSHOT,
                            json!({ "taskId": source.id, "targetDeviceId": source.device_id }),
                        )
                        .await?;
                    if current.snapshot != snapshot {
                        return Err(RpcError::Failed(
                            "Source task changed; reopen the review picker".into(),
                        ));
                    }
                    if let Some(id) = existing_destination {
                        id
                    } else {
                        let id = uuid::Uuid::new_v4().to_string();
                        crate::task_commands::execute(
                            &engine,
                            &id,
                            json!({
                                "kind": "createChat", "spaceId": source.space_id,
                                "cwd": cwd, "config": config, "branch": source.branch, "deviceId": source.device_id,
                            }),
                        )
                        .await?;
                        this.update(cx, |shell, cx| {
                            if generation == shell.history_flow_generation
                                && origin.is_current(shell.state.read(cx))
                                && let Some(dialog) = shell.native_review_dialog.as_mut()
                            {
                                dialog.created_task_id = Some(id.clone());
                            }
                        })
                        .ok();
                        id
                    }
                } else {
                    source.id.clone()
                };
                let request = RunRequest {
                    review: Some(NativeReviewRequest {
                        target: target.clone(),
                    }),
                    goal: None,
                    prompt: target.label(),
                    harness: Some(config.harness),
                    provider_account_id: config.provider_account_id,
                    model: config.model,
                    reasoning: config.reasoning,
                    model_options: config.model_options,
                    cwd,
                    sandbox: config.sandbox,
                    auto_approve: false,
                    permission_mode: config.permission_mode,
                    parent_permission_ceiling: None,
                    environment_permission_ceiling: None,
                    permission_policy: None,
                    resume: None,
                    attachments: Vec::new(),
                    input_items: Vec::new(),
                    worktree: None,
                };
                let message_id = uuid::Uuid::new_v4().to_string();
                let intent = json!({ "kind": "nativeReview", "request": request, "messageId": message_id });
                let current = this
                    .update(cx, |shell, cx| origin.is_current(shell.state.read(cx)))
                    .unwrap_or(false);
                if !current {
                    return Err(RpcError::Failed(
                        "Profile changed during review setup".into(),
                    ));
                }
                // Address the execution host so an older host rejects the
                // nativeReview kind instead of a newer local engine queueing a
                // Run that the host would treat as an ordinary text turn.
                let reviewed = (!separate).then_some(snapshot);
                crate::task_commands::execute_reviewed_on(
                    &engine,
                    &destination,
                    Some(source.device_id.as_str()),
                    reviewed,
                    intent,
                )
                .await?;
                Ok::<_, RpcError>(destination)
            }
            .await;
            this.update(cx, |shell, cx| {
                if generation != shell.history_flow_generation
                    || !origin.is_current(shell.state.read(cx))
                {
                    return;
                }
                match result {
                    Ok(destination) => {
                        shell.native_review_dialog = None;
                        shell
                            .state
                            .update(cx, |state, cx| state.select_chat(Some(destination), cx));
                    }
                    Err(error) => {
                        if let Some(dialog) = shell.native_review_dialog.as_mut() {
                            dialog.sending = false;
                            dialog.error =
                                Some(format!("Review could not be started or confirmed: {error}"));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn render_native_review(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.native_review_dialog.as_mut()?;
        if std::mem::take(&mut dialog.focus_pending) {
            window.focus(&dialog.input.focus_handle(cx), cx);
        }
        let input = dialog.input.clone();
        let kind = dialog.target_kind;
        let separate = dialog.separate;
        let sending = dialog.sending;
        let ready = dialog.snapshot.is_some() && !sending;
        let error = dialog.error.clone();
        let reference_error = dialog.reference_error.clone();
        let destination = dialog.created_task_id.clone();
        let claude = dialog.claude;
        let cwd = dialog.source.cwd.clone().unwrap_or_default();
        let branches = dialog.branches.clone();
        let theme = Theme::of(cx).for_popup();
        let targets = [
            "Working changes",
            "Base branch",
            "Commit",
            "Custom instructions",
        ];
        let description = match (claude, kind) {
            (false, 0) => "Review staged, unstaged, and untracked files in this checkout.",
            (false, 1) => "Review changes between the current branch and the selected base branch.",
            (false, 2) => "Review the changes introduced by one commit.",
            (false, _) => "Run the native reviewer with your instructions.",
            (true, 0) => {
                "Claude's /code-review reads this branch's commits since its upstream plus uncommitted changes."
            }
            (true, 1) => {
                "Claude's /code-review of <base>...HEAD: the changes since the selected base branch."
            }
            (true, 2) => "Claude's /code-review of the changes introduced by one commit.",
            (true, _) => {
                "Sends your instructions as an ordinary message; Claude reviews in the chat."
            }
        };
        let card = popover::dialog_card(&theme)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_history_flow(cx);
                    cx.stop_propagation();
                }
            }))
            .child(popover::dialog_title(&theme, "Review code"))
            .when(claude, |card| card.child(div().mt(px(4.0)).text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child("Claude Code /code-review. Findings appear in the chat with their source revision.")))
            .child(div().mt(px(12.0)).flex().flex_wrap().gap(px(6.0)).children(
                targets.into_iter().enumerate().map(|(index, label)| {
                    popover::btn_ghost(&theme, label, format!("native-review-target-{index}"))
                        .id(("native-review-target", index))
                        .opacity(if kind == index { 1.0 } else { 0.65 })
                        .when(!sending, |button| button.on_click(cx.listener(move |this, _, _, cx| {
                            this.select_native_review_target(index, cx);
                        })))
                })
            ))
            .child(div().mt(px(8.0)).text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted).child(description))
            .when(kind != 0, |card| card.child(div().mt(px(12.0)).child(popover::dialog_field(input.into_any_element()))))
            .when(kind == 1 && !branches.is_empty(), |card| {
                card.child(div().mt(px(8.0)).flex().flex_wrap().gap(px(4.0)).children(
                    branches.into_iter().take(12).enumerate().map(|(index, branch)| {
                        popover::btn_ghost(&theme, &branch, format!("native-review-branch-{index}"))
                            .id(("native-review-branch", index))
                            .when(!sending, |button| button.on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(dialog) = this.native_review_dialog.as_mut() {
                                    dialog.input.update(cx, |input, cx| input.set_text(branch.clone(), cx));
                                    dialog.error = None;
                                    cx.notify();
                                }
                            })))
                    })
                ))
            })
            .when(claude, |card| card.child(claude_cloud_lane(&theme)))
            .when(kind == 1 && reference_error.is_some(), |card| card.child(div().mt(px(6.0))
                .text_size(crate::typography::ui_rems(12.0)).text_color(theme.text_muted)
                .child(reference_error.unwrap_or_default())))
            .child(div().mt(px(12.0)).text_size(crate::typography::ui_rems(12.0)).text_color(theme.text_muted).child(format!("Checkout: {cwd}")))
            .child(div().mt(px(12.0)).flex().gap(px(8.0)).children([false, true].into_iter().map(|choice| {
                popover::btn_ghost(&theme, if choice { "Separate review chat" } else { "In this chat" }, if choice { "native-review-separate" } else { "native-review-inline" })
                    .id(if choice { "native-review-separate" } else { "native-review-inline" })
                    .opacity(if choice == separate { 1.0 } else { 0.65 })
                    .when(!sending && destination.is_none(), |button| button.on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(dialog) = this.native_review_dialog.as_mut() { dialog.separate = choice; cx.notify(); }
                    })))
            })))
            .when(separate, |card| card.child(div().mt(px(8.0)).text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted).child("Results open in an independent review chat using this checkout and account.")))
            .when(destination.is_some(), |card| card.child(div().mt(px(8.0)).text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted).child(format!("Created review chat: {}", destination.unwrap_or_default()))))
            .when(error.is_some(), |card| card.child(div().mt(px(8.0)).text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text).child(error.unwrap_or_default())))
            .child(div().mt(px(16.0)).flex().justify_end().gap(px(8.0))
                .child(popover::btn_ghost(&theme, if sending { "Close" } else { "Cancel" }, "native-review-cancel")
                    .id("native-review-cancel")
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_history_flow(cx))))
                .child(popover::btn_primary(&theme, if sending { "Starting review…" } else { "Start review" })
                    .id("native-review-confirm").opacity(if ready { 1.0 } else { 0.5 })
                    .when(ready, |button| button.on_click(cx.listener(|this, _, _, cx| this.submit_native_review(cx))))))
            .into_any_element();
        Some(popover::modal("native-review-dialog", viewport, card))
    }
}

/// Claude's cloud ultrareview: a separate claude.ai service job with its own
/// cost and result delivery. LUNA has no verified interface to start, track
/// or read one, so the lane states that instead of offering a control.
fn claude_cloud_lane(theme: &Theme) -> impl IntoElement {
    const TEXT: &str = "Cloud ultrareview is a separate claude.ai service job. LUNA can't start or track one yet, so this review always runs locally. Run /ultrareview in a terminal if your account offers it.";
    div()
        .id("native-review-cloud-lane")
        .role(Role::Status)
        .aria_label(TEXT)
        .mt(px(10.0))
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border)
        .flex()
        .gap(px(8.0))
        .items_start()
        .text_size(crate::typography::ui_rems(12.0))
        .line_height(crate::typography::ui_rems(16.0))
        .text_color(theme.text_muted)
        .child(
            div()
                .flex_none()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child("Cloud ultrareview \u{00b7} unavailable"),
        )
        .child(div().flex_1().min_w_0().child(TEXT))
}

/// Headless fixture hooks: the production dialog, driven without pointer
/// coordinates (`claude-review-fixture`).
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_open_native_review(&mut self, task_id: String, cx: &mut Context<Self>) {
        self.open_native_review(task_id, cx);
    }

    pub fn fixture_native_review_target(
        &mut self,
        kind: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        self.select_native_review_target(kind, cx);
        if let Some(dialog) = self.native_review_dialog.as_ref() {
            let text = text.to_owned();
            dialog
                .input
                .update(cx, |input, cx| input.set_text(text, cx));
        }
        cx.notify();
    }

    pub fn fixture_submit_native_review(&mut self, cx: &mut Context<Self>) {
        self.submit_native_review(cx);
    }

    pub fn fixture_native_review_state(&self) -> serde_json::Value {
        match self.native_review_dialog.as_ref() {
            None => json!({"open": false}),
            Some(dialog) => json!({
                "open": true,
                "claude": dialog.claude,
                "kind": dialog.target_kind,
                "snapshotReady": dialog.snapshot.is_some(),
                "sending": dialog.sending,
                "error": dialog.error,
            }),
        }
    }
}
