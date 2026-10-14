//! Latest turn snapshots, kept out of the checkout's Git database.
//!
//! A snapshot is `git add -A` into a private index followed by `git
//! write-tree`. `git add` writes a blob for every file it hashes; the earlier
//! implementation wrote those blobs, and every tree, into the checkout's own
//! object database, where nothing referenced them. Each turn start and each
//! Latest turn read added more. Here:
//!
//! - **Objects stay in an engine-owned store.** Each snapshot writes into its
//!   own absolute `GIT_OBJECT_DIRECTORY` under the engine data root and reads
//!   the checkout's real object directory (resolved by Git, so a linked
//!   worktree uses its common dir) as an alternate. The comparison capture,
//!   the tree diff and `cat-file` all use that same context. A store is
//!   deleted when the last [`TurnSnapshot`] clone holding it drops.
//! - **The checkout is otherwise untouched.** The index lives inside the
//!   store, Git's location variables are cleared, and hooks, fsmonitor, split
//!   and untracked index caches are off. Listing commands only read the real
//!   index.
//! - **Work is bounded.** Oversized untracked files and untracked directories
//!   over budget are omitted, consistently on both sides of a comparison, and
//!   reported as a partial result. Tracked files are never omitted. Every Git
//!   child runs in its own process group and is killed with that group when
//!   its capture is cancelled, superseded or exceeds [`SNAPSHOT_TIMEOUT`].
//!
//! Git still refreshes the mtime of checkout objects a snapshot finds instead
//! of writing them again; their contents and the set of object files do not
//! change.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::Capture;
use crate::EngineError;

/// Stores live in `{data root}/turn-snapshots/{run}/{store}`.
const STORE_DIR: &str = "turn-snapshots";
/// Largest untracked file a snapshot carries. The Changes pane renders at most
/// [`super::MAX_DIFF_SOURCE_BYTES`] of a file; the headroom keeps ordinary
/// assets in the file list. The largest tracked file measured in local
/// repositories was 4.3 MiB.
const MAX_UNTRACKED_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Untracked content budget, applied to each wholly untracked directory and
/// then to everything left. Source-sized sets fit by two orders of magnitude;
/// build output, dependency trees and datasets do not.
const MAX_UNTRACKED_FILES: usize = 10_000;
const MAX_UNTRACKED_BYTES: u64 = 256 * 1024 * 1024;
/// Cap on one Git path listing.
const MAX_LISTING_BYTES: usize = 8 * 1024 * 1024;
/// More omitted units than this makes the scope unavailable: every one is an
/// exclusion pathspec Git matches against every path.
const MAX_OMITTED_UNITS: usize = 2_048;
/// Backstop for one capture, permit wait included (a wedged mount, a hung
/// clean filter). The largest measured warm hash pass took 4.1 s.
pub(super) const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(60);
/// Snapshot captures one engine runs at once.
const MAX_CONCURRENT_CAPTURES: usize = 2;
const MAX_STDERR_BYTES: usize = 64 * 1024;
const MAX_PATH_ARGUMENT_BYTES: usize = 64 * 1024;
/// Prefix of every user-facing reason the scope cannot be shown.
pub(super) const UNAVAILABLE: &str = "Latest turn is unavailable";

/// Configuration for every snapshot Git command: nothing a snapshot runs may
/// start a daemon or hook, or write anything outside its own store.
const HARDENING: &[&str] = &[
    "core.fsmonitor=false",
    "core.splitIndex=false",
    "core.untrackedCache=false",
    "gc.auto=0",
    "maintenance.auto=false",
];
/// Variables that would point Git at another repository, index or database.
const LOCATION_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

const ALTERNATES_SEPARATOR: &str = ":";

pub(super) fn unavailable(reason: &str) -> EngineError {
    EngineError::Other(format!("{UNAVAILABLE}: {reason}"))
}

/// A working tree written as a Git tree, with the private object store that
/// holds it. Cloning shares the store; a reader keeps its clone for as long
/// as it reads, and the last clone to drop deletes the store.
#[derive(Debug, Clone)]
pub struct TurnSnapshot {
    /// Canonical checkout root the tree was captured in.
    pub root: PathBuf,
    /// Checkout identity id the tree was captured for.
    pub checkout_id: String,
    /// `git write-tree` sha of the tracked + untracked (unignored) tree.
    pub tree: String,
    pub at: chrono::DateTime<chrono::Utc>,
    /// Turn-start generation this snapshot was captured for.
    pub generation: u64,
    /// Untracked units left out of `tree` (Git's spelling, from the root).
    /// A comparison layered on this snapshot leaves them as they are here.
    omitted: Arc<Vec<Vec<u8>>>,
    store: Arc<ObjectStore>,
    runtime: Arc<SnapshotRuntime>,
}

impl TurnSnapshot {
    /// Paths whose changes this snapshot (or a comparison with it) cannot
    /// show. Non-zero means the result is partial.
    pub fn omitted_paths(&self) -> usize {
        self.omitted.len()
    }

    /// The current working tree, layered on this snapshot: the comparison
    /// starts from this tree and leaves out what either side left out, so a
    /// path omitted on one side only (a directory that crossed the budget
    /// mid-turn) keeps this side's state instead of becoming thousands of
    /// added or deleted files. Its store reads through this one.
    pub(super) async fn capture_current(&self) -> Result<TurnSnapshot, EngineError> {
        SnapshotRuntime::bounded(
            &self.runtime,
            &self.root,
            &self.checkout_id,
            Some(self),
            self.generation,
        )
        .await
    }
}

/// One engine's snapshot stores and capture budget.
#[derive(Debug)]
pub(crate) struct SnapshotRuntime {
    /// This process's run directory under the data root.
    run_dir: PathBuf,
    permits: Semaphore,
}

impl SnapshotRuntime {
    pub(crate) fn new(data_dir: &Path) -> Self {
        Self {
            run_dir: data_dir.join(STORE_DIR).join(run_id()),
            permits: Semaphore::new(MAX_CONCURRENT_CAPTURES),
        }
    }

    /// Delete the stores of earlier engine runs on this data root. Snapshots
    /// are in-memory state, so nothing on disk from another run is reachable;
    /// a run that was killed or crashed leaves its stores for this to collect.
    /// Engine assembly calls it while holding the data root's
    /// [`crate::InstanceLock`], so no other live engine can own a sibling run;
    /// engines started again in this process share its run directory.
    pub(crate) fn sweep_stale_runs(&self) {
        let Some(parent) = self.run_dir.parent().map(Path::to_path_buf) else {
            return;
        };
        let current = self.run_dir.file_name().map(OsStr::to_os_string);
        let sweep = move || remove_other_runs(&parent, current.as_deref());
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn_blocking(sweep);
            }
            Err(_) => sweep(),
        }
    }

    /// Capture the checkout at `root` as a new turn-start snapshot.
    pub(crate) async fn capture(
        runtime: &Arc<Self>,
        root: &Path,
        checkout_id: &str,
        generation: u64,
    ) -> Result<TurnSnapshot, EngineError> {
        Self::bounded(runtime, root, checkout_id, None, generation).await
    }

    async fn bounded(
        runtime: &Arc<Self>,
        root: &Path,
        checkout_id: &str,
        base: Option<&TurnSnapshot>,
        generation: u64,
    ) -> Result<TurnSnapshot, EngineError> {
        // Dropping this future on timeout or cancellation drops every Git
        // child it owns, which kills each one's process group.
        let work = async {
            let _permit = runtime
                .permits
                .acquire()
                .await
                .map_err(|_| unavailable("the snapshot capture stopped"))?;
            Box::pin(write_snapshot(runtime, root, checkout_id, base, generation)).await
        };
        match tokio::time::timeout(SNAPSHOT_TIMEOUT, work).await {
            Ok(result) => result,
            Err(_) => Err(unavailable(&format!(
                "the turn snapshot took longer than {}s",
                SNAPSHOT_TIMEOUT.as_secs()
            ))),
        }
    }
}

fn run_id() -> &'static str {
    static RUN_ID: OnceLock<String> = OnceLock::new();
    RUN_ID.get_or_init(|| {
        format!(
            "{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_micros()
        )
    })
}

fn remove_other_runs(parent: &Path, current: Option<&OsStr>) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if Some(entry.file_name().as_os_str()) == current {
            continue;
        }
        let path = entry.path();
        // `remove_dir_all` never follows a symlink; it fails on one, and the
        // link itself is removed instead.
        if std::fs::remove_dir_all(&path).is_err() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// One snapshot's private Git directory: `objects/`, the transient `index`,
/// and an (absent) `hooks/` directory for `core.hooksPath`.
#[derive(Debug)]
struct ObjectStore {
    dir: PathBuf,
    /// `GIT_ALTERNATE_OBJECT_DIRECTORIES`: the base snapshot's objects when
    /// layered, then the checkout's real object directory.
    alternates: OsString,
    /// A layered store reads through its base, which must outlive it.
    _base: Option<Arc<ObjectStore>>,
}

impl ObjectStore {
    async fn create(
        runtime: &SnapshotRuntime,
        alternates: OsString,
        base: Option<Arc<ObjectStore>>,
    ) -> Result<Self, EngineError> {
        // Absolute: Git resolves `GIT_OBJECT_DIRECTORY` against the checkout
        // (`-C`), not against this process's working directory.
        let run_dir = std::path::absolute(&runtime.run_dir)
            .map_err(|error| EngineError::Other(format!("turn snapshot store: {error}")))?;
        let store = Self {
            dir: run_dir.join(uuid::Uuid::new_v4().simple().to_string()),
            alternates,
            _base: base,
        };
        // From here on `store`'s drop removes whatever this creates, even
        // when the capture is cancelled part way.
        for sub in ["objects/info", "objects/pack"] {
            tokio::fs::create_dir_all(store.dir.join(sub))
                .await
                .map_err(|error| EngineError::Other(format!("turn snapshot store: {error}")))?;
        }
        Ok(store)
    }

    fn objects_dir(&self) -> PathBuf {
        self.dir.join("objects")
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index")
    }

    fn hooks_dir(&self) -> PathBuf {
        self.dir.join("hooks")
    }
}

impl Drop for ObjectStore {
    fn drop(&mut self) {
        let dir = std::mem::take(&mut self.dir);
        if dir.as_os_str().is_empty() {
            return;
        }
        // A store can hold thousands of loose objects: delete it off the
        // runtime's worker threads.
        let remove = move || {
            let _ = std::fs::remove_dir_all(&dir);
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn_blocking(remove);
            }
            Err(_) => remove(),
        }
    }
}

fn push_alternate(value: &mut OsString, dir: &Path) {
    if !value.is_empty() {
        value.push(ALTERNATES_SEPARATOR);
    }
    value.push(quote_alternate(dir.as_os_str()));
}

/// Git splits `GIT_ALTERNATE_OBJECT_DIRECTORIES` on the platform's path-list
/// separator and reads an entry that starts with `"` as a C-quoted path —
/// the only way to name a directory containing the separator. Inside quotes
/// only `\` and `"` need escaping; other bytes pass through.
#[cfg(unix)]
fn quote_alternate(dir: &OsStr) -> OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let bytes = dir.as_bytes();
    if bytes.first() != Some(&b'"') && !bytes.contains(&b':') {
        return dir.to_os_string();
    }
    let mut quoted = Vec::with_capacity(bytes.len() + 2);
    quoted.push(b'"');
    for &byte in bytes {
        if byte == b'"' || byte == b'\\' {
            quoted.push(b'\\');
        }
        quoted.push(byte);
    }
    quoted.push(b'"');
    OsString::from_vec(quoted)
}

#[cfg(not(unix))]
fn quote_alternate(dir: &OsStr) -> OsString {
    match dir.to_str() {
        Some(text) if text.contains(ALTERNATES_SEPARATOR) || text.starts_with('"') => {
            OsString::from(format!(
                "\"{}\"",
                text.replace('\\', "\\\\").replace('"', "\\\"")
            ))
        }
        _ => dir.to_os_string(),
    }
}

async fn write_snapshot(
    runtime: &Arc<SnapshotRuntime>,
    root: &Path,
    checkout_id: &str,
    base: Option<&TurnSnapshot>,
    generation: u64,
) -> Result<TurnSnapshot, EngineError> {
    let mut alternates = OsString::new();
    match base {
        Some(base) => {
            push_alternate(&mut alternates, &base.store.objects_dir());
            alternates.push(ALTERNATES_SEPARATOR);
            alternates.push(&base.store.alternates);
        }
        None => push_alternate(&mut alternates, &checkout_objects(root).await?),
    }
    let store =
        ObjectStore::create(runtime, alternates, base.map(|base| base.store.clone())).await?;

    let mut omitted = untracked_to_omit(root).await?;
    if let Some(base) = base {
        let known: HashSet<Vec<u8>> = omitted.iter().cloned().collect();
        omitted.extend(
            base.omitted
                .iter()
                .filter(|path| !known.contains(*path))
                .cloned(),
        );
    }
    if omitted.len() > MAX_OMITTED_UNITS {
        return Err(unavailable(
            "too many untracked paths exceed the snapshot budget",
        ));
    }
    omitted.sort();
    omitted.dedup();

    let index = store.index_path();
    if let Some(base) = base {
        run_git(
            root,
            Some(&store),
            Some(&index),
            &os_args(["read-tree", base.tree.as_str()]),
            None,
            64 * 1024,
        )
        .await?
        .check("git read-tree")?;
    }
    // NUL-separated pathspecs on stdin: no argv limit, no quoting.
    let mut pathspecs = b".\0".to_vec();
    for path in &omitted {
        pathspecs.extend_from_slice(b":(top,exclude,literal)");
        pathspecs.extend_from_slice(path);
        pathspecs.push(0);
    }
    run_git(
        root,
        Some(&store),
        Some(&index),
        &os_args([
            "add",
            "-A",
            "--ignore-errors",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ]),
        Some(pathspecs),
        64 * 1024,
    )
    .await?
    .check("git add")?;
    // `add -A` never adds an ignored path, but a tracked file stays tracked
    // when it matches an ignore rule. Without this its changes would vanish
    // from the scope while it reported complete.
    let forced = tracked_ignored(root, &omitted).await?;
    if !forced.is_empty() {
        let mut input = Vec::new();
        for path in &forced {
            input.extend_from_slice(path);
            input.push(0);
        }
        run_git(
            root,
            Some(&store),
            Some(&index),
            &os_args(["update-index", "--add", "--remove", "-z", "--stdin"]),
            Some(input),
            64 * 1024,
        )
        .await?
        .check("git update-index")?;
    }
    let written = run_git(
        root,
        Some(&store),
        Some(&index),
        &os_args(["write-tree"]),
        None,
        4096,
    )
    .await;
    // Only the tree is kept; the index can run to megabytes.
    let _ = tokio::fs::remove_file(&index).await;
    let tree = String::from_utf8_lossy(&written?.check("git write-tree")?.stdout)
        .trim()
        .to_string();
    if tree.is_empty() {
        return Err(EngineError::Other("git write-tree returned no tree".into()));
    }
    Ok(TurnSnapshot {
        root: root.to_path_buf(),
        checkout_id: checkout_id.to_string(),
        tree,
        at: chrono::Utc::now(),
        generation,
        omitted: Arc::new(omitted),
        store: Arc::new(store),
        runtime: runtime.clone(),
    })
}

/// The checkout's real object database. `--git-path` resolves a linked
/// worktree to its common dir; nothing assumes `.git/objects`.
async fn checkout_objects(root: &Path) -> Result<PathBuf, EngineError> {
    let listed = run_git(
        root,
        None,
        None,
        &os_args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ]),
        None,
        16 * 1024,
    )
    .await?
    .check("git rev-parse")?;
    let mut path = listed.stdout.as_slice();
    while let Some(rest) = path
        .strip_suffix(b"\n")
        .or_else(|| path.strip_suffix(b"\r"))
    {
        path = rest;
    }
    let path = PathBuf::from(os_path(path));
    let is_dir = tokio::fs::metadata(&path)
        .await
        .is_ok_and(|metadata| metadata.is_dir());
    if !path.is_absolute() || !is_dir {
        return Err(EngineError::Other("git object directory unknown".into()));
    }
    Ok(path)
}

/// Tracked paths that match an ignore rule, minus any under an omitted unit.
async fn tracked_ignored(root: &Path, omitted: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, EngineError> {
    let listed = run_git(
        root,
        None,
        None,
        &os_args([
            "ls-files",
            "-z",
            "--cached",
            "--ignored",
            "--exclude-standard",
        ]),
        None,
        MAX_LISTING_BYTES,
    )
    .await?;
    if listed.truncated {
        return Err(unavailable(
            "the checkout has too many tracked ignored files to snapshot",
        ));
    }
    let listed = listed.check("git ls-files")?;
    Ok(split_nul(&listed.stdout)
        .into_iter()
        .filter(|path| !omitted.iter().any(|unit| covers(unit, path)))
        .collect())
}

fn covers(unit: &[u8], path: &[u8]) -> bool {
    path == unit || (unit.ends_with(b"/") && path.starts_with(unit))
}

/// The untracked paths a snapshot must leave out, as Git spells them from the
/// checkout root.
///
/// Git lists untracked content as units: a loose file, or a wholly untracked
/// directory. A directory over the untracked budget goes as one unit (a build
/// tree is useless in part); inside the directories that fit, and among loose
/// files, only files past [`MAX_UNTRACKED_FILE_BYTES`] go. If what remains
/// still exceeds the budget, the largest units go first, so new source files
/// keep showing.
async fn untracked_to_omit(root: &Path) -> Result<Vec<Vec<u8>>, EngineError> {
    let listing = run_git(
        root,
        None,
        None,
        &os_args([
            "ls-files",
            "--others",
            "--exclude-standard",
            "--directory",
            "--no-empty-directory",
            "-z",
        ]),
        None,
        MAX_LISTING_BYTES,
    )
    .await?;
    if listing.truncated {
        return Err(unavailable(
            "the checkout has too many untracked paths to snapshot",
        ));
    }
    let units = split_nul(&listing.check("git ls-files")?.stdout);
    if units.is_empty() {
        return Ok(Vec::new());
    }

    // Budget-check directories with a walk that stops at the budget before
    // asking Git to enumerate them: Git lists a directory in full or not at
    // all. The walk runs on a blocking thread, which outlives this future if
    // the capture is cancelled, so the guard tells it to stop.
    let cancel = CancellationToken::new();
    let _stop_walk = cancel.clone().drop_guard();
    let walk_root = root.to_path_buf();
    let (mut omitted, dirs, files) =
        tokio::task::spawn_blocking(move || triage_units(&walk_root, units, &cancel))
            .await
            .map_err(|error| EngineError::Other(format!("untracked walk: {error}")))?;

    // Files inside the directories that fit, in Git's own spelling: only Git
    // knows which of them are ignored. Loose files are units of their own.
    let mut measured: Vec<Unit> = files
        .into_iter()
        .map(|path| Unit {
            files: vec![path.clone()],
            path,
        })
        .collect();
    let first_dir = measured.len();
    let owners: HashMap<Vec<u8>, usize> = dirs
        .iter()
        .enumerate()
        .map(|(index, dir)| (dir.clone(), first_dir + index))
        .collect();
    measured.extend(dirs.iter().map(|dir| Unit {
        path: dir.clone(),
        files: Vec::new(),
    }));
    for batch in path_batches(&dirs) {
        let mut args = os_args([
            "--literal-pathspecs",
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
        ]);
        args.extend(batch.iter().map(|dir| os_path(dir)));
        let listed = run_git(root, None, None, &args, None, MAX_LISTING_BYTES).await?;
        if listed.truncated {
            // Unmeasurable: leave every directory out, still fit loose files.
            measured.truncate(first_dir);
            omitted.extend(dirs.iter().cloned());
            break;
        }
        for file in split_nul(&listed.check("git ls-files")?.stdout) {
            // Untracked units never nest: a file belongs to the one listed
            // directory among its ancestors.
            let owner = file
                .iter()
                .enumerate()
                .filter(|(_, byte)| **byte == b'/')
                .find_map(|(end, _)| owners.get(&file[..=end]));
            if let Some(&owner) = owner {
                measured[owner].files.push(file);
            }
        }
    }

    let stat_root = root.to_path_buf();
    let (oversized, excess) = tokio::task::spawn_blocking(move || fit_units(&stat_root, measured))
        .await
        .map_err(|error| EngineError::Other(format!("untracked stat: {error}")))?;
    omitted.extend(oversized);
    omitted.extend(excess);
    Ok(omitted)
}

/// An untracked unit (a loose file, or a wholly untracked directory) and the
/// files it holds, as Git spells them.
struct Unit {
    path: Vec<u8>,
    files: Vec<Vec<u8>>,
}

fn split_nul(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

/// Argument batches that stay far below any platform's argv limit.
fn path_batches(paths: &[Vec<u8>]) -> Vec<&[Vec<u8>]> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (index, path) in paths.iter().enumerate() {
        if index > start && bytes + path.len() + 1 > MAX_PATH_ARGUMENT_BYTES {
            batches.push(&paths[start..index]);
            start = index;
            bytes = 0;
        }
        bytes += path.len() + 1;
    }
    if start < paths.len() {
        batches.push(&paths[start..]);
    }
    batches
}

/// Sort untracked units into directories over budget (omitted whole),
/// directories that fit, and loose files. Git marks a directory with a
/// trailing slash.
#[allow(clippy::type_complexity)]
fn triage_units(
    root: &Path,
    units: Vec<Vec<u8>>,
    cancel: &CancellationToken,
) -> (Vec<Vec<u8>>, Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut omitted = Vec::new();
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for unit in units {
        if cancel.is_cancelled() {
            break;
        }
        if !unit.ends_with(b"/") {
            files.push(unit);
        } else if dir_exceeds_budget(&root.join(os_path(&unit)), cancel) {
            omitted.push(unit);
        } else {
            dirs.push(unit);
        }
    }
    (omitted, dirs, files)
}

/// True once `dir` holds more unignored content than the untracked budget.
/// Stops at the budget, so its cost is bounded however large the directory
/// is. Files past the per-file cap count as files but not as bytes: they are
/// omitted one by one and must not condemn the directory around them.
fn dir_exceeds_budget(dir: &Path, cancel: &CancellationToken) -> bool {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let walk = ignore::WalkBuilder::new(dir)
        .hidden(false)
        .ignore(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for entry in walk.flatten() {
        if cancel.is_cancelled() {
            return false;
        }
        let Some(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            continue;
        }
        files += 1;
        if kind.is_file()
            && let Ok(metadata) = entry.metadata()
            && metadata.len() <= MAX_UNTRACKED_FILE_BYTES
        {
            bytes += metadata.len();
        }
        if files > MAX_UNTRACKED_FILES || bytes > MAX_UNTRACKED_BYTES {
            return true;
        }
    }
    false
}

/// Files past the per-file cap, then the units to leave out so the rest fits
/// the untracked budget, largest first. A path that vanished since Git listed
/// it costs nothing; `git add --ignore-errors` copes with it.
fn fit_units(root: &Path, units: Vec<Unit>) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut oversized = Vec::new();
    // (path, files, bytes) each unit would add.
    let mut costs = Vec::with_capacity(units.len());
    for unit in units {
        let (mut files, mut bytes) = (0usize, 0u64);
        for file in unit.files {
            let Ok(metadata) = std::fs::symlink_metadata(root.join(os_path(&file))) else {
                continue;
            };
            if metadata.is_file() && metadata.len() > MAX_UNTRACKED_FILE_BYTES {
                oversized.push(file);
                continue;
            }
            files += 1;
            if metadata.is_file() {
                bytes += metadata.len();
            }
        }
        costs.push((unit.path, files, bytes));
    }
    let mut files: usize = costs.iter().map(|(_, files, _)| files).sum();
    let mut bytes: u64 = costs.iter().map(|(_, _, bytes)| bytes).sum();
    let mut excess = Vec::new();
    if files <= MAX_UNTRACKED_FILES && bytes <= MAX_UNTRACKED_BYTES {
        return (oversized, excess);
    }
    // Largest first, measured against the budget each overruns most.
    let weight = |unit_files: usize, unit_bytes: u64| {
        (unit_files as f64 / MAX_UNTRACKED_FILES as f64)
            .max(unit_bytes as f64 / MAX_UNTRACKED_BYTES as f64)
    };
    costs.sort_by(|a, b| weight(b.1, b.2).total_cmp(&weight(a.1, a.2)));
    for (path, unit_files, unit_bytes) in costs {
        if files <= MAX_UNTRACKED_FILES && bytes <= MAX_UNTRACKED_BYTES {
            break;
        }
        files -= unit_files;
        bytes -= unit_bytes;
        // A loose oversized file is already omitted on its own.
        if unit_files > 0 || unit_bytes > 0 {
            excess.push(path);
        }
    }
    (oversized, excess)
}

/// [`super::capture_git`] for a command that reads `snapshot`'s objects: the
/// same output cap and error shape, under the snapshot's private context.
pub(super) async fn capture_git_in(
    snapshot: &TurnSnapshot,
    args: &[&str],
    max_bytes: usize,
) -> Result<Capture, EngineError> {
    let run = run_git(
        &snapshot.root,
        Some(&snapshot.store),
        None,
        &os_args(args.iter().copied()),
        None,
        max_bytes,
    )
    .await?;
    if !run.status.success() && !run.truncated {
        return Err(run.error("git"));
    }
    Ok(Capture {
        stdout: run.stdout,
        truncated: run.truncated,
    })
}

struct GitRun {
    status: ExitStatus,
    stdout: Vec<u8>,
    /// Stdout hit its cap; the child was killed.
    truncated: bool,
    stderr: Vec<u8>,
}

impl GitRun {
    fn error(&self, what: &str) -> EngineError {
        if self.truncated {
            return EngineError::Other(format!("{what}: output exceeded its limit"));
        }
        let stderr = String::from_utf8_lossy(&self.stderr);
        let message = stderr.trim();
        EngineError::Other(if message.is_empty() {
            format!("{what} exited {}", self.status)
        } else {
            format!("{what}: {message}")
        })
    }

    fn check(self, what: &str) -> Result<Self, EngineError> {
        if self.status.success() && !self.truncated {
            Ok(self)
        } else {
            Err(self.error(what))
        }
    }
}

/// A Git child in its own process group. Dropped before it has been reaped —
/// a cancelled, superseded or timed-out capture — it kills the whole group,
/// so a clean filter or textconv driver it started dies with it.
struct GroupChild(tokio::process::Child);

impl GroupChild {
    fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.id() {
            // Still unreaped, so the group id cannot have been reused.
            unsafe {
                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
        }
        let _ = self.0.start_kill();
    }
}

impl Drop for GroupChild {
    fn drop(&mut self) {
        if self.0.id().is_some() {
            self.kill();
        }
    }
}

/// Run one snapshot Git command. `store` supplies the private object context
/// (and hooks path); `index` the private index. Without them the command only
/// reads the checkout.
async fn run_git(
    root: &Path,
    store: Option<&ObjectStore>,
    index: Option<&Path>,
    args: &[OsString],
    stdin: Option<Vec<u8>>,
    max_stdout: usize,
) -> Result<GitRun, EngineError> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(root);
    for setting in HARDENING {
        cmd.arg("-c").arg(setting);
    }
    for name in LOCATION_ENV {
        cmd.env_remove(name);
    }
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(store) = store {
        let mut hooks = OsString::from("core.hooksPath=");
        hooks.push(store.hooks_dir());
        cmd.arg("-c").arg(hooks);
        cmd.env("GIT_OBJECT_DIRECTORY", store.objects_dir());
        cmd.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &store.alternates);
    }
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    cmd.args(args);
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = GroupChild(
        cmd.spawn()
            .map_err(|error| EngineError::Other(format!("git spawn failed: {error}")))?,
    );
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| EngineError::Other("git stdout unavailable".into()))?;
    let mut stderr = child
        .0
        .stderr
        .take()
        .ok_or_else(|| EngineError::Other("git stderr unavailable".into()))?;
    let input = child.0.stdin.take().zip(stdin);

    let feed = async move {
        if let Some((mut pipe, bytes)) = input {
            pipe.write_all(&bytes).await?;
            pipe.shutdown().await?;
        }
        Ok::<(), std::io::Error>(())
    };
    let read_stdout = async {
        let mut out = Vec::new();
        // Heap buffer: this future nests inside long-lived RPC futures.
        let mut buf = vec![0u8; 64 * 1024];
        let mut truncated = false;
        loop {
            let n = stdout.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let remaining = max_stdout.saturating_sub(out.len());
            if n > remaining {
                out.extend_from_slice(&buf[..remaining]);
                truncated = true;
                child.kill();
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        Ok::<_, std::io::Error>((out, truncated))
    };
    let read_stderr = async move {
        let mut kept = Vec::new();
        let mut buf = vec![0u8; 8 * 1024];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            let room = MAX_STDERR_BYTES.saturating_sub(kept.len());
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
        kept
    };
    let (fed, read, stderr) = tokio::join!(feed, read_stdout, read_stderr);
    let status = child
        .0
        .wait()
        .await
        .map_err(|error| EngineError::Other(format!("git wait failed: {error}")))?;
    let (stdout, truncated) =
        read.map_err(|error| EngineError::Other(format!("git read failed: {error}")))?;
    if let Err(error) = fed
        && status.success()
    {
        // A short pathspec list would silently drop exclusions.
        return Err(EngineError::Other(format!("git input failed: {error}")));
    }
    Ok(GitRun {
        status,
        stdout,
        truncated,
        stderr,
    })
}

#[cfg(unix)]
fn os_path(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    OsStr::from_bytes(bytes).to_os_string()
}

#[cfg(not(unix))]
fn os_path(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

fn os_args<'a>(args: impl IntoIterator<Item = &'a str>) -> Vec<OsString> {
    args.into_iter().map(OsString::from).collect()
}
