//! Atomic host-local automation metadata and no-replay occurrence claims.
use super::*;
use luna_proto::{
    AutomationMutationReceipt, AutomationNotificationPolicy, AutomationReadRequest,
    AutomationRecord, AutomationRun, AutomationRunState as State,
};

/// Minimal producer metadata; prompts and provider configuration are not read.
pub struct AutomationAttentionSource {
    pub automation_id: String,
    pub origin_task_id: String,
    pub revision: u64,
    pub updated_at_ms: i64,
    pub watch_unknown: bool,
    pub run: Option<AutomationRun>,
    pub notification_policy: AutomationNotificationPolicy,
    pub project_id: String,
}

fn get_record(c: &Connection, id: &str) -> Result<Option<AutomationRecord>, StoreError> {
    let b: Option<Vec<u8>> = c
        .query_row("SELECT record FROM automations WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    b.map(|v| serde_json::from_slice(&v))
        .transpose()
        .map_err(Into::into)
}
fn write_record(c: &Connection, r: &AutomationRecord) -> Result<(), StoreError> {
    c.execute("INSERT INTO automations(id,owner_host_id,revision,enabled,deleted,next_due_ms,origin_task_id,record) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,enabled=excluded.enabled,deleted=excluded.deleted,next_due_ms=excluded.next_due_ms,record=excluded.record,origin_task_id=excluded.origin_task_id",params![r.id,r.owner_host_id,r.revision,r.enabled,r.deleted,r.next_due_ms,r.authority.origin_task_id,serde_json::to_vec(r)?])?;
    Ok(())
}
fn write_run(c: &Connection, r: &AutomationRun) -> Result<(), StoreError> {
    c.execute("INSERT INTO automation_runs(id,automation_id,owner_host_id,scheduled_at_ms,task_id,message_id,blocks_next,state,record) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(id) DO UPDATE SET blocks_next=excluded.blocks_next,state=excluded.state,record=excluded.record",params![r.id,r.automation_id,r.owner_host_id,r.scheduled_at_ms,r.task_id,r.message_id,r.state.blocks_next(),serde_json::to_string(&r.state)?,serde_json::to_vec(r)?])?;
    Ok(())
}
/// A deleted definition keeps its tombstone and run history, but its items
/// leave Activity (Needs attention and Recent). Rows are dismissed, never
/// removed, so attention history stays auditable; an item still unresolved is
/// resolved first, with its transition. The range is the projector's
/// `automation:<host>:<id>:` key prefix (`;` is the byte after `:`). Ids may
/// contain `:` but the run/`watch`/`skipped` suffix never does, so a colon-free
/// remainder keeps `a` from matching the items of a live `a:b`.
pub(super) fn retire_deleted_automation_attention(
    tx: &rusqlite::Transaction<'_>,
    now: i64,
) -> Result<bool, StoreError> {
    let mut q = tx.prepare(
        "SELECT i.item_json FROM automations a CROSS JOIN attention_items i
         WHERE a.deleted=1 AND i.dismissed=0
           AND i.dedup_key>='automation:'||a.owner_host_id||':'||a.id||':'
           AND i.dedup_key<'automation:'||a.owner_host_id||':'||a.id||';'
           AND instr(substr(i.dedup_key,length(a.owner_host_id)+length(a.id)+14),':')=0",
    )?;
    let rows = q
        .query_map([], |r| r.get::<_, Vec<u8>>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(q);
    for bytes in &rows {
        let mut item: AttentionItem = serde_json::from_slice(bytes)?;
        if item.unresolved() {
            item.lifecycle = AttentionLifecycle::Resolved;
            item.resolved_at_ms = Some(now);
            item.updated_at_ms = now;
            insert_attention_transition(tx, AttentionTransitionKind::Resolved, &item)?;
        }
        update_attention_item(tx, &item, true)?;
    }
    Ok(!rows.is_empty())
}
impl DocsStore {
    pub fn automation_changes(
        &self,
    ) -> tokio::sync::watch::Receiver<luna_proto::AutomationWatchFrame> {
        self.automation_changes.subscribe()
    }

    fn notify_automation_change(&self) {
        self.automation_changes
            .send_modify(|frame| frame.seq = frame.seq.wrapping_add(1));
    }
    pub fn automation_attention_sources(
        &self,
        host: &str,
    ) -> Result<Vec<AutomationAttentionSource>, StoreError> {
        let c = self.conn();
        let mut q = c.prepare("SELECT a.id,a.origin_task_id,a.revision,json_extract(CAST(a.record AS TEXT),'$.updatedAtMs'),json_extract(CAST(a.record AS TEXT),'$.watch.lastError') IS NOT NULL,(SELECT r.record FROM automation_runs r WHERE r.automation_id=a.id ORDER BY r.blocks_next DESC,r.id DESC LIMIT 1),a.record FROM automations a WHERE a.owner_host_id=?1 AND a.deleted=0 ORDER BY a.id LIMIT 1000")?;
        let rows = q
            .query_map([host], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, Option<Vec<u8>>>(5)?,
                    r.get::<_, Vec<u8>>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(
                    automation_id,
                    origin_task_id,
                    revision,
                    updated_at_ms,
                    watch_unknown,
                    run,
                    record,
                )| {
                    let record: AutomationRecord = serde_json::from_slice(&record)?;
                    Ok(AutomationAttentionSource {
                        automation_id,
                        origin_task_id,
                        revision,
                        updated_at_ms,
                        watch_unknown,
                        run: run.map(|b| serde_json::from_slice(&b)).transpose()?,
                        notification_policy: record.definition.notification_policy,
                        project_id: record.authority.project_id,
                    })
                },
            )
            .collect()
    }
    /// Read observations have no task effect. Fence against edits and competing
    /// polls without generating noisy occurrence rows for unchanged state.
    pub fn observe_automation(
        &self,
        expected: &AutomationRecord,
        next: &AutomationRecord,
        permit: Option<&WritePermit>,
    ) -> Result<bool, StoreError> {
        let mut c = self.command_writer(permit)?;
        let tx = c.transaction()?;
        if get_record(&tx, &expected.id)?.as_ref() != Some(expected) {
            return Ok(false);
        }
        write_record(&tx, next)?;
        tx.commit()?;
        self.notify_automation_change();
        Ok(true)
    }

    pub fn automation(&self, id: &str) -> Result<Option<AutomationRecord>, StoreError> {
        get_record(&self.conn(), id)
    }
    pub fn automation_mutation_receipt(
        &self,
        id: &str,
    ) -> Result<Option<AutomationMutationReceipt>, StoreError> {
        let b: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT receipt FROM automation_mutations WHERE request_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        b.map(|v| serde_json::from_slice(&v))
            .transpose()
            .map_err(Into::into)
    }
    pub fn automations(
        &self,
        host: &str,
        filter: &AutomationReadRequest,
        limit: usize,
    ) -> Result<Vec<AutomationRecord>, StoreError> {
        self.automation_rows(host, filter, limit, None)
    }

    /// Provider reads retain project scope even when the caller has historical
    /// runs of an automation that was subsequently moved to another project.
    pub fn agent_automations(
        &self,
        host: &str,
        filter: &AutomationReadRequest,
        project: &str,
        caller: &str,
        limit: usize,
    ) -> Result<Vec<AutomationRecord>, StoreError> {
        self.automation_rows(host, filter, limit, Some((project, caller)))
    }

    fn automation_rows(
        &self,
        host: &str,
        filter: &AutomationReadRequest,
        limit: usize,
        visibility: Option<(&str, &str)>,
    ) -> Result<Vec<AutomationRecord>, StoreError> {
        let c = self.conn();
        let mut q=c.prepare("SELECT a.record FROM automations a WHERE a.owner_host_id=?1 AND (?2 IS NULL OR a.origin_task_id=?2 OR json_extract(CAST(a.record AS TEXT),'$.definition.sourceTaskId')=?2 OR json_extract(CAST(a.record AS TEXT),'$.definition.target.taskId')=?2 OR EXISTS(SELECT 1 FROM automation_runs r WHERE r.automation_id=a.id AND r.task_id=?2)) AND (?3 IS NULL OR a.id>?3) AND (?4 OR a.deleted=0) AND (?5 IS NULL OR json_extract(CAST(a.record AS TEXT),'$.authority.projectId')=?5) AND (?6 IS NULL OR a.id=?6) AND (?8 IS NULL OR json_extract(CAST(a.record AS TEXT),'$.authority.projectId')=?8 OR COALESCE(json_extract(CAST(a.record AS TEXT),'$.definition.sourceTaskId'),a.origin_task_id)=?9) ORDER BY a.id LIMIT ?7")?;
        let rows = q
            .query_map(
                params![
                    host,
                    filter.task_id,
                    filter.after_id,
                    filter.include_deleted,
                    filter.project_id,
                    filter.automation_id,
                    limit.clamp(1, 101),
                    visibility.map(|v| v.0),
                    visibility.map(|v| v.1)
                ],
                |r| r.get::<_, Vec<u8>>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|b| Ok(serde_json::from_slice(&b)?))
            .collect()
    }
    pub fn due_automations(
        &self,
        host: &str,
        now: i64,
        pr_watches: bool,
    ) -> Result<Vec<AutomationRecord>, StoreError> {
        let c = self.conn();
        let mut q=c.prepare("SELECT record FROM automations WHERE owner_host_id=?1 AND enabled=1 AND deleted=0 AND next_due_ms<=?2 AND (json_extract(CAST(record AS TEXT),'$.definition.trigger.kind')='prWatch')=?3 ORDER BY next_due_ms,id LIMIT 32")?;
        let rows = q
            .query_map(params![host, now, pr_watches], |r| r.get::<_, Vec<u8>>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|b| Ok(serde_json::from_slice(&b)?))
            .collect()
    }
    pub fn automation_runs(
        &self,
        host: &str,
        id: Option<&str>,
        after: Option<&str>,
        active_only: bool,
        limit: usize,
    ) -> Result<Vec<AutomationRun>, StoreError> {
        let c = self.conn();
        let mut q=c.prepare("SELECT record FROM automation_runs WHERE owner_host_id=?1 AND (?2 IS NULL OR automation_id=?2) AND (?3 IS NULL OR id<?3) AND (NOT ?4 OR blocks_next=1) ORDER BY id DESC LIMIT ?5")?;
        let rows = q
            .query_map(
                params![host, id, after, active_only, limit.clamp(1, 101)],
                |r| r.get::<_, Vec<u8>>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|b| Ok(serde_json::from_slice(&b)?))
            .collect()
    }

    /// UI and tool reads intersect every supplied scope before pagination.
    pub fn scoped_automation_runs(
        &self,
        host: &str,
        filter: &AutomationReadRequest,
        limit: usize,
    ) -> Result<Vec<AutomationRun>, StoreError> {
        let c = self.conn();
        let mut q = c.prepare("SELECT r.record FROM automation_runs r JOIN automations a ON a.id=r.automation_id WHERE r.owner_host_id=?1 AND (?2 IS NULL OR r.automation_id=?2) AND (?3 IS NULL OR r.task_id=?3) AND (?4 IS NULL OR COALESCE(NULLIF(json_extract(CAST(r.record AS TEXT),'$.projectId'),''),json_extract(CAST(a.record AS TEXT),'$.authority.projectId'))=?4) AND (?5 IS NULL OR r.id<?5) ORDER BY r.id DESC LIMIT ?6")?;
        let rows = q
            .query_map(
                params![
                    host,
                    filter.automation_id,
                    filter.task_id,
                    filter.project_id,
                    filter.after_id,
                    limit.clamp(1, 101)
                ],
                |r| r.get::<_, Vec<u8>>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|b| Ok(serde_json::from_slice(&b)?))
            .collect()
    }

    pub fn automation_run(&self, id: &str) -> Result<Option<AutomationRun>, StoreError> {
        let b: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT record FROM automation_runs WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        b.map(|b| serde_json::from_slice(&b))
            .transpose()
            .map_err(Into::into)
    }

    pub fn automation_for_message(
        &self,
        task: &str,
        message: &str,
    ) -> Result<Option<AutomationRun>, StoreError> {
        let b: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT record FROM automation_runs WHERE task_id=?1 AND message_id=?2",
                params![task, message],
                |r| r.get(0),
            )
            .optional()?;
        b.map(|b| serde_json::from_slice(&b))
            .transpose()
            .map_err(Into::into)
    }

    pub fn automation_terminal_effects(
        &self,
        host: &str,
    ) -> Result<Vec<AutomationRun>, StoreError> {
        let c = self.conn();
        let mut q = c.prepare("SELECT record FROM automation_runs WHERE owner_host_id=?1 AND blocks_next=0 AND json_extract(CAST(record AS TEXT),'$.finalizedAtMs') IS NULL ORDER BY id LIMIT 100")?;
        let rows = q
            .query_map([host], |r| r.get::<_, Vec<u8>>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|b| Ok(serde_json::from_slice(&b)?))
            .collect()
    }
    pub fn mutate_automation(
        &self,
        expected: u64,
        r: &AutomationRecord,
        receipt: &AutomationMutationReceipt,
        cancel: Option<&str>,
        resolve_unknown: Option<&str>,
        permit: Option<&WritePermit>,
    ) -> Result<AutomationMutationReceipt, StoreError> {
        let mut c = self.command_writer(permit)?;
        let tx = c.transaction()?;
        let previous: Option<Vec<u8>> = tx
            .query_row(
                "SELECT receipt FROM automation_mutations WHERE request_id=?1",
                [&receipt.request_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(b) = previous {
            let p: AutomationMutationReceipt = serde_json::from_slice(&b)?;
            if p.payload_hash != receipt.payload_hash {
                return Err(StoreError::AutomationConflict);
            }
            return Ok(p);
        }
        let current = get_record(&tx, &r.id)?;
        if current.as_ref().map_or(0, |r| r.revision) != expected
            || current
                .as_ref()
                .is_some_and(|p| p.owner_host_id != r.owner_host_id || p.deleted)
        {
            return Err(StoreError::AutomationConflict);
        }
        if current.is_none() {
            let count: u64 = tx.query_row(
                "SELECT count(*) FROM automations WHERE owner_host_id=?1 AND deleted=0",
                [&r.owner_host_id],
                |r| r.get(0),
            )?;
            if count >= 1000 {
                return Err(StoreError::AutomationLimit);
            }
        }
        if let Some(id) = cancel {
            let b: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT record FROM automation_runs WHERE id=?1 AND automation_id=?2",
                    params![id, r.id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(b) = b else {
                return Err(StoreError::AutomationConflict);
            };
            let mut run: AutomationRun = serde_json::from_slice(&b)?;
            if run.state != State::Claimed {
                return Err(StoreError::AutomationConflict);
            }
            run.state = State::Cancelled;
            run.updated_at_ms = receipt.committed_at_ms;
            run.detail = "Cancelled before dispatch admission".into();
            write_run(&tx, &run)?;
        }
        if let Some(id) = resolve_unknown {
            let b: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT record FROM automation_runs WHERE id=?1 AND automation_id=?2",
                    params![id, r.id],
                    |r| r.get(0),
                )
                .optional()?;
            let mut run: AutomationRun =
                serde_json::from_slice(&b.ok_or(StoreError::AutomationConflict)?)?;
            if run.state != State::Unknown {
                return Err(StoreError::AutomationConflict);
            }
            run.state = State::Resolved;
            run.updated_at_ms = receipt.committed_at_ms;
            run.detail = "Uncertain outcome acknowledged by the user; this occurrence was not resent. Future scheduled work is allowed.".into();
            write_run(&tx, &run)?;
        }
        write_record(&tx, r)?;
        // P5: the tombstone and its Activity retirement commit together. An
        // admitted run may still finish; tombstones are never projected again.
        let now = now_ms();
        if r.deleted && retire_deleted_automation_attention(&tx, now)? {
            bump_attention_revision(&tx, now)?;
        }
        tx.execute("INSERT INTO automation_mutations(request_id,automation_id,payload_hash,committed_at_ms,receipt) VALUES(?1,?2,?3,?4,?5)",params![receipt.request_id,r.id,receipt.payload_hash,receipt.committed_at_ms,serde_json::to_vec(receipt)?])?;
        tx.commit()?;
        self.notify_automation_change();
        Ok(receipt.clone())
    }
    /// The occurrence and next due slot commit together. No effects run under this lock.
    pub fn claim_automation(
        &self,
        expected: &AutomationRecord,
        next: &AutomationRecord,
        run: &AutomationRun,
        permit: Option<&WritePermit>,
    ) -> Result<bool, StoreError> {
        let mut c = self.command_writer(permit)?;
        let tx = c.transaction()?;
        let Some(current) = get_record(&tx, &expected.id)? else {
            return Ok(false);
        };
        if current != *expected || !current.enabled || current.deleted {
            return Ok(false);
        }
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM automation_runs WHERE id=?1)",
            [&run.id],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(false);
        }
        let busy: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM automation_runs WHERE automation_id=?1 AND blocks_next=1)",
            [&run.automation_id],
            |r| r.get(0),
        )?;
        let mut run = run.clone();
        if busy {
            run.state = State::Skipped;
            run.detail = "Previous occurrence is active or delivery remains unknown".into();
        }
        write_record(&tx, next)?;
        write_run(&tx, &run)?;
        // The monotonic due cursor prevents replay after pruning terminal history.
        tx.execute("DELETE FROM automation_runs WHERE automation_id=?1 AND blocks_next=0 AND id NOT IN (SELECT id FROM automation_runs WHERE automation_id=?1 ORDER BY id DESC LIMIT 1000)",[&run.automation_id])?;
        tx.commit()?;
        self.notify_automation_change();
        Ok(run.state == State::Claimed)
    }
    /// CAS prevents a late reconciliation from overwriting a real completion.
    pub fn update_automation_run(
        &self,
        r: &AutomationRun,
        expected: State,
        dispatch: bool,
        permit: Option<&WritePermit>,
    ) -> Result<bool, StoreError> {
        let mut c = self.command_writer(permit)?;
        let tx = c.transaction()?;
        let b: Option<Vec<u8>> = tx
            .query_row(
                "SELECT record FROM automation_runs WHERE id=?1",
                [&r.id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(b) = b else { return Ok(false) };
        let old: AutomationRun = serde_json::from_slice(&b)?;
        if old.state != expected {
            return Ok(false);
        }
        if dispatch
            && !get_record(&tx, &r.automation_id)?
                .is_some_and(|a| a.enabled && !a.deleted && a.revision == r.automation_revision)
        {
            return Ok(false);
        }
        write_run(&tx, r)?;
        tx.commit()?;
        self.notify_automation_change();
        Ok(true)
    }
    /// Called with the admitted native run's write ticket and exact user-message id.
    pub fn finish_automation_message(
        &self,
        task: &str,
        message: &str,
        state: State,
        detail: &str,
        now: i64,
        permit: Option<&WritePermit>,
    ) -> Result<(), StoreError> {
        let mut c = self.command_writer(permit)?;
        let tx = c.transaction()?;
        let b: Option<Vec<u8>> = tx
            .query_row(
                "SELECT record FROM automation_runs WHERE task_id=?1 AND message_id=?2",
                params![task, message],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(b) = b {
            let mut r: AutomationRun = serde_json::from_slice(&b)?;
            if matches!(
                r.state,
                State::Dispatching | State::Accepted | State::Unknown
            ) {
                r.state = state;
                r.detail = detail.into();
                r.updated_at_ms = now;
                write_run(&tx, &r)?;
                if let Some(mut automation) = get_record(&tx, &r.automation_id)?
                    && automation.revision == r.automation_revision
                    && automation.next_due_ms.is_none()
                    && state == State::Completed
                {
                    automation.completed = true;
                    automation.enabled = false;
                    automation.updated_at_ms = now;
                    write_record(&tx, &automation)?;
                }
            }
        }
        tx.commit()?;
        self.notify_automation_change();
        Ok(())
    }
}
