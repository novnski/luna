//! AW-09 read-only diagnostics: the engine process, and a bounded
//! accounting of what this engine owns on disk.
//!
//! The storage walk uses only `canonicalize`, `symlink_metadata` and
//! `read_dir`. It never opens file contents, never follows a symbolic link,
//! never creates a folder and never deletes anything: cleanup is a separate,
//! reviewed action. It runs on the blocking pool, one walk at a time per
//! engine, inside an entry and time budget; a stopped walk reports lower
//! bounds and says why. This profile's folders are measured first and the
//! (potentially huge) worktree root last.
//!
//! Folder names mirror their owners: the profile store (`profile.rs`),
//! `journals`, `uploads`, `terminals`, `extensions` (CORE-06/19), the turn
//! snapshot store (`diff_sync/turn_snapshot.rs`), `model-cache`, the usage
//! scans (`usage.rs`, `usage_cache.rs`), `repos` (`repos.rs`), `updates`
//! (`luna-update`) and the backup staging and destination markers
//! (`backup.rs`). A name no owner claims is reported as unrecognized, never
//! guessed at. Other profiles' folders are counted, not walked.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use luna_proto::{
    EngineDiagnostics, EngineResources, StorageCategory, StorageCategoryId, StorageCategoryState,
    StorageExclusion, StorageExclusionReason, StorageScanStop, StorageScope, StorageUsage,
};

/// The walk's time budget.
const SCAN_DEADLINE: Duration = Duration::from_secs(15);
/// The walk's entry budget (files, links and folders visited).
const MAX_ENTRIES: u64 = 1_500_000;
/// Top-level names listed for the unrecognized category.
const MAX_ITEMS: usize = 20;
/// Mirrors `diff_sync/turn_snapshot.rs` `STORE_DIR`.
const TURN_SNAPSHOTS_DIR: &str = "turn-snapshots";
/// Every backup/restore staging name `backup.rs` creates in the data root
/// (`.luna-l17-snapshot-*`, `-restore-stage-*`, `-managed-rollback-*`, …).
const BACKUP_STAGING_PREFIX: &str = ".luna-l17-";
/// Mirrors `backup.rs` `DESTINATION_MARKER`: present only in a backup
/// destination LUNA created.
const BACKUP_DESTINATION_MARKER: &str = ".luna-l17-isolated-backups";

/// Every reported category, in output order, with its fixed scope (the
/// worktree root's scope depends on where it lives).
const CATEGORIES: [(StorageCategoryId, StorageScope); 19] = [
    (StorageCategoryId::WorkspaceDatabase, StorageScope::Profile),
    (StorageCategoryId::RunJournals, StorageScope::Profile),
    (StorageCategoryId::Attachments, StorageScope::Profile),
    (StorageCategoryId::TerminalHistory, StorageScope::Profile),
    (StorageCategoryId::ExtensionData, StorageScope::Profile),
    (StorageCategoryId::ProfileState, StorageScope::Profile),
    (StorageCategoryId::TurnSnapshots, StorageScope::Device),
    (StorageCategoryId::ModelCache, StorageScope::Device),
    (StorageCategoryId::UsageCache, StorageScope::Device),
    (StorageCategoryId::ExtensionPackages, StorageScope::Device),
    (StorageCategoryId::Logs, StorageScope::Device),
    (StorageCategoryId::Repositories, StorageScope::Device),
    (StorageCategoryId::UpdateDownloads, StorageScope::Device),
    (StorageCategoryId::LegacyAttachments, StorageScope::Device),
    (StorageCategoryId::BackupStaging, StorageScope::Device),
    (StorageCategoryId::DeviceSettings, StorageScope::Device),
    (StorageCategoryId::Unrecognized, StorageScope::Device),
    (StorageCategoryId::Backups, StorageScope::Device),
    (StorageCategoryId::Worktrees, StorageScope::Shared),
];

/// One engine's diagnostics: its start time and the roots it owns. Cloning
/// shares the walk gate.
#[derive(Clone)]
pub(crate) struct Diagnostics {
    inner: Arc<Inner>,
}

struct Inner {
    roots: StorageRoots,
    started_at_ms: i64,
    started: Instant,
    /// One storage walk at a time. The permit moves into the blocking task,
    /// so a caller that disconnects cannot start a second walk beside it.
    scan: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug, Clone)]
struct StorageRoots {
    /// The engine data folder (device-scoped).
    device: PathBuf,
    /// This profile's store root, normally inside `device`.
    profile: PathBuf,
    /// Where LUNA creates worktrees (`$LUNA_WORKTREES_DIR` or
    /// `~/.luna/worktrees`).
    worktrees: PathBuf,
}

impl Diagnostics {
    pub(crate) fn new(device_root: &Path, profile_root: &Path, worktrees_root: &Path) -> Self {
        Self {
            inner: Arc::new(Inner {
                roots: StorageRoots {
                    device: device_root.to_path_buf(),
                    profile: profile_root.to_path_buf(),
                    worktrees: worktrees_root.to_path_buf(),
                },
                started_at_ms: crate::now_ms(),
                started: Instant::now(),
                scan: Arc::new(tokio::sync::Mutex::new(())),
            }),
        }
    }

    /// The serving engine process, sampled now.
    pub(crate) fn engine(&self) -> EngineDiagnostics {
        EngineDiagnostics {
            engine_version: env!("CARGO_PKG_VERSION").to_owned(),
            pid: std::process::id(),
            started_at_ms: self.inner.started_at_ms,
            uptime_ms: u64::try_from(self.inner.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            resources: process_resources(),
            read_at_ms: crate::now_ms(),
        }
    }

    /// One bounded, read-only walk on the blocking pool.
    pub(crate) async fn storage_usage(&self) -> Result<StorageUsage, String> {
        let permit = self.inner.scan.clone().lock_owned().await;
        let roots = self.inner.roots.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            scan(
                &roots,
                Budget {
                    deadline: Instant::now() + SCAN_DEADLINE,
                    max_entries: MAX_ENTRIES,
                },
            )
        })
        .await
        .map_err(|error| format!("storage scan stopped: {error}"))
    }
}

// ---------------------------------------------------------------------------
// Process resources
// ---------------------------------------------------------------------------

fn process_resources() -> EngineResources {
    #[allow(unused_mut)]
    let mut resources = EngineResources::default();
    #[cfg(unix)]
    {
        // SAFETY: `usage` is a valid, zeroed `rusage` the call fills in.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } == 0 {
            resources.cpu_user_ms = Some(timeval_ms(usage.ru_utime));
            resources.cpu_system_ms = Some(timeval_ms(usage.ru_stime));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: `info` is a valid buffer of `size` bytes for this flavor.
        let (written, info) = unsafe {
            let mut info: libc::proc_taskinfo = std::mem::zeroed();
            let written = libc::proc_pidinfo(
                std::process::id() as libc::c_int,
                libc::PROC_PIDTASKINFO,
                0,
                (&mut info as *mut libc::proc_taskinfo).cast(),
                size,
            );
            (written, info)
        };
        if written == size {
            resources.resident_bytes = Some(info.pti_resident_size);
            resources.threads = u32::try_from(info.pti_threadnum).ok();
        }
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: plain sysconf query.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
            resources.resident_bytes = statm
                .split_whitespace()
                .nth(1)
                .and_then(|pages| pages.parse::<u64>().ok())
                .map(|pages| pages.saturating_mul(page));
        }
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            resources.threads = status
                .lines()
                .find_map(|line| line.strip_prefix("Threads:"))
                .and_then(|count| count.trim().parse().ok());
        }
    }
    resources
}

#[cfg(unix)]
fn timeval_ms(value: libc::timeval) -> u64 {
    let seconds = u64::try_from(value.tv_sec).unwrap_or(0);
    let micros = u64::try_from(value.tv_usec).unwrap_or(0);
    seconds.saturating_mul(1000).saturating_add(micros / 1000)
}

// ---------------------------------------------------------------------------
// Storage walk
// ---------------------------------------------------------------------------

struct Budget {
    deadline: Instant,
    max_entries: u64,
}

/// One category's running totals.
#[derive(Default)]
struct Tally {
    /// Paths queued for this category.
    planned: u32,
    /// Paths whose measurement began.
    started: u32,
    /// Paths measured to the end.
    finished: u32,
    /// At least one queued path existed.
    seen: bool,
    bytes: u64,
    files: u64,
    directories: u64,
    unreadable: u64,
    first_error: Option<String>,
    items: Vec<String>,
}

impl Tally {
    fn note_error(&mut self, error: &std::io::Error) {
        self.unreadable += 1;
        if self.first_error.is_none() {
            // The kind only ("permission denied"): an `io::Error` built by
            // `std::fs` carries no path, and none is added here.
            self.first_error = Some(error.kind().to_string());
        }
    }

    fn state(&self) -> StorageCategoryState {
        if self.planned == 0 {
            StorageCategoryState::Missing
        } else if self.started == 0 {
            StorageCategoryState::NotScanned
        } else if self.finished < self.planned {
            StorageCategoryState::Partial
        } else if !self.seen {
            StorageCategoryState::Missing
        } else if self.unreadable > 0 {
            StorageCategoryState::Partial
        } else {
            StorageCategoryState::Complete
        }
    }
}

struct Walker {
    budget: Budget,
    entries: u64,
    stop: Option<StorageScanStop>,
    /// Folders measured as categories of their own; no other walk descends
    /// into them, so nothing is counted twice.
    skip: Vec<PathBuf>,
}

impl Walker {
    fn exhausted(&mut self) -> bool {
        if self.stop.is_none() {
            if self.entries >= self.budget.max_entries {
                self.stop = Some(StorageScanStop::EntryLimit);
            } else if self.entries.is_multiple_of(256) && Instant::now() >= self.budget.deadline {
                self.stop = Some(StorageScanStop::Deadline);
            }
        }
        self.stop.is_some()
    }

    /// Measure `root` (a file, a link or a folder tree) into `tally`.
    /// Returns whether it was measured to the end.
    fn measure(&mut self, root: &Path, tally: &mut Tally) -> bool {
        if self.exhausted() {
            return false;
        }
        tally.started += 1;
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            if self.exhausted() {
                return false;
            }
            self.entries += 1;
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                // Gone since it was listed (or never there): nothing to count.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    tally.note_error(&error);
                    continue;
                }
            };
            tally.seen = true;
            // `symlink_metadata` never follows a link: a link to a folder is
            // one entry of its own size, and its target is never visited.
            if !metadata.is_dir() {
                tally.files += 1;
                tally.bytes = tally.bytes.saturating_add(metadata.len());
                continue;
            }
            if path != root && self.skip.contains(&path) {
                continue;
            }
            tally.directories += 1;
            let listing = match std::fs::read_dir(&path) {
                Ok(listing) => listing,
                Err(error) => {
                    tally.note_error(&error);
                    continue;
                }
            };
            for (index, entry) in listing.enumerate() {
                if self.entries.saturating_add(pending.len() as u64) >= self.budget.max_entries {
                    self.stop = Some(StorageScanStop::EntryLimit);
                    return false;
                }
                if index % 1024 == 1023 && Instant::now() >= self.budget.deadline {
                    self.stop = Some(StorageScanStop::Deadline);
                    return false;
                }
                match entry {
                    Ok(entry) => pending.push(entry.path()),
                    Err(error) => tally.note_error(&error),
                }
            }
        }
        tally.finished += 1;
        true
    }
}

/// Where one top-level name of the data folder belongs.
enum DeviceEntry {
    Category(StorageCategoryId),
    /// `profiles/` or `orgs/`: profile store roots, this one and others.
    ProfileContainer {
        depth: u8,
    },
}

fn device_entry(name: &str) -> DeviceEntry {
    use StorageCategoryId::*;
    DeviceEntry::Category(match name {
        "profiles" => return DeviceEntry::ProfileContainer { depth: 1 },
        "orgs" => return DeviceEntry::ProfileContainer { depth: 2 },
        TURN_SNAPSHOTS_DIR => TurnSnapshots,
        "model-cache" => ModelCache,
        "extensions" => ExtensionPackages,
        "logs" => Logs,
        "repos" => Repositories,
        "updates" => UpdateDownloads,
        "uploads" => LegacyAttachments,
        name if name.starts_with(BACKUP_STAGING_PREFIX) => BackupStaging,
        name if name.starts_with("usage-") || name.starts_with(".luna-usage-") => UsageCache,
        "device-id"
        | "device-id.lock"
        | "engine.lock"
        | "local-profile.json"
        | "legacy-uploads-owner.json"
        | "local-import.json"
        | "harness-prefs.json"
        | "repos.json"
        | "ui-settings.json"
        | "composer-defaults.json"
        | "new-thread-backgrounds"
        | "session.json"
        | "credentials"
        | "agent-accounts"
        | "provider-auth"
        | "provider-account-state-v1.json" => DeviceSettings,
        _ => Unrecognized,
    })
}

fn profile_entry(name: &str) -> StorageCategoryId {
    use StorageCategoryId::*;
    match name {
        "journals" => RunJournals,
        "uploads" => Attachments,
        "terminals" => TerminalHistory,
        "extensions" => ExtensionData,
        name if name.starts_with("docs.sqlite3") => WorkspaceDatabase,
        _ => ProfileState,
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `~`-abbreviated, for a location outside the data folder.
fn display_path(path: &Path) -> String {
    let home = crate::repos::home_dir();
    if home.parent().is_some()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

/// Relative to the data folder when inside it, else `~`-abbreviated.
fn location_in(device: &Path, path: &Path) -> String {
    match path.strip_prefix(device) {
        Ok(rest) if !rest.as_os_str().is_empty() => rest.display().to_string(),
        _ => display_path(path),
    }
}

/// The sibling managed backup destination (`backup.rs`
/// `managed_destination_path`): `<data folder>.backups`.
fn managed_backups(device: &Path) -> Option<PathBuf> {
    let mut name = device.file_name()?.to_os_string();
    name.push(".backups");
    Some(device.parent()?.join(name))
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Every path to measure, in measuring order (category order, then listing
/// order), plus the other-profile folders found on the way.
struct Plan {
    work: Vec<(StorageCategoryId, PathBuf)>,
    excluded_profiles: u32,
    /// Listing failures of a container, charged to a category.
    errors: Vec<(StorageCategoryId, std::io::Error)>,
}

impl Plan {
    fn push(&mut self, id: StorageCategoryId, path: PathBuf) {
        self.work.push((id, path));
    }

    /// Children of a `profiles/` (`depth` 1) or `orgs/` (`depth` 2) folder:
    /// this profile is measured on its own, other profile folders are only
    /// counted, and stray files are unrecognized.
    fn profile_container(&mut self, container: &Path, depth: u8, profile: &Path) {
        let listing = match std::fs::read_dir(container) {
            Ok(listing) => listing,
            Err(error) => {
                self.errors.push((StorageCategoryId::Unrecognized, error));
                return;
            }
        };
        for entry in listing {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.errors.push((StorageCategoryId::Unrecognized, error));
                    continue;
                }
            };
            let path = entry.path();
            if path == profile {
                continue;
            }
            let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir());
            if !is_dir {
                self.push(StorageCategoryId::Unrecognized, path);
            } else if depth > 1 {
                self.profile_container(&path, depth - 1, profile);
            } else {
                self.excluded_profiles = self.excluded_profiles.saturating_add(1);
            }
        }
    }
}

fn scan(roots: &StorageRoots, budget: Budget) -> StorageUsage {
    let started_at_ms = crate::now_ms();
    let started = Instant::now();
    let device = canonical(&roots.device);
    let profile = canonical(&roots.profile);
    let worktrees = canonical(&roots.worktrees);
    let mut plan = Plan {
        work: Vec::new(),
        excluded_profiles: 0,
        errors: Vec::new(),
    };

    // This profile first: small and the most relevant.
    match std::fs::read_dir(&profile) {
        Ok(listing) => {
            for entry in listing {
                match entry {
                    Ok(entry) => {
                        let path = entry.path();
                        if path == worktrees {
                            continue;
                        }
                        plan.push(profile_entry(&name_of(&path)), path);
                    }
                    Err(error) => plan.errors.push((StorageCategoryId::ProfileState, error)),
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => plan.errors.push((StorageCategoryId::ProfileState, error)),
    }

    // The data folder's own entries; this profile and the worktree root are
    // measured as their own categories.
    if device != profile {
        match std::fs::read_dir(&device) {
            Ok(listing) => {
                for entry in listing {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            plan.errors.push((StorageCategoryId::Unrecognized, error));
                            continue;
                        }
                    };
                    let path = entry.path();
                    if path == profile || path == worktrees {
                        continue;
                    }
                    let name = name_of(&path);
                    match device_entry(&name) {
                        DeviceEntry::Category(id) => plan.push(id, path),
                        DeviceEntry::ProfileContainer { depth } => {
                            if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                                plan.profile_container(&path, depth, &profile);
                            } else {
                                plan.push(StorageCategoryId::Unrecognized, path);
                            }
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => plan.errors.push((StorageCategoryId::Unrecognized, error)),
        }
    }

    // Outside the data folder: the managed backup destination LUNA marked,
    // then the worktree root (measured last; it can be the largest).
    if let Some(backups) = managed_backups(&device)
        && std::fs::symlink_metadata(backups.join(BACKUP_DESTINATION_MARKER))
            .is_ok_and(|meta| meta.is_file())
    {
        plan.push(StorageCategoryId::Backups, backups);
    }
    // A worktree root that contains the data folder (a misconfigured `~` or
    // `/`) would walk everything around it; it is not measured.
    let worktrees_exists =
        std::fs::symlink_metadata(&worktrees).is_ok() && !device.starts_with(&worktrees);
    if worktrees_exists {
        plan.push(StorageCategoryId::Worktrees, worktrees.clone());
    }

    // Stable: category order, then listing order within a category.
    plan.work.sort_by_key(|(id, _)| *id);

    let mut tallies: std::collections::BTreeMap<StorageCategoryId, Tally> = CATEGORIES
        .iter()
        .map(|(id, _)| (*id, Tally::default()))
        .collect();
    for (id, error) in &plan.errors {
        if let Some(tally) = tallies.get_mut(id) {
            tally.note_error(error);
            // A listing that failed is a place that exists but wasn't read.
            tally.seen = true;
            tally.planned = tally.planned.max(1);
            tally.started = tally.started.max(1);
            tally.finished = tally.finished.max(1);
        }
    }
    for (id, path) in &plan.work {
        let tally = tallies.entry(*id).or_default();
        tally.planned += 1;
        if *id == StorageCategoryId::Unrecognized && tally.items.len() < MAX_ITEMS {
            tally.items.push(location_in(&device, path));
        }
    }

    let mut skip = vec![profile.clone()];
    if worktrees_exists {
        skip.push(worktrees.clone());
    }
    let mut walker = Walker {
        budget,
        entries: 0,
        stop: None,
        skip,
    };
    for (id, path) in &plan.work {
        if walker.stop.is_some() {
            break;
        }
        let tally = tallies.entry(*id).or_default();
        walker.measure(path, tally);
    }

    let profile_location = location_in(&device, &profile);
    let categories: Vec<StorageCategory> = CATEGORIES
        .iter()
        .map(|(id, scope)| {
            let tally = tallies.remove(id).unwrap_or_default();
            let (scope, location) = match id {
                StorageCategoryId::Worktrees => (
                    if worktrees.starts_with(&device) {
                        StorageScope::Device
                    } else {
                        StorageScope::Shared
                    },
                    location_in(&device, &worktrees),
                ),
                StorageCategoryId::Backups => (
                    *scope,
                    managed_backups(&device)
                        .map(|path| display_path(&path))
                        .unwrap_or_default(),
                ),
                _ => (*scope, category_location(*id, &profile_location)),
            };
            StorageCategory {
                id: *id,
                scope,
                location,
                state: tally.state(),
                bytes: tally.bytes,
                files: tally.files,
                directories: tally.directories,
                unreadable: tally.unreadable,
                first_error: tally.first_error,
                items: tally.items,
            }
        })
        .collect();
    let total_bytes = categories
        .iter()
        .fold(0u64, |total, category| total.saturating_add(category.bytes));
    StorageUsage {
        root: display_path(&device),
        profile_location,
        categories,
        excluded: (plan.excluded_profiles > 0)
            .then_some(StorageExclusion {
                reason: StorageExclusionReason::OtherProfile,
                count: plan.excluded_profiles,
            })
            .into_iter()
            .collect(),
        total_bytes,
        entries: walker.entries,
        complete: walker.stop.is_none(),
        stop: walker.stop,
        started_at_ms,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

/// Where a category lives, for display. Empty when it is a set of names
/// with no single place (settings files, unrecognized items).
fn category_location(id: StorageCategoryId, profile: &str) -> String {
    use StorageCategoryId::*;
    match id {
        WorkspaceDatabase => format!("{profile}/docs.sqlite3*"),
        RunJournals => format!("{profile}/journals"),
        Attachments => format!("{profile}/uploads"),
        TerminalHistory => format!("{profile}/terminals"),
        ExtensionData => format!("{profile}/extensions"),
        ProfileState => profile.to_owned(),
        TurnSnapshots => TURN_SNAPSHOTS_DIR.to_owned(),
        ModelCache => "model-cache".into(),
        UsageCache => "usage-*".into(),
        ExtensionPackages => "extensions".into(),
        Logs => "logs".into(),
        Repositories => "repos".into(),
        UpdateDownloads => "updates".into(),
        LegacyAttachments => "uploads".into(),
        BackupStaging => format!("{BACKUP_STAGING_PREFIX}*"),
        DeviceSettings | Unrecognized | Backups | Worktrees | Unknown => String::new(),
    }
}
