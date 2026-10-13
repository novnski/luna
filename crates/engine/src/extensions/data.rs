//! Profile-scoped plugin data: `{store root}/extensions/data/<key>/<generation>/`.
//!
//! `<key>` is the extension id with `/` replaced by `+` (ids are lowercase
//! and never contain `+`, so the mapping is one to one). Each generation is a
//! folder named by a UUID; the profile's grants file says which generation is
//! current and which older ones are kept (`grants::DataRecord`). A folder the
//! grants file does not name is either a migration copy that never committed
//! or a kept copy that aged out, and recovery removes it.
//!
//! A migration works on a copy: the current generation's files are copied
//! into a fresh generation, the new version migrates that copy, and only the
//! grants file's rename makes it current. The copy refuses links and special
//! files, so a package cannot smuggle an outside path into its next version's
//! data.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use luna_proto::contract::ExtensionId;

use super::grants::{DataGeneration, DataRecord};

/// Most files and folders one data generation may hold when it is copied.
const MAX_COPY_ENTRIES: usize = 100_000;
/// Largest data generation that is copied for a migration.
const MAX_COPY_BYTES: u64 = 1024 * 1024 * 1024;
/// Older generations kept for rollback, per extension.
pub(super) const MAX_KEPT: usize = 2;
/// Prefix of a generation being erased (renamed first, then deleted).
const ERASING_PREFIX: &str = ".erasing-";

pub(super) struct DataStore {
    root: PathBuf,
}

/// The folder key of an extension id.
pub(super) fn key(id: &ExtensionId) -> String {
    id.as_str().replace('/', "+")
}

impl DataStore {
    pub fn new(store_root: &Path) -> Self {
        Self {
            root: store_root.join("extensions").join("data"),
        }
    }

    fn extension_dir(&self, id: &ExtensionId) -> PathBuf {
        self.root.join(key(id))
    }

    pub fn generation_dir(&self, id: &ExtensionId, generation: &str) -> PathBuf {
        self.extension_dir(id).join(generation)
    }

    /// Create an empty generation folder and make its entry durable.
    pub fn create_empty(&self, id: &ExtensionId) -> Result<String, String> {
        let generation = uuid::Uuid::new_v4().to_string();
        let dir = self.generation_dir(id, &generation);
        fs::create_dir_all(&dir).map_err(|error| {
            format!("the extension's data folder could not be created: {error}")
        })?;
        sync_dir(&self.extension_dir(id));
        sync_dir(&self.root);
        Ok(generation)
    }

    /// Make sure a committed generation's folder exists (it may have been
    /// deleted outside LUNA, or by an erase interrupted after its rename).
    pub fn ensure(&self, id: &ExtensionId, generation: &str) -> Result<PathBuf, String> {
        let dir = self.generation_dir(id, generation);
        match fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.is_dir() => Ok(dir),
            Ok(_) => Err("the extension's data folder is not a folder".into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&dir).map_err(|error| {
                    format!("the extension's data folder could not be created: {error}")
                })?;
                sync_dir(&self.extension_dir(id));
                Ok(dir)
            }
            Err(error) => Err(format!(
                "the extension's data folder is unavailable: {error}"
            )),
        }
    }

    /// Copy generation `from` into a fresh generation, file by file, every
    /// file synced. Only regular files and folders are copied.
    pub fn copy(&self, id: &ExtensionId, from: &str) -> Result<String, String> {
        let source = self.generation_dir(id, from);
        let generation = uuid::Uuid::new_v4().to_string();
        let target = self.generation_dir(id, &generation);
        fs::create_dir_all(&target)
            .map_err(|error| format!("the data copy could not be created: {error}"))?;
        let mut budget = (0_usize, 0_u64);
        let copied = if source.exists() {
            copy_tree(&source, &target, &mut budget)
        } else {
            Ok(())
        };
        if let Err(error) = copied {
            let _ = remove_tree(&target);
            return Err(error);
        }
        sync_dir(&target);
        sync_dir(&self.extension_dir(id));
        Ok(generation)
    }

    /// Delete one generation folder (a discarded copy or an aged-out one).
    pub fn remove_generation(&self, id: &ExtensionId, generation: &str) {
        if let Err(error) = remove_tree(&self.generation_dir(id, generation)) {
            tracing::warn!(%error, "an extension data copy could not be removed; recovery retries");
        }
    }

    /// Erase every generation of `id`. The folder is renamed away first, so
    /// an interrupted erase never leaves a half-deleted current generation.
    pub fn erase(&self, id: &ExtensionId) -> Result<(), String> {
        let dir = self.extension_dir(id);
        if fs::symlink_metadata(&dir).is_err() {
            return Ok(());
        }
        let aside = self
            .root
            .join(format!("{ERASING_PREFIX}{}", uuid::Uuid::new_v4()));
        fs::rename(&dir, &aside)
            .map_err(|error| format!("the extension's data could not be erased: {error}"))?;
        sync_dir(&self.root);
        remove_tree(&aside)
            .map_err(|error| format!("the extension's data could not be erased: {error}"))
    }

    /// Total bytes of regular files in one generation (links not followed).
    pub fn size(&self, id: &ExtensionId, generation: &str) -> u64 {
        tree_size(&self.generation_dir(id, generation))
    }

    pub fn exists(&self, id: &ExtensionId) -> bool {
        self.extension_dir(id).exists()
    }

    /// Remove interrupted erases and every generation folder the grants file
    /// does not name for an extension it has data for. Folders of an id with
    /// no record are left alone: they can only come from a grants file that
    /// was replaced, and deleting them would lose data.
    pub fn recover<'a>(
        &self,
        records: impl Iterator<Item = (&'a String, &'a DataRecord)>,
    ) -> Result<usize, String> {
        let mut removed = 0;
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries.flatten() {
            let is_aside = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(ERASING_PREFIX));
            if is_aside {
                remove_tree(&entry.path()).map_err(|error| error.to_string())?;
                removed += 1;
            }
        }
        for (id, record) in records {
            let Ok(id) = ExtensionId::parse(id) else {
                continue;
            };
            let dir = self.extension_dir(&id);
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let named = name.to_str().is_some_and(|name| {
                    record.current.id == name || record.kept.iter().any(|kept| kept.id == name)
                });
                if !named {
                    remove_tree(&entry.path()).map_err(|error| error.to_string())?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

/// The record after `generation` became current: the previous current one is
/// kept first, the restored one (if it was kept) leaves the kept list, and
/// the list is bounded. Returns the generations that aged out.
pub(super) fn promote(record: &mut DataRecord, generation: DataGeneration) -> Vec<DataGeneration> {
    let previous = std::mem::replace(&mut record.current, generation);
    let current = record.current.id.clone();
    record.kept.retain(|kept| kept.id != current);
    record.kept.insert(0, previous);
    if record.kept.len() > MAX_KEPT {
        record.kept.split_off(MAX_KEPT)
    } else {
        Vec::new()
    }
}

fn copy_tree(source: &Path, target: &Path, budget: &mut (usize, u64)) -> Result<(), String> {
    let mut entries = fs::read_dir(source)
        .map_err(|error| format!("the data could not be read: {error}"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("the data could not be read: {error}"))?;
    entries.sort();
    for path in entries {
        budget.0 += 1;
        if budget.0 > MAX_COPY_ENTRIES {
            return Err(format!(
                "the data holds more than {MAX_COPY_ENTRIES} entries; it was not copied"
            ));
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        let destination = target.join(name);
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "{} is a link; extension data may only hold files and folders",
                path.display()
            ));
        } else if metadata.is_dir() {
            fs::create_dir(&destination).map_err(|error| error.to_string())?;
            copy_tree(&path, &destination, budget)?;
            sync_dir(&destination);
        } else if metadata.is_file() {
            budget.1 += metadata.len();
            if budget.1 > MAX_COPY_BYTES {
                return Err(format!(
                    "the data is larger than {} MiB; it was not copied",
                    MAX_COPY_BYTES / (1024 * 1024)
                ));
            }
            copy_file(&path, &destination)?;
        } else {
            return Err(format!(
                "{} is not a regular file; extension data may only hold files and folders",
                path.display()
            ));
        }
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut input = options
        .open(source)
        .map_err(|error| format!("{} could not be read: {error}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| format!("{} could not be copied: {error}", destination.display()))?;
    io::copy(&mut input, &mut output)
        .map_err(|error| format!("{} could not be copied: {error}", destination.display()))?;
    output
        .sync_all()
        .map_err(|error| format!("{} could not be copied: {error}", destination.display()))
}

fn tree_size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match fs::symlink_metadata(entry.path()) {
            Ok(metadata) if metadata.is_dir() => tree_size(&entry.path()),
            Ok(metadata) if metadata.is_file() => metadata.len(),
            _ => 0,
        })
        .sum()
}

fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Err(error) = fs::File::open(dir).and_then(|directory| directory.sync_all()) {
        tracing::debug!(%error, dir = %dir.display(), "extension data directory sync unavailable");
    }
    #[cfg(not(unix))]
    let _ = dir;
}
