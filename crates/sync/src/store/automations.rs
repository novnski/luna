//! Atomic host-local automation metadata and no-replay occurrence claims.
use super::*;
use luna_proto::{
    AutomationMutationReceipt, AutomationRecord, AutomationRun, AutomationRunState as State,
};

/// Minimal producer metadata; prompts and provider configuration are not read.
pub struct AutomationAttentionSource {
    pub automation_id: String,
    pub origin_task_id: String,
    pub revision: u64,
    pub updated_at_ms: i64,
    pub watch_unknown: bool,
    pub run: Option<AutomationRun>,
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
impl DocsStore {
    pub fn automation_attention_sources(
        &self,
        host: &str,
    ) -> Result<Vec<AutomationAttentionSource>, StoreError> {
        let c = self.conn();
        let mut q = c.prepare("SELECT a.id,a.origin_task_id,a.revision,json_extract(CAST(a.record AS TEXT),'$.updatedAtMs'),json_extract(CAST(a.record AS TEXT),'$.watch.lastError') IS NOT NULL,(SELECT r.record FROM automation_runs r WHERE r.automation_id=a.id ORDER BY r.blocks_next DESC,r.id DESC LIMIT 1) FROM automations a WHERE a.owner_host_id=?1 AND a.deleted=0 ORDER BY a.id LIMIT 1000")?;
        let rows = q
            .query_map([host], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, Option<Vec<u8>>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(automation_id, origin_task_id, revision, updated_at_ms, watch_unknown, run)| {
                    Ok(AutomationAttentionSource {
                        automation_id,
                        origin_task_id,
                        revision,
                        updated_at_ms,
                        watch_unknown,
                        run: run.map(|b| serde_json::from_slice(&b)).transpose()?,
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
        task: Option<&str>,
        after: Option<&str>,
        deleted: bool,
        limit: usize,
    ) -> Result<Vec<AutomationRecord>, StoreError> {
        let c = self.conn();
        let mut q=c.prepare("SELECT record FROM automations WHERE owner_host_id=?1 AND (?2 IS NULL OR origin_task_id=?2) AND (?3 IS NULL OR id>?3) AND (?4 OR deleted=0) ORDER BY id LIMIT ?5")?;
        let rows = q
            .query_map(
                params![host, task, after, deleted, limit.clamp(1, 101)],
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
    pub fn mutate_automation(
        &self,
        expected: u64,
        r: &AutomationRecord,
        receipt: &AutomationMutationReceipt,
        cancel: Option<&str>,
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
        write_record(&tx, r)?;
        tx.execute("INSERT INTO automation_mutations(request_id,automation_id,payload_hash,committed_at_ms,receipt) VALUES(?1,?2,?3,?4,?5)",params![receipt.request_id,r.id,receipt.payload_hash,receipt.committed_at_ms,serde_json::to_vec(receipt)?])?;
        tx.commit()?;
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
            }
        }
        tx.commit()?;
        Ok(())
    }
}
