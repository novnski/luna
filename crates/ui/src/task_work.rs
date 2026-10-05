//! Shared background work (harness evolution slice 3): the focused chat's
//! host-projected work, read through one watch, and the explicit owned-work
//! stop. Ownership, blocking, attention and stop authority are the host's;
//! nothing here infers them from ancestry, kind or unread state, and no
//! native identifier is ever shown.

use gpui::{Context, Task};
use luna_proto::{
    TaskWorkItem, TaskWorkKind, TaskWorkSnapshot, TaskWorkState, TaskWorkStopPreview,
    TaskWorkStopReceipt, TaskWorkStopStatus,
};
use luna_rpc::RpcError;

use crate::state::{AppState, EngineHandle};

pub(crate) const GET_METHOD: &str = luna_rpc::methods::GET_TASK_WORK;
pub(crate) const WATCH_METHOD: &str = luna_rpc::methods::WATCH_TASK_WORK;
pub(crate) const PREVIEW_STOP_METHOD: &str = luna_rpc::methods::PREVIEW_STOP_TASK_WORK;
pub(crate) const STOP_METHOD: &str = luna_rpc::methods::STOP_TASK_WORK;
/// One page of work rows (the host's default; its maximum is 100).
pub(crate) const PAGE: usize = 50;
/// The most rows one owned-work stop may name.
pub(crate) const MAX_STOP: usize = 32;

/// Whether the focused chat's work view is live.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkStatus {
    Loading,
    Live,
    /// The host does not report background work (older host).
    Unavailable(String),
    /// No live evidence right now (stream end, reconnect): nothing shown is
    /// current until the next frame.
    Unknown(String),
}

/// The focused chat's work, as its host last reported it.
#[derive(Debug, Clone)]
pub struct TaskWorkView {
    pub task_id: String,
    pub engine_generation: u64,
    pub status: WorkStatus,
    /// The watched first page with the full view's aggregate counts.
    pub snapshot: Option<TaskWorkSnapshot>,
    /// Further pages read with "Show more" (cleared on every new frame).
    pub more: Vec<TaskWorkItem>,
    pub more_offset: Option<usize>,
    pub loading_more: bool,
}

impl TaskWorkView {
    fn new(task_id: String, engine_generation: u64, status: WorkStatus) -> Self {
        Self {
            task_id,
            engine_generation,
            status,
            snapshot: None,
            more: Vec::new(),
            more_offset: None,
            loading_more: false,
        }
    }

    /// Rows loaded so far, in the host's order.
    pub fn items(&self) -> impl Iterator<Item = &TaskWorkItem> {
        self.snapshot
            .iter()
            .flat_map(|snapshot| snapshot.items.iter())
            .chain(self.more.iter())
    }

    pub fn loaded(&self) -> usize {
        self.items().count()
    }

    /// Whether the chip has anything to say (work, or an unknown state).
    pub fn has_work(&self) -> bool {
        matches!(self.status, WorkStatus::Unknown(_))
            || self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.total_count > 0)
    }

    /// The snapshot's aggregate counts in words (never page counts).
    pub fn summary(&self) -> String {
        match (&self.status, &self.snapshot) {
            (WorkStatus::Unknown(reason), _) => reason.clone(),
            (WorkStatus::Unavailable(reason), _) => reason.clone(),
            (WorkStatus::Loading, None) => "Reading background work…".into(),
            (_, Some(snapshot)) => {
                let mut parts = vec![format!(
                    "{} keeping this chat busy",
                    snapshot.blocking_count
                )];
                if snapshot.service_count > 0 {
                    parts.push(plural(snapshot.service_count, "service", "services"));
                }
                if snapshot.unknown_count > 0 {
                    parts.push(format!("{} unknown", snapshot.unknown_count));
                }
                parts.join(" · ")
            }
            (_, None) => "No background work".into(),
        }
    }
}

pub(crate) fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The popover/Agents grouping: lifecycle ownership, not ancestry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkGroup {
    Owned,
    Services,
    Elsewhere,
}

impl WorkGroup {
    pub fn of(item: &TaskWorkItem) -> Self {
        if item.kind == TaskWorkKind::Service {
            Self::Services
        } else if item.owned {
            Self::Owned
        } else {
            Self::Elsewhere
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Owned => "Owned by this chat",
            Self::Services => "Services",
            Self::Elsewhere => "Started here, owned elsewhere",
        }
    }

    pub fn note(self) -> Option<&'static str> {
        match self {
            Self::Owned => None,
            Self::Services => {
                Some("Services keep running on their own; use their command controls.")
            }
            Self::Elsewhere => Some("Shown for where it started; it is stopped from its own chat."),
        }
    }
}

/// Whether a row may be chosen for an owned-work stop: the host says it is
/// owned and stoppable, it is not a service, and it has not ended.
pub fn selectable(item: &TaskWorkItem) -> bool {
    item.owned && item.can_stop && item.kind != TaskWorkKind::Service && !item.state.terminal()
}

pub fn state_label(state: TaskWorkState) -> &'static str {
    match state {
        TaskWorkState::Starting => "Starting",
        TaskWorkState::Running => "Running",
        TaskWorkState::Waiting => "Waiting",
        TaskWorkState::Completed => "Completed",
        TaskWorkState::Failed => "Failed",
        TaskWorkState::Stopped => "Stopped",
        TaskWorkState::Unknown => "Unknown",
    }
}

pub fn kind_label(kind: TaskWorkKind) -> &'static str {
    match kind {
        TaskWorkKind::ChildChat => "Child chat",
        TaskWorkKind::NativeChild => "Subagent",
        TaskWorkKind::Workflow => "Workflow",
        TaskWorkKind::Shell => "Shell",
        TaskWorkKind::Service => "Service",
        TaskWorkKind::ResultDelivery => "Result delivery",
    }
}

pub fn stop_status_label(status: TaskWorkStopStatus) -> &'static str {
    match status {
        TaskWorkStopStatus::Requested => "Stop requested; not stopped yet",
        TaskWorkStopStatus::Stopped => "Stopped",
        TaskWorkStopStatus::Rejected => "Not stopped",
        TaskWorkStopStatus::Unknown => "Unknown. Not retried; refresh to check.",
    }
}

/// `{taskId}` addressed to the chat's own host.
pub(crate) fn task_params(state: &AppState, task_id: &str) -> serde_json::Value {
    let mut params = serde_json::json!({ "taskId": task_id });
    if let Some(device) = state
        .chats
        .iter()
        .find(|chat| chat.id == task_id)
        .map(|chat| chat.device_id.clone())
        .filter(|device| state.local_device_id.as_deref() != Some(device.as_str()))
    {
        params["targetDeviceId"] = device.into();
    }
    params
}

/// The owned-work stop's answer, as the popover shows it.
#[derive(Debug, Clone)]
pub enum StopAnswer {
    Receipt(TaskWorkStopReceipt),
    /// The host refused this preview (it no longer describes the work).
    Refused(String),
    /// The answer was lost: shown as unknown, never resent.
    Lost,
}

pub(crate) async fn preview_stop(
    engine: EngineHandle,
    params: serde_json::Value,
    work_ids: Vec<String>,
) -> Result<TaskWorkStopPreview, String> {
    let mut params = params;
    params["workIds"] = serde_json::json!(work_ids);
    engine
        .client()
        .call_as::<TaskWorkStopPreview>(PREVIEW_STOP_METHOD, params)
        .await
        .map_err(|error| error.to_string())
}

pub(crate) async fn stop(
    engine: EngineHandle,
    params: serde_json::Value,
    work_ids: Vec<String>,
    preview_id: String,
    request_id: String,
) -> StopAnswer {
    let mut params = params;
    params["workIds"] = serde_json::json!(work_ids);
    params["previewId"] = preview_id.into();
    params["requestId"] = request_id.into();
    match engine
        .client()
        .call_as::<TaskWorkStopReceipt>(STOP_METHOD, params)
        .await
    {
        Ok(receipt) => StopAnswer::Receipt(receipt),
        Err(RpcError::Transport(_) | RpcError::Closed) => StopAnswer::Lost,
        Err(error) => StopAnswer::Refused(error.to_string()),
    }
}

impl AppState {
    /// Keep one work watch on the focused chat (and its engine connection).
    /// A reconnect for the same chat shows "unknown" until the host reports
    /// again; a change of focus starts over.
    pub(crate) fn sync_task_work(&mut self, cx: &mut Context<Self>) {
        let focus = self.execution_focus_chat();
        let generation = self.engine_generation();
        let current = self
            .task_work
            .as_ref()
            .map(|view| (view.task_id.clone(), view.engine_generation));
        let Some(focus) = focus else {
            if self.task_work.take().is_some() {
                self.task_work_task = None;
                cx.notify();
            }
            return;
        };
        if current.as_ref() == Some(&(focus.clone(), generation)) {
            return;
        }
        let reconnect = current
            .as_ref()
            .is_some_and(|(task, previous)| *task == focus && *previous != generation);
        let status = if reconnect {
            WorkStatus::Unknown(
                "Reconnecting. Work state is unknown until the host reports again.".into(),
            )
        } else {
            WorkStatus::Loading
        };
        self.task_work = Some(TaskWorkView::new(focus.clone(), generation, status));
        self.task_work_task = None;
        self.task_work_more_task = None;
        #[cfg(feature = "appshots-fixture")]
        if self.task_work_fake.is_some() {
            self.install_fake_task_work();
            cx.notify();
            return;
        }
        let Some(engine) = self.engine().cloned() else {
            if let Some(view) = self.task_work.as_mut() {
                view.status = WorkStatus::Unknown("Engine not connected.".into());
            }
            cx.notify();
            return;
        };
        let mut params = task_params(self, &focus);
        params["limit"] = PAGE.into();
        self.task_work_task = Some(spawn_task_work_watch(cx, engine, focus, generation, params));
        cx.notify();
    }

    /// A watch frame for `task_id` on connection `generation` (refused for
    /// any other focus or connection).
    fn install_task_work(
        &mut self,
        task_id: &str,
        generation: u64,
        snapshot: TaskWorkSnapshot,
        cx: &mut Context<Self>,
    ) {
        let current_generation = self.engine_generation();
        let Some(view) = self.task_work.as_mut().filter(|view| {
            view.task_id == task_id
                && view.engine_generation == generation
                && current_generation == generation
                && snapshot.task_id == task_id
        }) else {
            return;
        };
        view.more_offset = snapshot.next_offset;
        view.snapshot = Some(snapshot);
        view.more.clear();
        view.loading_more = false;
        view.status = WorkStatus::Live;
        self.task_work_more_task = None;
        cx.notify();
    }

    fn mark_task_work(
        &mut self,
        task_id: &str,
        generation: u64,
        status: WorkStatus,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self
            .task_work
            .as_mut()
            .filter(|view| view.task_id == task_id && view.engine_generation == generation)
        {
            view.status = status;
            cx.notify();
        }
    }

    /// "Show more": the next page after the rows loaded so far.
    pub(crate) fn load_more_task_work(&mut self, cx: &mut Context<Self>) {
        let generation = self.engine_generation();
        let Some(view) = self.task_work.as_ref() else {
            return;
        };
        let Some(offset) = view.more_offset.filter(|_| !view.loading_more) else {
            return;
        };
        let task_id = view.task_id.clone();
        #[cfg(feature = "appshots-fixture")]
        if self.task_work_fake.is_some() {
            let page = self.fake_task_work_page(&task_id, offset);
            self.install_more_task_work(&task_id, generation, offset, Ok(page), cx);
            return;
        }
        let Some(engine) = self.engine().cloned() else {
            return;
        };
        let mut params = task_params(self, &task_id);
        params["offset"] = offset.into();
        params["limit"] = PAGE.into();
        if let Some(view) = self.task_work.as_mut() {
            view.loading_more = true;
        }
        self.task_work_more_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<TaskWorkSnapshot>(GET_METHOD, params)
                .await
                .map_err(|error| error.to_string());
            this.update(cx, |state, cx| {
                state.install_more_task_work(&task_id, generation, offset, result, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    fn install_more_task_work(
        &mut self,
        task_id: &str,
        generation: u64,
        offset: usize,
        result: Result<TaskWorkSnapshot, String>,
        cx: &mut Context<Self>,
    ) {
        let current_generation = self.engine_generation();
        let Some(view) = self.task_work.as_mut().filter(|view| {
            view.task_id == task_id
                && view.engine_generation == generation
                && current_generation == generation
                && view.more_offset == Some(offset)
        }) else {
            return;
        };
        view.loading_more = false;
        // An error or a page for another task appends nothing; the cursor
        // stays so Show more can be tried again.
        if let Ok(page) = result
            && page.task_id == task_id
        {
            let known: std::collections::HashSet<String> =
                view.items().map(|item| item.id.clone()).collect();
            view.more.extend(
                page.items
                    .into_iter()
                    .filter(|item| !known.contains(&item.id)),
            );
            view.more_offset = page.next_offset;
        }
        cx.notify();
    }
}

fn spawn_task_work_watch(
    cx: &mut Context<AppState>,
    engine: EngineHandle,
    task_id: String,
    generation: u64,
    params: serde_json::Value,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        loop {
            let mut rx = match engine
                .client()
                .subscribe_checked(WATCH_METHOD, params.clone())
                .await
            {
                Ok(rx) => rx,
                Err(RpcError::UnknownMethod(_)) => {
                    this.update(cx, |state, cx| {
                        state.mark_task_work(
                            &task_id,
                            generation,
                            WorkStatus::Unavailable(
                                "This chat's host doesn't report background work.".into(),
                            ),
                            cx,
                        )
                    })
                    .ok();
                    return;
                }
                Err(error) => {
                    let alive = this.update(cx, |state, cx| {
                        state.mark_task_work(
                            &task_id,
                            generation,
                            WorkStatus::Unknown(format!(
                                "Work state is unknown: {error}. Retrying."
                            )),
                            cx,
                        )
                    });
                    if alive.is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue;
                }
            };
            while let Some(value) = rx.recv().await {
                let Ok(snapshot) = serde_json::from_value::<TaskWorkSnapshot>(value) else {
                    tracing::warn!(%task_id, "dropping malformed task work frame");
                    continue;
                };
                if this
                    .update(cx, |state, cx| {
                        state.install_task_work(&task_id, generation, snapshot, cx)
                    })
                    .is_err()
                {
                    return;
                }
            }
            // The stream ended: nothing shown is current until it resumes.
            let alive = this.update(cx, |state, cx| {
                state.mark_task_work(
                    &task_id,
                    generation,
                    WorkStatus::Unknown(
                        "The host stopped reporting work. Work state is unknown until it reconnects."
                            .into(),
                    ),
                    cx,
                )
            });
            if alive.is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

/// QA: a scripted host for work views and stops (the engine seam stands in
/// for the real projection where it is not yet wired).
#[cfg(feature = "appshots-fixture")]
#[derive(Debug, Clone, Default)]
pub struct TaskWorkFake {
    pub items: std::collections::HashMap<String, Vec<TaskWorkItem>>,
    /// The next stop's answer per work id (`None`: the answer is lost).
    pub stop_statuses: Option<std::collections::HashMap<String, TaskWorkStopStatus>>,
    /// Refuse the next stop as a stale preview.
    pub refuse_next_stop: bool,
    pub preview_calls: u32,
    pub stop_calls: u32,
    pub stop_request_ids: Vec<String>,
}

#[cfg(feature = "appshots-fixture")]
impl AppState {
    pub fn fixture_set_task_work_fake(
        &mut self,
        fake: Option<TaskWorkFake>,
        cx: &mut Context<Self>,
    ) {
        self.task_work_fake = fake;
        // Re-read through the fake (or the real watch) for the focused chat.
        self.task_work = None;
        self.sync_task_work(cx);
    }

    pub fn fixture_task_work_fake(&self) -> Option<&TaskWorkFake> {
        self.task_work_fake.as_ref()
    }

    /// Replace a task's scripted rows and deliver a frame as the watch would.
    pub fn fixture_set_fake_work(
        &mut self,
        task_id: &str,
        items: Vec<TaskWorkItem>,
        cx: &mut Context<Self>,
    ) {
        if let Some(fake) = self.task_work_fake.as_mut() {
            fake.items.insert(task_id.to_string(), items);
        }
        if self
            .task_work
            .as_ref()
            .is_some_and(|view| view.task_id == task_id)
        {
            self.install_fake_task_work();
            cx.notify();
        }
    }

    pub fn fixture_set_fake_stop(
        &mut self,
        statuses: Option<std::collections::HashMap<String, TaskWorkStopStatus>>,
        refuse: bool,
    ) {
        if let Some(fake) = self.task_work_fake.as_mut() {
            fake.stop_statuses = statuses;
            fake.refuse_next_stop = refuse;
        }
    }

    /// End the watch as a dropped stream would.
    pub fn fixture_end_task_work_watch(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.task_work.clone() {
            self.task_work_task = None;
            self.mark_task_work(
                &view.task_id,
                view.engine_generation,
                WorkStatus::Unknown(
                    "The host stopped reporting work. Work state is unknown until it reconnects."
                        .into(),
                ),
                cx,
            );
        }
    }

    fn fake_snapshot(&self, task_id: &str, offset: usize) -> TaskWorkSnapshot {
        let all = self
            .task_work_fake
            .as_ref()
            .and_then(|fake| fake.items.get(task_id))
            .cloned()
            .unwrap_or_default();
        let live = |item: &&TaskWorkItem| !item.state.terminal();
        let page: Vec<TaskWorkItem> = all.iter().skip(offset).take(PAGE).cloned().collect();
        let next = (offset + page.len() < all.len()).then_some(offset + page.len());
        TaskWorkSnapshot {
            schema_version: 1,
            task_id: task_id.to_string(),
            host_device_id: self.local_device_id.clone().unwrap_or_default(),
            blocking_count: all
                .iter()
                .filter(live)
                .filter(|item| item.blocks_completion)
                .count(),
            service_count: all
                .iter()
                .filter(live)
                .filter(|item| item.kind == TaskWorkKind::Service)
                .count(),
            unknown_count: all
                .iter()
                .filter(|item| item.state == TaskWorkState::Unknown)
                .count(),
            total_count: all.len(),
            next_offset: next,
            truncated: false,
            items: page,
        }
    }

    fn install_fake_task_work(&mut self) {
        let Some(view) = self.task_work.as_ref() else {
            return;
        };
        let snapshot = self.fake_snapshot(&view.task_id.clone(), 0);
        if let Some(view) = self.task_work.as_mut() {
            view.more_offset = snapshot.next_offset;
            view.snapshot = Some(snapshot);
            view.more.clear();
            view.loading_more = false;
            view.status = WorkStatus::Live;
        }
    }

    fn fake_task_work_page(&self, task_id: &str, offset: usize) -> TaskWorkSnapshot {
        self.fake_snapshot(task_id, offset)
    }

    /// The scripted host's stop preview: exactly the requested selectable
    /// rows, the rest excluded.
    pub(crate) fn fake_preview_stop(
        &mut self,
        task_id: &str,
        work_ids: &[String],
    ) -> TaskWorkStopPreview {
        let all = self
            .task_work_fake
            .as_ref()
            .and_then(|fake| fake.items.get(task_id))
            .cloned()
            .unwrap_or_default();
        let fake = self.task_work_fake.get_or_insert_with(Default::default);
        fake.preview_calls += 1;
        let (items, excluded) = all
            .into_iter()
            .filter(|item| work_ids.contains(&item.id))
            .partition(selectable);
        TaskWorkStopPreview {
            task_id: task_id.to_string(),
            preview_id: format!("fake-preview-{}", fake.preview_calls),
            items,
            excluded,
        }
    }

    pub(crate) fn fake_stop(
        &mut self,
        task_id: &str,
        work_ids: &[String],
        preview_id: &str,
        request_id: &str,
    ) -> StopAnswer {
        let fake = self.task_work_fake.get_or_insert_with(Default::default);
        fake.stop_calls += 1;
        fake.stop_request_ids.push(request_id.to_string());
        if std::mem::take(&mut fake.refuse_next_stop) {
            return StopAnswer::Refused(
                "This stop preview no longer matches the work. Review it again.".into(),
            );
        }
        match &fake.stop_statuses {
            None => StopAnswer::Lost,
            Some(statuses) => StopAnswer::Receipt(TaskWorkStopReceipt {
                request_id: request_id.to_string(),
                task_id: task_id.to_string(),
                preview_id: preview_id.to_string(),
                results: work_ids
                    .iter()
                    .map(|id| luna_proto::TaskWorkStopResult {
                        work_id: id.clone(),
                        status: statuses
                            .get(id)
                            .copied()
                            .unwrap_or(TaskWorkStopStatus::Requested),
                        detail: String::new(),
                    })
                    .collect(),
            }),
        }
    }
}
