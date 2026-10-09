//! The Board data layer: an entity that loads tickets for one scope through
//! [`BoardApi`], follows `BoardWatch` and re-reads after every journal event,
//! observation change or reconnect. Views render what the engine derived;
//! nothing here computes phase, attention or authority.
use std::time::Duration;

use gpui::{App, Context, Entity, EventEmitter, Subscription, Task};
use luna_proto::{
    BoardArchivedFilter, BoardColumn, BoardListRequest, BoardMutationReceipt, BoardProjectSummary,
    BoardScope, BoardSourceKind, BoardWatchFrame, BoardWatchRequest, HarnessId, WorkItemEvent,
    WorkItemView,
};

use super::api::{BoardApi, BoardCallError};
use crate::state::AppState;

/// A watch that ended (host restart, transport error) is retried this often.
const WATCH_RETRY: Duration = Duration::from_secs(2);

/// List filters applied by the engine. Presentation only.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BoardListFilters {
    pub text: String,
    pub attention_only: bool,
    pub provider: Option<HarnessId>,
    pub source: Option<BoardSourceKind>,
    pub archived: BoardArchivedFilter,
}

/// Whether what the model shows is current.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardStatus {
    Loading,
    Live,
    /// No engine connection: nothing shown is current.
    Disconnected(String),
    /// The host does not serve the board (older engine).
    Unsupported,
    Error(String),
}

/// What the model tells its views.
#[derive(Debug, Clone)]
pub enum BoardModelEvent {
    /// Items, projects or status changed.
    Changed,
    /// New journal events (for activity/badge consumers).
    Events(Vec<WorkItemEvent>),
}

pub struct BoardModel {
    state: Entity<AppState>,
    scope: BoardScope,
    filters: BoardListFilters,
    items: Vec<WorkItemView>,
    projects: Vec<BoardProjectSummary>,
    next_cursor: Option<String>,
    as_of_seq: u64,
    status: BoardStatus,
    engine_generation: u64,
    /// The generation the live watch belongs to (`None`: no watch).
    watching: Option<u64>,
    /// A change arrived while a load was in flight: load again after it.
    dirty: bool,
    load_task: Option<Task<()>>,
    watch_task: Option<Task<()>>,
    _state: Subscription,
}

impl EventEmitter<BoardModelEvent> for BoardModel {}

impl BoardModel {
    pub fn new(state: Entity<AppState>, scope: BoardScope, cx: &mut Context<Self>) -> Self {
        Self::with_filters(state, scope, BoardListFilters::default(), cx)
    }

    /// [`Self::new`] with initial filters (restored presentation), so the
    /// first read already applies them.
    pub fn with_filters(
        state: Entity<AppState>,
        scope: BoardScope,
        filters: BoardListFilters,
        cx: &mut Context<Self>,
    ) -> Self {
        let observer = cx.observe(&state, |model, state, cx| {
            if state.read(cx).engine_generation() != model.engine_generation {
                model.refresh(cx);
            }
        });
        let mut model = Self {
            state,
            scope,
            filters,
            items: Vec::new(),
            projects: Vec::new(),
            next_cursor: None,
            as_of_seq: 0,
            status: BoardStatus::Loading,
            engine_generation: u64::MAX,
            watching: None,
            dirty: false,
            load_task: None,
            watch_task: None,
            _state: observer,
        };
        model.refresh(cx);
        model
    }

    /// The typed API on the current engine connection.
    pub fn api(&self, cx: &App) -> Option<BoardApi> {
        self.state.read(cx).engine().cloned().map(BoardApi::new)
    }

    pub fn scope(&self) -> &BoardScope {
        &self.scope
    }

    /// The app state this model reads (project names, host identity).
    pub fn state(&self) -> &Entity<AppState> {
        &self.state
    }

    pub fn filters(&self) -> &BoardListFilters {
        &self.filters
    }

    pub fn status(&self) -> &BoardStatus {
        &self.status
    }

    /// Every loaded ticket, ordered by project, column, rank.
    pub fn items(&self) -> &[WorkItemView] {
        &self.items
    }

    /// One column's tickets in board order.
    pub fn column(&self, column: BoardColumn) -> impl Iterator<Item = &WorkItemView> {
        self.items
            .iter()
            .filter(move |view| view.item.column == column)
    }

    pub fn item(&self, work_item_id: &str) -> Option<&WorkItemView> {
        self.items.iter().find(|view| view.item.id == work_item_id)
    }

    pub fn projects(&self) -> &[BoardProjectSummary] {
        &self.projects
    }

    /// More tickets exist past the loaded page.
    pub fn has_more(&self) -> bool {
        self.next_cursor.is_some()
    }

    /// Journal head of the loaded state.
    pub fn as_of_seq(&self) -> u64 {
        self.as_of_seq
    }

    /// Switch scope (project / All projects): reload and re-watch.
    pub fn set_scope(&mut self, scope: BoardScope, cx: &mut Context<Self>) {
        if scope == self.scope {
            return;
        }
        self.scope = scope;
        self.items.clear();
        self.projects.clear();
        self.restart(cx);
    }

    pub fn set_filters(&mut self, filters: BoardListFilters, cx: &mut Context<Self>) {
        if filters == self.filters {
            return;
        }
        self.filters = filters;
        self.refresh(cx);
    }

    /// Show a committed mutation immediately (its record is canonical), then
    /// re-read the engine's derived view.
    pub fn apply_receipt(&mut self, receipt: &BoardMutationReceipt, cx: &mut Context<Self>) {
        if let Some(view) = self
            .items
            .iter_mut()
            .find(|view| view.item.id == receipt.work_item_id)
            && view.item.revision <= receipt.item.revision
        {
            view.item = receipt.item.clone();
            cx.emit(BoardModelEvent::Changed);
            cx.notify();
        }
        self.refresh(cx);
    }

    /// Show a committed move at once: its record (column, rank) and the
    /// phase the engine reported with it, in board order. The engine's
    /// derived view replaces it on the following re-read.
    pub fn apply_move(&mut self, receipt: &luna_proto::BoardMoveReceipt, cx: &mut Context<Self>) {
        self.apply_committed(&receipt.mutation.item, Some(receipt.phase), cx);
    }

    /// [`Self::apply_receipt`] that also restores board order, so a rank or
    /// column change shows in its new place before the re-read.
    pub fn apply_committed(
        &mut self,
        item: &luna_proto::WorkItem,
        phase: Option<luna_proto::BoardPhase>,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.items.iter_mut().find(|view| view.item.id == item.id)
            && view.item.revision <= item.revision
        {
            view.item = item.clone();
            if let Some(phase) = phase {
                view.phase = phase;
            }
            self.items.sort_by(|left, right| {
                left.item
                    .project_id
                    .cmp(&right.item.project_id)
                    .then(left.item.column.order().cmp(&right.item.column.order()))
                    .then_with(|| luna_proto::compare_pin_order(&left.item.rank, &right.item.rank))
                    .then_with(|| left.item.id.cmp(&right.item.id))
            });
            cx.emit(BoardModelEvent::Changed);
            cx.notify();
        }
        self.refresh(cx);
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        self.watch_task = None;
        self.watching = None;
        self.load_task = None;
        self.dirty = false;
        self.refresh(cx);
    }

    fn list_request(&self) -> BoardListRequest {
        BoardListRequest {
            scope: self.scope.clone(),
            columns: Vec::new(),
            archived: self.filters.archived,
            text: (!self.filters.text.trim().is_empty()).then(|| self.filters.text.clone()),
            attention_only: self.filters.attention_only,
            provider: self.filters.provider,
            source: self.filters.source,
            cursor: None,
            limit: Some(luna_proto::BOARD_LIST_MAX_LIMIT),
        }
    }

    /// Re-read the current scope (coalesced while a read is in flight).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let generation = self.state.read(cx).engine_generation();
        if generation != self.engine_generation {
            // A reconnect: nothing shown is current until the host answers.
            self.engine_generation = generation;
            self.watch_task = None;
            self.watching = None;
            self.load_task = None;
            if !self.items.is_empty() {
                self.status = BoardStatus::Loading;
            }
        }
        if self.load_task.is_some() {
            self.dirty = true;
            return;
        }
        let Some(api) = self.api(cx) else {
            self.status = BoardStatus::Disconnected("Engine not connected.".into());
            cx.emit(BoardModelEvent::Changed);
            cx.notify();
            return;
        };
        let request = self.list_request();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = api.list(request).await;
            this.update(cx, |model, cx| model.install(generation, result, cx))
                .ok();
        }));
    }

    fn install(
        &mut self,
        generation: u64,
        result: Result<luna_proto::BoardListPage, BoardCallError>,
        cx: &mut Context<Self>,
    ) {
        self.load_task = None;
        if generation != self.engine_generation {
            self.refresh(cx);
            return;
        }
        match result {
            Ok(page) => {
                self.items = page.items;
                self.projects = page.projects;
                self.next_cursor = page.next_cursor;
                self.as_of_seq = page.as_of_seq;
                self.status = BoardStatus::Live;
                if self.watching != Some(generation) {
                    self.start_watch(generation, page.as_of_seq, cx);
                }
            }
            Err(BoardCallError::Unsupported) => self.status = BoardStatus::Unsupported,
            Err(error) => self.status = BoardStatus::Error(error.message()),
        }
        cx.emit(BoardModelEvent::Changed);
        cx.notify();
        if std::mem::take(&mut self.dirty) {
            self.refresh(cx);
        }
    }

    fn start_watch(&mut self, generation: u64, after_seq: u64, cx: &mut Context<Self>) {
        let Some(api) = self.api(cx) else {
            return;
        };
        let request = BoardWatchRequest {
            scope: self.scope.clone(),
            after_seq: Some(after_seq),
        };
        self.watching = Some(generation);
        self.watch_task = Some(cx.spawn(async move |this, cx| {
            match api.watch(request).await {
                Ok(mut frames) => {
                    while let Some(value) = frames.recv().await {
                        let Ok(frame) = serde_json::from_value::<BoardWatchFrame>(value) else {
                            tracing::warn!("dropping malformed board watch frame");
                            continue;
                        };
                        if this
                            .update(cx, |model, cx| model.on_frame(generation, frame, cx))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(BoardCallError::Unsupported) => return,
                Err(_) => {}
            }
            // The stream ended: re-read and re-watch from the current head.
            cx.background_executor().timer(WATCH_RETRY).await;
            this.update(cx, |model, cx| {
                if model.watching == Some(generation) {
                    model.watching = None;
                    model.refresh(cx);
                }
            })
            .ok();
        }));
    }

    fn on_frame(&mut self, generation: u64, frame: BoardWatchFrame, cx: &mut Context<Self>) {
        if generation != self.engine_generation || self.watching != Some(generation) {
            return;
        }
        let changed = frame.reset || !frame.events.is_empty() || !frame.observed.is_empty();
        if !frame.events.is_empty() {
            cx.emit(BoardModelEvent::Events(frame.events));
        }
        if changed && frame.seq >= self.as_of_seq {
            self.refresh(cx);
        }
    }
}
