//! Host-authoritative task projections for a paired client (RM-03, M2).
//!
//! The Mac owns its projects, tasks and transcripts; a phone follows two
//! projections of them, with different costs:
//!
//! - the **task registry** (`WatchRemoteTasks`): every project and task this
//!   host runs, as cheap rows. It is the phone's project and task list.
//! - one **task's detail** (`WatchRemoteTask`): its row, its durable command
//!   position and a bounded tail of its transcript. It is the selected task.
//!
//! Every item names the host it describes ([`ProjectionHost`]), a cursor and
//! the host's clock when it was computed. A cursor is
//! `(generation, revision)`: the generation is fresh each time the engine
//! starts and the revision grows within it, so two cursors compare only when
//! their generations are equal.
//!
//! A watch starts from the client's cursor (`after`). An equal cursor is
//! answered `current` and nothing is resent; any other cursor, including one
//! from a previous engine generation, gets a full `snapshot`. After that,
//! each `delta` names the cursor it applies to (`base`). A client refuses a
//! delta whose base is not its own cursor and resubscribes for a snapshot:
//! the host keeps no replay log, so a snapshot is the repair.
//!
//! Projections are bounded on the host: see the `MAX_*` limits. Rows carry
//! names, not host paths: a project is its display name, never its folder.

use serde::{Deserialize, Serialize};

use crate::WorkspaceScope;

/// The projection contract version, advertised as an engine capability.
pub const REMOTE_PROJECTIONS_V1: &str = "remote-projections-v1";

/// Most tasks one registry projection carries (host order, newest first).
pub const MAX_REGISTRY_TASKS: usize = 500;
/// Most projects one registry projection carries.
pub const MAX_REGISTRY_PROJECTS: usize = 200;
/// Most transcript entries one task detail carries (its newest ones).
pub const MAX_HISTORY_ENTRIES: usize = 150;
/// Most text bytes one history entry carries.
pub const MAX_ENTRY_TEXT_BYTES: usize = 2048;
/// Most text bytes one task detail carries across its entries.
pub const MAX_HISTORY_TEXT_BYTES: usize = 256 * 1024;
/// Most tool kinds one history entry lists.
pub const MAX_ENTRY_TOOLS: usize = 16;
/// Most bytes of a title, a project name or a preview.
pub const MAX_LABEL_BYTES: usize = 240;
/// Most bytes of a task, project or entry id the host projects.
pub const MAX_ID_BYTES: usize = 256;

/// Where a projection is in its host's history.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProjectionCursor {
    /// Fresh per engine start; revisions compare only within one generation.
    pub generation: String,
    pub revision: u64,
}

/// The host a projection describes. A client checks it against the host it
/// paired with before applying anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProjectionHost {
    pub device_id: String,
    /// The remote environment (profile) id paired clients target; absent
    /// when this engine has no remote authority store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    pub workspace_scope: WorkspaceScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub generation: String,
    pub engine_version: String,
    /// What this host serves remotely for these projections and tasks, for
    /// example `remote-projections-v1`. Task control is absent until a host
    /// admits remote commands (RM-04).
    pub capabilities: Vec<String>,
}

/// A provider (harness) on this host and whether it can run work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProviderReadiness {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub enabled: bool,
}

/// One project, by display name only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteProject {
    pub project_id: String,
    pub name: String,
}

/// A task's status as the host derives it from its own live runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum RemoteTaskStatus {
    Idle,
    Working,
    AwaitingApproval,
    AwaitingInput,
    Errored,
    /// Finished, and not yet seen on any device.
    Completed,
}

/// One task's cheap registry row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskRow {
    pub task_id: String,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: RemoteTaskStatus,
    pub archived: bool,
    pub pinned: bool,
    pub unseen: bool,
    /// The task's provider id, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_ms: Option<i64>,
    /// A bounded preview of the last message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// The whole registry: the first item of a watch without a current cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskRegistry {
    pub host: ProjectionHost,
    pub cursor: ProjectionCursor,
    /// The host's clock (Unix ms) when this was computed.
    pub observed_at_ms: i64,
    pub projects: Vec<RemoteProject>,
    /// Host order: pinned first, then the most recent activity.
    pub tasks: Vec<RemoteTaskRow>,
    /// Every task the host would list; more than `tasks.len()` when bounded.
    pub total_tasks: u32,
    pub providers: Vec<ProviderReadiness>,
}

/// The registry changes from `base` to `cursor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteTasksDelta {
    pub base: ProjectionCursor,
    pub cursor: ProjectionCursor,
    pub observed_at_ms: i64,
    /// The complete project list, when it changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<RemoteProject>>,
    /// New or changed rows.
    pub upsert: Vec<RemoteTaskRow>,
    /// Rows that left the registry.
    pub remove: Vec<String>,
    /// The complete task order after this delta, when it changed. Always
    /// present when a row was added or removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    pub total_tasks: u32,
}

/// One `WatchRemoteTasks` item.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RemoteTasksItem {
    /// The whole registry; replaces anything the client holds.
    Snapshot {
        registry: RemoteTaskRegistry,
    },
    /// The client's cursor is current; nothing was resent.
    Current {
        current: ProjectionCurrent,
    },
    Delta {
        delta: RemoteTasksDelta,
    },
}

/// One transcript entry, reduced to what a list shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteHistoryEntry {
    pub entry_id: String,
    /// `user`, `assistant` or `system`.
    pub role: String,
    /// `streaming`, `complete` or `aborted`, when the entry has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub created_at_ms: i64,
    /// The entry's text parts, joined and bounded.
    pub text: String,
    pub text_truncated: bool,
    /// The kind of each tool call, in order and bounded.
    pub tools: Vec<String>,
    /// Parts not shown here (images, reasoning, questions, extra tools).
    pub other_parts: u32,
    pub error: bool,
}

/// One task in detail: the first item of a watch without a current cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskDetail {
    pub host: ProjectionHost,
    pub cursor: ProjectionCursor,
    pub observed_at_ms: i64,
    pub task: RemoteTaskRow,
    /// The task's durable command revision and sequence (CORE-05).
    pub task_revision: u64,
    pub task_sequence: u64,
    /// The newest entries, oldest first.
    pub entries: Vec<RemoteHistoryEntry>,
    /// Every entry in the transcript; more than `entries.len()` when bounded.
    pub total_entries: u32,
}

/// A task's changes from `base` to `cursor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskDelta {
    pub base: ProjectionCursor,
    pub cursor: ProjectionCursor,
    pub observed_at_ms: i64,
    /// The task's row, when it changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<RemoteTaskRow>,
    pub task_revision: u64,
    pub task_sequence: u64,
    pub upsert: Vec<RemoteHistoryEntry>,
    pub remove: Vec<String>,
    /// The complete entry order after this delta, when it changed. Always
    /// present when an entry was added or removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    pub total_entries: u32,
}

/// One `WatchRemoteTask` item.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RemoteTaskItem {
    /// The whole task; replaces anything the client holds for it.
    Snapshot {
        detail: RemoteTaskDetail,
    },
    /// The client's cursor is current; nothing was resent.
    Current {
        current: ProjectionCurrent,
    },
    Delta {
        delta: RemoteTaskDelta,
    },
    /// The task is not on this host (deleted, never existed, or its project
    /// was removed). The stream ends after it.
    Removed {
        removed: RemovedTask,
    },
}

/// A `current` answer: the client's cursor still names this host's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProjectionCurrent {
    pub host: ProjectionHost,
    pub cursor: ProjectionCursor,
    pub observed_at_ms: i64,
}

/// A task this host does not list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemovedTask {
    pub host: ProjectionHost,
    pub task_id: String,
    pub observed_at_ms: i64,
}

/// `WatchRemoteTasks` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteTasksParams {
    /// The cursor the client holds; omitted, the watch starts with a snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<ProjectionCursor>,
}

/// `WatchRemoteTask` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskParams {
    pub task_id: String,
    /// The cursor the client holds for this task; omitted, a snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<ProjectionCursor>,
}

/// `value` cut to at most `max` bytes on a character boundary; true when cut.
pub fn bounded(value: &str, max: usize) -> (String, bool) {
    if value.len() <= max {
        return (value.to_owned(), false);
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}
