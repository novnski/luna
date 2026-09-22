//! `DocsStore` — local SQLite persistence for doc snapshots and the
//! processed-command ledger (ARCHITECTURE §2 command plane: entries are marked
//! processed BEFORE execution so a crash can never double-execute a command).

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use zeron_proto::{
    ActivityEnvelope, CommandReasonCode, CommandReceipt, CommandReceiptReason,
    CommandReceiptStatus, ComposerDraft, ComposerDraftRead, ComposerDraftSaveOutcome,
    DraftSubmission, DraftSubmissionState, HistoryCursor, HistoryProjectPolicy, HistorySearchPage,
    HistorySearchRequest, HistorySearchScope, TaskCommandEvent, TaskCommandRead,
    TaskCommandSnapshot,
};

/// Errors surfaced by [`DocsStore`].
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("immutable draft submission {0} already has different content")]
    DraftSubmissionConflict(String),
    #[error("composer draft {0} reused a revision with different content")]
    DraftRevisionConflict(String),
    #[error("task recovery state changed before the requested transition")]
    RecoveryConflict,
}

/// Ordered, append-only migrations. Each entry runs once inside a transaction;
/// `schema_migrations` records what has been applied.
const MIGRATIONS: &[&str] = &[
    // v1 — snapshots + processed-command ledger
    "CREATE TABLE snapshots (
        doc_id   TEXT PRIMARY KEY,
        bytes    BLOB NOT NULL,
        saved_at INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE processed_commands (
        command_id   TEXT PRIMARY KEY,
        processed_at INTEGER NOT NULL
     ) STRICT;",
    // v2 — chat2 room cursor + doc epoch (docs/chat2-sync.md C2). The cursor
    // is persisted in the SAME transaction as the snapshot bytes, so content
    // and cursor cannot diverge (restored backups / copied devices simply
    // redownload from their honest cursor). `epoch` marks rebuild lineage
    // (M1/M3): 2 = thin chat2 rebuild; NULL/0 = pre-migration s2 doc.
    "ALTER TABLE snapshots ADD COLUMN cursor INTEGER;
     ALTER TABLE snapshots ADD COLUMN epoch INTEGER;",
    // v3 — publication survives actor eviction and process restart.
    "CREATE TABLE chat_outbox (
        ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
        doc_id TEXT NOT NULL,
        batch_id TEXT NOT NULL UNIQUE,
        bytes BLOB NOT NULL,
        needs_checkpoint INTEGER NOT NULL DEFAULT 0
     ) STRICT;
     CREATE INDEX chat_outbox_doc ON chat_outbox(doc_id,ordinal);
     CREATE TABLE chat_outbox_initialized (doc_id TEXT PRIMARY KEY) STRICT;",
    // v4 — L07 durable task command journal. The receipt is duplicated into
    // the append-only event so a replay never depends on mutable receipt state.
    "CREATE TABLE task_command_state (
        task_id TEXT PRIMARY KEY,
        revision INTEGER NOT NULL DEFAULT 0,
        task_sequence INTEGER NOT NULL DEFAULT 0,
        binding_id TEXT,
        binding_generation INTEGER,
        active_turn_id TEXT
     ) STRICT;
     CREATE TABLE task_command_receipts (
        command_id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL,
        payload_hash TEXT NOT NULL,
        receipt_json BLOB NOT NULL,
        updated_at INTEGER NOT NULL
     ) STRICT;
     CREATE INDEX task_command_receipts_task ON task_command_receipts(task_id, updated_at);
     CREATE TABLE task_command_events (
        sequence INTEGER PRIMARY KEY AUTOINCREMENT,
        task_id TEXT NOT NULL,
        task_sequence INTEGER NOT NULL,
        command_id TEXT NOT NULL,
        receipt_json BLOB NOT NULL,
        UNIQUE(task_id, task_sequence)
     ) STRICT;
     CREATE INDEX task_command_events_task ON task_command_events(task_id, task_sequence);
     CREATE TABLE task_command_conflicts (
        command_id TEXT NOT NULL,
        task_id TEXT NOT NULL,
        payload_hash TEXT NOT NULL,
        receipt_json BLOB NOT NULL,
        PRIMARY KEY(command_id,task_id,payload_hash)
     ) STRICT;",
    // v5 — L08 device-local composer drafts and explicit staged-blob owners.
    // Blob bytes live in the existing profile uploads directory; these rows
    // are the crash-consistent projection that says which durable files are
    // still owned by a draft, unresolved submission, or queued entry.
    "CREATE TABLE composer_drafts (
        device_id TEXT NOT NULL,
        draft_id TEXT NOT NULL,
        task_id TEXT,
        revision INTEGER NOT NULL,
        record_json BLOB NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY(device_id, draft_id)
     ) STRICT;
     CREATE UNIQUE INDEX composer_drafts_task
       ON composer_drafts(device_id, task_id) WHERE task_id IS NOT NULL;
     CREATE INDEX composer_drafts_canvas
       ON composer_drafts(device_id, updated_at DESC) WHERE task_id IS NULL;
     CREATE TABLE composer_draft_submissions (
        device_id TEXT NOT NULL,
        command_id TEXT NOT NULL,
        draft_id TEXT NOT NULL,
        task_id TEXT,
        draft_revision INTEGER NOT NULL,
        state TEXT NOT NULL,
        record_json BLOB NOT NULL,
        queue_id TEXT,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY(device_id, command_id)
     ) STRICT;
     CREATE INDEX composer_draft_submissions_scope
       ON composer_draft_submissions(device_id, task_id, draft_id, updated_at);
     CREATE TABLE attachment_owners (
        device_id TEXT NOT NULL,
        blob_path TEXT NOT NULL,
        owner_kind TEXT NOT NULL,
        owner_id TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY(device_id, blob_path, owner_kind, owner_id)
     ) STRICT;
     CREATE INDEX attachment_owners_owner
       ON attachment_owners(device_id, owner_kind, owner_id);",
    // v6 — L10 device-local recovery authority. `record_json` may contain the
    // frozen prompt, attachment paths, and provider session identity, so this
    // table is intentionally local SQLite state and is never projected into a
    // synced document. The scalar columns are the compare-and-set fence.
    "CREATE TABLE task_recovery_state (
        task_id TEXT PRIMARY KEY,
        chain_id TEXT NOT NULL,
        attempt_count INTEGER NOT NULL,
        status TEXT NOT NULL,
        record_json BLOB NOT NULL,
        updated_at INTEGER NOT NULL
     ) STRICT;",
    // v7 — L14 local derived session-history index. The canonical payload
    // remains in the transcript/receipt stores; rows here contain only the
    // minimized redacted envelope and can be replaced transactionally.
    "CREATE TABLE activity_events (
        event_id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL,
        project_id TEXT,
        device_id TEXT NOT NULL,
        harness TEXT,
        model TEXT,
        outcome TEXT NOT NULL,
        tool TEXT,
        app TEXT,
        timestamp_ms INTEGER NOT NULL,
        sequence INTEGER NOT NULL,
        search_text TEXT NOT NULL,
        envelope_json BLOB NOT NULL
     ) STRICT;
     CREATE INDEX activity_events_order
       ON activity_events(timestamp_ms DESC, sequence DESC, event_id DESC);
     CREATE INDEX activity_events_project_order
       ON activity_events(project_id, timestamp_ms DESC, sequence DESC, event_id DESC);
     CREATE INDEX activity_events_task ON activity_events(task_id);
     CREATE TABLE activity_project_policies (
        project_id TEXT PRIMARY KEY,
        policy_json BLOB NOT NULL,
        updated_at INTEGER NOT NULL
     ) STRICT;",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCommandBeginKind {
    Accepted,
    Duplicate,
    Conflict,
}

#[derive(Debug, Clone)]
pub struct TaskCommandBegin {
    pub kind: TaskCommandBeginKind,
    pub receipt: CommandReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredTaskRecovery {
    pub task_id: String,
    pub chain_id: String,
    pub attempt_count: u8,
    pub status: String,
    pub record_json: Vec<u8>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecoveryExpectation {
    pub chain_id: String,
    pub attempt_count: u8,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecoveryTransition {
    pub expected: Option<TaskRecoveryExpectation>,
    pub next: StoredTaskRecovery,
}

/// SQLite-backed store under a data directory (`{data_dir}/docs.sqlite3`).
///
/// Holds warm-open doc snapshots (the DO room is authoritative; these make
/// cold starts instant and offline restarts possible) and the command ledger
/// that gives command execution mark-BEFORE-execute idempotence.
pub struct DocsStore {
    conn: Mutex<Connection>,
    activity_gate: Mutex<()>,
    failed_publications: Mutex<HashSet<String>>,
}

impl DocsStore {
    /// Open (creating directory, database, and schema as needed).
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref();
        std::fs::create_dir_all(data_dir)?;
        let mut conn = Connection::open(data_dir.join("docs.sqlite3"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            activity_gate: Mutex::new(()),
            failed_publications: Mutex::new(HashSet::new()),
        })
    }

    /// Serialize policy reads with derived-row replacement. Callers rebuilding
    /// envelopes hold this across policy read + source normalization + commit,
    /// preventing an older policy snapshot from racing a stricter policy save.
    pub fn activity_index_guard(&self) -> MutexGuard<'_, ()> {
        self.activity_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Atomically replace one task's derived rows. Duplicate delivery is
    /// harmless because stable event ids are primary keys and the transaction
    /// never exposes a half-rebuilt task.
    pub fn replace_activity_events(
        &self,
        task_id: &str,
        events: &[ActivityEnvelope],
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM activity_events WHERE task_id=?1",
            params![task_id],
        )?;
        for event in events {
            insert_activity_event(&tx, event)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn activity_task_ids(&self) -> Result<Vec<String>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT DISTINCT task_id FROM activity_events")?;
        Ok(stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Replace the complete derived index in one durable transaction. Raw
    /// transcripts, receipts, task state, drafts, and files are not touched.
    pub fn rebuild_activity_events(
        &self,
        tasks: &[(String, Vec<ActivityEnvelope>)],
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM activity_events", [])?;
        for (_task_id, events) in tasks {
            for event in events {
                insert_activity_event(&tx, event)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn activity_project_policy(
        &self,
        project_id: &str,
    ) -> Result<Option<HistoryProjectPolicy>, StoreError> {
        let bytes: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT policy_json FROM activity_project_policies WHERE project_id=?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        bytes
            .map(|bytes| serde_json::from_slice(&bytes))
            .transpose()
            .map_err(StoreError::from)
    }

    pub fn save_activity_project_policy(
        &self,
        policy: &HistoryProjectPolicy,
    ) -> Result<(), StoreError> {
        let _activity_guard = self.activity_index_guard();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO activity_project_policies(project_id,policy_json,updated_at)
             VALUES (?1,?2,?3)
             ON CONFLICT(project_id) DO UPDATE SET
               policy_json=excluded.policy_json,updated_at=excluded.updated_at",
            params![policy.project_id, serde_json::to_vec(policy)?, now_ms()],
        )?;
        // Fail closed: a stricter policy must never leave earlier derived rows
        // searchable while source reconciliation is pending or fails.
        tx.execute(
            "DELETE FROM activity_events WHERE project_id=?1",
            params![policy.project_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Bounded scan with deterministic keyset paging. Scalar filters are
    /// applied to decoded minimized envelopes; raw payloads are never fetched.
    pub fn search_activity_events(
        &self,
        request: &HistorySearchRequest,
    ) -> Result<HistorySearchPage, StoreError> {
        let limit = request.limit.clamp(1, 100) as usize;
        let scan_limit = (limit.saturating_mul(40)).clamp(100, 2_000) as i64;
        let needle = request.query.trim().to_lowercase();
        let conn = self.conn();
        let mut sql = String::from("SELECT envelope_json FROM activity_events WHERE 1=1");
        let mut values = Vec::<rusqlite::types::Value>::new();
        if request.scope == HistorySearchScope::SelectedProject {
            if let Some(project_id) = request.project_id.as_ref() {
                sql.push_str(" AND project_id=?");
                values.push(project_id.clone().into());
            } else {
                sql.push_str(" AND project_id IS NULL");
            }
        }
        if !needle.is_empty() {
            sql.push_str(" AND instr(search_text,?)>0");
            values.push(needle.clone().into());
        }
        if let Some(cursor) = request.cursor.as_ref() {
            sql.push_str(
                " AND (timestamp_ms<? OR (timestamp_ms=? AND sequence<?)
                  OR (timestamp_ms=? AND sequence=? AND event_id<?))",
            );
            values.push(cursor.timestamp_ms.into());
            values.push(cursor.timestamp_ms.into());
            values.push((cursor.sequence as i64).into());
            values.push(cursor.timestamp_ms.into());
            values.push((cursor.sequence as i64).into());
            values.push(cursor.event_id.clone().into());
        }
        let filters = &request.filters;
        if let Some(after) = filters.after_ms {
            sql.push_str(" AND timestamp_ms>=?");
            values.push(after.into());
        }
        if let Some(before) = filters.before_ms {
            sql.push_str(" AND timestamp_ms<=?");
            values.push(before.into());
        }
        append_text_in(
            &mut sql,
            &mut values,
            "device_id",
            filters.device_ids.iter().cloned(),
        );
        append_text_in(
            &mut sql,
            &mut values,
            "harness",
            filters
                .harnesses
                .iter()
                .filter_map(|value| serde_json::to_string(value).ok()),
        );
        append_lower_text_in(
            &mut sql,
            &mut values,
            "model",
            filters.models.iter().map(|value| value.to_lowercase()),
        );
        append_text_in(
            &mut sql,
            &mut values,
            "outcome",
            filters
                .outcomes
                .iter()
                .filter_map(|value| serde_json::to_string(value).ok()),
        );
        append_lower_text_in(
            &mut sql,
            &mut values,
            "tool",
            filters.tools.iter().map(|value| value.to_lowercase()),
        );
        append_lower_text_in(
            &mut sql,
            &mut values,
            "app",
            filters.apps.iter().map(|value| value.to_lowercase()),
        );
        sql.push_str(" ORDER BY timestamp_ms DESC,sequence DESC,event_id DESC LIMIT ?");
        values.push(scan_limit.into());
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                row.get::<_, Vec<u8>>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut events = Vec::with_capacity(limit.saturating_add(1));
        for bytes in rows {
            let event: ActivityEnvelope = serde_json::from_slice(&bytes)?;
            if !activity_matches(request, &needle, &event) {
                continue;
            }
            events.push(event);
            if events.len() > limit {
                break;
            }
        }
        let has_more = events.len() > limit;
        events.truncate(limit);
        let next_cursor = has_more.then(|| {
            let last = events.last().expect("a paged result has a last row");
            HistoryCursor {
                timestamp_ms: last.timestamp_ms,
                sequence: last.sequence,
                event_id: last.event_id.clone(),
            }
        });
        let indexed_through_ms = conn.query_row(
            "SELECT COALESCE(MAX(timestamp_ms),0) FROM activity_events",
            [],
            |row| row.get(0),
        )?;
        Ok(HistorySearchPage {
            events,
            next_cursor,
            indexed_through_ms,
            heuristic_redaction: true,
        })
    }

    /// Insert before sending. Stable IDs make crash-after-ACK replay idempotent.
    pub fn enqueue_chat_update(
        &self,
        doc_id: &str,
        batch_id: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let result = self.conn().execute(
            "INSERT OR IGNORE INTO chat_outbox(doc_id,batch_id,bytes) VALUES (?1,?2,?3)",
            params![doc_id, batch_id, bytes],
        );
        if result.is_err() {
            self.failed_publications
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(doc_id.to_string());
        }
        result?;
        Ok(())
    }

    pub fn pending_chat_updates(&self, doc_id: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT batch_id,bytes FROM chat_outbox WHERE doc_id=?1 ORDER BY ordinal")?;
        Ok(stmt
            .query_map(params![doc_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?)
    }

    pub fn reject_chat_update(&self, doc_id: &str, batch_id: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE chat_outbox SET needs_checkpoint=1 WHERE doc_id=?1 AND batch_id=?2",
            params![doc_id, batch_id],
        )?;
        Ok(())
    }

    pub fn rejected_chat_updates(
        &self,
        doc_id: &str,
    ) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT batch_id,bytes FROM chat_outbox WHERE doc_id=?1 AND needs_checkpoint=1 ORDER BY ordinal")?;
        Ok(stmt
            .query_map(params![doc_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?)
    }

    pub fn acknowledge_chat_update(&self, doc_id: &str, batch_id: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "DELETE FROM chat_outbox WHERE doc_id=?1 AND batch_id=?2",
            params![doc_id, batch_id],
        )?;
        Ok(())
    }

    pub fn chat_outbox_initialized(&self, doc_id: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM chat_outbox_initialized WHERE doc_id=?1",
                params![doc_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Upgrade legacy snapshots (including orphaned local history) once. The
    /// marker and complete replay obligation commit together; never reset cursors.
    pub fn initialize_chat_outbox(
        &self,
        doc_id: &str,
        updates: &[Vec<u8>],
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if tx.execute(
            "INSERT OR IGNORE INTO chat_outbox_initialized(doc_id) VALUES (?1)",
            params![doc_id],
        )? != 0
        {
            for bytes in updates {
                tx.execute(
                    "INSERT INTO chat_outbox(doc_id,batch_id,bytes) VALUES (?1,?2,?3)",
                    params![doc_id, uuid::Uuid::new_v4().to_string(), bytes],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Latest saved snapshot for `doc_id`, if any.
    pub fn load_snapshot(&self, doc_id: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let bytes = self
            .conn()
            .query_row(
                "SELECT bytes FROM snapshots WHERE doc_id = ?1",
                params![doc_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(bytes)
    }

    /// Save (upsert) the snapshot for `doc_id`.
    pub fn save_snapshot(&self, doc_id: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        self.persist_snapshot_changes_tx(&tx, &[(doc_id, bytes)], &[], &[])?;
        tx.commit()?;
        Ok(())
    }

    /// Persist a doc projection and its already-derived local CRDT batches in
    /// one transaction. This is the non-receipt half of the queue durability
    /// boundary: a caller may publish/apply the candidate only after success.
    pub fn save_snapshot_with_updates(
        &self,
        doc_id: &str,
        bytes: &[u8],
        updates: &[(&str, &[u8])],
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let updates = updates
            .iter()
            .map(|(batch_id, bytes)| (doc_id, *batch_id, *bytes))
            .collect::<Vec<_>>();
        self.persist_snapshot_changes_tx(&tx, &[(doc_id, bytes)], &[], &updates)?;
        tx.commit()?;
        Ok(())
    }

    /// Save the snapshot together with its chat2 room cursor and doc epoch —
    /// ONE transaction, so bytes and cursor can never disagree (the C2 rule;
    /// a divergent pair is exactly the restored-backup redownload bug).
    pub fn save_snapshot_with_cursor(
        &self,
        doc_id: &str,
        bytes: &[u8],
        cursor: u64,
        epoch: u32,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO snapshots (doc_id, bytes, saved_at, cursor, epoch) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(doc_id) DO UPDATE SET bytes = excluded.bytes, saved_at = excluded.saved_at,
                 cursor = excluded.cursor, epoch = excluded.epoch",
            params![doc_id, bytes, now_ms(), cursor as i64, epoch as i64],
        )?;
        self.invalidate_failed_publication(&tx, doc_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Snapshot + chat2 cursor + epoch. Pre-migration rows (or rows written
    /// by [`Self::save_snapshot`]) read back as `(bytes, 0, 0)`.
    pub fn load_snapshot_with_cursor(
        &self,
        doc_id: &str,
    ) -> Result<Option<(Vec<u8>, u64, u32)>, StoreError> {
        let row = self
            .conn()
            .query_row(
                "SELECT bytes, cursor, epoch FROM snapshots WHERE doc_id = ?1",
                params![doc_id],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                        row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u32,
                    ))
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Persist one complete versioned composer draft and its blob ownership in
    /// one SQLite transaction. An older async save is ignored rather than
    /// restoring stale content after a task/profile switch.
    pub fn save_composer_draft(
        &self,
        device_id: &str,
        draft: &ComposerDraft,
    ) -> Result<ComposerDraftSaveOutcome, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let bytes = serde_json::to_vec(draft)?;
        let current = tx
            .query_row(
                "SELECT revision,record_json FROM composer_drafts
                 WHERE device_id=?1 AND draft_id=?2",
                params![device_id, draft.draft_id],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        if let Some((revision, current_bytes)) = current {
            if revision > draft.revision {
                return Ok(ComposerDraftSaveOutcome::Stale {
                    current_revision: revision,
                });
            }
            if revision == draft.revision {
                let mut current: ComposerDraft = serde_json::from_slice(&current_bytes)?;
                current.updated_at_ms = draft.updated_at_ms;
                if current == *draft {
                    return Ok(ComposerDraftSaveOutcome::Unchanged);
                }
                return Err(StoreError::DraftRevisionConflict(draft.draft_id.clone()));
            }
        }
        if let Some(task_id) = draft.task_id.as_deref() {
            tx.execute(
                "DELETE FROM composer_drafts
                 WHERE device_id=?1 AND task_id=?2 AND draft_id<>?3",
                params![device_id, task_id, draft.draft_id],
            )?;
        }
        tx.execute(
            "INSERT INTO composer_drafts
               (device_id,draft_id,task_id,revision,record_json,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(device_id,draft_id) DO UPDATE SET
               task_id=excluded.task_id, revision=excluded.revision,
               record_json=excluded.record_json, updated_at=excluded.updated_at",
            params![
                device_id,
                draft.draft_id,
                draft.task_id,
                draft.revision as i64,
                bytes,
                draft.updated_at_ms
            ],
        )?;
        replace_attachment_owners_tx(
            &tx,
            device_id,
            "draft",
            &draft.draft_id,
            draft.saved_blob_paths(),
        )?;
        tx.commit()?;
        Ok(ComposerDraftSaveOutcome::Saved)
    }

    /// Load only the selected draft plus unresolved submissions in the same
    /// scope. This never scans task journals or unrelated drafts.
    pub fn read_composer_draft(
        &self,
        device_id: &str,
        task_id: Option<&str>,
        draft_id: Option<&str>,
    ) -> Result<ComposerDraftRead, StoreError> {
        let conn = self.conn();
        let draft_bytes: Option<Vec<u8>> = match (task_id, draft_id) {
            (Some(task_id), _) => conn
                .query_row(
                    "SELECT record_json FROM composer_drafts
                     WHERE device_id=?1 AND task_id=?2 LIMIT 1",
                    params![device_id, task_id],
                    |row| row.get(0),
                )
                .optional()?,
            (None, Some(draft_id)) => conn
                .query_row(
                    "SELECT record_json FROM composer_drafts
                     WHERE device_id=?1 AND draft_id=?2 AND task_id IS NULL",
                    params![device_id, draft_id],
                    |row| row.get(0),
                )
                .optional()?,
            (None, None) => conn
                .query_row(
                    "SELECT record_json FROM composer_drafts
                     WHERE device_id=?1 AND task_id IS NULL
                     ORDER BY updated_at DESC LIMIT 1",
                    params![device_id],
                    |row| row.get(0),
                )
                .optional()?,
        };
        let draft = draft_bytes
            .map(|bytes| serde_json::from_slice(&bytes))
            .transpose()?;
        let effective_draft_id = draft
            .as_ref()
            .map(|draft: &ComposerDraft| draft.draft_id.as_str())
            .or(draft_id)
            .unwrap_or_default();
        let mut stmt = conn.prepare(
            "SELECT command_id,state,record_json,task_id,queue_id,updated_at
             FROM composer_draft_submissions
             WHERE device_id=?1 AND
               ((?2 IS NOT NULL AND task_id=?2) OR
                (?2 IS NULL AND task_id IS NULL AND draft_id=?3))
             ORDER BY updated_at, command_id",
        )?;
        let submissions = stmt
            .query_map(params![device_id, task_id, effective_draft_id], |row| {
                let state: String = row.get(1)?;
                let state = match state.as_str() {
                    "pending" => DraftSubmissionState::Pending,
                    _ => DraftSubmissionState::Recoverable,
                };
                let bytes: Vec<u8> = row.get(2)?;
                Ok((
                    row.get::<_, String>(0)?,
                    state,
                    bytes,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(
                |(command_id, state, bytes, task_id, queue_id, updated_at_ms)| {
                    Ok(DraftSubmission {
                        command_id,
                        state,
                        snapshot: serde_json::from_slice(&bytes)?,
                        task_id,
                        queue_id,
                        updated_at_ms,
                    })
                },
            )
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(ComposerDraftRead { draft, submissions })
    }

    /// Freeze the exact revision before dispatch. Reusing a command id with a
    /// different snapshot is rejected, matching L07's immutable envelope.
    pub fn begin_draft_submission(
        &self,
        device_id: &str,
        command_id: &str,
        task_id: Option<&str>,
        snapshot: &ComposerDraft,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let bytes = serde_json::to_vec(snapshot)?;
        let existing: Option<Vec<u8>> = tx
            .query_row(
                "SELECT record_json FROM composer_draft_submissions
                 WHERE device_id=?1 AND command_id=?2",
                params![device_id, command_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing == bytes {
                return Ok(());
            }
            return Err(StoreError::DraftSubmissionConflict(command_id.into()));
        }
        tx.execute(
            "INSERT INTO composer_draft_submissions
               (device_id,command_id,draft_id,task_id,draft_revision,state,
                record_json,updated_at)
             VALUES (?1,?2,?3,?4,?5,'pending',?6,?7)",
            params![
                device_id,
                command_id,
                snapshot.draft_id,
                task_id,
                snapshot.revision as i64,
                bytes,
                now_ms()
            ],
        )?;
        replace_attachment_owners_tx(
            &tx,
            device_id,
            "submission",
            command_id,
            snapshot.saved_blob_paths(),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn mark_draft_submission_recoverable(
        &self,
        device_id: &str,
        command_id: &str,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE composer_draft_submissions
             SET state='recoverable',updated_at=?3
             WHERE device_id=?1 AND command_id=?2",
            params![device_id, command_id, now_ms()],
        )?;
        Ok(())
    }

    /// Accept one submission and clear only the exact draft revision it froze.
    /// A queue id transfers blob ownership in the same transaction.
    pub fn accept_draft_submission(
        &self,
        device_id: &str,
        command_id: &str,
        queue_id: Option<&str>,
    ) -> Result<bool, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let row: Option<(String, u64, Vec<u8>)> = tx
            .query_row(
                "SELECT draft_id,draft_revision,record_json
                 FROM composer_draft_submissions
                 WHERE device_id=?1 AND command_id=?2",
                params![device_id, command_id],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64, row.get(2)?)),
            )
            .optional()?;
        let Some((draft_id, submitted_revision, bytes)) = row else {
            return Ok(false);
        };
        let snapshot: ComposerDraft = serde_json::from_slice(&bytes)?;
        if let Some(queue_id) = queue_id {
            replace_attachment_owners_tx(
                &tx,
                device_id,
                "queue",
                queue_id,
                snapshot.saved_blob_paths(),
            )?;
        }
        tx.execute(
            "DELETE FROM attachment_owners
             WHERE device_id=?1 AND owner_kind='submission' AND owner_id=?2",
            params![device_id, command_id],
        )?;
        tx.execute(
            "DELETE FROM composer_draft_submissions
             WHERE device_id=?1 AND command_id=?2",
            params![device_id, command_id],
        )?;
        let current: Option<u64> = tx
            .query_row(
                "SELECT revision FROM composer_drafts
                 WHERE device_id=?1 AND draft_id=?2",
                params![device_id, draft_id],
                |row| Ok(row.get::<_, i64>(0)? as u64),
            )
            .optional()?;
        let cleared = current == Some(submitted_revision);
        if cleared {
            tx.execute(
                "DELETE FROM attachment_owners
                 WHERE device_id=?1 AND owner_kind='draft' AND owner_id=?2",
                params![device_id, draft_id],
            )?;
            tx.execute(
                "DELETE FROM composer_drafts WHERE device_id=?1 AND draft_id=?2",
                params![device_id, draft_id],
            )?;
        }
        tx.commit()?;
        Ok(cleared)
    }

    /// Record owners for a currently observed queue. Stale owner retirement is
    /// deliberately deferred to L18; L08 prefers bounded leakage to deleting
    /// bytes that another unopened queue may still own.
    pub fn replace_queue_attachment_owners(
        &self,
        device_id: &str,
        queue: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for (queue_id, paths) in queue {
            for path in paths {
                tx.execute(
                    "INSERT OR IGNORE INTO attachment_owners
                       (device_id,blob_path,owner_kind,owner_id,created_at)
                     VALUES (?1,?2,'queue',?3,?4)",
                    params![device_id, path, queue_id, now_ms()],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    #[cfg(test)]
    fn attachment_owner_count(&self, device_id: &str, path: &str) -> usize {
        self.conn()
            .query_row(
                "SELECT COUNT(*) FROM attachment_owners WHERE device_id=?1 AND blob_path=?2",
                params![device_id, path],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or_default() as usize
    }

    /// Delete the snapshot row for `doc_id` (destructive schema breaks: the
    /// legacy `workspace` row is dropped on open). Missing rows are a no-op.
    pub fn delete_snapshot(&self, doc_id: &str) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM snapshots WHERE doc_id = ?1", params![doc_id])?;
        tx.execute("DELETE FROM chat_outbox WHERE doc_id = ?1", params![doc_id])?;
        tx.execute(
            "DELETE FROM chat_outbox_initialized WHERE doc_id = ?1",
            params![doc_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Whether a snapshot row exists for `doc_id` — presence only, no blob read.
    pub fn has_snapshot(&self, doc_id: &str) -> Result<bool, StoreError> {
        let hit = self
            .conn()
            .query_row(
                "SELECT 1 FROM snapshots WHERE doc_id = ?1",
                params![doc_id],
                |_| Ok(()),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    /// The full command ledger — profile-import reads the source's claims so
    /// imported pending commands can never re-execute under the new profile.
    pub fn processed_commands(&self) -> Result<Vec<(String, i64)>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT command_id, processed_at FROM processed_commands")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Merge foreign ledger claims (profile import). Existing claims win;
    /// returns how many rows were newly inserted.
    pub fn import_processed_commands(&self, rows: &[(String, i64)]) -> Result<usize, StoreError> {
        let mut inserted = 0;
        let conn = self.conn();
        for (command_id, processed_at) in rows {
            inserted += conn.execute(
                "INSERT OR IGNORE INTO processed_commands (command_id, processed_at) VALUES (?1, ?2)",
                params![command_id, processed_at],
            )?;
        }
        Ok(inserted)
    }

    /// Whether `command_id` has already been claimed for execution.
    pub fn is_processed(&self, command_id: &str) -> Result<bool, StoreError> {
        let hit = self
            .conn()
            .query_row(
                "SELECT 1 FROM processed_commands WHERE command_id = ?1",
                params![command_id],
                |_| Ok(()),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    /// Claim `command_id` for execution — call BEFORE executing (ledger rule:
    /// a crash mid-execution must never re-run the command). Returns `true`
    /// if this call claimed it, `false` if it was already processed.
    pub fn mark_processed(&self, command_id: &str) -> Result<bool, StoreError> {
        let changed = self.conn().execute(
            "INSERT OR IGNORE INTO processed_commands (command_id, processed_at) VALUES (?1, ?2)",
            params![command_id, now_ms()],
        )?;
        Ok(changed > 0)
    }

    /// Durably accept one task command or return the prior receipt. Validation
    /// and the initial pending receipt share a transaction, so two clients
    /// racing the same id can never both become executors.
    pub fn begin_task_command(
        &self,
        mut receipt: CommandReceipt,
        expected_revision: Option<u64>,
        expected_binding_generation: Option<u32>,
        expected_turn_id: Option<&str>,
    ) -> Result<TaskCommandBegin, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let existing = tx
            .query_row(
                "SELECT receipt_json FROM task_command_receipts WHERE command_id=?1",
                params![receipt.command_id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        if let Some(bytes) = existing {
            let prior: CommandReceipt = serde_json::from_slice(&bytes)?;
            if prior.task_id == receipt.task_id && prior.payload_hash == receipt.payload_hash {
                tx.commit()?;
                return Ok(TaskCommandBegin {
                    kind: TaskCommandBeginKind::Duplicate,
                    receipt: prior,
                });
            }
            if let Some(bytes) = tx
                .query_row(
                    "SELECT receipt_json FROM task_command_conflicts
                     WHERE command_id=?1 AND task_id=?2 AND payload_hash=?3",
                    params![receipt.command_id, receipt.task_id, receipt.payload_hash],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()?
            {
                tx.commit()?;
                return Ok(TaskCommandBegin {
                    kind: TaskCommandBeginKind::Conflict,
                    receipt: serde_json::from_slice(&bytes)?,
                });
            }
            tx.execute(
                "INSERT OR IGNORE INTO task_command_state(task_id) VALUES (?1)",
                params![receipt.task_id],
            )?;
            let candidate_snapshot = load_task_command_snapshot(&tx, &receipt.task_id)?;
            receipt.snapshot.task_revision = candidate_snapshot.task_revision;
            receipt.snapshot.task_sequence = candidate_snapshot.task_sequence;
            receipt.status = CommandReceiptStatus::Conflict;
            receipt.reason = Some(CommandReceiptReason {
                code: CommandReasonCode::CommandIdConflict,
                message: format!(
                    "command id {} already belongs to task {} with payload {}",
                    receipt.command_id, prior.task_id, prior.payload_hash
                ),
            });
            receipt.recovery_action =
                Some("Mint a new command id for the different target or payload.".into());
            receipt.committed_at_ms = Some(now_ms());
            receipt.observed_at_ms = now_ms();
            receipt = commit_task_event_only_tx(&tx, receipt)?;
            tx.execute(
                "INSERT INTO task_command_conflicts(command_id,task_id,payload_hash,receipt_json)
                 VALUES (?1,?2,?3,?4)",
                params![
                    receipt.command_id,
                    receipt.task_id,
                    receipt.payload_hash,
                    serde_json::to_vec(&receipt)?
                ],
            )?;
            tx.commit()?;
            return Ok(TaskCommandBegin {
                kind: TaskCommandBeginKind::Conflict,
                receipt,
            });
        }

        tx.execute(
            "INSERT OR IGNORE INTO task_command_state(task_id) VALUES (?1)",
            params![receipt.task_id],
        )?;
        let snapshot = load_task_command_snapshot(&tx, &receipt.task_id)?;
        receipt.snapshot.task_revision = snapshot.task_revision;
        receipt.snapshot.task_sequence = snapshot.task_sequence;

        let conflict = if expected_revision
            .is_some_and(|expected| expected != snapshot.task_revision)
        {
            Some((
                CommandReasonCode::StaleTaskRevision,
                format!(
                    "expected task revision {}, current revision is {}",
                    expected_revision.unwrap_or_default(),
                    snapshot.task_revision
                ),
                "Reload the bounded task snapshot, review the changed state, and retry with a new command id.",
            ))
        } else if expected_binding_generation
            .is_some_and(|expected| Some(expected) != receipt.snapshot.binding_generation)
        {
            Some((
                CommandReasonCode::StaleBindingGeneration,
                format!(
                    "expected binding generation {:?}, current generation is {:?}",
                    expected_binding_generation, receipt.snapshot.binding_generation
                ),
                "Reload task identity before issuing another provider mutation.",
            ))
        } else if expected_turn_id
            .is_some_and(|expected| receipt.snapshot.active_turn_id.as_deref() != Some(expected))
        {
            Some((
                CommandReasonCode::StaleTurn,
                format!(
                    "expected turn {:?}, current turn is {:?}",
                    expected_turn_id, receipt.snapshot.active_turn_id
                ),
                "Reload active task state; do not apply this control to a replacement turn.",
            ))
        } else {
            None
        };

        if let Some((code, message, recovery)) = conflict {
            receipt.status = CommandReceiptStatus::Conflict;
            receipt.reason = Some(CommandReceiptReason { code, message });
            receipt.recovery_action = Some(recovery.into());
            let receipt = commit_task_command_tx(&tx, receipt, false)?;
            tx.commit()?;
            return Ok(TaskCommandBegin {
                kind: TaskCommandBeginKind::Conflict,
                receipt,
            });
        }

        let bytes = serde_json::to_vec(&receipt)?;
        tx.execute(
            "INSERT INTO task_command_receipts(command_id,task_id,payload_hash,receipt_json,updated_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                receipt.command_id,
                receipt.task_id,
                receipt.payload_hash,
                bytes,
                now_ms()
            ],
        )?;
        tx.commit()?;
        Ok(TaskCommandBegin {
            kind: TaskCommandBeginKind::Accepted,
            receipt,
        })
    }

    /// Atomically append the terminal event, advance the task projection, and
    /// replace the pending receipt. `applied` is the only outcome that advances
    /// the optimistic task revision; every terminal outcome advances sequence.
    #[allow(clippy::too_many_arguments)] // mirrors the atomic receipt row fields
    pub fn complete_task_command(
        &self,
        command_id: &str,
        status: CommandReceiptStatus,
        applied: bool,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        active_turn_id: Option<&str>,
        reason: Option<CommandReceiptReason>,
        recovery_action: Option<&str>,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.complete_task_command_inner(
            command_id,
            status,
            applied,
            binding_id,
            binding_generation,
            active_turn_id,
            reason,
            recovery_action,
            None,
            None,
        )
    }

    /// L10 command boundary: terminal receipt and exact recovery-state CAS
    /// become durable together. A stale timer/client cannot receive Applied
    /// while mutating a replacement chain.
    #[allow(clippy::too_many_arguments)]
    pub fn complete_task_command_with_recovery(
        &self,
        command_id: &str,
        status: CommandReceiptStatus,
        applied: bool,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        active_turn_id: Option<&str>,
        reason: Option<CommandReceiptReason>,
        recovery_action: Option<&str>,
        recovery: &TaskRecoveryTransition,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.complete_task_command_inner(
            command_id,
            status,
            applied,
            binding_id,
            binding_generation,
            active_turn_id,
            reason,
            recovery_action,
            None,
            Some(recovery),
        )
    }

    /// L07 local projection boundary: the real registry snapshot and the
    /// terminal command event/projection/receipt commit in the same SQLite
    /// transaction. Callers publish the in-memory projection only after this
    /// returns successfully.
    #[allow(clippy::too_many_arguments)]
    pub fn complete_task_command_with_snapshot_changes(
        &self,
        command_id: &str,
        status: CommandReceiptStatus,
        applied: bool,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        active_turn_id: Option<&str>,
        reason: Option<CommandReceiptReason>,
        recovery_action: Option<&str>,
        snapshots: &[(&str, &[u8])],
        deleted_snapshots: &[&str],
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.complete_task_command_with_snapshot_changes_and_updates(
            command_id,
            status,
            applied,
            binding_id,
            binding_generation,
            active_turn_id,
            reason,
            recovery_action,
            snapshots,
            deleted_snapshots,
            &[],
        )
    }

    /// L09 queue projection boundary. The terminal receipt, durable session
    /// snapshot, and any chat2 publication batches become visible together.
    #[allow(clippy::too_many_arguments)]
    pub fn complete_task_command_with_snapshot_changes_and_updates(
        &self,
        command_id: &str,
        status: CommandReceiptStatus,
        applied: bool,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        active_turn_id: Option<&str>,
        reason: Option<CommandReceiptReason>,
        recovery_action: Option<&str>,
        snapshots: &[(&str, &[u8])],
        deleted_snapshots: &[&str],
        updates: &[(&str, &str, &[u8])],
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.complete_task_command_inner(
            command_id,
            status,
            applied,
            binding_id,
            binding_generation,
            active_turn_id,
            reason,
            recovery_action,
            Some((snapshots, deleted_snapshots, updates)),
            None,
        )
    }

    /// Registry/session projection, terminal receipt, and L10 recovery fence
    /// in one transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn complete_task_command_with_snapshot_changes_and_recovery(
        &self,
        command_id: &str,
        status: CommandReceiptStatus,
        applied: bool,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        active_turn_id: Option<&str>,
        reason: Option<CommandReceiptReason>,
        recovery_action: Option<&str>,
        snapshots: &[(&str, &[u8])],
        deleted_snapshots: &[&str],
        recovery: &TaskRecoveryTransition,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.complete_task_command_inner(
            command_id,
            status,
            applied,
            binding_id,
            binding_generation,
            active_turn_id,
            reason,
            recovery_action,
            Some((snapshots, deleted_snapshots, &[])),
            Some(recovery),
        )
    }

    /// L11 context boundary: registry, task identity, chat snapshot/outbox,
    /// receipt, and an optional recovery fence commit together.
    #[allow(clippy::too_many_arguments)]
    pub fn complete_task_command_with_snapshot_changes_updates_and_recovery(
        &self,
        command_id: &str,
        status: CommandReceiptStatus,
        applied: bool,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        active_turn_id: Option<&str>,
        reason: Option<CommandReceiptReason>,
        recovery_action: Option<&str>,
        snapshots: &[(&str, &[u8])],
        deleted_snapshots: &[&str],
        updates: &[(&str, &str, &[u8])],
        recovery: Option<&TaskRecoveryTransition>,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.complete_task_command_inner(
            command_id,
            status,
            applied,
            binding_id,
            binding_generation,
            active_turn_id,
            reason,
            recovery_action,
            Some((snapshots, deleted_snapshots, updates)),
            recovery,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::type_complexity)]
    fn complete_task_command_inner(
        &self,
        command_id: &str,
        status: CommandReceiptStatus,
        applied: bool,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        active_turn_id: Option<&str>,
        reason: Option<CommandReceiptReason>,
        recovery_action: Option<&str>,
        snapshot_changes: Option<(&[(&str, &[u8])], &[&str], &[(&str, &str, &[u8])])>,
        recovery: Option<&TaskRecoveryTransition>,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let Some(bytes) = tx
            .query_row(
                "SELECT receipt_json FROM task_command_receipts WHERE command_id=?1",
                params![command_id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
        else {
            tx.commit()?;
            return Ok(None);
        };
        let mut receipt: CommandReceipt = serde_json::from_slice(&bytes)?;
        if receipt.status.is_terminal() {
            tx.commit()?;
            return Ok(Some(receipt));
        }
        receipt.status = status;
        receipt.snapshot.binding_id = binding_id.map(str::to_owned);
        receipt.snapshot.binding_generation = binding_generation;
        receipt.snapshot.active_turn_id = active_turn_id.map(str::to_owned);
        receipt.reason = reason;
        receipt.recovery_action = recovery_action.map(str::to_owned);
        receipt.committed_at_ms = Some(now_ms());
        receipt.observed_at_ms = now_ms();
        let receipt = commit_task_command_tx(&tx, receipt, applied)?;
        if let Some((snapshots, deleted_snapshots, updates)) = snapshot_changes {
            self.persist_snapshot_changes_tx(&tx, snapshots, deleted_snapshots, updates)?;
        }
        if let Some(recovery) = recovery {
            apply_task_recovery_transition_tx(&tx, recovery)?;
        }
        tx.commit()?;
        Ok(Some(receipt))
    }

    fn persist_snapshot_changes_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        snapshots: &[(&str, &[u8])],
        deleted_snapshots: &[&str],
        updates: &[(&str, &str, &[u8])],
    ) -> Result<(), StoreError> {
        for (doc_id, bytes) in snapshots {
            tx.execute(
                "INSERT INTO snapshots (doc_id, bytes, saved_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(doc_id) DO UPDATE SET bytes=excluded.bytes,saved_at=excluded.saved_at",
                params![doc_id, bytes, now_ms()],
            )?;
            self.invalidate_failed_publication(tx, doc_id)?;
        }
        for (doc_id, batch_id, bytes) in updates {
            tx.execute(
                "INSERT OR IGNORE INTO chat_outbox(doc_id,batch_id,bytes) VALUES (?1,?2,?3)",
                params![doc_id, batch_id, bytes],
            )?;
        }
        for doc_id in deleted_snapshots {
            tx.execute("DELETE FROM snapshots WHERE doc_id=?1", params![doc_id])?;
            tx.execute("DELETE FROM chat_outbox WHERE doc_id=?1", params![doc_id])?;
            tx.execute(
                "DELETE FROM chat_outbox_initialized WHERE doc_id=?1",
                params![doc_id],
            )?;
        }
        Ok(())
    }

    /// Preserve an ambiguous provider outcome as pending. A retry with the same
    /// id reads this receipt and never invokes the provider again.
    pub fn mark_task_command_unknown(
        &self,
        command_id: &str,
        message: &str,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.mark_task_command_unknown_with_snapshot_changes(command_id, message, &[], &[])
    }

    pub fn mark_task_command_unknown_with_snapshot_changes(
        &self,
        command_id: &str,
        message: &str,
        snapshots: &[(&str, &[u8])],
        updates: &[(&str, &str, &[u8])],
    ) -> Result<Option<CommandReceipt>, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let Some(bytes) = tx
            .query_row(
                "SELECT receipt_json FROM task_command_receipts WHERE command_id=?1",
                params![command_id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
        else {
            tx.commit()?;
            return Ok(None);
        };
        let mut receipt: CommandReceipt = serde_json::from_slice(&bytes)?;
        if !receipt.status.is_terminal() {
            receipt.reason = Some(CommandReceiptReason {
                code: CommandReasonCode::ProviderAcceptanceUnknown,
                message: message.into(),
            });
            receipt.recovery_action = Some(
                "Reconcile provider and task state before deciding whether a new command is safe."
                    .into(),
            );
            receipt.committed_at_ms = Some(now_ms());
            receipt.observed_at_ms = now_ms();
            tx.execute(
                "UPDATE task_command_receipts SET receipt_json=?2,updated_at=?3 WHERE command_id=?1",
                params![command_id, serde_json::to_vec(&receipt)?, now_ms()],
            )?;
        }
        self.persist_snapshot_changes_tx(&tx, snapshots, &[], updates)?;
        tx.commit()?;
        Ok(Some(receipt))
    }

    pub fn set_task_command_correlation(
        &self,
        command_id: &str,
        correlation_id: &str,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.set_task_command_correlation_inner(command_id, correlation_id, &[], &[], None)
    }

    pub fn set_task_command_correlation_with_snapshot_changes(
        &self,
        command_id: &str,
        correlation_id: &str,
        snapshots: &[(&str, &[u8])],
        updates: &[(&str, &str, &[u8])],
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.set_task_command_correlation_inner(
            command_id,
            correlation_id,
            snapshots,
            updates,
            None,
        )
    }

    pub fn set_task_command_correlation_with_recovery(
        &self,
        command_id: &str,
        correlation_id: &str,
        recovery: &TaskRecoveryTransition,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.set_task_command_correlation_inner(
            command_id,
            correlation_id,
            &[],
            &[],
            Some(recovery),
        )
    }

    fn set_task_command_correlation_inner(
        &self,
        command_id: &str,
        correlation_id: &str,
        snapshots: &[(&str, &[u8])],
        updates: &[(&str, &str, &[u8])],
        recovery: Option<&TaskRecoveryTransition>,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let Some(bytes) = tx
            .query_row(
                "SELECT receipt_json FROM task_command_receipts WHERE command_id=?1",
                params![command_id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
        else {
            tx.commit()?;
            return Ok(None);
        };
        let mut receipt: CommandReceipt = serde_json::from_slice(&bytes)?;
        if !receipt.status.is_terminal() {
            receipt.correlation_id = Some(correlation_id.into());
            receipt.reason = Some(CommandReceiptReason {
                code: CommandReasonCode::ProviderAcceptanceUnknown,
                message: "accepted by the engine; waiting for provider correlation".into(),
            });
            receipt.recovery_action = Some(
                "Wait for provider acceptance or reconcile before creating a replacement command."
                    .into(),
            );
            receipt.observed_at_ms = now_ms();
            tx.execute(
                "UPDATE task_command_receipts SET receipt_json=?2,updated_at=?3 WHERE command_id=?1",
                params![command_id, serde_json::to_vec(&receipt)?, now_ms()],
            )?;
        }
        self.persist_snapshot_changes_tx(&tx, snapshots, &[], updates)?;
        if let Some(recovery) = recovery {
            apply_task_recovery_transition_tx(&tx, recovery)?;
        }
        tx.commit()?;
        Ok(Some(receipt))
    }

    /// Provider acceptance finalization. The correlation is written before
    /// dispatch, so even a very fast SessionStarted/Steered event finds the
    /// exact pending receipt. All matching changes commit atomically.
    pub fn confirm_task_command_correlation(
        &self,
        task_id: &str,
        correlation_id: &str,
        active_turn_id: &str,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
    ) -> Result<Vec<CommandReceipt>, StoreError> {
        self.confirm_task_command_correlation_with_snapshot_changes(
            task_id,
            correlation_id,
            active_turn_id,
            binding_id,
            binding_generation,
            &[],
            &[],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn confirm_task_command_correlation_with_snapshot_changes(
        &self,
        task_id: &str,
        correlation_id: &str,
        active_turn_id: &str,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        snapshots: &[(&str, &[u8])],
        updates: &[(&str, &str, &[u8])],
    ) -> Result<Vec<CommandReceipt>, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let rows = {
            let mut stmt = tx.prepare(
                "SELECT receipt_json FROM task_command_receipts WHERE task_id=?1 ORDER BY updated_at",
            )?;
            stmt.query_map(params![task_id], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut confirmed = Vec::new();
        for bytes in rows {
            let mut receipt: CommandReceipt = serde_json::from_slice(&bytes)?;
            if receipt.status != CommandReceiptStatus::Pending
                || receipt.correlation_id.as_deref() != Some(correlation_id)
            {
                continue;
            }
            receipt.status = CommandReceiptStatus::Applied;
            receipt.snapshot.binding_id = binding_id.map(str::to_owned);
            receipt.snapshot.binding_generation = binding_generation;
            receipt.snapshot.active_turn_id = Some(active_turn_id.into());
            receipt.reason = None;
            receipt.recovery_action = None;
            receipt.committed_at_ms = Some(now_ms());
            receipt.observed_at_ms = now_ms();
            confirmed.push(commit_task_command_tx(&tx, receipt, true)?);
        }
        if !confirmed.is_empty() {
            self.persist_snapshot_changes_tx(&tx, snapshots, &[], updates)?;
        }
        tx.commit()?;
        Ok(confirmed)
    }

    pub fn task_command_receipt(
        &self,
        command_id: &str,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        let bytes = self
            .conn()
            .query_row(
                "SELECT receipt_json FROM task_command_receipts WHERE command_id=?1",
                params![command_id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        bytes
            .map(|bytes| serde_json::from_slice(&bytes).map_err(StoreError::from))
            .transpose()
    }

    pub fn task_recovery(&self, task_id: &str) -> Result<Option<StoredTaskRecovery>, StoreError> {
        self.conn()
            .query_row(
                "SELECT task_id,chain_id,attempt_count,status,record_json,updated_at
                 FROM task_recovery_state WHERE task_id=?1",
                params![task_id],
                stored_task_recovery_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn task_recoveries(&self) -> Result<Vec<StoredTaskRecovery>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT task_id,chain_id,attempt_count,status,record_json,updated_at
             FROM task_recovery_state ORDER BY task_id",
        )?;
        Ok(stmt
            .query_map([], stored_task_recovery_from_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn put_task_recovery(&self, recovery: &StoredTaskRecovery) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        upsert_task_recovery_tx(&tx, recovery)?;
        tx.commit()?;
        Ok(())
    }

    pub fn transition_task_recovery(
        &self,
        transition: &TaskRecoveryTransition,
    ) -> Result<bool, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        match apply_task_recovery_transition_tx(&tx, transition) {
            Ok(()) => {
                tx.commit()?;
                Ok(true)
            }
            Err(StoreError::RecoveryConflict) => {
                tx.rollback()?;
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Latest durable command whose provider correlation is this message id.
    /// Used by recovery to distinguish accepted, rejected, and unknown without
    /// guessing from transcript text.
    pub fn task_command_receipt_for_correlation(
        &self,
        task_id: &str,
        correlation_id: &str,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT receipt_json FROM task_command_receipts
             WHERE task_id=?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt
            .query_map(params![task_id], |row| row.get::<_, Vec<u8>>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for bytes in rows {
            let receipt: CommandReceipt = serde_json::from_slice(&bytes)?;
            if receipt.correlation_id.as_deref() == Some(correlation_id) {
                return Ok(Some(receipt));
            }
        }
        Ok(None)
    }

    /// Read only the authoritative task projection. This is the normal UI
    /// bootstrap path and deliberately does not scan command history.
    pub fn task_command_snapshot(&self, task_id: &str) -> Result<TaskCommandRead, StoreError> {
        let conn = self.conn();
        let snapshot = load_task_command_snapshot(&conn, task_id)?;
        Ok(TaskCommandRead {
            snapshot,
            events: Vec::new(),
            reload_required: false,
            next_task_sequence: None,
            has_more: false,
        })
    }

    /// Read one bounded recovery page after a durable task-sequence cursor.
    /// A cursor outside retained history returns the authoritative snapshot
    /// with reload_required, never a misleading partial replay.
    pub fn read_task_commands(
        &self,
        task_id: &str,
        after_task_sequence: u64,
        limit: usize,
    ) -> Result<TaskCommandRead, StoreError> {
        let conn = self.conn();
        let snapshot = load_task_command_snapshot(&conn, task_id)?;
        let earliest = conn
            .query_row(
                "SELECT MIN(task_sequence) FROM task_command_events WHERE task_id=?1",
                params![task_id],
                |row| row.get::<_, Option<i64>>(0),
            )?
            .map(|value| value as u64);
        let reload_required = after_task_sequence > snapshot.task_sequence
            || earliest.is_some_and(|first| after_task_sequence.saturating_add(1) < first)
            || (earliest.is_none() && after_task_sequence < snapshot.task_sequence);
        if reload_required {
            return Ok(TaskCommandRead {
                snapshot,
                events: Vec::new(),
                reload_required: true,
                next_task_sequence: None,
                has_more: false,
            });
        }
        let limit = limit.clamp(1, 100);
        let mut stmt = conn.prepare(
            "SELECT sequence,task_sequence,command_id,receipt_json
             FROM task_command_events WHERE task_id=?1 AND task_sequence>?2
             ORDER BY task_sequence LIMIT ?3",
        )?;
        let events = stmt
            .query_map(
                params![task_id, after_task_sequence as i64, limit as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as u64,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, String>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )?
            .map(|row| {
                let (sequence, task_sequence, command_id, bytes) = row?;
                Ok(TaskCommandEvent {
                    sequence,
                    task_sequence,
                    command_id,
                    receipt: serde_json::from_slice(&bytes)?,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let next_task_sequence = events.last().map(|event| event.task_sequence);
        let has_more = next_task_sequence.is_some_and(|next| next < snapshot.task_sequence);
        Ok(TaskCommandRead {
            snapshot,
            events,
            reload_required: false,
            next_task_sequence,
            has_more,
        })
    }

    // Every snapshot path, including ACK/cursor persistence, must retain a
    // replay obligation for operations whose outbox write failed. Invalidate
    // the marker atomically with those operations becoming durable in a snapshot.
    fn invalidate_failed_publication(
        &self,
        tx: &rusqlite::Transaction<'_>,
        doc_id: &str,
    ) -> Result<(), StoreError> {
        if self
            .failed_publications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(doc_id)
        {
            tx.execute(
                "DELETE FROM chat_outbox_initialized WHERE doc_id=?1",
                params![doc_id],
            )?;
        }
        Ok(())
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A poisoned lock only means another thread panicked mid-query; the
        // connection itself is still usable.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn stored_task_recovery_from_row(
    row: &rusqlite::Row<'_>,
) -> Result<StoredTaskRecovery, rusqlite::Error> {
    let attempt_count = row.get::<_, i64>(2)?;
    Ok(StoredTaskRecovery {
        task_id: row.get(0)?,
        chain_id: row.get(1)?,
        attempt_count: u8::try_from(attempt_count).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        status: row.get(3)?,
        record_json: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn upsert_task_recovery_tx(
    tx: &rusqlite::Transaction<'_>,
    recovery: &StoredTaskRecovery,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO task_recovery_state
         (task_id,chain_id,attempt_count,status,record_json,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(task_id) DO UPDATE SET
           chain_id=excluded.chain_id,
           attempt_count=excluded.attempt_count,
           status=excluded.status,
           record_json=excluded.record_json,
           updated_at=excluded.updated_at",
        params![
            recovery.task_id,
            recovery.chain_id,
            i64::from(recovery.attempt_count),
            recovery.status,
            recovery.record_json,
            recovery.updated_at,
        ],
    )?;
    Ok(())
}

fn apply_task_recovery_transition_tx(
    tx: &rusqlite::Transaction<'_>,
    transition: &TaskRecoveryTransition,
) -> Result<(), StoreError> {
    if let Some(expected) = transition.expected.as_ref() {
        let current = tx
            .query_row(
                "SELECT chain_id,attempt_count,status FROM task_recovery_state WHERE task_id=?1",
                params![transition.next.task_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if current.as_ref()
            != Some(&(
                expected.chain_id.clone(),
                i64::from(expected.attempt_count),
                expected.status.clone(),
            ))
        {
            return Err(StoreError::RecoveryConflict);
        }
    }
    upsert_task_recovery_tx(tx, &transition.next)
}

fn replace_attachment_owners_tx<'a>(
    tx: &rusqlite::Transaction<'_>,
    device_id: &str,
    owner_kind: &str,
    owner_id: &str,
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM attachment_owners
         WHERE device_id=?1 AND owner_kind=?2 AND owner_id=?3",
        params![device_id, owner_kind, owner_id],
    )?;
    for path in paths {
        tx.execute(
            "INSERT OR IGNORE INTO attachment_owners
               (device_id,blob_path,owner_kind,owner_id,created_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![device_id, path, owner_kind, owner_id, now_ms()],
        )?;
    }
    Ok(())
}

fn load_task_command_snapshot(
    conn: &Connection,
    task_id: &str,
) -> Result<TaskCommandSnapshot, StoreError> {
    let row = conn
        .query_row(
            "SELECT revision,task_sequence,binding_id,binding_generation,active_turn_id
             FROM task_command_state WHERE task_id=?1",
            params![task_id],
            |row| {
                Ok(TaskCommandSnapshot {
                    task_id: task_id.into(),
                    task_revision: row.get::<_, i64>(0)? as u64,
                    task_sequence: row.get::<_, i64>(1)? as u64,
                    binding_id: row.get(2)?,
                    binding_generation: row.get::<_, Option<i64>>(3)?.map(|value| value as u32),
                    active_turn_id: row.get(4)?,
                })
            },
        )
        .optional()?;
    Ok(row.unwrap_or_else(|| TaskCommandSnapshot {
        task_id: task_id.into(),
        ..TaskCommandSnapshot::default()
    }))
}

fn commit_task_event_only_tx(
    tx: &rusqlite::Transaction<'_>,
    mut receipt: CommandReceipt,
) -> Result<CommandReceipt, StoreError> {
    let current = load_task_command_snapshot(tx, &receipt.task_id)?;
    let task_sequence = current.task_sequence.saturating_add(1);
    let sequence: u64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence),0)+1 FROM task_command_events",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64;
    receipt.snapshot.task_sequence = task_sequence;
    receipt.snapshot.task_revision = current.task_revision;
    receipt.event_sequence = Some(sequence);
    let bytes = serde_json::to_vec(&receipt)?;
    tx.execute(
        "UPDATE task_command_state SET task_sequence=?2,binding_id=?3,
         binding_generation=?4,active_turn_id=?5 WHERE task_id=?1",
        params![
            receipt.task_id,
            task_sequence as i64,
            receipt.snapshot.binding_id,
            receipt.snapshot.binding_generation.map(i64::from),
            receipt.snapshot.active_turn_id,
        ],
    )?;
    tx.execute(
        "INSERT INTO task_command_events(sequence,task_id,task_sequence,command_id,receipt_json)
         VALUES (?1,?2,?3,?4,?5)",
        params![
            sequence as i64,
            receipt.task_id,
            task_sequence as i64,
            receipt.command_id,
            bytes
        ],
    )?;
    Ok(receipt)
}

fn commit_task_command_tx(
    tx: &rusqlite::Transaction<'_>,
    mut receipt: CommandReceipt,
    applied: bool,
) -> Result<CommandReceipt, StoreError> {
    let current = load_task_command_snapshot(tx, &receipt.task_id)?;
    let task_sequence = current.task_sequence.saturating_add(1);
    let task_revision = current.task_revision.saturating_add(u64::from(applied));
    let sequence: u64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence),0)+1 FROM task_command_events",
        [],
        |row| row.get::<_, i64>(0),
    )? as u64;
    receipt.snapshot.task_sequence = task_sequence;
    receipt.snapshot.task_revision = task_revision;
    receipt.event_sequence = Some(sequence);
    let bytes = serde_json::to_vec(&receipt)?;
    tx.execute(
        "UPDATE task_command_state SET revision=?2,task_sequence=?3,binding_id=?4,
         binding_generation=?5,active_turn_id=?6 WHERE task_id=?1",
        params![
            receipt.task_id,
            task_revision as i64,
            task_sequence as i64,
            receipt.snapshot.binding_id,
            receipt.snapshot.binding_generation.map(i64::from),
            receipt.snapshot.active_turn_id,
        ],
    )?;
    tx.execute(
        "INSERT INTO task_command_events(sequence,task_id,task_sequence,command_id,receipt_json)
         VALUES (?1,?2,?3,?4,?5)",
        params![
            sequence as i64,
            receipt.task_id,
            task_sequence as i64,
            receipt.command_id,
            bytes
        ],
    )?;
    tx.execute(
        "INSERT INTO task_command_receipts(command_id,task_id,payload_hash,receipt_json,updated_at)
         VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(command_id) DO UPDATE SET receipt_json=excluded.receipt_json,
         updated_at=excluded.updated_at",
        params![
            receipt.command_id,
            receipt.task_id,
            receipt.payload_hash,
            serde_json::to_vec(&receipt)?,
            now_ms()
        ],
    )?;
    Ok(receipt)
}

fn append_text_in(
    sql: &mut String,
    values: &mut Vec<rusqlite::types::Value>,
    column: &str,
    items: impl IntoIterator<Item = String>,
) {
    let items = items.into_iter().collect::<Vec<_>>();
    if items.is_empty() {
        return;
    }
    sql.push_str(" AND ");
    sql.push_str(column);
    sql.push_str(" IN (");
    sql.push_str(
        &std::iter::repeat_n("?", items.len())
            .collect::<Vec<_>>()
            .join(","),
    );
    sql.push(')');
    values.extend(items.into_iter().map(rusqlite::types::Value::from));
}

fn append_lower_text_in(
    sql: &mut String,
    values: &mut Vec<rusqlite::types::Value>,
    column: &str,
    items: impl IntoIterator<Item = String>,
) {
    let items = items.into_iter().collect::<Vec<_>>();
    if items.is_empty() {
        return;
    }
    sql.push_str(" AND LOWER(");
    sql.push_str(column);
    sql.push_str(") IN (");
    sql.push_str(
        &std::iter::repeat_n("?", items.len())
            .collect::<Vec<_>>()
            .join(","),
    );
    sql.push(')');
    values.extend(items.into_iter().map(rusqlite::types::Value::from));
}

fn insert_activity_event(
    tx: &rusqlite::Transaction<'_>,
    event: &ActivityEnvelope,
) -> Result<(), StoreError> {
    let search_text = [
        Some(event.summary.as_str()),
        event.tool.as_deref(),
        event.app.as_deref(),
        event.file.as_deref(),
        event.command.as_deref(),
        event.model.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
    .to_lowercase();
    tx.execute(
        "INSERT INTO activity_events(
           event_id,task_id,project_id,device_id,harness,model,outcome,tool,app,
           timestamp_ms,sequence,search_text,envelope_json
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            event.event_id,
            event.task_id,
            event.project_id,
            event.device_id,
            event
                .harness
                .map(|value| serde_json::to_string(&value))
                .transpose()?,
            event.model,
            serde_json::to_string(&event.outcome)?,
            event.tool,
            event.app,
            event.timestamp_ms,
            event.sequence as i64,
            search_text,
            serde_json::to_vec(event)?,
        ],
    )?;
    Ok(())
}

fn activity_matches(
    request: &HistorySearchRequest,
    needle: &str,
    event: &ActivityEnvelope,
) -> bool {
    if request.scope == HistorySearchScope::SelectedProject
        && request.project_id.as_deref() != event.project_id.as_deref()
    {
        return false;
    }
    if let Some(cursor) = &request.cursor
        && (event.timestamp_ms, event.sequence, event.event_id.as_str())
            >= (
                cursor.timestamp_ms,
                cursor.sequence,
                cursor.event_id.as_str(),
            )
    {
        return false;
    }
    let filters = &request.filters;
    if !filters.device_ids.is_empty() && !filters.device_ids.contains(&event.device_id) {
        return false;
    }
    if !filters.harnesses.is_empty()
        && !event
            .harness
            .is_some_and(|harness| filters.harnesses.contains(&harness))
    {
        return false;
    }
    if !filters.models.is_empty()
        && !event.model.as_ref().is_some_and(|model| {
            filters
                .models
                .iter()
                .any(|value| value.eq_ignore_ascii_case(model))
        })
    {
        return false;
    }
    if !filters.outcomes.is_empty() && !filters.outcomes.contains(&event.outcome) {
        return false;
    }
    if !filters.tools.is_empty()
        && !event.tool.as_ref().is_some_and(|tool| {
            filters
                .tools
                .iter()
                .any(|value| value.eq_ignore_ascii_case(tool))
        })
    {
        return false;
    }
    if !filters.apps.is_empty()
        && !event.app.as_ref().is_some_and(|app| {
            filters
                .apps
                .iter()
                .any(|value| value.eq_ignore_ascii_case(app))
        })
    {
        return false;
    }
    if filters
        .after_ms
        .is_some_and(|after| event.timestamp_ms < after)
        || filters
            .before_ms
            .is_some_and(|before| event.timestamp_ms > before)
    {
        return false;
    }
    if needle.is_empty() {
        return true;
    }
    [
        Some(event.summary.as_str()),
        event.tool.as_deref(),
        event.app.as_deref(),
        event.file.as_deref(),
        event.command.as_deref(),
        event.model.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.to_lowercase().contains(needle))
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
         ) STRICT",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let version = index as i64 + 1;
        if version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, now_ms()],
        )?;
        tx.commit()?;
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_proto::{
        ACTIVITY_ENVELOPE_VERSION, ActivityKind, ActivityRedaction, ActivitySourceRef,
        HistorySearchFilters,
    };

    fn activity(
        event_id: &str,
        task_id: &str,
        project_id: &str,
        timestamp_ms: i64,
    ) -> ActivityEnvelope {
        ActivityEnvelope {
            schema_version: ACTIVITY_ENVELOPE_VERSION,
            event_id: event_id.into(),
            environment_id: "synthetic-environment".into(),
            device_id: "synthetic-machine".into(),
            task_id: task_id.into(),
            turn_id: format!("turn-{task_id}"),
            sequence: 1,
            timestamp_ms,
            kind: ActivityKind::Prompt,
            outcome: zeron_proto::ActivityOutcome::Succeeded,
            harness: None,
            provider: None,
            model: Some("fixture-model".into()),
            reasoning: None,
            project_id: Some(project_id.into()),
            workspace_id: Some("synthetic-workspace".into()),
            tool: None,
            app: None,
            file: None,
            command: None,
            summary: format!("searchable {event_id}"),
            redaction: ActivityRedaction::None,
            source: ActivitySourceRef {
                task_id: task_id.into(),
                message_id: format!("message-{event_id}"),
                part_id: Some("text".into()),
                available_here: true,
                unavailable_reason: None,
            },
        }
    }

    fn activity_request(project_id: &str) -> HistorySearchRequest {
        HistorySearchRequest {
            query: "searchable".into(),
            scope: HistorySearchScope::SelectedProject,
            project_id: Some(project_id.into()),
            filters: HistorySearchFilters::default(),
            cursor: None,
            limit: 50,
            target_device_id: None,
        }
    }

    #[test]
    fn snapshot_roundtrip_and_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();

        assert_eq!(store.load_snapshot("chat-1").unwrap(), None);
        store.save_snapshot("chat-1", b"v1").unwrap();
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"v1"[..])
        );
        store.save_snapshot("chat-1", b"v2-longer-bytes").unwrap();
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"v2-longer-bytes"[..])
        );
        // Distinct docs do not collide.
        store.save_snapshot("chat-2", b"other").unwrap();
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"v2-longer-bytes"[..])
        );
    }

    #[test]
    fn activity_index_reopens_deduplicates_pages_and_rolls_back_failed_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        store
            .replace_activity_events(
                "task-a",
                &[
                    activity("a-new", "task-a", "project-a", 20),
                    activity("a-old", "task-a", "project-a", 10),
                ],
            )
            .unwrap();
        // Duplicate delivery is a deterministic replacement, not a duplicate.
        store
            .replace_activity_events(
                "task-a",
                &[
                    activity("a-new", "task-a", "project-a", 20),
                    activity("a-old", "task-a", "project-a", 10),
                ],
            )
            .unwrap();
        let mut first = activity_request("project-a");
        first.limit = 1;
        let page = store.search_activity_events(&first).unwrap();
        assert_eq!(page.events[0].event_id, "a-new");
        assert!(page.next_cursor.is_some());
        first.cursor = page.next_cursor;
        assert_eq!(
            store.search_activity_events(&first).unwrap().events[0].event_id,
            "a-old"
        );
        drop(store);

        let store = DocsStore::open(dir.path()).unwrap();
        assert_eq!(
            store
                .search_activity_events(&activity_request("project-a"))
                .unwrap()
                .events
                .len(),
            2,
            "migration/reopen preserves the derived index"
        );
        let duplicate = activity("collision", "task-a", "project-a", 30);
        let failed = store.rebuild_activity_events(&[
            ("task-a".into(), vec![duplicate.clone()]),
            ("task-b".into(), vec![duplicate]),
        ]);
        assert!(failed.is_err(), "duplicate event ids abort the rebuild");
        assert_eq!(
            store
                .search_activity_events(&activity_request("project-a"))
                .unwrap()
                .events
                .len(),
            2,
            "a failed rebuild rolls back without losing prior rows"
        );
        store
            .save_activity_project_policy(&HistoryProjectPolicy {
                project_id: "project-a".into(),
                excluded_path_prefixes: Vec::new(),
                excluded_terms: vec!["searchable".into()],
                index_terminal_output: false,
                index_browser_fields: false,
                index_accessibility_text: false,
                index_screenshot_text: false,
            })
            .unwrap();
        assert!(
            store
                .search_activity_events(&activity_request("project-a"))
                .unwrap()
                .events
                .is_empty(),
            "a stricter policy atomically hides earlier derived rows before reconciliation"
        );
    }

    #[test]
    fn activity_index_serializes_concurrent_task_updates() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(DocsStore::open(dir.path()).unwrap());
        let mut workers = Vec::new();
        for index in 0..8 {
            let store = store.clone();
            workers.push(std::thread::spawn(move || {
                let task = format!("task-{index}");
                let event = activity(&format!("event-{index}"), &task, "project", index);
                store.replace_activity_events(&task, &[event]).unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(
            store
                .search_activity_events(&activity_request("project"))
                .unwrap()
                .events
                .len(),
            8
        );
    }

    #[test]
    fn cursor_rides_the_snapshot_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();

        // Plain saves (pre-chat2 path) read back with cursor/epoch 0.
        store.save_snapshot("chat-1", b"v1").unwrap();
        assert_eq!(
            store.load_snapshot_with_cursor("chat-1").unwrap(),
            Some((b"v1".to_vec(), 0, 0))
        );
        // Cursor and bytes land together; a re-save moves both.
        store
            .save_snapshot_with_cursor("chat-1", b"v2", 41, 2)
            .unwrap();
        assert_eq!(
            store.load_snapshot_with_cursor("chat-1").unwrap(),
            Some((b"v2".to_vec(), 41, 2))
        );
        // Plain load still works for cursor-written rows.
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"v2"[..])
        );
        // A plain save (legacy caller) clears nothing — cursor persists…
        store.save_snapshot("chat-1", b"v3").unwrap();
        let (bytes, cursor, epoch) = store.load_snapshot_with_cursor("chat-1").unwrap().unwrap();
        assert_eq!((bytes.as_slice(), cursor, epoch), (&b"v3"[..], 41, 2));
    }

    #[test]
    fn processed_ledger_claims_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();

        assert!(!store.is_processed("cmd-1").unwrap());
        assert!(store.mark_processed("cmd-1").unwrap(), "first mark claims");
        assert!(store.is_processed("cmd-1").unwrap());
        assert!(
            !store.mark_processed("cmd-1").unwrap(),
            "second mark must not re-claim"
        );
    }

    #[test]
    fn reopen_preserves_data_and_migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = DocsStore::open(dir.path()).unwrap();
            store.save_snapshot("chat-1", b"persisted").unwrap();
            store.mark_processed("cmd-1").unwrap();
        }
        let store = DocsStore::open(dir.path()).unwrap(); // re-runs migrate()
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"persisted"[..])
        );
        assert!(store.is_processed("cmd-1").unwrap());
        assert!(!store.mark_processed("cmd-1").unwrap());
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    #[test]
    fn outbox_survives_restart_initializes_once_and_preserves_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let original;
        {
            let store = DocsStore::open(dir.path()).unwrap();
            store
                .save_snapshot_with_cursor("chat", b"snapshot", 40229, 2)
                .unwrap();
            store
                .initialize_chat_outbox("chat", &[b"legacy-tail".to_vec()])
                .unwrap();
            original = store.pending_chat_updates("chat").unwrap();
            store
                .enqueue_chat_update("chat", "stable-id", b"new-turn")
                .unwrap();
            store
                .enqueue_chat_update("chat", "stable-id", b"new-turn")
                .unwrap();
        }
        let store = DocsStore::open(dir.path()).unwrap();
        store
            .initialize_chat_outbox("chat", &[b"must-not-reseed".to_vec()])
            .unwrap();
        let pending = store.pending_chat_updates("chat").unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0], original[0]);
        assert_eq!(pending[1], ("stable-id".into(), b"new-turn".to_vec()));
        assert_eq!(
            store.load_snapshot_with_cursor("chat").unwrap().unwrap(),
            (b"snapshot".to_vec(), 40229, 2)
        );
        store
            .acknowledge_chat_update("other-chat", "stable-id")
            .unwrap();
        assert_eq!(store.pending_chat_updates("chat").unwrap().len(), 2);
        store.acknowledge_chat_update("chat", "stable-id").unwrap();
        assert_eq!(store.pending_chat_updates("chat").unwrap(), original);
        store.delete_snapshot("chat").unwrap();
        assert!(store.pending_chat_updates("chat").unwrap().is_empty());
        assert!(!store.chat_outbox_initialized("chat").unwrap());
    }
}

#[cfg(test)]
mod draft_tests {
    use super::*;

    fn draft(revision: u64, text: &str, path: &str) -> ComposerDraft {
        ComposerDraft {
            version: zeron_proto::COMPOSER_DRAFT_VERSION,
            draft_id: "task-a".into(),
            task_id: Some("task-a".into()),
            revision,
            text: text.into(),
            attachments: vec![zeron_proto::DraftBlob {
                attachment_id: "image-a".into(),
                name: "fixture.png".into(),
                path: Some(path.into()),
                sha256: Some("synthetic".into()),
                size: Some(7),
                state: zeron_proto::DraftBlobState::Saved,
                error: None,
            }],
            appshots: vec![zeron_proto::DraftAppshot {
                id: "shot-a".into(),
                screenshot_attachment_id: "image-a".into(),
                app_name: "Fixture".into(),
                bundle_identifier: Some("test.fixture".into()),
                window_title: Some("Synthetic window".into()),
                accessibility_format_version: 1,
                accessibility_content: "synthetic captured context".into(),
                accessibility_truncated: false,
                screenshot_dimensions: Some((2, 2)),
                captured_at_ms: 1,
            }],
            context: zeron_proto::DraftContext {
                review_comments: vec![serde_json::json!({"path":"src/lib.rs","line":7})],
                quoted: Some(serde_json::json!({"messageId":"quoted"})),
                terminal: Some(serde_json::json!({"terminalId":"terminal"})),
            },
            execution: serde_json::json!({
                "model":"fixture-model",
                "providerAccountId":"fixture-account",
                "permissionMode":"auto"
            }),
            updated_at_ms: revision as i64,
        }
    }

    #[test]
    fn draft_revision_submission_and_blob_ownership_are_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("synthetic.png")
            .to_string_lossy()
            .to_string();
        let store = DocsStore::open(dir.path()).unwrap();

        let original = draft(7, "submitted", &path);
        assert_eq!(
            store.save_composer_draft("device-a", &original).unwrap(),
            ComposerDraftSaveOutcome::Saved
        );
        assert_eq!(store.attachment_owner_count("device-a", &path), 1);
        assert_eq!(
            store
                .save_composer_draft("device-a", &draft(6, "stale", &path))
                .unwrap(),
            ComposerDraftSaveOutcome::Stale {
                current_revision: 7
            },
            "a late async write must report the authoritative revision"
        );

        store
            .begin_draft_submission("device-a", "command-a", Some("task-a"), &original)
            .unwrap();
        assert_eq!(store.attachment_owner_count("device-a", &path), 2);
        let newer = draft(8, "newer typing", &path);
        assert_eq!(
            store.save_composer_draft("device-a", &newer).unwrap(),
            ComposerDraftSaveOutcome::Saved
        );
        assert!(
            !store
                .accept_draft_submission("device-a", "command-a", None)
                .unwrap(),
            "accepting revision 7 must not clear revision 8"
        );
        let read = store
            .read_composer_draft("device-a", Some("task-a"), None)
            .unwrap();
        assert_eq!(read.draft.unwrap().text, "newer typing");
        assert!(read.submissions.is_empty());
        assert_eq!(store.attachment_owner_count("device-a", &path), 1);

        store
            .begin_draft_submission("device-a", "command-b", Some("task-a"), &newer)
            .unwrap();
        drop(store);
        let reopened = DocsStore::open(dir.path()).unwrap();
        let pending = reopened
            .read_composer_draft("device-a", Some("task-a"), None)
            .unwrap();
        assert_eq!(pending.draft.as_ref().unwrap().text, "newer typing");
        assert_eq!(pending.submissions[0].state, DraftSubmissionState::Pending);
        reopened
            .mark_draft_submission_recoverable("device-a", "command-b")
            .unwrap();
        let read = reopened
            .read_composer_draft("device-a", Some("task-a"), None)
            .unwrap();
        assert_eq!(read.submissions.len(), 1);
        assert_eq!(read.submissions[0].state, DraftSubmissionState::Recoverable);
        assert_eq!(read.submissions[0].snapshot.appshots[0].app_name, "Fixture");

        assert!(
            reopened
                .accept_draft_submission("device-a", "command-b", Some("queue-a"))
                .unwrap()
        );
        assert!(
            reopened
                .read_composer_draft("device-a", Some("task-a"), None)
                .unwrap()
                .draft
                .is_none()
        );
        assert_eq!(reopened.attachment_owner_count("device-a", &path), 1);
    }
}

#[cfg(test)]
mod publication_failure_tests {
    use super::*;
    #[test]
    fn cursor_save_after_failed_outbox_write_keeps_a_durable_replay_obligation() {
        for cursor_save in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let store = DocsStore::open(dir.path()).unwrap();
            store
                .save_snapshot_with_cursor("chat", b"before", 42, 2)
                .unwrap();
            store.initialize_chat_outbox("chat", &[]).unwrap();
            store.conn().execute_batch("CREATE TRIGGER fail_publication BEFORE INSERT ON chat_outbox BEGIN SELECT RAISE(FAIL, 'injected disk failure'); END;").unwrap();
            assert!(
                store
                    .enqueue_chat_update("chat", "batch", b"cleanup")
                    .is_err()
            );
            if cursor_save {
                store
                    .save_snapshot_with_cursor("chat", b"after", 43, 2)
                    .unwrap();
            } else {
                store.save_snapshot("chat", b"after").unwrap();
            }
            drop(store);
            let reopened = DocsStore::open(dir.path()).unwrap();
            assert!(!reopened.chat_outbox_initialized("chat").unwrap());
            assert_eq!(
                reopened.load_snapshot_with_cursor("chat").unwrap().unwrap(),
                (b"after".to_vec(), if cursor_save { 43 } else { 42 }, 2)
            );
        }
    }
}

#[cfg(test)]
mod task_command_tests {
    use super::*;

    fn pending(command_id: &str, task_id: &str, hash: &str) -> CommandReceipt {
        CommandReceipt {
            command_id: command_id.into(),
            task_id: task_id.into(),
            intent: "rename".into(),
            payload_hash: hash.into(),
            status: CommandReceiptStatus::Pending,
            snapshot: TaskCommandSnapshot {
                task_id: task_id.into(),
                binding_id: Some("binding-1".into()),
                binding_generation: Some(1),
                active_turn_id: None,
                ..TaskCommandSnapshot::default()
            },
            event_sequence: None,
            actor_id: "actor".into(),
            client_id: "client".into(),
            client_session_id: "session".into(),
            execution_environment: Some("device".into()),
            correlation_id: None,
            reason: None,
            recovery_action: None,
            accepted_at_ms: 1,
            committed_at_ms: None,
            observed_at_ms: 1,
        }
    }

    #[test]
    fn receipts_survive_restart_detect_conflicts_and_recover_transactions() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let first = store
            .begin_task_command(pending("same", "task-a", "hash-a"), Some(0), Some(1), None)
            .unwrap();
        assert_eq!(first.kind, TaskCommandBeginKind::Accepted);
        assert_eq!(
            store
                .begin_task_command(pending("same", "task-a", "hash-a"), Some(0), Some(1), None,)
                .unwrap()
                .kind,
            TaskCommandBeginKind::Duplicate
        );
        let conflict = store
            .begin_task_command(pending("same", "task-b", "hash-b"), Some(0), Some(1), None)
            .unwrap();
        assert_eq!(conflict.kind, TaskCommandBeginKind::Conflict);
        assert_eq!(
            conflict.receipt.reason.unwrap().code,
            CommandReasonCode::CommandIdConflict
        );
        let conflict_retry = store
            .begin_task_command(pending("same", "task-b", "hash-b"), Some(0), Some(1), None)
            .unwrap();
        assert_eq!(conflict_retry.kind, TaskCommandBeginKind::Conflict);
        assert_eq!(
            store
                .read_task_commands("task-b", 0, 50)
                .unwrap()
                .events
                .len(),
            1,
            "response-loss retry returns the durable conflict without a second event"
        );
        let applied = store
            .complete_task_command(
                "same",
                CommandReceiptStatus::Applied,
                true,
                Some("binding-1"),
                Some(1),
                None,
                None,
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(applied.snapshot.task_revision, 1);
        assert_eq!(applied.snapshot.task_sequence, 1);

        drop(store);
        let store = DocsStore::open(dir.path()).unwrap();
        assert_eq!(
            store.task_command_receipt("same").unwrap().unwrap(),
            applied
        );
        let stale = store
            .begin_task_command(
                pending("stale", "task-a", "hash-stale"),
                Some(0),
                Some(1),
                None,
            )
            .unwrap();
        assert_eq!(stale.kind, TaskCommandBeginKind::Conflict);
        assert_eq!(
            stale.receipt.reason.unwrap().code,
            CommandReasonCode::StaleTaskRevision
        );
        assert!(
            store
                .read_task_commands("task-a", 999, 50)
                .unwrap()
                .reload_required
        );

        store
            .begin_task_command(
                pending("fail", "task-a", "hash-fail"),
                Some(1),
                Some(1),
                None,
            )
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TRIGGER fail_task_event BEFORE INSERT ON task_command_events
                 WHEN NEW.command_id='fail' BEGIN SELECT RAISE(ABORT, 'forced'); END;",
            )
            .unwrap();
        assert!(
            store
                .complete_task_command(
                    "fail",
                    CommandReceiptStatus::Applied,
                    true,
                    Some("binding-1"),
                    Some(1),
                    None,
                    None,
                    None,
                )
                .is_err()
        );
        let after_failure = store.task_command_snapshot("task-a").unwrap();
        assert_eq!(after_failure.snapshot.task_revision, 1);
        assert_eq!(after_failure.snapshot.task_sequence, 2);
        assert_eq!(
            store.task_command_receipt("fail").unwrap().unwrap().status,
            CommandReceiptStatus::Pending
        );
        store
            .conn()
            .execute_batch("DROP TRIGGER fail_task_event")
            .unwrap();
        let recovered = store
            .complete_task_command(
                "fail",
                CommandReceiptStatus::Applied,
                true,
                Some("binding-1"),
                Some(1),
                None,
                None,
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(recovered.snapshot.task_revision, 2);
        assert_eq!(recovered.snapshot.task_sequence, 3);

        let snapshot_only = store.task_command_snapshot("task-a").unwrap();
        assert!(snapshot_only.events.is_empty());
        assert_eq!(snapshot_only.snapshot.task_sequence, 3);
        let first_page = store.read_task_commands("task-a", 0, 1).unwrap();
        assert_eq!(first_page.events.len(), 1);
        assert_eq!(first_page.next_task_sequence, Some(1));
        assert!(first_page.has_more);
        let second_page = store
            .read_task_commands("task-a", first_page.next_task_sequence.unwrap(), 1)
            .unwrap();
        assert_eq!(second_page.events.len(), 1);
        assert_eq!(second_page.next_task_sequence, Some(2));
        assert!(second_page.has_more);

        store
            .conn()
            .execute(
                "DELETE FROM task_command_events WHERE task_id='task-a' AND task_sequence=1",
                [],
            )
            .unwrap();
        let retention_gap = store.read_task_commands("task-a", 0, 50).unwrap();
        assert!(retention_gap.reload_required);
        assert!(retention_gap.events.is_empty());
        assert_eq!(retention_gap.snapshot.task_sequence, 3);
    }
}
