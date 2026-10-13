//! Profile-scoped runtime state: `{profile store root}/extension-runtime.json`.
//!
//! It holds the package hashes quarantined in this profile. A quarantined
//! hash never starts again until the owner releases it, across engine
//! restarts. Like the grants file, it names its profile; a copy that lands in
//! another profile quarantines nothing there. It is local only: outside every
//! synced document and outside backups. A file that cannot be read is
//! preserved and every start is refused, so a damaged record never lets a
//! crash-looping package back in.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use luna_proto::ExtensionQuarantineView;
use luna_proto::contract::ExtensionId;
use serde::{Deserialize, Serialize};

use crate::device_write::DeviceWriteHandle;
use crate::extensions::store::{publish_file, read_regular_file};

const STATE_FILE: &str = "extension-runtime.json";
const STATE_VERSION: u32 = 1;
const STATE_TEMP_PREFIX: &str = ".extension-runtime-";
const MAX_STATE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QuarantineRecord {
    pub extension_id: ExtensionId,
    pub version: String,
    pub reason: String,
    pub faults: u32,
    pub quarantined_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateFile {
    version: u32,
    profile_id: String,
    revision: u64,
    /// Package hash → why it is quarantined.
    #[serde(default)]
    quarantined: BTreeMap<String, QuarantineRecord>,
}

pub(super) struct QuarantineStore {
    root: PathBuf,
    profile_id: String,
    writes: DeviceWriteHandle,
    /// The loaded file, or why it could not be read.
    state: Mutex<Result<StateFile, String>>,
}

impl QuarantineStore {
    pub fn open(store_root: &Path, profile_id: String, writes: DeviceWriteHandle) -> Self {
        let store = Self {
            root: store_root.to_path_buf(),
            profile_id,
            writes,
            state: Mutex::new(Err("not loaded".into())),
        };
        store.recover();
        let loaded = store.load();
        *store.lock() = loaded;
        store
    }

    fn lock(&self) -> MutexGuard<'_, Result<StateFile, String>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn empty(&self) -> StateFile {
        StateFile {
            version: STATE_VERSION,
            profile_id: self.profile_id.clone(),
            revision: 0,
            quarantined: BTreeMap::new(),
        }
    }

    /// Remove temp files an interrupted write left; they were never renamed
    /// into place.
    fn recover(&self) {
        let Ok(_permit) = self.writes.try_admit() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            let is_temp = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(STATE_TEMP_PREFIX));
            if !is_temp || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            if let Err(error) = std::fs::remove_file(entry.path()) {
                tracing::warn!(%error, "extension runtime temp file could not be removed");
            }
        }
    }

    fn load(&self) -> Result<StateFile, String> {
        let path = self.root.join(STATE_FILE);
        let bytes = match read_regular_file(&path, MAX_STATE_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(self.empty()),
            Err(error) => {
                return Err(format!(
                    "extension runtime state could not be read; it was preserved: {error}"
                ));
            }
        };
        let file: StateFile = serde_json::from_slice(&bytes).map_err(|error| {
            format!("extension runtime state is invalid; it was preserved: {error}")
        })?;
        if file.version != STATE_VERSION {
            return Err(format!(
                "extension runtime state has unsupported version {}; it was preserved",
                file.version
            ));
        }
        if file.profile_id != self.profile_id {
            tracing::warn!(
                path = %path.display(),
                "extension runtime state belongs to another profile or device; ignoring it"
            );
            return Ok(StateFile {
                revision: file.revision,
                ..self.empty()
            });
        }
        Ok(file)
    }

    /// Why starts are refused, when the state is unreadable.
    pub fn error(&self) -> Option<String> {
        self.lock().as_ref().err().cloned()
    }

    /// `Err` when the state is unreadable: callers refuse to start.
    pub fn get(&self, package_hash: &str) -> Result<Option<QuarantineRecord>, String> {
        match &*self.lock() {
            Ok(file) => Ok(file.quarantined.get(package_hash).cloned()),
            Err(error) => Err(error.clone()),
        }
    }

    pub fn views(&self) -> Vec<ExtensionQuarantineView> {
        match &*self.lock() {
            Ok(file) => file
                .quarantined
                .iter()
                .map(|(hash, record)| ExtensionQuarantineView {
                    extension_id: record.extension_id.clone(),
                    version: record.version.clone(),
                    package_hash: hash.clone(),
                    reason: record.reason.clone(),
                    faults: record.faults,
                    quarantined_at_ms: record.quarantined_at_ms,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Quarantine `package_hash`. The record applies in memory even when it
    /// cannot be saved, so the package stays refused in this engine.
    pub fn insert(&self, package_hash: &str, record: QuarantineRecord) -> Result<(), String> {
        let mut state = self.lock();
        let file = match &mut *state {
            Ok(file) => file,
            Err(error) => return Err(error.clone()),
        };
        file.quarantined.insert(package_hash.to_owned(), record);
        let snapshot = file.clone();
        self.save(snapshot).map(|revision| file.revision = revision)
    }

    /// Release `package_hash`; `false` when it was not quarantined.
    pub fn remove(&self, package_hash: &str) -> Result<bool, String> {
        let mut state = self.lock();
        let file = match &mut *state {
            Ok(file) => file,
            Err(error) => return Err(error.clone()),
        };
        if !file.quarantined.contains_key(package_hash) {
            return Ok(false);
        }
        let mut next = file.clone();
        next.quarantined.remove(package_hash);
        let revision = self.save(next.clone())?;
        next.revision = revision;
        *file = next;
        Ok(true)
    }

    /// Write `file` with its revision advanced (temp file, fsync, rename).
    fn save(&self, mut file: StateFile) -> Result<u64, String> {
        let _permit = self.writes.try_admit().map_err(|error| {
            format!("extension runtime state is temporarily unavailable ({error:?}); retry")
        })?;
        file.version = STATE_VERSION;
        file.profile_id = self.profile_id.clone();
        file.revision = file
            .revision
            .checked_add(1)
            .ok_or("extension runtime state revision exhausted")?;
        let mut bytes = serde_json::to_vec_pretty(&file).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err("extension runtime state exceeds its storage limit".into());
        }
        publish_file(
            &self.root,
            STATE_TEMP_PREFIX,
            &self.root.join(STATE_FILE),
            &bytes,
        )
        .map_err(|error| format!("extension runtime state could not be saved: {error}"))?;
        Ok(file.revision)
    }
}
