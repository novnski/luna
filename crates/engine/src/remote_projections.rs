//! RM-03 host-authoritative task projections (`WatchRemoteTasks`,
//! `WatchRemoteTask`); the wire contract is `luna_proto::remote_projection`.
//!
//! One owner per engine, shared by its local and remote services, so every
//! subscriber and every reconnect sees one generation and one revision
//! sequence. Nothing here is a second store: rows are computed from the
//! existing owners (workspace registry rows, live session status, the chat
//! document's published transcript and the durable task-command store) each
//! time one of them changes.
//!
//! Revisions are drawn from one counter. A projection's revision moves only
//! when its computed rows differ from the last published rows, and rows are
//! computed under the owner's lock from the newest values, so a newer
//! revision never carries older data. The owner keeps only the latest rows
//! (no replay log): a client whose cursor is not current gets a snapshot.
//!
//! Work and output are bounded: the registry by `MAX_REGISTRY_*`, a detail by
//! `MAX_HISTORY_*`, the tracked task states by [`MAX_TRACKED_TASKS`]. Bursts
//! of changes are coalesced into one item, and the transport's bounded queue
//! applies back-pressure, under which the watch channels keep only the newest
//! value.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_proto::remote_projection::{
    MAX_ENTRY_TEXT_BYTES, MAX_ENTRY_TOOLS, MAX_HISTORY_ENTRIES, MAX_HISTORY_TEXT_BYTES,
    MAX_ID_BYTES, MAX_LABEL_BYTES, MAX_REGISTRY_PROJECTS, MAX_REGISTRY_TASKS, ProjectionCurrent,
    ProjectionCursor, ProjectionHost, ProviderReadiness, REMOTE_PROJECTIONS_V1, RemoteHistoryEntry,
    RemoteProject, RemoteTaskDelta, RemoteTaskDetail, RemoteTaskItem, RemoteTaskRegistry,
    RemoteTaskRow, RemoteTaskStatus, RemoteTasksDelta, RemoteTasksItem, RemovedTask, bounded,
};
use luna_proto::{
    Chat, ChatIndicator, CommandReceipt, Session, Space, WorkspaceScope, chat_indicator,
    compare_pin_order,
};
use luna_rpc::RpcError;
use tokio::sync::{broadcast, watch};

use crate::doc_host::{ChatDocHandle, DocHost, TranscriptSnapshot};
use crate::registry::HarnessRegistry;
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

/// Task detail states kept for revision continuity; the oldest is dropped.
/// A dropped task's next revision still comes from the shared counter.
const MAX_TRACKED_TASKS: usize = 256;
/// A burst of registry changes within this window becomes one item.
const REGISTRY_COALESCE: Duration = Duration::from_millis(100);
/// A burst of transcript changes (a streaming reply) within this window
/// becomes one item.
const TASK_COALESCE: Duration = Duration::from_millis(50);

/// Which host the projections describe.
pub(crate) struct HostIdentity {
    pub(crate) device_id: String,
    pub(crate) environment_id: Option<String>,
    pub(crate) workspace_scope: WorkspaceScope,
    pub(crate) profile_id: Option<String>,
}

pub(crate) struct RemoteProjections {
    identity: HostIdentity,
    generation: String,
    revisions: AtomicU64,
    registry: Mutex<Option<Published<RegistryRows>>>,
    tasks: Mutex<TaskStates>,
    workspace: WorkspaceHost,
    sessions: SessionsEngine,
    doc_host: DocHost,
    harnesses: Arc<HarnessRegistry>,
}

struct Published<T> {
    revision: u64,
    value: Arc<T>,
}

#[derive(PartialEq)]
struct RegistryRows {
    projects: Vec<RemoteProject>,
    tasks: Vec<RemoteTaskRow>,
    total_tasks: u32,
}

#[derive(PartialEq)]
struct DetailRows {
    task: RemoteTaskRow,
    task_revision: u64,
    task_sequence: u64,
    entries: Vec<RemoteHistoryEntry>,
    total_entries: u32,
}

#[derive(Default)]
struct TaskStates {
    order: VecDeque<String>,
    states: HashMap<String, Published<DetailRows>>,
}

impl RemoteProjections {
    pub(crate) fn new(
        identity: HostIdentity,
        workspace: WorkspaceHost,
        sessions: SessionsEngine,
        doc_host: DocHost,
        harnesses: Arc<HarnessRegistry>,
    ) -> Arc<Self> {
        Arc::new(Self {
            identity,
            generation: uuid::Uuid::new_v4().to_string(),
            revisions: AtomicU64::new(0),
            registry: Mutex::new(None),
            tasks: Mutex::new(TaskStates::default()),
            workspace,
            sessions,
            doc_host,
            harnesses,
        })
    }

    fn host(&self) -> ProjectionHost {
        ProjectionHost {
            device_id: self.identity.device_id.clone(),
            environment_id: self.identity.environment_id.clone(),
            workspace_scope: self.identity.workspace_scope,
            profile_id: self.identity.profile_id.clone(),
            generation: self.generation.clone(),
            engine_version: env!("CARGO_PKG_VERSION").into(),
            capabilities: vec![REMOTE_PROJECTIONS_V1.into()],
        }
    }

    fn cursor(&self, revision: u64) -> ProjectionCursor {
        ProjectionCursor {
            generation: self.generation.clone(),
            revision,
        }
    }

    /// Whether `after` names exactly this published revision.
    fn is_current(&self, after: Option<&ProjectionCursor>, revision: u64) -> bool {
        after.is_some_and(|after| after.generation == self.generation && after.revision == revision)
    }

    fn current(&self, revision: u64) -> ProjectionCurrent {
        ProjectionCurrent {
            host: self.host(),
            cursor: self.cursor(revision),
            observed_at_ms: now_ms(),
        }
    }

    fn providers(&self) -> Vec<ProviderReadiness> {
        self.harnesses
            .descriptors()
            .into_iter()
            .map(|descriptor| ProviderReadiness {
                id: wire_name(&descriptor.id),
                enabled: descriptor.enabled.unwrap_or(descriptor.installed),
                name: descriptor.name,
                installed: descriptor.installed,
            })
            .collect()
    }

    /// Compute the registry under the lock and publish it if it changed.
    fn publish_registry(&self, compute: impl FnOnce() -> RegistryRows) -> (u64, Arc<RegistryRows>) {
        let mut slot = lock(&self.registry);
        let rows = compute();
        if let Some(current) = slot.as_ref()
            && *current.value == rows
        {
            return (current.revision, current.value.clone());
        }
        let revision = self.next_revision();
        let value = Arc::new(rows);
        *slot = Some(Published {
            revision,
            value: value.clone(),
        });
        (revision, value)
    }

    /// Compute one task's detail under the lock and publish it if it
    /// changed; `None` when the host no longer lists the task.
    fn publish_task(
        &self,
        task_id: &str,
        compute: impl FnOnce() -> Option<DetailRows>,
    ) -> Option<(u64, Arc<DetailRows>)> {
        let mut tasks = lock(&self.tasks);
        let Some(rows) = compute() else {
            tasks.states.remove(task_id);
            tasks.order.retain(|id| id != task_id);
            return None;
        };
        if let Some(current) = tasks.states.get(task_id)
            && *current.value == rows
        {
            return Some((current.revision, current.value.clone()));
        }
        let revision = self.next_revision();
        let value = Arc::new(rows);
        if tasks
            .states
            .insert(
                task_id.to_owned(),
                Published {
                    revision,
                    value: value.clone(),
                },
            )
            .is_none()
        {
            tasks.order.push_back(task_id.to_owned());
            while tasks.order.len() > MAX_TRACKED_TASKS {
                if let Some(oldest) = tasks.order.pop_front() {
                    tasks.states.remove(&oldest);
                }
            }
        }
        Some((revision, value))
    }

    fn next_revision(&self) -> u64 {
        self.revisions.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// `WatchRemoteTasks`: `snapshot` or `current`, then each change as a
    /// delta against the previous item's cursor.
    pub(crate) fn watch_tasks(
        self: &Arc<Self>,
        after: Option<ProjectionCursor>,
    ) -> BoxStream<'static, serde_json::Value> {
        let state = RegistryWatch {
            owner: self.clone(),
            chats: self.workspace.watch_chats(),
            spaces: self.workspace.watch_spaces(),
            sessions: self.sessions.watch_sessions(),
            after,
            last: None,
        };
        futures::stream::unfold(state, |mut state| async move {
            loop {
                if state.last.is_some() {
                    if !state.changed().await {
                        return None;
                    }
                    tokio::time::sleep(REGISTRY_COALESCE).await;
                }
                let owner = state.owner.clone();
                let device_id = owner.identity.device_id.clone();
                let (revision, rows) = owner.publish_registry(|| {
                    let chats = state.chats.borrow_and_update();
                    let spaces = state.spaces.borrow_and_update();
                    let sessions = state.sessions.borrow_and_update();
                    registry_rows(&device_id, &chats, &spaces, &sessions)
                });
                let item = match state.last.take() {
                    None if owner.is_current(state.after.as_ref(), revision) => {
                        Some(RemoteTasksItem::Current {
                            current: owner.current(revision),
                        })
                    }
                    None => Some(RemoteTasksItem::Snapshot {
                        registry: RemoteTaskRegistry {
                            host: owner.host(),
                            cursor: owner.cursor(revision),
                            observed_at_ms: now_ms(),
                            projects: rows.projects.clone(),
                            tasks: rows.tasks.clone(),
                            total_tasks: rows.total_tasks,
                            providers: owner.providers(),
                        },
                    }),
                    Some((base, _)) if base == revision => None,
                    Some((base, previous)) => {
                        let (upsert, remove, order) =
                            diff(&previous.tasks, &rows.tasks, |row| &row.task_id);
                        Some(RemoteTasksItem::Delta {
                            delta: RemoteTasksDelta {
                                base: owner.cursor(base),
                                cursor: owner.cursor(revision),
                                observed_at_ms: now_ms(),
                                projects: (previous.projects != rows.projects)
                                    .then(|| rows.projects.clone()),
                                upsert,
                                remove,
                                order,
                                total_tasks: rows.total_tasks,
                            },
                        })
                    }
                };
                state.last = Some((revision, rows));
                if let Some(item) = item {
                    let value = serde_json::to_value(item).ok()?;
                    return Some((value, state));
                }
            }
        })
        .boxed()
    }

    /// `WatchRemoteTask`: `snapshot` or `current`, then deltas; `removed`
    /// (and the end of the stream) once the host no longer lists the task.
    pub(crate) fn watch_task(
        self: &Arc<Self>,
        task_id: String,
        after: Option<ProjectionCursor>,
    ) -> Result<BoxStream<'static, serde_json::Value>, RpcError> {
        if task_id.is_empty() || task_id.len() > MAX_ID_BYTES {
            return Err(RpcError::BadParams(format!(
                "a task id is 1-{MAX_ID_BYTES} bytes"
            )));
        }
        let chats = self.workspace.watch_chats();
        let spaces = self.workspace.watch_spaces();
        let sessions = self.sessions.watch_sessions();
        let listed = task_row(
            &self.identity.device_id,
            &task_id,
            &chats.borrow(),
            &spaces.borrow(),
            &sessions.borrow(),
        )
        .is_some();
        if !listed {
            // Never open (and so create) a document for a task this host
            // does not list.
            let removed = RemoteTaskItem::Removed {
                removed: RemovedTask {
                    host: self.host(),
                    task_id,
                    observed_at_ms: now_ms(),
                },
            };
            let value = serde_json::to_value(removed)
                .map_err(|error| RpcError::Failed(error.to_string()))?;
            return Ok(futures::stream::iter([value]).boxed());
        }
        // Subscribe to receipts before reading the durable position, so a
        // receipt committed in between still triggers a recompute.
        let receipts = self.doc_host.subscribe_task_receipts();
        let handle = self
            .doc_host
            .open(&task_id)
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        let transcript = handle.watch_messages();
        let state = TaskWatch {
            owner: self.clone(),
            task_id,
            after,
            _handle: handle,
            transcript,
            receipts,
            chats,
            spaces,
            sessions,
            commands: (0, 0),
            last: None,
            ended: false,
        };
        Ok(futures::stream::unfold(state, |mut state| async move {
            loop {
                if state.ended {
                    return None;
                }
                if state.last.is_some() {
                    if !state.changed().await {
                        return None;
                    }
                    tokio::time::sleep(TASK_COALESCE).await;
                }
                let owner = state.owner.clone();
                if let Ok(read) = owner.doc_host.task_command_snapshot(&state.task_id) {
                    state.commands = (read.snapshot.task_revision, read.snapshot.task_sequence);
                }
                let device_id = owner.identity.device_id.clone();
                let (task_revision, task_sequence) = state.commands;
                let published = owner.publish_task(&state.task_id.clone(), || {
                    let task = {
                        let chats = state.chats.borrow_and_update();
                        let spaces = state.spaces.borrow_and_update();
                        let sessions = state.sessions.borrow_and_update();
                        task_row(&device_id, &state.task_id, &chats, &spaces, &sessions)?
                    };
                    let transcript: TranscriptSnapshot =
                        state.transcript.borrow_and_update().clone();
                    let (entries, total_entries) = history(&transcript.entries);
                    Some(DetailRows {
                        task,
                        task_revision,
                        task_sequence,
                        entries,
                        total_entries,
                    })
                });
                let Some((revision, rows)) = published else {
                    state.ended = true;
                    let removed = RemoteTaskItem::Removed {
                        removed: RemovedTask {
                            host: owner.host(),
                            task_id: state.task_id.clone(),
                            observed_at_ms: now_ms(),
                        },
                    };
                    let value = serde_json::to_value(removed).ok()?;
                    return Some((value, state));
                };
                let item = match state.last.take() {
                    None if owner.is_current(state.after.as_ref(), revision) => {
                        Some(RemoteTaskItem::Current {
                            current: owner.current(revision),
                        })
                    }
                    None => Some(RemoteTaskItem::Snapshot {
                        detail: RemoteTaskDetail {
                            host: owner.host(),
                            cursor: owner.cursor(revision),
                            observed_at_ms: now_ms(),
                            task: rows.task.clone(),
                            task_revision: rows.task_revision,
                            task_sequence: rows.task_sequence,
                            entries: rows.entries.clone(),
                            total_entries: rows.total_entries,
                        },
                    }),
                    Some((base, _)) if base == revision => None,
                    Some((base, previous)) => {
                        let (upsert, remove, order) =
                            diff(&previous.entries, &rows.entries, |entry| &entry.entry_id);
                        Some(RemoteTaskItem::Delta {
                            delta: RemoteTaskDelta {
                                base: owner.cursor(base),
                                cursor: owner.cursor(revision),
                                observed_at_ms: now_ms(),
                                task: (previous.task != rows.task).then(|| rows.task.clone()),
                                task_revision: rows.task_revision,
                                task_sequence: rows.task_sequence,
                                upsert,
                                remove,
                                order,
                                total_entries: rows.total_entries,
                            },
                        })
                    }
                };
                state.last = Some((revision, rows));
                if let Some(item) = item {
                    let value = serde_json::to_value(item).ok()?;
                    return Some((value, state));
                }
            }
        })
        .boxed())
    }
}

struct RegistryWatch {
    owner: Arc<RemoteProjections>,
    chats: watch::Receiver<Vec<Chat>>,
    spaces: watch::Receiver<Vec<Space>>,
    sessions: watch::Receiver<Vec<Session>>,
    after: Option<ProjectionCursor>,
    last: Option<(u64, Arc<RegistryRows>)>,
}

impl RegistryWatch {
    /// Resolves when a source changed; false once a source is gone.
    async fn changed(&mut self) -> bool {
        tokio::select! {
            changed = self.chats.changed() => changed.is_ok(),
            changed = self.spaces.changed() => changed.is_ok(),
            changed = self.sessions.changed() => changed.is_ok(),
        }
    }
}

struct TaskWatch {
    owner: Arc<RemoteProjections>,
    task_id: String,
    after: Option<ProjectionCursor>,
    /// Keeps the document (and its transcript publication) open.
    _handle: Arc<ChatDocHandle>,
    transcript: watch::Receiver<TranscriptSnapshot>,
    receipts: broadcast::Receiver<CommandReceipt>,
    chats: watch::Receiver<Vec<Chat>>,
    spaces: watch::Receiver<Vec<Space>>,
    sessions: watch::Receiver<Vec<Session>>,
    commands: (u64, u64),
    last: Option<(u64, Arc<DetailRows>)>,
    ended: bool,
}

impl TaskWatch {
    /// Resolves when the transcript, the task's row or its command position
    /// may have changed; false once a source is gone.
    async fn changed(&mut self) -> bool {
        loop {
            tokio::select! {
                changed = self.transcript.changed() => return changed.is_ok(),
                changed = self.chats.changed() => return changed.is_ok(),
                changed = self.spaces.changed() => return changed.is_ok(),
                changed = self.sessions.changed() => return changed.is_ok(),
                receipt = self.receipts.recv() => match receipt {
                    Ok(receipt) if receipt.task_id == self.task_id => return true,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => return true,
                    Err(broadcast::error::RecvError::Closed) => return false,
                },
            }
        }
    }
}

/// The projects (spaces) this host owns, and every task it lists.
fn registry_rows(
    device_id: &str,
    chats: &[Chat],
    spaces: &[Space],
    sessions: &[Session],
) -> RegistryRows {
    let listed = listed_spaces(device_id, spaces);
    let mut projects: Vec<RemoteProject> = spaces
        .iter()
        .filter(|space| listed.contains(space.id.as_str()))
        .map(|space| RemoteProject {
            project_id: space.id.clone(),
            name: bounded(space.display_name(), MAX_LABEL_BYTES).0,
        })
        .collect();
    projects.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.project_id.cmp(&b.project_id))
    });
    projects.truncate(MAX_REGISTRY_PROJECTS);
    let live = live_sessions(device_id, sessions);
    let mut listed_chats: Vec<&Chat> = chats
        .iter()
        .filter(|chat| is_listed(device_id, chat, &listed))
        .collect();
    listed_chats.sort_by(|a, b| host_order(a, b));
    let total_tasks = u32::try_from(listed_chats.len()).unwrap_or(u32::MAX);
    let tasks = listed_chats
        .into_iter()
        .take(MAX_REGISTRY_TASKS)
        .map(|chat| row(chat, live.get(chat.id.as_str()).copied()))
        .collect();
    RegistryRows {
        projects,
        tasks,
        total_tasks,
    }
}

/// One task's row, when this host lists it.
fn task_row(
    device_id: &str,
    task_id: &str,
    chats: &[Chat],
    spaces: &[Space],
    sessions: &[Session],
) -> Option<RemoteTaskRow> {
    let listed = listed_spaces(device_id, spaces);
    let chat = chats
        .iter()
        .find(|chat| chat.id == task_id && is_listed(device_id, chat, &listed))?;
    let session = sessions
        .iter()
        .find(|session| session.chat_id == task_id && session.device_id == device_id);
    Some(row(chat, session))
}

/// This host's projects that are not removed.
fn listed_spaces<'a>(device_id: &str, spaces: &'a [Space]) -> HashSet<&'a str> {
    spaces
        .iter()
        .filter(|space| space.device_id == device_id && !space.is_removed())
        .map(|space| space.id.as_str())
        .collect()
}

/// A task this host runs, in one of its listed projects, with a bounded id.
fn is_listed(device_id: &str, chat: &Chat, listed: &HashSet<&str>) -> bool {
    chat.device_id == device_id
        && !chat.id.is_empty()
        && chat.id.len() <= MAX_ID_BYTES
        && chat
            .space_id
            .as_deref()
            .is_some_and(|space| listed.contains(space))
}

fn live_sessions<'a>(device_id: &str, sessions: &'a [Session]) -> HashMap<&'a str, &'a Session> {
    sessions
        .iter()
        .filter(|session| session.device_id == device_id)
        .map(|session| (session.chat_id.as_str(), session))
        .collect()
}

/// Pinned first (by pin order), then the most recent activity, then id.
fn host_order(a: &Chat, b: &Chat) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (&a.pinned_order, &b.pinned_order) {
        (Some(left), Some(right)) => {
            let order = compare_pin_order(left, right);
            if order != Ordering::Equal {
                return order;
            }
        }
        (Some(_), None) => return Ordering::Less,
        (None, Some(_)) => return Ordering::Greater,
        (None, None) => {}
    }
    let activity = |chat: &Chat| chat.last_message_at.unwrap_or(chat.created_at);
    activity(b).cmp(&activity(a)).then_with(|| a.id.cmp(&b.id))
}

fn row(chat: &Chat, session: Option<&Session>) -> RemoteTaskRow {
    RemoteTaskRow {
        task_id: chat.id.clone(),
        project_id: chat.space_id.clone().unwrap_or_default(),
        title: chat
            .title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .map(|title| bounded(title, MAX_LABEL_BYTES).0),
        status: match chat_indicator(chat, session) {
            ChatIndicator::Working => RemoteTaskStatus::Working,
            ChatIndicator::AwaitingApproval => RemoteTaskStatus::AwaitingApproval,
            ChatIndicator::AwaitingInput => RemoteTaskStatus::AwaitingInput,
            ChatIndicator::Errored => RemoteTaskStatus::Errored,
            ChatIndicator::Completed => RemoteTaskStatus::Completed,
            ChatIndicator::Idle => RemoteTaskStatus::Idle,
        },
        archived: chat.archived,
        pinned: chat.pinned_order.is_some(),
        unseen: chat.unseen(),
        provider: chat
            .config
            .as_ref()
            .map(|config| wire_name(&config.harness)),
        created_at_ms: chat.created_at.timestamp_millis(),
        last_activity_ms: chat.last_message_at.map(|at| at.timestamp_millis()),
        preview: chat
            .last_message_preview
            .as_deref()
            .filter(|preview| !preview.trim().is_empty())
            .map(|preview| bounded(preview, MAX_LABEL_BYTES).0),
    }
}

/// The newest entries within the entry and text bounds, oldest first.
fn history(entries: &[SessionMessageEntry]) -> (Vec<RemoteHistoryEntry>, u32) {
    let total = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    let mut budget = MAX_HISTORY_TEXT_BYTES;
    let mut projected = Vec::new();
    for entry in entries.iter().rev() {
        if projected.len() == MAX_HISTORY_ENTRIES {
            break;
        }
        if entry.id.is_empty() || entry.id.len() > MAX_ID_BYTES {
            continue;
        }
        let item = history_entry(entry);
        if item.text.len() > budget {
            break;
        }
        budget -= item.text.len();
        projected.push(item);
    }
    projected.reverse();
    (projected, total)
}

fn history_entry(entry: &SessionMessageEntry) -> RemoteHistoryEntry {
    let mut text = String::new();
    let mut text_truncated = false;
    let mut tools = Vec::new();
    let mut other_parts = 0u32;
    let mut error = false;
    for part in &entry.parts {
        match part {
            MessagePart::Text { text: piece, .. } => {
                if text_truncated {
                    continue;
                }
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                let room = MAX_ENTRY_TEXT_BYTES.saturating_sub(text.len());
                let (piece, cut) = bounded(piece, room);
                text.push_str(&piece);
                text_truncated = cut || text.len() >= MAX_ENTRY_TEXT_BYTES;
            }
            MessagePart::Tool { call, is_error, .. } => {
                error |= *is_error;
                if tools.len() < MAX_ENTRY_TOOLS {
                    tools.push(tool_kind(call));
                } else {
                    other_parts = other_parts.saturating_add(1);
                }
            }
            MessagePart::Error { .. } => {
                error = true;
                other_parts = other_parts.saturating_add(1);
            }
            _ => other_parts = other_parts.saturating_add(1),
        }
    }
    let (text, cut) = bounded(&text, MAX_ENTRY_TEXT_BYTES);
    RemoteHistoryEntry {
        entry_id: entry.id.clone(),
        role: match entry.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
        }
        .into(),
        status: entry.status.map(|status| {
            String::from(match status {
                MessageStatus::Streaming => "streaming",
                MessageStatus::Complete => "complete",
                MessageStatus::Aborted => "aborted",
            })
        }),
        created_at_ms: entry.created_at,
        text,
        text_truncated: text_truncated || cut,
        tools,
        other_parts,
        error,
    }
}

/// A tool call's `kind` tag (`exec`, `readFile`, `mcp`, ...).
fn tool_kind(call: &luna_proto::ToolCall) -> String {
    serde_json::to_value(call)
        .ok()
        .and_then(|value| value.get("kind")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| "tool".into())
}

/// The rows of `next` that are new or changed against `base`, the ids that
/// left, and `next`'s complete order when it differs from `base`'s.
fn diff<T: Clone + PartialEq>(
    base: &[T],
    next: &[T],
    key: impl Fn(&T) -> &String,
) -> (Vec<T>, Vec<String>, Option<Vec<String>>) {
    let mut previous: HashMap<&str, &T> = HashMap::with_capacity(base.len());
    for row in base {
        previous.insert(key(row).as_str(), row);
    }
    let mut kept: HashSet<&str> = HashSet::with_capacity(next.len());
    let mut upsert = Vec::new();
    for row in next {
        kept.insert(key(row).as_str());
        let changed = match previous.get(key(row).as_str()) {
            Some(before) => *before != row,
            None => true,
        };
        if changed {
            upsert.push(row.clone());
        }
    }
    let mut remove = Vec::new();
    for row in base {
        if !kept.contains(key(row).as_str()) {
            remove.push(key(row).clone());
        }
    }
    let reordered = base.len() != next.len()
        || base
            .iter()
            .zip(next)
            .any(|(before, after)| key(before) != key(after));
    let order = reordered.then(|| next.iter().map(|row| key(row).clone()).collect());
    (upsert, remove, order)
}

/// A unit enum's wire spelling (`claudeCode`, `codex`, ...).
fn wire_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
