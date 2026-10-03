//! Private execution lineage and atomic association-repair commit boundary.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceRepairFence {
    pub expected_task_revision: u64,
    pub expected_workspace_repair_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMaterializationFence {
    pub expected_workspace_repair_revision: u64,
    pub original_command_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalExecutableAuthority {
    pub payload_hash: String,
    pub workspace_repair_revision: u64,
}

pub(super) fn workspace_repair_revision_in(
    conn: &Connection,
    task_id: &str,
) -> Result<u64, StoreError> {
    Ok(conn
        .query_row(
            "SELECT workspace_repair_revision FROM task_command_state WHERE task_id=?1",
            [task_id],
            |row| row.get::<_, u64>(0),
        )
        .optional()?
        .unwrap_or(0))
}

pub(super) fn command_workspace_revision_in(
    conn: &Connection,
    command_id: &str,
) -> Result<Option<u64>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT workspace_repair_revision FROM task_command_receipts WHERE command_id=?1",
            [command_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn recovery_workspace_revision(bytes: &[u8]) -> Result<u64, StoreError> {
    let record: serde_json::Value = serde_json::from_slice(bytes)?;
    match record.get("workspaceRepairRevision") {
        None => Ok(0),
        Some(value) => value.as_u64().ok_or_else(|| {
            StoreError::WorkspaceRepairConflict(
                "invalid original recovery workspace revision".into(),
            )
        }),
    }
}

fn quiescence_blocker(
    conn: &Connection,
    task_id: &str,
    repair_command_id: Option<&str>,
) -> Result<Option<String>, StoreError> {
    if let Some(command_id) = repair_command_id {
        let bytes: Option<Vec<u8>> = conn
            .query_row(
                "SELECT receipt_json FROM task_command_receipts WHERE command_id=?1 AND task_id=?2",
                params![command_id, task_id],
                |row| row.get(0),
            )
            .optional()?;
        let valid = bytes
            .map(|bytes| serde_json::from_slice::<CommandReceipt>(&bytes))
            .transpose()?
            .is_some_and(|receipt| {
                receipt.status == CommandReceiptStatus::Pending
                    && receipt.intent == "repairWorkspaceIdentity"
            });
        if !valid {
            return Ok(Some(
                "the exact pending repair command is unavailable".into(),
            ));
        }
    }
    let pending: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_command_receipts WHERE task_id=?1
         AND (?2 IS NULL OR command_id<>?2)
         AND json_extract(CAST(receipt_json AS TEXT),'$.status')='pending')",
        params![task_id, repair_command_id],
        |row| row.get(0),
    )?;
    if pending {
        return Ok(Some(
            "a task command is pending or its outcome is unknown".into(),
        ));
    }
    let side_send: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM side_chat_sends WHERE chat_id=?1 AND state='unknown')",
        [task_id],
        |row| row.get(0),
    )?;
    if side_send {
        return Ok(Some(
            "a side-panel send is in flight or its outcome is unknown".into(),
        ));
    }
    let recovery: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_recovery_state WHERE task_id=?1
         AND status IN ('scheduled','running','checkingDelivery','held','recoveryNeeded','waitingForSignIn'))",
        [task_id], |row| row.get(0))?;
    if recovery {
        return Ok(Some(
            "task recovery is pending or available for retry".into(),
        ));
    }

    // Inspect the durable document and its undelivered local updates without
    // opening a live handle, starting a watcher, or publishing a recovery edit.
    let snapshot: Option<Vec<u8>> = conn
        .query_row(
            "SELECT bytes FROM snapshots WHERE doc_id=?1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    let raw = loro::LoroDoc::new();
    if let Some(snapshot) = snapshot {
        let imported = raw.import(&snapshot).map_err(|_| {
            StoreError::WorkspaceRepairConflict("task snapshot cannot be verified".into())
        })?;
        if imported.pending.is_some() {
            return Ok(Some("task snapshot is missing causal history".into()));
        }
    }
    let mut statement =
        conn.prepare("SELECT bytes FROM chat_outbox WHERE doc_id=?1 ORDER BY ordinal")?;
    for update in statement.query_map([task_id], |row| row.get::<_, Vec<u8>>(0))? {
        let imported = raw.import(&update?).map_err(|_| {
            StoreError::WorkspaceRepairConflict("pending task updates cannot be verified".into())
        })?;
        if imported.pending.is_some() {
            return Ok(Some(
                "pending task updates are missing causal history".into(),
            ));
        }
    }
    let doc = luna_doc::SessionDoc::from_doc(raw);
    if !doc
        .read_queue()
        .map_err(|_| StoreError::WorkspaceRepairConflict("task queue cannot be verified".into()))?
        .is_empty()
    {
        return Ok(Some(
            "the task has queued, claimed or uncertain messages".into(),
        ));
    }
    if doc
        .read_commands()
        .map_err(|_| {
            StoreError::WorkspaceRepairConflict("task commands cannot be verified".into())
        })?
        .iter()
        .any(|entry| entry.status == luna_doc::SessionCommandStatus::Pending)
    {
        return Ok(Some(
            "the task document has a pending executable command".into(),
        ));
    }
    Ok(None)
}

impl DocsStore {
    /// Preparation within the original admitted Run, not a repair or a second
    /// command completion. The original receipt and R/W remain untouched.
    pub fn persist_workspace_materialization(
        &self,
        command_id: &str,
        task_id: &str,
        fence: &WorkspaceMaterializationFence,
        snapshots: &[(&str, &[u8])],
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        ensure_task_not_cleaning(&tx, task_id)?;
        let bytes: Option<Vec<u8>> = tx
            .query_row(
                "SELECT receipt_json FROM task_command_receipts WHERE command_id=?1 AND task_id=?2",
                params![command_id, task_id],
                |row| row.get(0),
            )
            .optional()?;
        let receipt = bytes
            .map(|bytes| serde_json::from_slice::<CommandReceipt>(&bytes))
            .transpose()?;
        let current_w = workspace_repair_revision_in(&tx, task_id)?;
        let matches: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_executable_authority WHERE doc_id=?1 AND kind='command'
             AND row_id=?2 AND payload_hash=?3 AND workspace_repair_revision=?4)",
            params![task_id, command_id, fence.original_command_hash, fence.expected_workspace_repair_revision],
            |row| row.get(0))?;
        if !matches
            || current_w != fence.expected_workspace_repair_revision
            || command_workspace_revision_in(&tx, command_id)? != Some(current_w)
            || !receipt.is_some_and(|receipt| {
                receipt.status == CommandReceiptStatus::Pending && receipt.intent == "run"
            })
        {
            return Err(StoreError::WorkspaceRepairConflict(
                "initial worktree materialization lost its original pending Run authority".into(),
            ));
        }
        self.persist_snapshot_changes_tx(&tx, snapshots, &[], &[])?;
        tx.commit()?;
        Ok(())
    }

    pub fn execution_workspace_repair_revision(
        &self,
        task_id: &str,
        message_id: &str,
    ) -> Result<Option<u64>, StoreError> {
        Ok(self.conn().query_row("SELECT workspace_repair_revision FROM task_execution_lineage WHERE task_id=?1 AND message_id=?2",
            params![task_id,message_id], |row| row.get(0)).optional()?)
    }

    /// Freeze the original message lineage before any provider effect. This
    /// also covers trusted engine callers without a public command receipt.
    pub fn record_execution_workspace_repair_revision(
        &self,
        task_id: &str,
        message_id: &str,
        revision: u64,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        ensure_task_not_cleaning(&tx, task_id)?;
        if workspace_repair_revision_in(&tx, task_id)? != revision {
            return Err(StoreError::WorkspaceRepairConflict(
                "dispatch was admitted before workspace repair".into(),
            ));
        }
        tx.execute("INSERT OR IGNORE INTO task_execution_lineage(task_id,message_id,workspace_repair_revision) VALUES (?1,?2,?3)",
            params![task_id,message_id,revision])?;
        let original: u64 = tx.query_row("SELECT workspace_repair_revision FROM task_execution_lineage WHERE task_id=?1 AND message_id=?2",
            params![task_id,message_id], |row| row.get(0))?;
        if original != revision {
            return Err(StoreError::WorkspaceRepairConflict(
                "the original message workspace revision cannot be renewed".into(),
            ));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn workspace_repair_revision(&self, task_id: &str) -> Result<u64, StoreError> {
        workspace_repair_revision_in(&self.conn(), task_id)
    }

    pub fn execution_configuration(
        &self,
        task_id: &str,
        message_id: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.conn().query_row(
            "SELECT configuration FROM task_execution_lineage WHERE task_id=?1 AND message_id=?2",
            params![task_id, message_id],
            |row| row.get::<_, Option<Vec<u8>>>(0),
        ).optional()?.flatten())
    }

    /// Freeze once alongside the original message lineage, before provider
    /// effects. A continuation must never renew the original configuration.
    pub fn record_execution_configuration(
        &self,
        task_id: &str,
        message_id: &str,
        configuration: &[u8],
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        ensure_task_not_cleaning(&tx, task_id)?;
        let changed = tx.execute(
            "UPDATE task_execution_lineage SET configuration=COALESCE(configuration,?3)
             WHERE task_id=?1 AND message_id=?2",
            params![task_id, message_id, configuration],
        )?;
        if changed != 1 {
            return Err(StoreError::WorkspaceRepairConflict(
                "original execution lineage is missing before configuration capture".into(),
            ));
        }
        tx.commit()?;
        Ok(())
    }

    /// Read the original admission stamp, never the task's current stamp.
    pub fn command_workspace_repair_revision(
        &self,
        command_id: &str,
    ) -> Result<Option<u64>, StoreError> {
        command_workspace_revision_in(&self.conn(), command_id)
    }

    pub fn workspace_repair_blocker(
        &self,
        task_id: &str,
        repair_command_id: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        quiescence_blocker(&self.conn(), task_id, repair_command_id)
    }

    pub fn local_executable_authority(
        &self,
        doc_id: &str,
        kind: LocalExecutableKind,
        row_id: &str,
    ) -> Result<Option<LocalExecutableAuthority>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT payload_hash,workspace_repair_revision FROM local_executable_authority
             WHERE doc_id=?1 AND kind=?2 AND row_id=?3",
                params![doc_id, kind.as_str(), row_id],
                |row| {
                    Ok(LocalExecutableAuthority {
                        payload_hash: row.get(0)?,
                        workspace_repair_revision: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// Association-only repair is committed once with its exact pending receipt.
    /// RPC duplicate handling must return terminal receipts before staging an
    /// identity projection; a terminal callback here cannot publish new state.
    #[allow(clippy::too_many_arguments)]
    pub fn complete_task_command_with_snapshot_changes_and_workspace_repair(
        &self,
        command_id: &str,
        task_id: &str,
        fence: &WorkspaceRepairFence,
        binding_id: Option<&str>,
        binding_generation: Option<u32>,
        snapshots: &[(&str, &[u8])],
    ) -> Result<CommandReceipt, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        ensure_task_not_cleaning(&tx, task_id)?;
        let current = load_task_command_snapshot(&tx, task_id)?;
        let w = workspace_repair_revision_in(&tx, task_id)?;
        if current.task_revision != fence.expected_task_revision
            || w != fence.expected_workspace_repair_revision
        {
            return Err(StoreError::WorkspaceRepairConflict(
                "the task or workspace repair revision changed".into(),
            ));
        }
        if let Some(blocker) = quiescence_blocker(&tx, task_id, Some(command_id))? {
            return Err(StoreError::WorkspaceRepairConflict(blocker));
        }
        let bytes: Vec<u8> = tx.query_row(
            "SELECT receipt_json FROM task_command_receipts WHERE command_id=?1 AND task_id=?2",
            params![command_id, task_id],
            |row| row.get(0),
        )?;
        let mut receipt: CommandReceipt = serde_json::from_slice(&bytes)?;
        if command_workspace_revision_in(&tx, command_id)? != Some(w)
            || receipt.snapshot.task_revision != current.task_revision
            || receipt.snapshot.binding_id.as_deref() != binding_id
            || receipt.snapshot.binding_generation != binding_generation
            || receipt.snapshot.active_turn_id.is_some()
        {
            return Err(StoreError::WorkspaceRepairConflict(
                "repair admission or binding evidence changed".into(),
            ));
        }
        let next = current
            .task_revision
            .checked_add(1)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| {
                StoreError::WorkspaceRepairConflict("repair revision overflow".into())
            })?;
        receipt.status = CommandReceiptStatus::Applied;
        receipt.reason = None;
        receipt.recovery_action = None;
        receipt.committed_at_ms = Some(now_ms());
        receipt.observed_at_ms = now_ms();
        let receipt = commit_task_command_tx(&tx, receipt, true)?;
        debug_assert_eq!(receipt.snapshot.task_revision, next as u64);
        tx.execute(
            "UPDATE task_command_state SET workspace_repair_revision=?2 WHERE task_id=?1",
            params![task_id, next],
        )?;
        self.persist_snapshot_changes_tx(&tx, snapshots, &[], &[])?;
        tx.commit()?;
        Ok(receipt)
    }
}
