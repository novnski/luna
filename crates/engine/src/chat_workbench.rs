//! Device/profile-local layout persistence. No chat lifecycle or provider calls.
//! A single service is shared by every local RPC client of this engine profile.
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use luna_proto::{
    ChatWorkbenchMutationResult, ChatWorkbenchSnapshot, MutateChatWorkbenchRequest,
    valid_workbench_id,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::EngineError;
use crate::device_write::DeviceWriteHandle;

const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_REQUEST_BYTES: usize = 16 * 1024;
const RECEIPT_LIMIT: usize = 64;

#[derive(Clone)]
pub(crate) struct ChatWorkbenches(Arc<Inner>);

struct Inner {
    root: PathBuf,
    writes: DeviceWriteHandle,
    entries: Mutex<HashMap<String, Entry>>,
}

struct Entry {
    saved: Saved,
    changes: watch::Sender<ChatWorkbenchSnapshot>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Saved {
    snapshot: ChatWorkbenchSnapshot,
    receipts: Vec<Receipt>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    command_id: String,
    payload_hash: String,
}

fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}

impl ChatWorkbenches {
    pub(crate) fn open(
        profile_root: &Path,
        writes: DeviceWriteHandle,
    ) -> Result<Self, EngineError> {
        let _permit = writes
            .try_admit()
            .map_err(|err| error(format!("Workbench storage is unavailable: {err:?}")))?;
        let profile_root = profile_root.canonicalize()?;
        let root = profile_root.join("chat-workbenches-v1");
        std::fs::create_dir_all(&root)?;
        if root.canonicalize()? != root
            || std::fs::symlink_metadata(&root)?.file_type().is_symlink()
        {
            return Err(error(
                "Workbench storage must be a directory inside this profile",
            ));
        }
        Ok(Self(Arc::new(Inner {
            root,
            writes,
            entries: Mutex::new(HashMap::new()),
        })))
    }

    fn path(&self, main: &str) -> PathBuf {
        self.0.root.join(format!(
            "{}.json",
            crate::repos::hex(&Sha256::digest(main.as_bytes()))
        ))
    }

    fn verify_path(&self, path: &Path) -> Result<(), EngineError> {
        if self.0.root.canonicalize()? != self.0.root {
            return Err(error("Workbench storage directory changed"));
        }
        match std::fs::symlink_metadata(path) {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
                Err(error("Workbench storage is not a regular file"))
            }
            Ok(meta) if meta.len() > MAX_FILE_BYTES => {
                Err(error("Saved workbench exceeds its storage limit"))
            }
            Ok(_) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    fn load(&self, main: &str) -> Result<Saved, EngineError> {
        if !valid_workbench_id(main) {
            return Err(error("Invalid main chat ID"));
        }
        let path = self.path(main);
        self.verify_path(&path)?;
        let saved = match std::fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() as u64 > MAX_FILE_BYTES {
                    return Err(error("Saved workbench exceeds its storage limit"));
                }
                serde_json::from_slice::<Saved>(&bytes).map_err(|err| {
                    error(format!(
                        "Saved workbench could not be read; it was preserved: {err}"
                    ))
                })?
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Saved {
                snapshot: ChatWorkbenchSnapshot::new(main),
                receipts: Vec::new(),
            },
            Err(err) => return Err(err.into()),
        };
        saved.snapshot.validate().map_err(|err| {
            error(format!(
                "Saved workbench is invalid; it was preserved: {err}"
            ))
        })?;
        if saved.snapshot.main_chat_id != main || saved.receipts.len() > RECEIPT_LIMIT {
            return Err(error(
                "Saved workbench identity or receipt history is invalid",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        if saved.receipts.iter().any(|receipt| {
            !valid_workbench_id(&receipt.command_id)
                || !seen.insert(receipt.command_id.as_str())
                || receipt.payload_hash.len() != 64
                || !receipt
                    .payload_hash
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(error("Saved workbench command history is invalid"));
        }
        Ok(saved)
    }

    fn entry<'a>(
        &self,
        entries: &'a mut HashMap<String, Entry>,
        main: &str,
    ) -> Result<&'a mut Entry, EngineError> {
        if !entries.contains_key(main) {
            let saved = self.load(main)?;
            let (changes, _) = watch::channel(saved.snapshot.clone());
            entries.insert(main.into(), Entry { saved, changes });
        }
        Ok(entries.get_mut(main).expect("loaded workbench"))
    }

    pub(crate) fn get(&self, main: &str) -> Result<ChatWorkbenchSnapshot, EngineError> {
        let mut entries = self
            .0
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(self.entry(&mut entries, main)?.saved.snapshot.clone())
    }

    pub(crate) fn watch(
        &self,
        main: &str,
    ) -> Result<watch::Receiver<ChatWorkbenchSnapshot>, EngineError> {
        let mut entries = self
            .0
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(self.entry(&mut entries, main)?.changes.subscribe())
    }

    /// Only IDs independently proven missing are removed. A transient metadata
    /// read error must never be interpreted as deletion of a saved layout.
    pub(crate) fn remove_missing(
        &self,
        main: &str,
        missing: &std::collections::HashSet<String>,
    ) -> Result<ChatWorkbenchSnapshot, EngineError> {
        let _permit = self
            .0
            .writes
            .try_admit()
            .map_err(|err| error(format!("Workbench storage is unavailable: {err:?}")))?;
        let mut entries = self
            .0
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = self.entry(&mut entries, main)?;
        let mut next = entry.saved.clone();
        for id in missing.iter().filter(|id| id.as_str() != main) {
            if next.snapshot.contains_chat(id) {
                next.snapshot
                    .apply(&luna_proto::ChatWorkbenchCommand::CloseTab {
                        chat_id: id.clone(),
                    })
                    .map_err(|err| error(err.to_string()))?;
            }
        }
        next.snapshot
            .closed_tabs
            .retain(|tab| !missing.contains(&tab.tab.chat_id));
        if next.snapshot != entry.saved.snapshot {
            next.snapshot.revision = next
                .snapshot
                .revision
                .checked_add(1)
                .ok_or_else(|| error("Workbench revision exhausted"))?;
            next.snapshot
                .validate()
                .map_err(|err| error(err.to_string()))?;
            self.persist(&next)?;
            entry.saved = next;
            entry.changes.send_replace(entry.saved.snapshot.clone());
        }
        Ok(entry.saved.snapshot.clone())
    }

    fn persist(&self, saved: &Saved) -> Result<(), EngineError> {
        let path = self.path(&saved.snapshot.main_chat_id);
        self.verify_path(&path)?;
        let bytes = serde_json::to_vec(saved)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(error("Workbench exceeds its storage limit"));
        }
        let mut temp = tempfile::Builder::new()
            .prefix(".pending-workbench-")
            .tempfile_in(&self.0.root)?;
        temp.write_all(&bytes)?;
        temp.flush()?;
        temp.as_file().sync_all()?;
        temp.persist(&path)
            .map_err(|err| error(format!("Could not save workbench: {err}")))?;
        // Rename is the commit point. A directory fsync problem must not report
        // a pre-commit failure after the command ledger is already committed.
        if let Err(err) = std::fs::File::open(&self.0.root).and_then(|dir| dir.sync_all()) {
            tracing::warn!(%err, "workbench committed; directory durability sync unavailable");
        }
        Ok(())
    }

    pub(crate) fn has_command(&self, main: &str, command_id: &str) -> Result<bool, EngineError> {
        let mut entries = self
            .0
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(self
            .entry(&mut entries, main)?
            .saved
            .receipts
            .iter()
            .any(|receipt| receipt.command_id == command_id))
    }

    pub(crate) fn mutate(
        &self,
        request: &MutateChatWorkbenchRequest,
    ) -> Result<ChatWorkbenchMutationResult, EngineError> {
        if !valid_workbench_id(&request.command_id) {
            return Err(error("Invalid workbench command ID"));
        }
        let bytes = serde_json::to_vec(request)?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(error("Workbench command exceeds its size limit"));
        }
        let fingerprint = crate::repos::hex(&Sha256::digest(bytes));
        let _permit = self
            .0
            .writes
            .try_admit()
            .map_err(|err| error(format!("Workbench storage is unavailable: {err:?}")))?;
        let mut entries = self
            .0
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = self.entry(&mut entries, &request.main_chat_id)?;
        if let Some(receipt) = entry
            .saved
            .receipts
            .iter()
            .find(|receipt| receipt.command_id == request.command_id)
        {
            if receipt.payload_hash != fingerprint {
                return Err(error(
                    "Workbench command ID was already used for another request",
                ));
            }
            return Ok(ChatWorkbenchMutationResult {
                snapshot: entry.saved.snapshot.clone(),
                applied: false,
                conflict: false,
            });
        }
        if entry.saved.snapshot.revision != request.expected_revision {
            return Ok(ChatWorkbenchMutationResult {
                snapshot: entry.saved.snapshot.clone(),
                applied: false,
                conflict: true,
            });
        }
        let mut next = entry.saved.clone();
        let changed = next
            .snapshot
            .apply(&request.command)
            .map_err(|err| error(err.to_string()))?;
        if changed {
            next.snapshot.revision = next
                .snapshot
                .revision
                .checked_add(1)
                .ok_or_else(|| error("Workbench revision exhausted"))?;
        }
        next.receipts.insert(
            0,
            Receipt {
                command_id: request.command_id.clone(),
                payload_hash: fingerprint,
            },
        );
        next.receipts.truncate(RECEIPT_LIMIT);
        self.persist(&next)?;
        entry.saved = next;
        if changed {
            entry.changes.send_replace(entry.saved.snapshot.clone());
        }
        Ok(ChatWorkbenchMutationResult {
            snapshot: entry.saved.snapshot.clone(),
            applied: changed,
            conflict: false,
        })
    }
}
