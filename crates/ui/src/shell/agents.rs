use super::*;
use luna_doc::{MessagePart, SubagentStatus};

impl Shell {
    pub(super) fn add_work_surface(&mut self, surface: RightSurface, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        let key = self.panel_key(cx);
        push_unique_right_surface(self.right_tabs.entry(key).or_default(), surface);
        if !self.right_pane_open(cx) {
            self.toggle_right_pane(cx);
        }
        self.set_right_active(surface, cx);
    }

    pub(super) fn render_agents(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let (rows, total) = agent_rows(&state.transcript);
        let rows: Vec<_> = rows
            .into_iter()
            .map(|row| {
                (
                    row.id,
                    row.title,
                    row.label,
                    row.activity,
                    row.metrics,
                    row.doc_ref,
                    row.frozen,
                )
            })
            .collect();
        let parent = self.active_chat.clone();
        let mut content = div()
            .id("agents-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0));
        // The focused chat's background work, grouped by lifecycle owner
        // (read-only here; the owned-work stop lives in its composer).
        if let Some(section) = work_section(&theme, self.state.read(cx)) {
            content = content.child(section);
        }
        // Native workflows/teammates are their own family, never subagent rows.
        let native =
            crate::claude_advanced::native_task_section(&theme, &self.state.read(cx).native_tasks);
        let has_native = native.is_some();
        if let Some(section) = native {
            content = content.child(section);
        }
        if rows.is_empty() && !has_native {
            content = content.child(div().p(px(crate::surface_chrome::CONTENT_INSET)).text_color(theme.text_muted).child("No agents yet. Native subagents from this conversation appear here when the provider reports them."));
        }
        for (id, title, label, activity, metrics, doc_id, frozen) in rows {
            let parent = parent.clone();
            let available = doc_id.is_some();
            let accessible = if available {
                format!("{title}, {label}. Open agent transcript")
            } else {
                format!(
                    "{title}, {label}. Transcript unavailable: the provider has not reported a transcript reference."
                )
            };
            let metrics = if !available {
                if metrics.is_empty() {
                    "Transcript unavailable".into()
                } else {
                    format!("Transcript unavailable · {metrics}")
                }
            } else if metrics.is_empty() {
                "Usage not reported".into()
            } else {
                metrics
            };
            content = content.child(
                div()
                    .id(SharedString::from(format!("agent-{id}")))
                    .min_h(px(78.0))
                    .flex_none()
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .role(gpui::Role::Group)
                    .aria_label(accessible)
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .gap(px(8.0))
                            .child(div().truncate().text_color(theme.text).child(title.clone()))
                            .child(div().flex_none().text_color(theme.text_muted).child(label)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(activity),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_color(theme.text_muted)
                            .text_size(crate::typography::ui_rems(11.0))
                            .child(metrics),
                    )
                    .when(available, |row| {
                        row.role(gpui::Role::Button)
                            .tab_index(0)
                            .focus_visible({
                                let ring = theme.focus_ring();
                                move |style| crate::theme::focus_ring(style, ring)
                            })
                            .hover(move |style| style.bg(theme.surface_raised))
                            .cursor_pointer()
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                if let Some(doc) = doc_id.clone() {
                                    shell.add_subagent_surface(
                                        parent.clone(),
                                        doc,
                                        title.clone(),
                                        frozen,
                                        cx,
                                    );
                                }
                            }))
                            .on_key_down(crate::swallow_activation_key)
                    }),
            );
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(crate::typography::ui_rems(12.0))
            .child(crate::surface_chrome::toolbar(&theme).child("Agents · this conversation"))
            .child(content)
            .child(
                div()
                    .px(px(16.0))
                    .py(px(8.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .text_color(theme.text_muted)
                    .child(if total > 100 {
                        "Showing the first 100 observed agents".into()
                    } else {
                        format!("{total} observed agents · provider-reported activity")
                    }),
            )
            .into_any_element()
    }
}

/// One Agents-surface row, derived from a root spawn chip.
pub(super) struct AgentRow {
    id: String,
    title: String,
    label: &'static str,
    activity: String,
    metrics: String,
    doc_ref: Option<String>,
    frozen: bool,
}

/// Rows for the first 100 spawn chips, plus the total observed.
impl Shell {
    /// A Work row's Open: only on the connection its list was read on, only
    /// for a chat this window knows; chats open through the related-chat
    /// routing (a workbench tab, else the chat), a service's commands in the
    /// terminal panel of its own (selected) chat.
    pub(super) fn open_work_target(
        &mut self,
        open: &OpenWorkTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (current, exists, selected) = {
            let state = self.state.read(cx);
            (
                state.engine_generation() == open.engine_generation,
                state.chats.iter().any(|chat| chat.id == open.task_id),
                state.selected_chat.as_deref() == Some(open.task_id.as_str()),
            )
        };
        let route = if !current {
            self.sidebar_notice =
                Some("That list was read before the engine reconnected; open it again.".into());
            "refused"
        } else if !exists {
            self.sidebar_notice = Some("That chat isn't available here.".into());
            "refused"
        } else if open.commands {
            if selected {
                if !self.terminal_open(cx) {
                    self.toggle_terminal(window, cx);
                }
                "commands"
            } else {
                // Commands follow the selected chat's checkout: open the
                // service's own chat first.
                self.open_side_chat_in_pane(open.task_id.clone(), cx);
                self.sidebar_notice = Some(
                    "Opened the service's chat. Its project commands are in that chat's terminal panel."
                        .into(),
                );
                "chat"
            }
        } else {
            self.open_side_chat_in_pane(open.task_id.clone(), cx);
            "chat"
        };
        #[cfg(feature = "appshots-fixture")]
        {
            self.last_work_open = Some(serde_json::json!({
                "taskId": open.task_id,
                "commands": open.commands,
                "route": route,
                "terminalOpen": self.terminal_open(cx),
            }));
        }
        let _ = route;
        cx.notify();
    }
}

/// "Work · this conversation": the focused chat's host-reported work, by
/// owner (owned, services, started here but owned elsewhere).
fn work_section(theme: &Theme, state: &crate::state::AppState) -> Option<AnyElement> {
    use crate::task_work::{WorkGroup, kind_label, state_label};
    let view = state.task_work.as_ref().filter(|view| view.has_work())?;
    let title = state
        .chats
        .iter()
        .find(|chat| chat.id == view.task_id)
        .and_then(|chat| chat.title.clone())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "Untitled chat".into());
    let muted = theme.readable_muted(0.7);
    let mut section = div()
        .id("agents-work")
        .flex()
        .flex_col()
        .gap(px(4.0))
        .pb(px(8.0))
        .child(
            div()
                .text_color(theme.text)
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(SharedString::from(format!("Work · {title}"))),
        )
        .child(
            div()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(muted)
                .child(SharedString::from(view.summary())),
        );
    for group in [WorkGroup::Owned, WorkGroup::Services, WorkGroup::Elsewhere] {
        let items: Vec<_> = view
            .items()
            .filter(|item| WorkGroup::of(item) == group)
            .collect();
        if items.is_empty() {
            continue;
        }
        section = section.child(
            div()
                .pt(px(4.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_faint)
                .child(group.title()),
        );
        for item in items {
            section = section.child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_color(theme.text)
                            .child(SharedString::from(item.title.clone())),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(muted)
                            .child(SharedString::from(if item.detail.trim().is_empty() {
                                format!("{} · {}", state_label(item.state), kind_label(item.kind))
                            } else {
                                format!(
                                    "{} · {} · {}",
                                    state_label(item.state),
                                    kind_label(item.kind),
                                    item.detail
                                )
                            })),
                    ),
            );
        }
    }
    if view.more_offset.is_some() {
        section = section.child(
            div()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(muted)
                .child(SharedString::from(format!(
                    "Showing {} of {}. Use the Work button in its composer for more.",
                    view.loaded(),
                    view.snapshot
                        .as_ref()
                        .map_or(0, |snapshot| snapshot.total_count)
                ))),
        );
    }
    Some(section.into_any_element())
}

pub(super) fn agent_rows(transcript: &[luna_doc::SessionMessageEntry]) -> (Vec<AgentRow>, usize) {
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for part in transcript.iter().flat_map(|entry| &entry.parts) {
        let MessagePart::Tool {
            id,
            call,
            subagent_ref,
            subagent_status,
            subagent_tail,
            subagent_activity,
            ..
        } = part
        else {
            continue;
        };
        if !call.is_subagent_spawn() || !seen.insert(id.clone()) || rows.len() == 100 {
            continue;
        }
        let title = match call {
            luna_proto::ToolCall::Unknown { name, .. } => {
                name.strip_prefix("Agent: ").unwrap_or(name).to_string()
            }
            luna_proto::ToolCall::Mcp { tool, .. } => {
                tool.strip_prefix("Agent: ").unwrap_or(tool).to_string()
            }
            _ => "Agent".into(),
        };
        let label = match subagent_status {
            Some(SubagentStatus::Running) => "Running",
            Some(SubagentStatus::Waiting) => "Waiting",
            Some(SubagentStatus::Idle) => "Idle",
            Some(SubagentStatus::Done) => "Done",
            Some(SubagentStatus::Failed) => "Failed",
            Some(SubagentStatus::Interrupted) => "Stopped",
            Some(SubagentStatus::Unobserved) | None => "Not observed",
        };
        let frozen = matches!(
            subagent_status,
            Some(
                SubagentStatus::Done
                    | SubagentStatus::Failed
                    | SubagentStatus::Interrupted
                    | SubagentStatus::Unobserved
            )
        );
        let activity = subagent_activity.as_ref();
        // An outcome first; while it still runs, its reported progress.
        let line = activity
            .and_then(|activity| activity.error.clone().or_else(|| activity.result.clone()))
            .or_else(|| {
                (!frozen)
                    .then(|| crate::native_agents::progress_line(activity))
                    .flatten()
            })
            .or_else(|| subagent_tail.clone())
            .unwrap_or_else(|| label.into());
        rows.push(AgentRow {
            id: id.clone(),
            title,
            label,
            activity: line,
            metrics: crate::native_agents::metrics(activity, call.subagent_model()).join(" · "),
            doc_ref: subagent_ref.clone(),
            frozen,
        });
    }
    (rows, seen.len())
}

/// Headless QA drives the production Agents surface through these hooks.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_open_agents(&mut self, cx: &mut Context<Self>) {
        self.add_work_surface(RightSurface::Agents, cx);
    }

    /// Open a child transcript exactly as an Agents row or chip click does.
    pub fn fixture_open_subagent(&mut self, doc_id: String, title: String, cx: &mut Context<Self>) {
        let parent = self.active_chat.clone();
        self.add_subagent_surface(parent, doc_id, title, true, cx);
    }

    pub fn fixture_agents_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        let state = self.state.read(cx);
        let (rows, total) = agent_rows(&state.transcript);
        let chips = |entries: &[luna_doc::SessionMessageEntry]| {
            entries
                .iter()
                .flat_map(|entry| &entry.parts)
                .filter_map(|part| match part {
                    MessagePart::Tool {
                        id,
                        call,
                        subagent_ref,
                        subagent_status,
                        subagent_activity,
                        output,
                        ..
                    } => Some(serde_json::json!({
                        "id": id,
                        "spawn": call.is_subagent_spawn(),
                        "ref": subagent_ref,
                        "status": subagent_status,
                        "activity": subagent_activity,
                        "output": output,
                    })),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let text = |entries: &[luna_doc::SessionMessageEntry]| {
            entries
                .iter()
                .flat_map(|entry| &entry.parts)
                .filter_map(|part| match part {
                    MessagePart::Text { text, .. } | MessagePart::Reasoning { text, .. } => {
                        Some(text.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        };
        let open_docs: Vec<String> = self
            .subagent_tabs
            .values()
            .map(|tab| tab.doc_id.clone())
            .collect();
        serde_json::json!({
            "activeChat": self.active_chat,
            "rightPaneOpen": self.right_pane_open(cx),
            "rightActive": format!("{:?}", self.resolved_right_active(cx)),
            "total": total,
            "rows": rows.iter().map(|row| serde_json::json!({
                "id": row.id,
                "title": row.title,
                "label": row.label,
                "activity": row.activity,
                "metrics": row.metrics,
                "docRef": row.doc_ref,
            })).collect::<Vec<_>>(),
            "rootText": text(&state.transcript),
            "rootChips": chips(&state.transcript),
            "subagents": open_docs.iter().map(|doc| serde_json::json!({
                "doc": doc,
                "rows": state.sub_transcript(doc).len(),
                "roles": state.sub_transcript(doc).iter().map(|entry| format!("{:?}", entry.role)).collect::<Vec<_>>(),
                "text": text(state.sub_transcript(doc)),
                "chips": chips(state.sub_transcript(doc)),
            })).collect::<Vec<_>>(),
        })
    }
}

/// QA: the scripted work host and the watch, for the workbench fixture.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    /// Script the work host (`None`: back to the engine's real watch).
    pub fn fixture_task_work_fake(
        &mut self,
        fake: Option<crate::task_work::TaskWorkFake>,
        cx: &mut Context<Self>,
    ) {
        self.state
            .update(cx, |state, cx| state.fixture_set_task_work_fake(fake, cx));
    }
    pub fn fixture_fake_work(
        &mut self,
        task_id: &str,
        items: Vec<luna_proto::TaskWorkItem>,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            state.fixture_set_fake_work(task_id, items, cx)
        });
    }
    pub fn fixture_fake_stop(
        &mut self,
        statuses: Option<std::collections::HashMap<String, luna_proto::TaskWorkStopStatus>>,
        refuse: bool,
        cx: &mut Context<Self>,
    ) {
        self.state
            .update(cx, |state, _| state.fixture_set_fake_stop(statuses, refuse));
    }
    pub fn fixture_end_task_work_watch(&mut self, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.fixture_end_task_work_watch(cx));
    }
    /// The scripted host's call counts and the stop request ids it saw.
    pub fn fixture_task_work_calls(&self, cx: &App) -> serde_json::Value {
        let state = self.state.read(cx);
        match state.fixture_task_work_fake() {
            None => serde_json::Value::Null,
            Some(fake) => serde_json::json!({
                "previewCalls": fake.preview_calls,
                "stopCalls": fake.stop_calls,
                "requestIds": fake.stop_request_ids,
            }),
        }
    }
    /// Close the terminal panel if open (restores the frame after QA).
    pub fn fixture_close_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_open(cx) {
            self.toggle_terminal(window, cx);
        }
    }
    /// What the last Work row Open did (route, target, terminal state).
    pub fn fixture_last_work_open(&self) -> serde_json::Value {
        self.last_work_open
            .clone()
            .unwrap_or(serde_json::Value::Null)
    }
    /// The focused chat's work view (which chat the single watch follows).
    pub fn fixture_task_work_view(&self, cx: &App) -> serde_json::Value {
        let state = self.state.read(cx);
        serde_json::json!({
            "focus": state.execution_focus_chat(),
            "taskId": state.task_work.as_ref().map(|view| view.task_id.clone()),
            "status": state.task_work.as_ref().map(|view| format!("{:?}", view.status)),
            "total": state.task_work.as_ref().and_then(|view| view.snapshot.as_ref().map(|snapshot| snapshot.total_count)),
        })
    }
}
