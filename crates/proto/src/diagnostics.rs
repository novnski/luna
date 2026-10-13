//! AW-09 read-only diagnostics: the serving engine process
//! (`GetEngineDiagnostics`) and what this engine owns on disk
//! (`GetStorageUsage`). Both are local-only queries answered by the owning
//! engine; nothing here starts, stops, cleans or deletes anything. Identity
//! (device, scope, profile) stays in [`crate::EngineInfo`].

use serde::{Deserialize, Serialize};

/// `GetEngineDiagnostics`: the engine process that answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineDiagnostics {
    /// The serving engine's build version.
    pub engine_version: String,
    /// The engine's own process id (the app's pid when it runs in-process).
    pub pid: u32,
    /// When this engine runtime was assembled, epoch ms.
    pub started_at_ms: i64,
    pub uptime_ms: u64,
    pub os: String,
    pub arch: String,
    /// Sampled at `read_at_ms`; a field is `None` where the platform cannot
    /// sample it.
    #[serde(default)]
    pub resources: EngineResources,
    pub read_at_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineResources {
    /// Resident memory of the engine process.
    #[serde(default)]
    pub resident_bytes: Option<u64>,
    /// Cumulative CPU time of the engine process.
    #[serde(default)]
    pub cpu_user_ms: Option<u64>,
    #[serde(default)]
    pub cpu_system_ms: Option<u64>,
    #[serde(default)]
    pub threads: Option<u32>,
}

/// `GetStorageUsage`: one bounded, read-only walk of this engine's data
/// folder and the folders it owns outside it. Sizes are apparent file sizes
/// (`du -A`); symbolic links are counted as links and never followed; no
/// file content is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsage {
    /// The data folder, `~`-abbreviated.
    pub root: String,
    /// This profile's folder, relative to `root`.
    pub profile_location: String,
    pub categories: Vec<StorageCategory>,
    /// Folders inside `root` that belong to other profiles; never walked.
    #[serde(default)]
    pub excluded: Vec<StorageExclusion>,
    /// Sum of every category's `bytes`.
    pub total_bytes: u64,
    /// Entries the walk visited.
    pub entries: u64,
    /// False when a budget stopped the walk: the sizes are lower bounds.
    pub complete: bool,
    #[serde(default)]
    pub stop: Option<StorageScanStop>,
    pub started_at_ms: i64,
    pub elapsed_ms: u64,
}

impl StorageUsage {
    pub fn category(&self, id: StorageCategoryId) -> Option<&StorageCategory> {
        self.categories.iter().find(|category| category.id == id)
    }
}

/// Why a walk stopped before measuring everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageScanStop {
    /// The time budget ran out.
    Deadline,
    /// The entry budget ran out.
    EntryLimit,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCategory {
    pub id: StorageCategoryId,
    pub scope: StorageScope,
    /// Relative to the data folder, or `~`-abbreviated outside it. A pattern
    /// (`usage-*`) when the category is a set of files.
    pub location: String,
    pub state: StorageCategoryState,
    pub bytes: u64,
    pub files: u64,
    pub directories: u64,
    /// Entries whose listing or metadata could not be read.
    #[serde(default)]
    pub unreadable: u64,
    /// The first read failure's kind (never a path).
    #[serde(default)]
    pub first_error: Option<String>,
    /// Top-level names, for [`StorageCategoryId::Unrecognized`] only.
    #[serde(default)]
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageCategoryId {
    // This profile.
    WorkspaceDatabase,
    RunJournals,
    Attachments,
    TerminalHistory,
    ExtensionData,
    ProfileState,
    // This data folder, shared by its profiles.
    TurnSnapshots,
    ModelCache,
    UsageCache,
    ExtensionPackages,
    Logs,
    Repositories,
    UpdateDownloads,
    LegacyAttachments,
    BackupStaging,
    DeviceSettings,
    Unrecognized,
    // Outside the data folder.
    Backups,
    Worktrees,
    #[serde(other)]
    Unknown,
}

/// Whose data a category is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageScope {
    /// This profile only.
    Profile,
    /// This data folder: every profile opened from it.
    Device,
    /// Outside the data folder and possibly used by other LUNA data folders.
    Shared,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageCategoryState {
    /// Every entry was measured.
    Complete,
    /// Some entries could not be read, or a budget stopped inside it.
    Partial,
    /// Nothing exists there yet.
    Missing,
    /// A budget stopped the walk before it reached this category.
    NotScanned,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageExclusion {
    pub reason: StorageExclusionReason,
    /// Folders excluded for this reason.
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageExclusionReason {
    /// Another LUNA profile opened from the same data folder.
    OtherProfile,
    #[serde(other)]
    Unknown,
}
