//! L17 offline backup core. Live app roots may be archived only after shutdown
//! under the instance lock; restore activation remains isolated-copy only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use chrono::{Datelike, Utc};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use getrandom::fill as random_fill;
use luna_doc::{
    KIND_CHATS, KIND_SPACES, PendingBatch, REGISTRY_DOC_ID, RegistryDoc, RegistryRow, SessionDoc,
    apply_op,
};
use luna_proto::{ActivityEnvelope, AgentEvent, TaskCommandEvent};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, TempDir};
use zeroize::Zeroize;

use crate::instance_lock::InstanceLock;
use crate::run_journal::journal_paths;

const MAGIC: &[u8; 8] = b"LUNABK01";
const ISOLATION_MARKER: &str = ".luna-l17-isolated-root";
const ISOLATION_VALUE: &str = "L17 synthetic or copy-isolated root\n";
const DESTINATION_MARKER: &str = ".luna-l17-isolated-backups";
const DESTINATION_VALUE: &str = "L17 synthetic or copy-isolated backup destination\n";
const MANAGED_DESTINATION_VALUE: &str = "LUNA private offline backup destination\n";
const MANAGED_RESTORE_JOURNAL: &str = ".luna-l17-managed-restore.json";
const MAX_MANIFEST: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const CHUNK: usize = 64 * 1024;
// Review new DocsStore migrations before raising this ceiling. v10/v11 keep
// owned cleanup checkpoints; v12 keeps exact local executable authority in
// the copied SQLite snapshot. v9 Appshot sources still rebuild the activity index.
// v13 adds device-owned project commands and their execution receipts.
// v14 adds local side-chat metadata, transcript pages and send admission records.
// v15 keeps workspace repair authority; v16 adds local prompt stash records.
// v17 keeps the original task defaults in local execution lineage. Preserve
// that immutable snapshot so recovery still detects later configuration edits.
const CURRENT_SCHEMA: i64 = 17;

// Serialized legacy labels are part of archive validation. The credentials/
// directory, including the new device-local key, remains outside the allowlist.
const EXCLUDED: &[&str] = &[
    "OS Keychain and LUNA credential vault",
    "provider-native stores and authentication homes",
    "active remote grants and tokens",
    "managed worktrees, arbitrary workspaces and build outputs",
    "terminal scrollback and logs",
    "derived activity/search/usage indexes and caches",
    "pending uploads and disposable previews",
    "device identity and live engine lock",
];

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("backup root or destination is not authorized for this operation")]
    UnauthorizedRoot,
    #[error("backup root has an active engine")]
    EngineRunning,
    #[error("archive is invalid or incompatible: {0}")]
    Invalid(String),
    #[error("archive authentication or recovery key failed")]
    Authentication,
    #[error("source changed while backup was being written: {0}")]
    SourceChanged(String),
    #[error("restore requires manual resolution: {0}")]
    Unsupported(String),
}

type Result<T> = std::result::Result<T, BackupError>;

/// A test/copy root that has been explicitly prepared outside normal LUNA use.
/// The marker is intentionally not created by this API, so a caller cannot
/// accidentally turn `~/.luna` into an eligible backup or restore target.
#[derive(Debug, Clone)]
pub struct IsolatedRoot(PathBuf);

impl IsolatedRoot {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(BackupError::UnauthorizedRoot);
        }
        let root = path.canonicalize()?;
        if !root.is_dir()
            || !fs::symlink_metadata(root.join(ISOLATION_MARKER))?
                .file_type()
                .is_file()
            || fs::read_to_string(root.join(ISOLATION_MARKER))
                .ok()
                .as_deref()
                != Some(ISOLATION_VALUE)
        {
            return Err(BackupError::UnauthorizedRoot);
        }
        Ok(Self(root))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    fn lock(&self) -> Result<InstanceLock> {
        if InstanceLock::holder(&self.0).is_some() {
            return Err(BackupError::EngineRunning);
        }
        InstanceLock::acquire(&self.0).map_err(|_| BackupError::EngineRunning)
    }
}

/// The caller owns key recovery. This API neither probes Keychain nor reads
/// provider state. Key bytes are zeroized on drop.
pub struct RecoveryKey([u8; 32]);

impl RecoveryKey {
    /// Accept bytes only after a higher-level owner/credential broker has
    /// explicitly authorized them. No filesystem or Keychain lookup occurs.
    pub fn from_authorized_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn injected(bytes: [u8; 32]) -> Self {
        Self::from_authorized_bytes(bytes)
    }
}

impl Drop for RecoveryKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackupEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub category: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseCounts {
    pub path: String,
    pub schema_version: i64,
    pub documents: i64,
    pub task_events: i64,
    pub draft_records: i64,
    pub attachment_owners: i64,
    #[serde(default)]
    pub cleanup_tasks: Option<i64>,
    #[serde(default)]
    pub cleanup_projects: Option<i64>,
    #[serde(default)]
    pub cleanup_paths: Option<i64>,
    #[serde(default)]
    pub local_executable_authority: Option<i64>,
    pub derived_events_discarded: i64,
    #[serde(default)]
    pub projects: Option<i64>,
    #[serde(default)]
    pub tasks: Option<i64>,
    #[serde(default)]
    pub attachment_blobs: Option<i64>,
    #[serde(default)]
    pub activity_events_expected: Option<i64>,
    #[serde(default)]
    pub derived_rebuild_blocker: Option<String>,
    #[serde(default)]
    pub activation_blocker: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackupManifest {
    pub format_version: u32,
    pub app_version: String,
    pub created_at_ms: i64,
    pub source_environment_id: Option<String>,
    pub source_root: String,
    pub entries: Vec<BackupEntry>,
    pub databases: Vec<DatabaseCounts>,
    pub total_bytes: u64,
    pub excluded_categories: Vec<String>,
    pub previous_backup_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupReceipt {
    pub archive: PathBuf,
    pub archive_sha256: String,
    pub manifest: BackupManifest,
}

/// Default verified-backup selection: newest backup for seven distinct days,
/// then newest backup for four distinct older ISO weeks. The caller owns any
/// actual retention deletion and must keep an independently verified prior
/// backup if a new run fails.
pub fn retention_candidates(receipts: &[BackupReceipt]) -> Vec<PathBuf> {
    let mut sorted: Vec<_> = receipts.iter().collect();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.manifest.created_at_ms));
    let mut days = BTreeSet::new();
    let mut daily_weeks = BTreeSet::new();
    let mut weekly_weeks = BTreeSet::new();
    let mut prune = Vec::new();
    for receipt in sorted {
        let millis = receipt.manifest.created_at_ms;
        let day = millis.div_euclid(86_400_000);
        if days.contains(&day) {
            prune.push(receipt.archive.clone());
            continue;
        }
        if days.len() < 7 {
            days.insert(day);
            if let Some(date) = chrono::DateTime::<Utc>::from_timestamp_millis(millis) {
                daily_weeks.insert((date.iso_week().year(), date.iso_week().week()));
            }
            continue;
        }
        let week = chrono::DateTime::<Utc>::from_timestamp_millis(millis)
            .map(|date| (date.iso_week().year(), date.iso_week().week()));
        if let Some(week) = week
            && !daily_weeks.contains(&week)
            && weekly_weeks.len() < 4
            && weekly_weeks.insert(week)
        {
            continue;
        }
        prune.push(receipt.archive.clone());
    }
    prune
}

#[derive(Debug, Clone)]
pub struct RestorePreview {
    pub manifest: BackupManifest,
    pub required_bytes: u64,
    pub conflicts: Vec<String>,
    pub unsupported: Vec<String>,
}

#[derive(Debug)]
pub struct StagedRestore {
    pub path: PathBuf,
    pub target: PathBuf,
    pub preview: RestorePreview,
    baseline: String,
    staged_fingerprint: String,
}

/// Persisted proof for rechecking a staged copy after the control process
/// restarts. It contains fingerprints and the manifest claim, never key bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StagedEvidence {
    archive: PathBuf,
    archive_sha256: String,
    stage: PathBuf,
    staged_fingerprint: String,
    target_baseline: String,
    manifest_sha256: String,
}

impl StagedRestore {
    pub(crate) fn evidence(&self, archive: &Path) -> Result<StagedEvidence> {
        ensure_real_file(archive)?;
        Ok(StagedEvidence {
            archive: archive.canonicalize()?,
            archive_sha256: hash_file(archive)?,
            stage: self.path.clone(),
            staged_fingerprint: self.staged_fingerprint.clone(),
            target_baseline: self.baseline.clone(),
            manifest_sha256: hash_bytes(&serde_json::to_vec(&self.preview.manifest)?),
        })
    }
}

#[derive(Debug)]
pub struct RestoreReceipt {
    pub active_root: PathBuf,
    pub rollback_root: PathBuf,
    pub source_manifest: BackupManifest,
    active_fingerprint: String,
    old_fingerprint: String,
}

/// Offline managed restore leaves the previous allowlisted files in this
/// private sibling directory. No credential or provider-owned file is moved.
#[derive(Debug)]
pub struct ManagedRestoreReceipt {
    pub rollback_dir: PathBuf,
    pub restored_entries: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedRestoreJournal {
    version: u32,
    root: PathBuf,
    rollback_dir: PathBuf,
    entries: Vec<ManagedRestoreEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedRestoreEntry {
    path: String,
    existed: bool,
}

pub struct BackupService {
    root: IsolatedRoot,
    codec: ArchiveCodec,
    managed_offline: bool,
}

enum ArchiveCodec {
    Encrypted(RecoveryKey),
    BbStylePlain,
}

struct SourceEntry {
    manifest: BackupEntry,
    source: PathBuf,
}

impl BackupService {
    pub fn new(root: IsolatedRoot, key: RecoveryKey) -> Self {
        Self {
            root,
            codec: ArchiveCodec::Encrypted(key),
            managed_offline: false,
        }
    }

    /// BB-style private gzip/tar archive. Entries are checksummed but not
    /// encrypted; caller must keep the archive in a private destination.
    pub fn new_bb_style(root: IsolatedRoot) -> Self {
        Self {
            root,
            codec: ArchiveCodec::BbStylePlain,
            managed_offline: false,
        }
    }

    /// The app-owned root may be backed up only after its engine has stopped.
    /// The actual export acquires InstanceLock; this constructor does not make
    /// the root eligible for restore activation or bypass that lock.
    pub(crate) fn new_bb_style_managed_offline(root: &Path) -> Result<Self> {
        if !root.is_absolute() || is_provider_private_root(root) {
            return Err(BackupError::UnauthorizedRoot);
        }
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.file_type().is_dir() {
            return Err(BackupError::UnauthorizedRoot);
        }
        let canonical = root.canonicalize()?;
        if is_provider_private_root(&canonical) || read_device_id(&canonical)?.is_none() {
            return Err(BackupError::UnauthorizedRoot);
        }
        Ok(Self {
            root: IsolatedRoot(canonical),
            codec: ArchiveCodec::BbStylePlain,
            managed_offline: true,
        })
    }

    pub(crate) fn inspect(&self, path: &Path) -> Result<BackupManifest> {
        match &self.codec {
            ArchiveCodec::Encrypted(key) => inspect_archive(path, key),
            ArchiveCodec::BbStylePlain => inspect_bb_style_archive(path),
        }
    }

    fn archive_suffix(&self) -> &'static str {
        match &self.codec {
            ArchiveCodec::Encrypted(_) => "lunabk",
            ArchiveCodec::BbStylePlain => "tar.gz",
        }
    }

    /// Run the daily BB-style backup policy only while no engine owns this
    /// LUNA root. The sibling destination is private and never inside the
    /// source inventory. This API does not activate restores.
    pub fn run_managed_offline_if_due(root: &Path, now_ms: i64) -> Result<Option<BackupReceipt>> {
        let service = Self::new_bb_style_managed_offline(root)?;
        let _lock = service.root.lock()?;
        ensure_no_pending_managed_restore(service.root.path())?;
        let destination = managed_destination(service.root.path())?;
        let last = service.latest_managed_receipt(&destination)?;
        service.backup_if_due_locked(&destination, now_ms, last.as_ref())
    }

    /// Explicit offline export for `luna backup now`. It refuses an active
    /// engine and links to the newest verified archive without deleting it.
    pub fn run_managed_offline_now(root: &Path, now_ms: i64) -> Result<BackupReceipt> {
        let service = Self::new_bb_style_managed_offline(root)?;
        let _lock = service.root.lock()?;
        ensure_no_pending_managed_restore(service.root.path())?;
        let destination = managed_destination(service.root.path())?;
        let last = service.latest_managed_receipt(&destination)?;
        let name = format!("luna-{}-{}.tar.gz", now_ms, uuid::Uuid::new_v4());
        service.export_at_locked(
            &destination.join(name),
            now_ms,
            last.map(|receipt| receipt.archive_sha256),
        )
    }

    /// Read-only, stopped-root compatibility report for a local LUNA archive.
    /// A copied root or foreign archive remains unsupported; this never activates data.
    pub fn preview_managed_offline(root: &Path, archive: &Path) -> Result<RestorePreview> {
        let service = Self::new_bb_style_managed_offline(root)?;
        let _lock = service.root.lock()?;
        ensure_no_pending_managed_restore(service.root.path())?;
        let manifest = service.inspect(archive)?;
        let root = service.root.path();
        let mut unsupported = Vec::new();
        if manifest.source_root != root.display().to_string() {
            unsupported
                .push("absolute attachment references require the original data-root path".into());
        }
        if manifest.source_environment_id != read_device_id(root)? {
            unsupported.push("source and target device identities differ".into());
        }
        for db in &manifest.databases {
            if let Some(reason) = &db.derived_rebuild_blocker {
                unsupported.push(format!("{}: {reason}", db.path));
            } else if db.activity_events_expected.is_none() {
                unsupported.push(format!("{}: activity event count is unavailable", db.path));
            }
            if let Some(reason) = &db.activation_blocker {
                unsupported.push(format!("{}: {reason}", db.path));
            }
        }
        let mut conflicts = Vec::new();
        for entry in &manifest.entries {
            validate_managed_restore_path(&entry.path)?;
            verify_existing_parent_chain(root, &entry.path)?;
            let target = root.join(&entry.path);
            match fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.file_type().is_file() => {
                    conflicts.push(entry.path.clone())
                }
                Ok(_) => unsupported.push(format!("unsafe restore target: {}", entry.path)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(RestorePreview {
            required_bytes: manifest.total_bytes.saturating_mul(2),
            manifest,
            conflicts,
            unsupported,
        })
    }

    /// Read-only status for the running app. Archives are atomically published
    /// while the engine is stopped, so this does not acquire its live lock.
    pub fn managed_offline_status(root: &Path) -> Result<(PathBuf, Option<BackupReceipt>)> {
        let service = Self::new_bb_style_managed_offline(root)?;
        ensure_no_pending_managed_restore(service.root.path())?;
        let destination = managed_destination_path(service.root.path())?;
        if !destination.exists() {
            return Ok((destination, None));
        }
        validate_managed_destination(&destination)?;
        let last = service.latest_managed_receipt(&destination)?;
        Ok((destination, last))
    }

    /// BB-style offline import: stage and verify first, then replace only the
    /// allowlisted data files. Existing files stay in a private sibling
    /// rollback directory. Credentials, identity and provider stores stay put.
    pub fn restore_managed_offline(root: &Path, archive: &Path) -> Result<ManagedRestoreReceipt> {
        Self::restore_managed_offline_with_readback(root, archive, |_| Ok(()))
    }

    /// Synthetic failure seam for verifying the journaled rollback path.
    #[doc(hidden)]
    pub fn restore_managed_offline_with_readback_for_tests<F>(
        root: &Path,
        archive: &Path,
        readback: F,
    ) -> Result<ManagedRestoreReceipt>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        Self::restore_managed_offline_with_readback(root, archive, readback)
    }

    fn restore_managed_offline_with_readback<F>(
        root: &Path,
        archive: &Path,
        readback: F,
    ) -> Result<ManagedRestoreReceipt>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        let service = Self::new_bb_style_managed_offline(root)?;
        let root = service.root.path();
        let _lock = service.root.lock()?;
        let journal_path = root.join(MANAGED_RESTORE_JOURNAL);
        if fs::symlink_metadata(&journal_path).is_ok() {
            return Err(BackupError::Unsupported(
                "an interrupted restore must be recovered before retrying".into(),
            ));
        }
        let manifest = service.inspect(archive)?;
        if manifest.source_root != root.display().to_string()
            || manifest.source_environment_id != read_device_id(root)?
            || manifest.databases.iter().any(|db| {
                db.derived_rebuild_blocker.is_some()
                    || db.activation_blocker.is_some()
                    || db.activity_events_expected.is_none()
            })
        {
            return Err(BackupError::Unsupported(
                "archive root, device identity or staged rebuild contract differs".into(),
            ));
        }
        let parent = root
            .parent()
            .ok_or_else(|| BackupError::Invalid("root has no parent".into()))?;
        let required = manifest
            .total_bytes
            .saturating_mul(2)
            .saturating_add(64 * 1024 * 1024);
        if available_space(parent)? < required {
            return Err(BackupError::Unsupported(format!(
                "offline restore needs at least {required} free bytes for staging and rollback"
            )));
        }
        let stage = tempfile::Builder::new()
            .prefix(".luna-l17-restore-stage-")
            .tempdir_in(parent)?;
        let extracted = extract_bb_style_archive(archive, stage.path())?;
        if extracted != manifest {
            return Err(BackupError::SourceChanged(
                "archive changed during staging".into(),
            ));
        }
        ensure_real_file(&root.join("device-id"))?;
        fs::copy(root.join("device-id"), stage.path().join("device-id"))?;
        verify_stage_base(stage.path(), &manifest, true)?;
        rebuild_stage_activity(stage.path(), &manifest)?;
        verify_stage(stage.path(), &manifest)?;

        // Discover only the same owned paths as export. The temporary SQLite
        // snapshots are read-only preflight material and are discarded before
        // the first target file is moved.
        let snapshots = tempfile::Builder::new()
            .prefix(".luna-l17-restore-inventory-")
            .tempdir_in(root)?;
        let (old_entries, _) = inventory(root, &snapshots)?;
        drop(snapshots);
        let mut paths = BTreeSet::new();
        let mut database_paths = BTreeSet::new();
        for entry in old_entries {
            if entry.manifest.category == "database" {
                database_paths.insert(entry.manifest.path.clone());
            }
            paths.insert(entry.manifest.path);
        }
        for entry in &manifest.entries {
            if entry.category == "database" {
                database_paths.insert(entry.path.clone());
            }
            paths.insert(entry.path.clone());
        }
        for db in database_paths {
            for suffix in ["-wal", "-shm", "-journal"] {
                let sidecar = format!("{db}{suffix}");
                if fs::symlink_metadata(root.join(&sidecar)).is_ok() {
                    paths.insert(sidecar);
                }
            }
        }
        let mut entries = Vec::with_capacity(paths.len());
        for path in paths {
            validate_managed_restore_path(&path)?;
            verify_existing_parent_chain(root, &path)?;
            let target = root.join(&path);
            let existed = match fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.file_type().is_file() => true,
                Ok(_) => {
                    return Err(BackupError::Invalid(format!(
                        "unsafe restore target: {path}"
                    )));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.into()),
            };
            entries.push(ManagedRestoreEntry { path, existed });
        }
        let rollback_dir = parent.join(format!(
            ".luna-l17-managed-rollback-{}",
            uuid::Uuid::new_v4()
        ));
        create_private_dir(&rollback_dir)?;
        let journal = ManagedRestoreJournal {
            version: 1,
            root: root.to_path_buf(),
            rollback_dir: rollback_dir.clone(),
            entries,
        };
        write_managed_restore_journal(&journal_path, &journal)?;
        let install = (|| -> Result<()> {
            for entry in &journal.entries {
                let target = root.join(&entry.path);
                let previous = rollback_dir.join(&entry.path);
                if entry.existed {
                    create_private_parents(&rollback_dir, &entry.path)?;
                    fs::rename(&target, &previous)?;
                    sync_parent(previous.parent().unwrap())?;
                }
                let staged = stage.path().join(&entry.path);
                if staged.exists() {
                    ensure_real_file(&staged)?;
                    create_private_parents(root, &entry.path)?;
                    fs::rename(&staged, &target)?;
                }
                sync_parent(target.parent().unwrap())?;
            }
            verify_stage(root, &manifest)?;
            readback(root)?;
            sync_parent(&rollback_dir)?;
            Ok(())
        })();
        if let Err(error) = install {
            recover_managed_restore_locked(root, &journal_path, &journal)?;
            return Err(error);
        }
        fs::remove_file(&journal_path)?;
        sync_parent(root)?;
        Ok(ManagedRestoreReceipt {
            rollback_dir,
            restored_entries: manifest.entries.len(),
        })
    }

    /// Resume rollback after a process failure. This never deletes the private
    /// previous-file directory; it moves those exact files back into place.
    pub fn recover_managed_restore(root: &Path) -> Result<bool> {
        let service = Self::new_bb_style_managed_offline(root)?;
        let root = service.root.path();
        let _lock = service.root.lock()?;
        let journal_path = root.join(MANAGED_RESTORE_JOURNAL);
        if fs::symlink_metadata(&journal_path).is_ok() {
            ensure_real_file(&journal_path)?;
        }
        let journal = match fs::read(&journal_path) {
            Ok(bytes) => serde_json::from_slice::<ManagedRestoreJournal>(&bytes)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        recover_managed_restore_locked(root, &journal_path, &journal)?;
        Ok(true)
    }

    fn latest_managed_receipt(&self, destination: &Path) -> Result<Option<BackupReceipt>> {
        let mut archives = fs::read_dir(destination)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        archives.retain(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("luna-") && name.ends_with(".tar.gz"))
        });
        archives.sort();
        let last = if let Some(path) = archives.last() {
            ensure_real_file(path)?;
            let manifest = self.inspect(path)?;
            if manifest.source_root != self.root.path().display().to_string() {
                return Err(BackupError::SourceChanged(
                    "latest backup belongs to another LUNA root".into(),
                ));
            }
            Some(BackupReceipt {
                archive: path.clone(),
                archive_sha256: hash_file(path)?,
                manifest,
            })
        } else {
            None
        };
        Ok(last)
    }

    /// Manual export to a caller-selected destination. Existing
    /// archives are never overwritten; successful output is reopen-verified.
    pub fn export(&self, destination: &Path) -> Result<BackupReceipt> {
        self.export_at(destination, now_ms(), None)
    }

    fn export_at(
        &self,
        destination: &Path,
        created_at_ms: i64,
        previous_backup_sha256: Option<String>,
    ) -> Result<BackupReceipt> {
        let _lock = self.root.lock()?;
        self.export_at_locked(destination, created_at_ms, previous_backup_sha256)
    }

    fn export_at_locked(
        &self,
        destination: &Path,
        created_at_ms: i64,
        previous_backup_sha256: Option<String>,
    ) -> Result<BackupReceipt> {
        let snapshot_dir = tempfile::Builder::new()
            .prefix(".luna-l17-snapshot-")
            .tempdir_in(self.root.path())?;
        let (entries, databases) = inventory(self.root.path(), &snapshot_dir)?;
        self.write_archive(
            destination,
            entries,
            databases,
            created_at_ms,
            previous_backup_sha256,
        )
    }

    /// Manual backup in an isolated destination directory. Automatic callers
    /// can use [`Self::backup_if_due`] with the same verified output path.
    pub fn backup_now(&self, destination_dir: &Path, now_ms: i64) -> Result<BackupReceipt> {
        ensure_isolated_destination(destination_dir)?;
        let name = format!(
            "luna-{}-{}.{}",
            now_ms,
            uuid::Uuid::new_v4(),
            self.archive_suffix()
        );
        self.export_at(&destination_dir.join(name), now_ms, None)
    }

    /// One daily backup after source change. This is a policy entry point, not
    /// a background scheduler; the host must call it after meaningful writes.
    pub fn backup_if_due(
        &self,
        destination_dir: &Path,
        now_ms: i64,
        last_success: Option<&BackupReceipt>,
    ) -> Result<Option<BackupReceipt>> {
        let _lock = self.root.lock()?;
        self.backup_if_due_locked(destination_dir, now_ms, last_success)
    }

    fn backup_if_due_locked(
        &self,
        destination_dir: &Path,
        now_ms: i64,
        last_success: Option<&BackupReceipt>,
    ) -> Result<Option<BackupReceipt>> {
        if let Some(previous) = last_success {
            if hash_file(&previous.archive)? != previous.archive_sha256
                || self.inspect(&previous.archive)? != previous.manifest
            {
                return Err(BackupError::Invalid(
                    "previous backup receipt is not verified".into(),
                ));
            }
            if now_ms.saturating_sub(previous.manifest.created_at_ms) < 86_400_000 {
                return Ok(None);
            }
            let latest_source = latest_authoritative_mtime(self.root.path())?;
            if latest_source <= previous.manifest.created_at_ms {
                return Ok(None);
            }
        }
        ensure_isolated_destination(destination_dir)?;
        let name = format!(
            "luna-{}-{}.{}",
            now_ms,
            uuid::Uuid::new_v4(),
            self.archive_suffix()
        );
        self.export_at_locked(
            &destination_dir.join(name),
            now_ms,
            last_success.map(|v| v.archive_sha256.clone()),
        )
        .map(Some)
    }

    fn write_archive(
        &self,
        destination: &Path,
        entries: Vec<SourceEntry>,
        databases: Vec<DatabaseCounts>,
        created_at_ms: i64,
        previous_backup_sha256: Option<String>,
    ) -> Result<BackupReceipt> {
        let parent = destination
            .parent()
            .ok_or_else(|| BackupError::Invalid("destination has no parent".into()))?;
        ensure_isolated_destination(parent)?;
        if destination.exists() {
            return Err(BackupError::Invalid(
                "destination is a symlink or already exists".into(),
            ));
        }
        let total_bytes = entries
            .iter()
            .try_fold(0u64, |sum, e| sum.checked_add(e.manifest.size))
            .ok_or_else(|| BackupError::Invalid("archive size overflow".into()))?;
        let device_id = self.root.path().join("device-id");
        let source_environment_id = if device_id.exists() {
            ensure_real_file(&device_id)?;
            let value = fs::read_to_string(&device_id)?;
            let value = value.trim();
            if value.len() > 128
                || !value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            {
                return Err(BackupError::Invalid("device identity is malformed".into()));
            }
            (!value.is_empty()).then(|| value.to_owned())
        } else {
            None
        };
        let manifest = BackupManifest {
            format_version: match &self.codec {
                ArchiveCodec::Encrypted(_) => 2,
                ArchiveCodec::BbStylePlain => 3,
            },
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            created_at_ms,
            source_environment_id,
            source_root: self.root.path().display().to_string(),
            entries: entries.iter().map(|e| e.manifest.clone()).collect(),
            databases,
            total_bytes,
            excluded_categories: EXCLUDED.iter().map(|s| (*s).to_owned()).collect(),
            previous_backup_sha256,
        };
        validate_manifest(&manifest)?;
        let json = serde_json::to_vec(&manifest)?;
        if json.len() > MAX_MANIFEST {
            return Err(BackupError::Invalid("manifest too large".into()));
        }
        let mut output = NamedTempFile::new_in(parent)?;
        match &self.codec {
            ArchiveCodec::Encrypted(key) => {
                let mut cipher = EncryptedWriter::new(output.as_file_mut(), key)?;
                cipher.write_all(&(json.len() as u32).to_be_bytes())?;
                cipher.write_all(&json)?;
                for entry in &entries {
                    let mut file = File::open(&entry.source)?;
                    let (size, digest) = copy_and_hash(&mut file, &mut cipher)?;
                    if size != entry.manifest.size || digest != entry.manifest.sha256 {
                        return Err(BackupError::SourceChanged(entry.manifest.path.clone()));
                    }
                }
                cipher.finish()?;
            }
            ArchiveCodec::BbStylePlain => {
                write_bb_style_archive(output.as_file_mut(), &json, &entries)?;
            }
        }
        output.as_file_mut().sync_all()?;
        let verified = self.inspect(output.path())?;
        if verified != manifest {
            return Err(BackupError::Invalid("reopened manifest differs".into()));
        }
        let archive_sha256 = hash_file(output.path())?;
        output
            .persist_noclobber(destination)
            .map_err(|e| BackupError::Io(e.error))?;
        sync_parent(parent)?;
        Ok(BackupReceipt {
            archive: destination.to_path_buf(),
            archive_sha256,
            manifest,
        })
    }
}

fn inventory(root: &Path, snapshots: &TempDir) -> Result<(Vec<SourceEntry>, Vec<DatabaseCounts>)> {
    let mut entries = Vec::new();
    let mut databases = Vec::new();
    for name in [
        "local-profile.json",
        "ui-settings.json",
        "harness-prefs.json",
        "repos.json",
    ] {
        add_file_if_present(root, &root.join(name), "settings", &mut entries)?;
    }
    let local = root.join("profiles/local");
    if local.exists() {
        ensure_real_dir(&root.join("profiles"))?;
        collect_profile(root, &local, snapshots, &mut entries, &mut databases)?;
    }
    let orgs = root.join("orgs");
    if orgs.exists() {
        ensure_real_dir(&orgs)?;
        for org in sorted_children(&orgs)? {
            ensure_real_dir(&org)?;
            for user in sorted_children(&org)? {
                ensure_real_dir(&user)?;
                collect_profile(root, &user, snapshots, &mut entries, &mut databases)?;
            }
        }
    }
    if databases.is_empty() {
        return Err(BackupError::Invalid(
            "no LUNA profile database found".into(),
        ));
    }
    entries.sort_by(|a, b| a.manifest.path.cmp(&b.manifest.path));
    databases.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((entries, databases))
}

fn collect_profile(
    root: &Path,
    profile: &Path,
    snapshots: &TempDir,
    entries: &mut Vec<SourceEntry>,
    databases: &mut Vec<DatabaseCounts>,
) -> Result<()> {
    ensure_real_dir(profile)?;
    let db = profile.join("docs.sqlite3");
    if !db.exists() {
        return Err(BackupError::Invalid(format!(
            "profile has no database: {}",
            profile.display()
        )));
    }
    ensure_real_file(&db)?;
    let snapshot_profile = snapshots
        .path()
        .join(format!("profile-{}", databases.len()));
    fs::create_dir(&snapshot_profile)?;
    let snapshot = snapshot_profile.join("docs.sqlite3");
    let source = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source.execute(
        "VACUUM INTO ?1",
        params![snapshot.to_string_lossy().as_ref()],
    )?;
    drop(source);
    let copy = Connection::open(&snapshot)?;
    copy.pragma_update(None, "journal_mode", "DELETE")?;
    // L14 search envelopes are derived from authoritative transcript/receipt
    // records. Their policy table remains in the snapshot for a later rebuild.
    let discarded_events = count_table(&copy, "activity_events")?;
    if has_table(&copy, "activity_events")? {
        copy.execute("DELETE FROM activity_events", [])?;
    }
    let mut counts = database_counts(&copy, rel(root, &db)?)?;
    counts.derived_events_discarded = discarded_events;
    verify_database(&copy)?;
    drop(copy);
    let local_device_id = read_device_id(root)?;
    match derive_profile(&snapshot_profile, profile, local_device_id.as_deref()) {
        Ok(projection) => {
            counts.projects = Some(projection.projects);
            counts.tasks = Some(projection.tasks);
            counts.activity_events_expected = projection.activity_count();
            counts.derived_rebuild_blocker = projection.blocker;
            if !projection.activation_blockers.is_empty() {
                counts.activation_blocker = Some(projection.activation_blockers.join("; "));
            }
            if projection.tasks == 0
                && (counts.task_events > 0 || counts.derived_events_discarded > 0)
            {
                counts.activity_events_expected = None;
                counts.derived_rebuild_blocker =
                    Some("activity or command rows have no authoritative registry task".into());
            }
        }
        Err(error) => counts.derived_rebuild_blocker = Some(error.to_string()),
    }
    add_file(root, &db, &snapshot, "database", entries)?;
    let journals = profile.join("journals");
    if journals.exists() {
        collect_tree(root, &journals, "journal", entries, true)?;
    }
    let attempts = profile.join("github-pr-attempts");
    if attempts.exists() {
        collect_tree(root, &attempts, "github-pr-attempt", entries, false)?;
    }
    let uploads = profile.join("uploads");
    if uploads.exists() {
        collect_tree(root, &uploads, "attachment", entries, false)?;
    }
    counts.attachment_blobs = Some(
        entries
            .iter()
            .filter(|entry| {
                entry.manifest.category == "attachment" && entry.source.starts_with(&uploads)
            })
            .count() as i64,
    );
    databases.push(counts);
    Ok(())
}

fn pr_lock_name(name: &str) -> bool {
    name.strip_suffix(".lock")
        .is_some_and(|key| key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit()))
}

fn collect_tree(
    root: &Path,
    dir: &Path,
    category: &str,
    entries: &mut Vec<SourceEntry>,
    journals_only: bool,
) -> Result<()> {
    ensure_real_dir(dir)?;
    for path in sorted_children(dir)? {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if category == "github-pr-attempt"
            && (name.starts_with(".pending-pr-") || pr_lock_name(name))
        {
            continue; // unpublished crash leftovers cannot authorize an external write
        }
        if !journals_only
            && name == "tmp"
            && path.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("uploads"))
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            return Err(BackupError::Invalid(format!(
                "unsafe source entry: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            collect_tree(root, &path, category, entries, journals_only)?;
        } else if !journals_only || path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            add_file(root, &path, &path, category, entries)?;
        }
    }
    Ok(())
}

fn add_file_if_present(
    root: &Path,
    path: &Path,
    category: &str,
    entries: &mut Vec<SourceEntry>,
) -> Result<()> {
    if path.exists() {
        add_file(root, path, path, category, entries)?;
    }
    Ok(())
}

fn add_file(
    root: &Path,
    logical: &Path,
    source: &Path,
    category: &str,
    entries: &mut Vec<SourceEntry>,
) -> Result<()> {
    ensure_real_file(source)?;
    if logical != source {
        ensure_real_file(logical)?;
    }
    let path = rel(root, logical)?;
    validate_entry_path(&path, category)?;
    let (size, sha256) = measure_file(source)?;
    entries.push(SourceEntry {
        manifest: BackupEntry {
            path,
            size,
            sha256,
            category: category.to_owned(),
        },
        source: source.to_path_buf(),
    });
    Ok(())
}

fn sorted_children(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(dir)?
        .map(|e| e.map(|v| v.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}

fn ensure_real_dir(path: &Path) -> Result<()> {
    let kind = fs::symlink_metadata(path)?.file_type();
    if !kind.is_dir() {
        return Err(BackupError::Invalid(format!(
            "not a regular directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_real_file(path: &Path) -> Result<()> {
    let kind = fs::symlink_metadata(path)?.file_type();
    if !kind.is_file() {
        return Err(BackupError::Invalid(format!(
            "not a regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn rel(root: &Path, path: &Path) -> Result<String> {
    let path = path
        .strip_prefix(root)
        .map_err(|_| BackupError::Invalid("path outside root".into()))?;
    Ok(path
        .to_str()
        .ok_or_else(|| BackupError::Invalid("non-UTF-8 path".into()))?
        .replace(std::path::MAIN_SEPARATOR, "/"))
}

fn has_table(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

fn count_table(conn: &Connection, table: &str) -> Result<i64> {
    if !has_table(conn, table)? {
        return Ok(0);
    }
    let sql = format!("SELECT count(*) FROM {table}");
    Ok(conn.query_row(&sql, [], |r| r.get(0))?)
}

fn count_required_table(conn: &Connection, table: &str) -> Result<i64> {
    if !has_table(conn, table)? {
        return Err(BackupError::Invalid(format!(
            "database schema requires missing table {table}"
        )));
    }
    count_table(conn, table)
}

fn database_counts(conn: &Connection, path: String) -> Result<DatabaseCounts> {
    let schema_version = if has_table(conn, "schema_migrations")? {
        conn.query_row(
            "SELECT coalesce(max(version),0) FROM schema_migrations",
            [],
            |r| r.get(0),
        )?
    } else {
        0
    };
    if schema_version > CURRENT_SCHEMA {
        return Err(BackupError::Unsupported(format!(
            "database schema {schema_version} is newer than supported {CURRENT_SCHEMA}"
        )));
    }
    // A version marker cannot make an incomplete feature schema restorable.
    // File hashes preserve its contents; validate table presence before export
    // and again when inspecting the staged database.
    for (introduced, table) in [
        (12, "local_executable_authority"),
        (13, "project_command_configs"),
        (13, "project_command_runs"),
        (14, "side_chats"),
        (14, "side_chat_entries"),
        (14, "side_chat_sends"),
        (15, "task_execution_lineage"),
        (16, "prompt_stash"),
    ] {
        if schema_version >= introduced && !has_table(conn, table)? {
            return Err(BackupError::Invalid(format!(
                "database schema requires missing table {table}"
            )));
        }
    }
    if schema_version >= 15 {
        for table in [
            "task_command_state",
            "task_command_receipts",
            "local_executable_authority",
            "task_execution_lineage",
        ] {
            let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if !columns
                .iter()
                .any(|name| name == "workspace_repair_revision")
            {
                return Err(BackupError::Invalid(format!(
                    "database schema requires missing column {table}.workspace_repair_revision"
                )));
            }
        }
    }
    if schema_version >= 17 {
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('task_execution_lineage')
             WHERE name='configuration')",
            [],
            |row| row.get(0),
        )?;
        if !present {
            return Err(BackupError::Invalid(
                "database schema requires missing column task_execution_lineage.configuration"
                    .into(),
            ));
        }
    }
    Ok(DatabaseCounts {
        path,
        schema_version,
        documents: count_table(conn, "snapshots")?,
        task_events: count_table(conn, "task_command_events")?,
        draft_records: count_table(conn, "composer_drafts")?,
        attachment_owners: count_table(conn, "attachment_owners")?,
        cleanup_tasks: (schema_version >= 10)
            .then(|| count_required_table(conn, "task_cleanup"))
            .transpose()?,
        cleanup_projects: (schema_version >= 10)
            .then(|| count_required_table(conn, "project_cleanup"))
            .transpose()?,
        cleanup_paths: (schema_version >= 10)
            .then(|| count_required_table(conn, "task_cleanup_paths"))
            .transpose()?,
        local_executable_authority: (schema_version >= 12)
            .then(|| count_required_table(conn, "local_executable_authority"))
            .transpose()?,
        derived_events_discarded: 0,
        projects: None,
        tasks: None,
        attachment_blobs: None,
        activity_events_expected: None,
        derived_rebuild_blocker: None,
        activation_blocker: None,
    })
}

fn read_device_id(root: &Path) -> Result<Option<String>> {
    let path = root.join("device-id");
    if !path.exists() {
        return Ok(None);
    }
    ensure_real_file(&path)?;
    Ok(Some(fs::read_to_string(path)?.trim().to_owned()))
}

struct ProfileProjection {
    projects: i64,
    tasks: i64,
    activity: Vec<(String, Vec<ActivityEnvelope>)>,
    blocker: Option<String>,
    activation_blockers: Vec<String>,
}

impl ProfileProjection {
    fn activity_count(&self) -> Option<i64> {
        self.blocker.is_none().then(|| {
            self.activity
                .iter()
                .map(|(_, events)| events.len() as i64)
                .sum()
        })
    }
}

/// Reconstruct only the L14 path whose full inputs are in this backup. The
/// profile argument supplies the matching journal copy; no provider home or
/// credential source is opened. Unknown lineage is an activation blocker.
fn derive_profile(
    database_profile: &Path,
    journal_profile: &Path,
    local_device_id: Option<&str>,
) -> Result<ProfileProjection> {
    let store = luna_sync::DocsStore::open(database_profile)
        .map_err(|e| BackupError::Invalid(format!("staged database open failed: {e}")))?;
    let registry_bytes = store
        .load_snapshot(REGISTRY_DOC_ID)
        .map_err(|e| BackupError::Invalid(format!("registry read failed: {e}")))?;
    let Some(registry_bytes) = registry_bytes else {
        let legacy = store
            .load_snapshot("workspace2")
            .map_err(|e| BackupError::Invalid(format!("legacy registry read failed: {e}")))?
            .is_some();
        let identity = store
            .load_snapshot("luna-task-identity-events-v1")
            .map_err(|e| BackupError::Invalid(format!("task identity read failed: {e}")))?
            .is_some();
        let journals = journal_profile.join("journals");
        let journal_records = if journals.exists() {
            ensure_real_dir(&journals)?;
            sorted_children(&journals)?
                .iter()
                .any(|path| path.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        } else {
            false
        };
        return Ok(ProfileProjection {
            projects: 0,
            tasks: 0,
            activity: Vec::new(),
            blocker: if legacy || identity || journal_records {
                Some(
                    "registry is absent while workspace, task identity, or journal records exist"
                        .into(),
                )
            } else {
                None
            },
            activation_blockers: Vec::new(),
        });
    };
    let registry = RegistryDoc::from_bytes(&registry_bytes, local_device_id.unwrap_or("backup"))
        .map_err(|e| BackupError::Invalid(format!("registry snapshot invalid: {e}")))?;
    let spaces = registry
        .read_spaces()
        .map_err(|e| BackupError::Invalid(format!("project rows invalid: {e}")))?;
    let chats = registry
        .read_chats()
        .map_err(|e| BackupError::Invalid(format!("task rows invalid: {e}")))?;
    let (raw_projects, raw_tasks) = raw_registry_counts(&registry_bytes)?;
    if spaces.len() != raw_projects || chats.len() != raw_tasks {
        return Err(BackupError::Unsupported(
            "registry skipped malformed project or task rows".into(),
        ));
    }
    let mut projection = ProfileProjection {
        projects: spaces.len() as i64,
        tasks: chats.len() as i64,
        activity: Vec::new(),
        blocker: None,
        activation_blockers: Vec::new(),
    };
    let identity_views = crate::task_identity::offline_views_for_backup(&store)
        .map_err(|e| BackupError::Unsupported(format!("task identity replay failed: {e}")))?;
    if let Some(views) = &identity_views {
        let chat_ids = chats
            .iter()
            .map(|chat| chat.id.as_str())
            .collect::<BTreeSet<_>>();
        if let Some(task_id) = views
            .iter()
            .filter(|(task_id, view)| !view.task.tombstoned && !chat_ids.contains(task_id.as_str()))
            .map(|(task_id, _)| task_id)
            .min()
        {
            projection.blocker = Some(format!(
                "active task identity {task_id} has no registry task"
            ));
            return Ok(projection);
        }
    }
    if chats.is_empty() {
        return Ok(projection);
    }
    let Some(device_id) = local_device_id.filter(|id| !id.is_empty()) else {
        projection.blocker = Some("source device identity is missing for activity rebuild".into());
        return Ok(projection);
    };
    let journal_dir = journal_profile.join("journals");
    if journal_dir.exists() {
        ensure_real_dir(&journal_dir)?;
    }
    for chat in chats {
        let snapshot = store
            .load_snapshot_with_cursor(&chat.id)
            .map_err(|e| BackupError::Invalid(format!("task snapshot read failed: {e}")))?;
        let Some((bytes, cursor, epoch)) = snapshot else {
            projection.blocker = Some(format!(
                "task {} has no staged transcript snapshot",
                chat.id
            ));
            break;
        };
        let chat2 = chat.on_chat2() || epoch >= crate::chat2_host::CHAT2_DOC_EPOCH;
        if chat2 && epoch < crate::chat2_host::CHAT2_DOC_EPOCH {
            projection.blocker = Some(format!(
                "task {} has an incomplete chat2 adoption checkpoint",
                chat.id
            ));
            break;
        }
        if !chat2 && cursor != 0 {
            projection.blocker = Some(format!(
                "task {} has an unexpected snapshot cursor",
                chat.id
            ));
            break;
        }
        let doc = SessionDoc::from_snapshot(&bytes)
            .map_err(|e| BackupError::Invalid(format!("task transcript invalid: {e}")))?;
        if chat2 {
            if !store
                .rejected_chat_updates(&chat.id)
                .map_err(|e| BackupError::Invalid(format!("chat2 update state invalid: {e}")))?
                .is_empty()
            {
                projection.blocker = Some(format!("task {} has rejected chat2 updates", chat.id));
                break;
            }
            let pending = store
                .pending_chat_updates(&chat.id)
                .map_err(|e| BackupError::Invalid(format!("chat2 outbox invalid: {e}")))?;
            for (_, update) in &pending {
                doc.doc()
                    .import(update)
                    .map_err(|e| BackupError::Invalid(format!("chat2 update invalid: {e}")))?;
            }
            projection.activation_blockers.push(format!(
                "task {} chat2 remote frontier{} needs reconciliation before activation",
                chat.id,
                if pending.is_empty() {
                    ""
                } else {
                    " and pending updates"
                }
            ));
        }
        let entries = doc
            .read_entries()
            .map_err(|e| BackupError::Invalid(format!("task entries invalid: {e}")))?;
        if entries.len() != doc.doc().get_list("messages").len() {
            projection.blocker = Some(format!(
                "task {} has unsalvageable transcript entries",
                chat.id
            ));
            break;
        }
        let (receipts, truncated) = staged_command_history(&store, &chat.id)?;
        if truncated {
            projection.blocker = Some(format!("task {} has incomplete command history", chat.id));
            break;
        }
        let policy = chat
            .space_id
            .as_deref()
            .map(|id| store.activity_project_policy(id))
            .transpose()
            .map_err(|e| BackupError::Invalid(format!("project policy read failed: {e}")))?
            .flatten();
        let metadata = if journal_dir.exists() {
            let (journal_path, _) = journal_paths(&journal_dir, &chat.id);
            read_journal_metadata(&journal_path)?
        } else {
            HashMap::new()
        };
        let identity = identity_views
            .as_ref()
            .and_then(|views| views.get(&chat.id));
        if identity.is_some_and(|view| view.current_binding.is_some()) {
            projection.activation_blockers.push(format!(
                "task {} provider mapping needs owner review before activation",
                chat.id
            ));
        }
        let mut events = crate::activity_history::build_activity_envelopes(
            device_id,
            &chat,
            identity,
            &entries,
            &receipts,
            false,
            &metadata,
            policy.as_ref(),
        );
        let appshots = store
            .appshot_activity_for_task(&chat.id)
            .map_err(|e| BackupError::Invalid(format!("Appshot activity read failed: {e}")))?;
        events.extend(crate::activity_history::build_appshot_envelopes(
            device_id,
            &chat,
            identity,
            &appshots,
            Some(&entries),
            policy.as_ref(),
        ));
        projection.activity.push((chat.id, events));
    }
    Ok(projection)
}

#[derive(Deserialize)]
struct BackupRegistryRecords {
    rows: Vec<RegistryRow>,
    pending: Vec<PendingBatch>,
}

fn raw_registry_counts(bytes: &[u8]) -> Result<(usize, usize)> {
    let records: BackupRegistryRecords = serde_json::from_slice(bytes)?;
    let mut rows = HashMap::new();
    for row in records.rows {
        rows.insert((row.kind.clone(), row.id.clone()), row);
    }
    for batch in records.pending {
        for op in batch.ops {
            let key = (op.kind.clone(), op.id.clone());
            let (next, _) = apply_op(rows.get(&key), &op);
            if let Some(next) = next {
                rows.insert(key, next);
            }
        }
    }
    Ok((
        rows.values()
            .filter(|row| row.kind == KIND_SPACES && !row.deleted)
            .count(),
        rows.values()
            .filter(|row| row.kind == KIND_CHATS && !row.deleted)
            .count(),
    ))
}

#[derive(Deserialize)]
struct BackupJournalLine {
    seq: u64,
    event: AgentEvent,
}

fn read_journal_metadata(
    path: &Path,
) -> Result<HashMap<String, crate::activity_history::ActivityRunMetadata>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    ensure_real_file(path)?;
    let bytes = fs::read(path)?;
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        return Err(BackupError::Unsupported(format!(
            "journal has an interrupted tail: {}",
            path.display()
        )));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| BackupError::Unsupported("journal is not UTF-8".into()))?;
    let mut prior = 0u64;
    let mut metadata = HashMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let record: BackupJournalLine = serde_json::from_str(line)
            .map_err(|_| BackupError::Unsupported("journal has a malformed record".into()))?;
        if record.seq <= prior || record.seq == 0 {
            return Err(BackupError::Unsupported(
                "journal sequence is not increasing".into(),
            ));
        }
        prior = record.seq;
        if let AgentEvent::SessionStarted {
            harness,
            model,
            assistant_message_id,
            ..
        } = record.event
        {
            metadata.insert(
                assistant_message_id,
                crate::activity_history::ActivityRunMetadata {
                    harness: Some(harness),
                    model: (!model.trim().is_empty()).then_some(model),
                },
            );
        }
    }
    Ok(metadata)
}

fn staged_command_history(
    store: &luna_sync::DocsStore,
    task_id: &str,
) -> Result<(Vec<TaskCommandEvent>, bool)> {
    let mut cursor = 0;
    let mut events = Vec::new();
    loop {
        let page = store
            .read_task_commands(task_id, cursor, 100)
            .map_err(|e| BackupError::Invalid(format!("command history invalid: {e}")))?;
        if page.reload_required {
            return Ok((events, true));
        }
        events.extend(page.events);
        if events.len() >= 10_000 {
            events.truncate(10_000);
            return Ok((events, page.has_more));
        }
        if !page.has_more {
            return Ok((events, false));
        }
        let Some(next) = page.next_task_sequence else {
            return Ok((events, true));
        };
        cursor = next;
    }
}

fn verify_database(conn: &Connection) -> Result<()> {
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(BackupError::Invalid(format!(
            "database integrity check: {integrity}"
        )));
    }
    Ok(())
}

fn validate_entry_path(path: &str, category: &str) -> Result<()> {
    let parsed = Path::new(path);
    if parsed.is_absolute() || path.contains('\\') || path.contains('\0') || path.is_empty() {
        return Err(BackupError::Invalid(format!("unsafe archive path: {path}")));
    }
    let components = parsed
        .components()
        .map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| BackupError::Invalid(format!("unsafe archive path: {path}")))?;
    if components.join("/") != path {
        return Err(BackupError::Invalid(format!(
            "noncanonical archive path: {path}"
        )));
    }
    let c = components.as_slice();
    let profile_tail = if c.len() >= 3 && c[0] == "profiles" && c[1] == "local" {
        Some(&c[2..])
    } else if c.len() >= 4 && c[0] == "orgs" {
        Some(&c[3..])
    } else {
        None
    };
    let allowed = match category {
        "settings" => {
            c.len() == 1
                && [
                    "local-profile.json",
                    "ui-settings.json",
                    "harness-prefs.json",
                    "repos.json",
                ]
                .contains(&c[0])
        }
        "database" => profile_tail == Some(["docs.sqlite3"].as_slice()),
        "journal" => profile_tail.is_some_and(|tail| {
            tail.len() >= 2 && tail[0] == "journals" && path.ends_with(".jsonl")
        }),
        "github-pr-attempt" => profile_tail.is_some_and(|tail| {
            tail.len() == 2
                && tail[0] == "github-pr-attempts"
                && tail[1].strip_suffix(".json").is_some_and(|name| {
                    name.len() == 64 && name.bytes().all(|c| c.is_ascii_hexdigit())
                })
        }),
        "attachment" => profile_tail
            .is_some_and(|tail| tail.len() >= 2 && tail[0] == "uploads" && tail[1] != "tmp"),
        _ => false,
    };
    if !allowed || c.iter().any(|s| *s == "." || *s == ".." || s.is_empty()) {
        return Err(BackupError::Invalid(format!(
            "unapproved archive entry: {path}"
        )));
    }
    Ok(())
}

fn validate_manifest(manifest: &BackupManifest) -> Result<()> {
    if !matches!(manifest.format_version, 1..=3)
        || manifest.entries.len() > MAX_ENTRIES
        || manifest.databases.is_empty()
    {
        return Err(BackupError::Invalid(
            "unsupported manifest format or entry count".into(),
        ));
    }
    if manifest.excluded_categories != EXCLUDED.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()
    {
        return Err(BackupError::Invalid("exclusion contract differs".into()));
    }
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for entry in &manifest.entries {
        validate_entry_path(&entry.path, &entry.category)?;
        if !seen.insert(&entry.path)
            || entry.sha256.len() != 64
            || !entry.sha256.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(BackupError::Invalid(
                "duplicate path or invalid digest".into(),
            ));
        }
        total = total
            .checked_add(entry.size)
            .ok_or_else(|| BackupError::Invalid("archive size overflow".into()))?;
    }
    if total != manifest.total_bytes {
        return Err(BackupError::Invalid("manifest byte count differs".into()));
    }
    let mut db_paths = BTreeSet::new();
    for db in &manifest.databases {
        if !manifest
            .entries
            .iter()
            .any(|e| e.path == db.path && e.category == "database")
            || db.schema_version > CURRENT_SCHEMA
            || db.schema_version < 0
            || db.documents < 0
            || db.task_events < 0
            || db.draft_records < 0
            || db.attachment_owners < 0
            || (db.schema_version >= 10
                && (db.cleanup_tasks.is_none()
                    || db.cleanup_projects.is_none()
                    || db.cleanup_paths.is_none()))
            || (db.schema_version >= 12 && db.local_executable_authority.is_none())
            || db.derived_events_discarded < 0
            || [
                db.cleanup_tasks,
                db.cleanup_projects,
                db.cleanup_paths,
                db.local_executable_authority,
                db.projects,
                db.tasks,
                db.attachment_blobs,
                db.activity_events_expected,
            ]
            .into_iter()
            .flatten()
            .any(|value| value < 0)
            || (manifest.format_version >= 2
                && (db.projects.is_none() || db.tasks.is_none() || db.attachment_blobs.is_none())
                && db.derived_rebuild_blocker.is_none())
            || !db_paths.insert(&db.path)
        {
            return Err(BackupError::Invalid(
                "database manifest mismatch or unsupported schema".into(),
            ));
        }
    }
    Ok(())
}

fn measure_file(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path)?;
    copy_and_hash(&mut file, &mut io::sink())
}

fn hash_file(path: &Path) -> Result<String> {
    Ok(measure_file(path)?.1)
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn copy_and_hash(reader: &mut impl Read, writer: &mut impl Write) -> Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; CHUNK];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
        count = count
            .checked_add(n as u64)
            .ok_or_else(|| BackupError::Invalid("file too large".into()))?;
    }
    Ok((count, format!("{:x}", hash.finalize())))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn latest_authoritative_mtime(root: &Path) -> Result<i64> {
    fn file_mtime(path: &Path, latest: &mut i64) -> Result<()> {
        ensure_real_file(path)?;
        let modified = fs::metadata(path)?
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        *latest = (*latest).max(modified);
        Ok(())
    }
    fn tree_mtime(path: &Path, latest: &mut i64, journals_only: bool) -> Result<()> {
        ensure_real_dir(path)?;
        for child in sorted_children(path)? {
            let metadata = fs::symlink_metadata(&child)?;
            if metadata.file_type().is_dir() {
                tree_mtime(&child, latest, journals_only)?;
            } else if metadata.file_type().is_file() {
                if !journals_only || child.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                {
                    file_mtime(&child, latest)?;
                }
            } else {
                return Err(BackupError::Invalid(
                    "unsafe authoritative source entry".into(),
                ));
            }
        }
        Ok(())
    }
    fn profile_mtime(profile: &Path, latest: &mut i64) -> Result<()> {
        ensure_real_dir(profile)?;
        let db = profile.join("docs.sqlite3");
        file_mtime(&db, latest)?;
        for suffix in ["-wal", "-journal"] {
            let sidecar = profile.join(format!("docs.sqlite3{suffix}"));
            if sidecar.exists() {
                file_mtime(&sidecar, latest)?;
            }
        }
        let journals = profile.join("journals");
        if journals.exists() {
            tree_mtime(&journals, latest, true)?;
        }
        let attempts = profile.join("github-pr-attempts");
        if attempts.exists() {
            tree_mtime(&attempts, latest, false)?;
        }
        let uploads = profile.join("uploads");
        if uploads.exists() {
            ensure_real_dir(&uploads)?;
            for child in sorted_children(&uploads)? {
                if child.file_name().and_then(|name| name.to_str()) == Some("tmp") {
                    continue;
                }
                let metadata = fs::symlink_metadata(&child)?;
                if metadata.file_type().is_dir() {
                    tree_mtime(&child, latest, false)?;
                } else if metadata.file_type().is_file() {
                    file_mtime(&child, latest)?;
                } else {
                    return Err(BackupError::Invalid("unsafe upload source entry".into()));
                }
            }
        }
        Ok(())
    }
    let mut latest = 0;
    for name in [
        "local-profile.json",
        "ui-settings.json",
        "harness-prefs.json",
        "repos.json",
    ] {
        let path = root.join(name);
        if path.exists() {
            file_mtime(&path, &mut latest)?;
        }
    }
    let local = root.join("profiles/local");
    if local.exists() {
        profile_mtime(&local, &mut latest)?;
    }
    let orgs = root.join("orgs");
    if orgs.exists() {
        ensure_real_dir(&orgs)?;
        for org in sorted_children(&orgs)? {
            ensure_real_dir(&org)?;
            for user in sorted_children(&org)? {
                profile_mtime(&user, &mut latest)?;
            }
        }
    }
    Ok(latest)
}

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn ensure_isolated_destination(path: &Path) -> Result<()> {
    ensure_real_dir(path)?;
    let marker = path.join(DESTINATION_MARKER);
    if !fs::symlink_metadata(&marker).is_ok_and(|m| m.file_type().is_file())
        || !matches!(
            fs::read_to_string(marker).ok().as_deref(),
            Some(DESTINATION_VALUE | MANAGED_DESTINATION_VALUE)
        )
    {
        return Err(BackupError::UnauthorizedRoot);
    }
    Ok(())
}

/// Read-only destination validation for the engine's explicit copy-isolated
/// backup configuration. The marker is prepared outside this API.
pub fn validate_isolated_destination(path: &Path) -> Result<()> {
    ensure_isolated_destination(path)
}

fn is_provider_private_root(path: &Path) -> bool {
    let mut homes = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        homes.extend([
            home.join(".codex"),
            home.join(".claude"),
            home.join(".config/opencode"),
            home.join(".local/share/opencode"),
        ]);
    }
    for variable in [
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "OPENCODE_HOME",
        "OPENCODE_CONFIG_DIR",
    ] {
        if let Some(home) = std::env::var_os(variable).map(PathBuf::from)
            && home.is_absolute()
        {
            homes.push(home);
        }
    }
    homes.into_iter().any(|home| path.starts_with(home))
}

fn managed_destination_path(root: &Path) -> Result<PathBuf> {
    let parent = root
        .parent()
        .ok_or_else(|| BackupError::Invalid("managed root has no parent".into()))?;
    let name = root
        .file_name()
        .ok_or_else(|| BackupError::Invalid("managed root has no name".into()))?;
    let mut destination_name = name.to_os_string();
    destination_name.push(".backups");
    Ok(parent.join(destination_name))
}

fn validate_managed_destination(destination: &Path) -> Result<()> {
    ensure_real_dir(destination)?;
    let marker = destination.join(DESTINATION_MARKER);
    if !fs::symlink_metadata(&marker).is_ok_and(|m| m.file_type().is_file())
        || fs::read_to_string(marker).ok().as_deref() != Some(MANAGED_DESTINATION_VALUE)
    {
        return Err(BackupError::UnauthorizedRoot);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = fs::metadata(destination)?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(BackupError::UnauthorizedRoot);
        }
    }
    Ok(())
}

/// Engine startup must not open a profile left half-imported by a crash.
pub(crate) fn ensure_no_pending_managed_restore(root: &Path) -> Result<()> {
    match fs::symlink_metadata(root.join(MANAGED_RESTORE_JOURNAL)) {
        Ok(_) => Err(BackupError::Unsupported(
            "offline restore was interrupted; run `luna backup recover` before starting LUNA"
                .into(),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_managed_restore_path(path: &str) -> Result<()> {
    for category in [
        "settings",
        "database",
        "journal",
        "attachment",
        "github-pr-attempt",
    ] {
        if validate_entry_path(path, category).is_ok() {
            return Ok(());
        }
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        if let Some(database) = path.strip_suffix(suffix) {
            return validate_entry_path(database, "database");
        }
    }
    Err(BackupError::Invalid(format!(
        "unapproved managed restore path: {path}"
    )))
}

fn verify_existing_parent_chain(root: &Path, path: &str) -> Result<()> {
    let mut parent = root.to_path_buf();
    for component in Path::new(path)
        .parent()
        .into_iter()
        .flat_map(Path::components)
    {
        parent.push(component.as_os_str());
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(BackupError::Invalid(format!(
                    "unsafe restore parent: {}",
                    parent.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn create_private_parents(root: &Path, path: &str) -> Result<()> {
    let mut parent = root.to_path_buf();
    for component in Path::new(path)
        .parent()
        .into_iter()
        .flat_map(Path::components)
    {
        parent.push(component.as_os_str());
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(BackupError::Invalid(format!(
                    "unsafe restore parent: {}",
                    parent.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => create_private_dir(&parent)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn write_managed_restore_journal(path: &Path, journal: &ManagedRestoreJournal) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| BackupError::Invalid("journal has no parent".into()))?;
    let json = serde_json::to_vec(journal)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&json)?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| BackupError::Io(error.error))?;
    sync_parent(parent)?;
    Ok(())
}

fn recover_managed_restore_locked(
    root: &Path,
    journal_path: &Path,
    journal: &ManagedRestoreJournal,
) -> Result<()> {
    if journal.version != 1 || journal.root != root {
        return Err(BackupError::UnauthorizedRoot);
    }
    let rollback = &journal.rollback_dir;
    if rollback.parent() != root.parent()
        || !rollback
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".luna-l17-managed-rollback-"))
    {
        return Err(BackupError::UnauthorizedRoot);
    }
    ensure_real_dir(rollback)?;
    let mut seen = BTreeSet::new();
    for entry in &journal.entries {
        validate_managed_restore_path(&entry.path)?;
        if !seen.insert(&entry.path) {
            return Err(BackupError::Invalid(
                "duplicate restore journal path".into(),
            ));
        }
        verify_existing_parent_chain(root, &entry.path)?;
        verify_existing_parent_chain(rollback, &entry.path)?;
    }
    for entry in journal.entries.iter().rev() {
        let target = root.join(&entry.path);
        let previous = rollback.join(&entry.path);
        let backed_up = match fs::symlink_metadata(&previous) {
            Ok(metadata) if metadata.file_type().is_file() => true,
            Ok(_) => return Err(BackupError::Invalid("unsafe rollback file".into())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if backed_up || !entry.existed {
            match fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.file_type().is_file() => fs::remove_file(&target)?,
                Ok(_) => return Err(BackupError::Invalid("unsafe installed file".into())),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if backed_up {
            create_private_parents(root, &entry.path)?;
            fs::rename(&previous, &target)?;
        } else if entry.existed && !target.is_file() {
            return Err(BackupError::Invalid(format!(
                "previous file missing during recovery: {}",
                entry.path
            )));
        }
    }
    fs::remove_file(journal_path)?;
    sync_parent(root)?;
    Ok(())
}

/// Create a private sibling destination only when no unrelated directory is
/// already there. The marker makes later runs refuse a swapped-in directory.
fn managed_destination(root: &Path) -> Result<PathBuf> {
    let parent = root
        .parent()
        .ok_or_else(|| BackupError::Invalid("managed root has no parent".into()))?;
    let destination = managed_destination_path(root)?;
    match fs::symlink_metadata(&destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_dir(&destination)?;
            let marker = destination.join(DESTINATION_MARKER);
            let mut file = create_private_file(&marker)?;
            file.write_all(MANAGED_DESTINATION_VALUE.as_bytes())?;
            file.sync_all()?;
            sync_parent(&destination)?;
            sync_parent(parent)?;
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(BackupError::UnauthorizedRoot);
        }
        Ok(_) => {}
    }
    validate_managed_destination(&destination)?;
    Ok(destination)
}

const BB_STYLE_MANIFEST: &str = "manifest.json";

fn bb_style_header(size: u64) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(0o600);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    header
}

fn write_bb_style_archive(output: &mut File, json: &[u8], entries: &[SourceEntry]) -> Result<()> {
    let gzip = GzEncoder::new(output, Compression::default());
    let mut tar = tar::Builder::new(gzip);
    tar.append_data(
        &mut bb_style_header(json.len() as u64),
        BB_STYLE_MANIFEST,
        json,
    )?;
    for entry in entries {
        let file = File::open(&entry.source)?;
        tar.append_data(
            &mut bb_style_header(entry.manifest.size),
            &entry.manifest.path,
            file,
        )?;
        let (size, digest) = measure_file(&entry.source)?;
        if size != entry.manifest.size || digest != entry.manifest.sha256 {
            return Err(BackupError::SourceChanged(entry.manifest.path.clone()));
        }
    }
    let gzip = tar.into_inner()?;
    gzip.finish()?;
    Ok(())
}

fn read_bb_style_archive(path: &Path, stage: Option<&Path>) -> Result<BackupManifest> {
    ensure_real_file(path)?;
    let gzip = GzDecoder::new(File::open(path)?);
    let mut archive = tar::Archive::new(gzip);
    let mut entries = archive.entries()?;
    let mut first = entries
        .next()
        .ok_or_else(|| BackupError::Invalid("archive has no manifest".into()))??;
    if !first.header().entry_type().is_file()
        || first.path()?.as_ref() != Path::new(BB_STYLE_MANIFEST)
        || first.size() > MAX_MANIFEST as u64
    {
        return Err(BackupError::Invalid(
            "archive manifest entry is invalid".into(),
        ));
    }
    let mut json = Vec::with_capacity(first.size() as usize);
    first.read_to_end(&mut json)?;
    drop(first);
    let manifest: BackupManifest = serde_json::from_slice(&json)?;
    if manifest.format_version != 3 {
        return Err(BackupError::Invalid(
            "BB-style manifest version differs".into(),
        ));
    }
    validate_manifest(&manifest)?;
    for expected in &manifest.entries {
        let mut entry = entries
            .next()
            .ok_or_else(|| BackupError::Invalid("archive entry is missing".into()))??;
        if !entry.header().entry_type().is_file()
            || entry.path()?.as_ref() != Path::new(&expected.path)
            || entry.size() != expected.size
        {
            return Err(BackupError::Invalid(format!(
                "archive entry differs: {}",
                expected.path
            )));
        }
        let digest = if let Some(stage) = stage {
            let dest = stage.join(&expected.path);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = create_private_file(&dest)?;
            let (_, digest) = copy_and_hash(&mut entry, &mut file)?;
            file.sync_all()?;
            digest
        } else {
            let (_, digest) = copy_and_hash(&mut entry, &mut io::sink())?;
            digest
        };
        if digest != expected.sha256 {
            return Err(BackupError::Invalid(format!(
                "digest mismatch: {}",
                expected.path
            )));
        }
    }
    if entries.next().is_some() {
        return Err(BackupError::Invalid("archive has excess entries".into()));
    }
    Ok(manifest)
}

fn inspect_bb_style_archive(path: &Path) -> Result<BackupManifest> {
    read_bb_style_archive(path, None)
}

fn extract_bb_style_archive(path: &Path, stage: &Path) -> Result<BackupManifest> {
    read_bb_style_archive(path, Some(stage))
}

/// Independently authenticated frames keep memory bounded and authenticate
/// order, truncation, and the final frame. A random 64-bit nonce prefix plus
/// frame counter is unique for each archive with overwhelming probability.
struct EncryptedWriter<W: Write> {
    output: W,
    cipher: Aes256Gcm,
    prefix: [u8; 8],
    frame: u32,
    buffer: Vec<u8>,
}

impl<W: Write> EncryptedWriter<W> {
    fn new(mut output: W, key: &RecoveryKey) -> Result<Self> {
        let cipher = Aes256Gcm::new_from_slice(&key.0).map_err(|_| BackupError::Authentication)?;
        let mut prefix = [0u8; 8];
        random_fill(&mut prefix)
            .map_err(|e| BackupError::Invalid(format!("randomness unavailable: {e}")))?;
        output.write_all(MAGIC)?;
        output.write_all(&prefix)?;
        Ok(Self {
            output,
            cipher,
            prefix,
            frame: 0,
            buffer: Vec::with_capacity(CHUNK),
        })
    }

    fn flush_frame(&mut self, kind: u8, bytes: &[u8]) -> Result<()> {
        let nonce = self.nonce();
        let aad = self.aad(kind);
        let encrypted = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: bytes,
                    aad: &aad,
                },
            )
            .map_err(|_| BackupError::Authentication)?;
        self.output.write_all(&[kind])?;
        self.output
            .write_all(&(encrypted.len() as u32).to_be_bytes())?;
        self.output.write_all(&encrypted)?;
        self.frame = self
            .frame
            .checked_add(1)
            .ok_or_else(|| BackupError::Invalid("archive too large".into()))?;
        Ok(())
    }

    fn nonce(&self) -> Nonce<aes_gcm::aead::consts::U12> {
        let mut bytes = [0u8; 12];
        bytes[..8].copy_from_slice(&self.prefix);
        bytes[8..].copy_from_slice(&self.frame.to_be_bytes());
        Nonce::from(bytes)
    }

    fn aad(&self, kind: u8) -> [u8; 21] {
        let mut aad = [0u8; 21];
        aad[..8].copy_from_slice(MAGIC);
        aad[8..16].copy_from_slice(&self.prefix);
        aad[16..20].copy_from_slice(&self.frame.to_be_bytes());
        aad[20] = kind;
        aad
    }

    fn finish(mut self) -> Result<()> {
        if !self.buffer.is_empty() {
            let pending = std::mem::take(&mut self.buffer);
            self.flush_frame(0, &pending)?;
        }
        self.flush_frame(1, &[])?;
        self.output.flush()?;
        Ok(())
    }
}

impl<W: Write> Write for EncryptedWriter<W> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let length = input.len();
        while !input.is_empty() {
            let take = (CHUNK - self.buffer.len()).min(input.len());
            self.buffer.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.buffer.len() == CHUNK {
                let pending = std::mem::take(&mut self.buffer);
                self.flush_frame(0, &pending).map_err(io::Error::other)?;
            }
        }
        Ok(length)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

struct EncryptedReader<R: Read> {
    input: R,
    cipher: Aes256Gcm,
    prefix: [u8; 8],
    frame: u32,
    buffer: Vec<u8>,
    offset: usize,
    finished: bool,
}

impl<R: Read> EncryptedReader<R> {
    fn new(mut input: R, key: &RecoveryKey) -> Result<Self> {
        let mut header = [0u8; 16];
        input.read_exact(&mut header)?;
        if &header[..8] != MAGIC {
            return Err(BackupError::Invalid("archive magic differs".into()));
        }
        let mut prefix = [0u8; 8];
        prefix.copy_from_slice(&header[8..]);
        Ok(Self {
            input,
            cipher: Aes256Gcm::new_from_slice(&key.0).map_err(|_| BackupError::Authentication)?,
            prefix,
            frame: 0,
            buffer: Vec::new(),
            offset: 0,
            finished: false,
        })
    }

    fn next_frame(&mut self) -> Result<()> {
        let mut header = [0u8; 5];
        self.input
            .read_exact(&mut header)
            .map_err(|_| BackupError::Invalid("truncated encrypted frame".into()))?;
        let kind = header[0];
        let size = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
        if !matches!(kind, 0 | 1) || !(16..=CHUNK + 16).contains(&size) {
            return Err(BackupError::Invalid("invalid encrypted frame".into()));
        }
        let mut bytes = vec![0; size];
        self.input
            .read_exact(&mut bytes)
            .map_err(|_| BackupError::Invalid("truncated encrypted frame".into()))?;
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..8].copy_from_slice(&self.prefix);
        nonce_bytes[8..].copy_from_slice(&self.frame.to_be_bytes());
        let mut aad = [0u8; 21];
        aad[..8].copy_from_slice(MAGIC);
        aad[8..16].copy_from_slice(&self.prefix);
        aad[16..20].copy_from_slice(&self.frame.to_be_bytes());
        aad[20] = kind;
        self.buffer = self
            .cipher
            .decrypt(
                &Nonce::from(nonce_bytes),
                Payload {
                    msg: &bytes,
                    aad: &aad,
                },
            )
            .map_err(|_| BackupError::Authentication)?;
        self.offset = 0;
        self.frame = self
            .frame
            .checked_add(1)
            .ok_or_else(|| BackupError::Invalid("archive too large".into()))?;
        if kind == 1 {
            if !self.buffer.is_empty() {
                return Err(BackupError::Invalid("nonempty final frame".into()));
            }
            let mut trailing = [0u8; 1];
            if self.input.read(&mut trailing)? != 0 {
                return Err(BackupError::Invalid("trailing archive bytes".into()));
            }
            self.finished = true;
        } else if self.buffer.is_empty() {
            return Err(BackupError::Invalid("empty data frame".into()));
        }
        Ok(())
    }
}

impl<R: Read> Read for EncryptedReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        while self.offset == self.buffer.len() && !self.finished {
            self.next_frame().map_err(io::Error::other)?;
        }
        if self.finished {
            return Ok(0);
        }
        let len = output.len().min(self.buffer.len() - self.offset);
        output[..len].copy_from_slice(&self.buffer[self.offset..self.offset + len]);
        self.offset += len;
        Ok(len)
    }
}

fn read_manifest(reader: &mut impl Read) -> Result<BackupManifest> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_MANIFEST {
        return Err(BackupError::Invalid("manifest length out of bounds".into()));
    }
    let mut json = vec![0u8; length];
    reader.read_exact(&mut json)?;
    let manifest: BackupManifest = serde_json::from_slice(&json)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn copy_exact_and_hash(
    reader: &mut impl Read,
    writer: &mut impl Write,
    mut remaining: u64,
) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; CHUNK];
    while remaining > 0 {
        let n = remaining.min(buffer.len() as u64) as usize;
        reader.read_exact(&mut buffer[..n])?;
        writer.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
        remaining -= n as u64;
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Full authenticated readback without extracting any file.
pub fn inspect_archive(path: &Path, key: &RecoveryKey) -> Result<BackupManifest> {
    ensure_real_file(path)?;
    let mut reader = EncryptedReader::new(File::open(path)?, key)?;
    let manifest = read_manifest(&mut reader)?;
    for entry in &manifest.entries {
        let digest = copy_exact_and_hash(&mut reader, &mut io::sink(), entry.size)?;
        if digest != entry.sha256 {
            return Err(BackupError::Invalid(format!(
                "digest mismatch: {}",
                entry.path
            )));
        }
    }
    let mut tail = [0u8; 1];
    if reader.read(&mut tail)? != 0 {
        return Err(BackupError::Invalid("archive has excess content".into()));
    }
    Ok(manifest)
}

impl BackupService {
    /// Verify every entry before extracting. Preview never writes
    /// to the current root; it reports path and compatibility limits.
    pub fn preview_restore(&self, archive: &Path) -> Result<RestorePreview> {
        let manifest = self.inspect(archive)?;
        let mut conflicts = Vec::new();
        for entry in &manifest.entries {
            if self.root.path().join(&entry.path).exists() {
                conflicts.push(entry.path.clone());
            }
        }
        let mut unsupported = Vec::new();
        if manifest.source_root != self.root.path().display().to_string() {
            unsupported.push(
                "absolute attachment references require restore at the original data-root path"
                    .into(),
            );
        }
        if let Err(e) = replaceable_root_fingerprint(self.root.path()) {
            unsupported.push(e.to_string());
        }
        if manifest.format_version < 2 {
            unsupported.push("legacy manifest lacks authoritative restore count claims".into());
        }
        if manifest.source_environment_id != read_device_id(self.root.path())? {
            unsupported.push("source and target device identities differ".into());
        }
        for db in &manifest.databases {
            if let Some(reason) = &db.derived_rebuild_blocker {
                unsupported.push(format!("{}: {reason}", db.path));
            } else if db.activity_events_expected.is_none() {
                unsupported.push(format!("{}: activity event count is unavailable", db.path));
            }
            if let Some(reason) = &db.activation_blocker {
                unsupported.push(format!("{}: {reason}", db.path));
            }
        }
        Ok(RestorePreview {
            required_bytes: manifest.total_bytes,
            manifest,
            conflicts,
            unsupported,
        })
    }

    /// Stage into a sibling directory, verify hashes, migrate supported older
    /// SQLite schemas, reconcile authoritative counts and referenced blobs.
    /// The returned stage remains untouched until explicit activation.
    pub fn stage_restore(&self, archive: &Path) -> Result<StagedRestore> {
        self.stage_restore_with_space(archive, None)
    }

    /// Reopen a persisted stage without extracting or activating anything.
    /// Both the archive and the staged root must match the exact prior proof.
    pub(crate) fn reverify_staged(
        &self,
        evidence: &StagedEvidence,
        recorded_archive: &Path,
        recorded_stage: &Path,
    ) -> Result<()> {
        let _root_lock = self.root.lock()?;
        ensure_real_file(recorded_archive)?;
        if recorded_archive.canonicalize()? != evidence.archive || recorded_stage != evidence.stage
        {
            return Err(BackupError::SourceChanged(
                "staged checkpoint paths changed".into(),
            ));
        }
        let parent = self
            .root
            .path()
            .parent()
            .ok_or_else(|| BackupError::Invalid("root has no parent".into()))?;
        if evidence.stage.parent() != Some(parent)
            || !evidence.stage.file_name().is_some_and(|name| {
                name.to_str()
                    .is_some_and(|name| name.starts_with(".luna-l17-stage-"))
            })
            || restore_journal_path(self.root.path())?.exists()
        {
            return Err(BackupError::Unsupported(
                "staged path or interrupted restore journal requires owner review".into(),
            ));
        }
        let stage = IsolatedRoot::open(&evidence.stage)?;
        if stage.path() != evidence.stage {
            return Err(BackupError::UnauthorizedRoot);
        }
        let _stage_lock = stage.lock()?;
        ensure_real_file(&evidence.archive)?;
        if hash_file(&evidence.archive)? != evidence.archive_sha256 {
            return Err(BackupError::SourceChanged("staged archive changed".into()));
        }
        let preview = self.preview_restore(&evidence.archive)?;
        if hash_bytes(&serde_json::to_vec(&preview.manifest)?) != evidence.manifest_sha256 {
            return Err(BackupError::SourceChanged(
                "staged archive manifest changed".into(),
            ));
        }
        if replaceable_root_fingerprint(self.root.path())? != evidence.target_baseline {
            return Err(BackupError::SourceChanged(
                "staged restore target changed".into(),
            ));
        }
        if replaceable_root_fingerprint(stage.path())? != evidence.staged_fingerprint {
            return Err(BackupError::SourceChanged("staged data changed".into()));
        }
        verify_stage(stage.path(), &preview.manifest)?;
        if replaceable_root_fingerprint(stage.path())? != evidence.staged_fingerprint {
            return Err(BackupError::SourceChanged(
                "staged data changed during readback".into(),
            ));
        }
        Ok(())
    }

    /// Hermetic restart/readback seam for a previously staged isolated copy.
    #[doc(hidden)]
    pub fn verify_staged_for_tests(&self, staged: &StagedRestore) -> Result<()> {
        if staged.target != self.root.path() {
            return Err(BackupError::UnauthorizedRoot);
        }
        verify_stage(&staged.path, &staged.preview.manifest)
    }

    /// Hermetic preflight seam for low-space tests; production always queries
    /// the actual staging filesystem before any new directory is created.
    #[doc(hidden)]
    pub fn stage_restore_with_available_bytes_for_tests(
        &self,
        archive: &Path,
        available_bytes: u64,
    ) -> Result<StagedRestore> {
        self.stage_restore_with_space(archive, Some(available_bytes))
    }

    fn stage_restore_with_space(
        &self,
        archive: &Path,
        available_override: Option<u64>,
    ) -> Result<StagedRestore> {
        let _lock = self.root.lock()?;
        let preview = self.preview_restore(archive)?;
        let parent = self
            .root
            .path()
            .parent()
            .ok_or_else(|| BackupError::Invalid("root has no parent".into()))?;
        let required = preview
            .required_bytes
            .saturating_mul(2)
            .saturating_add(64 * 1024 * 1024);
        let available = match available_override {
            Some(value) => value,
            None => available_space(parent)?,
        };
        if available < required {
            return Err(BackupError::Unsupported(format!(
                "staged restore needs at least {required} free bytes; {available} available"
            )));
        }
        let baseline = replaceable_root_fingerprint(self.root.path())?;
        let stage = parent.join(format!(".luna-l17-stage-{}", uuid::Uuid::new_v4()));
        create_private_dir(&stage)?;
        let extraction = (|| -> Result<()> {
            fs::write(stage.join(ISOLATION_MARKER), ISOLATION_VALUE)?;
            let extracted_manifest = match &self.codec {
                ArchiveCodec::Encrypted(key) => extract_archive(archive, key, &stage)?,
                ArchiveCodec::BbStylePlain => extract_bb_style_archive(archive, &stage)?,
            };
            if extracted_manifest != preview.manifest {
                return Err(BackupError::SourceChanged(
                    "archive changed after preview".into(),
                ));
            }
            // Keep the target installation identity; it is deliberately absent
            // from the archive, so restoring a foreign backup cannot clone it.
            let device_id = self.root.path().join("device-id");
            if device_id.exists() {
                ensure_real_file(&device_id)?;
                fs::copy(&device_id, stage.join("device-id"))?;
            }
            verify_stage_base(&stage, &preview.manifest, true)?;
            rebuild_stage_activity(&stage, &preview.manifest)?;
            verify_stage_derived(&stage, &preview.manifest)?;
            replaceable_root_fingerprint(&stage)?;
            sync_parent(&stage)?;
            Ok(())
        })();
        if extraction.is_err() {
            let _ = fs::remove_dir_all(&stage);
        }
        extraction?;
        let staged_fingerprint = replaceable_root_fingerprint(&stage)?;
        Ok(StagedRestore {
            path: stage,
            target: self.root.path().to_path_buf(),
            preview,
            baseline,
            staged_fingerprint,
        })
    }

    /// Activate only a same-location isolated root with no excluded live
    /// material. Both old and new roots retain an engine lock during the swap.
    /// A journal is left for explicit interruption recovery if the process dies.
    pub fn activate(&self, staged: StagedRestore) -> Result<RestoreReceipt> {
        if self.managed_offline {
            return Err(BackupError::Unsupported(
                "managed-profile restore activation is not wired".into(),
            ));
        }
        self.activate_with_readback(staged, |_| Ok(()))
    }

    /// Synthetic integration seam for post-activation readback. A failure
    /// returns the exact prior isolated root and retains the failed new root.
    #[doc(hidden)]
    pub fn activate_with_readback_for_tests<F>(
        &self,
        staged: StagedRestore,
        readback: F,
    ) -> Result<RestoreReceipt>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        self.activate_with_readback(staged, readback)
    }

    fn activate_with_readback<F>(
        &self,
        staged: StagedRestore,
        readback: F,
    ) -> Result<RestoreReceipt>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        if self.managed_offline {
            return Err(BackupError::Unsupported(
                "managed-profile restore activation is not wired".into(),
            ));
        }
        if staged.target != self.root.path() || !staged.preview.unsupported.is_empty() {
            return Err(BackupError::Unsupported(
                staged.preview.unsupported.join("; "),
            ));
        }
        let _old_lock = self.root.lock()?;
        let stage_root = IsolatedRoot::open(&staged.path)?;
        let _new_lock = stage_root.lock()?;
        if replaceable_root_fingerprint(&staged.path)? != staged.staged_fingerprint {
            return Err(BackupError::SourceChanged(
                "staged data changed after verification".into(),
            ));
        }
        if replaceable_root_fingerprint(self.root.path())? != staged.baseline {
            return Err(BackupError::SourceChanged(
                "target changed after restore staging".into(),
            ));
        }
        verify_stage(&staged.path, &staged.preview.manifest)?;
        replaceable_root_fingerprint(&staged.path)?;
        let parent = self
            .root
            .path()
            .parent()
            .ok_or_else(|| BackupError::Invalid("root has no parent".into()))?;
        let rollback = parent.join(format!(".luna-l17-rollback-{}", uuid::Uuid::new_v4()));
        let journal_path = restore_journal_path(self.root.path())?;
        if journal_path.exists() {
            return Err(BackupError::Unsupported(
                "interrupted restore journal requires recovery".into(),
            ));
        }
        let mut journal = RestoreJournal::new(self.root.path(), &staged.path, &rollback)?;
        journal.phase = RestorePhase::Prepared;
        write_journal(&journal_path, &journal)?;
        fs::rename(self.root.path(), &rollback)?;
        journal.phase = RestorePhase::OldMoved;
        write_journal(&journal_path, &journal)?;
        if let Err(error) = fs::rename(&staged.path, self.root.path()) {
            fs::rename(&rollback, self.root.path())?;
            let _ = fs::remove_file(&journal_path);
            return Err(error.into());
        }
        journal.phase = RestorePhase::NewActive;
        write_journal(&journal_path, &journal)?;
        let verified = verify_stage(self.root.path(), &staged.preview.manifest)
            .and_then(|_| readback(self.root.path()))
            .and_then(|_| replaceable_root_fingerprint(self.root.path()));
        let active_fingerprint = match verified {
            Ok(value) => value,
            Err(error) => {
                let failed = parent.join(format!(".luna-l17-failed-{}", uuid::Uuid::new_v4()));
                fs::rename(self.root.path(), &failed)?;
                fs::rename(&rollback, self.root.path())?;
                let _ = fs::remove_file(&journal_path);
                return Err(error);
            }
        };
        fs::remove_file(&journal_path)?;
        sync_parent(parent)?;
        Ok(RestoreReceipt {
            active_root: self.root.path().to_path_buf(),
            rollback_root: rollback,
            source_manifest: staged.preview.manifest,
            active_fingerprint,
            old_fingerprint: staged.baseline,
        })
    }
}

fn available_space(path: &Path) -> Result<u64> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| BackupError::Invalid("NUL in staging path".into()))?;
        let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `path` is NUL-terminated and `info` points to writable
        // storage for the full statvfs result. No borrowed Rust data escapes.
        let rc = unsafe { libc::statvfs(path.as_ptr(), info.as_mut_ptr()) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let info = unsafe { info.assume_init() };
        Ok(u64::from(info.f_bavail).saturating_mul(info.f_frsize))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(BackupError::Unsupported(
            "free-space preflight is unavailable on this platform".into(),
        ))
    }
}

impl RestoreReceipt {
    /// Roll back a just-activated isolated root after detecting a failed
    /// post-start readback. Changed active data is preserved and needs review.
    pub fn rollback(self) -> Result<PathBuf> {
        let active = IsolatedRoot::open(&self.active_root)?;
        let old = IsolatedRoot::open(&self.rollback_root)?;
        let _active_lock = active.lock()?;
        let _old_lock = old.lock()?;
        if replaceable_root_fingerprint(active.path())? != self.active_fingerprint
            || replaceable_root_fingerprint(old.path())? != self.old_fingerprint
        {
            return Err(BackupError::SourceChanged(
                "active or rollback root changed".into(),
            ));
        }
        let parent = active
            .path()
            .parent()
            .ok_or_else(|| BackupError::Invalid("root has no parent".into()))?;
        let journal_path = restore_journal_path(active.path())?;
        if journal_path.exists() {
            return Err(BackupError::Unsupported(
                "interrupted restore journal requires recovery".into(),
            ));
        }
        let failed = parent.join(format!(".luna-l17-failed-{}", uuid::Uuid::new_v4()));
        let mut journal = RestoreJournal::new(active.path(), &failed, old.path())?;
        write_journal(&journal_path, &journal)?;
        fs::rename(active.path(), &failed)?;
        journal.phase = RestorePhase::OldMoved;
        write_journal(&journal_path, &journal)?;
        if let Err(error) = fs::rename(old.path(), active.path()) {
            fs::rename(&failed, active.path())?;
            let _ = fs::remove_file(&journal_path);
            return Err(error.into());
        }
        fs::remove_file(&journal_path)?;
        sync_parent(parent)?;
        Ok(failed)
    }
}

fn extract_archive(archive: &Path, key: &RecoveryKey, stage: &Path) -> Result<BackupManifest> {
    let mut reader = EncryptedReader::new(File::open(archive)?, key)?;
    let manifest = read_manifest(&mut reader)?;
    for entry in &manifest.entries {
        let dest = stage.join(&entry.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = create_private_file(&dest)?;
        let digest = copy_exact_and_hash(&mut reader, &mut file, entry.size)?;
        if digest != entry.sha256 {
            return Err(BackupError::Invalid(format!(
                "digest mismatch: {}",
                entry.path
            )));
        }
        file.sync_all()?;
    }
    let mut tail = [0u8; 1];
    if reader.read(&mut tail)? != 0 {
        return Err(BackupError::Invalid("archive has excess content".into()));
    }
    Ok(manifest)
}

fn create_private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

fn create_private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn verify_stage(stage: &Path, manifest: &BackupManifest) -> Result<()> {
    verify_stage_base(stage, manifest, false)?;
    verify_stage_derived(stage, manifest)
}

fn verify_stage_base(stage: &Path, manifest: &BackupManifest, migrate: bool) -> Result<()> {
    for entry in &manifest.entries {
        let path = stage.join(&entry.path);
        ensure_real_file(&path)?;
        let (size, digest) = measure_file(&path)?;
        if entry.category != "database" && (size != entry.size || digest != entry.sha256) {
            return Err(BackupError::Invalid(format!(
                "stage digest differs: {}",
                entry.path
            )));
        }
    }
    for expected in &manifest.databases {
        let path = stage.join(&expected.path);
        let profile = path
            .parent()
            .ok_or_else(|| BackupError::Invalid("database path has no parent".into()))?;
        // Migrate only during first extraction. Subsequent readback must not
        // rewrite the checkpoint whose fingerprint it is proving.
        let conn = if migrate {
            let store = luna_sync::DocsStore::open(profile)
                .map_err(|e| BackupError::Invalid(format!("staged migration failed: {e}")))?;
            drop(store);
            let conn = Connection::open(&path)?;
            conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
            conn.pragma_update(None, "journal_mode", "DELETE")?;
            conn
        } else {
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?
        };
        verify_database(&conn)?;
        let actual = database_counts(&conn, expected.path.clone())?;
        if actual.documents != expected.documents
            || actual.task_events != expected.task_events
            || actual.draft_records != expected.draft_records
            || actual.attachment_owners != expected.attachment_owners
            || expected
                .cleanup_tasks
                .is_some_and(|count| actual.cleanup_tasks != Some(count))
            || expected
                .cleanup_projects
                .is_some_and(|count| actual.cleanup_projects != Some(count))
            || expected
                .cleanup_paths
                .is_some_and(|count| actual.cleanup_paths != Some(count))
            || expected
                .local_executable_authority
                .is_some_and(|count| actual.local_executable_authority != Some(count))
        {
            return Err(BackupError::Invalid(format!(
                "authoritative database counts differ: {}",
                expected.path
            )));
        }
        verify_attachment_refs(&conn, stage, &manifest.source_root)?;
        if let Some(expected_blobs) = expected.attachment_blobs {
            let parent = Path::new(&expected.path).parent().ok_or_else(|| {
                BackupError::Invalid("database path has no profile prefix".into())
            })?;
            let actual_blobs = manifest
                .entries
                .iter()
                .filter(|entry| {
                    entry.category == "attachment"
                        && Path::new(&entry.path).starts_with(parent.join("uploads"))
                })
                .count() as i64;
            if actual_blobs != expected_blobs {
                return Err(BackupError::Invalid(format!(
                    "attachment blob count differs: {}",
                    expected.path
                )));
            }
        }
    }
    // Open a deterministic sample again after database/reference checks. The
    // extraction pass authenticated every blob; this later readback catches a
    // changed or unreadable staged attachment before activation.
    let attachments: Vec<_> = manifest
        .entries
        .iter()
        .filter(|entry| entry.category == "attachment")
        .collect();
    for index in [
        0,
        attachments.len() / 2,
        attachments.len().saturating_sub(1),
    ] {
        if let Some(entry) = attachments.get(index) {
            let (size, digest) = measure_file(&stage.join(&entry.path))?;
            if size != entry.size || digest != entry.sha256 {
                return Err(BackupError::Invalid(format!(
                    "staged attachment sample differs: {}",
                    entry.path
                )));
            }
        }
    }
    Ok(())
}

fn rebuild_stage_activity(stage: &Path, manifest: &BackupManifest) -> Result<()> {
    for expected in &manifest.databases {
        if expected.derived_rebuild_blocker.is_some() || expected.activity_events_expected.is_none()
        {
            continue;
        }
        let profile = stage.join(&expected.path).parent().unwrap().to_path_buf();
        let projection = derive_profile(&profile, &profile, read_device_id(stage)?.as_deref())?;
        if let Some(reason) = projection.blocker {
            return Err(BackupError::Unsupported(format!(
                "staged activity rebuild became unavailable: {reason}"
            )));
        }
        if projection.projects != expected.projects.unwrap_or(-1)
            || projection.tasks != expected.tasks.unwrap_or(-1)
            || projection.activity_count() != expected.activity_events_expected
        {
            return Err(BackupError::Invalid(format!(
                "project/task/activity counts differ before rebuild: {}",
                expected.path
            )));
        }
        let store = luna_sync::DocsStore::open(&profile)
            .map_err(|e| BackupError::Invalid(format!("staged index open failed: {e}")))?;
        store
            .rebuild_activity_events(&projection.activity)
            .map_err(|e| BackupError::Invalid(format!("staged index rebuild failed: {e}")))?;
    }
    Ok(())
}

fn verify_stage_derived(stage: &Path, manifest: &BackupManifest) -> Result<()> {
    for expected in &manifest.databases {
        let profile = stage.join(&expected.path).parent().unwrap().to_path_buf();
        let projection = derive_profile(&profile, &profile, read_device_id(stage)?.as_deref())?;
        if expected
            .projects
            .is_some_and(|count| projection.projects != count)
            || expected
                .tasks
                .is_some_and(|count| projection.tasks != count)
        {
            return Err(BackupError::Invalid(format!(
                "staged project/task count differs: {}",
                expected.path
            )));
        }
        if let Some(expected_events) = expected.activity_events_expected {
            if projection.blocker.is_some() || projection.activity_count() != Some(expected_events)
            {
                return Err(BackupError::Invalid(format!(
                    "staged activity source count differs: {}",
                    expected.path
                )));
            }
            let conn = Connection::open(profile.join("docs.sqlite3"))?;
            if count_table(&conn, "activity_events")? != expected_events {
                return Err(BackupError::Invalid(format!(
                    "staged activity index readback differs: {}",
                    expected.path
                )));
            }
            let expected_rows = projection
                .activity
                .iter()
                .flat_map(|(_, events)| events)
                .map(|event| Ok((event.event_id.clone(), serde_json::to_vec(event)?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            let mut stmt = conn.prepare("SELECT event_id,envelope_json FROM activity_events")?;
            let actual_rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                })?
                .collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
            if actual_rows != expected_rows {
                return Err(BackupError::Invalid(format!(
                    "staged activity envelopes differ: {}",
                    expected.path
                )));
            }
        }
    }
    Ok(())
}

fn verify_attachment_refs(conn: &Connection, stage: &Path, source_root: &str) -> Result<()> {
    if !has_table(conn, "attachment_owners")? {
        return Ok(());
    }
    let mut stmt = conn.prepare("SELECT DISTINCT blob_path FROM attachment_owners")?;
    let refs = stmt.query_map([], |row| row.get::<_, String>(0))?;
    for blob in refs {
        let path = PathBuf::from(blob?);
        let resolved = resolve_reference_path(&path)?;
        let relative = resolved.strip_prefix(source_root).map_err(|_| {
            BackupError::Unsupported("attachment reference lies outside LUNA data root".into())
        })?;
        let rel = relative
            .to_str()
            .ok_or_else(|| BackupError::Invalid("non-UTF-8 attachment reference".into()))?;
        validate_entry_path(rel, "attachment")?;
        if !stage.join(relative).is_file() {
            return Err(BackupError::Invalid(format!(
                "referenced attachment missing: {rel}"
            )));
        }
    }
    Ok(())
}

fn resolve_reference_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(BackupError::Unsupported(
            "attachment reference is not absolute".into(),
        ));
    }
    let mut ancestor = path;
    let mut missing = Vec::new();
    while !ancestor.exists() {
        let name = ancestor.file_name().ok_or_else(|| {
            BackupError::Unsupported("attachment reference has no existing ancestor".into())
        })?;
        missing.push(name.to_owned());
        ancestor = ancestor
            .parent()
            .ok_or_else(|| BackupError::Unsupported("attachment reference has no parent".into()))?;
    }
    let mut resolved = ancestor.canonicalize()?;
    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

fn replaceable_root_fingerprint(root: &Path) -> Result<String> {
    let mut files = BTreeMap::new();
    fn visit(root: &Path, path: &Path, files: &mut BTreeMap<String, String>) -> Result<()> {
        let rel = rel(root, path)?;
        let md = fs::symlink_metadata(path)?;
        if md.file_type().is_symlink() {
            return Err(BackupError::Unsupported(format!(
                "symlink in target root: {rel}"
            )));
        }
        if md.is_dir() {
            for child in sorted_children(path)? {
                visit(root, &child, files)?;
            }
        } else if md.is_file() {
            if path.parent().and_then(Path::file_name)
                == Some(std::ffi::OsStr::new("github-pr-attempts"))
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(pr_lock_name)
            {
                return Ok(()); // OS coordination only; never restore held locks.
            }
            if rel == ISOLATION_MARKER
                || rel == "engine.lock"
                || rel == "engine.lock.pid"
                || rel == "device-id.lock"
            {
                return Ok(());
            }
            if rel == "device-id" {
                files.insert(rel, hash_file(path)?);
                return Ok(());
            }
            if rel.ends_with("/docs.sqlite3-wal") || rel.ends_with("/docs.sqlite3-shm") {
                let database = path.with_file_name("docs.sqlite3");
                if !database.is_file() {
                    return Err(BackupError::Unsupported(format!(
                        "orphan SQLite sidecar in target: {rel}"
                    )));
                }
                files.insert(rel, hash_file(path)?);
                return Ok(());
            }
            let category = if rel.starts_with("profiles/") || rel.starts_with("orgs/") {
                if rel.ends_with("/docs.sqlite3") {
                    "database"
                } else if rel.contains("/journals/") {
                    "journal"
                } else if rel.contains("/github-pr-attempts/") {
                    "github-pr-attempt"
                } else if rel.contains("/uploads/") {
                    "attachment"
                } else {
                    "unknown"
                }
            } else {
                "settings"
            };
            validate_entry_path(&rel, category).map_err(|_| {
                BackupError::Unsupported(format!("target contains excluded or unknown data: {rel}"))
            })?;
            files.insert(rel, hash_file(path)?);
        } else {
            return Err(BackupError::Unsupported(format!(
                "special file in target root: {rel}"
            )));
        }
        Ok(())
    }
    for child in sorted_children(root)? {
        visit(root, &child, &mut files)?;
    }
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&files)?)))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RestorePhase {
    Prepared,
    OldMoved,
    NewActive,
}

#[derive(Debug, Serialize, Deserialize)]
struct RestoreJournal {
    target: String,
    stage: String,
    rollback: String,
    phase: RestorePhase,
}

impl RestoreJournal {
    fn new(target: &Path, stage: &Path, rollback: &Path) -> Result<Self> {
        let name = |path: &Path| {
            path.file_name()
                .and_then(|s| s.to_str())
                .map(str::to_owned)
                .ok_or_else(|| BackupError::Invalid("non-UTF-8 restore path".into()))
        };
        Ok(Self {
            target: name(target)?,
            stage: name(stage)?,
            rollback: name(rollback)?,
            phase: RestorePhase::Prepared,
        })
    }
}

fn restore_journal_path(target: &Path) -> Result<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| BackupError::Invalid("root has no parent".into()))?;
    let name = target
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| BackupError::Invalid("non-UTF-8 root name".into()))?;
    Ok(parent.join(format!(".luna-l17-{name}.restore.json")))
}

fn write_journal(path: &Path, journal: &RestoreJournal) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| BackupError::Invalid("journal has no parent".into()))?;
    let mut temp = NamedTempFile::new_in(parent)?;
    serde_json::to_writer(temp.as_file_mut(), journal)?;
    temp.as_file_mut().sync_all()?;
    temp.persist(path).map_err(|e| BackupError::Io(e.error))?;
    sync_parent(parent)
}

/// Recover a crashed synthetic restore by returning the old root to its exact
/// path. The journal may only name siblings using this module's reserved names.
pub fn recover_interrupted_restore(target: &Path) -> Result<Option<PathBuf>> {
    let parent = target
        .parent()
        .ok_or_else(|| BackupError::Invalid("root has no parent".into()))?;
    let journal_path = restore_journal_path(target)?;
    if !journal_path.exists() {
        return Ok(None);
    }
    ensure_real_file(&journal_path)?;
    let journal: RestoreJournal = serde_json::from_slice(&fs::read(&journal_path)?)?;
    let expected_target = target.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if journal.target != expected_target
        || !(journal.stage.starts_with(".luna-l17-stage-")
            || journal.stage.starts_with(".luna-l17-failed-"))
        || !journal.rollback.starts_with(".luna-l17-rollback-")
        || journal.stage.contains('/')
        || journal.rollback.contains('/')
    {
        return Err(BackupError::Invalid(
            "restore journal paths are unsafe".into(),
        ));
    }
    let rollback = parent.join(&journal.rollback);
    if matches!(journal.phase, RestorePhase::Prepared) && target.exists() && !rollback.exists() {
        IsolatedRoot::open(target)?;
        fs::remove_file(&journal_path)?;
        sync_parent(parent)?;
        return Ok(None);
    }
    let old = IsolatedRoot::open(&rollback)?;
    let _old_lock = old.lock()?;
    let failed = if target.exists() {
        let active = IsolatedRoot::open(target)?;
        let _active_lock = active.lock()?;
        let failed = parent.join(format!(".luna-l17-failed-{}", uuid::Uuid::new_v4()));
        fs::rename(target, &failed)?;
        Some(failed)
    } else {
        None
    };
    fs::rename(&rollback, target)?;
    fs::remove_file(&journal_path)?;
    sync_parent(parent)?;
    Ok(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_returns_old_isolated_root_after_interrupted_swap() {
        let parent = TempDir::new().unwrap();
        let target = parent.path().join("synthetic-luna");
        let stage = parent.path().join(".luna-l17-stage-synthetic");
        let rollback = parent.path().join(".luna-l17-rollback-synthetic");
        fs::create_dir(&target).unwrap();
        fs::create_dir(&stage).unwrap();
        fs::write(target.join(ISOLATION_MARKER), ISOLATION_VALUE).unwrap();
        fs::write(stage.join(ISOLATION_MARKER), ISOLATION_VALUE).unwrap();
        fs::write(target.join("ui-settings.json"), b"old").unwrap();
        fs::write(stage.join("ui-settings.json"), b"new").unwrap();
        let journal_path = restore_journal_path(&target).unwrap();
        let mut journal = RestoreJournal::new(&target, &stage, &rollback).unwrap();
        journal.phase = RestorePhase::Prepared;
        write_journal(&journal_path, &journal).unwrap();
        fs::rename(&target, &rollback).unwrap();
        assert!(recover_interrupted_restore(&target).unwrap().is_none());
        assert_eq!(fs::read(target.join("ui-settings.json")).unwrap(), b"old");
        assert_eq!(fs::read(stage.join("ui-settings.json")).unwrap(), b"new");
        assert!(!journal_path.exists());
    }

    #[test]
    fn recovery_preserves_failed_new_root_after_interrupted_readback() {
        let parent = TempDir::new().unwrap();
        let target = parent.path().join("synthetic-luna");
        let stage = parent.path().join(".luna-l17-stage-synthetic");
        let rollback = parent.path().join(".luna-l17-rollback-synthetic");
        fs::create_dir(&target).unwrap();
        fs::create_dir(&rollback).unwrap();
        fs::write(target.join(ISOLATION_MARKER), ISOLATION_VALUE).unwrap();
        fs::write(rollback.join(ISOLATION_MARKER), ISOLATION_VALUE).unwrap();
        fs::write(target.join("ui-settings.json"), b"new").unwrap();
        fs::write(rollback.join("ui-settings.json"), b"old").unwrap();
        let journal_path = restore_journal_path(&target).unwrap();
        let mut journal = RestoreJournal::new(&target, &stage, &rollback).unwrap();
        journal.phase = RestorePhase::NewActive;
        write_journal(&journal_path, &journal).unwrap();
        let failed = recover_interrupted_restore(&target).unwrap().unwrap();
        assert_eq!(fs::read(target.join("ui-settings.json")).unwrap(), b"old");
        assert_eq!(fs::read(failed.join("ui-settings.json")).unwrap(), b"new");
        assert!(!journal_path.exists());
    }

    #[test]
    fn retention_keeps_recent_daily_and_older_weekly_points() {
        let receipt = |day: i64, sequence: usize| BackupReceipt {
            archive: PathBuf::from(format!("backup-{day}-{sequence}")),
            archive_sha256: String::new(),
            manifest: BackupManifest {
                format_version: 1,
                app_version: String::new(),
                created_at_ms: day * 86_400_000 + sequence as i64,
                source_environment_id: None,
                source_root: String::new(),
                entries: Vec::new(),
                databases: Vec::new(),
                total_bytes: 0,
                excluded_categories: Vec::new(),
                previous_backup_sha256: None,
            },
        };
        let mut receipts: Vec<_> = (0..45).map(|day| receipt(day, 0)).collect();
        receipts.push(receipt(44, 1));
        let prune = retention_candidates(&receipts);
        assert!(prune.contains(&PathBuf::from("backup-44-0")));
        assert!(!prune.contains(&PathBuf::from("backup-44-1")));
        assert!(!prune.contains(&PathBuf::from("backup-38-0")));
        assert!(prune.len() > 20);
    }
}
