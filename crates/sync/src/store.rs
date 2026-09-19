//! `DocsStore` — local SQLite persistence for doc snapshots and the
//! processed-command ledger (ARCHITECTURE §2 command plane: entries are marked
//! processed BEFORE execution so a crash can never double-execute a command).

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use zeron_proto::{
    CommandReasonCode, CommandReceipt, CommandReceiptReason, CommandReceiptStatus,
    TaskCommandEvent, TaskCommandRead, TaskCommandSnapshot,
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

/// SQLite-backed store under a data directory (`{data_dir}/docs.sqlite3`).
///
/// Holds warm-open doc snapshots (the DO room is authoritative; these make
/// cold starts instant and offline restarts possible) and the command ledger
/// that gives command execution mark-BEFORE-execute idempotence.
pub struct DocsStore {
    conn: Mutex<Connection>,
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
            failed_publications: Mutex::new(HashSet::new()),
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
        tx.execute(
            "INSERT INTO snapshots (doc_id, bytes, saved_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(doc_id) DO UPDATE SET bytes = excluded.bytes, saved_at = excluded.saved_at",
            params![doc_id, bytes, now_ms()],
        )?;
        self.invalidate_failed_publication(&tx, doc_id)?;
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
        tx.commit()?;
        Ok(Some(receipt))
    }

    /// Preserve an ambiguous provider outcome as pending. A retry with the same
    /// id reads this receipt and never invokes the provider again.
    pub fn mark_task_command_unknown(
        &self,
        command_id: &str,
        message: &str,
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
        tx.commit()?;
        Ok(Some(receipt))
    }

    pub fn set_task_command_correlation(
        &self,
        command_id: &str,
        correlation_id: &str,
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

    pub fn read_task_commands(
        &self,
        task_id: &str,
        after_task_sequence: Option<u64>,
    ) -> Result<TaskCommandRead, StoreError> {
        let conn = self.conn();
        let snapshot = load_task_command_snapshot(&conn, task_id)?;
        let reload_required =
            after_task_sequence.is_some_and(|cursor| cursor > snapshot.task_sequence);
        let from = if reload_required {
            snapshot.task_sequence
        } else {
            after_task_sequence.unwrap_or(0)
        };
        let mut stmt = conn.prepare(
            "SELECT sequence,task_sequence,command_id,receipt_json
             FROM task_command_events WHERE task_id=?1 AND task_sequence>?2
             ORDER BY task_sequence",
        )?;
        let events = if reload_required {
            Vec::new()
        } else {
            stmt.query_map(params![task_id, from as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })?
            .map(|row| {
                let (sequence, task_sequence, command_id, bytes) = row?;
                Ok(TaskCommandEvent {
                    sequence,
                    task_sequence,
                    command_id,
                    receipt: serde_json::from_slice(&bytes)?,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?
        };
        Ok(TaskCommandRead {
            snapshot,
            events,
            reload_required,
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
                .read_task_commands("task-b", None)
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
                .read_task_commands("task-a", Some(999))
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
        let after_failure = store.read_task_commands("task-a", None).unwrap();
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
    }
}
