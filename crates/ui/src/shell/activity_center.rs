//! Existing-style Activity action, persistent rows, exact navigation, and one
//! transition router. The engine owns lifecycle; the Shell owns presentation.

use super::*;
use crate::settings::TaskNotificationPolicy;
use zeron_proto::{
    AttentionItem, AttentionKind, AttentionMutation, AttentionMutationRequest,
    AttentionMutationResult, AttentionReadRequest, AttentionSnapshot, AttentionTarget,
    AttentionTransition, AttentionTransitionBatch, AttentionTransitionClaim,
    AttentionTransitionKind,
};

#[derive(Default)]
pub(super) struct ActivityUi {
    pub(super) open: bool,
    snapshot: Option<AttentionSnapshot>,
    error: Option<SharedString>,
    loading: bool,
    in_flight: bool,
    armed: bool,
    engine_generation: u64,
    request_generation: u64,
    last_request: Option<std::time::Instant>,
    poll_task: Option<Task<()>>,
    ticker: Option<Task<()>>,
    mutation_task: Option<Task<()>>,
    queued_mutations: std::collections::VecDeque<PendingMutation>,
    previous_focus: Option<FocusHandle>,
    focus: Option<FocusHandle>,
    pub(super) notice: Option<AttentionItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingMutation {
    mutation: AttentionMutation,
    retries: u8,
    command_id: Option<String>,
}

impl PendingMutation {
    fn retry_with_command_id(&self, command_id: String) -> Self {
        Self {
            mutation: self.mutation.clone(),
            retries: self.retries.saturating_add(1),
            command_id: Some(command_id),
        }
    }
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

    fn queue_mutation(&mut self, pending: PendingMutation) -> bool {
        if self
            .queued_mutations
            .iter()
            .any(|queued| queued.mutation == pending.mutation)
        {
            return true;
        }
        if self.queued_mutations.len() >= 32 {
            return false;
        }
        self.queued_mutations.push_back(pending);
        true
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Delivery {
    in_app: bool,
    sound: Option<crate::sound::Sound>,
    banner: bool,
}

#[derive(Clone, Copy)]
struct DeliveryContext<'a> {
    policy: TaskNotificationPolicy,
    app_focused: bool,
    selected_task: Option<&'a str>,
    in_app_enabled: bool,
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
    let selected = item.task_id.is_some() && item.task_id.as_deref() == context.selected_task;
    Delivery {
        in_app: context.in_app_enabled && context.app_focused && !selected,
        sound: Some(sound),
        banner: context.desktop_enabled
            && (!context.background_only || !context.app_focused)
            && !(context.app_focused && selected && completion),
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
                crate::power::selected_active_task(
                    state.selected_chat.as_deref(),
                    &self.settings.keep_awake_task_ids,
                    state.local_device_id.as_deref(),
                    &state.sessions,
                    Utc::now(),
                )
                .map(str::to_owned)
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
            self.activity.mutation_task = None;
            self.activity.queued_mutations.clear();
            self.activity.snapshot = None;
            self.activity.error = None;
            self.activity.armed = false;
            self.activity.in_flight = false;
            self.activity.notice = None;
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
                .map_err(|error| zeron_rpc::RpcError::Failed(error.to_string()))?;
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
                .map_err(|error| zeron_rpc::RpcError::Failed(error.to_string()))?;
                Ok::<_, zeron_rpc::RpcError>((snapshot, transitions))
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
                    Ok((snapshot, transitions)) => {
                        shell.activity.accept_snapshot(snapshot);
                        shell.activity.error = None;
                        shell.activity.armed = true;
                        for transition in transitions.transitions {
                            shell.deliver_activity_transition(transition, cx);
                        }
                        if shell.activity.mutation_task.is_none()
                            && let Some(pending) = shell.activity.queued_mutations.pop_front()
                        {
                            shell.mutate_activity_with_retry(pending, cx);
                        }
                        if shell.activity.open {
                            shell.mark_initial_activity_seen(cx);
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
        let (app_focused, selected, local_device) = {
            let state = self.state.read(cx);
            (
                cx.active_window().is_some(),
                state.selected_chat.clone(),
                state.local_device_id.clone(),
            )
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
                selected_task: selected.as_deref(),
                in_app_enabled: self.settings.in_app_notices_enabled,
                desktop_enabled: self.settings.notifications_enabled,
                background_only: self.settings.notifications_background_only,
                local_device: local_device.as_deref(),
            },
        );
        if delivery.in_app {
            self.activity.notice = Some(transition.item.clone());
        }
        if let Some(sound) = delivery.sound
            && self.settings.session_sound_enabled(sound)
        {
            crate::sound::play(sound);
        }
        if delivery.banner {
            let body = match transition.item.kind {
                AttentionKind::TaskCompleted | AttentionKind::TaskRecovered => "A task finished.",
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
        if self.activity.open {
            self.close_activity(window, cx);
            return;
        }
        if self.command_palette.is_some() {
            self.close_command_palette(window, cx);
        }
        self.add_space = None;
        self.activity.open = true;
        self.activity.previous_focus = window.focused(cx);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.activity.focus = Some(focus);
        self.refresh_activity(cx);
        self.mark_initial_activity_seen(cx);
        cx.notify();
    }

    pub(super) fn close_activity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activity.open = false;
        self.activity.focus = None;
        if let Some(previous) = self.activity.previous_focus.take() {
            window.focus(&previous, cx);
        } else {
            window.focus(&self.shortcut_focus, cx);
        }
        cx.notify();
    }

    fn mark_initial_activity_seen(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = &self.activity.snapshot else {
            return;
        };
        let ids = snapshot
            .needs_attention
            .iter()
            .take(5)
            .chain(snapshot.recent.iter().take(5))
            .filter(|item| item.seen_at_ms.is_none())
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        if !ids.is_empty() && self.activity.mutation_task.is_none() {
            self.mutate_activity(AttentionMutation::MarkSeen { item_ids: ids }, cx);
        }
    }

    fn mutate_activity(&mut self, mutation: AttentionMutation, cx: &mut Context<Self>) {
        self.mutate_activity_with_retry(
            PendingMutation {
                mutation,
                retries: 0,
                command_id: None,
            },
            cx,
        );
    }

    fn mutate_activity_with_retry(&mut self, pending: PendingMutation, cx: &mut Context<Self>) {
        let Some(snapshot) = &self.activity.snapshot else {
            return;
        };
        let (handle, engine_generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(handle) = handle else {
            self.activity.error = Some("Activity is unavailable while reconnecting.".into());
            return;
        };
        if self.activity.mutation_task.is_some() {
            if !self.activity.queue_mutation(pending) {
                self.activity.error =
                    Some("Activity has too many pending actions; try again shortly.".into());
                cx.notify();
            }
            return;
        }
        let generation = self.activity.request_generation;
        let command_id = pending
            .command_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let retry = pending.retry_with_command_id(command_id.clone());
        let request = AttentionMutationRequest {
            command_id,
            expected_revision: snapshot.revision,
            mutation: pending.mutation,
            target_device_id: Some(handle.engine_info().device_id.clone()),
        };
        self.activity.mutation_task = Some(cx.spawn(async move |this, cx| {
            let result = handle
                .client()
                .call(
                    methods::MUTATE_ATTENTION,
                    serde_json::to_value(request).unwrap_or_default(),
                )
                .await
                .and_then(|value| {
                    serde_json::from_value::<AttentionMutationResult>(value)
                        .map_err(|error| zeron_rpc::RpcError::Failed(error.to_string()))
                });
            this.update(cx, |shell, cx| {
                if generation != shell.activity.request_generation
                    || engine_generation != shell.state.read(cx).engine_generation()
                {
                    return;
                }
                shell.activity.mutation_task = None;
                match result {
                    Ok(result) => {
                        shell.activity.accept_snapshot(result.snapshot);
                        shell.activity.error = None;
                        if let Some(next) = shell.activity.queued_mutations.pop_front() {
                            shell.mutate_activity_with_retry(next, cx);
                        }
                    }
                    Err(error) => {
                        shell.activity.error = Some(error.to_string().into());
                        if pending.retries == 0 {
                            shell.activity.queued_mutations.push_front(retry);
                        }
                        shell.activity.last_request = None;
                        shell.refresh_activity(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn open_attention_item(
        &mut self,
        item: AttentionItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let same_environment = self
            .state
            .read(cx)
            .engine()
            .is_some_and(|engine| engine.engine_info().device_id == item.environment_id);
        if !same_environment {
            self.activity.error = Some("This Activity item belongs to another environment.".into());
            cx.notify();
            return;
        }
        if item.task_id.as_deref() != item.source.task_id.as_deref() {
            self.activity.error = Some("The Activity source no longer matches its task.".into());
            cx.notify();
            return;
        }
        match item.target {
            AttentionTarget::Task { task_id }
            | AttentionTarget::TaskQueue { task_id, .. }
            | AttentionTarget::TaskRecovery { task_id, .. } => {
                if !self.attention_task_available(&task_id, &item.device_id, cx) {
                    return;
                }
                self.close_activity(window, cx);
                self.open_chat(task_id, cx);
            }
            AttentionTarget::TaskRequest {
                task_id,
                request_id,
                message_id,
            } => {
                if item.request_id.as_deref() != Some(request_id.as_str())
                    || item.source.request_id.as_deref() != Some(request_id.as_str())
                    || item.source.message_id.as_deref() != Some(message_id.as_str())
                    || !self.attention_task_available(&task_id, &item.device_id, cx)
                {
                    self.activity.error =
                        Some("The exact Activity request is unavailable here.".into());
                    cx.notify();
                    return;
                }
                self.transcript.update(cx, |transcript, cx| {
                    transcript.navigate_to_message(task_id.clone(), message_id, cx)
                });
                self.close_activity(window, cx);
                self.open_chat(task_id, cx);
            }
            AttentionTarget::SettingsAccounts => {
                self.close_activity(window, cx);
                self.open_settings(SettingsSection::Agents, cx);
            }
            AttentionTarget::SettingsDevices => {
                self.close_activity(window, cx);
                self.open_settings(SettingsSection::Devices, cx);
            }
            AttentionTarget::Unavailable { reason } => {
                self.activity.error = Some(reason.into());
                cx.notify();
            }
        }
    }

    fn attention_task_available(
        &mut self,
        task_id: &str,
        device_id: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let found = self
            .state
            .read(cx)
            .chats
            .iter()
            .any(|chat| chat.id == task_id && chat.device_id == device_id);
        if !found {
            self.activity.error = Some("The exact owning task is unavailable here.".into());
            cx.notify();
        }
        found
    }

    pub(super) fn render_activity_notice(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let item = self.activity.notice.clone()?;
        let open_item = item.clone();
        Some(
            div()
                .id("activity-in-app-notice")
                .mx(px(Theme::SPACE_SM))
                .mb(px(4.0))
                .px(px(Theme::SPACE_SM))
                .py(px(7.0))
                .rounded(px(8.0))
                .bg(theme.accent_wash)
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(crate::typography::ui_rems(11.0))
                        .child(item.title),
                )
                .child(
                    popover::btn_ghost(theme, "Open", "activity-notice-open")
                        .id("activity-notice-open")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.activity.notice = None;
                            this.open_attention_item(open_item.clone(), window, cx);
                        })),
                )
                .child(
                    popover::btn_ghost(theme, "×", "activity-notice-close")
                        .id("activity-notice-close")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.activity.notice = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_activity_action(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = self
            .activity
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.unresolved_count);
        let wide = self.viewport_width >= 1040.0;
        let popup = if self.activity.open && wide {
            Some(popover::anchored_menu_above(
                "activity-popover",
                self.render_activity_card(theme, cx),
                None,
            ))
        } else {
            None
        };
        div()
            .id("sidebar-activity-action")
            .mx(px(Theme::SPACE_SM))
            .px(px(Theme::SPACE_SM))
            .py(px(7.0))
            .rounded(px(8.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .role(gpui::Role::Button)
            .aria_label(if count == 0 {
                "View Activity".into()
            } else {
                format!("View Activity, {count} needs attention")
            })
            .tab_index(0)
            .focus_visible(move |style| style.border_2().border_color(theme.accent))
            .cursor_pointer()
            .hover(|style| style.bg(theme.glass_hover()))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_activity(window, cx)))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") && !event.is_held {
                    this.toggle_activity(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                icon(icons::BELL)
                    .size(px(16.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(crate::typography::ui_rems(12.0))
                    .child("Activity"),
            )
            .when(count > 0, |row| {
                row.child(
                    div()
                        .px(px(5.0))
                        .rounded(px(5.0))
                        .bg(theme.warning.opacity(0.16))
                        .text_color(theme.warning)
                        .text_size(crate::typography::ui_rems(10.0))
                        .child(format!("{count}")),
                )
            })
            .children(popup)
            .into_any_element()
    }

    pub(super) fn render_activity_compact_overlay(
        &mut self,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.activity.open || self.viewport_width >= 1040.0 {
            return None;
        }
        let theme = Theme::of(cx).for_popup();
        Some(popover::modal(
            "activity-compact-dialog",
            viewport,
            self.render_activity_card(&theme, cx),
        ))
    }

    fn render_activity_card(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let snapshot = self.activity.snapshot.clone();
        let error = self.activity.error.clone();
        let needs = snapshot
            .as_ref()
            .map_or(&[][..], |snapshot| snapshot.needs_attention.as_slice());
        let recent = snapshot
            .as_ref()
            .map_or(&[][..], |snapshot| snapshot.recent.as_slice());
        let can_clear = !recent.is_empty();
        let focus = self.activity.focus.clone();
        popover::popover_card(theme)
            .id("activity-center")
            .w(px(380.0))
            .max_h(px(480.0))
            .when_some(focus, |card, focus| card.track_focus(&focus))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_activity(window, cx)))
            .child(
                div()
                    .px(px(10.0))
                    .py(px(8.0))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Activity"),
                    )
                    .when(can_clear, |row| {
                        row.child(
                            popover::btn_ghost(theme, "Clear recent", "activity-clear-recent")
                                .id("activity-clear-recent")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.mutate_activity(AttentionMutation::ClearRecent, cx)
                                })),
                        )
                    }),
            )
            .when_some(error, |card, error| {
                card.child(
                    div()
                        .px(px(10.0))
                        .py(px(5.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .child(
                div()
                    .id("activity-scroll")
                    .max_h(px(420.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .child(self.render_activity_section("Needs attention", needs, theme, cx))
                    .child(self.render_activity_section("Recent", recent, theme, cx)),
            )
            .into_any_element()
    }

    fn render_activity_section(
        &self,
        label: &'static str,
        items: &[AttentionItem],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .px(px(6.0))
            .pb(px(8.0))
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(9.0))
                    .py(px(5.0))
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(theme.text_faint)
                    .child(label),
            )
            .when(items.is_empty(), |column| {
                column.child(
                    div()
                        .px(px(9.0))
                        .py(px(7.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(if label == "Recent" {
                            "No recent activity"
                        } else {
                            "Nothing needs attention"
                        }),
                )
            })
            .children(
                items
                    .iter()
                    .cloned()
                    .map(|item| self.render_activity_row(item, theme, cx)),
            )
            .into_any_element()
    }

    fn render_activity_row(
        &self,
        item: AttentionItem,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = item.id.clone();
        let title = item.title.clone();
        let summary = item.summary.clone();
        let time = chrono::DateTime::from_timestamp_millis(item.updated_at_ms)
            .map(|at| format_time_ago(at, Utc::now()))
            .unwrap_or_else(|| "now".into());
        let task_label = item
            .task_id
            .as_ref()
            .and_then(|id| {
                self.state
                    .read(cx)
                    .chats
                    .iter()
                    .find(|chat| &chat.id == id)
                    .and_then(|chat| chat.title.clone())
            })
            .unwrap_or_else(|| "Task".into());
        let unresolved = item.unresolved() && item.actionable();
        let unseen = item.seen_at_ms.is_none();
        div()
            .id(SharedString::from(format!("activity-row-{id}")))
            .px(px(9.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .role(gpui::Role::Button)
            .aria_label(format!("{title}, {task_label}, {time}"))
            .tab_index(0)
            .focus_visible(move |style| style.border_2().border_color(theme.accent))
            .cursor_pointer()
            .hover(|style| style.bg(theme.glass_hover()))
            .on_hover({
                let id = id.clone();
                cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered && unseen {
                        this.mutate_activity(
                            AttentionMutation::MarkSeen {
                                item_ids: vec![id.clone()],
                            },
                            cx,
                        );
                    }
                })
            })
            .on_click({
                let item = item.clone();
                cx.listener(move |this, _, window, cx| {
                    this.open_attention_item(item.clone(), window, cx)
                })
            })
            .on_key_down({
                let item = item.clone();
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") && !event.is_held {
                        this.open_attention_item(item.clone(), window, cx);
                        cx.stop_propagation();
                    }
                })
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_weight(if unseen {
                                gpui::FontWeight::SEMIBOLD
                            } else {
                                gpui::FontWeight::MEDIUM
                            })
                            .text_size(crate::typography::ui_rems(12.0))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(10.0))
                            .text_color(theme.text_faint)
                            .child(time),
                    ),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(summary),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(theme.text_faint)
                    .child(task_label)
                    .when(unresolved, |row| {
                        let item_id = id.clone();
                        row.child(
                            popover::btn_ghost(theme, "Acknowledge", format!("ack-{id}"))
                                .id(SharedString::from(format!("activity-ack-{id}")))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.mutate_activity(
                                        AttentionMutation::Acknowledge {
                                            item_id: item_id.clone(),
                                        },
                                        cx,
                                    );
                                })),
                        )
                    })
                    .when(!unresolved, |row| {
                        let item_id = id.clone();
                        row.child(
                            popover::btn_ghost(theme, "Dismiss", format!("dismiss-{id}"))
                                .id(SharedString::from(format!("activity-dismiss-{id}")))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.mutate_activity(
                                        AttentionMutation::DismissRecent {
                                            item_id: item_id.clone(),
                                        },
                                        cx,
                                    );
                                })),
                        )
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_proto::{
        ATTENTION_CONTRACT_VERSION, AttentionLifecycle, AttentionSeverity, AttentionSourcePointer,
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
    fn active_completion_has_no_notice_and_other_task_routes_once() {
        let completed = transition(AttentionKind::TaskCompleted, "active");
        let focused = DeliveryContext {
            policy: TaskNotificationPolicy::All,
            app_focused: true,
            selected_task: Some("active"),
            in_app_enabled: true,
            desktop_enabled: true,
            background_only: true,
            local_device: Some("device"),
        };
        let active = route_transition(&completed, focused);
        assert!(!active.in_app && !active.banner);
        let other = route_transition(
            &transition(AttentionKind::ApprovalRequired, "other"),
            focused,
        );
        assert!(other.in_app);
        assert!(!other.banner);
        assert_eq!(other.sound, Some(crate::sound::Sound::Request));
    }

    #[test]
    fn background_muted_and_wrong_device_routes_are_bounded() {
        let completed = transition(AttentionKind::TaskCompleted, "other");
        let background_context = DeliveryContext {
            policy: TaskNotificationPolicy::All,
            app_focused: false,
            selected_task: Some("active"),
            in_app_enabled: true,
            desktop_enabled: true,
            background_only: true,
            local_device: Some("device"),
        };
        let background = route_transition(&completed, background_context);
        assert!(background.banner && !background.in_app);
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
                selected_task: None,
                in_app_enabled: true,
                desktop_enabled: true,
                background_only: true,
                local_device: Some("device"),
            },
        );
        assert!(routed.in_app);
        assert!(!routed.banner);
        assert_eq!(routed.sound, Some(crate::sound::Sound::Attention));
    }

    #[test]
    fn pending_actions_are_serialized_and_duplicate_clicks_coalesce() {
        let mut activity = ActivityUi::default();
        let action = PendingMutation {
            mutation: AttentionMutation::Acknowledge {
                item_id: "synthetic-item".into(),
            },
            retries: 0,
            command_id: None,
        };
        assert!(activity.queue_mutation(action.clone()));
        assert!(activity.queue_mutation(action.clone()));
        assert_eq!(activity.queued_mutations.len(), 1);
        assert_eq!(activity.queued_mutations.pop_front(), Some(action.clone()));
        let retry = action.retry_with_command_id("same-command".into());
        assert_eq!(retry.retries, 1);
        assert_eq!(retry.command_id.as_deref(), Some("same-command"));
    }
}
