//! RM-03 projections as the platform reads them: immutable value copies of
//! the core's [`luna_sdk::ProjectionStore`] views, labelled with freshness.
//!
//! The core follows the paired host's registry and a bounded set of tasks
//! itself (see [`crate::session::RemoteSession::following`]); delivery does
//! not depend on any open view. The platform reads these records on its own
//! actor whenever the session says it changed, and renders the freshness it
//! is given: `cached` and `stale` data are never presented as live.

use luna_proto::remote_projection::{
    ProviderReadiness, RemoteHistoryEntry, RemoteProject, RemoteTaskRow, RemoteTaskStatus,
};
use luna_sdk::{Freshness, ProjectionUpdate, RegistryView, TaskView};

/// How much a projection can be trusted right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ProjectionFreshness {
    /// Confirmed by the Mac on the open connection, which follows changes.
    Live,
    /// Confirmed earlier on this run; that connection ended.
    Stale,
    /// Restored from this phone's saved cache; not confirmed since.
    Cached,
}

/// What last changed a projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ProjectionChange {
    /// The Mac sent the whole projection (first follow, or a repair).
    Snapshot,
    /// The Mac confirmed the cursor this phone held; nothing was resent.
    Current,
    Delta,
    Removed,
    /// Restored from the saved cache.
    Restored,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RemoteProjectRecord {
    pub project_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RemoteTaskRecord {
    pub task_id: String,
    pub project_id: String,
    pub title: Option<String>,
    /// `idle`, `working`, `awaitingApproval`, `awaitingInput`, `errored` or
    /// `completed`.
    pub status: String,
    pub archived: bool,
    pub pinned: bool,
    pub unseen: bool,
    pub provider: Option<String>,
    pub created_at_ms: i64,
    pub last_activity_ms: Option<i64>,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ProviderRecord {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub enabled: bool,
}

/// The Mac's projects and tasks, in the Mac's order.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RemoteTasksProjection {
    /// The host it describes (always the paired Mac).
    pub device_id: String,
    pub generation: String,
    pub revision: u64,
    pub freshness: ProjectionFreshness,
    pub last_change: ProjectionChange,
    /// The Mac's clock when it computed this state.
    pub observed_at_ms: i64,
    /// This phone's clock when the Mac last confirmed it.
    pub confirmed_at_ms: i64,
    pub projects: Vec<RemoteProjectRecord>,
    pub tasks: Vec<RemoteTaskRecord>,
    /// Every task the Mac lists; more than `tasks` when bounded.
    pub total_tasks: u32,
    pub providers: Vec<ProviderRecord>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RemoteEntryRecord {
    pub entry_id: String,
    pub role: String,
    pub status: Option<String>,
    pub created_at_ms: i64,
    pub text: String,
    pub text_truncated: bool,
    pub tools: Vec<String>,
    pub other_parts: u32,
    pub error: bool,
}

/// One followed task as the Mac last confirmed it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RemoteTaskProjection {
    pub task_id: String,
    /// The Mac no longer lists this task.
    pub removed: bool,
    pub task: Option<RemoteTaskRecord>,
    pub generation: Option<String>,
    pub revision: Option<u64>,
    pub freshness: ProjectionFreshness,
    pub last_change: ProjectionChange,
    pub observed_at_ms: i64,
    pub confirmed_at_ms: i64,
    /// The task's durable command revision and sequence on the Mac.
    pub task_revision: u64,
    pub task_sequence: u64,
    /// The newest entries, oldest first.
    pub entries: Vec<RemoteEntryRecord>,
    pub total_entries: u32,
}

pub(crate) fn tasks_record(view: &RegistryView) -> RemoteTasksProjection {
    RemoteTasksProjection {
        device_id: view.host.device_id.clone(),
        generation: view.cursor.generation.clone(),
        revision: view.cursor.revision,
        freshness: freshness(view.freshness),
        last_change: change(view.last_update),
        observed_at_ms: view.observed_at_ms,
        confirmed_at_ms: view.confirmed_at_ms,
        projects: view.projects.iter().map(project).collect(),
        tasks: view.tasks.iter().map(task_record).collect(),
        total_tasks: view.total_tasks,
        providers: view.providers.iter().map(provider).collect(),
        capabilities: view.host.capabilities.clone(),
    }
}

pub(crate) fn task_projection(view: &TaskView) -> RemoteTaskProjection {
    RemoteTaskProjection {
        task_id: view.task_id.clone(),
        removed: view.task.is_none(),
        task: view.task.as_ref().map(task_record),
        generation: view.cursor.as_ref().map(|cursor| cursor.generation.clone()),
        revision: view.cursor.as_ref().map(|cursor| cursor.revision),
        freshness: freshness(view.freshness),
        last_change: change(view.last_update),
        observed_at_ms: view.observed_at_ms,
        confirmed_at_ms: view.confirmed_at_ms,
        task_revision: view.task_revision,
        task_sequence: view.task_sequence,
        entries: view.entries.iter().map(entry).collect(),
        total_entries: view.total_entries,
    }
}

fn freshness(value: Freshness) -> ProjectionFreshness {
    match value {
        Freshness::Live => ProjectionFreshness::Live,
        Freshness::Stale => ProjectionFreshness::Stale,
        Freshness::Cached => ProjectionFreshness::Cached,
    }
}

fn change(value: ProjectionUpdate) -> ProjectionChange {
    match value {
        ProjectionUpdate::Snapshot => ProjectionChange::Snapshot,
        ProjectionUpdate::Current => ProjectionChange::Current,
        ProjectionUpdate::Delta => ProjectionChange::Delta,
        ProjectionUpdate::Removed => ProjectionChange::Removed,
        ProjectionUpdate::Restored => ProjectionChange::Restored,
    }
}

fn project(value: &RemoteProject) -> RemoteProjectRecord {
    RemoteProjectRecord {
        project_id: value.project_id.clone(),
        name: value.name.clone(),
    }
}

fn provider(value: &ProviderReadiness) -> ProviderRecord {
    ProviderRecord {
        id: value.id.clone(),
        name: value.name.clone(),
        installed: value.installed,
        enabled: value.enabled,
    }
}

fn task_record(row: &RemoteTaskRow) -> RemoteTaskRecord {
    RemoteTaskRecord {
        task_id: row.task_id.clone(),
        project_id: row.project_id.clone(),
        title: row.title.clone(),
        status: match row.status {
            RemoteTaskStatus::Idle => "idle",
            RemoteTaskStatus::Working => "working",
            RemoteTaskStatus::AwaitingApproval => "awaitingApproval",
            RemoteTaskStatus::AwaitingInput => "awaitingInput",
            RemoteTaskStatus::Errored => "errored",
            RemoteTaskStatus::Completed => "completed",
        }
        .into(),
        archived: row.archived,
        pinned: row.pinned,
        unseen: row.unseen,
        provider: row.provider.clone(),
        created_at_ms: row.created_at_ms,
        last_activity_ms: row.last_activity_ms,
        preview: row.preview.clone(),
    }
}

fn entry(value: &RemoteHistoryEntry) -> RemoteEntryRecord {
    RemoteEntryRecord {
        entry_id: value.entry_id.clone(),
        role: value.role.clone(),
        status: value.status.clone(),
        created_at_ms: value.created_at_ms,
        text: value.text.clone(),
        text_truncated: value.text_truncated,
        tools: value.tools.clone(),
        other_parts: value.other_parts,
        error: value.error,
    }
}
