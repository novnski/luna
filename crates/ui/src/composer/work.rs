//! The Work chip and popover (slice 3): the focused chat's background work in
//! its own composer, grouped by lifecycle ownership, with an explicit,
//! previewed owned-work stop that is never the current-turn Stop.

use super::*;
use crate::AccessibleButton as _;
use crate::task_work::{
    MAX_STOP, StopAnswer, WorkGroup, WorkStatus, kind_label, selectable, state_label,
    stop_status_label,
};
use gpui::AnyElement;
use luna_proto::{TaskWorkItem, TaskWorkStopPreview};

/// An icon-sized chip, plus room for a count.
pub(super) const WORK_CHIP_WIDTH: f32 = 44.0;

#[derive(Debug, Clone)]
enum WorkPhase {
    Select,
    Previewing,
    Preview(TaskWorkStopPreview),
    Stopping {
        request_id: String,
    },
    Answer {
        request_id: String,
        answer: StopAnswer,
    },
    Error(String),
}

/// The open Work popover: for one chat on one engine connection.
pub(super) struct WorkMenu {
    task_id: String,
    engine_generation: u64,
    /// Explicitly chosen rows, in the order chosen (at most `MAX_STOP`).
    selected: Vec<String>,
    phase: WorkPhase,
    note: Option<String>,
    focus: FocusHandle,
    focus_pending: bool,
}

/// Where a Work row's Open goes.
struct WorkOpen {
    task_id: String,
    commands: bool,
    label: &'static str,
}

/// A service: its own chat's project commands. Another chat's row: that
/// chat. Work started here but owned elsewhere: its lifecycle owner. Nothing
/// for this chat's own rows, or for chats `known` does not list.
fn work_open(item: &TaskWorkItem, focused: &str, known: impl Fn(&str) -> bool) -> Option<WorkOpen> {
    if item.kind == luna_proto::TaskWorkKind::Service {
        return known(&item.task_id).then(|| WorkOpen {
            task_id: item.task_id.clone(),
            commands: true,
            label: "Show commands",
        });
    }
    if item.task_id != focused && known(&item.task_id) {
        return Some(WorkOpen {
            task_id: item.task_id.clone(),
            commands: false,
            label: "Open chat",
        });
    }
    let owner = item
        .origin
        .as_ref()
        .map(|origin| origin.owner_task_id.as_str())?;
    (WorkGroup::of(item) == WorkGroup::Elsewhere && owner != focused && known(owner)).then(|| {
        WorkOpen {
            task_id: owner.to_string(),
            commands: false,
            label: "Open owner",
        }
    })
}

impl Composer {
    /// This composer's chat's work view, when it is the focused chat's (the
    /// only one watched): other panes never show or act on it.
    fn own_work<'a>(&self, cx: &'a App) -> Option<&'a crate::task_work::TaskWorkView> {
        let state = self.state.read(cx);
        let chat = self.binding.chat_id(state)?;
        state.task_work.as_ref().filter(|view| view.task_id == chat)
    }

    /// The chip's width for the controls row budget (0 when hidden).
    pub(super) fn work_chip_width(&self, cx: &App) -> f32 {
        if self.own_work(cx).is_some_and(|view| view.has_work()) {
            WORK_CHIP_WIDTH
        } else {
            0.0
        }
    }

    pub(crate) fn work_menu_open(&self) -> bool {
        self.work_menu.is_some()
    }

    /// Close the popover (Escape, Close, outside click).
    pub(crate) fn close_work_menu(&mut self, cx: &mut Context<Self>) -> bool {
        if self.work_menu.take().is_some() {
            self.work_task = None;
            cx.notify();
            true
        } else {
            false
        }
    }

    fn toggle_work_menu(&mut self, cx: &mut Context<Self>) {
        if self.close_work_menu(cx) {
            return;
        }
        let Some(task_id) = self.own_work(cx).map(|view| view.task_id.clone()) else {
            return;
        };
        self.work_menu = Some(WorkMenu {
            task_id,
            engine_generation: self.state.read(cx).engine_generation(),
            selected: Vec::new(),
            phase: WorkPhase::Select,
            note: None,
            focus: cx.focus_handle(),
            focus_pending: true,
        });
        cx.notify();
    }

    /// The popover still belongs to this composer's chat, its work view and
    /// the engine connection it opened on.
    fn work_menu_current(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        self.work_menu.as_ref().is_some_and(|menu| {
            menu.engine_generation == state.engine_generation()
                && self.binding.chat_id(state) == Some(menu.task_id.as_str())
                && state
                    .task_work
                    .as_ref()
                    .is_some_and(|view| view.task_id == menu.task_id)
        })
    }

    fn toggle_work_row(&mut self, id: String, cx: &mut Context<Self>) {
        // Only live rows the host says are owned and stoppable.
        let eligible = self
            .own_work(cx)
            .filter(|view| view.status == WorkStatus::Live)
            .and_then(|view| view.items().find(|item| item.id == id).cloned())
            .is_some_and(|item| selectable(&item));
        let Some(menu) = self.work_menu.as_mut() else {
            return;
        };
        if !matches!(menu.phase, WorkPhase::Select) || !eligible {
            return;
        }
        if let Some(index) = menu.selected.iter().position(|selected| *selected == id) {
            menu.selected.remove(index);
            menu.note = None;
        } else if menu.selected.len() >= MAX_STOP {
            menu.note = Some(format!("Choose at most {MAX_STOP} at a time."));
        } else {
            menu.selected.push(id);
            menu.note = None;
        }
        cx.notify();
    }

    /// Review stop: the host's exact preview for the chosen rows only.
    fn review_work_stop(&mut self, cx: &mut Context<Self>) {
        if !self.work_menu_current(cx) {
            self.close_work_menu(cx);
            return;
        }
        // No stop from a view without live evidence (stream end, reconnect).
        if !self
            .own_work(cx)
            .is_some_and(|view| view.status == WorkStatus::Live)
        {
            return;
        }
        let (engine, params) = {
            let state = self.state.read(cx);
            let Some(menu) = self.work_menu.as_ref() else {
                return;
            };
            (
                state.engine().cloned(),
                crate::task_work::task_params(state, &menu.task_id),
            )
        };
        let Some(menu) = self.work_menu.as_mut() else {
            return;
        };
        if menu.selected.is_empty() || !matches!(menu.phase, WorkPhase::Select) {
            return;
        }
        let work_ids = menu.selected.clone();
        let (task_id, generation) = (menu.task_id.clone(), menu.engine_generation);
        menu.phase = WorkPhase::Previewing;
        menu.note = None;
        #[cfg(feature = "appshots-fixture")]
        if self.state.read(cx).fixture_task_work_fake().is_some() {
            let preview = self
                .state
                .update(cx, |state, _| state.fake_preview_stop(&task_id, &work_ids));
            self.settle_work_preview(&task_id, generation, Ok(preview), cx);
            return;
        }
        let Some(engine) = engine else {
            self.settle_work_preview(&task_id, generation, Err("Engine not connected".into()), cx);
            return;
        };
        self.work_task = Some(cx.spawn(async move |this, cx| {
            let result = crate::task_work::preview_stop(engine, params, work_ids).await;
            this.update(cx, |composer, cx| {
                composer.settle_work_preview(&task_id, generation, result, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    fn settle_work_preview(
        &mut self,
        task_id: &str,
        generation: u64,
        result: Result<TaskWorkStopPreview, String>,
        cx: &mut Context<Self>,
    ) {
        let current = self.work_menu_current(cx);
        let Some(menu) = self.work_menu.as_mut().filter(|menu| {
            current
                && menu.task_id == task_id
                && menu.engine_generation == generation
                && matches!(menu.phase, WorkPhase::Previewing)
        }) else {
            return;
        };
        menu.phase = match result {
            Ok(preview) if preview.task_id == task_id => WorkPhase::Preview(preview),
            Ok(_) => WorkPhase::Error("The host answered for another chat.".into()),
            Err(error) => WorkPhase::Error(format!("Couldn't prepare the stop: {error}")),
        };
        cx.notify();
    }

    /// Stop exactly the previewed rows, under one request id kept for this
    /// preview. A lost answer is unknown and is never resent.
    fn confirm_work_stop(&mut self, cx: &mut Context<Self>) {
        if !self.work_menu_current(cx) {
            self.close_work_menu(cx);
            return;
        }
        let (engine, params) = {
            let state = self.state.read(cx);
            let Some(menu) = self.work_menu.as_ref() else {
                return;
            };
            (
                state.engine().cloned(),
                crate::task_work::task_params(state, &menu.task_id),
            )
        };
        let Some(menu) = self.work_menu.as_mut() else {
            return;
        };
        let WorkPhase::Preview(preview) = &menu.phase else {
            return;
        };
        if preview.items.is_empty() {
            return;
        }
        let work_ids: Vec<String> = preview.items.iter().map(|item| item.id.clone()).collect();
        let preview_id = preview.preview_id.clone();
        let request_id = uuid::Uuid::new_v4().to_string();
        let (task_id, generation) = (menu.task_id.clone(), menu.engine_generation);
        menu.phase = WorkPhase::Stopping {
            request_id: request_id.clone(),
        };
        #[cfg(feature = "appshots-fixture")]
        if self.state.read(cx).fixture_task_work_fake().is_some() {
            let answer = self.state.update(cx, |state, _| {
                state.fake_stop(&task_id, &work_ids, &preview_id, &request_id)
            });
            self.settle_work_stop(&task_id, generation, &request_id, answer, cx);
            return;
        }
        let Some(engine) = engine else {
            self.settle_work_stop(&task_id, generation, &request_id, StopAnswer::Lost, cx);
            return;
        };
        self.work_task = Some(cx.spawn(async move |this, cx| {
            let answer =
                crate::task_work::stop(engine, params, work_ids, preview_id, request_id.clone())
                    .await;
            this.update(cx, |composer, cx| {
                composer.settle_work_stop(&task_id, generation, &request_id, answer, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    fn settle_work_stop(
        &mut self,
        task_id: &str,
        generation: u64,
        request_id: &str,
        answer: StopAnswer,
        cx: &mut Context<Self>,
    ) {
        let current = self.work_menu_current(cx);
        let visible: std::collections::HashSet<String> = self
            .own_work(cx)
            .map(|view| {
                view.items()
                    .filter(|item| selectable(item))
                    .map(|item| item.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let Some(menu) = self.work_menu.as_mut().filter(|menu| {
            current
                && menu.task_id == task_id
                && menu.engine_generation == generation
                && matches!(&menu.phase, WorkPhase::Stopping { request_id: sent } if sent == request_id)
        }) else {
            return;
        };
        menu.phase = match answer {
            // The preview no longer describes the work: choose again, keeping
            // only rows that are still stoppable.
            StopAnswer::Refused(reason) => {
                menu.selected.retain(|id| visible.contains(id));
                menu.note = Some(reason);
                WorkPhase::Select
            }
            answer => WorkPhase::Answer {
                request_id: request_id.to_string(),
                answer,
            },
        };
        cx.notify();
    }

    /// Open a Work row's target: the popover closes, and the shell routes it
    /// (whichever composer this is) for the connection the list was read on.
    fn open_work_target(
        &mut self,
        task_id: String,
        commands: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let engine_generation = self
            .work_menu
            .as_ref()
            .map(|menu| menu.engine_generation)
            .unwrap_or_else(|| self.state.read(cx).engine_generation());
        self.close_work_menu(cx);
        window.dispatch_action(
            Box::new(crate::shell::OpenWorkTarget {
                task_id,
                commands,
                engine_generation,
            }),
            cx,
        );
    }

    /// An ordinary read after a stop (never a resend): the watch reads the
    /// work again and the popover returns to the list.
    fn refresh_work(&mut self, cx: &mut Context<Self>) {
        if let Some(menu) = self.work_menu.as_mut() {
            menu.phase = WorkPhase::Select;
            menu.selected.clear();
            menu.note = None;
        }
        self.state.update(cx, |state, cx| {
            state.task_work = None;
            state.sync_task_work(cx);
        });
        cx.notify();
    }

    /// The chip (and its popover while open), for the focused chat only.
    pub(super) fn render_work_chip(
        &mut self,
        window: &mut Window,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // A popover whose chat or connection changed closes without acting.
        if self.work_menu.is_some() && !self.work_menu_current(cx) {
            self.work_menu = None;
            self.work_task = None;
        }
        let view = self.own_work(cx).filter(|view| view.has_work())?.clone();
        let blocking = view
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.blocking_count);
        let unknown = matches!(view.status, WorkStatus::Unknown(_));
        let attention = view.items().any(|item| item.needs_attention);
        let summary: SharedString = view.summary().into();
        let open = self.work_menu.is_some();
        let tooltip_summary = summary.clone();
        let chip = div()
            .id("composer-work-chip")
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("Background work: {summary}")))
            .h(px(32.0))
            .w(px(WORK_CHIP_WIDTH))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(4.0))
            .rounded(px(8.0))
            .cursor_pointer()
            .when(open, |el| el.bg(theme.element_hover))
            .hover(|style| style.bg(theme.element_hover))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_work_menu(cx)))
            .when(!open, |el| {
                el.tooltip(move |_, cx| {
                    crate::pickers::ControlTooltip::view(
                        "Background work",
                        Some(tooltip_summary.clone()),
                        cx,
                    )
                })
            })
            .child(
                div()
                    .relative()
                    .child(
                        crate::icons::icon(crate::icons::LIST)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    )
                    .when(unknown || attention, |icon| {
                        icon.child(
                            div()
                                .absolute()
                                .top(px(-1.0))
                                .right(px(-2.0))
                                .size(px(6.0))
                                .rounded_full()
                                .bg(theme.warning),
                        )
                    }),
            )
            .when(blocking > 0, |el| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(blocking.to_string())),
                )
            });
        let menu = open.then(|| self.render_work_menu(window, theme, &view, cx));
        Some(
            div()
                .relative()
                .flex_none()
                .child(chip)
                .children(menu)
                .into_any_element(),
        )
    }

    fn render_work_menu(
        &mut self,
        window: &mut Window,
        theme: &Theme,
        view: &crate::task_work::TaskWorkView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(menu) = self.work_menu.as_mut()
            && std::mem::take(&mut menu.focus_pending)
        {
            window.focus(&menu.focus, cx);
        }
        let Some(menu) = self.work_menu.as_ref() else {
            return div().into_any_element();
        };
        let muted = theme.readable_muted(0.7);
        let small = |text: String, color: gpui::Hsla| {
            div()
                .text_size(crate::typography::ui_rems(11.0))
                .line_height(px(16.0))
                .text_color(color)
                .child(SharedString::from(text))
        };
        let title_of = |id: &str| {
            view.items()
                .find(|item| item.id == id)
                .map(|item| item.title.clone())
                .unwrap_or_else(|| "Work".into())
        };
        let row = |item: &TaskWorkItem, chosen: Option<bool>| {
            let mut line =
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .p(px(6.0))
                    .rounded(px(6.0))
                    .when(chosen.is_some(), |el| {
                        el.cursor_pointer()
                            .hover(|style| style.bg(crate::theme::ink(0.05)))
                    })
                    .child(div().w(px(14.0)).flex_none().pt(px(1.0)).when_some(
                        chosen,
                        |slot, on| {
                            if on {
                                slot.child(
                                    crate::icons::icon(crate::icons::CHECK)
                                        .size(px(13.0))
                                        .text_color(theme.accent),
                                )
                            } else {
                                // An empty check box.
                                slot.child(
                                    div()
                                        .size(px(12.0))
                                        .rounded(px(3.0))
                                        .border_1()
                                        .border_color(muted),
                                )
                            }
                        },
                    ));
            let mut text = div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text)
                                .child(SharedString::from(item.title.clone())),
                        )
                        .when(item.needs_attention, |el| {
                            el.child(
                                div()
                                    .mt(px(5.0))
                                    .size(px(6.0))
                                    .rounded_full()
                                    .bg(theme.warning),
                            )
                        }),
                )
                .child(small(
                    format!("{} · {}", state_label(item.state), kind_label(item.kind)),
                    muted,
                ));
            if !item.detail.trim().is_empty() {
                text = text.child(small(item.detail.clone(), muted));
            }
            line = line.child(text);
            line
        };
        let mut body = div().flex().flex_col().gap(px(6.0));
        match &menu.phase {
            WorkPhase::Select => {
                body = body.child(small(view.summary(), theme.text));
                if let WorkStatus::Unknown(_) | WorkStatus::Unavailable(_) = view.status {
                    // The summary already says why; rows (if any) are the last
                    // known and are not selectable.
                }
                let live = view.status == WorkStatus::Live;
                for group in [WorkGroup::Owned, WorkGroup::Services, WorkGroup::Elsewhere] {
                    let items: Vec<&TaskWorkItem> = view
                        .items()
                        .filter(|item| WorkGroup::of(item) == group)
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    body = body.child(
                        div()
                            .pt(px(4.0))
                            .text_size(crate::typography::ui_rems(10.0))
                            .text_color(theme.text_faint)
                            .child(group.title()),
                    );
                    if let Some(note) = group.note() {
                        body = body.child(small(note.into(), muted));
                    }
                    for item in items {
                        let can = live && selectable(item);
                        let chosen = can.then(|| menu.selected.contains(&item.id));
                        let id = item.id.clone();
                        let mut element = row(item, chosen);
                        // Open: the row's own chat, its owner, or a service's
                        // commands (only for chats this window knows).
                        if let Some(target) = work_open(item, &view.task_id, |id| {
                            self.state.read(cx).chats.iter().any(|chat| chat.id == id)
                        }) {
                            let (task_id, commands) = (target.task_id.clone(), target.commands);
                            element = element.child(
                                div()
                                    .id(SharedString::from(format!("work-open-{}", item.id)))
                                    .accessible_button(target.label, theme.accent)
                                    .flex_none()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.accent)
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_work_target(
                                            task_id.clone(),
                                            commands,
                                            window,
                                            cx,
                                        );
                                    }))
                                    .child(target.label),
                            );
                        }
                        body = body.child(if can {
                            element
                                .id(SharedString::from(format!("work-row-{}", item.id)))
                                .role(gpui::Role::CheckBox)
                                .aria_label(SharedString::from(item.title.clone()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_work_row(id.clone(), cx)
                                }))
                                .into_any_element()
                        } else {
                            element.into_any_element()
                        });
                    }
                }
                if let Some(snapshot) = &view.snapshot {
                    if view.more_offset.is_some() {
                        body = body.child(
                            div()
                                .id("work-show-more")
                                .accessible_button("Show more work", theme.accent)
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.accent)
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state
                                        .update(cx, |state, cx| state.load_more_task_work(cx));
                                }))
                                .child(SharedString::from(format!(
                                    "Show more ({} of {})",
                                    view.loaded(),
                                    snapshot.total_count
                                ))),
                        );
                    }
                    if snapshot.truncated {
                        body = body.child(small(
                            "The host shortened this list; not every item is shown.".into(),
                            muted,
                        ));
                    }
                }
            }
            WorkPhase::Previewing => {
                body = body.child(small("Checking what would stop…".into(), muted));
            }
            WorkPhase::Preview(preview) => {
                body = body.child(small("These will be asked to stop:".into(), theme.text));
                for item in &preview.items {
                    body = body.child(row(item, None));
                }
                if !preview.excluded.is_empty() {
                    body = body.child(small("Not included:".into(), muted));
                    for item in &preview.excluded {
                        body = body.child(row(item, None));
                    }
                }
            }
            WorkPhase::Stopping { .. } => {
                body = body.child(small("Asking to stop…".into(), muted));
            }
            WorkPhase::Answer { answer, .. } => match answer {
                StopAnswer::Receipt(receipt) => {
                    for result in &receipt.results {
                        body = body.child(small(
                            format!(
                                "{} · {}{}",
                                title_of(&result.work_id),
                                stop_status_label(result.status),
                                if result.detail.trim().is_empty() {
                                    String::new()
                                } else {
                                    format!(" ({})", result.detail)
                                }
                            ),
                            theme.text,
                        ));
                    }
                }
                StopAnswer::Lost => {
                    body = body.child(small(
                        "LUNA couldn't confirm the answer. The stop is unknown and will not be sent again; refresh to check."
                            .into(),
                        theme.warning,
                    ));
                }
                StopAnswer::Refused(reason) => {
                    body = body.child(small(reason.clone(), theme.warning));
                }
            },
            WorkPhase::Error(error) => {
                body = body.child(small(
                    error.clone(),
                    theme.readable_tone(theme.danger_muted, 1.0),
                ));
            }
        }
        if let Some(note) = &menu.note {
            body = body.child(small(note.clone(), theme.warning));
        }
        let button = |id: &'static str, label: String, enabled: bool, primary: bool| {
            div()
                .id(id)
                .accessible_button(label.clone(), theme.accent)
                .px(px(10.0))
                .h(px(28.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .text_size(crate::typography::ui_rems(12.0))
                .when(primary, |el| el.bg(theme.accent.opacity(0.16)))
                .when(enabled, |el| el.cursor_pointer())
                .when(!enabled, |el| el.opacity(0.5))
                .child(SharedString::from(label))
        };
        let chat = menu.task_id.clone();
        let run_live = self.run_live(cx);
        let mut actions = div().flex().flex_row().justify_end().gap(px(8.0));
        match &menu.phase {
            WorkPhase::Select => {
                if run_live {
                    actions = actions.child(
                        button("work-stop-turn", "Stop current turn".into(), true, false).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.interrupt_chat(chat.clone(), cx);
                            }),
                        ),
                    );
                }
                let count = menu.selected.len();
                let enabled = count > 0 && view.status == WorkStatus::Live;
                let review = button(
                    "work-review-stop",
                    format!("Review stop ({count})"),
                    enabled,
                    true,
                );
                actions = actions.child(if enabled {
                    review
                        .on_click(cx.listener(|this, _, _, cx| this.review_work_stop(cx)))
                        .into_any_element()
                } else {
                    review.into_any_element()
                });
            }
            WorkPhase::Preview(preview) => {
                let count = preview.items.len();
                actions = actions
                    .child(
                        button("work-back", "Back".into(), true, false).on_click(cx.listener(
                            |this, _, _, cx| {
                                if let Some(menu) = this.work_menu.as_mut() {
                                    menu.phase = WorkPhase::Select;
                                }
                                cx.notify();
                            },
                        )),
                    )
                    .child({
                        let stop = button(
                            "work-confirm-stop",
                            format!("Stop {count}"),
                            count > 0,
                            true,
                        );
                        if count > 0 {
                            stop.on_click(cx.listener(|this, _, _, cx| this.confirm_work_stop(cx)))
                                .into_any_element()
                        } else {
                            stop.into_any_element()
                        }
                    });
            }
            WorkPhase::Answer { .. } | WorkPhase::Error(_) => {
                actions = actions.child(
                    button("work-refresh", "Refresh".into(), true, true)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_work(cx))),
                );
            }
            WorkPhase::Previewing | WorkPhase::Stopping { .. } => {}
        }
        let card = crate::popover::popover_card(theme)
            .id("composer-work-menu")
            .bg(theme.tooltip_surface())
            .w(px(360.0))
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .track_focus(&menu.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        this.close_work_menu(cx);
                        cx.stop_propagation();
                    }
                    // Enter never confirms a stop from the keyboard alone.
                    "enter" => cx.stop_propagation(),
                    _ => {}
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_work_menu(cx);
            }))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child("Background work"),
            )
            .child(
                div()
                    .id("composer-work-body")
                    .max_h(px(360.0))
                    .overflow_y_scroll()
                    .child(body),
            )
            .child(actions);
        gpui::deferred(
            gpui::anchored()
                .anchor(gpui::Anchor::BottomLeft)
                .snap_to_window_with_margin(px(8.0))
                .child(div().occlude().mb(px(6.0)).child(card)),
        )
        .priority(1)
        .into_any_element()
    }

    /// The chip and popover as rendered, for headless fixtures.
    pub(crate) fn work_readback(&self, cx: &App) -> serde_json::Value {
        let view = self.own_work(cx);
        let state = self.state.read(cx);
        serde_json::json!({
            "chip": view.is_some_and(|view| view.has_work()),
            "status": view.map(|view| match &view.status {
                WorkStatus::Loading => "loading".to_string(),
                WorkStatus::Live => "live".to_string(),
                WorkStatus::Unavailable(_) => "unavailable".to_string(),
                WorkStatus::Unknown(_) => "unknown".to_string(),
            }),
            "summary": view.map(|view| view.summary()),
            "taskId": view.map(|view| view.task_id.clone()),
            "blocking": view.and_then(|view| view.snapshot.as_ref().map(|snapshot| snapshot.blocking_count)),
            "total": view.and_then(|view| view.snapshot.as_ref().map(|snapshot| snapshot.total_count)),
            "loaded": view.map(|view| view.loaded()),
            "moreOffset": view.and_then(|view| view.more_offset),
            "rows": view.map(|view| view.items().map(|item| serde_json::json!({
                "id": item.id,
                "title": item.title,
                "group": format!("{:?}", WorkGroup::of(item)),
                "selectable": selectable(item),
                "state": state_label(item.state),
                "kind": kind_label(item.kind),
                "open": work_open(item, &view.task_id, |id| state.chats.iter().any(|chat| chat.id == id))
                    .map(|target| serde_json::json!({
                        "taskId": target.task_id,
                        "commands": target.commands,
                        "label": target.label,
                    })),
            })).collect::<Vec<_>>()),
            "menu": self.work_menu.as_ref().map(|menu| serde_json::json!({
                "selected": menu.selected,
                "note": menu.note,
                "phase": match &menu.phase {
                    WorkPhase::Select => serde_json::json!({"kind": "select"}),
                    WorkPhase::Previewing => serde_json::json!({"kind": "previewing"}),
                    WorkPhase::Preview(preview) => serde_json::json!({
                        "kind": "preview",
                        "previewId": preview.preview_id,
                        "items": preview.items.iter().map(|item| item.id.clone()).collect::<Vec<_>>(),
                        "excluded": preview.excluded.iter().map(|item| item.id.clone()).collect::<Vec<_>>(),
                    }),
                    WorkPhase::Stopping { request_id } => serde_json::json!({"kind": "stopping", "requestId": request_id}),
                    WorkPhase::Answer { request_id, answer } => serde_json::json!({
                        "kind": "answer",
                        "requestId": request_id,
                        "answer": match answer {
                            StopAnswer::Receipt(receipt) => serde_json::json!({
                                "receipt": receipt.results.iter().map(|result| serde_json::json!({
                                    "workId": result.work_id,
                                    "status": result.status,
                                    "label": stop_status_label(result.status),
                                })).collect::<Vec<_>>(),
                            }),
                            StopAnswer::Lost => serde_json::json!({"lost": true}),
                            StopAnswer::Refused(reason) => serde_json::json!({"refused": reason}),
                        },
                    }),
                    WorkPhase::Error(error) => serde_json::json!({"kind": "error", "message": error}),
                },
            })),
        })
    }
}

/// Headless QA drives the chip and popover through their own paths.
#[cfg(feature = "appshots-fixture")]
impl Composer {
    pub fn fixture_open_work(&mut self, cx: &mut Context<Self>) {
        if self.work_menu.is_none() {
            self.toggle_work_menu(cx);
        }
    }
    pub fn fixture_work_toggle(&mut self, id: &str, cx: &mut Context<Self>) {
        self.toggle_work_row(id.to_string(), cx);
    }
    pub fn fixture_work_review(&mut self, cx: &mut Context<Self>) {
        self.review_work_stop(cx);
    }
    pub fn fixture_work_confirm(&mut self, cx: &mut Context<Self>) {
        self.confirm_work_stop(cx);
    }
    pub fn fixture_work_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_work(cx);
    }
    pub fn fixture_work_more(&mut self, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.load_more_task_work(cx));
    }
    /// A row's Open, dispatched through the window as its link does.
    pub fn fixture_work_open(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let target = self.own_work(cx).and_then(|view| {
            let state = self.state.read(cx);
            view.items().find(|item| item.id == id).and_then(|item| {
                work_open(item, &view.task_id, |chat| {
                    state.chats.iter().any(|row| row.id == chat)
                })
            })
        });
        let Some(target) = target else {
            return false;
        };
        self.open_work_target(target.task_id, target.commands, window, cx);
        true
    }
    pub fn fixture_work_readback(&self, cx: &App) -> serde_json::Value {
        self.work_readback(cx)
    }
}
