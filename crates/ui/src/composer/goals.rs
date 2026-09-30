//! Explicit goal controls. Edits are bound to the chat that opened the editor;
//! provider snapshots drive every status and usage label.
use super::*;
use luna_proto::{GoalBudget, GoalOperation, GoalStatus};

#[derive(Default)]
pub(super) struct GoalUi {
    editor: Option<GoalEditor>,
    pending: Option<String>,
    task: Option<Task<()>>,
    error: Option<(String, String)>,
}
struct GoalEditor {
    chat_id: String,
    objective: Entity<ComposerInput>,
    budget: Entity<ComposerInput>,
    creating: bool,
    _subscriptions: Vec<Subscription>,
}

enum GoalAction {
    Edit,
    Save,
    Cancel,
    Operation(GoalOperation),
}

impl Composer {
    fn open_goal_editor(&mut self, cx: &mut Context<Self>) {
        if self.goal_ui.pending.is_some() {
            return;
        }
        let state = self.state.read(cx);
        let Some(chat_id) = state.selected_chat.clone() else {
            return;
        };
        let goal = state
            .context_control
            .goal
            .as_ref()
            .and_then(|readback| readback.goal.clone());
        let objective_text = goal
            .as_ref()
            .map(|goal| goal.objective.clone())
            .unwrap_or_default();
        let budget_text = goal
            .as_ref()
            .and_then(|goal| goal.token_budget)
            .map(|tokens| tokens.to_string())
            .unwrap_or_default();
        let objective = cx.new(|cx| {
            let mut input = ComposerInput::new("What should Codex keep working toward?", cx);
            input.set_text(objective_text, cx);
            input
        });
        let budget = cx.new(|cx| {
            let mut input = ComposerInput::new("Optional token budget", cx).with_single_line();
            input.set_text(budget_text, cx);
            input
        });
        let subscriptions = vec![
            cx.observe(&objective, |_: &mut Self, _, cx| cx.notify()),
            cx.observe(&budget, |_: &mut Self, _, cx| cx.notify()),
        ];
        self.goal_ui.editor = Some(GoalEditor {
            chat_id,
            objective,
            budget,
            creating: goal.is_none(),
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn save_goal_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.goal_ui.editor.as_ref() else {
            return;
        };
        if self.state.read(cx).selected_chat.as_deref() != Some(editor.chat_id.as_str()) {
            return;
        }
        let objective = editor.objective.read(cx).text().trim().to_owned();
        let budget = editor.budget.read(cx).text().trim().to_owned();
        let token_budget = if budget.is_empty() {
            GoalBudget::Unlimited
        } else {
            match budget.parse::<i64>() {
                Ok(tokens) if tokens > 0 => GoalBudget::Limited { tokens },
                _ => {
                    self.goal_ui.error = Some((
                        editor.chat_id.clone(),
                        "Enter a positive token budget, or leave it blank for no budget.".into(),
                    ));
                    cx.notify();
                    return;
                }
            }
        };
        let operation = GoalOperation::Set {
            objective: Some(objective),
            status: editor.creating.then_some(GoalStatus::Active),
            token_budget: Some(token_budget),
        };
        self.submit_goal_operation(operation, cx);
    }

    fn submit_goal_operation(&mut self, operation: GoalOperation, cx: &mut Context<Self>) {
        if self.goal_ui.pending.is_some() {
            return;
        }
        let state = self.state.read(cx);
        let Some(chat_id) = state.selected_chat.clone() else {
            return;
        };
        let Some(engine) = state.engine().cloned() else {
            return;
        };
        if let Err(error) = operation.validate() {
            self.goal_ui.error = Some((chat_id, error.into()));
            cx.notify();
            return;
        }
        self.goal_ui.pending = Some(chat_id.clone());
        self.goal_ui.error = None;
        self.goal_ui.task = Some(cx.spawn(async move |this, cx| {
            let intent = serde_json::json!({"kind": "goal", "operation": operation});
            let result = crate::task_commands::execute(&engine, &chat_id, intent).await;
            this.update(cx, |composer, cx| {
                composer.goal_ui.pending = None;
                match result {
                    Ok(receipt) if receipt.status == CommandReceiptStatus::Applied => {
                        if composer.goal_ui.editor.as_ref().is_some_and(|editor| editor.chat_id == chat_id) {
                            composer.goal_ui.editor = None;
                        }
                    }
                    Ok(_) => composer.goal_ui.error = Some((chat_id.clone(), "Goal action is pending or unconfirmed. Refresh readback before another change.".into())),
                    Err(error) => composer.goal_ui.error = Some((chat_id.clone(), format!("Goal action failed or is unconfirmed: {error}"))),
                }
                cx.notify();
            }).ok();
        }));
        cx.notify();
    }

    fn goal_button(
        &self,
        id: &'static str,
        label: &'static str,
        enabled: bool,
        action: GoalAction,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx);
        let owner = self.state.read(cx).selected_chat.clone();
        let accessible_label = match id {
            "goal-refresh" => "Refresh goal status",
            "goal-toggle" if label == "Pause" => "Pause goal",
            "goal-toggle" => "Resume goal",
            "goal-complete" => "Mark goal complete",
            "goal-cancel" => "Cancel goal edit",
            _ => label,
        };
        gpui_base::Button::new(id)
            .role(Role::Button)
            .accessibility_id(id)
            .accessibility_label(accessible_label)
            .disabled(!enabled)
            .tab_index(0)
            .tab_stop(enabled)
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted)
            .opacity(if enabled { 1.0 } else { 0.5 })
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.surface_raised_hover))
                    .focus_visible(move |style| {
                        style
                            .bg(theme.accent.opacity(0.18))
                            .text_color(theme.accent)
                    })
            })
            .when(!enabled, |button| {
                button.aria_description("This goal action is currently unavailable.")
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.goal_ui.pending.is_some() || this.state.read(cx).selected_chat != owner {
                    return;
                }
                match &action {
                    GoalAction::Edit => this.open_goal_editor(cx),
                    GoalAction::Save => this.save_goal_editor(cx),
                    GoalAction::Cancel => {
                        this.goal_ui.editor = None;
                        cx.notify();
                    }
                    GoalAction::Operation(operation) => {
                        this.submit_goal_operation(operation.clone(), cx)
                    }
                }
            }))
            .child(label)
            .into_any_element()
    }

    pub(super) fn render_goal_controls(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.state.read(cx);
        let chat = state.selected_chat_row()?;
        if chat
            .config
            .as_ref()
            .is_none_or(|config| config.harness != HarnessId::Codex)
        {
            return None;
        }
        let chat_id = chat.id.clone();
        let readback = state.context_control.goal.clone();
        let goal = readback
            .as_ref()
            .and_then(|readback| readback.goal.as_ref());
        let pending = self.goal_ui.pending.is_some();
        let theme = Theme::of(cx);
        let mut row = div()
            .id("goal-controls")
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .px(px(8.0))
            .py(px(8.0))
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted);
        if let Some(goal) = goal {
            let usage = match goal.token_budget {
                Some(budget) => format!(
                    "{} / {} tokens · {} remaining · {}s",
                    goal.tokens_used,
                    budget,
                    budget.saturating_sub(goal.tokens_used).max(0),
                    goal.time_used_seconds
                ),
                None => format!("{} tokens · {}s", goal.tokens_used, goal.time_used_seconds),
            };
            row = row
                .child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(
                            div()
                                .text_color(theme.text)
                                .child(format!("Goal · {}", goal.status.label())),
                        )
                        .child(usage),
                )
                .child(div().text_color(theme.text).child(goal.objective.clone()));
        } else {
            row = row.child(if readback.is_some() {
                "No goal"
            } else {
                "Goal status has not been read"
            });
        }
        if let Some(readback) = &readback {
            let at = chrono::DateTime::from_timestamp_millis(readback.checked_at_ms)
                .map(|time| time.format("%H:%M:%S UTC").to_string())
                .unwrap_or_else(|| "unknown time".into());
            let runtime = if readback.continuation_ready {
                "live subscription observed"
            } else {
                "continuation waits for the next message"
            };
            row = row.child(format!("Last readback {at} · {runtime}"));
        }
        let mut actions = div()
            .flex()
            .flex_wrap()
            .gap(px(4.0))
            .child(self.goal_button(
                "goal-edit",
                if goal.is_some() {
                    "Edit goal"
                } else {
                    "Create goal"
                },
                !pending,
                GoalAction::Edit,
                cx,
            ))
            .child(self.goal_button(
                "goal-refresh",
                "Refresh",
                !pending,
                GoalAction::Operation(GoalOperation::Get),
                cx,
            ));
        if let Some(goal) = goal {
            let (label, status) = if goal.status == GoalStatus::Active {
                ("Pause", GoalStatus::Paused)
            } else {
                ("Resume", GoalStatus::Active)
            };
            actions = actions
                .child(self.goal_button(
                    "goal-toggle",
                    label,
                    !pending,
                    GoalAction::Operation(GoalOperation::Set {
                        objective: None,
                        status: Some(status),
                        token_budget: None,
                    }),
                    cx,
                ))
                .child(self.goal_button(
                    "goal-complete",
                    "Mark complete",
                    !pending && goal.status != GoalStatus::Complete,
                    GoalAction::Operation(GoalOperation::Set {
                        objective: None,
                        status: Some(GoalStatus::Complete),
                        token_budget: None,
                    }),
                    cx,
                ))
                .child(self.goal_button(
                    "goal-clear",
                    "Clear goal",
                    !pending,
                    GoalAction::Operation(GoalOperation::Clear),
                    cx,
                ));
        }
        if pending {
            actions = actions.child("Updating goal…");
        }
        row = row.child(actions);
        if let Some(editor) = self
            .goal_ui
            .editor
            .as_ref()
            .filter(|editor| editor.chat_id == chat_id)
        {
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .p(px(8.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface_raised)
                    .child("Objective")
                    .child(editor.objective.clone())
                    .child("Token budget · blank means no budget")
                    .child(editor.budget.clone())
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(self.goal_button(
                                "goal-save",
                                "Save goal",
                                !pending,
                                GoalAction::Save,
                                cx,
                            ))
                            .child(self.goal_button(
                                "goal-cancel",
                                "Cancel",
                                !pending,
                                GoalAction::Cancel,
                                cx,
                            )),
                    ),
            );
        }
        if let Some((_, error)) = self
            .goal_ui
            .error
            .as_ref()
            .filter(|(owner, _)| owner == &chat_id)
        {
            row = row.child(crate::settings::widgets::error_strip(&theme, error.clone()));
        }
        Some(row.into_any_element())
    }
}
