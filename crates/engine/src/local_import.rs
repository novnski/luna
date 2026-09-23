//! One-time local→synced profile import.
//!
//! A device that worked locally (PR #30's `profiles/local`) and then signs in
//! gets its chats, spaces, journals, and command-ledger claims carried into
//! the synced profile — through the same write paths every live mutation
//! uses, so nothing here invents a second persistence or sync mechanism:
//!
//!  - chat docs land via `save_snapshot_with_cursor(chat_id, bytes, 0, CHAT2_DOC_EPOCH)`,
//!    which is exactly the "born chat2" shape: cursor 0 makes the first room
//!    join push the doc's full update log from VV zero (doc_host first-contact
//!    push), and epoch 2 keeps `DocHost::open` off the s2 discard-and-adopt
//!    branch that would drop the transcript.
//!  - registry rows go through `WorkspaceHost::import_chat_row` /
//!    `import_space_row` (live upserts: persisted, pushed, watched).
//!  - attachments are NOT copied or rewritten. Transcripts embed absolute
//!    paths under the local profile's uploads root, so that root becomes a
//!    read-only jail root of the synced profile — the exact mechanism the
//!    legacy-uploads adoption already uses (`EngineProfile::claim_legacy_uploads_root`).
//!    The marker file re-arms the root on every later synced boot.
//!
//! Idempotence is structural: a chat/space whose row already exists in the
//! target is skipped, so re-running after a partial import (or after new
//! local work from a later signed-out stretch) imports only what's missing.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zeron_doc::{REGISTRY_DOC_ID, RegistryDoc};
use zeron_sync::DocsStore;

use crate::EngineError;
use crate::chat2_host::CHAT2_DOC_EPOCH;
use crate::device_write::{DeviceWriteHandle, DeviceWritePermit};
use crate::run_journal::{journal_paths, runtime_receipt_paths};
use crate::uploads::Uploads;
use crate::workspace_host::WorkspaceHost;

/// Marker recording completed imports, at `{data_dir}/local-import.json`.
/// One entry per target (org, user): the same device may sign into several
/// accounts, and each gets its own one-time import.
const MARKER_FILE: &str = "local-import.json";

/// Serializes every marker read-modify-write in this process. Two runtimes in
/// one process (a swap mid-flight, tests) must not lose an account's entry to
/// a racing update; cross-process exclusion is the data-dir `InstanceLock`'s job.
fn marker_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Marker {
    #[serde(default)]
    imports: Vec<MarkerEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarkerEntry {
    org_id: String,
    user_id: String,
    imported_at_ms: i64,
    imported_chats: usize,
    imported_spaces: usize,
}

/// What the wizard needs to offer (or silently skip) the import step.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalImportStatus {
    /// Chats in the local profile with no row in this synced workspace yet.
    pub available_chats: usize,
    /// Spaces in the local profile with no row in this synced workspace yet.
    pub available_spaces: usize,
    /// A completed import for this (org, user) is already on record.
    pub imported_before: bool,
}

/// Per-item progress for the wizard's progress step.
/// NB: `rename_all` renames the *variants* (the `kind` tag); struct-variant
/// *fields* need `rename_all_fields` — without it the wire shape silently
/// ships snake_case counters (caught by `import_events_serialize_camel_case`).
#[derive(Debug, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum ImportEvent {
    /// Emitted once, before any copying.
    Start { chats: usize, spaces: usize },
    /// One imported chat (docs + journal + row).
    Chat {
        index: usize,
        total: usize,
        chat_id: String,
        title: Option<String>,
    },
    /// Terminal summary — also the RPC's final stream item.
    Summary {
        imported_chats: usize,
        imported_spaces: usize,
        skipped_chats: usize,
        skipped_spaces: usize,
        journals_copied: usize,
        ledger_rows_merged: usize,
        errors: Vec<String>,
    },
}

/// The import service a synced runtime carries (`EngineCore::assemble` builds
/// one when the profile is account-scoped and a local profile exists on disk).
#[derive(Clone)]
pub struct LocalImporter {
    inner: Arc<ImporterInner>,
}

struct ImporterInner {
    data_dir: PathBuf,
    device_id: String,
    org_id: String,
    user_id: String,
    target_store: Arc<DocsStore>,
    target_journals: PathBuf,
    workspace: WorkspaceHost,
    uploads: Uploads,
    write_handle: Option<DeviceWriteHandle>,
    /// Importer clones share this lock so their dedup reads and file publishes
    /// cannot race. Acquire it only after the optional device write ticket.
    run_lock: Mutex<()>,
    #[cfg(test)]
    before_run_lock: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl LocalImporter {
    /// Legacy standalone constructor without device-write admission. Product
    /// engine assembly uses [`Self::new_guarded`] so freezes reject new work.
    #[allow(clippy::too_many_arguments)] // engine assembly seam, not a public API
    pub fn new(
        data_dir: &Path,
        device_id: &str,
        org_id: &str,
        user_id: &str,
        target_store: Arc<DocsStore>,
        target_journals: PathBuf,
        workspace: WorkspaceHost,
        uploads: Uploads,
    ) -> Self {
        Self::new_with_handle(
            data_dir,
            device_id,
            org_id,
            user_id,
            target_store,
            target_journals,
            workspace,
            uploads,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_guarded(
        data_dir: &Path,
        device_id: &str,
        org_id: &str,
        user_id: &str,
        target_store: Arc<DocsStore>,
        target_journals: PathBuf,
        workspace: WorkspaceHost,
        uploads: Uploads,
        write_handle: DeviceWriteHandle,
    ) -> Self {
        Self::new_with_handle(
            data_dir,
            device_id,
            org_id,
            user_id,
            target_store,
            target_journals,
            workspace,
            uploads,
            Some(write_handle),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_handle(
        data_dir: &Path,
        device_id: &str,
        org_id: &str,
        user_id: &str,
        target_store: Arc<DocsStore>,
        target_journals: PathBuf,
        workspace: WorkspaceHost,
        uploads: Uploads,
        write_handle: Option<DeviceWriteHandle>,
    ) -> Self {
        Self {
            inner: Arc::new(ImporterInner {
                data_dir: data_dir.to_path_buf(),
                device_id: device_id.to_string(),
                org_id: org_id.to_string(),
                user_id: user_id.to_string(),
                target_store,
                target_journals,
                workspace,
                uploads,
                write_handle,
                run_lock: Mutex::new(()),
                #[cfg(test)]
                before_run_lock: Mutex::new(None),
            }),
        }
    }

    #[cfg(test)]
    fn copy_journal_with_handle(
        source: &Path,
        destination: &Path,
        target_journals: &Path,
        write_handle: Option<&DeviceWriteHandle>,
    ) -> Result<(), EngineError> {
        let _permit = write_handle
            .map(DeviceWriteHandle::try_admit)
            .transpose()
            .map_err(|error| {
                EngineError::Other(format!(
                    "journal import temporarily unavailable ({error:?})"
                ))
            })?;
        Self::copy_file_atomic(source, destination, target_journals)
    }

    /// Publish a complete file once. `run` calls this under its outer ticket;
    /// the standalone journal helper admits its own ticket above.
    fn copy_file_atomic(
        source: &Path,
        destination: &Path,
        target_journals: &Path,
    ) -> Result<(), EngineError> {
        std::fs::create_dir_all(target_journals)?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".local-import-")
            .suffix(".tmp")
            .tempfile_in(target_journals)?;
        let mut source_file = std::fs::File::open(source)?;
        std::io::copy(&mut source_file, &mut temporary)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        temporary
            .into_temp_path()
            .persist_noclobber(destination)
            .map_err(std::io::Error::from)?;
        #[cfg(unix)]
        std::fs::File::open(target_journals)?.sync_all()?;
        Ok(())
    }

    fn source_root(&self) -> PathBuf {
        self.inner.data_dir.join("profiles").join("local")
    }

    fn source_uploads(&self) -> PathBuf {
        self.source_root().join("uploads")
    }

    fn marker_path(&self) -> PathBuf {
        self.inner.data_dir.join(MARKER_FILE)
    }

    /// Load the marker under [`marker_lock`]. A file that exists but does not
    /// parse is moved aside to `local-import.json.corrupt` — its grants were
    /// already unreadable (boot's [`marker_grants_read_root`] parses the same
    /// file), and keeping the bytes as evidence beats silently clobbering them
    /// on the next write.
    fn load_marker_locked(&self) -> Marker {
        let path = self.marker_path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => return Marker::default(),
        };
        match serde_json::from_str(&raw) {
            Ok(marker) => marker,
            Err(err) => {
                let aside = path.with_extension("json.corrupt");
                tracing::warn!(error = %err, aside = %aside.display(),
                    "local-import marker is corrupt; moving it aside");
                if let Err(err) = std::fs::rename(&path, &aside) {
                    tracing::warn!(error = %err, "corrupt marker aside failed");
                }
                Marker::default()
            }
        }
    }

    fn imported_before(&self) -> bool {
        let _guard = marker_lock();
        self.load_marker_locked()
            .imports
            .iter()
            .any(|e| e.org_id == self.inner.org_id && e.user_id == self.inner.user_id)
    }

    /// Record a completed import and re-arm the read-only uploads root for
    /// future boots (`EngineCore::assemble` consults [`marker_grants_read_root`]).
    ///
    /// The read-modify-write is serialized process-wide (cross-process is
    /// already excluded by the data-dir `InstanceLock`) and published
    /// atomically — write + fsync a sibling temp file, then rename over the
    /// marker — so a crash or short write can never truncate the file and
    /// erase other accounts' grants. Failures propagate to the caller and end
    /// up in the import summary.
    fn record_import(&self, chats: usize, spaces: usize) -> Result<(), EngineError> {
        let _guard = marker_lock();
        let mut marker = self.load_marker_locked();
        marker
            .imports
            .retain(|e| !(e.org_id == self.inner.org_id && e.user_id == self.inner.user_id));
        marker.imports.push(MarkerEntry {
            org_id: self.inner.org_id.clone(),
            user_id: self.inner.user_id.clone(),
            imported_at_ms: crate::now_ms(),
            imported_chats: chats,
            imported_spaces: spaces,
        });
        let bytes = serde_json::to_vec_pretty(&marker)
            .map_err(|err| EngineError::Other(format!("marker serialize: {err}")))?;
        let path = self.marker_path();
        let tmp = path.with_extension("json.tmp");
        let publish = || -> std::io::Result<()> {
            {
                use std::io::Write;
                let mut file = std::fs::File::create(&tmp)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
            }
            std::fs::rename(&tmp, &path)
        };
        publish().map_err(|err| {
            let _ = std::fs::remove_file(&tmp);
            EngineError::Other(format!("marker write: {err}"))
        })
    }

    /// Open the local profile's stores read-only-ish. `None` when the device
    /// never ran a local profile (nothing to import).
    fn open_source(&self) -> Result<Option<(DocsStore, RegistryDoc)>, EngineError> {
        let root = self.source_root();
        if !root.join("docs.sqlite3").is_file() {
            return Ok(None);
        }
        let store = DocsStore::open(&root)?;
        let Some(bytes) = store.load_snapshot(REGISTRY_DOC_ID)? else {
            return Ok(None); // store exists but no registry replica — nothing rowed
        };
        let registry = RegistryDoc::from_bytes(&bytes, &self.inner.device_id)?;
        Ok(Some((store, registry)))
    }

    /// What's importable right now (target-dedup applied).
    pub fn status(&self) -> Result<LocalImportStatus, EngineError> {
        // This read can quarantine a corrupt marker and DocsStore::open can
        // migrate the source SQLite schema. Hold admission through both and
        // the target-dedup reads; a freeze must not interleave those writes.
        let _permit = self
            .inner
            .write_handle
            .as_ref()
            .map(DeviceWriteHandle::try_admit)
            .transpose()
            .map_err(|error| {
                use zeron_sync::write_epoch::EpochError;
                let detail = match error {
                    EpochError::Busy => "backup snapshot in progress; retry after it finishes",
                    EpochError::Retired => "engine writer retired; retry after engine restart",
                    _ => "device writer unavailable; retry after engine restart",
                };
                EngineError::Other(format!(
                    "local import status unavailable ({error:?}): {detail}"
                ))
            })?;
        let imported_before = self.imported_before();
        let Some((_, registry)) = self.open_source()? else {
            return Ok(LocalImportStatus {
                available_chats: 0,
                available_spaces: 0,
                imported_before,
            });
        };
        let mut available_chats = 0;
        for chat in registry.read_chats()? {
            if self.inner.workspace.chat(&chat.id)?.is_none() {
                available_chats += 1;
            }
        }
        let mut available_spaces = 0;
        for space in registry.read_spaces()? {
            if self.inner.workspace.space(&space.id)?.is_none() {
                available_spaces += 1;
            }
        }
        Ok(LocalImportStatus {
            available_chats,
            available_spaces,
            imported_before,
        })
    }

    /// Run the import, emitting [`ImportEvent`]s (the last is always
    /// `Summary`). Blocking (sqlite + fs) — callers run it off the async path.
    pub fn run(&self, mut emit: impl FnMut(ImportEvent)) -> Result<(), EngineError> {
        // Source open may migrate SQLite, and the marker read below may move a
        // corrupt file aside. Admit before either, then retain the same ticket
        // through every target write and the terminal progress event. The
        // public `new` constructor remains unguarded for standalone callers;
        // product assembly uses `new_guarded`.
        let permit = self
            .inner
            .write_handle
            .as_ref()
            .map(DeviceWriteHandle::try_admit)
            .transpose()
            .map_err(|error| {
                use zeron_sync::write_epoch::EpochError;
                let detail = match error {
                    EpochError::Busy => "backup snapshot in progress; retry after it finishes",
                    EpochError::Retired => "engine writer retired; retry after engine restart",
                    _ => "device writer unavailable; retry after engine restart",
                };
                EngineError::Other(format!("local import unavailable ({error:?}): {detail}"))
            })?;
        #[cfg(test)]
        if let Some(hook) = self
            .inner
            .before_run_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            hook();
        }
        let _run = self
            .inner
            .run_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((source_store, registry)) = self.open_source()? else {
            emit(ImportEvent::Start {
                chats: 0,
                spaces: 0,
            });
            emit(ImportEvent::Summary {
                imported_chats: 0,
                imported_spaces: 0,
                skipped_chats: 0,
                skipped_spaces: 0,
                journals_copied: 0,
                ledger_rows_merged: 0,
                errors: Vec::new(),
            });
            return Ok(());
        };

        let mut errors: Vec<String> = Vec::new();

        // Spaces first: chats reference `space_id`, and viewers resolve the
        // reference as soon as the chat row lands.
        let spaces = registry.read_spaces()?;
        let chats = registry.read_chats()?;
        let (total_chats, total_spaces) = (chats.len(), spaces.len());
        let pending_chats: Vec<_> = chats
            .into_iter()
            .filter(|chat| !matches!(self.inner.workspace.chat(&chat.id), Ok(Some(_))))
            .collect();
        let pending_spaces: Vec<_> = spaces
            .into_iter()
            .filter(|space| !matches!(self.inner.workspace.space(&space.id), Ok(Some(_))))
            .collect();
        let skipped_chats = total_chats - pending_chats.len();
        let skipped_spaces = total_spaces - pending_spaces.len();

        emit(ImportEvent::Start {
            chats: pending_chats.len(),
            spaces: pending_spaces.len(),
        });

        // Claim source command IDs before publishing imported chat rows. A
        // ledger failure leaves this attempt incomplete and retryable.
        let ledger = source_store
            .processed_commands()
            .and_then(|rows| self.inner.target_store.import_processed_commands(&rows));
        let (ledger_rows_merged, ledger_ok) = match ledger {
            Ok(count) => (count, true),
            Err(err) => {
                errors.push(format!("command ledger: {err}"));
                (0, false)
            }
        };

        let mut imported_spaces = 0;
        if ledger_ok {
            for space in &pending_spaces {
                let result = match permit.as_ref() {
                    Some(permit) => self
                        .inner
                        .workspace
                        .import_space_row_admitted(space, permit),
                    None => self.inner.workspace.import_space_row(space),
                };
                match result {
                    Ok(()) => imported_spaces += 1,
                    Err(err) => errors.push(format!("space {}: {err}", space.id)),
                }
            }
        }

        let source_journals = self.source_root().join("journals");
        let total = pending_chats.len();
        let mut imported_chats = 0;
        let mut journals_copied = 0;
        if ledger_ok {
            for (index, chat) in pending_chats.iter().enumerate() {
                emit(ImportEvent::Chat {
                    index,
                    total,
                    chat_id: chat.id.clone(),
                    title: chat.title.clone(),
                });
                match self.import_chat(&source_store, chat, &source_journals, permit.as_ref()) {
                    Ok(journal) => {
                        imported_chats += 1;
                        if journal {
                            journals_copied += 1;
                        }
                    }
                    Err(err) => errors.push(format!("chat {}: {err}", chat.id)),
                }
            }
        }

        // Transcripts embed absolute paths under the local uploads root; jail
        // it read-only now (and on future boots, via the marker). A marker
        // that fails to persist means imported attachments stop resolving
        // after a restart — that is a real failure, not a footnote.
        if self.source_uploads().is_dir() {
            self.inner
                .uploads
                .add_read_only_root(&self.source_uploads());
        }
        // Persist imported registry rows before publishing a marker that will
        // re-arm their uploads root on a future boot. Partial attempts remain
        // retryable and never claim completion.
        let flush = match permit.as_ref() {
            Some(permit) => self.inner.workspace.flush_checked_admitted(permit),
            None => self.inner.workspace.flush_checked(),
        };
        if let Err(err) = flush {
            errors.push(format!("registry snapshot: {err}"));
        }
        if errors.is_empty()
            && let Err(err) = self.record_import(imported_chats, imported_spaces)
        {
            errors.push(format!("import marker: {err}"));
        }

        emit(ImportEvent::Summary {
            imported_chats,
            imported_spaces,
            skipped_chats,
            skipped_spaces,
            journals_copied,
            ledger_rows_merged,
            errors,
        });
        Ok(())
    }

    /// Copy one chat: doc snapshot (born-chat2 shape), journal files, row.
    /// Returns whether a journal file was copied.
    fn import_chat(
        &self,
        source_store: &DocsStore,
        chat: &zeron_proto::Chat,
        source_journals: &Path,
        permit: Option<&DeviceWritePermit>,
    ) -> Result<bool, EngineError> {
        // Doc bytes may be absent (a chat row created but never opened) — the
        // row alone is still worth carrying; a doc materializes on first open.
        if let Some(bytes) = source_store.load_snapshot(&chat.id)?
            && !self.inner.target_store.has_snapshot(&chat.id)?
        {
            self.inner.target_store.save_snapshot_with_cursor(
                &chat.id,
                &bytes,
                0,
                CHAT2_DOC_EPOCH,
            )?;
        }

        let mut copied = false;
        let (src_journal, src_resume) = journal_paths(source_journals, &chat.id);
        let (dst_journal, dst_resume) = journal_paths(&self.inner.target_journals, &chat.id);
        if src_journal.is_file() && !dst_journal.exists() {
            // `run` already owns its ticket. A second admission could fail if
            // a freeze enters Quiescing while this import is in flight.
            Self::copy_file_atomic(&src_journal, &dst_journal, &self.inner.target_journals)?;
            copied = true;
        }
        if src_resume.is_file() && !dst_resume.exists() {
            Self::copy_file_atomic(&src_resume, &dst_resume, &self.inner.target_journals)?;
        }
        for source in runtime_receipt_paths(source_journals, &chat.id)? {
            let destination = self.inner.target_journals.join(
                source
                    .file_name()
                    .ok_or_else(|| EngineError::Other("invalid runtime receipt name".into()))?,
            );
            if !destination.exists() {
                Self::copy_file_atomic(&source, &destination, &self.inner.target_journals)?;
            }
        }

        // Row last: once it appears in watchers the chat is clickable, and by
        // then its doc + journal are already in place. Chat2 lineage is
        // explicit so `DocHost::open` never routes the import down the legacy
        // s2 adopt branch.
        let mut row = chat.clone();
        row.room_gen = Some(CHAT2_DOC_EPOCH);
        match permit {
            Some(permit) => self.inner.workspace.import_chat_row_admitted(&row, permit)?,
            None => self.inner.workspace.import_chat_row(&row)?,
        }
        Ok(copied)
    }
}

#[cfg(test)]
mod tests {
    use super::{ImportEvent, LocalImporter};
    use crate::{
        InstanceLock,
        device_write::DeviceWriteOwner,
        uploads::Uploads,
        workspace_host::{WorkspaceHost, WorkspaceHostConfig},
    };
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;
    use zeron_sync::DocsStore;

    fn synthetic_target(
        root: &Path,
        owner: &DeviceWriteOwner,
    ) -> (Arc<DocsStore>, WorkspaceHost, LocalImporter) {
        let target_root = root.join("orgs/synthetic-org/synthetic-user");
        let store = Arc::new(DocsStore::open(&target_root).unwrap());
        let workspace = WorkspaceHost::open_with_registry_write_handle(
            store.clone(),
            WorkspaceHostConfig {
                device_id: "synthetic-device".into(),
                device_name: "Synthetic device".into(),
                platform: "test".into(),
                org_id: "synthetic-org".into(),
                user_id: "synthetic-user".into(),
                edge: None,
            },
            owner.handle(),
        )
        .unwrap();
        let importer = LocalImporter::new_guarded(
            root,
            "synthetic-device",
            "synthetic-org",
            "synthetic-user",
            store.clone(),
            target_root.join("journals"),
            workspace.clone(),
            Uploads::from_root(&target_root.join("uploads")),
            owner.handle(),
        );
        (store, workspace, importer)
    }

    fn seed_synthetic_source(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let source_root = root.join("profiles/local");
        let store = Arc::new(DocsStore::open(&source_root).unwrap());
        let workspace = WorkspaceHost::open(
            store.clone(),
            WorkspaceHostConfig {
                device_id: "synthetic-device".into(),
                device_name: "Synthetic device".into(),
                platform: "test".into(),
                org_id: "local".into(),
                user_id: "local".into(),
                edge: None,
            },
        )
        .unwrap();
        workspace
            .create_space("space-1", "synthetic-device", "/synthetic", None, false)
            .unwrap();
        workspace
            .create_chat("chat-1", Some("space-1"), None, None, None)
            .unwrap();
        workspace.flush_checked().unwrap();
        store
            .save_snapshot("chat-1", b"synthetic chat snapshot")
            .unwrap();
        store.mark_processed("command-1").unwrap();
        drop(workspace);
        drop(store);

        let journals = source_root.join("journals");
        std::fs::create_dir_all(&journals).unwrap();
        let (journal, resume) = crate::run_journal::journal_paths(&journals, "chat-1");
        let receipt = journals.join("chat-1.runtime.1.json");
        std::fs::write(&journal, b"{\"seq\":1,\"event\":{}}\n").unwrap();
        std::fs::write(&resume, b"synthetic resume").unwrap();
        std::fs::write(&receipt, b"{\"synthetic\":true}").unwrap();
        std::fs::create_dir_all(source_root.join("uploads")).unwrap();
        (journal, resume, receipt)
    }

    fn summary_errors(events: &[ImportEvent]) -> &[String] {
        match events.last().unwrap() {
            ImportEvent::Summary { errors, .. } => errors,
            other => panic!("expected summary, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admitted_import_finishes_rows_marker_and_summary_while_freeze_quiesces() {
        use zeron_sync::write_epoch::Phase;

        let root = tempfile::tempdir().unwrap();
        seed_synthetic_source(root.path());
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let (store, workspace, importer) = synthetic_target(root.path(), &owner);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let (summary_tx, summary_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let run = tokio::task::spawn_blocking(move || {
            let mut events = Vec::new();
            let mut started_tx = Some(started_tx);
            let mut resume_rx = Some(resume_rx);
            let mut summary_tx = Some(summary_tx);
            let mut finish_rx = Some(finish_rx);
            importer.run(|event| {
                match &event {
                    ImportEvent::Start { .. } => {
                        started_tx.take().unwrap().send(()).unwrap();
                        resume_rx
                            .take()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap();
                    }
                    ImportEvent::Summary { .. } => {
                        summary_tx.take().unwrap().send(()).unwrap();
                        finish_rx
                            .take()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap();
                    }
                    _ => {}
                }
                events.push(event);
            })?;
            Ok::<_, crate::EngineError>(events)
        });
        tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .unwrap()
            .unwrap();
        let epoch = owner.epoch_for_tests();
        let mut freeze = Box::pin(epoch.freeze(Duration::from_secs(5)));
        assert!(matches!(futures::poll!(freeze.as_mut()), std::task::Poll::Pending));
        assert_eq!(epoch.phase(), Phase::Quiescing);
        resume_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), summary_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(root.path().join("local-import.json").is_file());
        assert!(matches!(futures::poll!(freeze.as_mut()), std::task::Poll::Pending));
        finish_tx.send(()).unwrap();
        let events = run.await.unwrap().unwrap();
        assert!(summary_errors(&events).is_empty(), "{events:?}");
        assert!(matches!(
            events.last().unwrap(),
            ImportEvent::Summary {
                imported_chats: 1,
                imported_spaces: 1,
                ..
            }
        ));
        let snapshot = freeze.await.unwrap().into_snapshot().unwrap();
        assert!(workspace.chat("chat-1").unwrap().is_some());
        assert!(workspace.space("space-1").unwrap().is_some());
        assert!(store.load_snapshot(REGISTRY_DOC_ID).unwrap().is_some());
        drop(snapshot);
        workspace.drain_tasks_checked().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admitted_import_finishes_after_owner_retirement() {
        let root = tempfile::tempdir().unwrap();
        seed_synthetic_source(root.path());
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let (store, workspace, importer) = synthetic_target(root.path(), &owner);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let run = tokio::task::spawn_blocking(move || {
            let mut events = Vec::new();
            let mut started_tx = Some(started_tx);
            let mut resume_rx = Some(resume_rx);
            importer.run(|event| {
                if matches!(event, ImportEvent::Start { .. }) {
                    started_tx.take().unwrap().send(()).unwrap();
                    resume_rx
                        .take()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                }
                events.push(event);
            })?;
            Ok::<_, crate::EngineError>(events)
        });
        tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .unwrap()
            .unwrap();
        owner.retire();
        drop(owner);
        assert!(InstanceLock::acquire(root.path()).is_err());
        resume_tx.send(()).unwrap();
        let events = run.await.unwrap().unwrap();
        assert!(summary_errors(&events).is_empty(), "{events:?}");
        assert!(root.path().join("local-import.json").is_file());
        assert!(workspace.chat("chat-1").unwrap().is_some());
        assert!(workspace.space("space-1").unwrap().is_some());
        assert!(store.load_snapshot(REGISTRY_DOC_ID).unwrap().is_some());
        let _ = workspace.drain_tasks_checked().await;
        drop(workspace);
        drop(store);
        let replacement = DeviceWriteOwner::new(
            root.path(),
            InstanceLock::acquire(root.path()).unwrap(),
        )
        .unwrap();
        let (_, reopened, _) = synthetic_target(root.path(), &replacement);
        assert!(reopened.chat("chat-1").unwrap().is_some());
        reopened.drain_tasks_checked().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_busy_or_retired_does_not_migrate_source_or_quarantine_marker() {
        let root = tempfile::tempdir().unwrap();
        let source_root = root.path().join("profiles/local");
        std::fs::create_dir_all(&source_root).unwrap();
        let source_db = source_root.join("docs.sqlite3");
        {
            let connection = rusqlite::Connection::open(&source_db).unwrap();
            connection
                .execute_batch("CREATE TABLE seed (id INTEGER)")
                .unwrap();
        }
        let source_before = std::fs::read(&source_db).unwrap();
        let marker = root.path().join("local-import.json");
        let corrupt_marker = b"{corrupt synthetic marker";
        std::fs::write(&marker, corrupt_marker).unwrap();
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let (store, _, importer) = synthetic_target(root.path(), &owner);
        let target_before = store.load_snapshot(zeron_doc::REGISTRY_DOC_ID).unwrap();
        let target_journals = root
            .path()
            .join("orgs/synthetic-org/synthetic-user/journals");
        let assert_untouched = || {
            assert_eq!(std::fs::read(&source_db).unwrap(), source_before);
            assert_eq!(std::fs::read_dir(&source_root).unwrap().count(), 1);
            assert_eq!(std::fs::read(&marker).unwrap(), corrupt_marker);
            assert!(!marker.with_extension("json.corrupt").exists());
            assert_eq!(
                store.load_snapshot(zeron_doc::REGISTRY_DOC_ID).unwrap(),
                target_before
            );
            assert!(!target_journals.exists());
        };

        let snapshot = owner
            .epoch_for_tests()
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        let mut events = Vec::new();
        let busy = importer
            .run(|event| events.push(event))
            .unwrap_err()
            .to_string();
        assert!(busy.contains("Busy") && busy.contains("retry"), "{busy}");
        assert!(events.is_empty());
        assert_untouched();
        drop(snapshot);

        owner.retire();
        let retired = importer
            .run(|event| events.push(event))
            .unwrap_err()
            .to_string();
        assert!(
            retired.contains("Retired") && retired.contains("retry"),
            "{retired}"
        );
        assert!(events.is_empty());
        assert_untouched();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admitted_run_finishes_all_copies_and_flush_before_freeze_snapshot() {
        use zeron_sync::write_epoch::Phase;

        let root = tempfile::tempdir().unwrap();
        let (source_journal, source_resume, source_receipt) = seed_synthetic_source(root.path());
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let (store, workspace, importer) = synthetic_target(root.path(), &owner);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let running = importer.clone();
        let run_task = tokio::task::spawn_blocking(move || {
            let mut events = Vec::new();
            let mut started_tx = Some(started_tx);
            let mut release_rx = Some(release_rx);
            running.run(|event| {
                if matches!(event, ImportEvent::Start { .. }) {
                    started_tx.take().unwrap().send(()).unwrap();
                    release_rx
                        .take()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                }
                events.push(event);
            })?;
            Ok::<_, crate::EngineError>(events)
        });
        started_rx.await.unwrap();
        let epoch = owner.epoch_for_tests();
        let freezing = tokio::spawn(async move { epoch.freeze(Duration::from_secs(5)).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while owner.epoch_for_tests().phase() != Phase::Quiescing {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let target_journals = root
            .path()
            .join("orgs/synthetic-org/synthetic-user/journals");
        assert!(!target_journals.exists());
        release_tx.send(()).unwrap();
        let events = run_task.await.unwrap().unwrap();
        assert!(summary_errors(&events).is_empty(), "{events:?}");
        let snapshot = freezing.await.unwrap().unwrap().into_snapshot().unwrap();

        for source in [&source_journal, &source_resume, &source_receipt] {
            let target = target_journals.join(source.file_name().unwrap());
            assert_eq!(
                std::fs::read(target).unwrap(),
                std::fs::read(source).unwrap()
            );
        }
        assert_eq!(std::fs::read_dir(&target_journals).unwrap().count(), 3);
        assert!(store.is_processed("command-1").unwrap());
        assert!(workspace.chat("chat-1").unwrap().is_some());
        assert!(workspace.space("space-1").unwrap().is_some());
        let saved = store
            .load_snapshot(zeron_doc::REGISTRY_DOC_ID)
            .unwrap()
            .unwrap();
        let saved = zeron_doc::RegistryDoc::from_bytes(&saved, "synthetic-device").unwrap();
        assert!(saved.chat("chat-1").unwrap().is_some());
        assert_eq!(
            super::marker_grants_read_root(root.path(), "synthetic-org", "synthetic-user"),
            Some(root.path().join("profiles/local/uploads"))
        );
        drop(snapshot);

        let mut retry = Vec::new();
        importer.run(|event| retry.push(event)).unwrap();
        assert!(summary_errors(&retry).is_empty());
        assert!(matches!(
            retry.last().unwrap(),
            ImportEvent::Summary {
                imported_chats: 0,
                imported_spaces: 0,
                skipped_chats: 1,
                skipped_spaces: 1,
                ..
            }
        ));
        assert_eq!(std::fs::read_dir(&target_journals).unwrap().count(), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_ledger_and_flush_leave_no_marker_and_retry_after_owner_restart() {
        let root = tempfile::tempdir().unwrap();
        let (source_journal, source_resume, source_receipt) = seed_synthetic_source(root.path());
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let (store, workspace, importer) = synthetic_target(root.path(), &owner);
        let target_db = root
            .path()
            .join("orgs/synthetic-org/synthetic-user/docs.sqlite3");
        let connection = rusqlite::Connection::open(&target_db).unwrap();
        let marker = root.path().join("local-import.json");

        connection
            .execute_batch(
                "CREATE TRIGGER fail_import_ledger BEFORE INSERT ON processed_commands
                 BEGIN SELECT RAISE(ABORT, 'synthetic ledger failure'); END;",
            )
            .unwrap();
        let mut events = Vec::new();
        importer.run(|event| events.push(event)).unwrap();
        assert!(
            summary_errors(&events)
                .iter()
                .any(|error| error.contains("synthetic ledger failure")),
            "{events:?}"
        );
        assert!(!marker.exists());
        assert!(!store.is_processed("command-1").unwrap());
        assert!(workspace.chat("chat-1").unwrap().is_none());
        connection
            .execute_batch("DROP TRIGGER fail_import_ledger")
            .unwrap();

        connection
            .execute_batch(
                "CREATE TRIGGER fail_import_registry BEFORE UPDATE ON snapshots
                 WHEN NEW.doc_id = 'registry1'
                 BEGIN SELECT RAISE(ABORT, 'synthetic registry failure'); END;",
            )
            .unwrap();
        events.clear();
        importer.run(|event| events.push(event)).unwrap();
        assert!(
            summary_errors(&events)
                .iter()
                .any(|error| error.contains("synthetic registry failure")),
            "{events:?}"
        );
        assert!(!marker.exists());
        assert!(store.is_processed("command-1").unwrap());
        assert!(workspace.chat("chat-1").unwrap().is_some());
        let persisted = store
            .load_snapshot(zeron_doc::REGISTRY_DOC_ID)
            .unwrap()
            .unwrap();
        let persisted = zeron_doc::RegistryDoc::from_bytes(&persisted, "synthetic-device").unwrap();
        assert!(persisted.chat("chat-1").unwrap().is_none());

        owner.retire();
        drop(importer);
        drop(workspace);
        drop(store);
        connection
            .execute_batch("DROP TRIGGER fail_import_registry")
            .unwrap();
        drop(connection);
        drop(owner);

        let replacement =
            DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
                .unwrap();
        let (store, workspace, restarted) = synthetic_target(root.path(), &replacement);
        assert!(workspace.chat("chat-1").unwrap().is_none());
        let mut retry = Vec::new();
        restarted.run(|event| retry.push(event)).unwrap();
        assert!(summary_errors(&retry).is_empty(), "{retry:?}");
        assert!(marker.is_file());
        assert!(workspace.chat("chat-1").unwrap().is_some());
        assert!(store.is_processed("command-1").unwrap());
        let target_journals = root
            .path()
            .join("orgs/synthetic-org/synthetic-user/journals");
        for source in [&source_journal, &source_resume, &source_receipt] {
            let target = target_journals.join(source.file_name().unwrap());
            assert_eq!(
                std::fs::read(target).unwrap(),
                std::fs::read(source).unwrap()
            );
        }
        assert_eq!(std::fs::read_dir(&target_journals).unwrap().count(), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_importer_clones_serialize_dedup_and_file_publication() {
        let root = tempfile::tempdir().unwrap();
        seed_synthetic_source(root.path());
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let (_, _, importer) = synthetic_target(root.path(), &owner);
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first = importer.clone();
        let first_run = tokio::task::spawn_blocking(move || {
            let mut events = Vec::new();
            let mut started_tx = Some(first_started_tx);
            let mut release_rx = Some(release_rx);
            first.run(|event| {
                if matches!(event, ImportEvent::Start { .. }) {
                    started_tx.take().unwrap().send(()).unwrap();
                    release_rx
                        .take()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                }
                events.push(event);
            })?;
            Ok::<_, crate::EngineError>(events)
        });
        first_started_rx.await.unwrap();
        assert!(importer.inner.run_lock.try_lock().is_err());

        let (second_entered_tx, second_entered_rx) = tokio::sync::oneshot::channel();
        let entered = std::sync::Mutex::new(Some(second_entered_tx));
        *importer
            .inner
            .before_run_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(move || {
            if let Some(sender) = entered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                sender.send(()).unwrap();
            }
        }));
        let second = importer.clone();
        let second_run = tokio::task::spawn_blocking(move || {
            let mut events = Vec::new();
            second.run(|event| events.push(event))?;
            Ok::<_, crate::EngineError>(events)
        });
        second_entered_rx.await.unwrap();
        assert!(importer.inner.run_lock.try_lock().is_err());
        release_tx.send(()).unwrap();
        let first_events = first_run.await.unwrap().unwrap();
        let second_events = second_run.await.unwrap().unwrap();
        assert!(summary_errors(&first_events).is_empty(), "{first_events:?}");
        assert!(
            summary_errors(&second_events).is_empty(),
            "{second_events:?}"
        );
        assert!(matches!(
            first_events.last().unwrap(),
            ImportEvent::Summary {
                imported_chats: 1,
                imported_spaces: 1,
                ..
            }
        ));
        assert!(matches!(
            second_events.last().unwrap(),
            ImportEvent::Summary {
                imported_chats: 0,
                imported_spaces: 0,
                skipped_chats: 1,
                skipped_spaces: 1,
                ..
            }
        ));
        let target_journals = root
            .path()
            .join("orgs/synthetic-org/synthetic-user/journals");
        assert_eq!(std::fs::read_dir(&target_journals).unwrap().count(), 3);
        let marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("local-import.json")).unwrap())
                .unwrap();
        assert_eq!(marker["imports"].as_array().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_file_publication_keeps_marker_absent_and_retryable() {
        let root = tempfile::tempdir().unwrap();
        let (journal, resume, receipt) = seed_synthetic_source(root.path());
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let (store, workspace, importer) = synthetic_target(root.path(), &owner);
        let target_journals = root
            .path()
            .join("orgs/synthetic-org/synthetic-user/journals");
        std::fs::write(&target_journals, b"synthetic blocked directory").unwrap();

        let mut events = Vec::new();
        importer.run(|event| events.push(event)).unwrap();
        assert!(
            summary_errors(&events)
                .iter()
                .any(|error| error.starts_with("chat chat-1:")),
            "{events:?}"
        );
        assert!(!root.path().join("local-import.json").exists());
        assert_eq!(
            std::fs::read(&target_journals).unwrap(),
            b"synthetic blocked directory"
        );
        assert!(workspace.chat("chat-1").unwrap().is_none());
        assert!(store.is_processed("command-1").unwrap());

        std::fs::remove_file(&target_journals).unwrap();
        events.clear();
        importer.run(|event| events.push(event)).unwrap();
        assert!(summary_errors(&events).is_empty(), "{events:?}");
        assert!(root.path().join("local-import.json").is_file());
        assert!(workspace.chat("chat-1").unwrap().is_some());
        for source in [&journal, &resume, &receipt] {
            assert_eq!(
                std::fs::read(target_journals.join(source.file_name().unwrap())).unwrap(),
                std::fs::read(source).unwrap()
            );
        }
        assert_eq!(std::fs::read_dir(&target_journals).unwrap().count(), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn status_blocks_marker_quarantine_and_source_migration_until_restart() {
        let root = tempfile::tempdir().unwrap();
        let source_root = root.path().join("profiles/local");
        std::fs::create_dir_all(&source_root).unwrap();
        let source_db = source_root.join("docs.sqlite3");
        {
            let connection = rusqlite::Connection::open(&source_db).unwrap();
            connection
                .execute_batch("CREATE TABLE seed (id INTEGER PRIMARY KEY) STRICT")
                .unwrap();
        }
        let marker = root.path().join("local-import.json");
        let corrupt_marker = b"{invalid synthetic marker";
        std::fs::write(&marker, corrupt_marker).unwrap();
        let marker_aside = root.path().join("local-import.json.corrupt");
        let source_before = std::fs::read(&source_db).unwrap();
        let source_files_before: Vec<_> = std::fs::read_dir(&source_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();

        let target_root = root.path().join("orgs/synthetic-org/synthetic-user");
        let target_store = Arc::new(DocsStore::open(&target_root).unwrap());
        let workspace = WorkspaceHost::open(
            target_store.clone(),
            WorkspaceHostConfig {
                device_id: "synthetic-device".into(),
                device_name: "Synthetic device".into(),
                platform: "test".into(),
                org_id: "synthetic-org".into(),
                user_id: "synthetic-user".into(),
                edge: None,
            },
        )
        .unwrap();
        let uploads = Uploads::from_root(&target_root.join("uploads"));
        let target_journals = target_root.join("journals");
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let importer = LocalImporter::new_guarded(
            root.path(),
            "synthetic-device",
            "synthetic-org",
            "synthetic-user",
            target_store.clone(),
            target_journals.clone(),
            workspace.clone(),
            uploads.clone(),
            owner.handle(),
        );

        let assert_source_unchanged = || {
            assert_eq!(std::fs::read(&marker).unwrap(), corrupt_marker);
            assert!(!marker_aside.exists());
            assert_eq!(std::fs::read(&source_db).unwrap(), source_before);
            let source_files: Vec<_> = std::fs::read_dir(&source_root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(source_files, source_files_before);
        };

        let snapshot = owner
            .epoch_for_tests()
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        let busy = importer.status().unwrap_err().to_string();
        assert!(busy.contains("Busy") && busy.contains("retry"), "{busy}");
        assert_source_unchanged();
        drop(snapshot);

        owner.retire();
        let retired = importer.status().unwrap_err().to_string();
        assert!(
            retired.contains("Retired") && retired.contains("retry"),
            "{retired}"
        );
        assert_source_unchanged();
        drop(owner);

        let replacement =
            DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
                .unwrap();
        let restarted = LocalImporter::new_guarded(
            root.path(),
            "synthetic-device",
            "synthetic-org",
            "synthetic-user",
            target_store,
            target_journals,
            workspace,
            uploads,
            replacement.handle(),
        );
        let replacement_snapshot = replacement
            .epoch_for_tests()
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        let busy = restarted.status().unwrap_err().to_string();
        assert!(busy.contains("Busy") && busy.contains("retry"), "{busy}");
        assert_source_unchanged();
        drop(replacement_snapshot);

        let status = restarted.status().unwrap();
        assert_eq!(status.available_chats, 0);
        assert_eq!(status.available_spaces, 0);
        assert!(!status.imported_before);
        assert!(!marker.exists());
        assert_eq!(std::fs::read(&marker_aside).unwrap(), corrupt_marker);
        let connection = rusqlite::Connection::open(&source_db).unwrap();
        let migrations: i64 = connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(migrations > 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn journal_import_waits_for_same_freeze_and_retries_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("local/chat.jsonl");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        let bytes = b"{\"seq\":1,\"event\":{\"type\":\"textDelta\",\"text\":\"synthetic\"}}\n";
        std::fs::write(&source, bytes).unwrap();
        let target_dir = root.path().join("target/journals");
        let destination = target_dir.join("chat.jsonl");
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let handle = owner.handle();
        let snapshot = owner
            .epoch_for_tests()
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        let error = LocalImporter::copy_journal_with_handle(
            &source,
            &destination,
            &target_dir,
            Some(&handle),
        )
        .unwrap_err();
        assert!(error.to_string().contains("Busy"));
        assert!(!destination.exists());
        assert!(!target_dir.exists());
        drop(snapshot);

        LocalImporter::copy_journal_with_handle(&source, &destination, &target_dir, Some(&handle))
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&target_dir).unwrap().count(), 1);
        drop(owner);
        let replacement =
            DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
                .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
        let second = target_dir.join("second.jsonl");
        LocalImporter::copy_journal_with_handle(
            &source,
            &second,
            &target_dir,
            Some(&replacement.handle()),
        )
        .unwrap();
        assert_eq!(std::fs::read(&second).unwrap(), bytes);
    }

    #[test]
    fn import_events_serialize_camel_case() {
        let summary = serde_json::to_value(ImportEvent::Summary {
            imported_chats: 2,
            imported_spaces: 1,
            skipped_chats: 0,
            skipped_spaces: 0,
            journals_copied: 2,
            ledger_rows_merged: 3,
            errors: Vec::new(),
        })
        .expect("serialize");
        assert_eq!(summary["kind"], "summary");
        assert_eq!(
            summary["importedChats"], 2,
            "fields must be camelCase: {summary}"
        );
        let chat = serde_json::to_value(ImportEvent::Chat {
            index: 0,
            total: 2,
            chat_id: "c1".into(),
            title: None,
        })
        .expect("serialize");
        assert_eq!(chat["chatId"], "c1", "fields must be camelCase: {chat}");
    }
}

/// Whether a recorded import grants the synced profile `(org, user)` the local
/// profile's uploads root as a read-only jail root. `EngineCore::assemble`
/// calls this on every account-scoped boot.
pub fn marker_grants_read_root(data_dir: &Path, org_id: &str, user_id: &str) -> Option<PathBuf> {
    let marker: Marker = std::fs::read_to_string(data_dir.join(MARKER_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())?;
    let hit = marker
        .imports
        .iter()
        .any(|e| e.org_id == org_id && e.user_id == user_id);
    let uploads = data_dir.join("profiles").join("local").join("uploads");
    (hit && uploads.is_dir()).then_some(uploads)
}
