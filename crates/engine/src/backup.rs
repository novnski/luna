//! L17 offline backup core. Every filesystem entry point requires a deliberately
//! marked synthetic or copy-isolated root. The production engine/Keychain/RPC
//! integration must supply a separate authority boundary before using this.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use chrono::{Datelike, Utc};
use getrandom::fill as random_fill;
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, TempDir};
use zeroize::Zeroize;

use crate::instance_lock::InstanceLock;

const MAGIC: &[u8; 8] = b"LUNABK01";
const ISOLATION_MARKER: &str = ".luna-l17-isolated-root";
const ISOLATION_VALUE: &str = "L17 synthetic or copy-isolated root\n";
const DESTINATION_MARKER: &str = ".luna-l17-isolated-backups";
const DESTINATION_VALUE: &str = "L17 synthetic or copy-isolated backup destination\n";
const MAX_MANIFEST: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const CHUNK: usize = 64 * 1024;
const CURRENT_SCHEMA: i64 = 8;

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
    #[error("backup root must be an explicitly marked synthetic or copy-isolated LUNA root")]
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
    pub fn injected(bytes: [u8; 32]) -> Self {
        Self(bytes)
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
    pub derived_events_discarded: i64,
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

#[derive(Debug, Clone)]
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

#[derive(Debug)]
pub struct RestoreReceipt {
    pub active_root: PathBuf,
    pub rollback_root: PathBuf,
    pub source_manifest: BackupManifest,
    active_fingerprint: String,
    old_fingerprint: String,
}

pub struct BackupService {
    root: IsolatedRoot,
    key: RecoveryKey,
}

struct SourceEntry {
    manifest: BackupEntry,
    source: PathBuf,
}

impl BackupService {
    pub fn new(root: IsolatedRoot, key: RecoveryKey) -> Self {
        Self { root, key }
    }

    /// Encrypted manual export to a caller-selected destination. Existing
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
        let name = format!("luna-{}-{}.lunabk", now_ms, uuid::Uuid::new_v4());
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
        if let Some(previous) = last_success {
            if hash_file(&previous.archive)? != previous.archive_sha256
                || inspect_archive(&previous.archive, &self.key)? != previous.manifest
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
        let name = format!("luna-{}-{}.lunabk", now_ms, uuid::Uuid::new_v4());
        self.export_at(
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
            format_version: 1,
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
        {
            let mut cipher = EncryptedWriter::new(output.as_file_mut(), &self.key)?;
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
        output.as_file_mut().sync_all()?;
        let verified = inspect_archive(output.path(), &self.key)?;
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
    let snapshot = snapshots
        .path()
        .join(format!("{}.sqlite3", databases.len()));
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
    databases.push(counts);
    add_file(root, &db, &snapshot, "database", entries)?;
    let journals = profile.join("journals");
    if journals.exists() {
        collect_tree(root, &journals, "journal", entries, true)?;
    }
    let uploads = profile.join("uploads");
    if uploads.exists() {
        collect_tree(root, &uploads, "attachment", entries, false)?;
    }
    Ok(())
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
    Ok(DatabaseCounts {
        path,
        schema_version,
        documents: count_table(conn, "snapshots")?,
        task_events: count_table(conn, "task_command_events")?,
        draft_records: count_table(conn, "composer_drafts")?,
        attachment_owners: count_table(conn, "attachment_owners")?,
        derived_events_discarded: 0,
    })
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
    if manifest.format_version != 1
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
            || db.derived_events_discarded < 0
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
    fn visit(path: &Path, latest: &mut i64) -> Result<()> {
        let md = fs::symlink_metadata(path)?;
        if md.file_type().is_symlink() {
            return Err(BackupError::Invalid("symlink in source".into()));
        }
        if md.is_dir() {
            for child in sorted_children(path)? {
                visit(&child, latest)?;
            }
        } else if md.is_file() {
            let modified = md
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            *latest = (*latest).max(modified);
        }
        Ok(())
    }
    let mut latest = 0;
    for name in [
        "local-profile.json",
        "ui-settings.json",
        "harness-prefs.json",
        "repos.json",
        "profiles",
        "orgs",
    ] {
        let path = root.join(name);
        if path.exists() {
            visit(&path, &mut latest)?;
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
        || fs::read_to_string(marker).ok().as_deref() != Some(DESTINATION_VALUE)
    {
        return Err(BackupError::UnauthorizedRoot);
    }
    Ok(())
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
    /// Decrypt and verify every entry before extracting. Preview never writes
    /// to the current root; it reports path and compatibility limits.
    pub fn preview_restore(&self, archive: &Path) -> Result<RestorePreview> {
        let manifest = inspect_archive(archive, &self.key)?;
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
        if manifest
            .databases
            .iter()
            .any(|db| db.derived_events_discarded > 0)
        {
            unsupported.push(
                "derived activity/search rows require a verified engine rebuild before activation"
                    .into(),
            );
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
        let _lock = self.root.lock()?;
        let preview = self.preview_restore(archive)?;
        let baseline = replaceable_root_fingerprint(self.root.path())?;
        let parent = self
            .root
            .path()
            .parent()
            .ok_or_else(|| BackupError::Invalid("root has no parent".into()))?;
        let stage = parent.join(format!(".luna-l17-stage-{}", uuid::Uuid::new_v4()));
        create_private_dir(&stage)?;
        let extraction = (|| -> Result<()> {
            fs::write(stage.join(ISOLATION_MARKER), ISOLATION_VALUE)?;
            let extracted_manifest = extract_archive(archive, &self.key, &stage)?;
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
            verify_stage(&stage, &preview.manifest)?;
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
        // Migrations are applied to the staged copy only. No source or target
        // database is opened writable during this phase.
        let profile = path
            .parent()
            .ok_or_else(|| BackupError::Invalid("database path has no parent".into()))?;
        let store = zeron_sync::DocsStore::open(profile)
            .map_err(|e| BackupError::Invalid(format!("staged migration failed: {e}")))?;
        drop(store);
        let conn = Connection::open(&path)?;
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        conn.pragma_update(None, "journal_mode", "DELETE")?;
        verify_database(&conn)?;
        let actual = database_counts(&conn, expected.path.clone())?;
        if actual.documents != expected.documents
            || actual.task_events != expected.task_events
            || actual.draft_records != expected.draft_records
            || actual.attachment_owners != expected.attachment_owners
        {
            return Err(BackupError::Invalid(format!(
                "authoritative database counts differ: {}",
                expected.path
            )));
        }
        verify_attachment_refs(&conn, stage, &manifest.source_root)?;
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
