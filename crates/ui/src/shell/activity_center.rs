//! Sidebar Activity filter and transition delivery.
//! The engine owns attention lifecycle; the Shell only presents task rows.

use super::*;
use crate::settings::TaskNotificationPolicy;
use luna_proto::{
    AttentionKind, AttentionReadRequest, AttentionSnapshot, AttentionTransition,
    AttentionTransitionBatch, AttentionTransitionClaim, AttentionTransitionKind,
    TerminalInventoryEntry, TerminalProcessState,
};

#[derive(Default)]
pub(super) struct ActivityUi {
    pub(super) view_active: bool,
    snapshot: Option<AttentionSnapshot>,
    terminals: Vec<TerminalInventoryEntry>,
    terminal_error: Option<SharedString>,
    error: Option<SharedString>,
    loading: bool,
    in_flight: bool,
    armed: bool,
    engine_generation: u64,
    request_generation: u64,
    last_request: Option<std::time::Instant>,
    poll_task: Option<Task<()>>,
    ticker: Option<Task<()>>,
}

impl ActivityUi {
    fn accept_snapshot(&mut self, snapshot: AttentionSnapshot) {
        let current = self.snapshot.as_ref();
        if current.is_none_or(|current| {
            (snapshot.revision, snapshot.read_at_ms) >= (current.revision, current.read_at_ms)
        }) {
            self.snapshot = Some(snapshot);
        }
    }
}

fn review_clears_on_visit(kind: AttentionKind) -> bool {
    matches!(
        kind,
        AttentionKind::TaskFailed
            | AttentionKind::QueuedSendFailed
            | AttentionKind::AutomationFailed
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Delivery {
    sound: Option<crate::sound::Sound>,
    banner: bool,
}

#[derive(Clone, Copy)]
struct DeliveryContext<'a> {
    policy: TaskNotificationPolicy,
    app_focused: bool,
    desktop_enabled: bool,
    background_only: bool,
    local_device: Option<&'a str>,
}

fn route_transition(transition: &AttentionTransition, context: DeliveryContext<'_>) -> Delivery {
    let item = &transition.item;
    if transition.transition == AttentionTransitionKind::Resolved
        || Some(item.device_id.as_str()) != context.local_device
        || context.policy == TaskNotificationPolicy::Muted
    {
        return Delivery::default();
    }
    let completion = matches!(
        item.kind,
        AttentionKind::TaskCompleted | AttentionKind::TaskRecovered
    );
    if context.policy == TaskNotificationPolicy::ImportantOnly && completion {
        return Delivery::default();
    }
    let sound = if completion {
        crate::sound::Sound::Done
    } else if matches!(
        item.kind,
        AttentionKind::ApprovalRequired | AttentionKind::UserInputRequired
    ) {
        crate::sound::Sound::Request
    } else {
        crate::sound::Sound::Attention
    };
    Delivery {
        sound: Some(sound),
        banner: !completion
            && context.desktop_enabled
            && (!context.background_only || !context.app_focused),
    }
}

impl Shell {
    pub(super) fn notification_tasks(&self, cx: &App) -> Vec<NotificationTask> {
        let state = self.state.read(cx);
        state
            .chats
            .iter()
            .filter(|chat| Some(chat.device_id.as_str()) == state.local_device_id.as_deref())
            .map(|chat| NotificationTask {
                id: chat.id.clone(),
                title: chat.title.clone().unwrap_or_else(|| "New task".into()),
                policy: self
                    .settings
                    .task_notification_policies
                    .get(&chat.id)
                    .copied()
                    .unwrap_or_default(),
                keep_awake: self.settings.keep_awake_task_ids.contains(&chat.id),
            })
            .collect()
    }

    pub(super) fn ensure_activity_polling(&mut self, cx: &mut Context<Self>) {
        if self.activity.ticker.is_some() {
            return;
        }
        self.activity.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if this
                    .update(cx, |shell, cx| {
                        shell.refresh_activity(cx);
                        shell.reconcile_keep_awake(cx);
                        if shell
                            .sidebar_notice
                            .as_ref()
                            .and_then(|notice| notice.expires_at)
                            .is_some_and(|until| std::time::Instant::now() >= until)
                        {
                            shell.sidebar_notice = None;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    pub(super) fn reconcile_keep_awake(&mut self, cx: &mut Context<Self>) {
        let target = {
            let state = self.state.read(cx);
            if state.engine().is_some() {
                let now = Utc::now();
                if self.settings.keep_awake_while_running {
                    let is_local_working = |session: &luna_proto::Session| {
                        Some(session.device_id.as_str()) == state.local_device_id.as_deref()
                            && luna_proto::view::effective_indicator(Some(session), now)
                                == luna_proto::view::Indicator::Working
                    };
                    state
                        .selected_chat
                        .as_deref()
                        .and_then(|id| {
                            state
                                .sessions
                                .iter()
                                .find(|session| session.chat_id == id && is_local_working(session))
                        })
                        .or_else(|| {
                            state
                                .sessions
                                .iter()
                                .find(|session| is_local_working(session))
                        })
                        .map(|session| session.chat_id.clone())
                } else {
                    crate::power::selected_active_task(
                        state.selected_chat.as_deref(),
                        &self.settings.keep_awake_task_ids,
                        state.local_device_id.as_deref(),
                        &state.sessions,
                        now,
                    )
                    .map(str::to_owned)
                }
            } else {
                None
            }
        };
        if let Err(error) = self.keep_awake.reconcile(target.as_deref()) {
            self.activity.error = Some(error.into());
        }
    }

    pub(super) fn refresh_activity(&mut self, cx: &mut Context<Self>) {
        let (handle, engine_generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        if engine_generation != self.activity.engine_generation {
            self.activity.engine_generation = engine_generation;
            self.activity.request_generation = self.activity.request_generation.wrapping_add(1);
            self.activity.poll_task = None;
            self.activity.snapshot = None;
            self.activity.terminals.clear();
            self.activity.terminal_error = None;
            self.activity.error = None;
            self.activity.armed = false;
            self.activity.in_flight = false;
            self.activity.last_request = None;
            let _ = self.keep_awake.reconcile(None);
        }
        if self.activity.in_flight
            || self
                .activity
                .last_request
                .is_some_and(|at| at.elapsed() < Duration::from_millis(700))
        {
            return;
        }
        let Some(handle) = handle else {
            self.activity.loading = false;
            return;
        };
        let target_device_id = handle.engine_info().device_id.clone();
        let generation = self.activity.request_generation;
        let armed = self.activity.armed;
        self.activity.in_flight = true;
        self.activity.loading = self.activity.snapshot.is_none();
        self.activity.last_request = Some(std::time::Instant::now());
        self.activity.poll_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let snapshot: AttentionSnapshot = serde_json::from_value(
                    handle
                        .client()
                        .call(
                            methods::GET_ATTENTION,
                            serde_json::to_value(AttentionReadRequest {
                                target_device_id: target_device_id.clone(),
                            })
                            .unwrap_or_default(),
                        )
                        .await?,
                )
                .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))?;
                let transitions: AttentionTransitionBatch = serde_json::from_value(
                    handle
                        .client()
                        .call(
                            methods::CLAIM_ATTENTION_TRANSITIONS,
                            serde_json::to_value(AttentionTransitionClaim {
                                consumer_id: "luna-desktop".into(),
                                deliver_existing: armed,
                                target_device_id: Some(target_device_id),
                            })
                            .unwrap_or_default(),
                        )
                        .await?,
                )
                .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))?;
                let terminals = handle
                    .client()
                    .call_as::<Vec<TerminalInventoryEntry>>(
                        methods::LIST_TERMINALS,
                        serde_json::json!({}),
                    )
                    .await;
                Ok::<_, luna_rpc::RpcError>((snapshot, transitions, terminals))
            }
            .await;
            this.update(cx, |shell, cx| {
                if generation != shell.activity.request_generation
                    || engine_generation != shell.state.read(cx).engine_generation()
                {
                    return;
                }
                shell.activity.in_flight = false;
                shell.activity.loading = false;
                match result {
                    Ok((snapshot, transitions, terminals)) => {
                        shell.activity.accept_snapshot(snapshot);
                        match terminals {
                            Ok(terminals) => {
                                shell.activity.terminals = terminals;
                                shell.activity.terminal_error = None;
                            }
                            Err(error) => {
                                shell.activity.terminals.clear();
                                shell.activity.terminal_error = Some(error.to_string().into());
                            }
                        }
                        shell.activity.error = None;
                        shell.activity.armed = true;
                        for transition in transitions.transitions {
                            shell.deliver_activity_transition(transition, cx);
                        }
                    }
                    Err(error) => shell.activity.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn deliver_activity_transition(
        &mut self,
        transition: AttentionTransition,
        cx: &mut Context<Self>,
    ) {
        let (app_focused, local_device) = {
            let state = self.state.read(cx);
            (cx.active_window().is_some(), state.local_device_id.clone())
        };
        let policy = transition
            .item
            .task_id
            .as_ref()
            .and_then(|id| self.settings.task_notification_policies.get(id))
            .copied()
            .unwrap_or_default();
        let delivery = route_transition(
            &transition,
            DeliveryContext {
                policy,
                app_focused,
                desktop_enabled: self.settings.notifications_enabled,
                background_only: self.settings.notifications_background_only,
                local_device: local_device.as_deref(),
            },
        );
        if let Some(sound) = delivery.sound
            && self.settings.session_sound_enabled(sound)
        {
            crate::sound::play(sound);
        }
        if delivery.banner {
            let body = match transition.item.kind {
                AttentionKind::ApprovalRequired | AttentionKind::UserInputRequired => {
                    "A task needs your response."
                }
                _ => "A task needs attention.",
            };
            // The existing native banner API carries one opaque click string.
            // Include the owning environment so a delayed click cannot open
            // a same-id task after an in-place profile switch.
            let click_target = transition
                .item
                .task_id
                .as_ref()
                .map(|task_id| format!("l15:{}:{task_id}", transition.item.environment_id));
            crate::notify::post("LUNA Activity", body, click_target.as_deref());
        }
    }

    pub(super) fn toggle_activity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.activity.view_active {
            self.close_activity(window, cx);
            return;
        }
        if self.command_palette.is_some() {
            self.close_command_palette(window, cx);
        }
        self.add_space = None;
        self.close_spaces_menu(cx);
        self.close_sidebar_view_menu(cx);
        self.activity.view_active = true;
        self.sidebar_prev_order.clear();
        self.sidebar_resort.clear();
        self.sidebar_new_keys.clear();
        self.activity_sidebar_scroll
            .set_offset(gpui::Point::default());
        self.refresh_activity(cx);
        cx.notify();
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_open_activity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.activity.view_active {
            self.toggle_activity(window, cx);
        }
    }

    pub(super) fn close_activity(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.activity.view_active = false;
        self.sidebar_prev_order.clear();
        self.sidebar_resort.clear();
        self.sidebar_new_keys.clear();
        cx.notify();
    }

    pub(super) fn render_activity_load_state(&self, theme: &Theme) -> Option<AnyElement> {
        if !self.activity.view_active {
            return None;
        }
        let message = if self.activity.error.is_some() {
            "Activity may be out of date. Retrying…"
        } else if self.activity.loading && self.activity.snapshot.is_none() {
            "Loading activity…"
        } else {
            return None;
        };
        Some(
            div()
                .px(px(Theme::SPACE_SM))
                .pb(px(Theme::SPACE_SM))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .child(message)
                .into_any_element(),
        )
    }

    pub(super) fn activity_is_unavailable(&self) -> bool {
        self.activity.view_active && self.activity.error.is_some()
    }

    pub(super) fn review_attention_updated_at(
        &self,
        chat_id: &str,
        device_id: &str,
    ) -> Option<i64> {
        self.activity
            .snapshot
            .as_ref()?
            .needs_attention
            .iter()
            .filter(|item| {
                item.task_id.as_deref() == Some(chat_id)
                    && item.device_id == device_id
                    && review_clears_on_visit(item.kind)
            })
            .map(|item| item.updated_at_ms)
            .max()
    }

    pub(super) fn activity_task_keys(
        &self,
        cx: &App,
    ) -> std::collections::HashSet<(String, String)> {
        let state = self.state.read(cx);
        let mut keys: std::collections::HashSet<_> = self
            .activity
            .snapshot
            .as_ref()
            .into_iter()
            .flat_map(|snapshot| &snapshot.needs_attention)
            .filter_map(|item| {
                let task_id = item.task_id.as_ref()?;
                if review_clears_on_visit(item.kind)
                    && state.chats.iter().any(|chat| {
                        chat.id == *task_id
                            && chat.device_id == item.device_id
                            && !chat.manually_unread
                            && chat
                                .last_seen_at
                                .is_some_and(|seen| seen.timestamp_millis() >= item.updated_at_ms)
                    })
                {
                    return None;
                }
                Some((item.device_id.clone(), task_id.clone()))
            })
            .collect();
        if let Some(chat_id) = state.activity_chat_being_read()
            && let Some(chat) = state.chats.iter().find(|chat| chat.id == chat_id)
        {
            keys.insert((chat.device_id.clone(), chat.id.clone()));
        }
        keys
    }

    /// Keep recent completions discoverable in the bell view after they have
    /// been read. They do not count as unresolved attention or light its dot.
    pub(super) fn activity_recent_completion_keys(
        &self,
    ) -> std::collections::HashSet<(String, String)> {
        let cutoff = Utc::now().timestamp_millis() - chrono::Duration::days(7).num_milliseconds();
        self.activity
            .snapshot
            .as_ref()
            .into_iter()
            .flat_map(|snapshot| &snapshot.recent)
            .filter(|item| {
                item.updated_at_ms >= cutoff
                    && matches!(
                        item.kind,
                        AttentionKind::TaskCompleted | AttentionKind::TaskRecovered
                    )
            })
            .filter_map(|item| {
                item.task_id
                    .as_ref()
                    .map(|task_id| (item.device_id.clone(), task_id.clone()))
            })
            .collect()
    }

    /// Reading clears review-only rows from Activity, but queued work and
    /// unanswered requests still block settlement until actually resolved.
    pub(super) fn settlement_blocking_task_keys(
        &self,
    ) -> std::collections::HashSet<(String, String)> {
        self.activity
            .snapshot
            .as_ref()
            .into_iter()
            .flat_map(|snapshot| &snapshot.needs_attention)
            .filter(|item| {
                matches!(
                    item.kind,
                    AttentionKind::ApprovalRequired
                        | AttentionKind::UserInputRequired
                        | AttentionKind::QueuedSendFailed
                )
            })
            .filter_map(|item| {
                item.task_id
                    .as_ref()
                    .map(|task_id| (item.device_id.clone(), task_id.clone()))
            })
            .collect()
    }

    /// Running LUNA-owned PTYs remain visible even when their task is archived.
    /// Saved/interrupted records remain available through that task's terminal
    /// panel, but are never presented here as live processes.
    pub(super) fn render_terminal_inventory(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.activity.view_active {
            return None;
        }
        let running = self
            .activity
            .terminals
            .iter()
            .filter(|item| item.state == TerminalProcessState::Running)
            .cloned()
            .collect::<Vec<_>>();
        if running.is_empty() && self.activity.terminal_error.is_none() {
            return None;
        }
        let terminal_error = self.activity.terminal_error.clone();
        let state = self.state.read(cx);
        let rows = running
            .iter()
            .map(|item| {
                let task = state
                    .chats
                    .iter()
                    .find(|chat| chat.id == item.chat_id)
                    .and_then(|chat| chat.title.clone())
                    .unwrap_or_else(|| item.chat_id.clone());
                let device = state
                    .devices
                    .iter()
                    .find(|device| device.id == item.device_id)
                    .map(|device| device.name.clone())
                    .unwrap_or_else(|| item.device_id.clone());
                let open_task = item.chat_id.clone();
                let open_id = item.id.clone();
                let stop_id = item.id.clone();
                let open_key = format!("terminal-open-{}", item.id);
                div()
                    .id(SharedString::from(format!(
                        "terminal-inventory-{}",
                        item.id
                    )))
                    .px(px(Theme::SPACE_SM))
                    .py(px(6.0))
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .bg(theme.surface_raised)
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text)
                            .truncate()
                            .child(format!("{} · {}", task, item.shell)),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .truncate()
                            .child(format!("{} · {}", device, item.cwd)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(5.0))
                            .child(
                                popover::btn_ghost(theme, "Open", open_key)
                                    .id(SharedString::from(format!("terminal-open-{}", item.id)))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_chat(open_task.clone(), cx);
                                        this.terminal_panel(cx).update(cx, |panel, cx| {
                                            panel.focus_terminal_id(&open_task, &open_id, cx);
                                        });
                                        if !this.terminal_open(cx) {
                                            this.toggle_terminal(window, cx);
                                        }
                                    })),
                            )
                            .child(
                                popover::btn_ghost(
                                    theme,
                                    "Stop",
                                    format!("terminal-stop-{}", item.id),
                                )
                                .id(SharedString::from(format!("terminal-stop-{}", item.id)))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let Some(engine) = this.state.read(cx).engine().cloned()
                                        else {
                                            return;
                                        };
                                        let id = stop_id.clone();
                                        cx.spawn(async move |this, cx| {
                                            let result = engine
                                                .client()
                                                .call(
                                                    methods::CLOSE_TERMINAL,
                                                    serde_json::json!({ "terminalId": id }),
                                                )
                                                .await;
                                            let _ = this.update(cx, |shell, cx| {
                                                if let Err(error) = result {
                                                    shell.sidebar_notice = Some(
                                                        format!("Could not stop terminal: {error}")
                                                            .into(),
                                                    );
                                                }
                                                shell.activity.last_request = None;
                                                shell.refresh_activity(cx);
                                                cx.notify();
                                            });
                                        })
                                        .detach();
                                    },
                                )),
                            ),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        Some(
            div()
                .mt(px(8.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .px(px(Theme::SPACE_SM))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(format!("Running terminals ({})", running.len())),
                )
                .when_some(terminal_error, |el, _| {
                    el.child(
                        div()
                            .px(px(Theme::SPACE_SM))
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child("Running terminals unavailable. Retrying…"),
                    )
                })
                .children(rows)
                .into_any_element(),
        )
    }

    pub(super) fn render_activity_action(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.activity.view_active;
        let task_keys = self.activity_task_keys(cx);
        let has_attention = self
            .state
            .read(cx)
            .overview_chats(chrono::Utc::now())
            .iter()
            .any(|(status, chat)| {
                matches!(
                    status,
                    luna_proto::ChatIndicator::AwaitingApproval
                        | luna_proto::ChatIndicator::AwaitingInput
                ) || (matches!(
                    status,
                    luna_proto::ChatIndicator::Completed | luna_proto::ChatIndicator::Errored
                ) && chat.unseen())
                    || task_keys.contains(&(chat.device_id.clone(), chat.id.clone()))
            });
        /// Shared unread-dot size.
        const UNREAD_DOT: f32 = 6.0;
        div()
            .id("sidebar-activity-action")
            .relative()
            .size(px(29.0))
            .flex_none()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border.opacity(0.0))
            .flex()
            .items_center()
            .justify_center()
            .role(gpui::Role::Button)
            .aria_label(if active {
                "Turn off activity view"
            } else {
                "View activity"
            })
            .aria_toggled(if active {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .tab_index(0)
            .focus_visible({
                let ring = theme.focus_ring();
                move |style| crate::theme::focus_ring(style, ring)
            })
            .bg(if active {
                theme.glass_hover()
            } else {
                theme.glass_hover().opacity(0.0)
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme.glass_hover()))
            .tooltip(move |_, cx| {
                cx.new(|_| SurfaceTabTooltip {
                    text: (if active {
                        "Turn off activity view"
                    } else {
                        "View activity"
                    })
                    .into(),
                })
                .into()
            })
            .tooltip_show_delay(Duration::from_millis(350))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_activity(window, cx)))
            .on_key_down(crate::swallow_activation_key)
            .child(icon(icons::BELL).size(px(16.0)).text_color(if active {
                theme.accent
            } else {
                theme.text_muted
            }))
            .when(has_attention, |button| {
                // Seated on the bell's top-right shoulder (center ≈ 19.5,
                // 8.5 in the box) instead of floating at the corner (SH-26).
                button.child(
                    div()
                        .absolute()
                        .top(px(4.5))
                        .right(px(5.5))
                        .size(px(UNREAD_DOT))
                        .rounded_full()
                        .bg(theme.accent),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luna_proto::{
        ATTENTION_CONTRACT_VERSION, AttentionItem, AttentionLifecycle, AttentionSeverity,
        AttentionSourcePointer, AttentionTarget,
    };

    fn transition(kind: AttentionKind, task_id: &str) -> AttentionTransition {
        AttentionTransition {
            sequence: 1,
            transition: if kind == AttentionKind::TaskCompleted {
                AttentionTransitionKind::Recent
            } else {
                AttentionTransitionKind::NeedsAttention
            },
            item: AttentionItem {
                contract_version: ATTENTION_CONTRACT_VERSION,
                id: "id".into(),
                dedup_key: "key".into(),
                source_key: "source".into(),
                kind,
                severity: AttentionSeverity::Info,
                lifecycle: AttentionLifecycle::Unresolved,
                environment_id: "device".into(),
                device_id: "device".into(),
                task_id: Some(task_id.into()),
                turn_id: None,
                request_id: None,
                title: "Task".into(),
                summary: "Safe summary".into(),
                created_at_ms: 1,
                updated_at_ms: 1,
                seen_at_ms: None,
                acknowledged_at_ms: None,
                resolved_at_ms: None,
                target: AttentionTarget::Task {
                    task_id: task_id.into(),
                },
                source: AttentionSourcePointer {
                    task_id: Some(task_id.into()),
                    message_id: None,
                    part_id: None,
                    request_id: None,
                    queue_id: None,
                    recovery_id: None,
                    activity_event_id: None,
                },
            },
        }
    }

    #[test]
    fn completion_stays_in_activity_and_requests_can_notify_in_background() {
        let completed = transition(AttentionKind::TaskCompleted, "active");
        let focused = DeliveryContext {
            policy: TaskNotificationPolicy::All,
            app_focused: true,
            desktop_enabled: true,
            background_only: true,
            local_device: Some("device"),
        };
        let active = route_transition(&completed, focused);
        assert!(!active.banner);
        let other = route_transition(
            &transition(AttentionKind::ApprovalRequired, "other"),
            focused,
        );
        assert!(!other.banner);
        assert_eq!(other.sound, Some(crate::sound::Sound::Request));
    }

    #[test]
    fn background_muted_and_wrong_device_routes_are_bounded() {
        let completed = transition(AttentionKind::TaskCompleted, "other");
        let background_context = DeliveryContext {
            policy: TaskNotificationPolicy::All,
            app_focused: false,
            desktop_enabled: true,
            background_only: true,
            local_device: Some("device"),
        };
        let background = route_transition(&completed, background_context);
        assert!(!background.banner);
        for policy in [
            TaskNotificationPolicy::Muted,
            TaskNotificationPolicy::ImportantOnly,
        ] {
            assert_eq!(
                route_transition(
                    &completed,
                    DeliveryContext {
                        policy,
                        ..background_context
                    }
                ),
                Delivery::default()
            );
        }
        assert_eq!(
            route_transition(
                &completed,
                DeliveryContext {
                    local_device: Some("other"),
                    ..background_context
                }
            ),
            Delivery::default()
        );
    }

    #[test]
    fn async_snapshot_revision_fence_rejects_late_older_read() {
        let mut activity = ActivityUi::default();
        let snapshot = |revision, read_at_ms| AttentionSnapshot {
            contract_version: ATTENTION_CONTRACT_VERSION,
            revision,
            read_at_ms,
            unresolved_count: revision as u32,
            needs_attention: Vec::new(),
            recent: Vec::new(),
            producers: Vec::new(),
        };
        activity.accept_snapshot(snapshot(5, 50));
        activity.accept_snapshot(snapshot(4, 100));
        activity.accept_snapshot(snapshot(5, 40));
        assert_eq!(activity.snapshot.as_ref().unwrap().unresolved_count, 5);
        activity.accept_snapshot(snapshot(6, 1));
        assert_eq!(activity.snapshot.as_ref().unwrap().unresolved_count, 6);
    }

    #[test]
    fn taskless_local_connectivity_can_notify_without_a_selected_task() {
        let mut connection = transition(AttentionKind::ConnectivityUnavailable, "unused");
        connection.item.task_id = None;
        connection.item.source.task_id = None;
        let routed = route_transition(
            &connection,
            DeliveryContext {
                policy: TaskNotificationPolicy::All,
                app_focused: true,
                desktop_enabled: true,
                background_only: true,
                local_device: Some("device"),
            },
        );
        assert!(!routed.banner);
        assert_eq!(routed.sound, Some(crate::sound::Sound::Attention));
    }
}
