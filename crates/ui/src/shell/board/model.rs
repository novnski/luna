//! The Board data layer: an entity that loads tickets for one scope through
//! [`BoardApi`], follows `BoardWatch` and re-reads after every journal event,
//! observation change or reconnect. Views render what the engine derived;
//! nothing here computes phase, attention or authority.
//!
//! Each column pages on its own (`BoardList` with that column and the
//! engine's cursor), so a long Backlog never hides Ready or Done. A column
//! loads more on request, up to [`MAX_COLUMN_PAGES`]; every re-read covers
//! the pages already loaded, and a scope or filter change starts again from
//! each column's first page.
use std::cmp::Ordering;
use std::collections::HashMap;
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
/// Tickets per read: the engine's largest page.
const PAGE_LIMIT: u32 = luna_proto::BOARD_LIST_MAX_LIMIT;
/// Pages one column keeps loaded at most. 5,000 tickets bounds memory, the
/// re-read after every change and the (unvirtualized) column render; past
/// it, search or filters narrow the column.
pub const MAX_COLUMN_PAGES: usize = 10;

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

/// One column's paging, for the views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnPaging {
    /// Tickets loaded in the column.
    pub loaded: usize,
    /// The engine's count for the column, when it is exact (no search or
    /// filter narrows it).
    pub total: Option<u64>,
    /// The engine has more tickets past the loaded pages.
    pub has_more: bool,
    /// Pages loaded.
    pub pages: usize,
    /// [`MAX_COLUMN_PAGES`] reached: no further Load more.
    pub at_limit: bool,
    /// A Load more for this column is in flight.
    pub loading: bool,
}

impl ColumnPaging {
    /// The column header's count: "500 of 528", "500+" or "28".
    pub fn count_text(&self) -> String {
        match (self.has_more, self.total) {
            (false, _) => self.loaded.to_string(),
            (true, Some(total)) => {
                format!("{} of {}", self.loaded, total.max(self.loaded as u64))
            }
            (true, None) => format!("{}+", self.loaded),
        }
    }
}

/// Where a column's loaded pages end.
#[derive(Debug, Clone, Default)]
struct ColumnPages {
    /// Pages to keep loaded (1..=MAX_COLUMN_PAGES).
    wanted: usize,
    /// Pages the last read covered.
    pages: usize,
    /// Past the loaded pages (`None`: the column is complete).
    next_cursor: Option<String>,
    /// Load more is in flight.
    loading: bool,
}

impl ColumnPages {
    fn first() -> Self {
        Self {
            wanted: 1,
            ..Self::default()
        }
    }
}

/// One column's read: `pages` engine pages from a cursor.
struct ColumnRead {
    column: BoardColumn,
    items: Vec<WorkItemView>,
    pages: usize,
    next_cursor: Option<String>,
    projects: Vec<BoardProjectSummary>,
    as_of_seq: u64,
}

/// What a load covers.
enum Load {
    /// Every column from its first page, the pages it keeps.
    All(Vec<ColumnRead>),
    /// One more page of one column.
    More(ColumnRead),
}

/// The engine's list order: project, column, rank, id.
fn board_order(left: &WorkItemView, right: &WorkItemView) -> Ordering {
    left.item
        .project_id
        .cmp(&right.item.project_id)
        .then(left.item.column.order().cmp(&right.item.column.order()))
        .then_with(|| luna_proto::compare_pin_order(&left.item.rank, &right.item.rank))
        .then_with(|| left.item.id.cmp(&right.item.id))
}

fn slot(column: BoardColumn) -> usize {
    match column {
        BoardColumn::Backlog => 0,
        BoardColumn::Ready => 1,
        BoardColumn::Done => 2,
    }
}

/// One ticket per id (the newest revision; a ticket read twice across a
/// concurrent move keeps its later state), in board order.
fn dedupe(items: Vec<WorkItemView>) -> Vec<WorkItemView> {
    let mut index: HashMap<String, usize> = HashMap::with_capacity(items.len());
    let mut unique: Vec<WorkItemView> = Vec::with_capacity(items.len());
    for view in items {
        match index.get(&view.item.id) {
            Some(&at) => {
                if unique[at].item.revision <= view.item.revision {
                    unique[at] = view;
                }
            }
            None => {
                index.insert(view.item.id.clone(), unique.len());
                unique.push(view);
            }
        }
    }
    unique.sort_by(board_order);
    unique
}

/// Read `pages` pages of one column, following the engine's cursor.
async fn read_column(
    api: &BoardApi,
    base: &BoardListRequest,
    column: BoardColumn,
    cursor: Option<String>,
    pages: usize,
) -> Result<ColumnRead, BoardCallError> {
    let mut read = ColumnRead {
        column,
        items: Vec::new(),
        pages: 0,
        next_cursor: cursor,
        projects: Vec::new(),
        as_of_seq: u64::MAX,
    };
    loop {
        let mut request = base.clone();
        request.columns = vec![column];
        request.cursor = read.next_cursor.take();
        request.limit = Some(PAGE_LIMIT);
        let page = api.list(request).await?;
        read.pages += 1;
        read.items.extend(page.items);
        read.projects = page.projects;
        read.as_of_seq = read.as_of_seq.min(page.as_of_seq);
        read.next_cursor = page.next_cursor;
        if read.pages >= pages || read.next_cursor.is_none() {
            return Ok(read);
        }
    }
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
    /// Per column (Backlog, Ready, Done).
    pages: [ColumnPages; 3],
    /// Bumped by every scope or filter change: a read for an older query
    /// never lands.
    query: u64,
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
            pages: std::array::from_fn(|_| ColumnPages::first()),
            query: 0,
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

    /// Some column has more tickets past its loaded pages.
    pub fn has_more(&self) -> bool {
        self.pages.iter().any(|pages| pages.next_cursor.is_some())
    }

    /// The engine's counts are exact for what the board lists: no search or
    /// filter narrows it.
    fn exact_totals(&self) -> bool {
        self.filters.text.trim().is_empty()
            && !self.filters.attention_only
            && self.filters.provider.is_none()
            && self.filters.source.is_none()
            && self.filters.archived == BoardArchivedFilter::Exclude
    }

    /// The engine's count for one column of this scope, when exact.
    pub fn total(&self, column: BoardColumn) -> Option<u64> {
        self.exact_totals().then(|| {
            self.projects
                .iter()
                .filter(|summary| match &self.scope {
                    BoardScope::Project { project_id } => &summary.project_id == project_id,
                    _ => true,
                })
                .map(|summary| match column {
                    BoardColumn::Backlog => summary.backlog,
                    BoardColumn::Ready => summary.ready,
                    BoardColumn::Done => summary.done,
                })
                .sum()
        })
    }

    /// The engine's count for the whole board, when exact.
    pub fn board_total(&self) -> Option<u64> {
        BoardColumn::ALL
            .into_iter()
            .map(|column| self.total(column))
            .sum()
    }

    /// One column's paging.
    pub fn paging(&self, column: BoardColumn) -> ColumnPaging {
        let pages = &self.pages[slot(column)];
        ColumnPaging {
            loaded: self.column(column).count(),
            total: self.total(column),
            has_more: pages.next_cursor.is_some(),
            pages: pages.pages,
            at_limit: pages.next_cursor.is_some() && pages.pages >= MAX_COLUMN_PAGES,
            loading: pages.loading,
        }
    }

    /// Load one more page of `column` (bounded by [`MAX_COLUMN_PAGES`]).
    /// Returns whether a read was asked for.
    pub fn load_more(&mut self, column: BoardColumn, cx: &mut Context<Self>) -> bool {
        let at = slot(column);
        let Some(cursor) = self.pages[at].next_cursor.clone() else {
            return false;
        };
        if self.pages[at].pages >= MAX_COLUMN_PAGES || self.pages[at].loading {
            return false;
        }
        self.pages[at].loading = true;
        self.pages[at].wanted = (self.pages[at].pages + 1).min(MAX_COLUMN_PAGES);
        cx.emit(BoardModelEvent::Changed);
        cx.notify();
        if self.load_task.is_some() {
            // A read is in flight: the next one covers the extra page.
            self.dirty = true;
            return true;
        }
        let Some(api) = self.api(cx) else {
            self.pages[at].loading = false;
            return false;
        };
        let generation = self.engine_generation;
        let query = self.query;
        let base = self.list_request();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = read_column(&api, &base, column, Some(cursor), 1)
                .await
                .map(Load::More);
            this.update(cx, |model, cx| model.install(generation, query, result, cx))
                .ok();
        }));
        true
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
        if self.status == BoardStatus::Live {
            self.status = BoardStatus::Loading;
        }
        self.restart(cx);
    }

    pub fn set_filters(&mut self, filters: BoardListFilters, cx: &mut Context<Self>) {
        if filters == self.filters {
            return;
        }
        self.filters = filters;
        // A new query: drop a read for the old one and start again from
        // each column's first page (the watch stays: it follows the scope).
        self.new_query();
        self.load_task = None;
        self.dirty = false;
        self.refresh(cx);
    }

    fn new_query(&mut self) {
        self.query += 1;
        self.pages = std::array::from_fn(|_| ColumnPages::first());
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
            self.items.sort_by(board_order);
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
        self.new_query();
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
            limit: Some(PAGE_LIMIT),
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
        // Every column from its first page, as many pages as it keeps.
        let base = self.list_request();
        let query = self.query;
        let plan: Vec<(BoardColumn, usize)> = BoardColumn::ALL
            .into_iter()
            .map(|column| (column, self.pages[slot(column)].wanted.max(1)))
            .collect();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let reads = plan
                .into_iter()
                .map(|(column, pages)| read_column(&api, &base, column, None, pages));
            let result = futures::future::try_join_all(reads).await.map(Load::All);
            this.update(cx, |model, cx| model.install(generation, query, result, cx))
                .ok();
        }));
    }

    fn install(
        &mut self,
        generation: u64,
        query: u64,
        result: Result<Load, BoardCallError>,
        cx: &mut Context<Self>,
    ) {
        if query != self.query {
            // A read for an older scope or filter (its task was dropped;
            // this never replaces the current read).
            return;
        }
        self.load_task = None;
        if !self.dirty {
            // A Load more asked for during this read stays pending until
            // the read that covers it.
            for pages in &mut self.pages {
                pages.loading = false;
            }
        }
        if generation != self.engine_generation {
            self.refresh(cx);
            return;
        }
        match result {
            Ok(Load::All(reads)) => {
                let mut items = Vec::new();
                let mut as_of_seq = u64::MAX;
                for read in reads {
                    let pages = &mut self.pages[slot(read.column)];
                    pages.pages = read.pages;
                    if read.next_cursor.is_none() {
                        // The column ends here: Load more starts from it.
                        pages.wanted = read.pages.max(1);
                    }
                    pages.next_cursor = read.next_cursor;
                    as_of_seq = as_of_seq.min(read.as_of_seq);
                    self.projects = read.projects;
                    items.extend(read.items);
                }
                self.items = dedupe(items);
                // The earliest page's head: a change after it re-reads.
                self.as_of_seq = as_of_seq;
                self.status = BoardStatus::Live;
                if self.watching != Some(generation) {
                    self.start_watch(generation, as_of_seq, cx);
                }
            }
            Ok(Load::More(read)) => {
                let pages = &mut self.pages[slot(read.column)];
                pages.pages += read.pages;
                pages.wanted = pages.pages;
                pages.next_cursor = read.next_cursor;
                self.projects = read.projects;
                let mut items = std::mem::take(&mut self.items);
                items.extend(read.items);
                self.items = dedupe(items);
                // A change between the reads arrives as a watch frame after
                // `as_of_seq` and re-reads every page.
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
