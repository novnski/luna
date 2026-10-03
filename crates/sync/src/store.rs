//! `DocsStore` — local SQLite persistence for doc snapshots and the
//! processed-command ledger (ARCHITECTURE §2 command plane: entries are marked
//! processed BEFORE execution so a crash can never double-execute a command).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use luna_proto::{
    ActivityEnvelope, AppshotActivityKind, AppshotActivityRecord, AttentionItem, AttentionKind,
    AttentionLifecycle, AttentionMutation, AttentionTransition, AttentionTransitionKind,
    CommandReasonCode, CommandReceipt, CommandReceiptReason, CommandReceiptStatus, ComposerDraft,
    ComposerDraftRead, ComposerDraftSaveOutcome, DraftSubmission, DraftSubmissionState,
    HistoryCursor, HistoryProjectPolicy, HistorySearchPage, HistorySearchRequest,
    HistorySearchScope, MAX_PROMPT_STASH_ENTRIES, MAX_PROMPT_STASH_ENTRY_BYTES, PromptStashEntry,
    TaskCommandEvent, TaskCommandRead, TaskCommandSnapshot,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::write_epoch::{FlushPermit, WriteEpoch, WritePermit};

mod project_commands;
mod workspace_repair;
pub use workspace_repair::{
    LocalExecutableAuthority, WorkspaceMaterializationFence, WorkspaceRepairFence,
};

/// Errors surfaced by [`DocsStore`].
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(
        "database schema {found} is newer than supported {supported}; open it with a newer Luna version"
    )]
    NewerSchema { found: i64, supported: i64 },
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
    #[error("prompt stash id {0} already has different content")]
    PromptStashConflict(String),
    #[error("prompt stash is full (maximum 50 entries)")]
    PromptStashFull,
    #[error("prompt stash entry exceeds 1 MiB of metadata")]
    PromptStashEntryTooLarge,
    #[error("task {0} is being permanently deleted")]
    TaskCleanupPending(String),
    #[error("task recovery state changed before the requested transition")]
    RecoveryConflict,
    #[error("attention projection revision changed before the requested mutation")]
    AttentionRevisionConflict,
    #[error("attention mutation command id was reused with different content")]
    AttentionCommandConflict,
    #[error("cannot dismiss unresolved attention")]
    UnresolvedAttention,
    #[error("write requires an admitted writer or flush permit")]
    AdmissionRequired,
    #[error("write permit does not belong to this guarded store")]
    AdmissionMismatch,
    #[error("database is already open in a conflicting admission mode")]
    DatabaseOpenConflict,
    #[error("local executable authority changed before projection commit")]
    LocalExecutableAuthorityConflict,
    #[error("workspace repair authority: {0}")]
    WorkspaceRepairConflict(String),
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
    // v8 — L15 local activity/attention projection. Authoritative task,
    // request, recovery, queue, and connectivity state remains in its owning
    // subsystem; these rows persist minimized presentation lifecycle only.
    "CREATE TABLE attention_meta (
        singleton INTEGER PRIMARY KEY CHECK(singleton=1),
        revision INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
     ) STRICT;
     INSERT INTO attention_meta(singleton,revision,updated_at) VALUES(1,0,0);
     CREATE TABLE attention_items (
        item_id TEXT PRIMARY KEY,
        dedup_key TEXT NOT NULL UNIQUE,
        source_key TEXT NOT NULL,
        kind TEXT NOT NULL,
        lifecycle TEXT NOT NULL,
        actionable INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        dismissed INTEGER NOT NULL DEFAULT 0,
        item_json BLOB NOT NULL
     ) STRICT;
     CREATE INDEX attention_items_source ON attention_items(source_key,updated_at DESC);
     CREATE INDEX attention_items_recent ON attention_items(dismissed,lifecycle,updated_at DESC);
     CREATE TABLE attention_transitions (
        sequence INTEGER PRIMARY KEY AUTOINCREMENT,
        item_id TEXT NOT NULL,
        transition_json BLOB NOT NULL,
        claimed_by TEXT,
        claimed_at INTEGER
     ) STRICT;
     CREATE INDEX attention_transitions_claim ON attention_transitions(claimed_by,sequence);
     CREATE TABLE attention_mutations (
        command_id TEXT PRIMARY KEY,
        mutation_json BLOB NOT NULL,
        revision INTEGER NOT NULL,
        committed_at INTEGER NOT NULL
     ) STRICT;",
    // v9 — minimal, device-local Appshot lifecycle facts. The attachment and
    // semantic text remain owned by L08 drafts/transcripts, never this log.
    "CREATE TABLE appshot_activity (
        event_id TEXT PRIMARY KEY,
        draft_id TEXT NOT NULL,
        task_id TEXT,
        capture_id TEXT NOT NULL,
        target_environment_id TEXT,
        record_json BLOB NOT NULL
     ) STRICT;
     CREATE INDEX appshot_activity_task ON appshot_activity(task_id);",
    // v10 — L18 local cleanup checkpoint. Paths are only candidates from
    // task-owned, unsent drafts; the engine still checks the upload jail and
    // surviving owners before unlinking any bytes.
    "CREATE TABLE task_cleanup (
        task_id TEXT PRIMARY KEY,
        state TEXT NOT NULL,
        detail TEXT NOT NULL,
        updated_at INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE task_cleanup_paths (
        task_id TEXT NOT NULL,
        blob_path TEXT NOT NULL,
        PRIMARY KEY(task_id, blob_path)
     ) STRICT;
     CREATE TABLE project_cleanup (
        project_id TEXT PRIMARY KEY,
        child_ids_json BLOB NOT NULL,
        state TEXT NOT NULL,
        detail TEXT NOT NULL,
        updated_at INTEGER NOT NULL
     ) STRICT;",
    // v11 — distinguish an in-flight preparation owned by this engine boot
    // from one abandoned by a previous process. A second RPC service in the
    // same engine must never clear the first service's active preparation.
    "ALTER TABLE task_cleanup ADD COLUMN owner_boot_id TEXT NOT NULL DEFAULT '';
     ALTER TABLE project_cleanup ADD COLUMN owner_boot_id TEXT NOT NULL DEFAULT '';
     CREATE INDEX task_cleanup_pending ON task_cleanup(state,task_id);
     CREATE INDEX project_cleanup_pending ON project_cleanup(state,project_id);",
    // v12 — only locally accepted executable CRDT rows may reach the host
    // drains. Foreign chat2 imports cannot create or edit these records.
    "CREATE TABLE local_executable_authority (
        doc_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        row_id TEXT NOT NULL,
        payload_hash TEXT NOT NULL,
        PRIMARY KEY(doc_id,kind,row_id)
     ) STRICT;",
    // v13 — device-owned saved actions and attempt-fenced terminal receipts.
    "CREATE TABLE project_command_configs (
        project_id TEXT NOT NULL, device_id TEXT NOT NULL, config BLOB NOT NULL,
        PRIMARY KEY(project_id,device_id)
     ) STRICT;
     CREATE TABLE project_command_runs (
        id TEXT PRIMARY KEY, project_id TEXT NOT NULL, device_id TEXT NOT NULL,
        attempt INTEGER NOT NULL, updated_at INTEGER NOT NULL, receipt BLOB NOT NULL
     ) STRICT;
     CREATE INDEX project_command_runs_project ON project_command_runs(project_id,device_id);",
    // v14 — profile-local side-chat leases, saved history and no-replay sends.
    include_str!("migrations/side_chats.sql"),
    // v15 — original local execution authority survives association repair.
    include_str!("migrations/workspace_repair.sql"),
    // v16 — user-managed prompts survive independently of active drafts and
    // keep their uploaded bytes owned after the source draft is cleared.
    "CREATE TABLE prompt_stash (
        device_id TEXT NOT NULL,
        id TEXT NOT NULL,
        label TEXT NOT NULL,
        record_json BLOB NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY(device_id,id)
     ) STRICT;
     CREATE INDEX prompt_stash_recent
       ON prompt_stash(device_id,created_at DESC,id DESC);",
    // v17 — compare retry configuration with the original task defaults,
    // rather than the resolved per-turn request. NULL preserves legacy guards.
    "ALTER TABLE task_execution_lineage ADD COLUMN configuration BLOB;",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalExecutableKind {
    Command,
    Queue,
}

/// One exact local grant transition committed with its document projection.
#[derive(Debug, Clone)]
pub struct LocalExecutableChange {
    pub doc_id: String,
    pub kind: LocalExecutableKind,
    pub row_id: String,
    pub expected_hash: Option<String>,
    pub replacement_hash: Option<String>,
    pub expected_workspace_repair_revision: Option<u64>,
    pub replacement_workspace_repair_revision: Option<u64>,
}

impl LocalExecutableKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Queue => "queue",
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAttentionSnapshot {
    pub revision: u64,
    pub unresolved_count: u32,
    pub needs_attention: Vec<AttentionItem>,
    pub recent: Vec<AttentionItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttentionMutationCommit {
    pub duplicate: bool,
    pub revision: u64,
}

// Reserve the canonical database path before SQLite can open or migrate it.
// The mutex protects only map operations; an open may wait on SQLite without
// delaying admission decisions for unrelated databases. This is process-local:
// external SQLite/file writers and hard-link aliases are not covered.
static OPEN_DATABASES: OnceLock<Mutex<HashMap<PathBuf, OpenDatabase>>> = OnceLock::new();

enum OpenDatabase {
    Unguarded(usize),
    Guarded {
        coordinator: WriteEpoch,
        handles: usize,
    },
}

fn open_databases() -> &'static Mutex<HashMap<PathBuf, OpenDatabase>> {
    OPEN_DATABASES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn canonical_database_path(data_dir: &Path) -> Result<PathBuf, StoreError> {
    let data_dir = data_dir.canonicalize()?;
    let path = data_dir.join("docs.sqlite3");
    match std::fs::symlink_metadata(&path) {
        Ok(_) => Ok(path.canonicalize()?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(error.into()),
    }
}

struct OpenRegistration {
    path: PathBuf,
    guarded: bool,
}

impl OpenRegistration {
    fn acquire(path: PathBuf, coordinator: Option<&WriteEpoch>) -> Result<Self, StoreError> {
        let mut databases = open_databases()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match (databases.get_mut(&path), coordinator) {
            (None, Some(coordinator)) => {
                databases.insert(
                    path.clone(),
                    OpenDatabase::Guarded {
                        coordinator: coordinator.clone(),
                        handles: 1,
                    },
                );
            }
            (None, None) => {
                databases.insert(path.clone(), OpenDatabase::Unguarded(1));
            }
            (Some(OpenDatabase::Unguarded(handles)), None) => {
                *handles = handles
                    .checked_add(1)
                    .ok_or(StoreError::DatabaseOpenConflict)?;
            }
            (
                Some(OpenDatabase::Guarded {
                    coordinator: existing,
                    handles,
                }),
                Some(coordinator),
            ) if existing.same_coordinator(coordinator) => {
                *handles = handles
                    .checked_add(1)
                    .ok_or(StoreError::DatabaseOpenConflict)?;
            }
            (Some(OpenDatabase::Guarded { .. }), Some(_)) => {
                return Err(StoreError::AdmissionMismatch);
            }
            _ => return Err(StoreError::DatabaseOpenConflict),
        }
        Ok(Self {
            path,
            guarded: coordinator.is_some(),
        })
    }
}

impl Drop for OpenRegistration {
    fn drop(&mut self) {
        let mut databases = open_databases()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let remove = match databases.get_mut(&self.path) {
            Some(OpenDatabase::Unguarded(handles)) if !self.guarded => {
                debug_assert!(*handles > 0);
                *handles -= 1;
                *handles == 0
            }
            Some(OpenDatabase::Guarded { handles, .. }) if self.guarded => {
                debug_assert!(*handles > 0);
                *handles -= 1;
                *handles == 0
            }
            _ => {
                debug_assert!(false, "database open registration changed before drop");
                return;
            }
        };
        if remove {
            databases.remove(&self.path);
        }
    }
}

/// SQLite-backed store under a data directory (`{data_dir}/docs.sqlite3`).
///
/// Holds warm-open doc snapshots (the DO room is authoritative; these make
/// cold starts instant and offline restarts possible) and the command ledger
/// that gives command execution mark-BEFORE-execute idempotence.
pub struct DocsStore {
    // Guarded stores expose this connection only for reads. Unguarded stores
    // retain their existing single read/write connection here.
    conn: Mutex<Connection>,
    // Guarded snapshot saves reach this connection only after permit checks.
    snapshot_writer: Option<Mutex<Connection>>,
    activity_gate: Mutex<()>,
    failed_publications: Mutex<HashSet<String>>,
    // Other mutators on a guarded store fail closed on the read-only conn;
    // they still need admitted APIs before product wiring is possible.
    write_epoch: Option<WriteEpoch>,
    // Last field: release the path reservation after both connections close.
    _registration: OpenRegistration,
}

impl DocsStore {
    /// Open (creating directory, database, and schema as needed).
    /// Refuses an active database held by any guarded handle in this process.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        std::fs::create_dir_all(data_dir.as_ref())?;
        let data_dir = data_dir.as_ref().canonicalize()?;
        let registration = OpenRegistration::acquire(canonical_database_path(&data_dir)?, None)?;
        Self::open_inner(&data_dir, None, registration)
    }

    /// Synthetic guard seam. The directory must already exist inside the
    /// coordinator's canonical device root. Validation happens before SQLite
    /// open, PRAGMA updates, and migrations, all of which may write.
    /// A process-local path reservation blocks overlapping unguarded handles
    /// and permits another guarded handle only for this same coordinator.
    pub fn open_guarded(
        data_dir: impl AsRef<Path>,
        coordinator: WriteEpoch,
        permit: &WritePermit,
    ) -> Result<Self, StoreError> {
        if !coordinator.permits_write(permit) {
            return Err(StoreError::AdmissionMismatch);
        }
        let data_dir = data_dir.as_ref().canonicalize()?;
        if !data_dir.is_dir() || !data_dir.starts_with(coordinator.root()) {
            return Err(StoreError::AdmissionMismatch);
        }
        let database = canonical_database_path(&data_dir)?;
        if !database.starts_with(coordinator.root()) {
            return Err(StoreError::AdmissionMismatch);
        }
        let registration = OpenRegistration::acquire(database, Some(&coordinator))?;
        Self::open_inner(&data_dir, Some(coordinator), registration)
    }

    fn open_inner(
        data_dir: &Path,
        write_epoch: Option<WriteEpoch>,
        registration: OpenRegistration,
    ) -> Result<Self, StoreError> {
        let database = data_dir.join("docs.sqlite3");
        let mut writer = Connection::open(&database)?;
        ensure_known_schema(&writer)?;
        writer.pragma_update(None, "journal_mode", "WAL")?;
        writer.pragma_update(None, "synchronous", "NORMAL")?;
        writer.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&mut writer)?;
        let (conn, snapshot_writer) = if write_epoch.is_some() {
            let reader = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            reader.busy_timeout(std::time::Duration::from_secs(5))?;
            reader.pragma_update(None, "query_only", true)?;
            (reader, Some(Mutex::new(writer)))
        } else {
            (writer, None)
        };
        Ok(Self {
            conn: Mutex::new(conn),
            snapshot_writer,
            activity_gate: Mutex::new(()),
            failed_publications: Mutex::new(HashSet::new()),
            write_epoch,
            _registration: registration,
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

    /// Refresh only Appshot rows after a local draft mutation. Transcript rows
    /// are not read or replaced by the capture bridge.
    pub fn replace_appshot_activity_events(
        &self,
        task_id: &str,
        events: &[ActivityEnvelope],
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM activity_events WHERE task_id=?1 AND event_id GLOB 'appshot:*'",
            params![task_id],
        )?;
        for event in events {
            insert_activity_event(&tx, event)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn appshot_activity_for_task(
        &self,
        task_id: &str,
    ) -> Result<Vec<AppshotActivityRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT record_json,task_id,target_environment_id FROM appshot_activity WHERE task_id=?1 ORDER BY rowid",
        )?;
        let rows = stmt
            .query_map(params![task_id], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(bytes, task_id, environment_id)| {
                let mut record: AppshotActivityRecord = serde_json::from_slice(&bytes)?;
                record.task_id = Some(task_id);
                record.target_environment_id = environment_id;
                Ok(record)
            })
            .collect()
    }

    pub fn has_appshot_activity_for_task(&self, task_id: &str) -> Result<bool, StoreError> {
        self.conn()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM appshot_activity WHERE task_id=?1 LIMIT 1)",
                params![task_id],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    /// Snapshot the task's host identity once. A later task move cannot
    /// rewrite where an earlier capture was staged or sent.
    pub fn bind_appshot_target_environment(
        &self,
        task_id: &str,
        environment_id: &str,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE appshot_activity SET target_environment_id=?2
             WHERE task_id=?1 AND target_environment_id IS NULL",
            params![task_id, environment_id],
        )?;
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

    /// Page one task's already minimized L14 activity rows. Cross-task reads
    /// must not fall back to raw transcript content when the index is empty.
    pub fn read_activity_task_page(
        &self,
        task_id: &str,
        cursor: Option<&HistoryCursor>,
        limit: usize,
    ) -> Result<(Vec<ActivityEnvelope>, Option<HistoryCursor>, bool), StoreError> {
        let limit = limit.clamp(1, 20);
        let conn = self.conn();
        let mut sql = String::from("SELECT envelope_json FROM activity_events WHERE task_id=?");
        let mut values = vec![rusqlite::types::Value::from(task_id.to_owned())];
        if let Some(cursor) = cursor {
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
        sql.push_str(" ORDER BY timestamp_ms DESC,sequence DESC,event_id DESC LIMIT ?");
        values.push(((limit + 1) as i64).into());
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                row.get::<_, Vec<u8>>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut events = rows
            .iter()
            .map(|bytes| serde_json::from_slice::<ActivityEnvelope>(bytes))
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = events.len() > limit;
        events.truncate(limit);
        let next_cursor = has_more.then(|| {
            let last = events.last().expect("paged task activity has a last row");
            HistoryCursor {
                timestamp_ms: last.timestamp_ms,
                sequence: last.sequence,
                event_id: last.event_id.clone(),
            }
        });
        Ok((events, next_cursor, has_more))
    }

    /// Reconcile the minimized Activity projection with a fresh authoritative
    /// snapshot. Presentation state survives unchanged; only disappearance
    /// from one of the explicitly managed producer kinds resolves an item.
    pub fn reconcile_attention_items(
        &self,
        desired: &[AttentionItem],
        managed_kinds: &[AttentionKind],
    ) -> Result<u64, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut changed = false;
        let now = now_ms();
        let desired_keys = desired
            .iter()
            .map(|item| item.dedup_key.as_str())
            .collect::<HashSet<_>>();

        for incoming in desired {
            let existing: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT item_json FROM attention_items WHERE dedup_key=?1",
                    params![incoming.dedup_key],
                    |row| row.get(0),
                )
                .optional()?;
            let (mut item, inserted, reopened, kind_changed, dismissed) =
                if let Some(bytes) = existing {
                    let previous: AttentionItem = serde_json::from_slice(&bytes)?;
                    let mut next = incoming.clone();
                    next.id = previous.id.clone();
                    next.created_at_ms = previous.created_at_ms;
                    next.seen_at_ms = previous.seen_at_ms;
                    next.acknowledged_at_ms = previous.acknowledged_at_ms;
                    let reopened =
                        previous.lifecycle == AttentionLifecycle::Resolved && incoming.unresolved();
                    let kind_changed = next.kind != previous.kind && next.unresolved();
                    if reopened || kind_changed {
                        next.seen_at_ms = None;
                        next.acknowledged_at_ms = None;
                    }
                    if incoming.unresolved() {
                        next.resolved_at_ms = None;
                        next.lifecycle = if next.acknowledged_at_ms.is_some() {
                            AttentionLifecycle::Acknowledged
                        } else {
                            AttentionLifecycle::Unresolved
                        };
                    }
                    // A producer heartbeat is not a new Activity transition. The
                    // source key denotes a material authoritative transition.
                    if next.source_key == previous.source_key {
                        next.updated_at_ms = previous.updated_at_ms;
                        if next.lifecycle == AttentionLifecycle::Resolved {
                            next.resolved_at_ms = previous.resolved_at_ms;
                        }
                    }
                    if previous == next {
                        continue;
                    }
                    let dismissed: bool = tx.query_row(
                        "SELECT dismissed FROM attention_items WHERE item_id=?1",
                        params![previous.id],
                        |row| row.get(0),
                    )?;
                    (next, false, reopened, kind_changed, dismissed && !reopened)
                } else {
                    (incoming.clone(), true, false, false, false)
                };
            if inserted {
                item.updated_at_ms = item.updated_at_ms.max(now);
            }
            tx.execute(
                "INSERT INTO attention_items(
                    item_id,dedup_key,source_key,kind,lifecycle,actionable,updated_at,dismissed,item_json
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT(item_id) DO UPDATE SET
                    dedup_key=excluded.dedup_key,source_key=excluded.source_key,
                    kind=excluded.kind,lifecycle=excluded.lifecycle,
                    actionable=excluded.actionable,updated_at=excluded.updated_at,
                    dismissed=excluded.dismissed,item_json=excluded.item_json",
                params![
                    item.id,
                    item.dedup_key,
                    item.source_key,
                    enum_key(&item.kind)?,
                    enum_key(&item.lifecycle)?,
                    item.actionable(),
                    item.updated_at_ms,
                    dismissed,
                    serde_json::to_vec(&item)?,
                ],
            )?;
            if inserted || reopened || kind_changed {
                let transition = if item.actionable() && item.unresolved() {
                    AttentionTransitionKind::NeedsAttention
                } else {
                    AttentionTransitionKind::Recent
                };
                insert_attention_transition(&tx, transition, &item)?;
            }
            changed = true;
        }

        let managed = managed_kinds
            .iter()
            .map(enum_key)
            .collect::<Result<HashSet<_>, _>>()?;
        let mut stmt = tx.prepare(
            "SELECT item_json FROM attention_items
             WHERE dismissed=0 AND lifecycle<>?1",
        )?;
        let rows = stmt
            .query_map(params![enum_key(&AttentionLifecycle::Resolved)?], |row| {
                row.get::<_, Vec<u8>>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for bytes in rows {
            let mut item: AttentionItem = serde_json::from_slice(&bytes)?;
            if !managed.contains(&enum_key(&item.kind)?)
                || desired_keys.contains(item.dedup_key.as_str())
            {
                continue;
            }
            item.lifecycle = AttentionLifecycle::Resolved;
            item.resolved_at_ms = Some(now);
            item.updated_at_ms = now;
            update_attention_item(&tx, &item, false)?;
            insert_attention_transition(&tx, AttentionTransitionKind::Resolved, &item)?;
            changed = true;
        }

        // BB's inspected notification center keeps 200 rows. LUNA applies
        // that bound only to Recent presentation; unresolved action items are
        // excluded from this pruning query and therefore never evicted.
        let mut stmt = tx.prepare(
            "SELECT item_id FROM attention_items
             WHERE dismissed=0 AND (lifecycle=?1 OR actionable=0)
             ORDER BY updated_at DESC,item_id DESC LIMIT -1 OFFSET 200",
        )?;
        let overflow = stmt
            .query_map(params![enum_key(&AttentionLifecycle::Resolved)?], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for item_id in overflow {
            changed |= tx.execute(
                "UPDATE attention_items SET dismissed=1 WHERE item_id=?1 AND dismissed=0",
                params![item_id],
            )? > 0;
        }

        let revision = if changed {
            bump_attention_revision(&tx, now)?
        } else {
            attention_revision(&tx)?
        };
        tx.commit()?;
        Ok(revision)
    }

    pub fn attention_snapshot(&self) -> Result<StoredAttentionSnapshot, StoreError> {
        let conn = self.conn();
        let revision = attention_revision(&conn)?;
        let resolved_key = enum_key(&AttentionLifecycle::Resolved)?;
        let needs_attention = read_attention_items(
            &conn,
            "SELECT item_json FROM attention_items
             WHERE dismissed=0 AND actionable=1 AND lifecycle<>?1
             ORDER BY
               CASE kind
                 WHEN '\"approvalRequired\"' THEN 0
                 WHEN '\"userInputRequired\"' THEN 1
                 WHEN '\"taskFailed\"' THEN 2
                 WHEN '\"queuedSendFailed\"' THEN 3
                 ELSE 4 END,
               updated_at DESC,item_id DESC",
            params![resolved_key.clone()],
        )?;
        let recent = read_attention_items(
            &conn,
            "SELECT item_json FROM attention_items
             WHERE dismissed=0 AND (lifecycle=?1 OR actionable=0)
             ORDER BY updated_at DESC,item_id DESC LIMIT 200",
            params![resolved_key],
        )?;
        Ok(StoredAttentionSnapshot {
            revision,
            unresolved_count: needs_attention.len().try_into().unwrap_or(u32::MAX),
            needs_attention,
            recent,
        })
    }

    /// Mark-before-delivery avoids sound/banner replay storms after a crash or
    /// reconnect. A baseline claim intentionally returns no historical rows.
    pub fn claim_attention_transitions(
        &self,
        consumer_id: &str,
        deliver_existing: bool,
    ) -> Result<Vec<AttentionTransition>, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let now = now_ms();
        if !deliver_existing {
            tx.execute(
                "UPDATE attention_transitions SET claimed_by=?1,claimed_at=?2 WHERE claimed_by IS NULL",
                params![consumer_id, now],
            )?;
            prune_claimed_attention_transitions(&tx)?;
            tx.commit()?;
            return Ok(Vec::new());
        }
        let mut stmt = tx.prepare(
            "SELECT sequence,transition_json FROM attention_transitions
             WHERE claimed_by IS NULL ORDER BY sequence LIMIT 100",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, u64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut transitions = Vec::with_capacity(rows.len());
        for (sequence, bytes) in rows {
            tx.execute(
                "UPDATE attention_transitions SET claimed_by=?1,claimed_at=?2
                 WHERE sequence=?3 AND claimed_by IS NULL",
                params![consumer_id, now, sequence],
            )?;
            let mut transition: AttentionTransition = serde_json::from_slice(&bytes)?;
            transition.sequence = sequence;
            transitions.push(transition);
        }
        prune_claimed_attention_transitions(&tx)?;
        tx.commit()?;
        Ok(transitions)
    }

    pub fn mutate_attention(
        &self,
        command_id: &str,
        expected_revision: u64,
        mutation: &AttentionMutation,
    ) -> Result<AttentionMutationCommit, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mutation_json = serde_json::to_vec(mutation)?;
        if let Some((prior_json, revision)) = tx
            .query_row(
                "SELECT mutation_json,revision FROM attention_mutations WHERE command_id=?1",
                params![command_id],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, u64>(1)?)),
            )
            .optional()?
        {
            if prior_json != mutation_json {
                return Err(StoreError::AttentionCommandConflict);
            }
            tx.commit()?;
            return Ok(AttentionMutationCommit {
                duplicate: true,
                revision,
            });
        }
        let current = attention_revision(&tx)?;
        if current != expected_revision {
            return Err(StoreError::AttentionRevisionConflict);
        }
        let now = now_ms();
        let mut changed = false;
        match mutation {
            AttentionMutation::MarkSeen { item_ids } => {
                for item_id in item_ids {
                    if let Some(mut item) = read_attention_item(&tx, item_id)?
                        && item.seen_at_ms.is_none()
                    {
                        item.seen_at_ms = Some(now);
                        update_attention_item(&tx, &item, false)?;
                        changed = true;
                    }
                }
            }
            AttentionMutation::Acknowledge { item_id } => {
                if let Some(mut item) = read_attention_item(&tx, item_id)?
                    && item.unresolved()
                    && item.acknowledged_at_ms.is_none()
                {
                    item.acknowledged_at_ms = Some(now);
                    item.lifecycle = AttentionLifecycle::Acknowledged;
                    item.updated_at_ms = now;
                    update_attention_item(&tx, &item, false)?;
                    changed = true;
                }
            }
            AttentionMutation::DismissRecent { item_id } => {
                if let Some(item) = read_attention_item(&tx, item_id)? {
                    if item.actionable() && item.unresolved() {
                        return Err(StoreError::UnresolvedAttention);
                    }
                    changed |= tx.execute(
                        "UPDATE attention_items SET dismissed=1 WHERE item_id=?1 AND dismissed=0",
                        params![item_id],
                    )? > 0;
                }
            }
            AttentionMutation::ClearRecent => {
                changed |= tx.execute(
                    "UPDATE attention_items SET dismissed=1
                     WHERE dismissed=0 AND (lifecycle=?1 OR actionable=0)",
                    params![enum_key(&AttentionLifecycle::Resolved)?],
                )? > 0;
            }
        }
        let revision = if changed {
            bump_attention_revision(&tx, now)?
        } else {
            current
        };
        tx.execute(
            "INSERT INTO attention_mutations(command_id,mutation_json,revision,committed_at) VALUES(?1,?2,?3,?4)",
            params![command_id, mutation_json, revision, now],
        )?;
        tx.commit()?;
        Ok(AttentionMutationCommit {
            duplicate: false,
            revision,
        })
    }

    /// Insert before sending. Stable IDs make crash-after-ACK replay idempotent.
    pub fn enqueue_chat_update(
        &self,
        doc_id: &str,
        batch_id: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        // A guarded legacy call is an admission failure, not a publication
        // failure: recording it would invalidate a later valid snapshot's
        // durable outbox-initialized marker.
        if self.write_epoch.is_some() {
            return Err(StoreError::AdmissionRequired);
        }
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
        self.require_uncoordinated_snapshot_save()?;
        self.save_snapshot_inner(doc_id, bytes)
    }

    pub fn save_snapshot_admitted(
        &self,
        permit: &WritePermit,
        doc_id: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.require_writer(permit)?;
        self.save_snapshot_inner(doc_id, bytes)
    }

    pub fn save_snapshot_flushing(
        &self,
        permit: &FlushPermit<'_>,
        doc_id: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.require_flush(permit)?;
        self.save_snapshot_inner(doc_id, bytes)
    }

    fn save_snapshot_inner(&self, doc_id: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let mut conn = self.snapshot_write_conn();
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
        self.require_uncoordinated_snapshot_save()?;
        self.save_snapshot_with_updates_inner(doc_id, bytes, updates)
    }

    pub fn save_snapshot_with_updates_admitted(
        &self,
        permit: &WritePermit,
        doc_id: &str,
        bytes: &[u8],
        updates: &[(&str, &[u8])],
    ) -> Result<(), StoreError> {
        self.require_writer(permit)?;
        self.save_snapshot_with_updates_inner(doc_id, bytes, updates)
    }

    pub fn save_snapshot_with_updates_flushing(
        &self,
        permit: &FlushPermit<'_>,
        doc_id: &str,
        bytes: &[u8],
        updates: &[(&str, &[u8])],
    ) -> Result<(), StoreError> {
        self.require_flush(permit)?;
        self.save_snapshot_with_updates_inner(doc_id, bytes, updates)
    }

    fn save_snapshot_with_updates_inner(
        &self,
        doc_id: &str,
        bytes: &[u8],
        updates: &[(&str, &[u8])],
    ) -> Result<(), StoreError> {
        self.save_snapshot_with_updates_and_authority_inner(doc_id, bytes, updates, &[])
    }

    pub fn save_snapshot_with_updates_and_authority(
        &self,
        doc_id: &str,
        bytes: &[u8],
        updates: &[(&str, &[u8])],
        authority: &[LocalExecutableChange],
    ) -> Result<(), StoreError> {
        self.require_uncoordinated_snapshot_save()?;
        self.save_snapshot_with_updates_and_authority_inner(doc_id, bytes, updates, authority)
    }

    fn save_snapshot_with_updates_and_authority_inner(
        &self,
        doc_id: &str,
        bytes: &[u8],
        updates: &[(&str, &[u8])],
        authority: &[LocalExecutableChange],
    ) -> Result<(), StoreError> {
        let mut conn = self.snapshot_write_conn();
        let tx = conn.transaction()?;
        let updates = updates
            .iter()
            .map(|(batch_id, bytes)| (doc_id, *batch_id, *bytes))
            .collect::<Vec<_>>();
        self.persist_snapshot_changes_tx(&tx, &[(doc_id, bytes)], &[], &updates)?;
        Self::apply_local_executable_changes_tx(&tx, authority)?;
        tx.commit()?;
        Ok(())
    }

    /// Durable, local-only correlation for an executable row in a CRDT doc.
    /// `expected` is a compare-and-set fence for an explicitly local edit;
    /// `None` inserts a new grant without replacing a conflicting old one.
    /// Remote row imports never call this method.
    pub fn record_local_executable(
        &self,
        doc_id: &str,
        kind: LocalExecutableKind,
        row_id: &str,
        payload_hash: &str,
        expected: Option<&str>,
    ) -> Result<bool, StoreError> {
        let revision = self.workspace_repair_revision(doc_id)?;
        self.record_local_executable_at_workspace_revision(
            doc_id,
            kind,
            row_id,
            payload_hash,
            expected,
            revision,
        )
    }

    /// The stamp is captured at the original local admission, before any
    /// asynchronous staging. Existing authority cannot be silently resealed.
    #[allow(clippy::too_many_arguments)]
    pub fn record_local_executable_at_workspace_revision(
        &self,
        doc_id: &str,
        kind: LocalExecutableKind,
        row_id: &str,
        payload_hash: &str,
        expected: Option<&str>,
        workspace_repair_revision: u64,
    ) -> Result<bool, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if workspace_repair::workspace_repair_revision_in(&tx, doc_id)? != workspace_repair_revision
        {
            return Ok(false);
        }
        let changed = match expected {
            Some(prior) => tx.execute(
                "UPDATE local_executable_authority SET payload_hash=?4
                 WHERE doc_id=?1 AND kind=?2 AND row_id=?3 AND payload_hash=?5 AND workspace_repair_revision=?6",
                params![doc_id, kind.as_str(), row_id, payload_hash, prior, workspace_repair_revision],
            )?,
            None => tx.execute(
                "INSERT OR IGNORE INTO local_executable_authority(doc_id,kind,row_id,payload_hash,workspace_repair_revision)
                 VALUES (?1,?2,?3,?4,?5)",
                params![doc_id, kind.as_str(), row_id, payload_hash, workspace_repair_revision],
            )?,
        };
        if changed > 0 {
            tx.commit()?;
            return Ok(true);
        }
        let matched = tx
            .query_row(
                "SELECT payload_hash,workspace_repair_revision FROM local_executable_authority
             WHERE doc_id=?1 AND kind=?2 AND row_id=?3",
                params![doc_id, kind.as_str(), row_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
            )
            .optional()?
            .is_some_and(|(hash, revision)| {
                hash == payload_hash && revision == workspace_repair_revision
            });
        tx.commit()?;
        Ok(matched)
    }

    pub fn local_executable_hash(
        &self,
        doc_id: &str,
        kind: LocalExecutableKind,
        row_id: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT payload_hash FROM local_executable_authority
             WHERE doc_id=?1 AND kind=?2 AND row_id=?3",
                params![doc_id, kind.as_str(), row_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn local_executable_matches(
        &self,
        doc_id: &str,
        kind: LocalExecutableKind,
        row_id: &str,
        payload_hash: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM local_executable_authority
             WHERE doc_id=?1 AND kind=?2 AND row_id=?3 AND payload_hash=?4
             AND workspace_repair_revision=COALESCE((SELECT workspace_repair_revision FROM task_command_state WHERE task_id=?1),0))",
            params![doc_id, kind.as_str(), row_id, payload_hash], |row| row.get(0))?)
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
        self.require_uncoordinated_snapshot_save()?;
        self.save_snapshot_with_cursor_inner(doc_id, bytes, cursor, epoch)
    }

    pub fn save_snapshot_with_cursor_admitted(
        &self,
        permit: &WritePermit,
        doc_id: &str,
        bytes: &[u8],
        cursor: u64,
        epoch: u32,
    ) -> Result<(), StoreError> {
        self.require_writer(permit)?;
        self.save_snapshot_with_cursor_inner(doc_id, bytes, cursor, epoch)
    }

    pub fn save_snapshot_with_cursor_flushing(
        &self,
        permit: &FlushPermit<'_>,
        doc_id: &str,
        bytes: &[u8],
        cursor: u64,
        epoch: u32,
    ) -> Result<(), StoreError> {
        self.require_flush(permit)?;
        self.save_snapshot_with_cursor_inner(doc_id, bytes, cursor, epoch)
    }

    fn save_snapshot_with_cursor_inner(
        &self,
        doc_id: &str,
        bytes: &[u8],
        cursor: u64,
        epoch: u32,
    ) -> Result<(), StoreError> {
        let mut conn = self.snapshot_write_conn();
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
        if let Some(task_id) = draft.task_id.as_deref() {
            ensure_task_not_cleaning(&tx, task_id)?;
        }
        let bytes = serde_json::to_vec(draft)?;
        let current = tx
            .query_row(
                "SELECT revision,record_json FROM composer_drafts
                 WHERE device_id=?1 AND draft_id=?2",
                params![device_id, draft.draft_id],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        let previous = current
            .as_ref()
            .map(|(_, bytes)| serde_json::from_slice::<ComposerDraft>(bytes))
            .transpose()?;
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
        let current_ids: HashSet<&str> =
            draft.appshots.iter().map(|shot| shot.id.as_str()).collect();
        let saved_ids: HashSet<&str> = draft
            .attachments
            .iter()
            .filter(|blob| blob.state == luna_proto::DraftBlobState::Saved)
            .map(|blob| blob.attachment_id.as_str())
            .collect();
        for shot in &draft.appshots {
            let base = format!("appshot:{}:{}", draft.draft_id, shot.id);
            let captured = AppshotActivityRecord {
                event_id: format!("{base}:captured"),
                draft_id: draft.draft_id.clone(),
                task_id: draft.task_id.clone(),
                capture_id: shot.id.clone(),
                screenshot_attachment_id: shot.screenshot_attachment_id.clone(),
                capture_device_id: device_id.into(),
                target_environment_id: None,
                kind: AppshotActivityKind::Captured,
                timestamp_ms: shot.captured_at_ms,
                app_name: shot.app_name.chars().take(128).collect(),
                message_id: None,
            };
            insert_appshot_activity_tx(&tx, &captured)?;
            if saved_ids.contains(shot.screenshot_attachment_id.as_str()) {
                insert_appshot_activity_tx(
                    &tx,
                    &AppshotActivityRecord {
                        event_id: format!("{base}:staged"),
                        kind: AppshotActivityKind::Staged,
                        timestamp_ms: draft.updated_at_ms,
                        ..captured
                    },
                )?;
            }
        }
        if let Some(previous) = previous.as_ref() {
            for shot in &previous.appshots {
                if current_ids.contains(shot.id.as_str()) {
                    continue;
                }
                insert_appshot_activity_tx(
                    &tx,
                    &AppshotActivityRecord {
                        event_id: format!(
                            "appshot:{}:{}:removed:{}",
                            draft.draft_id, shot.id, draft.revision
                        ),
                        draft_id: draft.draft_id.clone(),
                        task_id: draft.task_id.clone(),
                        capture_id: shot.id.clone(),
                        screenshot_attachment_id: shot.screenshot_attachment_id.clone(),
                        capture_device_id: device_id.into(),
                        target_environment_id: None,
                        kind: AppshotActivityKind::Removed,
                        timestamp_ms: draft.updated_at_ms,
                        app_name: shot.app_name.chars().take(128).collect(),
                        message_id: None,
                    },
                )?;
            }
        }
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

    /// Save one immutable stash entry and give every saved blob an independent
    /// owner in the same transaction. Retrying the same entry is safe.
    pub fn save_prompt_stash(
        &self,
        device_id: &str,
        entry: &PromptStashEntry,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let bytes = serde_json::to_vec(entry)?;
        if bytes.len() > MAX_PROMPT_STASH_ENTRY_BYTES {
            return Err(StoreError::PromptStashEntryTooLarge);
        }
        let existing: Option<Vec<u8>> = tx
            .query_row(
                "SELECT record_json FROM prompt_stash WHERE device_id=?1 AND id=?2",
                params![device_id, entry.id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing == bytes {
                return Ok(());
            }
            return Err(StoreError::PromptStashConflict(entry.id.clone()));
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM prompt_stash WHERE device_id=?1",
            params![device_id],
            |row| row.get(0),
        )?;
        if count >= MAX_PROMPT_STASH_ENTRIES as i64 {
            return Err(StoreError::PromptStashFull);
        }
        tx.execute(
            "INSERT INTO prompt_stash(device_id,id,label,record_json,created_at)
             VALUES(?1,?2,?3,?4,?5)",
            params![device_id, entry.id, entry.label, bytes, entry.created_at_ms],
        )?;
        replace_attachment_owners_tx(&tx, device_id, "stash", &entry.id, entry.saved_blob_paths())?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_prompt_stash(&self, device_id: &str) -> Result<Vec<PromptStashEntry>, StoreError> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT record_json FROM prompt_stash WHERE device_id=?1
             ORDER BY created_at DESC,id DESC LIMIT ?2",
        )?;
        let entries = statement
            .query_map(params![device_id, MAX_PROMPT_STASH_ENTRIES as i64], |row| {
                row.get::<_, Vec<u8>>(0)
            })?
            .map(|row| Ok(serde_json::from_slice(&row?)?))
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(entries)
    }

    /// Deleting a stash removes only its ownership rows. Shared blobs remain
    /// protected by any active draft, submission, queue, or other stash owner.
    pub fn delete_prompt_stash(&self, device_id: &str, id: &str) -> Result<bool, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let deleted = tx.execute(
            "DELETE FROM prompt_stash WHERE device_id=?1 AND id=?2",
            params![device_id, id],
        )? > 0;
        if deleted {
            tx.execute(
                "DELETE FROM attachment_owners
                 WHERE device_id=?1 AND owner_kind='stash' AND owner_id=?2",
                params![device_id, id],
            )?;
        }
        tx.commit()?;
        Ok(deleted)
    }

    /// Freeze the exact revision before dispatch. Reusing a command id with a
    /// different snapshot or destination is rejected, matching L07's immutable envelope.
    pub fn begin_draft_submission(
        &self,
        device_id: &str,
        command_id: &str,
        task_id: Option<&str>,
        snapshot: &ComposerDraft,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if let Some(task_id) = task_id {
            ensure_task_not_cleaning(&tx, task_id)?;
        }
        let bytes = serde_json::to_vec(snapshot)?;
        let existing: Option<(Vec<u8>, Option<String>)> = tx
            .query_row(
                "SELECT record_json,task_id FROM composer_draft_submissions
                 WHERE device_id=?1 AND command_id=?2",
                params![device_id, command_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((existing, existing_task_id)) = existing {
            if existing == bytes && existing_task_id.as_deref() == task_id {
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

    pub fn draft_submission_task_id(
        &self,
        device_id: &str,
        command_id: &str,
    ) -> Result<Option<String>, StoreError> {
        self.conn()
            .query_row(
                "SELECT task_id FROM composer_draft_submissions WHERE device_id=?1 AND command_id=?2",
                params![device_id, command_id],
                |row| row.get(0),
            )
            .optional()
            .map(|task_id| task_id.flatten())
            .map_err(StoreError::from)
    }

    /// Discard only a confirmed recoverable submission. Pending delivery must
    /// remain intact because its provider effect may still be unknown.
    pub fn discard_recoverable_draft_submission(
        &self,
        device_id: &str,
        command_id: &str,
    ) -> Result<(bool, Option<u64>), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let submission: Option<(String, Vec<u8>)> = tx
            .query_row(
                "SELECT draft_id,record_json FROM composer_draft_submissions
                 WHERE device_id=?1 AND command_id=?2 AND state='recoverable'",
                params![device_id, command_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((draft_id, submission_bytes)) = submission else {
            return Ok((false, None));
        };
        let submission: ComposerDraft = serde_json::from_slice(&submission_bytes)?;
        let saved_draft: Option<Vec<u8>> = tx
            .query_row(
                "SELECT record_json FROM composer_drafts WHERE device_id=?1 AND draft_id=?2",
                params![device_id, draft_id],
                |row| row.get(0),
            )
            .optional()?;
        let matching_draft = saved_draft
            .map(|bytes| serde_json::from_slice::<ComposerDraft>(&bytes))
            .transpose()?
            .filter(|draft| draft.same_message_content(&submission));
        tx.execute(
            "DELETE FROM composer_draft_submissions
             WHERE device_id=?1 AND command_id=?2 AND state='recoverable'",
            params![device_id, command_id],
        )?;
        tx.execute(
            "DELETE FROM attachment_owners
             WHERE device_id=?1 AND owner_kind='submission' AND owner_id=?2",
            params![device_id, command_id],
        )?;
        let mut cleared_revision = None;
        if let Some(mut draft) = matching_draft {
            draft.revision = draft.revision.max(submission.revision).saturating_add(1);
            draft.updated_at_ms = now_ms();
            draft.text.clear();
            // Typed identities are part of the matched submitted content; a
            // newer draft with different selections never reaches this branch.
            draft.input_items.clear();
            draft.attachments.clear();
            let removed_appshots = std::mem::take(&mut draft.appshots);
            draft.context = Default::default();
            tx.execute(
                "UPDATE composer_drafts SET revision=?3,record_json=?4,updated_at=?5
                 WHERE device_id=?1 AND draft_id=?2",
                params![
                    device_id,
                    draft_id,
                    draft.revision as i64,
                    serde_json::to_vec(&draft)?,
                    draft.updated_at_ms
                ],
            )?;
            tx.execute(
                "DELETE FROM attachment_owners
                 WHERE device_id=?1 AND owner_kind='draft' AND owner_id=?2",
                params![device_id, draft_id],
            )?;
            for shot in removed_appshots {
                insert_appshot_activity_tx(
                    &tx,
                    &AppshotActivityRecord {
                        event_id: format!(
                            "appshot:{}:{}:removed:{}",
                            draft.draft_id, shot.id, draft.revision
                        ),
                        draft_id: draft.draft_id.clone(),
                        task_id: draft.task_id.clone(),
                        capture_id: shot.id.clone(),
                        screenshot_attachment_id: shot.screenshot_attachment_id.clone(),
                        capture_device_id: device_id.into(),
                        target_environment_id: None,
                        kind: AppshotActivityKind::Removed,
                        timestamp_ms: draft.updated_at_ms,
                        app_name: shot.app_name.chars().take(128).collect(),
                        message_id: None,
                    },
                )?;
            }
            cleared_revision = Some(draft.revision);
        }
        tx.commit()?;
        Ok((true, cleared_revision))
    }

    /// Accept one submission and clear only the exact draft revision it froze.
    /// A queue id transfers blob ownership in the same transaction.
    pub fn accept_draft_submission(
        &self,
        device_id: &str,
        command_id: &str,
        queue_id: Option<&str>,
    ) -> Result<bool, StoreError> {
        self.accept_draft_submission_inner(device_id, command_id, queue_id, false)
    }

    /// A provider-accepted direct send also consumes any older recoverable
    /// copy of the same message, atomically with its own draft settlement.
    pub fn accept_applied_draft_submission(
        &self,
        device_id: &str,
        command_id: &str,
    ) -> Result<bool, StoreError> {
        self.accept_draft_submission_inner(device_id, command_id, None, true)
    }

    fn accept_draft_submission_inner(
        &self,
        device_id: &str,
        command_id: &str,
        queue_id: Option<&str>,
        supersede_recoverable: bool,
    ) -> Result<bool, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let row: Option<(String, u64, Vec<u8>, Option<String>)> = tx
            .query_row(
                "SELECT draft_id,draft_revision,record_json,task_id
                 FROM composer_draft_submissions
                 WHERE device_id=?1 AND command_id=?2",
                params![device_id, command_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, i64>(1)? as u64,
                        row.get(2)?,
                        row.get(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((draft_id, submitted_revision, bytes, task_id)) = row else {
            return Ok(false);
        };
        let snapshot: ComposerDraft = serde_json::from_slice(&bytes)?;
        if let Some(task_id) = task_id.as_deref() {
            for shot in &snapshot.appshots {
                tx.execute(
                    "UPDATE appshot_activity SET task_id=?1
                     WHERE draft_id=?2 AND capture_id=?3 AND task_id IS NULL",
                    params![task_id, draft_id, shot.id],
                )?;
                insert_appshot_activity_tx(
                    &tx,
                    &AppshotActivityRecord {
                        event_id: format!(
                            "appshot:{}:{}:submission:{}",
                            draft_id, shot.id, command_id
                        ),
                        draft_id: draft_id.clone(),
                        task_id: Some(task_id.into()),
                        capture_id: shot.id.clone(),
                        screenshot_attachment_id: shot.screenshot_attachment_id.clone(),
                        capture_device_id: device_id.into(),
                        target_environment_id: None,
                        kind: if queue_id.is_some() {
                            AppshotActivityKind::Queued
                        } else {
                            AppshotActivityKind::Sent
                        },
                        timestamp_ms: now_ms(),
                        app_name: shot.app_name.chars().take(128).collect(),
                        message_id: Some(command_id.into()),
                    },
                )?;
            }
        }
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
        if supersede_recoverable {
            if let Some(task_id) = task_id.as_deref() {
                let matching_ids = {
                    let mut statement = tx.prepare(
                        "SELECT command_id,record_json FROM composer_draft_submissions
                         WHERE device_id=?1 AND task_id=?2 AND state='recoverable'",
                    )?;
                    let rows = statement.query_map(params![device_id, task_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                    })?;
                    let mut matching_ids = Vec::new();
                    for row in rows {
                        let (older_id, bytes) = row?;
                        let older: ComposerDraft = serde_json::from_slice(&bytes)?;
                        if older.same_message_content(&snapshot) {
                            matching_ids.push(older_id);
                        }
                    }
                    matching_ids
                };
                for older_id in matching_ids {
                    tx.execute(
                        "DELETE FROM attachment_owners
                         WHERE device_id=?1 AND owner_kind='submission' AND owner_id=?2",
                        params![device_id, older_id],
                    )?;
                    tx.execute(
                        "DELETE FROM composer_draft_submissions
                         WHERE device_id=?1 AND command_id=?2 AND state='recoverable'",
                        params![device_id, older_id],
                    )?;
                }
            }
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

    /// Checkpoint an irreversible task cleanup before the registry tombstone.
    /// Only draft-only paths are candidates: submitted/queued/transcript bytes
    /// may be referenced by a surviving fork or another task.
    pub fn prepare_task_cleanup(
        &self,
        device_id: &str,
        task_id: &str,
        owner_boot_id: &str,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO task_cleanup(task_id,state,detail,updated_at,owner_boot_id)
             VALUES(?1,'pending','Local cleanup is pending',?2,?3)
             ON CONFLICT(task_id) DO UPDATE SET state='pending',
               detail=excluded.detail,updated_at=excluded.updated_at,
               owner_boot_id=excluded.owner_boot_id",
            params![task_id, now_ms(), owner_boot_id],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO task_cleanup_paths(task_id,blob_path)
             SELECT ?2,a.blob_path FROM attachment_owners a
             JOIN composer_drafts d ON d.device_id=a.device_id AND d.draft_id=a.owner_id
             WHERE a.device_id=?1 AND d.task_id=?2 AND a.owner_kind='draft'
               AND NOT EXISTS (SELECT 1 FROM composer_draft_submissions s
                               WHERE s.device_id=d.device_id AND s.draft_id=d.draft_id)",
            params![device_id, task_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Roll back a preparation only when the registry still contains the task.
    /// The caller must establish that the irreversible delete did not commit.
    pub fn abort_task_cleanup(&self, task_id: &str) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM task_cleanup_paths WHERE task_id=?1",
            params![task_id],
        )?;
        tx.execute(
            "DELETE FROM task_cleanup WHERE task_id=?1 AND state IN ('pending','failed')",
            params![task_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Retire only records keyed to this LUNA task. The command receipt and
    /// identity tombstone remain as durable replay guards.
    /// Side chat ownership is metadata only. Startup consumers use this
    /// before opening transcript documents or probing native sessions.
    pub fn is_side_chat(&self, task_id: &str) -> Result<bool, StoreError> {
        let conn = self.conn();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='side_chats')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(false);
        }
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM side_chats WHERE id=?1)",
            [task_id],
            |row| row.get(0),
        )?)
    }

    pub fn retire_task_local_records(
        &self,
        device_id: &str,
        task_id: &str,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM attachment_owners WHERE device_id=?1 AND owner_kind='draft'
             AND owner_id IN (SELECT draft_id FROM composer_drafts WHERE device_id=?1 AND task_id=?2)",
            params![device_id, task_id],
        )?;
        tx.execute(
            "DELETE FROM attachment_owners WHERE device_id=?1 AND owner_kind='submission'
             AND owner_id IN (SELECT command_id FROM composer_draft_submissions WHERE device_id=?1 AND task_id=?2)",
            params![device_id, task_id],
        )?;
        for table in ["composer_drafts", "composer_draft_submissions"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE device_id=?1 AND task_id=?2"),
                params![device_id, task_id],
            )?;
        }
        tx.execute(
            "DELETE FROM appshot_activity WHERE task_id=?1",
            params![task_id],
        )?;
        tx.execute(
            "DELETE FROM activity_events WHERE task_id=?1",
            params![task_id],
        )?;
        tx.execute(
            "DELETE FROM task_recovery_state WHERE task_id=?1",
            params![task_id],
        )?;
        tx.execute(
            "DELETE FROM task_execution_lineage WHERE task_id=?1",
            params![task_id],
        )?;
        tx.execute("DELETE FROM snapshots WHERE doc_id=?1", params![task_id])?;
        tx.execute("DELETE FROM chat_outbox WHERE doc_id=?1", params![task_id])?;
        tx.execute(
            "DELETE FROM chat_outbox_initialized WHERE doc_id=?1",
            params![task_id],
        )?;
        // Setup state belongs to the checkout generation and remains as a
        // replay guard until project deletion; ordinary action runs are task-owned.
        tx.execute("DELETE FROM project_command_runs WHERE device_id=?1 AND json_extract(receipt,'$.chatId')=?2 AND json_extract(receipt,'$.setup')=0", params![device_id, task_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn task_cleanup_paths(&self, task_id: &str) -> Result<Vec<String>, StoreError> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT blob_path FROM task_cleanup_paths WHERE task_id=?1 ORDER BY blob_path",
        )?;
        Ok(statement
            .query_map(params![task_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn attachment_has_owner(&self, path: &str) -> Result<bool, StoreError> {
        Ok(self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM attachment_owners WHERE blob_path=?1)",
            params![path],
            |row| row.get::<_, bool>(0),
        )?)
    }

    /// Hold the SQLite writer reservation from owner check through unlink so
    /// a concurrent draft cannot adopt a blob in the gap between those steps.
    /// Callers must validate that `path` is a regular file in their upload jail.
    pub fn remove_unowned_attachment(&self, path: &str) -> Result<bool, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let owned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM attachment_owners WHERE blob_path=?1)",
            params![path],
            |row| row.get(0),
        )?;
        if owned {
            return Ok(false);
        }
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn set_task_cleanup_state(
        &self,
        task_id: &str,
        state: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE task_cleanup SET state=?2,detail=?3,updated_at=?4 WHERE task_id=?1",
            params![task_id, state, detail, now_ms()],
        )?;
        if state == "completed" || state == "provider-pending" {
            tx.execute(
                "DELETE FROM task_cleanup_paths WHERE task_id=?1",
                params![task_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn pending_task_cleanups(&self, limit: usize) -> Result<Vec<String>, StoreError> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT task_id FROM task_cleanup WHERE state IN ('pending','failed')
             ORDER BY updated_at,task_id LIMIT ?1",
        )?;
        Ok(statement
            .query_map(params![limit as i64], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Keyset page so a permanently blocked first row cannot starve later
    /// cleanup work. The caller wraps to the empty cursor after the last page.
    pub fn pending_task_cleanups_after(
        &self,
        after: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, String)>, StoreError> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT task_id,state,owner_boot_id FROM task_cleanup
             WHERE state IN ('pending','failed') AND task_id>?1
             ORDER BY task_id LIMIT ?2",
        )?;
        Ok(statement
            .query_map(params![after, limit as i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn task_cleanup_state(
        &self,
        task_id: &str,
    ) -> Result<Option<(String, String)>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT state,detail FROM task_cleanup WHERE task_id=?1",
                params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// A committed delete receipt is sufficient replay evidence for tasks
    /// created before the identity service registered every local chat.
    pub fn has_applied_task_delete(&self, task_id: &str) -> Result<bool, StoreError> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT receipt_json FROM task_command_receipts WHERE task_id=?1 ORDER BY updated_at DESC LIMIT 32",
        )?;
        for bytes in statement.query_map(params![task_id], |row| row.get::<_, Vec<u8>>(0))? {
            let receipt: CommandReceipt = serde_json::from_slice(&bytes?)?;
            if receipt.intent == "delete" && receipt.status == CommandReceiptStatus::Applied {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn prepare_project_cleanup(
        &self,
        project_id: &str,
        children: &[String],
        owner_boot_id: &str,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO project_cleanup(project_id,child_ids_json,state,detail,updated_at,owner_boot_id)
             VALUES(?1,?2,'pending','Project cleanup is pending',?3,?4)
             ON CONFLICT(project_id) DO UPDATE SET child_ids_json=excluded.child_ids_json,
               state='pending',detail=excluded.detail,updated_at=excluded.updated_at,
               owner_boot_id=excluded.owner_boot_id",
            params![project_id, serde_json::to_vec(children)?, now_ms(), owner_boot_id],
        )?;
        Ok(())
    }

    pub fn abort_project_cleanup(&self, project_id: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "DELETE FROM project_cleanup WHERE project_id=?1 AND state IN ('pending','failed')",
            params![project_id],
        )?;
        Ok(())
    }

    pub fn project_cleanup_state(
        &self,
        project_id: &str,
    ) -> Result<Option<(String, String)>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT state,detail FROM project_cleanup WHERE project_id=?1",
                params![project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub fn set_project_cleanup_state(
        &self,
        project_id: &str,
        state: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE project_cleanup SET state=?2,detail=?3,updated_at=?4 WHERE project_id=?1",
            params![project_id, state, detail, now_ms()],
        )?;
        Ok(())
    }

    pub fn pending_project_cleanups(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, Vec<String>)>, StoreError> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT project_id,child_ids_json FROM project_cleanup
             WHERE state IN ('pending','failed') ORDER BY updated_at,project_id LIMIT ?1",
        )?;
        let rows = statement
            .query_map(params![limit as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(project, bytes)| Ok((project, serde_json::from_slice(&bytes)?)))
            .collect()
    }

    pub fn pending_project_cleanups_after(
        &self,
        after: &str,
        limit: usize,
    ) -> Result<Vec<(String, Vec<String>, String, String)>, StoreError> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT project_id,child_ids_json,state,owner_boot_id FROM project_cleanup
             WHERE state IN ('pending','failed') AND project_id>?1
             ORDER BY project_id LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![after, limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, bytes, state, boot)| Ok((id, serde_json::from_slice(&bytes)?, state, boot)))
            .collect()
    }

    /// Bounded summary for the existing sidebar notice. The detail is an
    /// already-persisted local failure string, never transcript/provider data.
    pub fn deletion_cleanup_summary(&self) -> Result<(usize, usize, Option<String>), StoreError> {
        let conn = self.conn();
        let mut pending = 0usize;
        let mut failed = 0usize;
        for table in ["task_cleanup", "project_cleanup"] {
            pending += conn.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE state='pending' AND updated_at<?1"),
                params![now_ms() - 5_000],
                |row| row.get::<_, i64>(0),
            )? as usize;
            failed += conn.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE state='failed'"),
                [],
                |row| row.get::<_, i64>(0),
            )? as usize;
        }
        let first = conn
            .query_row(
                "SELECT detail FROM (
               SELECT detail,updated_at FROM task_cleanup WHERE state='failed'
               UNION ALL
               SELECT detail,updated_at FROM project_cleanup WHERE state='failed'
             ) ORDER BY updated_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok((pending, failed, first))
    }

    pub fn retire_project_local_records(&self, project_id: &str) -> Result<(), StoreError> {
        let _activity_guard = self.activity_index_guard();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM activity_project_policies WHERE project_id=?1",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM activity_events WHERE project_id=?1",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM project_command_configs WHERE project_id=?1",
            [project_id],
        )?;
        tx.execute(
            "DELETE FROM project_command_runs WHERE project_id=?1",
            [project_id],
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
            "INSERT INTO task_command_receipts(command_id,task_id,payload_hash,receipt_json,updated_at,workspace_repair_revision)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                receipt.command_id,
                receipt.task_id,
                receipt.payload_hash,
                bytes,
                now_ms(),
                workspace_repair::workspace_repair_revision_in(&tx, &receipt.task_id)?
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
            &[],
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
            &[],
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
            &[],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_task_command_with_snapshot_changes_updates_and_authority(
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
        authority: &[LocalExecutableChange],
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
            authority,
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
            &[],
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
        self.complete_task_command_with_snapshot_changes_updates_recovery_and_authority(
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
            updates,
            recovery,
            &[],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_task_command_with_snapshot_changes_updates_recovery_and_authority(
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
        authority: &[LocalExecutableChange],
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
            authority,
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
        authority: &[LocalExecutableChange],
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
            if !authority.is_empty() {
                return Err(StoreError::LocalExecutableAuthorityConflict);
            }
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
        Self::apply_local_executable_changes_tx(&tx, authority)?;
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

    fn apply_local_executable_changes_tx(
        tx: &rusqlite::Transaction<'_>,
        changes: &[LocalExecutableChange],
    ) -> Result<(), StoreError> {
        Self::apply_local_executable_changes_with_consent_tx(tx, changes, None)
    }

    fn apply_local_executable_changes_with_consent_tx(
        tx: &rusqlite::Transaction<'_>,
        changes: &[LocalExecutableChange],
        consent_command_id: Option<&str>,
    ) -> Result<(), StoreError> {
        for change in changes {
            let current: Option<(String, u64)> = tx
                .query_row(
                    "SELECT payload_hash,workspace_repair_revision FROM local_executable_authority
                     WHERE doc_id=?1 AND kind=?2 AND row_id=?3",
                    params![change.doc_id, change.kind.as_str(), change.row_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if current.as_ref().map(|(hash, _)| hash) != change.expected_hash.as_ref()
                || current.as_ref().map(|(_, revision)| *revision)
                    != change.expected_workspace_repair_revision
            {
                return Err(StoreError::LocalExecutableAuthorityConflict);
            }
            match &change.replacement_hash {
                Some(hash) => {
                    let revision = change
                        .replacement_workspace_repair_revision
                        .ok_or(StoreError::LocalExecutableAuthorityConflict)?;
                    if revision
                        != workspace_repair::workspace_repair_revision_in(tx, &change.doc_id)?
                    {
                        return Err(StoreError::WorkspaceRepairConflict(
                            "the executable belongs to an earlier workspace association".into(),
                        ));
                    }
                    if change
                        .expected_workspace_repair_revision
                        .is_some_and(|old| old != revision)
                    {
                        if change.kind != LocalExecutableKind::Queue {
                            return Err(StoreError::LocalExecutableAuthorityConflict);
                        }
                        let command_id = consent_command_id
                            .ok_or(StoreError::LocalExecutableAuthorityConflict)?;
                        let bytes: Vec<u8> = tx.query_row(
                            "SELECT receipt_json FROM task_command_receipts WHERE command_id=?1",
                            [command_id],
                            |row| row.get(0),
                        )?;
                        let consent: CommandReceipt = serde_json::from_slice(&bytes)?;
                        if consent.task_id != change.doc_id
                            || consent.status != CommandReceiptStatus::Pending
                            || !matches!(
                                consent.intent.as_str(),
                                "sendQueuedNow" | "steerQueuedNow"
                            )
                            || consent.correlation_id.as_deref() != Some(change.row_id.as_str())
                            || workspace_repair::command_workspace_revision_in(tx, command_id)?
                                != Some(revision)
                        {
                            return Err(StoreError::LocalExecutableAuthorityConflict);
                        }
                    }
                    tx.execute(
                        "INSERT INTO local_executable_authority(doc_id,kind,row_id,payload_hash,workspace_repair_revision)
                         VALUES (?1,?2,?3,?4,?5)
                         ON CONFLICT(doc_id,kind,row_id) DO UPDATE SET payload_hash=excluded.payload_hash,workspace_repair_revision=excluded.workspace_repair_revision",
                        params![change.doc_id, change.kind.as_str(), change.row_id, hash, revision],
                    )?;
                }
                None => {
                    if change.replacement_workspace_repair_revision.is_some() {
                        return Err(StoreError::LocalExecutableAuthorityConflict);
                    }
                    tx.execute(
                        "DELETE FROM local_executable_authority
                         WHERE doc_id=?1 AND kind=?2 AND row_id=?3",
                        params![change.doc_id, change.kind.as_str(), change.row_id],
                    )?;
                }
            }
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
        self.mark_task_command_unknown_with_snapshot_changes_and_authority(
            command_id,
            message,
            snapshots,
            updates,
            &[],
        )
    }

    pub fn mark_task_command_unknown_with_snapshot_changes_and_authority(
        &self,
        command_id: &str,
        message: &str,
        snapshots: &[(&str, &[u8])],
        updates: &[(&str, &str, &[u8])],
        authority: &[LocalExecutableChange],
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
        if receipt.status.is_terminal() && !authority.is_empty() {
            return Err(StoreError::LocalExecutableAuthorityConflict);
        }
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
        Self::apply_local_executable_changes_tx(&tx, authority)?;
        tx.commit()?;
        Ok(Some(receipt))
    }

    pub fn set_task_command_correlation(
        &self,
        command_id: &str,
        correlation_id: &str,
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.set_task_command_correlation_inner(command_id, correlation_id, &[], &[], None, &[])
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
            &[],
        )
    }

    pub fn set_task_command_correlation_with_snapshot_changes_and_authority(
        &self,
        command_id: &str,
        correlation_id: &str,
        snapshots: &[(&str, &[u8])],
        updates: &[(&str, &str, &[u8])],
        authority: &[LocalExecutableChange],
    ) -> Result<Option<CommandReceipt>, StoreError> {
        self.set_task_command_correlation_inner(
            command_id,
            correlation_id,
            snapshots,
            updates,
            None,
            authority,
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
            &[],
        )
    }

    fn set_task_command_correlation_inner(
        &self,
        command_id: &str,
        correlation_id: &str,
        snapshots: &[(&str, &[u8])],
        updates: &[(&str, &str, &[u8])],
        recovery: Option<&TaskRecoveryTransition>,
        authority: &[LocalExecutableChange],
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
        if receipt.status.is_terminal() && !authority.is_empty() {
            return Err(StoreError::LocalExecutableAuthorityConflict);
        }
        if !receipt.status.is_terminal() {
            if !authority.is_empty()
                && workspace_repair::command_workspace_revision_in(&tx, command_id)?
                    != Some(workspace_repair::workspace_repair_revision_in(
                        &tx,
                        &receipt.task_id,
                    )?)
            {
                return Err(StoreError::WorkspaceRepairConflict(
                    "the command was admitted before workspace repair".into(),
                ));
            }
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
        Self::apply_local_executable_changes_with_consent_tx(&tx, authority, Some(command_id))?;
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
        self.confirm_task_command_correlation_with_snapshot_changes_and_authority(
            task_id,
            correlation_id,
            active_turn_id,
            binding_id,
            binding_generation,
            snapshots,
            updates,
            &[],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn confirm_task_command_correlation_with_snapshot_changes_and_authority(
        &self,
        task_id: &str,
        correlation_id: &str,
        active_turn_id: &str,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        snapshots: &[(&str, &[u8])],
        updates: &[(&str, &str, &[u8])],
        authority: &[LocalExecutableChange],
    ) -> Result<Vec<CommandReceipt>, StoreError> {
        let mut conn = self.conn();
        // Reserve write authority before reading pending receipts. A deferred
        // read-to-write upgrade can return BUSY immediately under WAL contention,
        // bypassing the busy timeout and leaving an executed queue row claimed.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
                || workspace_repair::command_workspace_revision_in(&tx, &receipt.command_id)?
                    != Some(workspace_repair::workspace_repair_revision_in(
                        &tx, task_id,
                    )?)
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
            Self::apply_local_executable_changes_tx(&tx, authority)?;
        }
        tx.commit()?;
        Ok(confirmed)
    }

    /// Terminalize a Run whose adapter PROVED the provider never accepted the
    /// user turn (`RejectedBeforeAcceptance` before `SessionStarted`).
    ///
    /// Only an exact `run` receipt for this task/correlation that is still
    /// Pending and uncommitted is rejected, and only while the command's
    /// admission workspace stamp, the task's current stamp and the caller's
    /// expected stamp agree and any admission binding generation is still
    /// current. Committed-unknown, accepted, terminal, other-intent and
    /// fenced-out rows are left untouched, so a race with provider acceptance
    /// keeps whichever outcome committed first and a repeat is a no-op.
    ///
    /// The same transaction marks this device's frozen draft submission for
    /// the rejected command recoverable. Its snapshot (text, typed selections,
    /// attachments) and blob owners are retained. The task's binding and
    /// active turn are not changed.
    #[allow(clippy::too_many_arguments)]
    pub fn reject_run_before_acceptance(
        &self,
        device_id: &str,
        task_id: &str,
        correlation_id: &str,
        expected_workspace_repair_revision: u64,
        current_binding_generation: Option<u32>,
        message: &str,
        recovery_action: &str,
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
        let task_workspace_revision = workspace_repair::workspace_repair_revision_in(&tx, task_id)?;
        let mut rejected = Vec::new();
        for bytes in rows {
            let mut receipt: CommandReceipt = serde_json::from_slice(&bytes)?;
            if receipt.task_id != task_id
                || receipt.intent != "run"
                || receipt.status != CommandReceiptStatus::Pending
                || receipt.committed_at_ms.is_some()
                || receipt.correlation_id.as_deref() != Some(correlation_id)
                || task_workspace_revision != expected_workspace_repair_revision
                || workspace_repair::command_workspace_revision_in(&tx, &receipt.command_id)?
                    != Some(expected_workspace_repair_revision)
                || receipt
                    .snapshot
                    .binding_generation
                    .is_some_and(|expected| current_binding_generation != Some(expected))
            {
                continue;
            }
            // Keep the task's authoritative binding/turn: this outcome proves
            // no provider effect, so it must not move the task projection.
            let current = load_task_command_snapshot(&tx, task_id)?;
            receipt.status = CommandReceiptStatus::Rejected;
            receipt.snapshot.binding_id = current.binding_id;
            receipt.snapshot.binding_generation = current.binding_generation;
            receipt.snapshot.active_turn_id = current.active_turn_id;
            receipt.reason = Some(CommandReceiptReason {
                code: CommandReasonCode::OperationRejected,
                message: message.into(),
            });
            receipt.recovery_action = Some(recovery_action.into());
            receipt.committed_at_ms = Some(now_ms());
            receipt.observed_at_ms = now_ms();
            let receipt = commit_task_command_tx(&tx, receipt, false)?;
            tx.execute(
                "UPDATE composer_draft_submissions SET state='recoverable',updated_at=?3
                 WHERE device_id=?1 AND command_id=?2 AND state='pending'",
                params![device_id, receipt.command_id, now_ms()],
            )?;
            rejected.push(receipt);
        }
        tx.commit()?;
        Ok(rejected)
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

    fn require_uncoordinated_snapshot_save(&self) -> Result<(), StoreError> {
        if self.write_epoch.is_some() {
            Err(StoreError::AdmissionRequired)
        } else {
            Ok(())
        }
    }

    fn require_writer(&self, permit: &WritePermit) -> Result<(), StoreError> {
        match &self.write_epoch {
            Some(epoch) if epoch.permits_write(permit) => Ok(()),
            _ => Err(StoreError::AdmissionMismatch),
        }
    }

    fn require_flush(&self, permit: &FlushPermit<'_>) -> Result<(), StoreError> {
        match &self.write_epoch {
            Some(epoch) if epoch.permits_flush(permit) => Ok(()),
            _ => Err(StoreError::AdmissionMismatch),
        }
    }

    // Call only after the public save_snapshot entry point has checked its
    // writer/flush permit, or its unguarded legacy mode. No other mutator may
    // use the guarded store's writable connection.
    fn snapshot_write_conn(&self) -> MutexGuard<'_, Connection> {
        match &self.snapshot_writer {
            Some(writer) => writer.lock().unwrap_or_else(PoisonError::into_inner),
            None => self.conn(),
        }
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
    let original_w = workspace_repair::recovery_workspace_revision(&recovery.record_json)?;
    if matches!(recovery.status.as_str(), "running" | "scheduled")
        && original_w != workspace_repair::workspace_repair_revision_in(tx, &recovery.task_id)?
    {
        return Err(StoreError::WorkspaceRepairConflict(
            "recovery belongs to an earlier workspace association".into(),
        ));
    }
    let prior: Option<(String, Vec<u8>)> = tx
        .query_row(
            "SELECT chain_id,record_json FROM task_recovery_state WHERE task_id=?1",
            [&recovery.task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((chain, bytes)) = prior
        && chain == recovery.chain_id
        && workspace_repair::recovery_workspace_revision(&bytes)? != original_w
    {
        return Err(StoreError::WorkspaceRepairConflict(
            "the original recovery workspace revision is immutable".into(),
        ));
    }
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

fn ensure_task_not_cleaning(
    tx: &rusqlite::Transaction<'_>,
    task_id: &str,
) -> Result<(), StoreError> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_cleanup WHERE task_id=?1)",
        params![task_id],
        |row| row.get(0),
    )?;
    if exists {
        Err(StoreError::TaskCleanupPending(task_id.into()))
    } else {
        Ok(())
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

fn enum_key<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    Ok(serde_json::to_string(value)?)
}

fn attention_revision(conn: &Connection) -> Result<u64, StoreError> {
    Ok(conn.query_row(
        "SELECT revision FROM attention_meta WHERE singleton=1",
        [],
        |row| row.get(0),
    )?)
}

fn bump_attention_revision(tx: &rusqlite::Transaction<'_>, now: i64) -> Result<u64, StoreError> {
    tx.execute(
        "UPDATE attention_meta SET revision=revision+1,updated_at=?1 WHERE singleton=1",
        params![now],
    )?;
    attention_revision(tx)
}

fn read_attention_item(
    conn: &Connection,
    item_id: &str,
) -> Result<Option<AttentionItem>, StoreError> {
    let bytes: Option<Vec<u8>> = conn
        .query_row(
            "SELECT item_json FROM attention_items WHERE item_id=?1",
            params![item_id],
            |row| row.get(0),
        )
        .optional()?;
    bytes
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()
        .map_err(StoreError::from)
}

fn read_attention_items<P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
) -> Result<Vec<AttentionItem>, StoreError> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params, |row| row.get::<_, Vec<u8>>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|bytes| serde_json::from_slice(&bytes).map_err(StoreError::from))
        .collect()
}

fn update_attention_item(
    tx: &rusqlite::Transaction<'_>,
    item: &AttentionItem,
    dismissed: bool,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE attention_items SET
           source_key=?2,kind=?3,lifecycle=?4,actionable=?5,updated_at=?6,
           dismissed=?7,item_json=?8 WHERE item_id=?1",
        params![
            item.id,
            item.source_key,
            enum_key(&item.kind)?,
            enum_key(&item.lifecycle)?,
            item.actionable(),
            item.updated_at_ms,
            dismissed,
            serde_json::to_vec(item)?,
        ],
    )?;
    Ok(())
}

fn insert_attention_transition(
    tx: &rusqlite::Transaction<'_>,
    transition: AttentionTransitionKind,
    item: &AttentionItem,
) -> Result<(), StoreError> {
    let placeholder = AttentionTransition {
        sequence: 0,
        transition,
        item: item.clone(),
    };
    tx.execute(
        "INSERT INTO attention_transitions(item_id,transition_json) VALUES(?1,?2)",
        params![item.id, serde_json::to_vec(&placeholder)?],
    )?;
    let sequence = tx.last_insert_rowid() as u64;
    let committed = AttentionTransition {
        sequence,
        transition,
        item: item.clone(),
    };
    tx.execute(
        "UPDATE attention_transitions SET transition_json=?1 WHERE sequence=?2",
        params![serde_json::to_vec(&committed)?, sequence],
    )?;
    Ok(())
}

fn prune_claimed_attention_transitions(tx: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    // Unclaimed transitions remain deliverable; only old delivered/baselined
    // receipts are bounded. The item table remains the durable source of UI
    // state and has its own Recent presentation bound.
    tx.execute(
        "DELETE FROM attention_transitions WHERE sequence IN (
            SELECT sequence FROM attention_transitions
            WHERE claimed_by IS NOT NULL
            ORDER BY sequence DESC LIMIT -1 OFFSET 1000
        )",
        [],
    )?;
    Ok(())
}

fn insert_appshot_activity_tx(
    tx: &rusqlite::Transaction<'_>,
    event: &AppshotActivityRecord,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT OR IGNORE INTO appshot_activity(event_id,draft_id,task_id,capture_id,target_environment_id,record_json)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            event.event_id,
            event.draft_id,
            event.task_id,
            event.capture_id,
            event.target_environment_id,
            serde_json::to_vec(event)?,
        ],
    )?;
    Ok(())
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

fn ensure_known_schema(conn: &Connection) -> Result<(), StoreError> {
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')", [], |row| row.get(0))?;
    if exists {
        let found: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        let supported = MIGRATIONS.len() as i64;
        if found > supported {
            return Err(StoreError::NewerSchema { found, supported });
        }
    }
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    ensure_known_schema(conn)?;
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
    use crate::write_epoch::Phase;
    use luna_proto::{
        ACTIVITY_ENVELOPE_VERSION, ATTENTION_CONTRACT_VERSION, ActivityKind, ActivityRedaction,
        ActivitySourceRef, AttentionItem, AttentionKind, AttentionLifecycle, AttentionMutation,
        AttentionSeverity, AttentionSourcePointer, AttentionTarget, HistorySearchFilters,
    };
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;
    use std::time::Duration;

    fn is_sqlite_read_only<T>(result: Result<T, StoreError>) -> bool {
        matches!(
            result,
            Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(error, _)))
                if error.code == rusqlite::ErrorCode::ReadOnly
        )
    }

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
            outcome: luna_proto::ActivityOutcome::Succeeded,
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

    fn attention(id: &str, kind: AttentionKind, lifecycle: AttentionLifecycle) -> AttentionItem {
        AttentionItem {
            contract_version: ATTENTION_CONTRACT_VERSION,
            id: id.into(),
            dedup_key: format!("dedup:{id}"),
            source_key: format!("source:{id}"),
            kind,
            severity: AttentionSeverity::Warning,
            lifecycle,
            environment_id: "env".into(),
            device_id: "device".into(),
            task_id: Some("task".into()),
            turn_id: Some("turn".into()),
            request_id: Some(id.into()),
            title: "Attention needed".into(),
            summary: "A task is waiting for a safe action.".into(),
            created_at_ms: 10,
            updated_at_ms: 10,
            seen_at_ms: None,
            acknowledged_at_ms: None,
            resolved_at_ms: None,
            target: AttentionTarget::Task {
                task_id: "task".into(),
            },
            source: AttentionSourcePointer {
                task_id: Some("task".into()),
                message_id: Some("message".into()),
                part_id: None,
                request_id: Some(id.into()),
                queue_id: None,
                recovery_id: None,
                activity_event_id: None,
            },
        }
    }

    #[test]
    fn attention_reconcile_persists_presentation_without_resolving_on_seen_or_ack() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let item = attention(
            "approval-1",
            AttentionKind::ApprovalRequired,
            AttentionLifecycle::Unresolved,
        );
        let revision = store
            .reconcile_attention_items(&[item.clone()], &[AttentionKind::ApprovalRequired])
            .unwrap();
        assert_eq!(revision, 1);
        assert_eq!(store.attention_snapshot().unwrap().unresolved_count, 1);

        let seen = store
            .mutate_attention(
                "seen",
                revision,
                &AttentionMutation::MarkSeen {
                    item_ids: vec![item.id.clone()],
                },
            )
            .unwrap();
        let ack = store
            .mutate_attention(
                "ack",
                seen.revision,
                &AttentionMutation::Acknowledge {
                    item_id: item.id.clone(),
                },
            )
            .unwrap();
        let snapshot = store.attention_snapshot().unwrap();
        assert_eq!(snapshot.unresolved_count, 1);
        assert_eq!(
            snapshot.needs_attention[0].lifecycle,
            AttentionLifecycle::Acknowledged
        );
        assert!(snapshot.needs_attention[0].seen_at_ms.is_some());

        let duplicate = store
            .mutate_attention(
                "ack",
                revision,
                &AttentionMutation::Acknowledge {
                    item_id: item.id.clone(),
                },
            )
            .unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.revision, ack.revision);
        assert!(matches!(
            store.mutate_attention("stale", revision, &AttentionMutation::ClearRecent,),
            Err(StoreError::AttentionRevisionConflict)
        ));
        assert!(matches!(
            store.mutate_attention("ack", ack.revision, &AttentionMutation::ClearRecent,),
            Err(StoreError::AttentionCommandConflict)
        ));

        store
            .reconcile_attention_items(&[], &[AttentionKind::ApprovalRequired])
            .unwrap();
        let resolved = store.attention_snapshot().unwrap();
        assert_eq!(resolved.unresolved_count, 0);
        assert_eq!(resolved.recent.len(), 1);
        assert_eq!(resolved.recent[0].lifecycle, AttentionLifecycle::Resolved);
        assert!(resolved.recent[0].acknowledged_at_ms.is_some());
        assert!(resolved.recent[0].seen_at_ms.is_some());

        let baseline = store.claim_attention_transitions("ui", false).unwrap();
        assert!(baseline.is_empty());
    }

    #[test]
    fn attention_heartbeat_is_stable_and_recovery_kind_change_reopens_notice() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let mut completed = attention(
            "completion",
            AttentionKind::TaskCompleted,
            AttentionLifecycle::Resolved,
        );
        completed.resolved_at_ms = Some(10);
        let first = store
            .reconcile_attention_items(&[completed.clone()], &[AttentionKind::TaskCompleted])
            .unwrap();
        completed.updated_at_ms = 20;
        completed.resolved_at_ms = Some(20);
        assert_eq!(
            store
                .reconcile_attention_items(&[completed], &[AttentionKind::TaskCompleted])
                .unwrap(),
            first
        );
        assert_eq!(
            store.attention_snapshot().unwrap().recent[0].resolved_at_ms,
            Some(10)
        );

        let sign_in = attention(
            "recovery-chain",
            AttentionKind::ProviderAuthenticationRequired,
            AttentionLifecycle::Unresolved,
        );
        let revision = store
            .reconcile_attention_items(
                &[sign_in.clone()],
                &[
                    AttentionKind::ProviderAuthenticationRequired,
                    AttentionKind::TaskFailed,
                ],
            )
            .unwrap();
        store.claim_attention_transitions("ui", false).unwrap();
        store
            .mutate_attention(
                "ack-sign-in",
                revision,
                &AttentionMutation::Acknowledge {
                    item_id: sign_in.id.clone(),
                },
            )
            .unwrap();
        let mut failed = sign_in;
        failed.kind = AttentionKind::TaskFailed;
        failed.source_key.push_str(":failure");
        store
            .reconcile_attention_items(
                &[failed],
                &[
                    AttentionKind::ProviderAuthenticationRequired,
                    AttentionKind::TaskFailed,
                ],
            )
            .unwrap();
        let needs = store.attention_snapshot().unwrap();
        assert_eq!(needs.unresolved_count, 1);
        assert_eq!(
            needs.needs_attention[0].lifecycle,
            AttentionLifecycle::Unresolved
        );
        assert_eq!(needs.needs_attention[0].acknowledged_at_ms, None);
        let transitions = store.claim_attention_transitions("ui", true).unwrap();
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].item.kind, AttentionKind::TaskFailed);
    }

    #[test]
    fn attention_recent_is_bounded_without_evicting_unresolved_work() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let mut desired = (0..205)
            .map(|index| {
                let mut item = attention(
                    &format!("completed-{index}"),
                    AttentionKind::TaskCompleted,
                    AttentionLifecycle::Resolved,
                );
                item.updated_at_ms = 100 + index;
                item.resolved_at_ms = Some(100 + index);
                item
            })
            .collect::<Vec<_>>();
        let approval = attention(
            "approval-never-evicted",
            AttentionKind::ApprovalRequired,
            AttentionLifecycle::Unresolved,
        );
        desired.push(approval.clone());
        store
            .reconcile_attention_items(
                &desired,
                &[
                    AttentionKind::TaskCompleted,
                    AttentionKind::ApprovalRequired,
                ],
            )
            .unwrap();
        let snapshot = store.attention_snapshot().unwrap();
        assert_eq!(snapshot.recent.len(), 200);
        assert_eq!(snapshot.unresolved_count, 1);
        assert_eq!(snapshot.needs_attention[0].id, approval.id);
        assert!(
            store
                .mutate_attention(
                    "reject-unresolved-dismiss",
                    snapshot.revision,
                    &AttentionMutation::DismissRecent {
                        item_id: approval.id
                    }
                )
                .is_err()
        );
        assert_eq!(store.attention_snapshot().unwrap().unresolved_count, 1);
    }

    #[test]
    fn repeat_outage_reopens_without_inheriting_old_acknowledgment() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let item = attention(
            "connectivity",
            AttentionKind::ConnectivityUnavailable,
            AttentionLifecycle::Unresolved,
        );
        let revision = store
            .reconcile_attention_items(&[item.clone()], &[AttentionKind::ConnectivityUnavailable])
            .unwrap();
        let ack = store
            .mutate_attention(
                "ack-old-outage",
                revision,
                &AttentionMutation::Acknowledge {
                    item_id: item.id.clone(),
                },
            )
            .unwrap();
        assert!(ack.revision > revision);
        store
            .reconcile_attention_items(&[], &[AttentionKind::ConnectivityUnavailable])
            .unwrap();
        store
            .reconcile_attention_items(&[item], &[AttentionKind::ConnectivityUnavailable])
            .unwrap();
        let reopened = store.attention_snapshot().unwrap();
        assert_eq!(reopened.unresolved_count, 1);
        assert_eq!(
            reopened.needs_attention[0].lifecycle,
            AttentionLifecycle::Unresolved
        );
        assert_eq!(reopened.needs_attention[0].acknowledged_at_ms, None);
        assert_eq!(reopened.needs_attention[0].seen_at_ms, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guarded_snapshot_saves_require_the_matching_epoch() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("profile");
        std::fs::create_dir(&data_dir).unwrap();
        let coordinator = WriteEpoch::for_device_root(root.path()).unwrap();
        let first = coordinator.try_admit().unwrap();

        let wrong_root = tempfile::tempdir().unwrap();
        let other = WriteEpoch::for_device_root(wrong_root.path()).unwrap();
        let wrong_permit = other.try_admit().unwrap();
        let other_same_root = WriteEpoch::for_device_root(root.path()).unwrap();
        let wrong_coordinator_permit = other_same_root.try_admit().unwrap();
        assert!(matches!(
            DocsStore::open_guarded(&data_dir, coordinator.clone(), &wrong_coordinator_permit),
            Err(StoreError::AdmissionMismatch)
        ));
        assert!(matches!(
            DocsStore::open_guarded(&data_dir, coordinator.clone(), &wrong_permit),
            Err(StoreError::AdmissionMismatch)
        ));
        assert!(matches!(
            DocsStore::open_guarded(wrong_root.path(), coordinator.clone(), &first),
            Err(StoreError::AdmissionMismatch)
        ));
        assert!(!data_dir.join("docs.sqlite3").exists());
        assert!(!wrong_root.path().join("docs.sqlite3").exists());

        let store =
            Arc::new(DocsStore::open_guarded(&data_dir, coordinator.clone(), &first).unwrap());
        store
            .save_snapshot_with_updates_admitted(&first, "doc", b"base", &[])
            .unwrap();
        let freezing = {
            let coordinator = coordinator.clone();
            tokio::spawn(async move { coordinator.freeze(Duration::from_secs(2)).await })
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while coordinator.phase() != Phase::Quiescing {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        assert!(matches!(
            store.save_snapshot("doc", b"bypass"),
            Err(StoreError::AdmissionRequired)
        ));
        assert!(matches!(
            store.save_snapshot_with_updates("doc", b"bypass", &[]),
            Err(StoreError::AdmissionRequired)
        ));
        assert!(matches!(
            store.save_snapshot_with_cursor("doc", b"bypass", 1, 1),
            Err(StoreError::AdmissionRequired)
        ));
        assert!(matches!(
            store.save_snapshot_admitted(&wrong_permit, "doc", b"wrong-root"),
            Err(StoreError::AdmissionMismatch)
        ));
        assert!(matches!(
            store.save_snapshot_admitted(&wrong_coordinator_permit, "doc", b"wrong-coordinator"),
            Err(StoreError::AdmissionMismatch)
        ));
        assert_eq!(store.load_snapshot("doc").unwrap().unwrap(), b"base");

        // An operation admitted before the drain may finish while Quiescing.
        store
            .save_snapshot_with_cursor_admitted(&first, "doc", b"first", 2, 2)
            .unwrap();
        drop(first);
        let flush = freezing.await.unwrap().unwrap();
        assert_eq!(coordinator.phase(), Phase::Flushing);
        assert_eq!(
            coordinator.try_admit().err(),
            Some(crate::write_epoch::EpochError::Busy)
        );
        let flush_permit = flush.permit();
        store
            .save_snapshot_flushing(&flush_permit, "doc", b"flushed")
            .unwrap();
        let snapshot = flush.into_snapshot().unwrap();
        assert_eq!(store.load_snapshot("doc").unwrap().unwrap(), b"flushed");
        assert!(matches!(
            store.save_snapshot("doc", b"after-boundary"),
            Err(StoreError::AdmissionRequired)
        ));
        drop(snapshot);

        let second = coordinator.try_admit().unwrap();
        assert_eq!(second.epoch(), 2);
        store
            .save_snapshot_admitted(&second, "doc", b"second")
            .unwrap();
        assert_eq!(store.load_snapshot("doc").unwrap().unwrap(), b"second");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guarded_default_connection_rejects_legacy_mutations_across_freeze() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("profile");
        std::fs::create_dir(&data_dir).unwrap();
        let initial = DocsStore::open(&data_dir).unwrap();
        initial.save_snapshot("doc", b"seed").unwrap();
        initial
            .replace_activity_events("task", &[activity("event", "task", "project", 1)])
            .unwrap();
        drop(initial);

        let coordinator = WriteEpoch::for_device_root(root.path()).unwrap();
        let first = coordinator.try_admit().unwrap();
        let store =
            Arc::new(DocsStore::open_guarded(&data_dir, coordinator.clone(), &first).unwrap());
        let peer = DocsStore::open_guarded(&data_dir, coordinator.clone(), &first).unwrap();
        assert_eq!(peer.load_snapshot("doc").unwrap().unwrap(), b"seed");
        assert!(is_sqlite_read_only(store.mark_processed("blocked-open")));
        assert!(!peer.is_processed("blocked-open").unwrap());

        let freezing = {
            let coordinator = coordinator.clone();
            tokio::spawn(async move { coordinator.freeze(Duration::from_secs(2)).await })
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while coordinator.phase() != Phase::Quiescing {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let start = Arc::new(Barrier::new(4));
        let (results_tx, results_rx) = mpsc::channel();
        let mut workers = Vec::new();
        for operation in 0..3 {
            let store = store.clone();
            let start = start.clone();
            let results_tx = results_tx.clone();
            workers.push(thread::spawn(move || {
                start.wait();
                let denied = match operation {
                    0 => is_sqlite_read_only(store.mark_processed("blocked-drain")),
                    1 => is_sqlite_read_only(store.delete_snapshot("doc")),
                    _ => is_sqlite_read_only(store.replace_activity_events("task", &[])),
                };
                results_tx.send(denied).unwrap();
            }));
        }
        start.wait();
        for _ in 0..3 {
            assert!(results_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(!peer.is_processed("blocked-drain").unwrap());
        assert_eq!(peer.load_snapshot("doc").unwrap().unwrap(), b"seed");
        assert_eq!(peer.activity_task_ids().unwrap(), ["task"]);

        store
            .save_snapshot_admitted(&first, "doc", b"writer-A")
            .unwrap();
        assert_eq!(peer.load_snapshot("doc").unwrap().unwrap(), b"writer-A");
        drop(first);
        let flush = freezing.await.unwrap().unwrap();
        store
            .save_snapshot_flushing(&flush.permit(), "doc", b"flushed")
            .unwrap();
        let snapshot = flush.into_snapshot().unwrap();
        assert_eq!(peer.load_snapshot("doc").unwrap().unwrap(), b"flushed");
        assert!(is_sqlite_read_only(peer.mark_processed("blocked-snapshot")));
        drop(snapshot);

        let second = coordinator.try_admit().unwrap();
        assert_eq!(second.epoch(), 2);
        store
            .save_snapshot_admitted(&second, "doc", b"writer-B")
            .unwrap();
        assert_eq!(peer.load_snapshot("doc").unwrap().unwrap(), b"writer-B");
        drop(second);
        drop(peer);
        drop(store);

        let ordinary = DocsStore::open(&data_dir).unwrap();
        assert!(ordinary.mark_processed("ordinary").unwrap());
        ordinary.delete_snapshot("doc").unwrap();
        ordinary.replace_activity_events("task", &[]).unwrap();
        assert_eq!(ordinary.load_snapshot("doc").unwrap(), None);
        assert!(ordinary.activity_task_ids().unwrap().is_empty());
    }

    #[test]
    fn guarded_database_registration_covers_aliases_and_all_live_handles() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("profile");
        std::fs::create_dir(&data_dir).unwrap();
        let coordinator = WriteEpoch::for_device_root(root.path()).unwrap();
        let permit = coordinator.try_admit().unwrap();
        let first = DocsStore::open_guarded(&data_dir, coordinator.clone(), &permit).unwrap();
        first
            .save_snapshot_admitted(&permit, "doc", b"unchanged")
            .unwrap();

        assert!(matches!(
            DocsStore::open(&data_dir),
            Err(StoreError::DatabaseOpenConflict)
        ));
        #[cfg(unix)]
        {
            let alias = root.path().join("profile-alias");
            std::os::unix::fs::symlink(&data_dir, &alias).unwrap();
            assert!(matches!(
                DocsStore::open(&alias),
                Err(StoreError::DatabaseOpenConflict)
            ));
        }

        let other_same_root = WriteEpoch::for_device_root(root.path()).unwrap();
        let other_permit = other_same_root.try_admit().unwrap();
        assert!(matches!(
            DocsStore::open_guarded(&data_dir, other_same_root, &other_permit),
            Err(StoreError::AdmissionMismatch)
        ));
        let wrong_root = tempfile::tempdir().unwrap();
        let wrong_coordinator = WriteEpoch::for_device_root(wrong_root.path()).unwrap();
        let wrong_permit = wrong_coordinator.try_admit().unwrap();
        assert!(matches!(
            DocsStore::open_guarded(&data_dir, wrong_coordinator, &wrong_permit),
            Err(StoreError::AdmissionMismatch)
        ));

        let second = DocsStore::open_guarded(&data_dir, coordinator.clone(), &permit).unwrap();
        assert_eq!(second.load_snapshot("doc").unwrap().unwrap(), b"unchanged");
        drop(first);
        assert!(matches!(
            DocsStore::open(&data_dir),
            Err(StoreError::DatabaseOpenConflict)
        ));
        drop(second);

        let unguarded = DocsStore::open(&data_dir).unwrap();
        assert_eq!(
            unguarded.load_snapshot("doc").unwrap().unwrap(),
            b"unchanged"
        );
        assert!(matches!(
            DocsStore::open_guarded(&data_dir, coordinator.clone(), &permit),
            Err(StoreError::DatabaseOpenConflict)
        ));
        drop(unguarded);
        DocsStore::open_guarded(&data_dir, coordinator, &permit).unwrap();
    }

    #[test]
    fn concurrent_open_modes_never_share_a_database() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("profile");
        std::fs::create_dir(&data_dir).unwrap();
        let coordinator = WriteEpoch::for_device_root(root.path()).unwrap();
        let permit = coordinator.try_admit().unwrap();
        const UNGUARDED: usize = 4;
        let start = Arc::new(Barrier::new(UNGUARDED + 2));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (results_tx, results_rx) = mpsc::channel();
        let mut threads = Vec::new();

        {
            let start = start.clone();
            let release_rx = release_rx.clone();
            let results_tx = results_tx.clone();
            let data_dir = data_dir.clone();
            threads.push(thread::spawn(move || {
                start.wait();
                let result = DocsStore::open_guarded(&data_dir, coordinator, &permit);
                results_tx.send((true, result.is_ok())).unwrap();
                let _ = release_rx.lock().unwrap().recv();
                drop(result);
            }));
        }
        for _ in 0..UNGUARDED {
            let start = start.clone();
            let release_rx = release_rx.clone();
            let results_tx = results_tx.clone();
            let data_dir = data_dir.clone();
            threads.push(thread::spawn(move || {
                start.wait();
                let result = DocsStore::open(&data_dir);
                results_tx.send((false, result.is_ok())).unwrap();
                let _ = release_rx.lock().unwrap().recv();
                drop(result);
            }));
        }
        start.wait();
        let results = (0..=UNGUARDED)
            .map(|_| results_rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect::<Vec<_>>();
        for _ in 0..=UNGUARDED {
            release_tx.send(()).unwrap();
        }
        for thread in threads {
            thread.join().unwrap();
        }
        let guarded_succeeded = results.iter().any(|(guarded, ok)| *guarded && *ok);
        let unguarded_succeeded = results.iter().any(|(guarded, ok)| !*guarded && *ok);
        assert!(guarded_succeeded || unguarded_succeeded);
        assert!(!(guarded_succeeded && unguarded_succeeded));
    }

    #[test]
    fn opening_reservation_blocks_before_sqlite_open() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("profile");
        std::fs::create_dir(&data_dir).unwrap();
        let coordinator = WriteEpoch::for_device_root(root.path()).unwrap();
        let _permit = coordinator.try_admit().unwrap();
        let reservation = OpenRegistration::acquire(
            canonical_database_path(&data_dir).unwrap(),
            Some(&coordinator),
        )
        .unwrap();
        assert!(!data_dir.join("docs.sqlite3").exists());

        let attempted = thread::spawn({
            let data_dir = data_dir.clone();
            move || {
                matches!(
                    DocsStore::open(&data_dir),
                    Err(StoreError::DatabaseOpenConflict)
                )
            }
        });
        assert!(attempted.join().unwrap());
        assert!(!data_dir.join("docs.sqlite3").exists());
        drop(reservation);
        DocsStore::open(&data_dir).unwrap();
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
    fn task_activity_page_uses_exact_task_and_opaque_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        store
            .replace_activity_events(
                "task-a",
                &[
                    activity("a-new", "task-a", "project-a", 30),
                    activity("a-old", "task-a", "project-a", 10),
                ],
            )
            .unwrap();
        store
            .replace_activity_events("task-b", &[activity("b-middle", "task-b", "project-a", 20)])
            .unwrap();
        let (first, cursor, more) = store.read_activity_task_page("task-a", None, 1).unwrap();
        assert_eq!(
            first
                .iter()
                .map(|event| event.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a-new"]
        );
        assert!(more);
        let (second, _, more) = store
            .read_activity_task_page("task-a", cursor.as_ref(), 1)
            .unwrap();
        assert_eq!(
            second
                .iter()
                .map(|event| event.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a-old"]
        );
        assert!(!more);
        assert!(
            store
                .read_activity_task_page("unknown", None, 20)
                .unwrap()
                .0
                .is_empty()
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

    #[test]
    fn appshot_lifecycle_is_paired_minimal_and_bound_to_accepted_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let mut canvas = draft(1, "unsent", "/synthetic/fixture.png");
        canvas.draft_id = "canvas-a".into();
        canvas.task_id = None;
        store.save_composer_draft("viewer-a", &canvas).unwrap();
        assert!(
            store
                .appshot_activity_for_task("host-task")
                .unwrap()
                .is_empty()
        );
        let raw: Vec<Vec<u8>> = store
            .conn()
            .prepare("SELECT record_json FROM appshot_activity")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(raw.len(), 2);
        for bytes in &raw {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains("synthetic captured context"));
            assert!(!text.contains("Synthetic window"));
            assert!(!text.contains("/synthetic/"));
        }
        store
            .begin_draft_submission("viewer-a", "message-a", Some("host-task"), &canvas)
            .unwrap();
        store
            .accept_draft_submission("viewer-a", "message-a", None)
            .unwrap();
        store
            .bind_appshot_target_environment("host-task", "host-a")
            .unwrap();
        store
            .bind_appshot_target_environment("host-task", "host-b")
            .unwrap();
        let records = store.appshot_activity_for_task("host-task").unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(
            records.iter().map(|event| event.kind).collect::<Vec<_>>(),
            vec![
                AppshotActivityKind::Captured,
                AppshotActivityKind::Staged,
                AppshotActivityKind::Sent,
            ]
        );
        assert!(
            records
                .iter()
                .all(|event| event.capture_device_id == "viewer-a"
                    && event.capture_id == "shot-a"
                    && event.screenshot_attachment_id == "image-a"
                    && event.target_environment_id.as_deref() == Some("host-a"))
        );
        assert_eq!(records[2].message_id.as_deref(), Some("message-a"));
        store
            .accept_draft_submission("viewer-a", "message-a", None)
            .unwrap();
        assert_eq!(
            store.appshot_activity_for_task("host-task").unwrap().len(),
            3
        );
    }

    #[test]
    fn appshot_removal_and_stale_save_do_not_recreate_capture() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let first = draft(1, "draft", "/synthetic/fixture.png");
        store.save_composer_draft("viewer-a", &first).unwrap();
        let mut removed = draft(2, "draft", "/synthetic/fixture.png");
        removed.appshots.clear();
        removed.attachments.clear();
        store.save_composer_draft("viewer-a", &removed).unwrap();
        assert!(matches!(
            store.save_composer_draft("viewer-a", &first).unwrap(),
            ComposerDraftSaveOutcome::Stale { .. }
        ));
        let records = store.appshot_activity_for_task("task-a").unwrap();
        assert_eq!(
            records.iter().map(|event| event.kind).collect::<Vec<_>>(),
            vec![
                AppshotActivityKind::Captured,
                AppshotActivityKind::Staged,
                AppshotActivityKind::Removed,
            ]
        );
    }

    #[test]
    fn appshot_staging_waits_for_saved_screenshot_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let mut waiting = draft(1, "draft", "/synthetic/fixture.png");
        waiting.attachments[0].state = luna_proto::DraftBlobState::WaitingForUpload;
        waiting.attachments[0].path = None;
        store.save_composer_draft("viewer-a", &waiting).unwrap();
        assert_eq!(
            store
                .appshot_activity_for_task("task-a")
                .unwrap()
                .iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            vec![AppshotActivityKind::Captured]
        );
        store
            .save_composer_draft("viewer-a", &draft(2, "draft", "/synthetic/fixture.png"))
            .unwrap();
        assert_eq!(
            store
                .appshot_activity_for_task("task-a")
                .unwrap()
                .iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            vec![AppshotActivityKind::Captured, AppshotActivityKind::Staged,]
        );
    }

    #[test]
    fn accepted_queue_is_not_recorded_as_sent() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        let mut queued = draft(1, "queued", "/synthetic/fixture.png");
        queued.draft_id = "task-queue".into();
        queued.task_id = Some("task-queue".into());
        store.save_composer_draft("viewer-a", &queued).unwrap();
        store
            .begin_draft_submission("viewer-a", "message-queue", Some("task-queue"), &queued)
            .unwrap();
        store
            .accept_draft_submission("viewer-a", "message-queue", Some("queue-a"))
            .unwrap();
        let records = store.appshot_activity_for_task("task-queue").unwrap();
        assert_eq!(records.last().unwrap().kind, AppshotActivityKind::Queued);
    }

    pub(super) fn draft(revision: u64, text: &str, path: &str) -> ComposerDraft {
        ComposerDraft {
            input_items: Vec::new(),
            version: luna_proto::COMPOSER_DRAFT_VERSION,
            draft_id: "task-a".into(),
            task_id: Some("task-a".into()),
            revision,
            text: text.into(),
            attachments: vec![luna_proto::DraftBlob {
                attachment_id: "image-a".into(),
                name: "fixture.png".into(),
                path: Some(path.into()),
                sha256: Some("synthetic".into()),
                size: Some(7),
                state: luna_proto::DraftBlobState::Saved,
                error: None,
            }],
            appshots: vec![luna_proto::DraftAppshot {
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
            context: luna_proto::DraftContext {
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
    fn guarded_enqueue_denial_does_not_invalidate_later_admitted_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        let seed = DocsStore::open(&profile).unwrap();
        seed.initialize_chat_outbox("chat", &[]).unwrap();
        drop(seed);

        let coordinator = WriteEpoch::for_device_root(root.path()).unwrap();
        let permit = coordinator.try_admit().unwrap();
        let guarded = DocsStore::open_guarded(&profile, coordinator, &permit).unwrap();
        assert!(guarded.chat_outbox_initialized("chat").unwrap());
        assert!(matches!(
            guarded.enqueue_chat_update("chat", "denied", b"not-published"),
            Err(StoreError::AdmissionRequired)
        ));
        assert!(!guarded.failed_publications.lock().unwrap().contains("chat"));
        guarded
            .save_snapshot_admitted(&permit, "chat", b"valid")
            .unwrap();
        assert!(guarded.chat_outbox_initialized("chat").unwrap());
        assert!(guarded.pending_chat_updates("chat").unwrap().is_empty());
        assert_eq!(guarded.load_snapshot("chat").unwrap().unwrap(), b"valid");
    }

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
            assert!(store.failed_publications.lock().unwrap().contains("chat"));
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

    #[test]
    fn task_cleanup_preserves_shared_blob_and_recovers_after_failed_retirement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.png").to_string_lossy().to_string();
        let store = DocsStore::open(dir.path()).unwrap();
        store
            .save_composer_draft("device", &super::draft_tests::draft(1, "first", &path))
            .unwrap();
        let mut other = super::draft_tests::draft(1, "second", &path);
        other.draft_id = "task-b".into();
        other.task_id = Some("task-b".into());
        store.save_composer_draft("device", &other).unwrap();
        store
            .prepare_task_cleanup("device", "task-a", "synthetic-boot")
            .unwrap();
        assert!(matches!(
            store.save_composer_draft("device", &super::draft_tests::draft(2, "late", &path)),
            Err(StoreError::TaskCleanupPending(_))
        ));
        assert_eq!(
            store.task_cleanup_paths("task-a").unwrap(),
            vec![path.clone()]
        );
        store
            .conn()
            .execute_batch(
                "CREATE TRIGGER fail_l18_retire BEFORE DELETE ON composer_drafts
             BEGIN SELECT RAISE(ABORT, 'synthetic cleanup failure'); END;",
            )
            .unwrap();
        assert!(store.retire_task_local_records("device", "task-a").is_err());
        assert!(store.attachment_has_owner(&path).unwrap());
        store
            .set_task_cleanup_state("task-a", "failed", "synthetic cleanup failure")
            .unwrap();
        drop(store);

        let store = DocsStore::open(dir.path()).unwrap();
        assert_eq!(store.pending_task_cleanups(16).unwrap(), vec!["task-a"]);
        store
            .conn()
            .execute_batch("DROP TRIGGER fail_l18_retire")
            .unwrap();
        store.retire_task_local_records("device", "task-a").unwrap();
        assert!(
            store.attachment_has_owner(&path).unwrap(),
            "task-b still owns the blob"
        );
        store
            .set_task_cleanup_state("task-a", "completed", "done")
            .unwrap();
        assert!(store.task_cleanup_paths("task-a").unwrap().is_empty());
    }

    #[test]
    fn cleanup_keyset_pages_advance_past_the_first_batch() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        for index in 0..21 {
            let task = format!("task-{index:02}");
            store
                .prepare_task_cleanup("device", &task, "old-boot")
                .unwrap();
        }
        for index in 0..10 {
            let project = format!("project-{index:02}");
            store
                .prepare_project_cleanup(&project, &[], "old-boot")
                .unwrap();
        }
        store
            .set_task_cleanup_state("task-00", "failed", "synthetic blockage")
            .unwrap();
        let first = store.pending_task_cleanups_after("", 16).unwrap();
        assert_eq!(first.len(), 16);
        let second = store
            .pending_task_cleanups_after(&first.last().unwrap().0, 16)
            .unwrap();
        assert_eq!(second.len(), 5);
        assert_eq!(second.last().unwrap().0, "task-20");
        assert!(
            store
                .pending_task_cleanups_after(&second.last().unwrap().0, 16)
                .unwrap()
                .is_empty()
        );
        let projects = store.pending_project_cleanups_after("", 8).unwrap();
        assert_eq!(projects.len(), 8);
        let rest = store
            .pending_project_cleanups_after(&projects.last().unwrap().0, 8)
            .unwrap();
        assert_eq!(rest.len(), 2);
        assert_eq!(rest.last().unwrap().0, "project-09");
        store
            .conn()
            .execute(
                "UPDATE task_cleanup SET updated_at=?1 WHERE state='pending'",
                params![now_ms() - 10_000],
            )
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE project_cleanup SET updated_at=?1 WHERE state='pending'",
                params![now_ms() - 10_000],
            )
            .unwrap();
        let (pending, failed, detail) = store.deletion_cleanup_summary().unwrap();
        assert_eq!((pending, failed), (30, 1));
        assert_eq!(detail.as_deref(), Some("synthetic blockage"));
    }
}
