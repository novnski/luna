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
        let mut seen = std::collections::HashSet::new();
        let mut rows = Vec::new();
        'entries: for entry in &state.transcript {
            for part in &entry.parts {
                if let MessagePart::Tool {
                    id,
                    call,
                    subagent_ref,
                    subagent_status,
                    subagent_tail,
                    subagent_activity,
                    ..
                } = part
                {
                    if !call.is_subagent_spawn() || !seen.insert(id.clone()) {
                        continue;
                    }
                    if rows.len() == 100 {
                        break 'entries;
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
                        Some(SubagentStatus::Running) => "Working",
                        Some(SubagentStatus::Waiting) => "Waiting",
                        Some(SubagentStatus::Idle) => "Idle",
                        Some(SubagentStatus::Done) => "Completed",
                        Some(SubagentStatus::Failed) => "Failed",
                        Some(SubagentStatus::Interrupted) => "Interrupted",
                        Some(SubagentStatus::Unobserved) | None => "Not observed",
                    };
                    let activity = subagent_activity
                        .as_ref()
                        .and_then(|activity| activity.error.as_ref().or(activity.result.as_ref()))
                        .or(subagent_tail.as_ref())
                        .cloned()
                        .unwrap_or_else(|| label.into());
                    let tokens = subagent_activity.as_ref().and_then(|activity| {
                        Some(format!(
                            "{} in · {} out",
                            activity.input_tokens?, activity.output_tokens?
                        ))
                    });
                    let metrics = [call.subagent_model().map(str::to_owned), tokens]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · ");
                    rows.push((
                        id.clone(),
                        title,
                        label,
                        activity,
                        metrics,
                        subagent_ref.clone(),
                        matches!(
                            subagent_status,
                            Some(
                                SubagentStatus::Done
                                    | SubagentStatus::Failed
                                    | SubagentStatus::Interrupted
                                    | SubagentStatus::Unobserved
                            )
                        ),
                    ));
                }
            }
        }
        let total = seen.len();
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
        if rows.is_empty() {
            content = content.child(div().p(px(20.0)).text_color(theme.text_muted).child("No agents yet. Native subagents from this conversation appear here when the provider reports them."));
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
            let key_title = title.clone();
            let key_doc = doc_id.clone();
            let key_parent = parent.clone();
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
                            .focus_visible(move |style| style.border_color(theme.border_strong))
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
                            .on_key_down(cx.listener(
                                move |shell, event: &gpui::KeyDownEvent, _, cx| {
                                    if is_settings_nav_activation(
                                        &event.keystroke.key,
                                        event.is_held,
                                    ) {
                                        if let Some(doc) = key_doc.clone() {
                                            shell.add_subagent_surface(
                                                key_parent.clone(),
                                                doc,
                                                key_title.clone(),
                                                frozen,
                                                cx,
                                            );
                                        }
                                        cx.stop_propagation();
                                    }
                                },
                            ))
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
