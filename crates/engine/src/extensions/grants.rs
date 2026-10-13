//! Per-profile enablement and grants: `{profile store root}/extension-grants.json`.
//!
//! A record binds one extension id to one exact package hash and the exact
//! capability ids the user granted. The file names the profile it belongs to
//! (`profileId`, which covers the device, scope and account); a file copied,
//! restored or synced into another profile grants nothing there. The file is
//! local only: it is outside every synced document and outside backups.
//!
//! CORE-19 adds the profile's plugin data pointers (`data`): for each
//! extension id, its current data generation and the older copies kept for
//! rollback. Enablement and data switch in one write, so a version switch
//! has one commit point: this file's rename. A file holding data pointers is
//! written as version 2, which an older LUNA refuses (and preserves) rather
//! than rewriting it without them.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::store::{publish_file, read_regular_file};

const GRANTS_FILE: &str = "extension-grants.json";
const GRANTS_VERSION: u32 = 1;
/// Written when the file holds data pointers (CORE-19).
const GRANTS_VERSION_DATA: u32 = 2;
const GRANTS_TEMP_PREFIX: &str = ".extension-grants-";
const MAX_GRANTS_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GrantRecord {
    pub package_hash: String,
    pub version: String,
    /// Sorted, unique capability ids.
    pub grants: Vec<String>,
    pub enabled_at_ms: i64,
}

/// One generation of an extension's data: the folder
/// `{store root}/extensions/data/<key>/<id>/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DataGeneration {
    pub id: String,
    pub schema_version: u32,
    /// The package version (and hash) that created or migrated it.
    pub version: String,
    pub package_hash: String,
    pub created_at_ms: i64,
}

/// This profile's data for one extension id. It outlives enablement and
/// installation; only an explicit erase removes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DataRecord {
    pub current: DataGeneration,
    /// Older generations kept for rollback, newest first.
    #[serde(default)]
    pub kept: Vec<DataGeneration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GrantsFile {
    version: u32,
    profile_id: String,
    pub revision: u64,
    /// Extension id → its one enabled package.
    pub extensions: BTreeMap<String, GrantRecord>,
    /// Extension id → its data in this profile (CORE-19).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, DataRecord>,
}

pub(super) struct ProfileGrants {
    root: PathBuf,
    profile_id: String,
}

impl ProfileGrants {
    pub fn new(store_root: &Path, profile_id: String) -> Self {
        Self {
            root: store_root.to_path_buf(),
            profile_id,
        }
    }

    fn empty(&self) -> GrantsFile {
        GrantsFile {
            version: GRANTS_VERSION,
            profile_id: self.profile_id.clone(),
            revision: 0,
            extensions: BTreeMap::new(),
            data: BTreeMap::new(),
        }
    }

    /// This profile's grants. Missing means none. A file written for another
    /// profile is ignored (and replaced by the next change here); a corrupt
    /// one is an error and is preserved.
    pub fn read(&self) -> Result<GrantsFile, String> {
        let path = self.root.join(GRANTS_FILE);
        let bytes = match read_regular_file(&path, MAX_GRANTS_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(self.empty()),
            Err(error) => {
                return Err(format!(
                    "extension grants could not be read; the file was preserved: {error}"
                ));
            }
        };
        let file: GrantsFile = serde_json::from_slice(&bytes).map_err(|error| {
            format!("extension grants are invalid; the file was preserved: {error}")
        })?;
        if file.version != GRANTS_VERSION && file.version != GRANTS_VERSION_DATA {
            return Err(format!(
                "extension grants have unsupported version {}; the file was preserved",
                file.version
            ));
        }
        if file.profile_id != self.profile_id {
            tracing::warn!(
                path = %path.display(),
                "extension grants belong to another profile or device; ignoring them"
            );
            return Ok(GrantsFile {
                revision: file.revision,
                ..self.empty()
            });
        }
        Ok(file)
    }

    /// Publish `file` with its revision advanced.
    pub fn write(&self, mut file: GrantsFile) -> Result<GrantsFile, String> {
        file.version = if file.data.is_empty() {
            GRANTS_VERSION
        } else {
            GRANTS_VERSION_DATA
        };
        file.profile_id = self.profile_id.clone();
        file.revision = file
            .revision
            .checked_add(1)
            .ok_or("extension grants revision exhausted")?;
        let mut bytes = serde_json::to_vec_pretty(&file).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_GRANTS_BYTES {
            return Err("extension grants exceed their storage limit".into());
        }
        publish_file(
            &self.root,
            GRANTS_TEMP_PREFIX,
            &self.root.join(GRANTS_FILE),
            &bytes,
        )
        .map_err(|error| format!("extension grants could not be saved: {error}"))?;
        Ok(file)
    }

    /// Remove temp files an interrupted write left. They were never renamed
    /// into place, so they never granted anything.
    pub fn recover(&self) -> Result<usize, String> {
        let mut removed = 0;
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let is_temp = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(GRANTS_TEMP_PREFIX));
            if is_temp && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                std::fs::remove_file(entry.path()).map_err(|error| error.to_string())?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}
