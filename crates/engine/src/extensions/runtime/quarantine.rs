//! Profile-scoped runtime state: `{profile store root}/extension-runtime.json`.
//!
//! It holds the package hashes quarantined in this profile. A quarantined
//! hash never starts again until the owner releases it, across engine
//! restarts. Like the grants file, it names its profile; a copy that lands in
//! another profile quarantines nothing there. It is local only: outside every
//! synced document and outside backups. A file that cannot be read is
//! preserved and every start is refused, so a damaged record never lets a
//! crash-looping package back in.
//!
//! CORE-19 adds the profile's safe-mode setting and the ledger of calls
//! whose worker stopped before answering (`unresolved`), with how each was
//! settled. A file holding either is written as version 2, which an older
//! LUNA refuses (and preserves) instead of dropping them.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use luna_proto::contract::ExtensionId;
use luna_proto::{
    ExtensionCallResolution, ExtensionExitReason, ExtensionQuarantineView,
    ExtensionUnresolvedCallView,
};
use serde::{Deserialize, Serialize};

use crate::device_write::DeviceWriteHandle;
use crate::extensions::store::{publish_file, read_regular_file};

const STATE_FILE: &str = "extension-runtime.json";
const STATE_VERSION: u32 = 1;
/// Written when the file holds safe mode or unresolved calls (CORE-19).
const STATE_VERSION_LIFECYCLE: u32 = 2;
/// Unresolved and settled calls kept per profile; settled ones go first.
const MAX_UNRESOLVED: usize = 256;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SafeModeRecord {
    pub enabled_at_ms: i64,
}

/// A call whose worker stopped before answering (CORE-19).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UnresolvedRecord {
    pub call_id: String,
    #[serde(default)]
    pub request_id: Option<String>,
    pub extension_id: ExtensionId,
    pub version: String,
    pub package_hash: String,
    pub instance_id: String,
    pub generation: u32,
    pub method: String,
    pub started_at_ms: i64,
    pub interrupted_at_ms: i64,
    pub exit_reason: ExtensionExitReason,
    #[serde(default)]
    pub resolution: Option<ExtensionCallResolution>,
}

impl UnresolvedRecord {
    fn view(&self) -> ExtensionUnresolvedCallView {
        ExtensionUnresolvedCallView {
            call_id: self.call_id.clone(),
            request_id: self.request_id.clone(),
            extension_id: self.extension_id.clone(),
            version: self.version.clone(),
            package_hash: self.package_hash.clone(),
            instance_id: self.instance_id.clone(),
            generation: self.generation,
            method: self.method.clone(),
            started_at_ms: self.started_at_ms,
            interrupted_at_ms: self.interrupted_at_ms,
            exit_reason: self.exit_reason,
            resolution: self.resolution.clone(),
        }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    safe_mode: Option<SafeModeRecord>,
    /// Oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unresolved: Vec<UnresolvedRecord>,
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
            safe_mode: None,
            unresolved: Vec::new(),
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
        if file.version != STATE_VERSION && file.version != STATE_VERSION_LIFECYCLE {
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

    /// Apply `change` to a copy, save it, then keep it (nothing changes in
    /// memory when the save fails).
    fn change<T>(&self, change: impl FnOnce(&mut StateFile) -> T) -> Result<T, String> {
        let mut state = self.lock();
        let file = match &mut *state {
            Ok(file) => file,
            Err(error) => return Err(error.clone()),
        };
        let mut next = file.clone();
        let value = change(&mut next);
        next.revision = self.save(next.clone())?;
        *file = next;
        Ok(value)
    }

    /// Drop quarantine records of packages no longer installed on this
    /// device (a removal interrupted before it released them). Returns how
    /// many were dropped.
    pub fn prune(&self, installed: &[String]) -> Result<usize, String> {
        let stale = match &*self.lock() {
            Ok(file) => file
                .quarantined
                .keys()
                .filter(|hash| !installed.iter().any(|installed| installed == *hash))
                .count(),
            Err(error) => return Err(error.clone()),
        };
        if stale == 0 {
            return Ok(0);
        }
        self.change(|file| {
            file.quarantined
                .retain(|hash, _| installed.iter().any(|installed| installed == hash));
            stale
        })
    }

    /// The profile's safe-mode setting; `Err` when the state is unreadable.
    pub fn safe_mode(&self) -> Result<Option<SafeModeRecord>, String> {
        match &*self.lock() {
            Ok(file) => Ok(file.safe_mode.clone()),
            Err(error) => Err(error.clone()),
        }
    }

    pub fn set_safe_mode(&self, record: Option<SafeModeRecord>) -> Result<(), String> {
        self.change(|file| file.safe_mode = record)
    }

    /// Record calls whose worker stopped before answering. Like a
    /// quarantine, they apply in memory even when they cannot be saved, so
    /// a resend is still refused by this engine.
    pub fn record_unresolved(&self, records: Vec<UnresolvedRecord>) -> Result<(), String> {
        if records.is_empty() {
            return Ok(());
        }
        let mut state = self.lock();
        let file = match &mut *state {
            Ok(file) => file,
            Err(error) => return Err(error.clone()),
        };
        file.unresolved.extend(records);
        while file.unresolved.len() > MAX_UNRESOLVED {
            let position = file
                .unresolved
                .iter()
                .position(|record| record.resolution.is_some())
                .unwrap_or(0);
            let dropped = file.unresolved.remove(position);
            if dropped.resolution.is_none() {
                tracing::warn!(
                    call = %dropped.call_id,
                    "the oldest unresolved extension call was dropped from the bounded ledger"
                );
            }
        }
        let snapshot = file.clone();
        self.save(snapshot).map(|revision| file.revision = revision)
    }

    /// Unresolved calls of `extension_id`, oldest first, at most `limit`.
    pub fn pending_for(&self, extension_id: &ExtensionId, limit: usize) -> Vec<UnresolvedRecord> {
        match &*self.lock() {
            Ok(file) => file
                .unresolved
                .iter()
                .filter(|record| {
                    record.extension_id == *extension_id && record.resolution.is_none()
                })
                .take(limit)
                .cloned()
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The newest record of `request_id` for `extension_id`.
    pub fn request(
        &self,
        extension_id: &ExtensionId,
        request_id: &str,
    ) -> Result<Option<UnresolvedRecord>, String> {
        match &*self.lock() {
            Ok(file) => Ok(file
                .unresolved
                .iter()
                .rev()
                .find(|record| {
                    record.extension_id == *extension_id
                        && record.request_id.as_deref() == Some(request_id)
                })
                .cloned()),
            Err(error) => Err(error.clone()),
        }
    }

    /// Settle unresolved calls by call id. Returns how many were settled.
    pub fn resolve(
        &self,
        resolutions: Vec<(String, ExtensionCallResolution)>,
    ) -> Result<usize, String> {
        if resolutions.is_empty() {
            return Ok(0);
        }
        self.change(|file| {
            let mut settled = 0;
            for (call_id, resolution) in resolutions {
                if let Some(record) = file
                    .unresolved
                    .iter_mut()
                    .find(|record| record.call_id == call_id && record.resolution.is_none())
                {
                    record.resolution = Some(resolution);
                    settled += 1;
                }
            }
            settled
        })
    }

    pub fn unresolved_views(&self) -> Vec<ExtensionUnresolvedCallView> {
        match &*self.lock() {
            Ok(file) => file.unresolved.iter().map(UnresolvedRecord::view).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Write `file` with its revision advanced (temp file, fsync, rename).
    fn save(&self, mut file: StateFile) -> Result<u64, String> {
        let _permit = self.writes.try_admit().map_err(|error| {
            format!("extension runtime state is temporarily unavailable ({error:?}); retry")
        })?;
        file.version = if file.safe_mode.is_none() && file.unresolved.is_empty() {
            STATE_VERSION
        } else {
            STATE_VERSION_LIFECYCLE
        };
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
