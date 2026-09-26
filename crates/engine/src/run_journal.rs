//! Per-session on-disk event journal (port of luna's `run-journal.ts`, JSONL-shaped).
//!
//! One append-only JSONL file per chat under `{data_dir}/journals/{chat_id}.jsonl`; each
//! line is `{"seq": n, "event": AgentEvent}` with a monotonically increasing `seq`. The
//! journal is the durable replay source for live streams (`Subscribe` = replay then tail
//! the broadcast hub) and the crash-recovery gauge: a journal whose LAST event is not
//! `Done` belongs to a run that died mid-stream — boot recovery stamps its doc entry
//! `aborted` and closes the journal with a synthetic `Done`.
//!
//! Bounded-window compaction is deferred (whole file kept for now, per M2 scope); a torn
//! trailing line from a crash mid-write is tolerated everywhere.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use luna_proto::AgentEvent;
use luna_proto::RuntimeContractDiagnostics;
use luna_sync::write_epoch::EpochError;
use sha2::{Digest, Sha256};

use crate::device_write::{DeviceWriteHandle, DeviceWritePermit};

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("device-root journal admission: {0:?}")]
    Admission(EpochError),
    #[error("journal append outcome uncertain: {0}")]
    Uncertain(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalLine {
    seq: u64,
    event: AgentEvent,
}

struct ChatJournal {
    file: File,
    next_seq: u64,
    /// True when the file ends without a newline (torn write) — the next append
    /// starts with one so the torn line stays isolated.
    needs_newline: bool,
}

/// Append-only JSONL journal store, one file per chat.
pub struct RunJournal {
    dir: PathBuf,
    open_files: Mutex<HashMap<String, ChatJournal>>,
    write_handle: Option<DeviceWriteHandle>,
    #[cfg(test)]
    fail_after_appends: Mutex<Option<usize>>,
}

impl RunJournal {
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, JournalError> {
        Self::open_with_handle(dir, None)
    }

    pub(crate) fn open_guarded(
        dir: impl AsRef<Path>,
        write_handle: DeviceWriteHandle,
    ) -> Result<Self, JournalError> {
        Self::open_with_handle(dir, Some(write_handle))
    }

    fn open_with_handle(
        dir: impl AsRef<Path>,
        write_handle: Option<DeviceWriteHandle>,
    ) -> Result<Self, JournalError> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            open_files: Mutex::new(HashMap::new()),
            write_handle,
            #[cfg(test)]
            fail_after_appends: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(crate) fn fail_next_append_for_tests(&self) {
        self.fail_after_appends_for_tests(0);
    }

    #[cfg(test)]
    pub(crate) fn fail_after_appends_for_tests(&self, successful_appends: usize) {
        *self
            .fail_after_appends
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(successful_appends);
    }

    pub(crate) fn try_admit(&self) -> Result<Option<DeviceWritePermit>, JournalError> {
        self.write_handle
            .as_ref()
            .map(DeviceWriteHandle::try_admit)
            .transpose()
            .map_err(JournalError::Admission)
    }

    pub(crate) async fn admit_async(
        &self,
        timeout: Duration,
    ) -> Result<Option<DeviceWritePermit>, JournalError> {
        match &self.write_handle {
            Some(handle) => handle
                .admit_async(timeout)
                .await
                .map(Some)
                .map_err(JournalError::Admission),
            None => Ok(None),
        }
    }

    fn check_permit(&self, permit: Option<&DeviceWritePermit>) -> Result<(), JournalError> {
        if let Some(handle) = &self.write_handle
            && !permit.is_some_and(|permit| handle.owns(permit))
        {
            return Err(JournalError::Admission(EpochError::Stale));
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, ChatJournal>> {
        self.open_files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn path_for(&self, chat_id: &str) -> PathBuf {
        self.dir.join(format!("{}.jsonl", sanitize_id(chat_id)))
    }

    fn attempts_path(&self, chat_id: &str) -> PathBuf {
        self.dir.join(format!("{}.resume", sanitize_id(chat_id)))
    }

    fn runtime_stem(&self, chat_id: &str, provider_session_id: &str) -> String {
        // The provider session id is never used as a path component. Different
        // provider sessions for one LUNA task retain independent contract proof.
        let digest = Sha256::digest(provider_session_id.as_bytes());
        format!("{}.runtime.{digest:x}", sanitize_id(chat_id))
    }

    fn runtime_path(&self, chat_id: &str, provider_session_id: &str) -> PathBuf {
        self.dir.join(format!(
            "{}.json",
            self.runtime_stem(chat_id, provider_session_id)
        ))
    }

    fn runtime_archive_path(&self, receipt: &RuntimeContractDiagnostics) -> PathBuf {
        let key = receipt.run_id.as_deref().unwrap_or(&receipt.contract_hash);
        self.dir.join(format!(
            "{}.{}.json",
            self.runtime_stem(&receipt.thread_id, &receipt.provider_session_id),
            sanitize_id(key)
        ))
    }

    /// Persist only the redacted, provider-acknowledged contract receipt.
    pub fn record_runtime_contract(
        &self,
        receipt: &RuntimeContractDiagnostics,
    ) -> Result<(), JournalError> {
        // One immutable receipt per accepted run remains available for
        // historical contract forensics; the latest pointer is for fast
        // current-session dispatch and redacted RPC readback.
        self.write_runtime_receipt(&self.runtime_archive_path(receipt), receipt)?;
        self.write_runtime_receipt(
            &self.runtime_path(&receipt.thread_id, &receipt.provider_session_id),
            receipt,
        )
    }

    fn write_runtime_receipt(
        &self,
        path: &Path,
        receipt: &RuntimeContractDiagnostics,
    ) -> Result<(), JournalError> {
        let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            serde_json::to_writer(&mut file, receipt)?;
            file.flush()?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)?;
            Ok::<(), JournalError>(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    pub fn runtime_contract(
        &self,
        chat_id: &str,
        provider_session_id: &str,
    ) -> Result<Option<RuntimeContractDiagnostics>, JournalError> {
        let path = self.runtime_path(chat_id, provider_session_id);
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let receipt: RuntimeContractDiagnostics = serde_json::from_reader(file)?;
        if receipt.thread_id != chat_id || receipt.provider_session_id != provider_session_id {
            return Err(JournalError::Io(std::io::Error::other(
                "runtime contract receipt identity mismatch",
            )));
        }
        Ok(Some(receipt))
    }

    /// Auto-resume revival budget (luna `resumeAttempt`/`MAX_AUTO_RESUME`):
    /// persisted beside the journal so a run that CRASHES THE ENGINE cannot
    /// revive itself in an infinite boot loop.
    pub fn resume_attempts(&self, chat_id: &str) -> u32 {
        std::fs::read_to_string(self.attempts_path(chat_id))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn note_resume_attempt(&self, chat_id: &str) -> u32 {
        let next = self.resume_attempts(chat_id) + 1;
        if let Err(err) = std::fs::write(self.attempts_path(chat_id), next.to_string()) {
            tracing::warn!(chat = %chat_id, error = %err, "resume-attempt ledger write failed");
        }
        next
    }

    /// A cleanly completed turn resets the budget — only consecutive
    /// crash-revive-crash cycles exhaust it.
    pub fn clear_resume_attempts(&self, chat_id: &str) {
        let _ = std::fs::remove_file(self.attempts_path(chat_id));
    }

    /// Append one event; returns its journal seq.
    pub fn append(&self, chat_id: &str, event: &AgentEvent) -> Result<u64, JournalError> {
        self.append_with_permit(chat_id, event, None)
    }

    pub(crate) fn append_with_permit(
        &self,
        chat_id: &str,
        event: &AgentEvent,
        permit: Option<&DeviceWritePermit>,
    ) -> Result<u64, JournalError> {
        #[cfg(test)]
        let should_fail = {
            let mut remaining = self
                .fail_after_appends
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match *remaining {
                Some(0) => {
                    *remaining = None;
                    true
                }
                Some(count) => {
                    *remaining = Some(count - 1);
                    false
                }
                None => false,
            }
        };
        #[cfg(test)]
        if should_fail {
            return Err(JournalError::Io(std::io::Error::other(
                "injected journal append failure",
            )));
        }
        self.append_with_writer(chat_id, event, permit, |file, bytes| {
            file.write_all(bytes)?;
            file.flush()
        })
    }

    fn append_with_writer(
        &self,
        chat_id: &str,
        event: &AgentEvent,
        permit: Option<&DeviceWritePermit>,
        write: impl FnOnce(&mut File, &[u8]) -> std::io::Result<()>,
    ) -> Result<u64, JournalError> {
        self.check_permit(permit)?;
        let mut files = self.lock();
        if !files.contains_key(chat_id) {
            // Bound the open-fd set: entries were never removed, so every chat
            // ever run held a descriptor for the process lifetime. Dropping is
            // safe — the next append reopens and rescans the tail. The cap
            // comfortably exceeds concurrent runs, so eviction stays rare.
            const OPEN_FILE_CAP: usize = 16;
            if files.len() >= OPEN_FILE_CAP {
                files.clear();
            }
            let path = self.path_for(chat_id);
            let (next_seq, needs_newline) = scan_tail(&path)?;
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            files.insert(
                chat_id.to_string(),
                ChatJournal {
                    file,
                    next_seq,
                    needs_newline,
                },
            );
        }
        // Entry guaranteed present; avoid unwrap in a library path regardless.
        let Some(journal) = files.get_mut(chat_id) else {
            return Err(JournalError::Io(std::io::Error::other(
                "journal entry vanished under lock",
            )));
        };
        let seq = journal.next_seq;
        let line = serde_json::to_string(&JournalLine {
            seq,
            event: event.clone(),
        })?;
        let mut buf = Vec::with_capacity(line.len() + 2);
        if journal.needs_newline {
            buf.push(b'\n');
        }
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        let start_len = journal.file.metadata()?.len();
        if let Err(error) = write(&mut journal.file, &buf) {
            // Never retry an uncertain append blindly. A full line that was
            // written before an error already has the confirmed seq;
            // otherwise leave the outcome explicit and reopen/rescan later.
            files.remove(chat_id);
            if appended_bytes_match(&self.path_for(chat_id), start_len, &buf) {
                return Ok(seq);
            }
            return Err(JournalError::Uncertain(error.to_string()));
        }
        journal.needs_newline = false;
        journal.next_seq = seq + 1;
        Ok(seq)
    }

    /// Events with `seq > after_seq`, in order. A cursor ahead of the last issued seq is
    /// from a previous era (file replaced) — falls back to a full replay, mirroring luna.
    pub fn replay(
        &self,
        chat_id: &str,
        after_seq: u64,
    ) -> Result<Vec<(u64, AgentEvent)>, JournalError> {
        let path = self.path_for(chat_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let all = read_lines(&path)?;
        let last_seq = all.last().map(|(seq, _)| *seq).unwrap_or(0);
        let from = if after_seq > last_seq { 0 } else { after_seq };
        Ok(all.into_iter().filter(|(seq, _)| *seq > from).collect())
    }

    /// Find a tool result on demand without materializing a whole long run.
    /// The latest matching event wins if a provider reused an item id.
    pub fn tool_output(
        &self,
        chat_id: &str,
        part_id: &str,
    ) -> Result<Option<String>, JournalError> {
        self.tool_blob(chat_id, part_id, false)
    }

    pub fn tool_diff(&self, chat_id: &str, part_id: &str) -> Result<Option<String>, JournalError> {
        self.tool_blob(chat_id, part_id, true)
    }

    fn tool_blob(
        &self,
        chat_id: &str,
        part_id: &str,
        diff: bool,
    ) -> Result<Option<String>, JournalError> {
        let file = match File::open(self.path_for(chat_id)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut found = None;
        for line in BufReader::new(file).lines() {
            let line = line?;
            if !line.contains(part_id) {
                continue;
            }
            if let Ok(parsed) = serde_json::from_str::<JournalLine>(&line) {
                if diff {
                    if let Some(value) = tool_result_diff(&parsed.event, part_id) {
                        found = serde_json::to_string(value).ok();
                    }
                } else if let Some(output) = tool_result_output(&parsed.event, part_id) {
                    found = Some(output.to_owned());
                }
            }
        }
        Ok(found)
    }

    /// The last event in a chat's journal, if any (ignores a torn tail line).
    pub fn last_event(&self, chat_id: &str) -> Result<Option<(u64, AgentEvent)>, JournalError> {
        let path = self.path_for(chat_id);
        if !path.exists() {
            return Ok(None);
        }
        Ok(read_lines(&path)?.into_iter().next_back())
    }

    /// Crash-recovery scan: chat ids whose journal's last event is NOT a `Done` — their
    /// runs died mid-stream and need recovery (stamp `aborted`, close the journal).
    pub fn stale_sessions(&self) -> Result<Vec<String>, JournalError> {
        self.stale_sessions_filtered(|_| true)
    }

    /// Apply metadata ownership before opening any journal payload. Sealed
    /// side-chat history is recovered lazily, never scanned at startup.
    pub(crate) fn stale_sessions_filtered(
        &self,
        include: impl Fn(&str) -> bool,
    ) -> Result<Vec<String>, JournalError> {
        let mut stale = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(chat_id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !include(chat_id) {
                continue;
            }
            let last = read_lines(&path)?.into_iter().next_back();
            match last {
                Some((_, AgentEvent::Done { .. })) | None => {}
                Some(_) => stale.push(chat_id.to_string()),
            }
        }
        stale.sort();
        Ok(stale)
    }

    /// Remove a chat's journal file entirely (tests / future compaction).
    pub fn discard(&self, chat_id: &str) -> Result<(), JournalError> {
        let _permit = self.try_admit()?;
        self.lock().remove(chat_id);
        let path = self.path_for(chat_id);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

fn tool_result_output<'a>(event: &'a AgentEvent, part_id: &str) -> Option<&'a str> {
    match event {
        AgentEvent::ToolResult {
            id,
            output: Some(output),
            ..
        } if id == part_id => Some(output),
        AgentEvent::Subagent { event, .. } => tool_result_output(event, part_id),
        _ => None,
    }
}

fn tool_result_diff<'a>(event: &'a AgentEvent, part_id: &str) -> Option<&'a luna_proto::ToolDiff> {
    match event {
        AgentEvent::ToolResult {
            id,
            diff: Some(diff),
            ..
        } if id == part_id => Some(diff),
        AgentEvent::Subagent { event, .. } => tool_result_diff(event, part_id),
        _ => None,
    }
}

fn appended_bytes_match(path: &Path, offset: u64, expected: &[u8]) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    if file.metadata().ok().map(|meta| meta.len()) != offset.checked_add(expected.len() as u64) {
        return false;
    }
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return false;
    }
    let mut actual = vec![0; expected.len()];
    file.read_exact(&mut actual).is_ok() && actual == expected
}

/// Parse every valid line; malformed lines (torn tail writes) are skipped.
fn read_lines(path: &Path) -> Result<Vec<(u64, AgentEvent)>, JournalError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalLine>(&line) {
            Ok(parsed) => out.push((parsed.seq, parsed.event)),
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "journal: skipping malformed line");
            }
        }
    }
    Ok(out)
}

/// Next seq (last valid seq + 1, starting at 1) and whether the file ends mid-line.
fn scan_tail(path: &Path) -> Result<(u64, bool), JournalError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((1, false)),
        Err(e) => return Err(e.into()),
    };
    let needs_newline = bytes.last().is_some_and(|b| *b != b'\n');
    let next_seq = read_lines(path)?
        .last()
        .map(|(seq, _)| seq + 1)
        .unwrap_or(1);
    Ok((next_seq, needs_newline))
}

/// Journal (`.jsonl`) and resume-budget (`.resume`) paths for `chat_id` under an
/// arbitrary journals directory — profile import copies these files between
/// profiles without opening a `RunJournal`.
pub fn journal_paths(dir: &Path, chat_id: &str) -> (PathBuf, PathBuf) {
    let stem = sanitize_id(chat_id);
    (
        dir.join(format!("{stem}.jsonl")),
        dir.join(format!("{stem}.resume")),
    )
}

/// Redacted runtime receipts belonging to a task, including historical run
/// archives. Profile import copies these alongside the task's event journal.
pub fn runtime_receipt_paths(dir: &Path, chat_id: &str) -> Result<Vec<PathBuf>, JournalError> {
    let prefix = format!("{}.runtime.", sanitize_id(chat_id));
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with(&prefix) && name.ends_with(".json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Chat ids become file names; anything outside a conservative set is replaced so a
/// hostile id cannot traverse paths. (Ids are uuids in practice.)
fn sanitize_id(chat_id: &str) -> String {
    chat_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstanceLock, device_write::DeviceWriteOwner};
    use luna_proto::DoneStatus;
    use luna_sync::write_epoch::Phase;

    fn text(s: &str) -> AgentEvent {
        AgentEvent::TextDelta { text: s.into() }
    }

    fn done() -> AgentEvent {
        AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guarded_run_ticket_keeps_events_ordered_across_freeze() {
        let root = tempfile::tempdir().unwrap();
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let epoch = owner.epoch_for_tests();
        let journal =
            RunJournal::open_guarded(root.path().join("journals"), owner.handle()).unwrap();
        let ticket = journal.try_admit().unwrap().unwrap();
        assert_eq!(
            journal
                .append_with_permit("chat", &text("one"), Some(&ticket))
                .unwrap(),
            1
        );
        let mut freezing = Box::pin(epoch.freeze(Duration::from_secs(2)));
        assert!(matches!(
            futures::poll!(freezing.as_mut()),
            std::task::Poll::Pending
        ));
        assert_eq!(epoch.phase(), Phase::Quiescing);
        assert!(matches!(
            journal.try_admit(),
            Err(JournalError::Admission(EpochError::Busy))
        ));
        assert_eq!(
            journal
                .append_with_permit("chat", &text("two"), Some(&ticket))
                .unwrap(),
            2
        );
        assert!(matches!(
            journal.append("chat", &text("unadmitted")),
            Err(JournalError::Admission(EpochError::Stale))
        ));
        drop(ticket);
        let snapshot = freezing.await.unwrap().into_snapshot().unwrap();
        assert_eq!(
            journal
                .replay("chat", 0)
                .unwrap()
                .into_iter()
                .map(|(seq, _)| seq)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        drop(snapshot);
        owner.retire();
        assert!(matches!(
            journal.try_admit(),
            Err(JournalError::Admission(EpochError::Retired))
        ));
    }

    #[test]
    fn wrong_coordinator_ticket_cannot_append() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let other_owner =
            DeviceWriteOwner::new(other.path(), InstanceLock::acquire(other.path()).unwrap())
                .unwrap();
        let ticket = other_owner.handle().try_admit().unwrap();
        let journal =
            RunJournal::open_guarded(root.path().join("journals"), owner.handle()).unwrap();
        assert!(matches!(
            journal.append_with_permit("chat", &text("wrong"), Some(&ticket)),
            Err(JournalError::Admission(EpochError::Stale))
        ));
        assert!(journal.replay("chat", 0).unwrap().is_empty());
    }

    #[test]
    fn full_line_after_write_error_is_confirmed_once() {
        let root = tempfile::tempdir().unwrap();
        let journal = RunJournal::open(root.path()).unwrap();
        let seq = journal
            .append_with_writer("chat", &text("one"), None, |file, bytes| {
                file.write_all(bytes)?;
                Err(std::io::Error::other("injected postwrite failure"))
            })
            .unwrap();
        assert_eq!(seq, 1);
        assert_eq!(journal.replay("chat", 0).unwrap().len(), 1);
        assert_eq!(journal.append("chat", &text("two")).unwrap(), 2);
        assert_eq!(journal.replay("chat", 0).unwrap().len(), 2);
    }

    #[test]
    fn partial_line_after_write_error_is_uncertain_and_next_seq_is_recovered() {
        let root = tempfile::tempdir().unwrap();
        let journal = RunJournal::open(root.path()).unwrap();
        assert!(matches!(
            journal.append_with_writer("chat", &text("partial"), None, |file, bytes| {
                file.write_all(&bytes[..bytes.len() / 2])?;
                Err(std::io::Error::other("injected partial write"))
            }),
            Err(JournalError::Uncertain(_))
        ));
        assert!(journal.replay("chat", 0).unwrap().is_empty());
        assert_eq!(journal.append("chat", &text("later")).unwrap(), 1);
        let replay = journal.replay("chat", 0).unwrap();
        assert_eq!(replay, vec![(1, text("later"))]);
    }

    #[test]
    fn redacted_runtime_receipts_are_session_scoped_and_durable() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = RuntimeContractDiagnostics {
            thread_id: "task-a".into(),
            provider_session_id: "provider-session-a".into(),
            run_id: Some("run-a".into()),
            accepted_at_ms: Some(1),
            contract_version: 1,
            contract_hash: "hash-a".into(),
            delivery: luna_proto::RuntimeDelivery::CodexDynamicTools,
            instructions: vec![luna_proto::RuntimeInstructionSummary {
                name: "LUNA core".into(),
                source: luna_proto::RuntimeInstructionSource::LunaCore,
                provenance: "LUNA runtime v1".into(),
                character_count: 100,
            }],
            attached_tools: vec![luna_proto::RuntimeToolSummary {
                name: "luna_status".into(),
                version: 1,
                required_scopes: Vec::new(),
            }],
            omitted_tools: Vec::new(),
            restart_required: false,
        };
        let journal = RunJournal::open(dir.path()).unwrap();
        journal.record_runtime_contract(&receipt).unwrap();
        drop(journal);
        let journal = RunJournal::open(dir.path()).unwrap();
        assert_eq!(
            journal
                .runtime_contract("task-a", "provider-session-a")
                .unwrap(),
            Some(receipt.clone())
        );
        assert!(
            journal
                .runtime_contract("task-a", "provider-session-b")
                .unwrap()
                .is_none()
        );
        let mut next = receipt.clone();
        next.run_id = Some("run-b".into());
        next.accepted_at_ms = Some(2);
        next.contract_hash = "hash-b".into();
        journal.record_runtime_contract(&next).unwrap();
        assert!(journal.runtime_archive_path(&receipt).is_file());
        assert!(journal.runtime_archive_path(&next).is_file());
        assert_eq!(
            journal
                .runtime_contract("task-a", "provider-session-a")
                .unwrap(),
            Some(next)
        );
        assert_eq!(
            runtime_receipt_paths(dir.path(), "task-a").unwrap().len(),
            3
        );
    }

    #[test]
    fn appends_are_monotonic_and_replayable() {
        let dir = tempfile::tempdir().unwrap();
        let journal = RunJournal::open(dir.path()).unwrap();
        assert_eq!(journal.append("chat-1", &text("a")).unwrap(), 1);
        assert_eq!(journal.append("chat-1", &text("b")).unwrap(), 2);
        assert_eq!(journal.append("chat-1", &done()).unwrap(), 3);

        let all = journal.replay("chat-1", 0).unwrap();
        assert_eq!(all.len(), 3);
        let after = journal.replay("chat-1", 2).unwrap();
        assert_eq!(after.len(), 1);
        assert!(matches!(after[0].1, AgentEvent::Done { .. }));
        // Era fallback: cursor ahead of last seq replays everything.
        assert_eq!(journal.replay("chat-1", 99).unwrap().len(), 3);
    }

    #[test]
    fn seq_continues_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let journal = RunJournal::open(dir.path()).unwrap();
            journal.append("chat-1", &text("a")).unwrap();
        }
        let journal = RunJournal::open(dir.path()).unwrap();
        assert_eq!(journal.append("chat-1", &text("b")).unwrap(), 2);
    }

    #[test]
    fn stale_scan_flags_journals_without_terminal_done() {
        let dir = tempfile::tempdir().unwrap();
        let journal = RunJournal::open(dir.path()).unwrap();
        journal.append("dead", &text("partial")).unwrap();
        journal.append("clean", &text("full")).unwrap();
        journal.append("clean", &done()).unwrap();
        assert_eq!(journal.stale_sessions().unwrap(), vec!["dead".to_string()]);
        // Closing the stale journal with a Done clears the flag.
        journal.append("dead", &done()).unwrap();
        assert!(journal.stale_sessions().unwrap().is_empty());
    }

    #[test]
    fn torn_tail_line_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        {
            let journal = RunJournal::open(dir.path()).unwrap();
            journal.append("chat-1", &text("a")).unwrap();
        }
        // Simulate a crash mid-write: garbage with no trailing newline.
        let path = dir.path().join("chat-1.jsonl");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"seq\":2,\"event\":{\"type\":\"textD")
            .unwrap();
        drop(f);

        let journal = RunJournal::open(dir.path()).unwrap();
        assert_eq!(journal.replay("chat-1", 0).unwrap().len(), 1);
        assert_eq!(journal.append("chat-1", &text("b")).unwrap(), 2);
        let all = journal.replay("chat-1", 0).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].0, 2);
    }
}
