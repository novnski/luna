//! Native project command settings card and checkout-scoped terminal controls,
//! plus the AW-11 repository layer: commands checked in as
//! `.luna/project.toml` are shown with their provenance and this device's
//! trust, and never get a Run control before their exact content is accepted.
use crate::{
    button::{self, ButtonKind, ControlSize},
    composer::ComposerInput,
    popover::{self, TooltipAboveExt as _},
    settings::widgets,
    state::AppState,
    theme::Theme,
    typography::{TextStyle, TypeStyleExt as _, ui_rems},
};
use gpui::{
    Action, AnyElement, Context, Entity, EventEmitter, IntoElement, Render, SharedString,
    Subscription, Task, Window, div, prelude::*, px,
};
use luna_proto::{
    PROJECT_CONFIG_FILE, ProjectAction, ProjectCommandRun, ProjectCommandSource,
    ProjectCommandStatus, ProjectCommandTrust, ProjectCommandsSnapshot, ProjectConfigState,
    ProjectRepositoryCommand, ProjectRepositoryConfig,
};
use luna_rpc::methods;
use std::collections::HashSet;

/// A kit push button for a command control. GPUI sends Enter/Space on a
/// focused `on_click` control as one keyboard click on key-up; the kit
/// swallows the key-down so it never activates twice.
fn command_button(
    theme: &Theme,
    kind: ButtonKind,
    size: ControlSize,
    enabled: bool,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    button::button(id, kind, size, enabled, theme).aria_label(label.into())
}

/// A run's status as a badge: running reads as live, a failure in the danger
/// tone, everything else neutral.
fn status_badge(theme: &Theme, status: ProjectCommandStatus) -> gpui::Div {
    match status {
        ProjectCommandStatus::Running => widgets::badge_active(theme, status.label()),
        ProjectCommandStatus::Failed => widgets::badge(theme, status.label())
            .border_color(theme.danger.opacity(0.35))
            .text_color(theme.readable_tone(theme.danger_muted, 0.9)),
        ProjectCommandStatus::Pending
        | ProjectCommandStatus::Succeeded
        | ProjectCommandStatus::Canceled => widgets::badge(theme, status.label()),
    }
}

/// Command text in the code face at the Appearance code size. Code surfaces
/// keep their own absolute size (see [`crate::typography::code_font_size`]),
/// so this is the one text size here that is not an interface rem.
fn code_text(theme: &Theme, cx: &gpui::App) -> gpui::Div {
    div()
        .font_family(theme.font_mono.clone())
        .text_size(px(crate::typography::code_font_size(cx)))
        .text_color(theme.code_text)
}

/// A repository command's trust as a badge. Overridden reads neutral: this
/// device's own command runs instead, whatever the repository trust.
fn trust_badge(theme: &Theme, command: &ProjectRepositoryCommand) -> gpui::Div {
    if command.overridden {
        return widgets::badge(theme, "Overridden on this device");
    }
    match command.trust {
        ProjectCommandTrust::Accepted => widgets::badge_active(theme, command.trust.label()),
        ProjectCommandTrust::Unaccepted | ProjectCommandTrust::Changed => {
            widgets::badge(theme, command.trust.label())
                .border_color(theme.warning.opacity(0.35))
                .text_color(theme.readable_tone(theme.warning_muted, 0.9))
        }
    }
}

fn trust_state(command: &ProjectRepositoryCommand) -> &'static str {
    if command.overridden {
        return "overridden";
    }
    match command.trust {
        ProjectCommandTrust::Accepted => "accepted",
        ProjectCommandTrust::Unaccepted => "unaccepted",
        ProjectCommandTrust::Changed => "changed",
    }
}

/// Every line of a repository command, exactly what Accept pins and Run
/// would execute, plus its preview URL. Never truncated.
fn exact_content(theme: &Theme, action: &ProjectAction, cx: &gpui::App) -> gpui::Div {
    let mut lines = code_text(theme, cx).flex().flex_col();
    for line in action.command.trim_end_matches('\n').split('\n') {
        lines = lines.child(div().whitespace_nowrap().child(SharedString::from(
            if line.is_empty() {
                " ".to_string()
            } else {
                line.to_string()
            },
        )));
    }
    let selector = action.id.clone();
    let mut block = div()
        .min_w_0()
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.code_wash)
        .px(ui_rems(10.0))
        .py(ui_rems(8.0))
        .gap(ui_rems(4.0))
        .flex()
        .flex_col()
        .when(cfg!(test), |el| {
            el.debug_selector(move || format!("qa-repo-command-content-{selector}"))
        })
        .child(
            div()
                .id(SharedString::from(format!("exact-command-{}", action.id)))
                .w_full()
                .min_w_0()
                .overflow_x_scroll()
                .child(lines),
        );
    if let Some(url) = &action.preview_url {
        block = block.child(
            div()
                .type_style(TextStyle::Caption)
                .text_color(theme.readable_muted(0.8))
                .child(SharedString::from(format!("Preview: {url}"))),
        );
    }
    block
}

#[derive(Clone, PartialEq, Action)]
#[action(no_json)]
pub struct OpenCommandPreview(pub String);

/// Composed by ProjectsPage. The page owns routing, selection and scrolling.
pub struct ProjectCommandSettings {
    state: Entity<AppState>,
    space_id: String,
    actions: Vec<ProjectAction>,
    /// The first read has answered (successfully or not): until then the
    /// list shows skeleton rows instead of an empty state.
    loaded: bool,
    editing: Option<String>,
    /// The saved command whose Remove is waiting for its confirming press.
    confirm_remove: Option<String>,
    name: Entity<ComposerInput>,
    command: Entity<ComposerInput>,
    preview: Entity<ComposerInput>,
    setup: bool,
    wait: bool,
    busy: bool,
    error: Option<String>,
    task: Option<Task<()>>,
    /// AW-11: the project folder's `.luna/project.toml` with this device's
    /// trust decisions. None until the first read answers.
    repository: Option<ProjectRepositoryConfig>,
    repository_error: Option<String>,
    /// A trust decision is in flight; its controls are disabled.
    trust_busy: bool,
    repository_task: Option<Task<()>>,
}
impl ProjectCommandSettings {
    pub fn new(state: Entity<AppState>, space_id: String, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            state,
            space_id,
            actions: vec![],
            loaded: false,
            editing: None,
            confirm_remove: None,
            name: cx.new(|cx| {
                ComposerInput::new("Command name (Build, Test, Start app…)", cx).with_single_line()
            }),
            command: cx.new(|cx| ComposerInput::new("Command to run", cx)),
            preview: cx
                .new(|cx| ComposerInput::new("Preview URL (optional)", cx).with_single_line()),
            setup: false,
            wait: true,
            busy: false,
            error: None,
            task: None,
            repository: None,
            repository_error: None,
            trust_busy: false,
            repository_task: None,
        };
        view.request(None, cx);
        view.load_repository(cx);
        view
    }
    fn request(&mut self, actions: Option<Vec<ProjectAction>>, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let saving = actions.is_some();
        let method = if saving {
            methods::SAVE_PROJECT_COMMANDS
        } else {
            methods::GET_PROJECT_COMMANDS
        };
        let params = serde_json::json!({"projectId": self.space_id, "actions": actions, "expectedActions": self.actions});
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<Vec<ProjectAction>>(method, params)
                .await;
            let _ = this.update(cx, |view, cx| {
                view.busy = false;
                view.loaded = true;
                match result {
                    Ok(actions) => {
                        view.actions = actions;
                        if saving {
                            view.editing = None;
                            // A saved command may now override (or stop
                            // overriding) a repository command.
                            view.load_repository(cx);
                        }
                    }
                    Err(error) => view.error = Some(error.to_string()),
                }
                if view
                    .confirm_remove
                    .as_ref()
                    .is_some_and(|id| !view.actions.iter().any(|action| &action.id == id))
                {
                    view.confirm_remove = None;
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn edit(&mut self, action: Option<ProjectAction>, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.confirm_remove = None;
        let action = action.unwrap_or_else(|| ProjectAction {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            command: String::new(),
            setup_on_worktree_create: false,
            wait_for_setup: true,
            preview_url: None,
        });
        self.editing = Some(action.id);
        self.setup = action.setup_on_worktree_create;
        self.wait = action.wait_for_setup;
        self.name
            .update(cx, |input, cx| input.set_text(action.name, cx));
        self.command
            .update(cx, |input, cx| input.set_text(action.command, cx));
        self.preview.update(cx, |input, cx| {
            input.set_text(action.preview_url.unwrap_or_default(), cx)
        });
        cx.notify();
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(id) = self.editing.clone() else {
            return;
        };
        let action = ProjectAction {
            id: id.clone(),
            name: self.name.read(cx).text().trim().into(),
            command: self.command.read(cx).text().into(),
            setup_on_worktree_create: self.setup,
            wait_for_setup: self.wait,
            preview_url: Some(self.preview.read(cx).text().trim().to_string())
                .filter(|s| !s.is_empty()),
        };
        let mut actions = self.actions.clone();
        if let Some(old) = actions.iter_mut().find(|a| a.id == id) {
            *old = action;
        } else {
            actions.push(action);
        }
        self.request(Some(actions), cx);
    }

    /// Remove is destructive, so it takes two presses on the same control:
    /// the first arms it (the row asks to confirm), the second removes.
    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.confirm_remove.as_deref() != Some(id) {
            self.confirm_remove = Some(id.to_string());
            cx.notify();
            return;
        }
        self.confirm_remove = None;
        let actions = self
            .actions
            .iter()
            .filter(|action| action.id != id)
            .cloned()
            .collect();
        self.request(Some(actions), cx);
    }

    fn cancel_remove(&mut self, cx: &mut Context<Self>) {
        self.confirm_remove = None;
        cx.notify();
    }

    fn toggle_setup(&mut self, cx: &mut Context<Self>) {
        self.setup = !self.setup;
        cx.notify();
    }

    fn toggle_wait(&mut self, cx: &mut Context<Self>) {
        self.wait = !self.wait;
        cx.notify();
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            self.request(None, cx);
        }
    }

    fn load_repository(&mut self, cx: &mut Context<Self>) {
        // A trust write answers with the fresh layer; never cancel its waiter.
        if self.trust_busy {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::json!({"projectId": self.space_id});
        self.repository_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<ProjectRepositoryConfig>(methods::GET_PROJECT_CONFIGURATION, params)
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(repository) => {
                        view.repository = Some(repository);
                        view.repository_error = None;
                    }
                    Err(error) => view.repository_error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
    }

    /// Accept (or revoke) exactly the content this card shows. The engine
    /// re-reads the file and refuses a digest that no longer matches it.
    /// Accepting never runs the command.
    fn set_trust(
        &mut self,
        command: &ProjectRepositoryCommand,
        trusted: bool,
        cx: &mut Context<Self>,
    ) {
        if self.trust_busy {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.trust_busy = true;
        self.repository_error = None;
        let params = serde_json::json!({
            "projectId": self.space_id,
            "actionId": command.action.id,
            "digest": command.digest,
            "trusted": trusted,
        });
        self.repository_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<ProjectRepositoryConfig>(methods::SET_PROJECT_COMMAND_TRUST, params)
                .await;
            let _ = this.update(cx, |view, cx| {
                view.trust_busy = false;
                match result {
                    Ok(repository) => view.repository = Some(repository),
                    Err(error) => view.repository_error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn reload_repository(&mut self, cx: &mut Context<Self>) {
        self.repository_error = None;
        self.load_repository(cx);
        cx.notify();
    }

    fn repository_command(&self, id: &str) -> Option<&ProjectRepositoryCommand> {
        self.repository
            .as_ref()?
            .commands
            .iter()
            .find(|command| command.action.id == id)
    }

    /// One saved command: name, the command's first line, its badges, and
    /// Edit / Remove at the right (Remove asks to confirm in place).
    fn command_row(
        &self,
        action: &ProjectAction,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let confirming = self.confirm_remove.as_deref() == Some(action.id.as_str());
        let overrides = self.repository_command(&action.id).is_some();
        let name: SharedString = if action.name.trim().is_empty() {
            "Untitled command".into()
        } else {
            action.name.clone().into()
        };
        let first_line = action
            .command
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        let multi_line = action.command.trim_end().contains('\n');
        let mut text = div()
            .flex_1()
            .min_w(ui_rems(widgets::ROW_TEXT_MIN))
            .flex()
            .flex_col()
            .gap(ui_rems(2.0))
            .child(widgets::row_title(theme, name.clone()));
        if confirming {
            text = text.child(
                widgets::row_description(
                    theme,
                    if overrides {
                        "Remove this command? The repository's command with this ID returns, and runs only if you accepted its exact content."
                    } else {
                        "Remove this command from the project?"
                    },
                )
                .text_color(theme.readable_tone(theme.danger_muted, 0.9)),
            );
        } else if !first_line.trim().is_empty() {
            // User data: one line, truncated, with the whole command on hover.
            text = text.child(
                code_text(theme, cx)
                    .id(SharedString::from(format!("command-preview-{}", action.id)))
                    .min_w_0()
                    .truncate()
                    .text_color(theme.readable_muted(0.85))
                    .text_tooltip_above(action.command.clone())
                    .child(SharedString::from(if multi_line {
                        format!("{first_line} …")
                    } else {
                        first_line
                    })),
            );
        }
        let mut controls = widgets::row_controls();
        if !confirming {
            if overrides {
                let selector = action.id.clone();
                controls = controls.child(
                    widgets::badge(theme, "Overrides repository").when(cfg!(test), |el| {
                        el.debug_selector(move || format!("qa-command-overrides-{selector}"))
                    }),
                );
            }
            if action.setup_on_worktree_create {
                controls = controls.child(widgets::badge(theme, "Worktree setup"));
            }
            if action.preview_url.is_some() {
                controls = controls.child(widgets::badge(theme, "Preview"));
            }
        }
        let edit_click = action.clone();
        let edit_selector_id = action.id.clone();
        let remove_selector_id = action.id.clone();
        let remove_click_id = action.id.clone();
        controls = if confirming {
            controls.child(
                command_button(
                    theme,
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    !self.busy,
                    SharedString::from(format!("keep-command-{}", action.id)),
                    format!("Keep project command: {}", action.name),
                )
                .child("Cancel")
                .when(!self.busy, |el| {
                    el.on_click(cx.listener(|view, _, _, cx| view.cancel_remove(cx)))
                }),
            )
        } else {
            controls.child(
                command_button(
                    theme,
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    !self.busy,
                    SharedString::from(format!("edit-command-{}", action.id)),
                    format!("Edit project command: {}", action.name),
                )
                .when(cfg!(test), |el| {
                    el.debug_selector(move || format!("qa-command-edit-{edit_selector_id}"))
                })
                .child("Edit")
                .when(!self.busy, |el| {
                    el.on_click(
                        cx.listener(move |view, _, _, cx| view.edit(Some(edit_click.clone()), cx)),
                    )
                }),
            )
        };
        // The same control (and so the same focus) arms and confirms; only
        // the armed press is the loud, destructive one.
        controls = controls.child(
            command_button(
                theme,
                if confirming {
                    ButtonKind::Danger
                } else {
                    ButtonKind::Secondary
                },
                ControlSize::Sm,
                !self.busy,
                SharedString::from(format!("remove-command-{}", action.id)),
                if confirming {
                    format!("Confirm removing project command: {}", action.name)
                } else {
                    format!("Remove project command: {}", action.name)
                },
            )
            .when(cfg!(test), |el| {
                el.debug_selector(move || format!("qa-command-remove-{remove_selector_id}"))
            })
            .child("Remove")
            .when(!self.busy, |el| {
                el.on_click(cx.listener(move |view, _, _, cx| {
                    view.remove(&remove_click_id, cx);
                }))
            }),
        );
        widgets::card_row(theme, false)
            .child(widgets::row_tile(theme, crate::icons::TERMINAL))
            .child(text)
            .child(controls)
    }

    /// The draft editor, as rows under the list: three fields, the two
    /// setup switches, then Cancel and Save at the right.
    fn editor_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let field = |title: &'static str,
                     description: Option<&'static str>,
                     input: AnyElement,
                     min_h: Option<f32>| {
            widgets::card_row(theme, false)
                .child(widgets::row_text(
                    theme,
                    title,
                    description.map(SharedString::from),
                ))
                .child(
                    widgets::row_block().mt_0().child(
                        div()
                            .w_full()
                            .when_some(min_h, |el, min_h| el.min_h(ui_rems(min_h)))
                            .child(popover::dialog_field(input)),
                    ),
                )
                .into_any_element()
        };
        let creating = self
            .editing
            .as_ref()
            .is_none_or(|id| !self.actions.iter().any(|action| &action.id == id));
        let overriding = creating
            && self
                .editing
                .as_deref()
                .is_some_and(|id| self.repository_command(id).is_some());
        vec![
            widgets::card_row(theme, false)
                .child(
                    widgets::row_title(
                        theme,
                        if overriding {
                            "Override repository command on this device"
                        } else if creating {
                            "New command"
                        } else {
                            "Edit command"
                        },
                    )
                    .font_weight(gpui::FontWeight::SEMIBOLD),
                )
                .into_any_element(),
            field("Name", None, self.name.clone().into_any_element(), None),
            field(
                "Command",
                Some("Runs with /bin/sh in the task's checkout, so shell operators and environment variables work."),
                self.command.clone().into_any_element(),
                Some(56.0),
            ),
            field(
                "Preview URL",
                Some("Optional. Adds a Preview button beside the command in Terminal → Commands."),
                self.preview.clone().into_any_element(),
                None,
            ),
            widgets::card_row(theme, false)
                .child(widgets::row_text(
                    theme,
                    "Run on new worktree creation",
                    Some("Setup runs only for newly created worktrees. Interrupted or failed setup stays visible and needs an explicit retry. Editing this command doesn't rerun existing setup.".into()),
                ))
                .child(
                    widgets::row_controls().child(
                        widgets::switch_control(
                            "command-setup",
                            "Run on new worktree creation",
                            self.setup,
                            theme,
                            cx,
                            |view, cx| view.toggle_setup(cx),
                        )
                        .when(cfg!(test), |el| el.debug_selector(|| "qa-command-setup".into())),
                    ),
                )
                .into_any_element(),
            widgets::card_row(theme, false)
                .child(widgets::row_text(
                    theme,
                    "Wait for setup before dependent work",
                    Some("When off, work can start while setup is still running.".into()),
                ))
                .child(
                    widgets::row_controls().child(
                        widgets::switch_control(
                            "command-wait",
                            "Wait for setup before dependent work",
                            self.wait,
                            theme,
                            cx,
                            |view, cx| view.toggle_wait(cx),
                        )
                        .when(cfg!(test), |el| el.debug_selector(|| "qa-command-wait".into())),
                    ),
                )
                .into_any_element(),
            widgets::card_row(theme, false)
                .child(
                    widgets::row_controls()
                        .child(
                            command_button(
                                theme,
                                ButtonKind::Secondary,
                                ControlSize::Md,
                                true,
                                "cancel-command-edit",
                                "Cancel command edit",
                            )
                            .when(cfg!(test), |el| el.debug_selector(|| "qa-command-cancel".into()))
                            .child("Cancel")
                            .on_click(cx.listener(|view, _, _, cx| view.cancel_edit(cx))),
                        )
                        .child(
                            command_button(
                                theme,
                                ButtonKind::Primary,
                                ControlSize::Md,
                                !self.busy,
                                "save-command",
                                "Save project command",
                            )
                            .when(cfg!(test), |el| el.debug_selector(|| "qa-command-save".into()))
                            .child(if self.busy { "Saving…" } else { "Save command" })
                            .when(!self.busy, |el| {
                                el.on_click(cx.listener(|view, _, _, cx| view.save(cx)))
                            }),
                        ),
                )
                .into_any_element(),
        ]
    }

    /// One repository command: provenance tile, name, its exact content
    /// until accepted (one line once accepted), the trust badge, and Accept
    /// or Revoke plus Override. An overridden command has no trust controls:
    /// this device's command with the same ID runs instead.
    fn repository_row(
        &self,
        command: &ProjectRepositoryCommand,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let action = &command.action;
        let id = action.id.clone();
        let mut text = div()
            .flex_1()
            .min_w(ui_rems(widgets::ROW_TEXT_MIN))
            .flex()
            .flex_col()
            .gap(ui_rems(4.0))
            .child(widgets::row_title(
                theme,
                SharedString::from(action.name.clone()),
            ));
        if command.trust == ProjectCommandTrust::Accepted || command.overridden {
            let first_line = action
                .command
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            let multi_line = action.command.trim_end().contains('\n');
            text = text.child(
                code_text(theme, cx)
                    .id(SharedString::from(format!("repo-command-preview-{id}")))
                    .min_w_0()
                    .truncate()
                    .text_color(theme.readable_muted(0.85))
                    .text_tooltip_above(action.command.clone())
                    .child(SharedString::from(if multi_line {
                        format!("{first_line} …")
                    } else {
                        first_line
                    })),
            );
        } else {
            text = text.child(exact_content(theme, action, cx));
        }
        if command.overridden {
            text = text.child(widgets::row_description(
                theme,
                "Your command with this ID runs instead on this device.",
            ));
        } else if command.trust == ProjectCommandTrust::Changed {
            text = text.child(widgets::row_description(
                theme,
                "This content changed since you accepted it. It won't run until you accept it again.",
            ));
        }
        let state_selector = format!("qa-repo-command-state-{id}-{}", trust_state(command));
        let mut controls = widgets::row_controls().child(
            trust_badge(theme, command)
                .when(cfg!(test), |el| el.debug_selector(move || state_selector)),
        );
        if !command.overridden {
            let accepted = command.trust == ProjectCommandTrust::Accepted;
            let override_action = action.clone();
            let override_selector = id.clone();
            controls = controls.child(
                command_button(
                    theme,
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    !self.busy,
                    SharedString::from(format!("override-repo-command-{id}")),
                    format!(
                        "Override repository command on this device: {}",
                        action.name
                    ),
                )
                .when(cfg!(test), |el| {
                    el.debug_selector(move || {
                        format!("qa-repo-command-override-{override_selector}")
                    })
                })
                .child("Override")
                .when(!self.busy, |el| {
                    el.on_click(cx.listener(move |view, _, _, cx| {
                        view.edit(Some(override_action.clone()), cx)
                    }))
                }),
            );
            let decision = command.clone();
            let trust_selector = id.clone();
            controls = controls.child(
                command_button(
                    theme,
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    !self.trust_busy,
                    SharedString::from(format!(
                        "{}-repo-command-{id}",
                        if accepted { "revoke" } else { "accept" }
                    )),
                    if accepted {
                        format!("Revoke acceptance of repository command: {}", action.name)
                    } else {
                        format!(
                            "Accept this exact repository command content: {}",
                            action.name
                        )
                    },
                )
                .when(cfg!(test), |el| {
                    el.debug_selector(move || {
                        format!(
                            "qa-repo-command-{}-{trust_selector}",
                            if accepted { "revoke" } else { "accept" }
                        )
                    })
                })
                .when(!accepted, |el| {
                    el.child(button::glyph(crate::icons::SHIELD_CHECK, ControlSize::Sm))
                })
                .child(if accepted { "Revoke" } else { "Accept" })
                .when(!self.trust_busy, |el| {
                    el.on_click(
                        cx.listener(move |view, _, _, cx| view.set_trust(&decision, !accepted, cx)),
                    )
                }),
            );
        }
        widgets::card_row(theme, false)
            .child(widgets::row_tile(theme, crate::icons::FILE_CODE))
            .child(text)
            .child(controls)
    }

    /// The repository layer, read from the project folder. A task in another
    /// checkout (a worktree on another branch) reads its own copy of the file
    /// in Terminal → Commands.
    fn repository_card(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let mut card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "Repository commands",
            Some(SharedString::from(format!(
                "Shared through the project's {PROJECT_CONFIG_FILE}. They never run until you accept their exact content on this device, and any change needs a new acceptance. A command on this device with the same ID takes precedence."
            ))),
            vec![],
        ));
        match &self.repository {
            None if self.repository_error.is_none() => {
                card = card.child(
                    widgets::card_row(theme, false).child(widgets::row_description(
                        theme,
                        format!("Checking {PROJECT_CONFIG_FILE}…"),
                    )),
                );
            }
            None => {}
            Some(repository) => match repository.state {
                ProjectConfigState::Absent => {
                    card = card.child(widgets::card_row(theme, false).child(
                        widgets::row_description(
                            theme,
                            format!(
                                "No {} in {}. Check one in to share commands with everyone who opens this repository.",
                                repository.file, repository.checkout
                            ),
                        ),
                    ));
                }
                ProjectConfigState::Refused | ProjectConfigState::Unavailable => {
                    let detail = repository.detail.clone().unwrap_or_default();
                    let message = if repository.state == ProjectConfigState::Refused {
                        format!(
                            "LUNA ignored {}: {detail} Commands saved on this device keep working.",
                            repository.file
                        )
                    } else {
                        format!("Couldn't read {}: {detail}", repository.file)
                    };
                    card = card.child(
                        widgets::card_row(theme, false).child(
                            widgets::warning_strip(theme, message)
                                .mt_0()
                                .w_full()
                                .when(cfg!(test), |el| {
                                    el.debug_selector(|| "qa-repo-config-refused".into())
                                }),
                        ),
                    );
                }
                ProjectConfigState::Loaded if repository.commands.is_empty() => {
                    card = card.child(widgets::card_row(theme, false).child(
                        widgets::row_description(
                            theme,
                            format!("{} declares no commands.", repository.file),
                        ),
                    ));
                }
                ProjectConfigState::Loaded => {
                    for command in &repository.commands {
                        card = card.child(self.repository_row(command, theme, cx));
                    }
                }
            },
        }
        if let Some(error) = &self.repository_error {
            card = card.child(
                widgets::card_row(theme, false)
                    .child(widgets::error_strip(theme, error.clone()).mt_0().w_full())
                    .child(
                        widgets::row_controls().child(
                            command_button(
                                theme,
                                ButtonKind::Secondary,
                                ControlSize::Sm,
                                !self.trust_busy,
                                "reload-project-configuration",
                                "Reload repository commands",
                            )
                            .child(button::glyph(crate::icons::REFRESH, ControlSize::Sm))
                            .child("Reload")
                            .when(!self.trust_busy, |el| {
                                el.on_click(
                                    cx.listener(|view, _, _, cx| view.reload_repository(cx)),
                                )
                            }),
                        ),
                    ),
            );
        }
        card
    }
}
impl Render for ProjectCommandSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let connected = self.state.read(cx).engine().is_some();
        let editing = self.editing.is_some();
        // The section action sits in the header. It hides while a draft is
        // open, so a second press cannot replace the draft.
        let mut header_actions = Vec::new();
        if !editing {
            header_actions.push(
                command_button(
                    &theme,
                    ButtonKind::Secondary,
                    ControlSize::Md,
                    !self.busy,
                    "add-project-command",
                    "Add project command",
                )
                .child(button::glyph(crate::icons::PLUS, ControlSize::Md))
                .child("Add command")
                .when(!self.busy, |el| {
                    el.on_click(cx.listener(|view, _, _, cx| view.edit(None, cx)))
                })
                .into_any_element(),
            );
        }
        let mut card = widgets::section_card(&theme).child(widgets::section_header(
            &theme,
            "Commands",
            Some("Saved on this device. Run them from Terminal → Commands in the selected task's checkout.".into()),
            header_actions,
        ));
        if !connected {
            card = card.child(
                widgets::card_row(&theme, false).child(
                    widgets::warning_strip(
                        &theme,
                        "Connect to this project's device to load and edit its commands.",
                    )
                    .mt_0()
                    .w_full(),
                ),
            );
        } else if !self.loaded && self.busy {
            card = card.child(
                div()
                    .w_full()
                    .px(ui_rems(widgets::ROW_PAD_X))
                    .py(ui_rems(widgets::ROW_PAD_Y))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(popover::skeleton_rows(
                        "project-commands-loading",
                        &theme,
                        2,
                        cx.entity_id(),
                        cx,
                    )),
            );
        } else if self.actions.is_empty() && !editing && self.error.is_none() {
            card = card.child(
                widgets::card_row(&theme, false).child(widgets::row_description(
                    &theme,
                    "No commands yet. Add Build, Test or Start app to run it from the terminal.",
                )),
            );
        }
        for action in self.actions.clone() {
            card = card.child(self.command_row(&action, &theme, cx));
        }
        if editing {
            card = card.children(self.editor_rows(&theme, cx));
        }
        if let Some(error) = &self.error {
            card = card.child(
                widgets::card_row(&theme, false)
                    .child(widgets::error_strip(&theme, error.clone()).mt_0().w_full())
                    .child(
                        widgets::row_controls().child(
                            command_button(
                                &theme,
                                ButtonKind::Secondary,
                                ControlSize::Sm,
                                !self.busy,
                                "reload-project-commands",
                                "Reload saved project commands and keep draft",
                            )
                            .child(button::glyph(crate::icons::REFRESH, ControlSize::Sm))
                            .child("Reload saved commands")
                            .when(!self.busy, |el| {
                                el.on_click(cx.listener(|view, _, _, cx| view.reload(cx)))
                            }),
                        ),
                    ),
            );
        }
        let mut column = div().w_full().flex().flex_col().child(card).child(widgets::section_footer(
            &theme,
            "Configuration and receipts are included in backups. Terminal output follows local terminal retention and is excluded from backups. Stop controls foreground processes; commands that detach themselves must manage their own lifecycle.",
        ));
        if connected {
            column = column
                .child(self.repository_card(&theme, cx))
                .child(widgets::section_footer(
                    &theme,
                    "Acceptance is saved on this device only and pins the command text, not scripts it calls. Repository commands never run on worktree creation.",
                ));
        }
        column
    }
}

/// Origin of a Commands view/request. The consumer checks current AppState as
/// well as the producer so queued Output events cannot cross a replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectCommandScope {
    engine_generation: u64,
    engine_device: Option<String>,
    chat: Option<String>,
    project: Option<String>,
    cwd: Option<String>,
    checkout: Option<String>,
}
impl ProjectCommandScope {
    fn capture(state: &AppState) -> Self {
        let task = state.selected_chat_row();
        Self {
            engine_generation: state.engine_generation(),
            engine_device: state
                .engine()
                .map(|engine| engine.engine_info().device_id.clone()),
            chat: state.selected_chat.clone(),
            // The selected task can move project or materialize a checkout
            // without changing its chat ID or engine. Labels are not identity.
            project: task.and_then(|task| task.space_id.clone()),
            cwd: task.and_then(|task| task.cwd.clone()),
            checkout: task.and_then(|task| task.checkout_id.clone()),
        }
    }
    pub(crate) fn is_current(&self, state: &AppState) -> bool {
        *self == Self::capture(state)
    }
}

pub enum ProjectCommandEvent {
    Output {
        scope: ProjectCommandScope,
        chat_id: String,
        terminal_id: String,
    },
}
pub struct ProjectCommandPanel {
    state: Entity<AppState>,
    scope: ProjectCommandScope,
    snapshot: Option<ProjectCommandsSnapshot>,
    expanded: bool,
    expanded_command_run: Option<String>,
    /// The repository command whose exact content is disclosed for review.
    reviewing: Option<String>,
    busy: bool,
    write_pending: bool,
    error: Option<String>,
    task: Option<Task<()>>,
    _observe: Subscription,
    _poll: Task<()>,
}
impl EventEmitter<ProjectCommandEvent> for ProjectCommandPanel {}
impl ProjectCommandPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |view: &mut Self, _, cx| {
            if view.sync_scope(cx) {
                if view.expanded {
                    view.load(cx);
                }
                cx.notify();
            }
        });
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                if this
                    .update(cx, |view, cx| {
                        if view.expanded && !view.busy {
                            view.load(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let scope = ProjectCommandScope::capture(state.read(cx));
        Self {
            state,
            scope,
            snapshot: None,
            expanded: false,
            expanded_command_run: None,
            reviewing: None,
            busy: false,
            write_pending: false,
            error: None,
            task: None,
            _observe: observe,
            _poll: poll,
        }
    }
    fn sync_scope(&mut self, cx: &mut Context<Self>) -> bool {
        let scope = ProjectCommandScope::capture(self.state.read(cx));
        if self.scope == scope {
            return false;
        }
        self.scope = scope;
        // Cancel only this view's waiter. Accepted commands and their PTYs
        // remain owned by the original engine until explicitly stopped there.
        self.task = None;
        self.snapshot = None;
        self.expanded_command_run = None;
        self.reviewing = None;
        self.error = None;
        self.busy = false;
        self.write_pending = false;
        true
    }
    fn load(&mut self, cx: &mut Context<Self>) {
        self.sync_scope(cx);
        if self.write_pending {
            return;
        }
        let scope = self.scope.clone();
        let Some(chat) = scope.chat.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.busy = true;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<ProjectCommandsSnapshot>(
                    methods::PROJECT_COMMAND_STATUS,
                    serde_json::json!({"chatId": chat}),
                )
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.scope != scope || !scope.is_current(view.state.read(cx)) {
                    return;
                }
                view.busy = false;
                match result {
                    Ok(snapshot) => {
                        view.retain_disclosed_run(&snapshot);
                        view.snapshot = Some(snapshot);
                    }
                    Err(e) => {
                        view.error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        }));
    }
    fn retain_disclosed_run(&mut self, snapshot: &ProjectCommandsSnapshot) {
        if self
            .expanded_command_run
            .as_ref()
            .is_some_and(|id| !snapshot.runs.iter().any(|run| &run.id == id))
        {
            self.expanded_command_run = None;
        }
    }
    fn execute(
        &mut self,
        method: &'static str,
        action_id: String,
        run_id: String,
        cx: &mut Context<Self>,
    ) {
        self.sync_scope(cx);
        // Stop may supersede a status read for a currently running command,
        // but a painted control cannot start a second write.
        if self.write_pending || (self.busy && method != methods::STOP_PROJECT_COMMAND) {
            return;
        }
        let scope = self.scope.clone();
        let Some(chat) = scope.chat.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        // Discard an in-flight status waiter before starting Stop. Its older
        // snapshot must not replace the stopped run after this write.
        self.task = None;
        self.busy = true;
        self.write_pending = true;
        self.error = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call_as::<ProjectCommandRun>(method,
                serde_json::json!({"chatId":chat,"actionId":action_id,"runId":run_id,"requestId":uuid::Uuid::new_v4().to_string()})).await;
            let _ = this.update(cx, |view, cx| {
                if view.scope != scope || !scope.is_current(view.state.read(cx)) { return; }
                view.busy = false;
                view.write_pending = false;
                match result {
                    Ok(run) => {
                        if method == methods::STOP_PROJECT_COMMAND
                            && let Some(current) = view.snapshot.as_mut().and_then(|snapshot| snapshot.runs.iter_mut().find(|current| current.id == run.id))
                        {
                            *current = run.clone();
                        }
                        if let Some(id) = run.terminal_id {
                            cx.emit(ProjectCommandEvent::Output { scope: scope.clone(), chat_id: run.chat_id, terminal_id: id });
                        }
                        view.load(cx);
                    }
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Accept exactly the reviewed content of one repository command for
    /// this checkout's file. It never runs it: the next status read only
    /// shows it as runnable.
    fn accept(
        &mut self,
        project: String,
        action_id: String,
        digest: String,
        cx: &mut Context<Self>,
    ) {
        self.sync_scope(cx);
        if self.write_pending || self.busy {
            return;
        }
        let scope = self.scope.clone();
        let Some(chat) = scope.chat.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.task = None;
        self.busy = true;
        self.write_pending = true;
        self.error = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<ProjectRepositoryConfig>(
                    methods::SET_PROJECT_COMMAND_TRUST,
                    serde_json::json!({"projectId":project,"chatId":chat,"actionId":action_id,"digest":digest,"trusted":true}),
                )
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.scope != scope || !scope.is_current(view.state.read(cx)) {
                    return;
                }
                view.busy = false;
                view.write_pending = false;
                match result {
                    Ok(_) => {
                        view.reviewing = None;
                        view.load(cx);
                    }
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// A repository command this device has not accepted, or that changed
    /// since: no Run. Review discloses its exact content; only then is
    /// Accept offered.
    fn review_row(
        &self,
        command: ProjectRepositoryCommand,
        project: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let id = command.action.id.clone();
        let reviewing = self.reviewing.as_deref() == Some(id.as_str());
        let toggle_origin = self.scope.clone();
        let toggle_id = id.clone();
        let review_selector = id.clone();
        let mut controls = div()
            .flex_none()
            .ml_auto()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui_rems(6.0))
            .child(
                command_button(
                    theme,
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    true,
                    SharedString::from(format!("review-repo-command-{id}")),
                    if reviewing {
                        format!("Hide the content of {}", command.action.name)
                    } else {
                        format!("Review the exact content of {}", command.action.name)
                    },
                )
                .aria_expanded(reviewing)
                .when(cfg!(test), |el| {
                    el.debug_selector(move || format!("qa-repo-command-review-{review_selector}"))
                })
                .child(button::glyph(
                    if reviewing {
                        crate::icons::ALT_ARROW_UP
                    } else {
                        crate::icons::EYE
                    },
                    ControlSize::Sm,
                ))
                .child(if reviewing { "Hide" } else { "Review" })
                .on_click(cx.listener(move |view, _, _, cx| {
                    if !toggle_origin.is_current(view.state.read(cx)) {
                        return;
                    }
                    view.reviewing = if view.reviewing.as_deref() == Some(toggle_id.as_str()) {
                        None
                    } else {
                        Some(toggle_id.clone())
                    };
                    cx.notify();
                })),
            );
        if reviewing {
            let origin = self.scope.clone();
            let accept_selector = id.clone();
            let action_id = id.clone();
            let digest = command.digest.clone();
            controls = controls.child(
                command_button(
                    theme,
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    !self.busy,
                    SharedString::from(format!("accept-repo-command-{id}")),
                    format!("Accept this exact content of {}", command.action.name),
                )
                .when(cfg!(test), |el| {
                    el.debug_selector(move || format!("qa-repo-command-accept-{accept_selector}"))
                })
                .child(button::glyph(crate::icons::SHIELD_CHECK, ControlSize::Sm))
                .child("Accept")
                .when(!self.busy, |el| {
                    el.on_click(cx.listener(move |view, _, _, cx| {
                        if !origin.is_current(view.state.read(cx)) {
                            return;
                        }
                        view.accept(project.clone(), action_id.clone(), digest.clone(), cx)
                    }))
                }),
            );
        }
        let summary = div()
            .flex_1()
            .min_w(ui_rems(140.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(ui_rems(6.0))
            .child(
                div()
                    .min_w_0()
                    .type_style(TextStyle::Footnote)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(command.action.name.clone())),
            )
            .child(trust_badge(theme, &command))
            .child(
                div()
                    .type_style(TextStyle::Caption)
                    .text_color(theme.readable_muted(0.8))
                    .child(SharedString::from(format!("From {PROJECT_CONFIG_FILE}"))),
            );
        let mut row = div().w_full().flex().flex_col().gap(ui_rems(4.0)).child(
            div()
                .w_full()
                .min_h(ui_rems(28.0))
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(ui_rems(8.0))
                .child(summary)
                .child(controls),
        );
        if reviewing {
            row = row.child(exact_content(theme, &command.action, cx)).child(
                widgets::row_description(
                    theme,
                    "Accepting lets Run execute exactly this content on this device. It doesn't run it, and any later change needs a new acceptance.",
                ),
            );
        }
        row
    }

    /// One runnable command: its name and, at the right, its provenance when
    /// it comes from the repository, Preview (when it has a URL) and Run.
    fn action_row(
        &self,
        action: ProjectAction,
        repository: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let action_id = action.id.clone();
        let origin = self.scope.clone();
        let selector_id = action.id.clone();
        let mut controls = div()
            .flex_none()
            .ml_auto()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui_rems(6.0));
        if repository {
            let badge_selector = action.id.clone();
            controls = controls.child(widgets::badge(theme, "Repository").when(cfg!(test), |el| {
                el.debug_selector(move || format!("qa-command-repository-{badge_selector}"))
            }));
        }
        if let Some(url) = action.preview_url.clone() {
            controls = controls.child(
                command_button(
                    theme,
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    true,
                    SharedString::from(format!("preview-command-{}", action.id)),
                    format!("Preview {}", action.name),
                )
                .child(button::glyph(crate::icons::GLOBAL, ControlSize::Sm))
                .child("Preview")
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(OpenCommandPreview(url.clone())), cx)
                }),
            );
        }
        controls = controls.child(
            command_button(
                theme,
                ButtonKind::Secondary,
                ControlSize::Sm,
                !self.busy,
                SharedString::from(format!("run-command-{}", action.id)),
                format!("Run {}", action.name),
            )
            .when(cfg!(test), |el| {
                el.debug_selector(move || format!("qa-command-run-{selector_id}"))
            })
            .child(button::glyph(crate::icons::PLAY, ControlSize::Sm))
            .child("Run")
            .when(!self.busy, |el| {
                el.on_click(cx.listener(move |view, _, _, cx| {
                    if !origin.is_current(view.state.read(cx)) {
                        return;
                    }
                    view.execute(
                        methods::RUN_PROJECT_COMMAND,
                        action_id.clone(),
                        String::new(),
                        cx,
                    )
                }))
            }),
        );
        div()
            .w_full()
            .min_h(ui_rems(28.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(ui_rems(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w(ui_rems(120.0))
                    .type_style(TextStyle::Footnote)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(action.name)),
            )
            .child(controls)
    }

    /// One run receipt: name, status badge and attempt, then Output, the
    /// captured-command disclosure and Stop/Retry at the right. The disclosed
    /// command and any detail sit under the row.
    fn run_row(&self, run: ProjectCommandRun, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let id = run.id.clone();
        let origin = self.scope.clone();
        let active = run.status.is_active();
        let disclosed = self.expanded_command_run.as_deref() == Some(run.id.as_str());
        let disclosure_id = run.id.clone();
        let disclosure_origin = self.scope.clone();
        let control_enabled = !self.busy || (active && !self.write_pending);
        let disclose_selector = run.id.clone();
        let control_selector = run.id.clone();
        let mut controls = div()
            .flex_none()
            .ml_auto()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui_rems(6.0));
        if let Some(terminal_id) = run.terminal_id.clone() {
            let chat = run.chat_id.clone();
            let scope = self.scope.clone();
            let same_chat = self.scope.chat.as_deref() == Some(chat.as_str());
            controls = controls.child(
                command_button(
                    theme,
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    true,
                    SharedString::from(format!("output-command-{}", run.id)),
                    if same_chat {
                        "Show output"
                    } else {
                        "Show output in the original task"
                    },
                )
                .child(button::glyph(crate::icons::TERMINAL, ControlSize::Sm))
                .child(if same_chat {
                    "Output"
                } else {
                    "Output in original task"
                })
                .on_click(cx.listener(move |view, _, _, cx| {
                    if scope.is_current(view.state.read(cx)) {
                        cx.emit(ProjectCommandEvent::Output {
                            scope: scope.clone(),
                            chat_id: chat.clone(),
                            terminal_id: terminal_id.clone(),
                        });
                    }
                })),
            );
        }
        controls = controls
            .child(
                command_button(
                    theme,
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    true,
                    SharedString::from(format!("disclose-command-{}", run.id)),
                    if disclosed {
                        "Hide captured command"
                    } else {
                        "Show captured command"
                    },
                )
                .aria_expanded(disclosed)
                .when(cfg!(test), |el| {
                    el.debug_selector(move || format!("qa-command-disclose-{disclose_selector}"))
                })
                .child(button::glyph(
                    if disclosed {
                        crate::icons::ALT_ARROW_UP
                    } else {
                        crate::icons::ALT_ARROW_DOWN
                    },
                    ControlSize::Sm,
                ))
                .child(if disclosed {
                    "Hide command"
                } else {
                    "Show command"
                })
                .on_click(cx.listener(move |view, _, _, cx| {
                    if !disclosure_origin.is_current(view.state.read(cx))
                        || !view.snapshot.as_ref().is_some_and(|snapshot| {
                            snapshot.runs.iter().any(|run| run.id == disclosure_id)
                        })
                    {
                        return;
                    }
                    view.expanded_command_run =
                        if view.expanded_command_run.as_deref() == Some(disclosure_id.as_str()) {
                            None
                        } else {
                            Some(disclosure_id.clone())
                        };
                    cx.notify();
                })),
            )
            .child(
                command_button(
                    theme,
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    control_enabled,
                    SharedString::from(format!("control-command-{}", run.id)),
                    if active {
                        format!("Stop {}", run.action.name)
                    } else {
                        format!("Retry {}", run.action.name)
                    },
                )
                .when(cfg!(test), |el| {
                    el.debug_selector(move || format!("qa-command-control-{control_selector}"))
                })
                .child(button::glyph(
                    if active {
                        crate::icons::STOP
                    } else {
                        crate::icons::RESTART
                    },
                    ControlSize::Sm,
                ))
                .child(if active { "Stop" } else { "Retry" })
                .when(control_enabled, |el| {
                    el.on_click(cx.listener(move |view, _, _, cx| {
                        if !origin.is_current(view.state.read(cx)) {
                            return;
                        }
                        view.execute(
                            if active {
                                methods::STOP_PROJECT_COMMAND
                            } else {
                                methods::RETRY_PROJECT_COMMAND
                            },
                            String::new(),
                            id.clone(),
                            cx,
                        )
                    }))
                }),
            );
        let summary = div()
            .flex_1()
            .min_w(ui_rems(140.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(ui_rems(6.0))
            .child(
                div()
                    .min_w_0()
                    .type_style(TextStyle::Footnote)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(format!(
                        "{}{}",
                        if run.setup { "Setup: " } else { "" },
                        run.action.name
                    ))),
            )
            .child(status_badge(theme, run.status))
            .when(run.source == ProjectCommandSource::Repository, |el| {
                el.child(widgets::badge(theme, "Repository"))
            })
            .child(
                div()
                    .type_style(TextStyle::Caption)
                    .text_color(theme.readable_muted(0.8))
                    .child(SharedString::from(format!("Attempt {}", run.attempt))),
            );
        let mut row = div().w_full().flex().flex_col().gap(ui_rems(4.0)).child(
            div()
                .w_full()
                .min_h(ui_rems(28.0))
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(ui_rems(8.0))
                .child(summary)
                .child(controls),
        );
        if disclosed {
            let mut command_lines = code_text(theme, cx).flex().flex_col();
            for line in run.action.command.split('\n') {
                command_lines = command_lines.child(div().whitespace_nowrap().child(
                    SharedString::from(if line.is_empty() {
                        " ".to_string()
                    } else {
                        line.to_string()
                    }),
                ));
            }
            row = row.child(
                div()
                    .min_w_0()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.code_wash)
                    .px(ui_rems(10.0))
                    .py(ui_rems(8.0))
                    .gap(ui_rems(4.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .type_style(TextStyle::Caption)
                            .text_color(theme.readable_muted(0.8))
                            .child(SharedString::from(format!(
                                "Captured for attempt {}",
                                run.attempt
                            ))),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("checkout-command-{}", run.id)))
                            .when(cfg!(test), |el| {
                                el.debug_selector(|| {
                                    format!("qa-command-checkout-{}-{}", run.id, run.cwd)
                                })
                            })
                            .min_w_0()
                            .whitespace_normal()
                            .type_style(TextStyle::Caption)
                            .text_color(theme.readable_muted(0.8))
                            .child(SharedString::from(format!("Checkout: {}", run.cwd))),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("captured-command-{}", run.id)))
                            .when(cfg!(test), |el| {
                                el.debug_selector(|| {
                                    format!("qa-command-captured-{}-{}", run.id, run.action.command)
                                })
                            })
                            .w_full()
                            .min_w_0()
                            .overflow_x_scroll()
                            .child(command_lines),
                    ),
            );
        }
        if let Some(detail) = run.detail {
            row = row.child(widgets::row_description(theme, detail));
        }
        row
    }
}
impl Render for ProjectCommandPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut panel = div()
            .flex_none()
            .flex()
            .flex_col()
            .px(ui_rems(12.0))
            .py(ui_rems(4.0))
            .gap(ui_rems(4.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div().flex().flex_row().child(
                    command_button(
                        &theme,
                        ButtonKind::Ghost,
                        ControlSize::Sm,
                        true,
                        "toggle-project-commands",
                        "Commands",
                    )
                    .aria_expanded(self.expanded)
                    .child(button::glyph(
                        if self.expanded {
                            crate::icons::ALT_ARROW_DOWN
                        } else {
                            crate::icons::ALT_ARROW_RIGHT
                        },
                        ControlSize::Sm,
                    ))
                    .child("Commands")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.expanded = !view.expanded;
                        if view.expanded {
                            view.load(cx);
                        }
                        cx.notify();
                    })),
                ),
            );
        if !self.expanded {
            return panel.into_any_element();
        }
        let mut rows = div()
            .id("project-command-list")
            .max_h(ui_rems(180.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(ui_rems(4.0))
            .pb(ui_rems(4.0));
        if let Some(snapshot) = self.snapshot.clone() {
            // User data that may be long: it wraps rather than truncating.
            rows = rows.child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(ui_rems(6.0))
                    .type_style(TextStyle::Caption)
                    .text_color(theme.readable_muted(0.8))
                    .child(
                        div().flex_none().mt(ui_rems(1.0)).child(
                            button::glyph(crate::icons::FOLDER, ControlSize::Sm)
                                .text_color(theme.readable_muted(0.8)),
                        ),
                    )
                    .child(div().min_w_0().child(SharedString::from(snapshot.cwd))),
            );
            let repository = snapshot.repository.clone();
            let accepted: HashSet<String> = repository
                .iter()
                .flat_map(|repository| repository.commands.iter())
                .filter(|command| command.runnable())
                .map(|command| command.action.id.clone())
                .collect();
            let pending: Vec<ProjectRepositoryCommand> = repository
                .iter()
                .flat_map(|repository| repository.commands.iter())
                .filter(|command| !command.runnable() && !command.overridden)
                .cloned()
                .collect();
            if snapshot.actions.is_empty() && pending.is_empty() {
                rows = rows.child(widgets::row_description(
                    &theme,
                    "No saved commands. Add them in Settings → Projects.",
                ));
            }
            for action in snapshot.actions {
                let from_repository = accepted.contains(&action.id);
                rows = rows.child(self.action_row(action, from_repository, &theme, cx));
            }
            if let Some(repository) = &repository
                && matches!(
                    repository.state,
                    ProjectConfigState::Refused | ProjectConfigState::Unavailable
                )
            {
                rows = rows.child(
                    widgets::warning_strip(
                        &theme,
                        format!(
                            "LUNA ignored {}: {}",
                            repository.file,
                            repository.detail.clone().unwrap_or_default()
                        ),
                    )
                    .mt(ui_rems(4.0)),
                );
            }
            for command in pending {
                rows =
                    rows.child(self.review_row(command, snapshot.project_id.clone(), &theme, cx));
            }
            for run in snapshot.runs.into_iter().take(12) {
                rows = rows.child(self.run_row(run, &theme, cx));
            }
        }
        if let Some(error) = &self.error {
            rows = rows.child(widgets::error_strip(&theme, error.clone()).mt(ui_rems(4.0)));
        }
        panel = panel.child(rows);
        panel.into_any_element()
    }
}

#[cfg(test)]
#[path = "project_commands_engine_epoch_tests.rs"]
mod engine_epoch_tests;

#[cfg(test)]
#[path = "project_commands_accessibility_tests.rs"]
mod accessibility_tests;
