//! Activity → Host capacity (AW-10): this host's active turns against its
//! limit, the new turns waiting for a free slot (what, where in line, for how
//! long), and the scheduled automatic retries, each with its owner's control.
//!
//! The engine owns the line (`GetHostCapacity` / `SetHostCapacity`) and the
//! retry chains (`TaskRecovery` on the session). This section only reads them
//! and sends the owners' own commands: `cancelCapacityWait` for a held send,
//! `retryRecovery` / `cancelRecovery` for a retry (the transcript trailer's
//! intents). A queued row is opened in its task, where its queue controls
//! live. Replies for a replaced engine are dropped by the generation fence.

use gpui::{
    AnyElement, Context, Entity, EventEmitter, SharedString, Task, Window, div, prelude::*, px,
};
use luna_proto::{HostCapacity, HostCapacityWaitKind, RecoveryStatus};
use luna_rpc::{RpcError, methods};

use crate::button::{ButtonKind, ControlSize};
use crate::state::AppState;
use crate::theme::Theme;
use crate::typography::ui_rems;

/// The limits offered as chips; another value set through the API is shown
/// as one more chip.
const LIMIT_CHIPS: [Option<u32>; 5] = [None, Some(1), Some(2), Some(4), Some(8)];

pub(crate) enum HostCapacityEvent {
    /// Open the task that owns a waiting queued row.
    OpenTask(String),
}

/// What a waiting row offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitControl {
    /// A held send: withdraw it unsent.
    CancelSend,
    /// A held queued row: its controls live in the task's queue.
    OpenTask,
    /// A held retry: Send now (an explicit override) or Cancel.
    Retry,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WaitRow {
    pub task_id: String,
    pub source_id: String,
    pub title: String,
    pub reason: String,
    pub control: WaitControl,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RetryRow {
    pub task_id: String,
    pub chain_id: String,
    pub title: String,
    pub detail: String,
}

/// Exactly what the section draws, from the engine's line and this host's
/// sessions.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CapacityView {
    pub summary: String,
    pub limit: Option<u32>,
    pub chips: Vec<Option<u32>>,
    pub waiting: Vec<WaitRow>,
    pub retries: Vec<RetryRow>,
}

fn place(position: u32) -> String {
    let suffix = match (position % 10, position % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{position}{suffix}")
}

fn waited(ms: i64) -> String {
    let seconds = (ms / 1_000).max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3_600)
    }
}

fn task_title(state: &AppState, task_id: &str) -> String {
    state
        .chats
        .iter()
        .find(|chat| chat.id == task_id)
        .and_then(|chat| chat.title.clone())
        .unwrap_or_else(|| "New task".into())
}

pub(crate) fn capacity_view(
    snapshot: &HostCapacity,
    state: &AppState,
    now_ms: i64,
) -> CapacityView {
    let occupied = snapshot.occupied();
    let mut summary = match snapshot.limit {
        Some(limit) => format!("{occupied} of {limit} turns active"),
        None => format!(
            "{occupied} {} active · no limit",
            if occupied == 1 { "turn" } else { "turns" }
        ),
    };
    if !snapshot.waiting.is_empty() {
        summary.push_str(&format!(" · {} waiting", snapshot.waiting.len()));
    }
    let mut chips = LIMIT_CHIPS.to_vec();
    if let Some(limit) = snapshot.limit
        && !chips.contains(&Some(limit))
    {
        chips.push(Some(limit));
        chips.sort();
    }
    let waiting = snapshot
        .waiting
        .iter()
        .map(|wait| {
            let (what, control) = match wait.kind {
                HostCapacityWaitKind::Send => ("New message", WaitControl::CancelSend),
                HostCapacityWaitKind::QueuedMessage => ("Queued message", WaitControl::OpenTask),
                HostCapacityWaitKind::AutomaticRetry => ("Automatic retry", WaitControl::Retry),
            };
            WaitRow {
                task_id: wait.task_id.clone(),
                source_id: wait.source_id.clone(),
                title: task_title(state, &wait.task_id),
                reason: format!(
                    "{what} · waiting for a free turn · {} in line · {}",
                    place(wait.position),
                    waited(now_ms.saturating_sub(wait.since_ms))
                ),
                control,
            }
        })
        .collect();
    let retries = state
        .sessions
        .iter()
        .filter(|session| session.device_id == snapshot.device_id)
        .filter_map(|session| {
            let recovery = session.recovery.as_ref()?;
            let held = snapshot.waiting.iter().any(|wait| {
                wait.kind == HostCapacityWaitKind::AutomaticRetry
                    && wait.source_id == recovery.chain_id
            });
            (recovery.status == RecoveryStatus::Scheduled && !held).then(|| {
                let seconds = recovery
                    .retry_at_ms
                    .map(|at| at.saturating_sub(now_ms).saturating_add(999) / 1_000)
                    .unwrap_or_default()
                    .max(0);
                RetryRow {
                    task_id: session.chat_id.clone(),
                    chain_id: recovery.chain_id.clone(),
                    title: task_title(state, &session.chat_id),
                    detail: format!(
                        "Retrying in {seconds}s · attempt {}/{}",
                        recovery.attempt_count.saturating_add(1),
                        recovery.max_attempts
                    ),
                }
            })
        })
        .collect();
    CapacityView {
        summary,
        limit: snapshot.limit,
        chips,
        waiting,
        retries,
    }
}

fn chip_id(limit: Option<u32>) -> String {
    match limit {
        Some(limit) => format!("host-capacity-limit-{limit}"),
        None => "host-capacity-limit-off".into(),
    }
}

pub(crate) struct HostCapacityPanel {
    state: Entity<AppState>,
    snapshot: Option<HostCapacity>,
    /// The engine has no capacity owner (an older engine): no section.
    unsupported: bool,
    error: Option<SharedString>,
    /// The control whose command is in flight.
    busy: Option<SharedString>,
    /// Engine generation the shown values belong to.
    generation: u64,
    read: Option<Task<()>>,
    /// The control command in flight; dropping it cancels it.
    _action: Option<Task<()>>,
}

impl EventEmitter<HostCapacityEvent> for HostCapacityPanel {}

impl HostCapacityPanel {
    pub(crate) fn new(state: Entity<AppState>) -> Self {
        Self {
            state,
            snapshot: None,
            unsupported: false,
            error: None,
            busy: None,
            generation: 0,
            read: None,
            _action: None,
        }
    }

    /// Shown once this engine answered (or failed to), never for an engine
    /// without the capacity owner.
    pub(crate) fn visible(&self) -> bool {
        !self.unsupported && (self.snapshot.is_some() || self.error.is_some())
    }

    /// Read the line again (the Activity poll calls this). One read at a time;
    /// a replaced engine drops everything shown for its predecessor.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let (engine, generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        if generation != self.generation {
            self.generation = generation;
            self.snapshot = None;
            self.unsupported = false;
            self.error = None;
            self.busy = None;
            self.read = None;
            self._action = None;
            cx.notify();
        }
        if self.read.is_some() {
            return;
        }
        let Some(engine) = engine else {
            return;
        };
        let target = engine.engine_info().device_id.clone();
        self.read = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<HostCapacity>(
                    methods::GET_HOST_CAPACITY,
                    serde_json::json!({ "targetDeviceId": target }),
                )
                .await;
            this.update(cx, |panel, cx| {
                if panel.generation != generation {
                    return;
                }
                panel.read = None;
                match result {
                    Ok(snapshot) => {
                        panel.snapshot = Some(snapshot);
                        panel.unsupported = false;
                        panel.error = None;
                    }
                    Err(RpcError::UnknownMethod(_)) => {
                        panel.snapshot = None;
                        panel.unsupported = true;
                        panel.error = None;
                    }
                    Err(error) => {
                        panel.error = Some(format!("Host capacity unavailable: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Read again now, dropping a read that may predate the last action.
    fn reread(&mut self, cx: &mut Context<Self>) {
        self.read = None;
        self.refresh(cx);
    }

    fn set_limit(&mut self, limit: Option<u32>, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.snapshot.as_ref().is_some_and(|s| s.limit == limit) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.busy = Some(chip_id(limit).into());
        let generation = self.generation;
        let target = engine.engine_info().device_id.clone();
        self._action = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<HostCapacity>(
                    methods::SET_HOST_CAPACITY,
                    serde_json::json!({ "limit": limit, "targetDeviceId": target }),
                )
                .await;
            this.update(cx, |panel, cx| {
                if panel.generation != generation {
                    return;
                }
                panel.busy = None;
                match result {
                    Ok(snapshot) => {
                        panel.snapshot = Some(snapshot);
                        panel.error = None;
                    }
                    Err(error) => {
                        panel.error =
                            Some(format!("Could not change the turn limit: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Send one task's own command (the same intents its other surfaces use)
    /// and read the line again afterwards.
    fn task_command(
        &mut self,
        control: String,
        task_id: String,
        intent: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.busy = Some(control.into());
        let generation = self.generation;
        self._action = Some(cx.spawn(async move |this, cx| {
            let result = crate::task_commands::execute(&engine, &task_id, intent).await;
            this.update(cx, |panel, cx| {
                if panel.generation != generation {
                    return;
                }
                panel.busy = None;
                panel.error = result.err().map(|error| error.to_string().into());
                panel.reread(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn control(
        &self,
        id: String,
        label: &'static str,
        theme: &Theme,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let enabled = self.busy.is_none();
        crate::button::button(
            id.clone(),
            ButtonKind::Ghost,
            ControlSize::Sm,
            enabled,
            theme,
        )
        .debug_selector(move || id.clone())
        .aria_label(label)
        .child(label)
        .when(enabled, |el| {
            el.on_click(cx.listener(move |panel, _, _, cx| on_click(panel, cx)))
        })
        .into_any_element()
    }

    fn row(
        id: String,
        title: String,
        detail: String,
        controls: Vec<AnyElement>,
        theme: &Theme,
    ) -> AnyElement {
        div()
            .id(SharedString::from(id.clone()))
            .debug_selector(move || id.clone())
            .role(gpui::Role::Group)
            .aria_label(SharedString::from(format!("{title}: {detail}")))
            .px(px(Theme::SPACE_SM))
            .py(ui_rems(6.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .bg(theme.surface_raised)
            .flex()
            .flex_col()
            .gap(ui_rems(3.0))
            .child(
                div()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.text)
                    .truncate()
                    .child(title),
            )
            .child(
                div()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.readable_tone(theme.warning, 1.0))
                    .truncate()
                    .child(detail),
            )
            .when(!controls.is_empty(), |el| {
                el.child(div().flex().flex_row().gap(ui_rems(5.0)).children(controls))
            })
            .into_any_element()
    }
}

impl Render for HostCapacityPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let now = chrono::Utc::now().timestamp_millis();
        let view = self
            .snapshot
            .as_ref()
            .map(|snapshot| capacity_view(snapshot, self.state.read(cx), now));
        let header = div()
            .px(px(Theme::SPACE_SM))
            .text_size(ui_rems(11.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.readable_muted(0.6))
            .child("Host capacity");
        let mut body: Vec<AnyElement> = Vec::new();
        if let Some(view) = view {
            body.push(
                div()
                    .debug_selector(|| "host-capacity-summary".into())
                    .px(px(Theme::SPACE_SM))
                    .text_size(ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(view.summary.clone())
                    .into_any_element(),
            );
            let chips = view
                .chips
                .iter()
                .map(|limit| {
                    let limit = *limit;
                    let selected = limit == view.limit;
                    let id = chip_id(limit);
                    let selector = if selected {
                        format!("{id}-selected")
                    } else {
                        id.clone()
                    };
                    let label: SharedString = match limit {
                        Some(limit) => limit.to_string().into(),
                        None => "Off".into(),
                    };
                    let enabled = self.busy.is_none();
                    crate::button::button(
                        id,
                        if selected {
                            ButtonKind::Secondary
                        } else {
                            ButtonKind::Ghost
                        },
                        ControlSize::Sm,
                        enabled,
                        &theme,
                    )
                    .debug_selector(move || selector.clone())
                    .aria_label(SharedString::from(match limit {
                        Some(limit) => format!("Limit this host to {limit} active turns"),
                        None => "No turn limit on this host".to_string(),
                    }))
                    .aria_toggled(if selected {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .child(label)
                    .when(enabled && !selected, |el| {
                        el.on_click(cx.listener(move |panel, _, _, cx| panel.set_limit(limit, cx)))
                    })
                    .into_any_element()
                })
                .collect::<Vec<_>>();
            body.push(
                div()
                    .id("host-capacity-limit")
                    .role(gpui::Role::Group)
                    .aria_label("Turn limit for this host")
                    .px(px(Theme::SPACE_SM))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(4.0))
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child("Limit"),
                    )
                    .children(chips)
                    .into_any_element(),
            );
            for wait in &view.waiting {
                let controls = match wait.control {
                    WaitControl::CancelSend => {
                        let task_id = wait.task_id.clone();
                        let command_id = wait.source_id.clone();
                        let key = format!("host-capacity-cancel-{}", wait.source_id);
                        let control = key.clone();
                        vec![self.control(key, "Cancel", &theme, cx, move |panel, cx| {
                            panel.task_command(
                                control.clone(),
                                task_id.clone(),
                                serde_json::json!({
                                    "kind": "cancelCapacityWait",
                                    "commandId": command_id,
                                }),
                                cx,
                            )
                        })]
                    }
                    WaitControl::OpenTask => {
                        let task_id = wait.task_id.clone();
                        vec![self.control(
                            format!("host-capacity-open-{}", wait.source_id),
                            "Open",
                            &theme,
                            cx,
                            move |_, cx| cx.emit(HostCapacityEvent::OpenTask(task_id.clone())),
                        )]
                    }
                    WaitControl::Retry => {
                        self.retry_controls(&wait.task_id, &wait.source_id, &theme, cx)
                    }
                };
                body.push(Self::row(
                    format!("host-capacity-wait-{}", wait.source_id),
                    wait.title.clone(),
                    wait.reason.clone(),
                    controls,
                    &theme,
                ));
            }
            for retry in &view.retries {
                let controls = self.retry_controls(&retry.task_id, &retry.chain_id, &theme, cx);
                body.push(Self::row(
                    format!("host-capacity-retry-{}", retry.chain_id),
                    retry.title.clone(),
                    retry.detail.clone(),
                    controls,
                    &theme,
                ));
            }
        }
        if let Some(error) = self.error.clone() {
            body.push(
                div()
                    .debug_selector(|| "host-capacity-error".into())
                    .px(px(Theme::SPACE_SM))
                    .text_size(ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(error)
                    .into_any_element(),
            );
        }
        div()
            .id("host-capacity")
            .debug_selector(|| "host-capacity".into())
            .role(gpui::Role::Group)
            .aria_label("Host capacity")
            .mt(ui_rems(8.0))
            .flex()
            .flex_col()
            .gap(ui_rems(4.0))
            .child(header)
            .children(body)
    }
}

impl HostCapacityPanel {
    /// Send now (an explicit override that may exceed the limit) and Cancel
    /// for one retry chain: the transcript trailer's own intents.
    fn retry_controls(
        &self,
        task_id: &str,
        chain_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let now_key = format!("host-capacity-retry-now-{chain_id}");
        let cancel_key = format!("host-capacity-retry-cancel-{chain_id}");
        let (task, chain, control) = (task_id.to_owned(), chain_id.to_owned(), now_key.clone());
        let send_now = self.control(now_key, "Send now", theme, cx, move |panel, cx| {
            panel.task_command(
                control.clone(),
                task.clone(),
                serde_json::json!({ "kind": "retryRecovery", "chainId": chain }),
                cx,
            )
        });
        let (task, chain, control) = (task_id.to_owned(), chain_id.to_owned(), cancel_key.clone());
        let cancel = self.control(cancel_key, "Cancel", theme, cx, move |panel, cx| {
            panel.task_command(
                control.clone(),
                task.clone(),
                serde_json::json!({ "kind": "cancelRecovery", "chainId": chain }),
                cx,
            )
        });
        vec![send_now, cancel]
    }
}

#[cfg(test)]
mod tests {
    //! Rendered Host capacity states over held memory RPC and the real
    //! AppState engine-attachment path. No engine runs: the line is scripted,
    //! and what is checked is what the section shows and which commands its
    //! controls send.
    use super::*;
    use crate::engine_epoch_fixture::{self as fixture, HeldRpc};
    use gpui::{AppContext, TestAppContext, VisualTestContext};
    use luna_proto::{
        HostCapacityTurn, HostCapacityWait, RecoveryAcceptance, RecoveryAction, RecoveryFailure,
        Session, SessionStatus, TaskRecovery,
    };
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::runtime::Runtime;

    fn runtime(cx: &mut TestAppContext) -> Runtime {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| gpui_tokio::init_from_handle(cx, runtime.handle().clone()));
        runtime
    }

    struct Surface(Entity<HostCapacityPanel>);
    impl Render for Surface {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    /// Two active turns at a limit of two, and three starts in line: a send,
    /// a queued row and a due retry.
    fn full_line(device: &str, limit: Option<u32>, now: i64) -> Value {
        serde_json::to_value(HostCapacity {
            device_id: device.into(),
            limit,
            active: vec![
                HostCapacityTurn {
                    task_id: "task-a".into(),
                    status: SessionStatus::Working,
                },
                HostCapacityTurn {
                    task_id: "task-b".into(),
                    status: SessionStatus::AwaitingApproval,
                },
            ],
            starting: 0,
            waiting: vec![
                HostCapacityWait {
                    task_id: "task-c".into(),
                    kind: HostCapacityWaitKind::Send,
                    source_id: "send-c".into(),
                    position: 1,
                    since_ms: now - 12_000,
                },
                HostCapacityWait {
                    task_id: "task-d".into(),
                    kind: HostCapacityWaitKind::QueuedMessage,
                    source_id: "row-d".into(),
                    position: 2,
                    since_ms: now - 3_000,
                },
                HostCapacityWait {
                    task_id: "task-e".into(),
                    kind: HostCapacityWaitKind::AutomaticRetry,
                    source_id: "chain-e".into(),
                    position: 3,
                    since_ms: now,
                },
            ],
            read_at_ms: now,
        })
        .unwrap()
    }

    fn chat(id: &str, title: &str) -> luna_proto::Chat {
        serde_json::from_value(json!({
            "id": id,
            "deviceId": "device-a",
            "title": title,
            "archived": false,
            "createdAt": "2026-10-14T00:00:00Z",
        }))
        .unwrap()
    }

    fn scheduled(task: &str, chain: &str, retry_in_ms: i64, now: i64) -> Session {
        Session {
            last_completed_turn: None,
            chat_id: task.into(),
            device_id: "device-a".into(),
            status: SessionStatus::Errored,
            started_at: None,
            updated_at: chrono::Utc::now(),
            recovery: Some(TaskRecovery {
                chain_id: chain.into(),
                task_id: task.into(),
                original_message_id: format!("{task}-message"),
                original_command_id: None,
                original_acceptance: RecoveryAcceptance::RejectedBeforeAcceptance,
                acceptance: RecoveryAcceptance::RejectedBeforeAcceptance,
                failure: RecoveryFailure::Overloaded,
                action: RecoveryAction::ResendOriginal,
                status: RecoveryStatus::Scheduled,
                attempt_count: 1,
                max_attempts: 4,
                automatic: true,
                retry_at_ms: Some(now + retry_in_ms),
                automatic_deadline_ms: now + 3_600_000,
                environment_id: "device-a".into(),
                binding_id: None,
                binding_generation: None,
                detail: String::new(),
                created_at_ms: now,
                updated_at_ms: now,
            }),
        }
    }

    fn scene(
        cx: &mut TestAppContext,
        runtime: &Runtime,
        rpc: &Arc<HeldRpc>,
    ) -> (
        Entity<AppState>,
        Entity<HostCapacityPanel>,
        VisualTestContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| AppState::new());
        fixture::attach(cx, &state, fixture::handle(runtime, rpc.clone()));
        fixture::drain(cx, runtime);
        let panel = cx.new(|_| HostCapacityPanel::new(state.clone()));
        let rendered = panel.clone();
        let window = cx.open_window(gpui::size(px(320.0), px(900.0)), move |_, _| {
            Surface(rendered)
        });
        let visual = VisualTestContext::from_window(window.into(), cx);
        (state, panel, visual)
    }

    fn painted(
        visual: &mut VisualTestContext,
        selectors: &[&'static str],
    ) -> serde_json::Map<String, Value> {
        visual.update(|window, app| window.draw(app).clear());
        selectors
            .iter()
            .map(|selector| {
                (
                    (*selector).to_owned(),
                    Value::Bool(visual.debug_bounds(*selector).is_some()),
                )
            })
            .collect()
    }

    fn click(visual: &mut VisualTestContext, selector: &'static str) {
        visual.update(|window, app| window.draw(app).clear());
        let point = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not painted"))
            .center();
        visual.simulate_click(point, gpui::Modifiers::default());
    }

    fn requests(rpc: &HeldRpc, method: &str) -> Vec<Value> {
        rpc.trace()
            .into_iter()
            .filter(|event| event["phase"] == "request" && event["method"] == method)
            .map(|event| event["params"].clone())
            .collect()
    }

    const SELECTORS: [&str; 15] = [
        "host-capacity",
        "host-capacity-summary",
        "host-capacity-limit-off",
        "host-capacity-limit-2-selected",
        "host-capacity-limit-4",
        "host-capacity-limit-4-selected",
        "host-capacity-wait-send-c",
        "host-capacity-cancel-send-c",
        "host-capacity-wait-row-d",
        "host-capacity-open-row-d",
        "host-capacity-wait-chain-e",
        "host-capacity-retry-now-chain-e",
        "host-capacity-retry-chain-f",
        "host-capacity-retry-cancel-chain-f",
        "host-capacity-error",
    ];

    /// C4 and the controls: the count against the limit, each waiting start
    /// with what waits, its place and age, and its owner's control; a
    /// scheduled retry outside the line with its countdown. Cancel sends
    /// `cancelCapacityWait` for that exact command, Open names its task, and
    /// a limit chip sends `SetHostCapacity` and shows the engine's answer.
    #[gpui::test]
    fn activity_capacity_renders_counts_reasons_and_controls(cx: &mut TestAppContext) {
        let runtime = runtime(cx);
        let _entered = runtime.enter();
        let now = chrono::Utc::now().timestamp_millis();
        let rpc = HeldRpc::new("capacity-a", "device-a");
        rpc.reply(
            methods::GET_HOST_CAPACITY,
            Ok(full_line("device-a", Some(2), now)),
            false,
        );
        rpc.reply(
            methods::SET_HOST_CAPACITY,
            Ok(full_line("device-a", Some(4), now)),
            false,
        );
        rpc.reply(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            Ok(json!({
                "snapshot": {
                    "taskId": "task-c",
                    "taskRevision": 3,
                    "taskSequence": 0,
                    "bindingGeneration": 5,
                },
                "events": [],
                "reloadRequired": false,
            })),
            false,
        );
        rpc.reply(
            methods::EXECUTE_TASK_COMMAND,
            Err("the fixture records the command".into()),
            false,
        );
        let (state, panel, mut visual) = scene(cx, &runtime, &rpc);
        state.update(cx, |state, _| {
            state.chats = vec![
                chat("task-c", "Charlie"),
                chat("task-d", "Delta"),
                chat("task-e", "Echo"),
                chat("task-f", "Foxtrot"),
            ];
            state.sessions = vec![
                scheduled("task-e", "chain-e", -1_000, now),
                scheduled("task-f", "chain-f", 40_000, now),
            ];
        });
        let opened = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let _opens = cx.update(|cx| {
            let opened = opened.clone();
            cx.subscribe(&panel, move |_, event: &HostCapacityEvent, _| match event {
                HostCapacityEvent::OpenTask(task) => opened.lock().unwrap().push(task.clone()),
            })
        });
        panel.update(cx, |panel, cx| panel.refresh(cx));
        fixture::pump_until(cx, &runtime, |cx| {
            panel.read_with(cx, |panel, _| panel.snapshot.is_some())
        });
        assert!(panel.read_with(cx, |panel, _| panel.visible()));
        let view = panel.read_with(cx, |panel, cx| {
            capacity_view(panel.snapshot.as_ref().unwrap(), panel.state.read(cx), now)
        });
        assert_eq!(view.summary, "2 of 2 turns active · 3 waiting");
        assert_eq!(view.chips, vec![None, Some(1), Some(2), Some(4), Some(8)]);
        let reasons: Vec<(&str, &str, WaitControl)> = view
            .waiting
            .iter()
            .map(|row| (row.title.as_str(), row.reason.as_str(), row.control))
            .collect();
        assert_eq!(
            reasons,
            vec![
                (
                    "Charlie",
                    "New message · waiting for a free turn · 1st in line · 12s",
                    WaitControl::CancelSend
                ),
                (
                    "Delta",
                    "Queued message · waiting for a free turn · 2nd in line · 3s",
                    WaitControl::OpenTask
                ),
                (
                    "Echo",
                    "Automatic retry · waiting for a free turn · 3rd in line · 0s",
                    WaitControl::Retry
                ),
            ]
        );
        assert_eq!(view.retries.len(), 1, "a retry in line is listed once");
        assert_eq!(view.retries[0].title, "Foxtrot");
        assert_eq!(view.retries[0].detail, "Retrying in 40s · attempt 2/4");
        let shown = painted(&mut visual, &SELECTORS);
        for selector in [
            "host-capacity",
            "host-capacity-summary",
            "host-capacity-limit-off",
            "host-capacity-limit-2-selected",
            "host-capacity-limit-4",
            "host-capacity-wait-send-c",
            "host-capacity-cancel-send-c",
            "host-capacity-wait-row-d",
            "host-capacity-open-row-d",
            "host-capacity-wait-chain-e",
            "host-capacity-retry-now-chain-e",
            "host-capacity-retry-chain-f",
            "host-capacity-retry-cancel-chain-f",
        ] {
            assert_eq!(shown[selector], true, "{selector} must render");
        }
        assert_eq!(shown["host-capacity-limit-4-selected"], false);
        assert_eq!(shown["host-capacity-error"], false);

        click(&mut visual, "host-capacity-open-row-d");
        fixture::drain(cx, &runtime);
        assert_eq!(*opened.lock().unwrap(), vec!["task-d".to_string()]);

        click(&mut visual, "host-capacity-limit-4");
        fixture::pump_until(cx, &runtime, |cx| {
            panel.read_with(cx, |panel, _| {
                panel
                    .snapshot
                    .as_ref()
                    .is_some_and(|line| line.limit == Some(4))
            })
        });
        let limits = requests(&rpc, methods::SET_HOST_CAPACITY);
        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0]["limit"], json!(4));
        assert_eq!(limits[0]["targetDeviceId"], json!("device-a"));
        let after_limit = painted(&mut visual, &SELECTORS);
        assert_eq!(after_limit["host-capacity-limit-4-selected"], true);

        click(&mut visual, "host-capacity-cancel-send-c");
        fixture::pump_until(cx, &runtime, |_| {
            rpc.calls(methods::EXECUTE_TASK_COMMAND) >= 1
        });
        fixture::pump_until(cx, &runtime, |cx| {
            panel.read_with(cx, |panel, _| panel.busy.is_none())
        });
        let commands = requests(&rpc, methods::EXECUTE_TASK_COMMAND);
        assert_eq!(commands[0]["taskId"], json!("task-c"));
        assert_eq!(commands[0]["intent"]["kind"], json!("cancelCapacityWait"));
        assert_eq!(commands[0]["intent"]["commandId"], json!("send-c"));
        assert!(
            commands[0].get("expectedBindingGeneration").is_none(),
            "a held send is not fenced by a provider binding"
        );
        let after_cancel = painted(&mut visual, &SELECTORS);
        assert_eq!(
            after_cancel["host-capacity-error"], true,
            "a refused or failed command is shown, not swallowed"
        );
        assert!(
            rpc.calls(methods::GET_HOST_CAPACITY) >= 2,
            "the line is read again after a control"
        );

        fixture::write_trace(
            "aw10-activity-capacity",
            &json!({
                "source": file!(),
                "summary": view.summary,
                "waiting": view.waiting.iter().map(|row| json!({
                    "title": row.title, "reason": row.reason,
                })).collect::<Vec<_>>(),
                "retries": view.retries.iter().map(|row| json!({
                    "title": row.title, "detail": row.detail,
                })).collect::<Vec<_>>(),
                "painted": shown,
                "afterLimit": after_limit,
                "afterCancel": after_cancel,
                "rpc": rpc.trace(),
            }),
        );
    }

    /// U1, U2: a reply still due from a replaced engine never overwrites the
    /// new engine's line, and an engine without the capacity owner hides the
    /// section instead of showing an error.
    #[gpui::test]
    fn activity_capacity_fences_replaced_engines_and_hides_without_an_owner(
        cx: &mut TestAppContext,
    ) {
        let runtime = runtime(cx);
        let _entered = runtime.enter();
        let now = chrono::Utc::now().timestamp_millis();
        let old = HeldRpc::new("capacity-old", "device-old");
        old.reply(
            methods::GET_HOST_CAPACITY,
            Ok(full_line("device-old", Some(8), now)),
            true,
        );
        let (state, panel, _visual) = scene(cx, &runtime, &old);
        panel.update(cx, |panel, cx| panel.refresh(cx));
        fixture::pump_until(cx, &runtime, |_| old.calls(methods::GET_HOST_CAPACITY) == 1);

        let new = HeldRpc::new("capacity-new", "device-new");
        new.reply(
            methods::GET_HOST_CAPACITY,
            Ok(full_line("device-new", Some(2), now)),
            false,
        );
        fixture::attach(cx, &state, fixture::handle(&runtime, new.clone()));
        panel.update(cx, |panel, cx| panel.refresh(cx));
        fixture::pump_until(cx, &runtime, |_| {
            new.settled(methods::GET_HOST_CAPACITY) == 1
        });
        old.release(methods::GET_HOST_CAPACITY);
        fixture::pump_until(cx, &runtime, |_| {
            old.settled(methods::GET_HOST_CAPACITY) == 1
        });
        fixture::drain(cx, &runtime);
        let shown = panel.read_with(cx, |panel, _| {
            panel
                .snapshot
                .as_ref()
                .map(|line| (line.device_id.clone(), line.limit))
        });
        assert_eq!(shown, Some(("device-new".to_string(), Some(2))));

        // An engine that does not serve the capacity owner (unknown method).
        let older = HeldRpc::new("capacity-older", "device-older");
        fixture::attach(cx, &state, fixture::handle(&runtime, older.clone()));
        panel.update(cx, |panel, cx| panel.refresh(cx));
        fixture::pump_until(cx, &runtime, |_| {
            older.settled(methods::GET_HOST_CAPACITY) == 1
        });
        fixture::drain(cx, &runtime);
        let hidden = panel.read_with(cx, |panel, _| {
            (panel.visible(), panel.unsupported, panel.error.clone())
        });
        assert_eq!(hidden, (false, true, None));
        fixture::write_trace(
            "aw10-activity-capacity-engines",
            &json!({
                "source": file!(),
                "oldEngine": old.trace(),
                "newEngine": new.trace(),
                "olderEngine": older.trace(),
            }),
        );
    }
}
