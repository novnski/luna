//! Native project command settings card and checkout-scoped terminal controls.
use crate::{composer::ComposerInput, popover, settings::widgets, state::AppState, theme::Theme};
use gpui::{
    Action, Context, Entity, EventEmitter, IntoElement, Render, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};
use luna_proto::{ProjectAction, ProjectCommandRun, ProjectCommandsSnapshot};
use luna_rpc::methods;

fn command_action(theme: &Theme, enabled: bool) -> gpui::Div {
    widgets::ghost_action(theme)
        .when(enabled, |el| {
            el.hover(|style| widgets::ghost_hover(theme, style))
        })
        .when(!enabled, |el| el.opacity(0.5).cursor_default())
}

#[derive(Clone, PartialEq, Action)]
#[action(no_json)]
pub struct OpenCommandPreview(pub String);

/// Composed by ProjectsPage. The page owns routing, selection and scrolling.
pub struct ProjectCommandSettings {
    state: Entity<AppState>,
    space_id: String,
    actions: Vec<ProjectAction>,
    editing: Option<String>,
    name: Entity<ComposerInput>,
    command: Entity<ComposerInput>,
    preview: Entity<ComposerInput>,
    setup: bool,
    wait: bool,
    busy: bool,
    error: Option<String>,
    task: Option<Task<()>>,
}
impl ProjectCommandSettings {
    pub fn new(state: Entity<AppState>, space_id: String, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            state,
            space_id,
            actions: vec![],
            editing: None,
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
        };
        view.request(None, cx);
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
                match result {
                    Ok(actions) => {
                        view.actions = actions;
                        if saving {
                            view.editing = None;
                        }
                    }
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn edit(&mut self, action: Option<ProjectAction>, cx: &mut Context<Self>) {
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
}
impl Render for ProjectCommandSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut card = widgets::section_card(&theme)
            .mt(px(20.0))
            .p(px(16.0))
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(widgets::row_title(&theme, "Project commands"))
                    .when(self.editing.is_none(), |row| {
                        row.child(
                            command_action(&theme, !self.busy)
                                .id("add-project-command")
                                .role(gpui::Role::Button)
                                .aria_label("Add project command")
                                .when(!self.busy, |el| el.tab_index(0))
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.surface_raised)
                                .child(crate::icons::icon(crate::icons::PLUS).size(px(14.0)))
                                .child(if self.busy { "Loading…" } else { "Add command" })
                                .when(!self.busy, |el| {
                                    el.on_click(cx.listener(|view, _, _, cx| view.edit(None, cx)))
                                        .on_key_down(cx.listener(
                                            |view, event: &gpui::KeyDownEvent, _, cx| {
                                                if !event.is_held
                                                    && matches!(
                                                        event.keystroke.key.as_str(),
                                                        "enter" | "space"
                                                    )
                                                {
                                                    view.edit(None, cx);
                                                    cx.stop_propagation();
                                                }
                                            },
                                        ))
                                }),
                        )
                    }),
            )
            .child(widgets::page_subtitle(&theme, "Saved on this device. Run commands from Terminal → Commands in the selected task’s checkout. Foreground commands use /bin/sh; shell operators and environment variables are supported."));
        for action in self.actions.clone() {
            let edit = action.clone();
            let id = action.id.clone();
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .text_color(theme.text)
                            .text_size(crate::typography::ui_rems(12.0))
                            .child(SharedString::from(format!(
                                "{}{}",
                                action.name,
                                if action.setup_on_worktree_create {
                                    " · Worktree setup"
                                } else {
                                    ""
                                }
                            ))),
                    )
                    .child(
                        command_action(&theme, !self.busy)
                            .id(SharedString::from(format!("edit-command-{}", action.id)))
                            .child("Edit")
                            .when(!self.busy, |el| {
                                el.on_click(cx.listener(move |view, _, _, cx| {
                                    view.edit(Some(edit.clone()), cx)
                                }))
                            }),
                    )
                    .child(
                        command_action(&theme, !self.busy)
                            .id(SharedString::from(format!("remove-command-{}", action.id)))
                            .child("Remove")
                            .when(!self.busy, |el| {
                                el.on_click(cx.listener(move |view, _, _, cx| {
                                    let actions = view
                                        .actions
                                        .iter()
                                        .filter(|a| a.id != id)
                                        .cloned()
                                        .collect();
                                    view.request(Some(actions), cx);
                                }))
                            }),
                    ),
            );
        }
        if self.editing.is_some() {
            card = card.child(widgets::field_label(&theme,"Name")).child(popover::dialog_field(self.name.clone().into_any_element()))
                .child(widgets::field_label(&theme,"Command")).child(div().min_h(px(56.0)).child(popover::dialog_field(self.command.clone().into_any_element())))
                .child(widgets::field_label(&theme,"Preview URL")).child(popover::dialog_field(self.preview.clone().into_any_element()))
                .child(command_action(&theme, true).id("command-setup").child(if self.setup { "✓ Run on new worktree creation" } else { "Run on new worktree creation" }).on_click(cx.listener(|view,_,_,cx| { view.setup = !view.setup; cx.notify(); })))
                .child(command_action(&theme, true).id("command-wait").child(if self.wait { "✓ Wait for setup before dependent work" } else { "Allow work while setup runs" }).on_click(cx.listener(|view,_,_,cx| { view.wait = !view.wait; cx.notify(); })))
                .child(widgets::page_subtitle(&theme,"Setup runs only for newly created worktrees. Interrupted or failed setup stays visible and requires an explicit retry. Editing this command does not rerun existing setup."))
                .child(div().flex().gap(px(8.0))
                    .child(command_action(&theme, !self.busy).id("save-command").child(if self.busy { "Saving…" } else { "Save command" }).when(!self.busy, |el| el.on_click(cx.listener(|view,_,_,cx| view.save(cx)))))
                    .child(command_action(&theme, true).id("cancel-command-edit").child("Cancel").on_click(cx.listener(|view,_,_,cx| { view.editing = None; cx.notify(); }))));
        }
        if let Some(error) = &self.error {
            card = card
                .child(widgets::error_strip(&theme, error.clone()))
                .child(
                    command_action(&theme, !self.busy)
                        .id("reload-project-commands")
                        .child("Reload saved commands (keep draft)")
                        .when(!self.busy, |el| {
                            el.on_click(cx.listener(|view, _, _, cx| view.request(None, cx)))
                        }),
                );
        }
        card.child(widgets::page_subtitle(&theme,"Configuration and receipts are included in backups. Terminal output follows local terminal retention and is excluded from backups. Stop controls foreground processes; commands that detach themselves must manage their own lifecycle."))
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
                        if method == methods::STOP_PROJECT_COMMAND {
                            if let Some(current) = view.snapshot.as_mut().and_then(|snapshot| snapshot.runs.iter_mut().find(|current| current.id == run.id)) {
                                *current = run.clone();
                            }
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
}
impl Render for ProjectCommandPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut panel = div()
            .flex_none()
            .flex()
            .flex_col()
            .px(px(12.0))
            .py(px(4.0))
            .gap(px(4.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                command_action(&theme, true)
                    .id("toggle-project-commands")
                    .child(if self.expanded {
                        "▾ Commands"
                    } else {
                        "▸ Commands"
                    })
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.expanded = !view.expanded;
                        if view.expanded {
                            view.load(cx);
                        }
                        cx.notify();
                    })),
            );
        if !self.expanded {
            return panel.into_any_element();
        }
        let mut rows = div()
            .id("project-command-list")
            .max_h(px(180.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(4.0));
        if let Some(snapshot) = self.snapshot.clone() {
            rows = rows.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(snapshot.cwd)),
            );
            if snapshot.actions.is_empty() {
                rows = rows.child(widgets::page_subtitle(
                    &theme,
                    "Add saved commands in Settings → Projects.",
                ));
            }
            for action in snapshot.actions {
                let action_id = action.id.clone();
                let origin = self.scope.clone();
                let mut row = div().flex().items_center().gap(px(8.0)).child(
                    command_action(&theme, !self.busy)
                        .id(SharedString::from(format!("run-command-{}", action.id)))
                        .when(cfg!(test), |el| {
                            el.debug_selector(|| format!("qa-command-run-{}", action.id))
                        })
                        .child(SharedString::from(action.name))
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
                if let Some(url) = action.preview_url {
                    row = row.child(
                        command_action(&theme, true)
                            .id(SharedString::from(format!("preview-command-{}", action.id)))
                            .child("Preview")
                            .on_click(move |_, window, cx| {
                                window
                                    .dispatch_action(Box::new(OpenCommandPreview(url.clone())), cx)
                            }),
                    );
                }
                rows = rows.child(row);
            }
            for run in snapshot.runs.into_iter().take(12) {
                let id = run.id.clone();
                let origin = self.scope.clone();
                let active = run.status.is_active();
                let mut row = div().flex().flex_col().gap(px(2.0)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(format!(
                                    "{}{} · {} · attempt {}",
                                    if run.setup { "Setup: " } else { "" },
                                    run.action.name,
                                    run.status.label(),
                                    run.attempt
                                ))),
                        )
                        .child(
                            command_action(&theme, !self.busy || (active && !self.write_pending))
                                .id(SharedString::from(format!("control-command-{}", run.id)))
                                .when(cfg!(test), |el| {
                                    el.debug_selector(|| format!("qa-command-control-{}", run.id))
                                })
                                .child(if active { "Stop" } else { "Retry" })
                                .when(!self.busy || (active && !self.write_pending), |el| {
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
                        ),
                );
                if let Some(id) = run.terminal_id {
                    let chat = run.chat_id;
                    let scope = self.scope.clone();
                    row = row.child(
                        command_action(&theme, true)
                            .id(SharedString::from(format!("output-command-{}", run.id)))
                            .child(if self.scope.chat.as_deref() == Some(chat.as_str()) {
                                "Output"
                            } else {
                                "Output in original task"
                            })
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if scope.is_current(view.state.read(cx)) {
                                    cx.emit(ProjectCommandEvent::Output {
                                        scope: scope.clone(),
                                        chat_id: chat.clone(),
                                        terminal_id: id.clone(),
                                    });
                                }
                            })),
                    );
                }
                if let Some(detail) = run.detail {
                    row = row.child(widgets::page_subtitle(&theme, detail));
                }
                rows = rows.child(row);
            }
        }
        if let Some(error) = &self.error {
            rows = rows.child(widgets::error_strip(&theme, error.clone()));
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
