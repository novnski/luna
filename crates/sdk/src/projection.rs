//! RM-03 client runtime state: the task projections one paired host serves,
//! held for exactly that host, applied strictly in order and kept, labelled
//! by freshness, through disconnects and restarts.
//!
//! The host is authoritative; a [`ProjectionStore`] only records what the
//! host last confirmed. It is shared by every client: the mobile core feeds
//! it from its reconnecting socket, and a desktop or CLI client can feed it
//! from any [`crate::Client`]. Its rules:
//!
//! - **One host.** A store is keyed by [`HostKey`] (device, environment and
//!   workspace scope). A projection or a saved cache naming any other host is
//!   refused, so data never crosses hosts or profiles.
//! - **One connection generation.** The owner calls
//!   [`ProjectionStore::begin_epoch`] for each verified socket and
//!   [`ProjectionStore::end_epoch`] when it ends. An update tagged with any
//!   other epoch (a late item from an old socket) is refused as
//!   [`ProjectionError::StaleGeneration`].
//! - **In order.** A `current` must name the held cursor, and a delta must
//!   apply to it (`base`) within the same connection, after that
//!   connection's snapshot or `current`. Anything else is
//!   [`ProjectionError::OutOfOrder`]: resubscribe without a cursor for a
//!   snapshot.
//! - **Immutable views.** Every applied update publishes a new `Arc` view; a
//!   reader's view never changes under it.
//! - **Honest freshness.** A view is [`Freshness::Live`] only while the
//!   connection that confirmed it is current, [`Freshness::Stale`] after that
//!   connection ended, and [`Freshness::Cached`] when it was restored from
//!   this device's saved cache and not yet confirmed again.
//! - **Bounded.** One registry within the host's bounds, at most
//!   [`MAX_CACHED_TASKS`] task details, and a saved cache of at most
//!   [`MAX_CACHE_BYTES`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::marker::PhantomData;
use std::sync::Arc;

use luna_proto::WorkspaceScope;
use luna_proto::remote_projection::{
    MAX_ENTRY_TEXT_BYTES, MAX_HISTORY_ENTRIES, MAX_ID_BYTES, MAX_REGISTRY_PROJECTS,
    MAX_REGISTRY_TASKS, ProjectionCursor, ProjectionHost, ProviderReadiness, RemoteHistoryEntry,
    RemoteProject, RemoteTaskItem, RemoteTaskRow, RemoteTasksItem,
};
use luna_rpc::RpcSubscription;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::SdkError;
use crate::client::decode;

/// Most task details one store keeps (least recently updated dropped first).
pub const MAX_CACHED_TASKS: usize = 8;
/// Largest saved cache a store writes or reads.
pub const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;
const CACHE_VERSION: u16 = 1;

/// One projection watch's typed items. Dropping it cancels the engine's
/// stream. It decodes only; ordering is the [`ProjectionStore`]'s to judge.
pub struct ProjectionStream<I> {
    operation: &'static str,
    subscription: RpcSubscription,
    _item: PhantomData<fn() -> I>,
}

impl<I: DeserializeOwned> ProjectionStream<I> {
    pub(crate) fn new(operation: &'static str, subscription: RpcSubscription) -> Self {
        Self {
            operation,
            subscription,
            _item: PhantomData,
        }
    }

    /// The next item; `None` once the engine ends the stream or the
    /// connection closes.
    pub async fn next(&mut self) -> Option<Result<I, SdkError>> {
        let value = self.subscription.recv().await?;
        Some(decode::<I>(self.operation, value))
    }
}

/// The host a store belongs to: the identity a phone paired with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKey {
    pub device_id: String,
    pub environment_id: String,
    pub workspace_scope: WorkspaceScope,
}

impl HostKey {
    /// Whether a projection describes this host.
    pub fn accepts(&self, host: &ProjectionHost) -> bool {
        host.device_id == self.device_id
            && host.workspace_scope == self.workspace_scope
            && host.environment_id.as_deref() == Some(self.environment_id.as_str())
    }
}

/// How much a view can be trusted right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Freshness {
    /// Confirmed on the current connection, which is following its changes.
    Live,
    /// Confirmed earlier; the connection that followed it has ended.
    Stale,
    /// Restored from this device's saved cache and not confirmed since.
    #[default]
    Cached,
}

/// What last changed a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectionUpdate {
    /// The host resent the whole projection (first follow or repair).
    Snapshot,
    /// The host confirmed the held cursor; nothing was resent.
    Current,
    Delta,
    Removed,
    /// Restored from the saved cache.
    #[default]
    Restored,
}

/// The registry as last confirmed by the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryView {
    pub host: ProjectionHost,
    pub cursor: ProjectionCursor,
    /// The host's clock when it computed this state.
    pub observed_at_ms: i64,
    pub projects: Vec<RemoteProject>,
    pub tasks: Vec<RemoteTaskRow>,
    pub total_tasks: u32,
    pub providers: Vec<ProviderReadiness>,
    /// This device's clock when the host last confirmed the view.
    pub confirmed_at_ms: i64,
    #[serde(skip)]
    pub freshness: Freshness,
    #[serde(skip)]
    pub last_update: ProjectionUpdate,
}

/// One task as last confirmed by the host; `task` is `None` once the host
/// reported it removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub task_id: String,
    pub host: ProjectionHost,
    /// Absent for a removed task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ProjectionCursor>,
    pub observed_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<RemoteTaskRow>,
    pub task_revision: u64,
    pub task_sequence: u64,
    pub entries: Vec<RemoteHistoryEntry>,
    pub total_entries: u32,
    pub confirmed_at_ms: i64,
    #[serde(skip)]
    pub freshness: Freshness,
    #[serde(skip)]
    pub last_update: ProjectionUpdate,
}

/// Why an update was not applied. Nothing changes when one is returned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectionError {
    /// The update came from a connection that is no longer current.
    #[error("an update from a previous connection was ignored")]
    StaleGeneration,
    /// The projection describes another host; never applied.
    #[error("{0}")]
    WrongHost(String),
    /// It does not follow the held cursor: resubscribe for a snapshot.
    #[error("{0}")]
    OutOfOrder(String),
    /// Malformed or outside the projection bounds.
    #[error("{0}")]
    Invalid(String),
}

/// Why a saved cache was not restored. The store then starts empty.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CacheError {
    #[error("the saved cache is larger than its 4 MiB bound")]
    TooLarge,
    #[error("the saved cache is unreadable: {0}")]
    Unreadable(String),
    #[error("the saved cache belongs to another host or profile")]
    OtherHost,
    #[error("the saved cache is invalid: {0}")]
    Invalid(String),
}

struct Held<V> {
    view: Arc<V>,
    /// The connection that last confirmed it; deltas need this one.
    epoch: Option<u64>,
}

/// One paired host's projections. See the module documentation.
pub struct ProjectionStore {
    key: HostKey,
    epoch: Option<u64>,
    registry: Option<Held<RegistryView>>,
    /// Least recently updated first.
    tasks: VecDeque<Held<TaskView>>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedCache {
    version: u16,
    key: HostKey,
    saved_at_ms: i64,
    #[serde(default)]
    registry: Option<RegistryView>,
    #[serde(default)]
    tasks: Vec<TaskView>,
}

impl ProjectionStore {
    pub fn new(key: HostKey) -> Self {
        Self {
            key,
            epoch: None,
            registry: None,
            tasks: VecDeque::new(),
        }
    }

    pub fn key(&self) -> &HostKey {
        &self.key
    }

    /// A verified connection starts: accept its updates only. Held views
    /// stay as they are until its first item confirms them.
    pub fn begin_epoch(&mut self, epoch: u64) {
        self.epoch = Some(epoch);
    }

    /// The connection ended: refuse every update and mark live views stale.
    pub fn end_epoch(&mut self) {
        self.epoch = None;
        if let Some(held) = &mut self.registry
            && held.view.freshness == Freshness::Live
        {
            let mut view = (*held.view).clone();
            view.freshness = Freshness::Stale;
            held.view = Arc::new(view);
        }
        for held in &mut self.tasks {
            if held.view.freshness == Freshness::Live {
                let mut view = (*held.view).clone();
                view.freshness = Freshness::Stale;
                held.view = Arc::new(view);
            }
        }
    }

    pub fn registry(&self) -> Option<Arc<RegistryView>> {
        self.registry.as_ref().map(|held| held.view.clone())
    }

    pub fn task(&self, task_id: &str) -> Option<Arc<TaskView>> {
        self.tasks
            .iter()
            .find(|held| held.view.task_id == task_id)
            .map(|held| held.view.clone())
    }

    /// The cursor to resume the registry watch from.
    pub fn registry_cursor(&self) -> Option<ProjectionCursor> {
        self.registry.as_ref().map(|held| held.view.cursor.clone())
    }

    /// The cursor to resume a task watch from (none for a removed task).
    pub fn task_cursor(&self, task_id: &str) -> Option<ProjectionCursor> {
        self.task(task_id).and_then(|view| view.cursor.clone())
    }

    /// Drop a task's detail from the store (and its saved cache).
    pub fn forget_task(&mut self, task_id: &str) {
        self.tasks.retain(|held| held.view.task_id != task_id);
    }

    /// The client stopped following a task: keep its detail, labelled
    /// stale, and accept no delta for it until a new watch confirms it.
    pub fn release_task(&mut self, task_id: &str) {
        if let Some(held) = self
            .tasks
            .iter_mut()
            .find(|held| held.view.task_id == task_id)
        {
            held.epoch = None;
            if held.view.freshness == Freshness::Live {
                let mut view = (*held.view).clone();
                view.freshness = Freshness::Stale;
                held.view = Arc::new(view);
            }
        }
    }

    /// Apply one `WatchRemoteTasks` item received on connection `epoch`.
    pub fn apply_tasks(
        &mut self,
        epoch: u64,
        item: RemoteTasksItem,
        now_ms: i64,
    ) -> Result<Arc<RegistryView>, ProjectionError> {
        self.accept(epoch)?;
        let view = match item {
            RemoteTasksItem::Snapshot { registry } => {
                self.check_host(&registry.host)?;
                check_registry(&registry.projects, &registry.tasks, registry.total_tasks)?;
                RegistryView {
                    host: registry.host,
                    cursor: registry.cursor,
                    observed_at_ms: registry.observed_at_ms,
                    projects: registry.projects,
                    tasks: registry.tasks,
                    total_tasks: registry.total_tasks,
                    providers: registry.providers,
                    confirmed_at_ms: now_ms,
                    freshness: Freshness::Live,
                    last_update: ProjectionUpdate::Snapshot,
                }
            }
            RemoteTasksItem::Current { current } => {
                self.check_host(&current.host)?;
                let held = self
                    .registry
                    .as_ref()
                    .ok_or_else(|| out_of_order("`current` arrived without a held registry"))?;
                if held.view.cursor != current.cursor {
                    return Err(out_of_order("`current` names another cursor"));
                }
                let mut view = (*held.view).clone();
                view.host = current.host;
                view.observed_at_ms = current.observed_at_ms;
                view.confirmed_at_ms = now_ms;
                view.freshness = Freshness::Live;
                view.last_update = ProjectionUpdate::Current;
                view
            }
            RemoteTasksItem::Delta { delta } => {
                let held = self
                    .registry
                    .as_ref()
                    .ok_or_else(|| out_of_order("a delta arrived without a held registry"))?;
                check_step(
                    held.epoch,
                    epoch,
                    &held.view.cursor,
                    &delta.base,
                    &delta.cursor,
                )?;
                let tasks = apply_rows(
                    &held.view.tasks,
                    delta.upsert,
                    &delta.remove,
                    delta.order,
                    |row| &row.task_id,
                    MAX_REGISTRY_TASKS,
                )?;
                let projects = delta.projects.unwrap_or_else(|| held.view.projects.clone());
                check_registry(&projects, &tasks, delta.total_tasks)?;
                let mut view = (*held.view).clone();
                view.cursor = delta.cursor;
                view.observed_at_ms = delta.observed_at_ms;
                view.projects = projects;
                view.tasks = tasks;
                view.total_tasks = delta.total_tasks;
                view.confirmed_at_ms = now_ms;
                view.freshness = Freshness::Live;
                view.last_update = ProjectionUpdate::Delta;
                view
            }
        };
        let view = Arc::new(view);
        self.registry = Some(Held {
            view: view.clone(),
            epoch: Some(epoch),
        });
        Ok(view)
    }

    /// Apply one `WatchRemoteTask` item for `task_id` received on
    /// connection `epoch`.
    pub fn apply_task(
        &mut self,
        epoch: u64,
        task_id: &str,
        item: RemoteTaskItem,
        now_ms: i64,
    ) -> Result<Arc<TaskView>, ProjectionError> {
        self.accept(epoch)?;
        let held = self.tasks.iter().find(|held| held.view.task_id == task_id);
        let view = match item {
            RemoteTaskItem::Snapshot { detail } => {
                self.check_host(&detail.host)?;
                if detail.task.task_id != task_id {
                    return Err(invalid(format!(
                        "a snapshot for task {} arrived on task {task_id}'s watch",
                        detail.task.task_id
                    )));
                }
                check_entries(&detail.entries, detail.total_entries)?;
                TaskView {
                    task_id: task_id.into(),
                    host: detail.host,
                    cursor: Some(detail.cursor),
                    observed_at_ms: detail.observed_at_ms,
                    task: Some(detail.task),
                    task_revision: detail.task_revision,
                    task_sequence: detail.task_sequence,
                    entries: detail.entries,
                    total_entries: detail.total_entries,
                    confirmed_at_ms: now_ms,
                    freshness: Freshness::Live,
                    last_update: ProjectionUpdate::Snapshot,
                }
            }
            RemoteTaskItem::Current { current } => {
                self.check_host(&current.host)?;
                let held =
                    held.ok_or_else(|| out_of_order("`current` arrived without a held task"))?;
                if held.view.cursor.as_ref() != Some(&current.cursor) {
                    return Err(out_of_order("`current` names another cursor"));
                }
                let mut view = (*held.view).clone();
                view.host = current.host;
                view.observed_at_ms = current.observed_at_ms;
                view.confirmed_at_ms = now_ms;
                view.freshness = Freshness::Live;
                view.last_update = ProjectionUpdate::Current;
                view
            }
            RemoteTaskItem::Delta { delta } => {
                let held =
                    held.ok_or_else(|| out_of_order("a delta arrived without a held task"))?;
                let base = held
                    .view
                    .cursor
                    .as_ref()
                    .ok_or_else(|| out_of_order("a delta arrived for a removed task"))?;
                check_step(held.epoch, epoch, base, &delta.base, &delta.cursor)?;
                if let Some(task) = &delta.task
                    && task.task_id != task_id
                {
                    return Err(invalid("a delta changed the task's identity"));
                }
                let entries = apply_rows(
                    &held.view.entries,
                    delta.upsert,
                    &delta.remove,
                    delta.order,
                    |entry| &entry.entry_id,
                    MAX_HISTORY_ENTRIES,
                )?;
                check_entries(&entries, delta.total_entries)?;
                let mut view = (*held.view).clone();
                view.cursor = Some(delta.cursor);
                view.observed_at_ms = delta.observed_at_ms;
                if let Some(task) = delta.task {
                    view.task = Some(task);
                }
                view.task_revision = delta.task_revision;
                view.task_sequence = delta.task_sequence;
                view.entries = entries;
                view.total_entries = delta.total_entries;
                view.confirmed_at_ms = now_ms;
                view.freshness = Freshness::Live;
                view.last_update = ProjectionUpdate::Delta;
                view
            }
            RemoteTaskItem::Removed { removed } => {
                self.check_host(&removed.host)?;
                if removed.task_id != task_id {
                    return Err(invalid("a removal for another task arrived"));
                }
                TaskView {
                    task_id: task_id.into(),
                    host: removed.host,
                    cursor: None,
                    observed_at_ms: removed.observed_at_ms,
                    task: None,
                    task_revision: 0,
                    task_sequence: 0,
                    entries: Vec::new(),
                    total_entries: 0,
                    confirmed_at_ms: now_ms,
                    freshness: Freshness::Live,
                    last_update: ProjectionUpdate::Removed,
                }
            }
        };
        let view = Arc::new(view);
        self.put_task(Held {
            view: view.clone(),
            epoch: Some(epoch),
        });
        Ok(view)
    }

    /// The saved form of this store, for the platform to keep in its own
    /// protected storage. Oldest task details are dropped to stay bounded.
    pub fn encode(&self, now_ms: i64) -> Vec<u8> {
        let mut saved = SavedCache {
            version: CACHE_VERSION,
            key: self.key.clone(),
            saved_at_ms: now_ms,
            registry: self.registry.as_ref().map(|held| (*held.view).clone()),
            tasks: self.tasks.iter().map(|held| (*held.view).clone()).collect(),
        };
        loop {
            let bytes = serde_json::to_vec(&saved).unwrap_or_default();
            if bytes.len() <= MAX_CACHE_BYTES {
                return bytes;
            }
            if saved.tasks.is_empty() {
                saved.registry = None;
            } else {
                saved.tasks.remove(0);
            }
        }
    }

    /// Restore a saved cache for `key`. Every view comes back
    /// [`Freshness::Cached`] until a connection confirms it.
    pub fn decode(bytes: &[u8], key: &HostKey) -> Result<Self, CacheError> {
        if bytes.len() > MAX_CACHE_BYTES {
            return Err(CacheError::TooLarge);
        }
        let saved: SavedCache = serde_json::from_slice(bytes)
            .map_err(|error| CacheError::Unreadable(error.to_string()))?;
        if saved.version != CACHE_VERSION {
            return Err(CacheError::Unreadable(format!(
                "version {} is not {CACHE_VERSION}",
                saved.version
            )));
        }
        if saved.key != *key {
            return Err(CacheError::OtherHost);
        }
        let mut store = Self::new(key.clone());
        if let Some(mut view) = saved.registry {
            if !key.accepts(&view.host) {
                return Err(CacheError::OtherHost);
            }
            check_registry(&view.projects, &view.tasks, view.total_tasks)
                .map_err(|error| CacheError::Invalid(error.to_string()))?;
            view.freshness = Freshness::Cached;
            view.last_update = ProjectionUpdate::Restored;
            store.registry = Some(Held {
                view: Arc::new(view),
                epoch: None,
            });
        }
        if saved.tasks.len() > MAX_CACHED_TASKS {
            return Err(CacheError::Invalid("too many task details".into()));
        }
        for mut view in saved.tasks {
            if !key.accepts(&view.host) {
                return Err(CacheError::OtherHost);
            }
            if view.task_id.is_empty()
                || view.task_id.len() > MAX_ID_BYTES
                || view
                    .task
                    .as_ref()
                    .is_some_and(|task| task.task_id != view.task_id)
                || store.task(&view.task_id).is_some()
            {
                return Err(CacheError::Invalid("a task detail is malformed".into()));
            }
            check_entries(&view.entries, view.total_entries)
                .map_err(|error| CacheError::Invalid(error.to_string()))?;
            view.freshness = Freshness::Cached;
            view.last_update = ProjectionUpdate::Restored;
            store.tasks.push_back(Held {
                view: Arc::new(view),
                epoch: None,
            });
        }
        Ok(store)
    }

    fn accept(&self, epoch: u64) -> Result<(), ProjectionError> {
        if self.epoch == Some(epoch) {
            Ok(())
        } else {
            Err(ProjectionError::StaleGeneration)
        }
    }

    fn check_host(&self, host: &ProjectionHost) -> Result<(), ProjectionError> {
        if self.key.accepts(host) {
            Ok(())
        } else {
            Err(ProjectionError::WrongHost(format!(
                "the projection describes device {} ({:?}), not the paired host {}",
                host.device_id, host.environment_id, self.key.device_id
            )))
        }
    }

    fn put_task(&mut self, held: Held<TaskView>) {
        let task_id = held.view.task_id.clone();
        self.tasks.retain(|current| current.view.task_id != task_id);
        self.tasks.push_back(held);
        while self.tasks.len() > MAX_CACHED_TASKS {
            self.tasks.pop_front();
        }
    }
}

fn out_of_order(detail: impl Into<String>) -> ProjectionError {
    ProjectionError::OutOfOrder(detail.into())
}

fn invalid(detail: impl Into<String>) -> ProjectionError {
    ProjectionError::Invalid(detail.into())
}

/// A delta follows the held cursor, on the connection that confirmed it,
/// and moves forward within one generation.
fn check_step(
    held_epoch: Option<u64>,
    epoch: u64,
    held: &ProjectionCursor,
    base: &ProjectionCursor,
    next: &ProjectionCursor,
) -> Result<(), ProjectionError> {
    if held_epoch != Some(epoch) {
        return Err(out_of_order(
            "a delta arrived before this connection confirmed the held state",
        ));
    }
    if held != base {
        return Err(out_of_order(format!(
            "a delta for revision {} arrived on revision {}",
            base.revision, held.revision
        )));
    }
    if next.generation != base.generation || next.revision <= base.revision {
        return Err(out_of_order("a delta does not move forward"));
    }
    Ok(())
}

fn check_registry(
    projects: &[RemoteProject],
    tasks: &[RemoteTaskRow],
    total_tasks: u32,
) -> Result<(), ProjectionError> {
    if projects.len() > MAX_REGISTRY_PROJECTS || tasks.len() > MAX_REGISTRY_TASKS {
        return Err(invalid("the registry exceeds its bounds"));
    }
    if (total_tasks as usize) < tasks.len() {
        return Err(invalid("the registry counts fewer tasks than it lists"));
    }
    unique(tasks.iter().map(|row| row.task_id.as_str()), "task")?;
    unique(
        projects.iter().map(|project| project.project_id.as_str()),
        "project",
    )
}

fn check_entries(
    entries: &[RemoteHistoryEntry],
    total_entries: u32,
) -> Result<(), ProjectionError> {
    if entries.len() > MAX_HISTORY_ENTRIES
        || entries
            .iter()
            .any(|entry| entry.text.len() > MAX_ENTRY_TEXT_BYTES)
    {
        return Err(invalid("the task detail exceeds its bounds"));
    }
    if (total_entries as usize) < entries.len() {
        return Err(invalid(
            "the task detail counts fewer entries than it lists",
        ));
    }
    unique(entries.iter().map(|entry| entry.entry_id.as_str()), "entry")
}

/// Every id is bounded and appears once.
fn unique<'a>(ids: impl Iterator<Item = &'a str>, what: &str) -> Result<(), ProjectionError> {
    let mut seen = HashSet::new();
    for id in ids {
        if id.is_empty() || id.len() > MAX_ID_BYTES || !seen.insert(id) {
            return Err(invalid(format!(
                "a {what} id is empty, too long or repeated"
            )));
        }
    }
    Ok(())
}

/// `current` with `remove` dropped and `upsert` applied, in `order` (or the
/// previous order when no row was added or removed).
fn apply_rows<T: Clone>(
    current: &[T],
    upsert: Vec<T>,
    remove: &[String],
    order: Option<Vec<String>>,
    key: impl Fn(&T) -> &String,
    max: usize,
) -> Result<Vec<T>, ProjectionError> {
    let mut rows: HashMap<String, T> = HashMap::with_capacity(current.len() + upsert.len());
    for row in current {
        rows.insert(key(row).clone(), row.clone());
    }
    let removed: HashSet<&str> = remove.iter().map(String::as_str).collect();
    for id in remove {
        rows.remove(id);
    }
    let mut added = false;
    for row in upsert {
        let id = key(&row).clone();
        if removed.contains(id.as_str()) {
            return Err(invalid("a row is both changed and removed"));
        }
        if rows.insert(id, row).is_none() {
            added = true;
        }
    }
    let order = match order {
        Some(order) => order,
        None if added || !remove.is_empty() => {
            return Err(invalid("rows were added or removed without an order"));
        }
        None => current.iter().map(|row| key(row).clone()).collect(),
    };
    if order.len() != rows.len() || order.len() > max {
        return Err(invalid("the order does not name every row once"));
    }
    let mut next = Vec::with_capacity(order.len());
    for id in order {
        let row = rows
            .remove(&id)
            .ok_or_else(|| invalid("the order names an unknown or repeated row"))?;
        next.push(row);
    }
    Ok(next)
}
