//! Device-scoped package store under `{device root}/extensions/`:
//!
//! - `staging/<uuid>/tree/`: one install in progress. Never read as a package.
//! - `packages/<package hash>/`: committed, read-only package trees.
//! - `installed.json`: the index. A package exists only once it is listed here.
//!
//! Commit order: copy into staging → verify the staged bytes → rename the
//! tree into `packages/` → publish the index (temp file, fsync, rename). A
//! crash before the index rename leaves only staging or an unlisted package
//! directory, and [`PackageStore::recover`] removes both on the next open.
//! The store creates nothing on disk until the first install.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use luna_proto::contract::ExtensionId;
use luna_proto::{
    EXTENSION_MANIFEST_FILE, ExtensionIntegrity, ExtensionIntegrityStatus, ExtensionOrigin,
    ExtensionRuntimeKind,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::manifest::{self, MAX_FILES, MAX_MANIFEST_BYTES, MAX_PACKAGE_BYTES, ValidatedManifest};

const INDEX_FILE: &str = "installed.json";
const INDEX_VERSION: u32 = 1;
const MAX_INDEX_BYTES: u64 = 4 * 1024 * 1024;
const PACKAGES_DIR: &str = "packages";
const STAGING_DIR: &str = "staging";
const INDEX_TEMP_PREFIX: &str = ".installed-";
/// Directories plus files one staged tree may hold.
const MAX_STAGED_ENTRIES: usize = MAX_FILES * 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct InstalledRecord {
    pub id: ExtensionId,
    pub version: String,
    pub package_hash: String,
    /// Copied from the manifest so a damaged package still lists readably.
    pub display_name: String,
    pub publisher: String,
    pub runtime: ExtensionRuntimeKind,
    pub origin: ExtensionOrigin,
    pub installed_at_ms: i64,
    pub size_bytes: u64,
    pub file_count: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexFile {
    version: u32,
    packages: Vec<InstalledRecord>,
}

/// A staged copy, removed on drop unless committed.
pub(super) struct Staged {
    dir: PathBuf,
    tree: PathBuf,
    committed: bool,
}

impl Staged {
    pub fn tree(&self) -> &Path {
        &self.tree
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed {
            let _ = remove_tree(&self.dir);
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct Recovered {
    pub staging: usize,
    pub orphans: usize,
}

pub(super) struct PackageStore {
    root: PathBuf,
}

impl PackageStore {
    pub fn new(device_root: &Path) -> Self {
        Self {
            root: device_root.join("extensions"),
        }
    }

    fn packages(&self) -> PathBuf {
        self.root.join(PACKAGES_DIR)
    }

    pub fn package_dir(&self, package_hash: &str) -> PathBuf {
        self.packages().join(package_hash)
    }

    /// Records in install order. A missing store is empty.
    pub fn read_index(&self) -> Result<Vec<InstalledRecord>, String> {
        let path = self.root.join(INDEX_FILE);
        let bytes = match read_regular_file(&path, MAX_INDEX_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(format!(
                    "the installed-extensions index could not be read; it was preserved: {error}"
                ));
            }
        };
        let index: IndexFile = serde_json::from_slice(&bytes).map_err(|error| {
            format!("the installed-extensions index is invalid; it was preserved: {error}")
        })?;
        if index.version != INDEX_VERSION {
            return Err(format!(
                "the installed-extensions index has unsupported version {}",
                index.version
            ));
        }
        Ok(index.packages)
    }

    fn write_index(&self, packages: Vec<InstalledRecord>) -> Result<(), String> {
        let mut bytes = serde_json::to_vec_pretty(&IndexFile {
            version: INDEX_VERSION,
            packages,
        })
        .map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        publish_file(
            &self.root,
            INDEX_TEMP_PREFIX,
            &self.root.join(INDEX_FILE),
            &bytes,
        )
        .map_err(|error| format!("the installed-extensions index could not be saved: {error}"))
    }

    /// Remove what an interrupted install left behind: every staging
    /// directory, index temp files and, when the index is readable, package
    /// directories it does not list. With an unreadable index nothing under
    /// `packages/` is touched.
    pub fn recover(&self) -> Result<Recovered, String> {
        let mut recovered = Recovered::default();
        if !self.root.exists() {
            return Ok(recovered);
        }
        let staging = self.root.join(STAGING_DIR);
        for entry in read_dir_or_empty(&staging).map_err(|error| error.to_string())? {
            remove_tree(&entry).map_err(|error| error.to_string())?;
            recovered.staging += 1;
        }
        for entry in read_dir_or_empty(&self.root).map_err(|error| error.to_string())? {
            let is_temp = entry
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(INDEX_TEMP_PREFIX));
            if is_temp {
                fs::remove_file(&entry).map_err(|error| error.to_string())?;
            }
        }
        let listed: Vec<String> = self
            .read_index()?
            .into_iter()
            .map(|record| record.package_hash)
            .collect();
        for entry in read_dir_or_empty(&self.packages()).map_err(|error| error.to_string())? {
            let name = entry.file_name().and_then(|name| name.to_str());
            if !name.is_some_and(|name| listed.iter().any(|hash| hash == name)) {
                remove_tree(&entry).map_err(|error| error.to_string())?;
                recovered.orphans += 1;
            }
        }
        Ok(recovered)
    }

    /// Copy `source` (a directory or a tar/tar.gz archive) into a fresh
    /// staging tree. Only plain files and directories with canonical relative
    /// paths are accepted; links, devices and escaping names are refused
    /// before anything is written for them. Nothing in the source is executed.
    pub fn stage(&self, source: &Path) -> Result<Staged, String> {
        if !source.is_absolute() {
            return Err("the package path must be absolute".into());
        }
        let metadata = fs::symlink_metadata(source)
            .map_err(|error| format!("the package could not be read: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err("the package path is a symbolic link; choose the package itself".into());
        }
        let staging = self.root.join(STAGING_DIR);
        fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
        let dir = staging.join(uuid::Uuid::new_v4().to_string());
        let tree = dir.join("tree");
        fs::create_dir_all(&tree).map_err(|error| error.to_string())?;
        let staged = Staged {
            dir,
            tree,
            committed: false,
        };
        let mut budget = Budget::default();
        if metadata.is_dir() {
            copy_directory(source, source, staged.tree(), &mut budget)?;
        } else if metadata.is_file() {
            copy_archive(source, staged.tree(), &mut budget)?;
        } else {
            return Err("the package must be a directory or a .tar/.tar.gz archive".into());
        }
        Ok(staged)
    }

    /// Move a verified staged tree into place and list it. Files become
    /// read-only; native entry points also stay executable.
    pub fn commit(
        &self,
        mut staged: Staged,
        validated: &ValidatedManifest,
        record: InstalledRecord,
    ) -> Result<InstalledRecord, String> {
        let executables = validated.executable_paths();
        set_read_only(staged.tree(), EXTENSION_MANIFEST_FILE, false)?;
        for file in &validated.manifest.files {
            set_read_only(
                staged.tree(),
                &file.path,
                executables.contains(file.path.as_str()),
            )?;
        }
        let packages = self.packages();
        fs::create_dir_all(&packages).map_err(|error| error.to_string())?;
        let target = self.package_dir(&validated.package_hash);
        if fs::symlink_metadata(&target).is_ok() {
            // Unlisted (the caller checked the index): an earlier crash left it.
            remove_tree(&target).map_err(|error| error.to_string())?;
        }
        fs::rename(staged.tree(), &target)
            .map_err(|error| format!("the package could not be moved into place: {error}"))?;
        sync_dir(&packages);
        let mut index = self.read_index()?;
        index.push(record.clone());
        if let Err(error) = self.write_index(index) {
            // Not listed, so not installed: take the tree back out.
            let _ = remove_tree(&target);
            return Err(error);
        }
        staged.committed = true;
        let _ = remove_tree(&staged.dir);
        Ok(record)
    }

    /// Unlist one package, then delete its tree (CORE-19). Once the index no
    /// longer lists it, it is not installed; a tree left by a crash before
    /// the deletion is removed by [`Self::recover`].
    pub fn remove(&self, package_hash: &str) -> Result<InstalledRecord, String> {
        let mut index = self.read_index()?;
        let position = index
            .iter()
            .position(|record| record.package_hash == package_hash)
            .ok_or_else(|| "that package is not installed on this device".to_string())?;
        let record = index.remove(position);
        self.write_index(index)?;
        if let Err(error) = remove_tree(&self.package_dir(package_hash)) {
            tracing::warn!(%error, "a removed package's files remain until the next open");
        }
        sync_dir(&self.packages());
        Ok(record)
    }

    /// The installed manifest, re-validated, after checking its bytes still
    /// hash to the identity the index names.
    pub fn read_manifest(&self, package_hash: &str) -> Result<ValidatedManifest, String> {
        let path = self.package_dir(package_hash).join(EXTENSION_MANIFEST_FILE);
        let bytes = read_regular_file(&path, MAX_MANIFEST_BYTES as u64)
            .map_err(|error| format!("the installed manifest could not be read: {error}"))?;
        let validated = manifest::validate(&bytes)?;
        if validated.package_hash != package_hash {
            return Err("the installed manifest changed after installation".into());
        }
        Ok(validated)
    }

    /// Check the installed tree: sizes only (`full: false`) or every byte.
    pub fn integrity(&self, validated: &ValidatedManifest, full: bool) -> ExtensionIntegrity {
        let tree = self.package_dir(&validated.package_hash);
        let result = if full {
            verify_tree(&tree, validated)
        } else {
            validated.manifest.files.iter().try_for_each(|file| {
                let path = tree.join(&file.path);
                match fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.is_file() && metadata.len() == file.size => Ok(()),
                    Ok(_) => Err(format!("{} changed after installation", file.path)),
                    Err(error) => Err(format!("{} is missing: {error}", file.path)),
                }
            })
        };
        match result {
            Ok(()) => ExtensionIntegrity {
                status: if full {
                    ExtensionIntegrityStatus::Verified
                } else {
                    ExtensionIntegrityStatus::Checked
                },
                detail: None,
            },
            Err(detail) => ExtensionIntegrity {
                status: ExtensionIntegrityStatus::Failed,
                detail: Some(detail),
            },
        }
    }
}

/// The exact file set and bytes of `tree` against the manifest: every listed
/// file present with its size and SHA-256, the manifest itself, and nothing
/// else (empty directories aside). Links and special files are refused.
pub(super) fn verify_tree(tree: &Path, validated: &ValidatedManifest) -> Result<(), String> {
    let mut present = Vec::new();
    collect_files(tree, tree, &mut present)?;
    let mut expected: std::collections::BTreeMap<&str, (u64, &str)> = validated
        .manifest
        .files
        .iter()
        .map(|file| (file.path.as_str(), (file.size, file.sha256.as_str())))
        .collect();
    let mut manifest_seen = false;
    for relative in &present {
        if relative == EXTENSION_MANIFEST_FILE {
            manifest_seen = true;
            continue;
        }
        let Some((size, sha256)) = expected.remove(relative.as_str()) else {
            return Err(format!("{relative} is not listed in the manifest"));
        };
        let (actual_size, actual) = hash_file(&tree.join(relative))
            .map_err(|error| format!("{relative} could not be read: {error}"))?;
        if actual_size != size {
            return Err(format!(
                "{relative} is {actual_size} bytes; the manifest says {size}"
            ));
        }
        if actual != sha256 {
            return Err(format!("{relative} does not match its manifest hash"));
        }
    }
    if !manifest_seen {
        return Err(format!("{EXTENSION_MANIFEST_FILE} is missing"));
    }
    if let Some(missing) = expected.keys().next() {
        return Err(format!("{missing} is listed in the manifest but missing"));
    }
    Ok(())
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    let mut entries = fs::read_dir(dir)
        .map_err(|error| error.to_string())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    entries.sort();
    for path in entries {
        let relative = relative_name(root, &path)?;
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!("{relative} is a symbolic link"));
        }
        if metadata.is_dir() {
            collect_files(root, &path, out)?;
        } else if metadata.is_file() {
            out.push(relative);
        } else {
            return Err(format!("{relative} is not a regular file"));
        }
    }
    Ok(())
}

/// `/`-joined path of `path` below `root`, validated like a manifest path.
fn relative_name(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "an entry is outside the package".to_string())?;
    entry_name(relative)
}

/// Accept only `Normal` UTF-8 components; anything else names a location
/// outside the package root.
fn entry_name(path: &Path) -> Result<String, String> {
    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => segments.push(
                segment
                    .to_str()
                    .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))?
                    .to_owned(),
            ),
            Component::CurDir if segments.is_empty() => {}
            _ => {
                return Err(format!("{} escapes the package root", path.display()));
            }
        }
    }
    let name = segments.join("/");
    manifest::check_relative_path(&name)?;
    Ok(name)
}

#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: u64,
}

impl Budget {
    fn entry(&mut self) -> Result<(), String> {
        self.entries += 1;
        if self.entries > MAX_STAGED_ENTRIES {
            return Err(format!(
                "the package holds more than {MAX_STAGED_ENTRIES} entries"
            ));
        }
        Ok(())
    }

    fn remaining(&self) -> u64 {
        MAX_PACKAGE_BYTES + MAX_MANIFEST_BYTES as u64 - self.bytes
    }

    fn spend(&mut self, bytes: u64) -> Result<(), String> {
        if bytes > self.remaining() {
            return Err(format!(
                "the package is larger than {} MiB",
                MAX_PACKAGE_BYTES / (1024 * 1024)
            ));
        }
        self.bytes += bytes;
        Ok(())
    }
}

fn copy_directory(root: &Path, dir: &Path, tree: &Path, budget: &mut Budget) -> Result<(), String> {
    let mut entries = fs::read_dir(dir)
        .map_err(|error| format!("the package could not be read: {error}"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("the package could not be read: {error}"))?;
    entries.sort();
    for path in entries {
        budget.entry()?;
        let relative = relative_name(root, &path)?;
        // Never follow a link, here or when opening.
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        let destination = tree.join(&relative);
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "{relative} is a symbolic link; packages may only contain files and folders"
            ));
        } else if metadata.is_dir() {
            fs::create_dir(&destination).map_err(|error| error.to_string())?;
            copy_directory(root, &path, tree, budget)?;
        } else if metadata.is_file() {
            let mut source = open_no_follow(&path)
                .map_err(|error| format!("{relative} could not be read: {error}"))?;
            if !source.metadata().is_ok_and(|opened| opened.is_file()) {
                return Err(format!("{relative} is not a regular file"));
            }
            write_staged_file(&mut source, &destination, &relative, budget)?;
        } else {
            return Err(format!(
                "{relative} is not a regular file; packages may only contain files and folders"
            ));
        }
    }
    Ok(())
}

fn copy_archive(source: &Path, tree: &Path, budget: &mut Budget) -> Result<(), String> {
    let mut file = open_no_follow(source)
        .map_err(|error| format!("the package could not be read: {error}"))?;
    let mut magic = [0_u8; 2];
    let gzip =
        file.read(&mut magic).map_err(|error| error.to_string())? == 2 && magic == [0x1f, 0x8b];
    let file = open_no_follow(source)
        .map_err(|error| format!("the package could not be read: {error}"))?;
    let reader: Box<dyn Read> = if gzip {
        Box::new(flate2::read::GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|error| format!("the package archive could not be read: {error}"))?;
    for entry in entries {
        budget.entry()?;
        let mut entry =
            entry.map_err(|error| format!("the package archive is invalid: {error}"))?;
        let raw = entry
            .path()
            .map_err(|error| format!("the package archive is invalid: {error}"))?
            .into_owned();
        let kind = entry.header().entry_type();
        // `tar -C package -cf x.tar .` starts with the root itself (`./`).
        if raw
            .components()
            .all(|component| matches!(component, Component::CurDir))
        {
            if kind.is_dir() {
                continue;
            }
            return Err("the package archive has an entry without a name".into());
        }
        let relative = entry_name(&raw)?;
        let destination = tree.join(&relative);
        if kind.is_dir() {
            fs::create_dir_all(&destination).map_err(|error| error.to_string())?;
        } else if kind.is_file() || kind.is_contiguous() {
            if entry.size() > budget.remaining() {
                return Err(format!(
                    "the package is larger than {} MiB",
                    MAX_PACKAGE_BYTES / (1024 * 1024)
                ));
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            write_staged_file(&mut entry, &destination, &relative, budget)?;
        } else if kind.is_symlink() || kind.is_hard_link() {
            return Err(format!(
                "{relative} is a link; packages may only contain files and folders"
            ));
        } else {
            return Err(format!(
                "{relative} is not a regular file; packages may only contain files and folders"
            ));
        }
    }
    Ok(())
}

/// Create one staged file (never replacing one) and copy at most the
/// remaining budget into it.
fn write_staged_file(
    source: &mut dyn Read,
    destination: &Path,
    relative: &str,
    budget: &mut Budget,
) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(destination).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            format!("{relative} appears more than once in the package")
        } else {
            format!("{relative} could not be staged: {error}")
        }
    })?;
    let limit = budget.remaining();
    let copied = io::copy(&mut source.take(limit + 1), &mut file)
        .map_err(|error| format!("{relative} could not be staged: {error}"))?;
    budget.spend(copied)?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(())
}

fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn set_read_only(tree: &Path, relative: &str, executable: bool) -> Result<(), String> {
    let path = tree.join(relative);
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt as _;
        fs::Permissions::from_mode(if executable { 0o555 } else { 0o444 })
    };
    #[cfg(not(unix))]
    let permissions = {
        let _ = executable;
        let mut permissions = fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_readonly(true);
        permissions
    };
    fs::set_permissions(&path, permissions).map_err(|error| error.to_string())
}

pub(super) fn hash_file(path: &Path) -> io::Result<(u64, String)> {
    let mut file = open_no_follow(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((size, crate::repos::hex(&hasher.finalize())))
}

/// Read a regular file (not a link) of at most `max` bytes.
pub(super) fn read_regular_file(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not a regular file of at most {max} bytes",
                path.display()
            ),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    open_no_follow(path)?
        .take(max + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} grew past {max} bytes", path.display()),
        ));
    }
    Ok(bytes)
}

/// Write `bytes` to a fresh temp file in `dir`, fsync it, and rename it over
/// `target`. A crash leaves either the old file or the new one.
pub(super) fn publish_file(
    dir: &Path,
    temp_prefix: &str,
    target: &Path,
    bytes: &[u8],
) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!("{temp_prefix}{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    sync_dir(dir);
    Ok(())
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Err(error) = File::open(dir).and_then(|directory| directory.sync_all()) {
        tracing::warn!(%error, dir = %dir.display(), "extension store directory sync unavailable");
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn read_dir_or_empty(dir: &Path) -> io::Result<Vec<PathBuf>> {
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Remove a tree the store owns, including read-only package files.
fn remove_tree(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() {
        return fs::remove_file(path);
    }
    // Unix removes read-only files from a writable directory; elsewhere the
    // read-only attribute must be cleared first.
    #[cfg(not(unix))]
    clear_read_only(path)?;
    fs::remove_dir_all(path)
}

#[cfg(not(unix))]
fn clear_read_only(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            clear_read_only(&entry?.path())?;
        }
    } else {
        let mut permissions = metadata.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}
