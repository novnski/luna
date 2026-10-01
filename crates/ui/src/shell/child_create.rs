//! "New child chat…": the manual creation dialog.
//!
//! The user chooses where the child works (the parent's current checkout or a
//! new LUNA worktree on a new branch), its model, reasoning level and access
//! level. Everything defaults from the parent. Access above the parent's
//! effective access is an explicit grant: it needs a confirmation checkbox
//! before Create is enabled, and the request carries the chosen mode so the
//! engine records `grantedByUser` (contract: `reports/contract.md` §1).
//! Provider limits are shown as text: unsupported access modes are disabled
//! with the provider's reason, and only the selected model's reasoning ladder
//! is offered.

use super::*;
use luna_proto::{Model, ReasoningLevel};

pub(super) struct ChildThreadDialog {
    pub(super) parent_id: String,
    pub(super) key: String,
    pub(super) prompt: Entity<ComposerInput>,
    pub(super) origin: HistoryEngineOrigin,
    pub(super) focus_pending: bool,
    pub(super) busy: bool,
    /// `false` = the parent's current checkout.
    new_worktree: bool,
    branch: Entity<ComposerInput>,
    base: Entity<ComposerInput>,
    /// `None` = the parent's value.
    model: Option<String>,
    reasoning: Option<ReasoningLevel>,
    access: Option<PermissionMode>,
    grant_confirmed: bool,
    error: Option<String>,
}

/// What the dialog shows and validates against, read from the parent row.
struct ParentFacts {
    harness: Option<HarnessId>,
    model: Option<String>,
    reasoning: Option<ReasoningLevel>,
    access: PermissionMode,
    cwd: Option<String>,
    branch: Option<String>,
    git: bool,
    models: Vec<Model>,
}

fn access_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Ask",
        PermissionMode::Auto => "Auto",
        PermissionMode::Full => "Full access",
    }
}

impl ChildThreadDialog {
    fn effective_model<'a>(&'a self, parent: &'a ParentFacts) -> Option<&'a str> {
        self.model.as_deref().or(parent.model.as_deref())
    }

    fn effective_access(&self, parent: &ParentFacts) -> PermissionMode {
        self.access.unwrap_or(parent.access)
    }

    /// Access above the parent's effective access needs an explicit grant.
    fn exceeds_parent(&self, parent: &ParentFacts) -> bool {
        self.effective_access(parent) > parent.access
    }

    /// The reasoning ladder of the selected model, when the catalog is known.
    fn ladder(&self, parent: &ParentFacts) -> Option<Vec<ReasoningLevel>> {
        let model = self.effective_model(parent)?;
        parent
            .models
            .iter()
            .find(|candidate| candidate.id == model)
            .map(|model| model.reasoning_levels.clone())
    }

    fn access_unavailable(&self, parent: &ParentFacts, mode: PermissionMode) -> Option<String> {
        let harness = parent.harness?;
        let capabilities =
            luna_engine::provider_capabilities_for(harness, self.effective_model(parent));
        (!capabilities.supports(mode)).then(|| match (harness, mode) {
            (HarnessId::ClaudeCode, PermissionMode::Auto) => {
                "Claude: Auto needs a model/account with a verified reviewer.".into()
            }
            (_, PermissionMode::Auto) => "This provider has no verified automatic reviewer.".into(),
            (_, PermissionMode::Full) => "This provider cannot enforce Full access.".into(),
            (_, PermissionMode::Ask) => "This provider cannot ask for approvals.".into(),
        })
    }

    /// The exact `CreateChildThread` request this dialog would send.
    fn request(&self, prompt: &str, parent: &ParentFacts, cx: &App) -> serde_json::Value {
        let mut request = serde_json::json!({
            "prompt": prompt,
            "idempotencyKey": self.key,
        });
        if let Some(model) = self
            .model
            .as_ref()
            .filter(|model| Some(*model) != parent.model.as_ref())
        {
            request["model"] = model.clone().into();
        }
        if let Some(reasoning) = self
            .reasoning
            .filter(|level| Some(*level) != parent.reasoning)
        {
            request["reasoning"] = serde_json::to_value(reasoning).unwrap_or_default();
        }
        if let Some(access) = self.access.filter(|mode| *mode != parent.access) {
            request["permissionMode"] = serde_json::to_value(access).unwrap_or_default();
        }
        request["checkout"] = if self.new_worktree {
            let branch = self.branch.read(cx).text().trim().to_string();
            let base = self.base.read(cx).text().trim().to_string();
            let mut checkout = serde_json::json!({ "kind": "newWorktree" });
            if !branch.is_empty() {
                checkout["branch"] = branch.into();
            }
            if !base.is_empty() {
                checkout["base"] = base.into();
            }
            checkout
        } else {
            serde_json::json!({ "kind": "current" })
        };
        request
    }
}

impl Shell {
    fn child_parent_facts(&self, parent_id: &str, cx: &App) -> ParentFacts {
        let state = self.state.read(cx);
        let chat = state.chats.iter().find(|chat| chat.id == parent_id);
        let config = chat.and_then(|chat| chat.config.as_ref());
        let harness = config.map(|config| config.harness);
        let git = chat
            .and_then(|chat| chat.space_id.as_deref())
            .and_then(|space| state.space_row(space))
            .is_some_and(|space| space.git_detected);
        // The main composer's picker already loads the parent's catalog while
        // the parent is the main chat; without it only the parent's model is
        // offered (truthfully labelled).
        let models = harness
            .and_then(|harness| {
                self.composer
                    .read(cx)
                    .pickers()
                    .read(cx)
                    .loaded_models(harness)
            })
            .unwrap_or_default();
        ParentFacts {
            harness,
            model: config.and_then(|config| config.model.clone()),
            reasoning: config.and_then(|config| config.reasoning),
            access: config
                .map(|config| config.effective_permission_mode)
                .unwrap_or(PermissionMode::Ask),
            cwd: chat.and_then(|chat| chat.cwd.clone()),
            branch: chat.and_then(|chat| chat.branch.clone()),
            git,
            models,
        }
    }

    pub(super) fn open_child_thread_dialog(&mut self, parent_id: String, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        let origin = HistoryEngineOrigin::capture(self.state.read(cx));
        let prompt =
            cx.new(|cx| ComposerInput::with_context("Describe the child task…", "DialogInput", cx));
        let branch = cx.new(|cx| {
            ComposerInput::with_context("New branch (generated when empty)", "DialogInput", cx)
        });
        let base_placeholder = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == parent_id)
            .and_then(|chat| chat.branch.clone())
            .map(|branch| format!("Base: {branch} (parent's branch)"))
            .unwrap_or_else(|| "Base: the parent's HEAD".into());
        let base = cx.new(|cx| ComposerInput::with_context(base_placeholder, "DialogInput", cx));
        self.child_thread_dialog = Some(ChildThreadDialog {
            parent_id,
            key: uuid::Uuid::new_v4().to_string(),
            prompt,
            origin,
            focus_pending: true,
            busy: false,
            new_worktree: false,
            branch,
            base,
            model: None,
            reasoning: None,
            access: None,
            grant_confirmed: false,
            error: None,
        });
        cx.notify();
    }

    pub(super) fn submit_child_thread(&mut self, cx: &mut Context<Self>) {
        let parent_id = match self.child_thread_dialog.as_ref() {
            Some(dialog) if !dialog.busy => dialog.parent_id.clone(),
            _ => return,
        };
        let parent = self.child_parent_facts(&parent_id, cx);
        let Some(dialog) = self.child_thread_dialog.as_mut() else {
            return;
        };
        let prompt = dialog.prompt.read(cx).text().trim().to_string();
        if prompt.is_empty() {
            dialog.error = Some("Describe the child task first.".into());
            cx.notify();
            return;
        }
        if dialog.exceeds_parent(&parent) && !dialog.grant_confirmed {
            dialog.error = Some(format!(
                "{} exceeds the parent's access ({}). Confirm the grant to continue.",
                access_label(dialog.effective_access(&parent)),
                access_label(parent.access)
            ));
            cx.notify();
            return;
        }
        if dialog.new_worktree && !parent.git {
            dialog.error =
                Some("This project has no Git checkout. Use the current checkout.".into());
            cx.notify();
            return;
        }
        let state = self.state.read(cx);
        if !dialog.origin.is_current(state) {
            self.child_thread_dialog = None;
            cx.notify();
            return;
        }
        let Some(engine) = state.engine().cloned() else {
            dialog.error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let origin = dialog.origin;
        let device = state
            .chats
            .iter()
            .find(|chat| chat.id == dialog.parent_id)
            .map(|chat| chat.device_id.clone());
        let request = dialog.request(&prompt, &parent, cx);
        let params = serde_json::json!({"parentTaskId": dialog.parent_id, "targetDeviceId": device, "request": request});
        dialog.busy = true;
        dialog.error = None;
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::CREATE_CHILD_THREAD, params).await;
            this.update(cx, |shell, cx| {
                if !origin.is_current(shell.state.read(cx)) {
                    return;
                }
                let Some(dialog) = shell.child_thread_dialog.as_mut() else {
                    return;
                };
                dialog.busy = false;
                match result {
                    Ok(value) => match serde_json::from_value::<luna_proto::ChildThreadReceipt>(value) {
                        Ok(receipt) => {
                            shell.child_thread_dialog = None;
                            shell.open_chat(receipt.task_id, cx);
                        }
                        Err(error) => {
                            dialog.error = Some(format!(
                                "Child receipt could not be read; inspect the child before retrying: {error}"
                            ))
                        }
                    },
                    // Engine errors are actionable (bad branch, branch exists,
                    // base not found, not a git repo); nothing was created.
                    Err(error) => dialog.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn child_dialog_update(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut ChildThreadDialog, &ParentFacts),
    ) {
        let Some(parent_id) = self
            .child_thread_dialog
            .as_ref()
            .map(|dialog| dialog.parent_id.clone())
        else {
            return;
        };
        let parent = self.child_parent_facts(&parent_id, cx);
        if let Some(dialog) = self.child_thread_dialog.as_mut() {
            update(dialog, &parent);
            dialog.error = None;
            // A grant confirms one concrete escalation; changing what is
            // granted asks again.
            if !dialog.exceeds_parent(&parent) {
                dialog.grant_confirmed = false;
            }
        }
        cx.notify();
    }

    fn option_chip(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        selected: bool,
        disabled: Option<String>,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let label = label.into();
        let border_strong = theme.border_strong;
        div()
            .id(id.into())
            .role(gpui::Role::RadioButton)
            .aria_label(label.clone())
            .tab_index(0)
            .px(px(9.0))
            .py(px(4.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(if selected {
                theme.border_strong
            } else {
                theme.border
            })
            .bg(if selected {
                crate::theme::ink(0.08)
            } else {
                crate::theme::ink(0.02)
            })
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(if selected {
                theme.text
            } else {
                theme.text_muted
            })
            .when(disabled.is_some(), |el| el.opacity(0.4))
            .when(disabled.is_none(), |el| {
                el.cursor_pointer()
                    .focus_visible(move |style| style.border_color(border_strong))
            })
            .child(label)
    }

    pub(super) fn render_child_thread_dialog(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let parent_id = self.child_thread_dialog.as_ref()?.parent_id.clone();
        let parent = self.child_parent_facts(&parent_id, cx);
        let theme = Theme::of(cx).for_popup();
        let dialog = self.child_thread_dialog.as_mut()?;
        if std::mem::take(&mut dialog.focus_pending) {
            window.focus(&dialog.prompt.focus_handle(cx), cx);
        }
        let prompt = dialog.prompt.clone();
        let busy = dialog.busy;
        let new_worktree = dialog.new_worktree;
        let branch = dialog.branch.clone();
        let base = dialog.base.clone();
        let model = dialog.effective_model(&parent).map(str::to_string);
        let reasoning = dialog.reasoning.or(parent.reasoning);
        let access = dialog.effective_access(&parent);
        let exceeds = dialog.exceeds_parent(&parent);
        let grant_confirmed = dialog.grant_confirmed;
        let error = dialog.error.clone();
        let ladder = dialog.ladder(&parent);
        let unavailable: Vec<(PermissionMode, Option<String>)> = [
            PermissionMode::Ask,
            PermissionMode::Auto,
            PermissionMode::Full,
        ]
        .into_iter()
        .map(|mode| (mode, dialog.access_unavailable(&parent, mode)))
        .collect();
        let provider = parent
            .harness
            .map(super::child_chats::provider_label)
            .unwrap_or("Provider");
        let section = |title: &'static str| {
            div()
                .mt(px(14.0))
                .mb(px(6.0))
                .text_size(crate::typography::ui_rems(11.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.readable_muted(0.7))
                .child(title)
        };
        let note = |text: String| {
            div()
                .mt(px(6.0))
                .text_size(crate::typography::ui_rems(11.5))
                .line_height(crate::typography::ui_rems(16.0))
                .text_color(theme.readable_muted(0.62))
                .child(text)
        };

        // Where it works.
        let checkout_row = div()
            .flex()
            .flex_wrap()
            .gap(px(6.0))
            .child(
                self.option_chip(
                    "child-create-current",
                    "Current checkout",
                    !new_worktree,
                    None,
                    &theme,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.child_dialog_update(cx, |dialog, _| dialog.new_worktree = false)
                })),
            )
            .child({
                let disabled =
                    (!parent.git).then(|| "This project has no Git checkout.".to_string());
                let chip = self.option_chip(
                    "child-create-worktree",
                    "New worktree",
                    new_worktree,
                    disabled.clone(),
                    &theme,
                );
                if disabled.is_none() {
                    chip.on_click(cx.listener(|this, _, _, cx| {
                        this.child_dialog_update(cx, |dialog, _| dialog.new_worktree = true)
                    }))
                } else {
                    chip
                }
            });
        let checkout_detail = if new_worktree {
            div()
                .mt(px(8.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(popover::dialog_field(branch.into_any_element()))
                .child(popover::dialog_field(base.into_any_element()))
                .child(note(
                    "A linked git worktree on a new branch, owned by the child. The project's worktree setup runs first."
                        .into(),
                ))
                .into_any_element()
        } else {
            note(format!(
                "Shares the parent's checkout: {}{}",
                parent
                    .cwd
                    .clone()
                    .unwrap_or_else(|| "the parent's folder".into()),
                parent
                    .branch
                    .as_ref()
                    .map(|branch| format!(" · {branch}"))
                    .unwrap_or_default()
            ))
            .into_any_element()
        };
        // Model.
        let mut model_row = div().flex().flex_wrap().gap(px(6.0));
        if parent.models.is_empty() {
            model_row = model_row.child(self.option_chip(
                "child-create-model-parent",
                format!(
                    "{} (parent)",
                    parent.model.clone().unwrap_or_else(|| "Provider default".into())
                ),
                true,
                None,
                &theme,
            ));
        } else {
            // The parent's model stays offered even when this device's
            // catalog no longer lists it (it is what the child would inherit).
            if let Some(parent_model) = parent
                .model
                .clone()
                .filter(|id| !parent.models.iter().any(|candidate| &candidate.id == id))
            {
                let selected = model.as_deref() == Some(parent_model.as_str());
                model_row = model_row.child(
                    self.option_chip(
                        "child-create-model-parent",
                        format!("{parent_model} (parent)"),
                        selected,
                        None,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.child_dialog_update(cx, |dialog, _| dialog.model = None)
                    })),
                );
            }
            for (index, candidate) in parent.models.iter().take(6).enumerate() {
                let id = candidate.id.clone();
                let is_parent = Some(&candidate.id) == parent.model.as_ref();
                let label = if is_parent {
                    format!("{} (parent)", candidate.label)
                } else {
                    candidate.label.clone()
                };
                model_row = model_row.child(
                    self.option_chip(
                        format!("child-create-model-{index}"),
                        label,
                        model.as_deref() == Some(candidate.id.as_str()),
                        None,
                        &theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = id.clone();
                        this.child_dialog_update(cx, move |dialog, parent| {
                            dialog.model = Some(id.clone());
                            // Keep the reasoning only when the new model offers it.
                            let keep = dialog.reasoning.or(parent.reasoning);
                            let ladder = dialog.ladder(parent).unwrap_or_default();
                            if keep.is_some_and(|level| !ladder.contains(&level)) {
                                dialog.reasoning = ladder.first().copied();
                            }
                        })
                    })),
                );
            }
        }

        // Reasoning.
        let mut reasoning_row = div().flex().flex_wrap().gap(px(6.0));
        match &ladder {
            Some(levels) if !levels.is_empty() => {
                for (index, level) in levels.iter().copied().enumerate() {
                    let is_parent = Some(level) == parent.reasoning;
                    let label = if is_parent {
                        format!("{} (parent)", crate::pickers::reasoning_label(level))
                    } else {
                        crate::pickers::reasoning_label(level).to_string()
                    };
                    reasoning_row = reasoning_row.child(
                        self.option_chip(
                            format!("child-create-reasoning-{index}"),
                            label,
                            reasoning == Some(level),
                            None,
                            &theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.child_dialog_update(cx, move |dialog, _| {
                                dialog.reasoning = Some(level)
                            })
                        })),
                    );
                }
            }
            _ => {
                reasoning_row = reasoning_row.child(
                    self.option_chip(
                        "child-create-reasoning-parent",
                        reasoning
                            .map(|level| {
                                format!("{} (parent)", crate::pickers::reasoning_label(level))
                            })
                            .unwrap_or_else(|| "Model default".into()),
                        true,
                        None,
                        &theme,
                    ),
                );
            }
        }

        // Access.
        let mut access_row = div().flex().flex_wrap().gap(px(6.0));
        let mut access_notes = Vec::new();
        for (mode, reason) in unavailable {
            let is_parent = mode == parent.access;
            let label = if is_parent {
                format!("{} (parent)", access_label(mode))
            } else {
                access_label(mode).to_string()
            };
            let chip = self.option_chip(
                format!(
                    "child-create-access-{}",
                    access_label(mode).to_lowercase().replace(' ', "-")
                ),
                label,
                access == mode,
                reason.clone(),
                &theme,
            );
            access_row = access_row.child(if reason.is_none() {
                chip.on_click(cx.listener(move |this, _, _, cx| {
                    this.child_dialog_update(cx, move |dialog, _| dialog.access = Some(mode))
                }))
            } else {
                chip
            });
            if let Some(reason) = reason {
                access_notes.push(format!("{}: {reason}", access_label(mode)));
            }
        }
        let grant = exceeds.then(|| {
            div()
                .id("child-create-grant")
                .role(gpui::Role::CheckBox)
                .aria_label("Grant broader access than the parent")
                .tab_index(0)
                .mt(px(8.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.warning.opacity(0.35))
                .bg(theme.warning.opacity(0.08))
                .flex()
                .items_start()
                .gap(px(8.0))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(dialog) = this.child_thread_dialog.as_mut() {
                        dialog.grant_confirmed = !dialog.grant_confirmed;
                        dialog.error = None;
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .mt(px(2.0))
                        .size(px(14.0))
                        .flex_none()
                        .rounded(px(3.0))
                        .border_1()
                        .border_color(theme.warning)
                        .when(grant_confirmed, |el| el.bg(theme.warning)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(crate::typography::ui_rems(12.0))
                        .line_height(crate::typography::ui_rems(17.0))
                        .text_color(theme.text)
                        .child(format!(
                            "Grants broader access: {} exceeds the parent's {}. I grant this child {} for its own runs; the parent's later limits will not re-cap it.",
                            access_label(access),
                            access_label(parent.access),
                            access_label(access)
                        )),
                )
        });
        let create_disabled = busy || (exceeds && !grant_confirmed);
        let card = popover::dialog_card(&theme)
            .id("child-thread-dialog-card")
            .w(px(440.0))
            .max_h(px((f32::from(viewport.height) - 80.0).max(320.0)))
            .overflow_y_scroll()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.child_thread_dialog = None;
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(popover::dialog_title(&theme, "New child chat"))
            .child(note(
                "A full, independent chat that starts from this prompt and reports its results to the parent.".into(),
            ))
            .child(div().mt(px(12.0)).child(popover::dialog_field(prompt.into_any_element())))
            .child(section("Where it works"))
            .child(checkout_row)
            .child(checkout_detail)
            .child(section("Model"))
            .child(model_row)
            .when(parent.models.is_empty(), |el| {
                el.child(note(format!(
                    "{provider}'s model list is not loaded on this device; the child uses the parent's model."
                )))
            })
            .child(section("Reasoning"))
            .child(reasoning_row)
            .when(!parent.models.is_empty(), |el| match &ladder {
                Some(levels) if levels.is_empty() => {
                    el.child(note("This model reports no reasoning levels.".into()))
                }
                None => el.child(note(
                    "This model's levels are not in the catalog; the child keeps the parent's reasoning."
                        .into(),
                )),
                Some(_) => el,
            })
            .child(section("Access"))
            .child(access_row)
            .children(access_notes.into_iter().map(note))
            .child(note(format!(
                "Defaults to the parent's access ({}). Access is still limited by this machine's ceiling and by {provider}.",
                access_label(parent.access)
            )))
            .children(grant)
            .when_some(error, |el, error| {
                el.child(
                    div()
                        .id("child-create-error")
                        .mt(px(10.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .child(
                div()
                    .mt(px(14.0))
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "child-chat-cancel")
                            .id("child-chat-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.child_thread_dialog = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_primary(&theme, if busy { "Creating…" } else { "Create child" })
                            .id("child-chat-create")
                            .when(create_disabled, |el| el.opacity(0.45))
                            .on_click(cx.listener(|this, _, _, cx| this.submit_child_thread(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal("child-thread-dialog", viewport, card))
    }
}

/// Headless QA drives the production dialog through these hooks.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_open_child_dialog(&mut self, parent: String, cx: &mut Context<Self>) {
        self.open_child_thread_dialog(parent, cx);
    }

    /// Fill the dialog: prompt, new worktree + branch, access.
    pub fn fixture_child_dialog_fill(
        &mut self,
        prompt: &str,
        new_worktree: bool,
        branch: &str,
        access: Option<PermissionMode>,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.child_thread_dialog.as_ref() {
            let (input, branch_input) = (dialog.prompt.clone(), dialog.branch.clone());
            input.update(cx, |input, cx| input.set_text(prompt, cx));
            branch_input.update(cx, |input, cx| input.set_text(branch, cx));
        }
        self.child_dialog_update(cx, |dialog, _| {
            dialog.new_worktree = new_worktree;
            dialog.access = access;
        });
    }

    /// The checkbox's own click path (toggles the grant, clears the error).
    pub fn fixture_child_dialog_confirm_grant(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.child_thread_dialog.as_mut() {
            dialog.grant_confirmed = true;
            dialog.error = None;
        }
        cx.notify();
    }

    pub fn fixture_child_dialog_submit(&mut self, cx: &mut Context<Self>) {
        self.submit_child_thread(cx);
    }

    pub fn fixture_child_dialog_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        let Some(dialog) = self.child_thread_dialog.as_ref() else {
            return serde_json::Value::Null;
        };
        let parent = self.child_parent_facts(&dialog.parent_id, cx);
        let prompt = dialog.prompt.read(cx).text().trim().to_string();
        serde_json::json!({
            "parent": dialog.parent_id,
            "parentAccess": parent.access,
            "access": dialog.effective_access(&parent),
            "exceedsParent": dialog.exceeds_parent(&parent),
            "grantConfirmed": dialog.grant_confirmed,
            "busy": dialog.busy,
            "error": dialog.error,
            "modelsOffered": parent.models.iter().map(|model| model.id.clone()).collect::<Vec<_>>(),
            "request": dialog.request(&prompt, &parent, cx),
        })
    }
}
