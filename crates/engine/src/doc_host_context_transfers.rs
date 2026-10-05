use super::*;
use crate::context_transfers as ledger;
use luna_proto::{ContextTransferStatus, RunRequest};

impl DocHost {
    /// Historical occupancy remains a conservative debit, but a window is
    /// current evidence only while the owning runtime and saved selection
    /// match. A cold engine or pending model change never certifies it.
    pub(crate) fn context_transfer_usage(
        &self,
        task: &str,
    ) -> Result<Option<luna_proto::ContextUsage>, EngineError> {
        let usage = self.open(task)?.doc.context_usage();
        let sessions = lock(&self.inner.sessions).clone();
        let matching = sessions
            .map(|sessions| sessions.execution_summary(task))
            .transpose()?
            .flatten()
            .is_some_and(|summary| {
                summary.running.is_some()
                    && !summary.pending.iter().any(|field| {
                        matches!(
                            field,
                            luna_proto::ExecutionSettingField::Provider
                                | luna_proto::ExecutionSettingField::Account
                                | luna_proto::ExecutionSettingField::Model
                                | luna_proto::ExecutionSettingField::ModelOptions
                        )
                    })
                    && matches!(
                        summary.phase,
                        luna_proto::ExecutionPhase::Warm | luna_proto::ExecutionPhase::Running
                    )
            });
        Ok(usage.map(|mut usage| {
            if !matching {
                usage.window = None;
            }
            usage
        }))
    }

    pub(crate) fn has_pending_context_transfer(&self, task: &str) -> Result<bool, EngineError> {
        Ok(ledger::records(&self.inner.store, task)?
            .iter()
            .any(|record| {
                record.receipt.target.task_id == task
                    && matches!(
                        record.receipt.status,
                        ContextTransferStatus::Planned | ContextTransferStatus::Unknown
                    )
            }))
    }

    pub(crate) fn has_portable_context_transfer(&self, task: &str) -> Result<bool, EngineError> {
        Ok(ledger::records(&self.inner.store, task)?
            .iter()
            .any(|record| {
                record.receipt.target.task_id == task
                    && record.receipt.kind == luna_proto::ContextTransferKind::PortableFork
                    && record.receipt.status != ContextTransferStatus::Failed
            }))
    }

    /// Prepare the exact captured text without changing delivery state. The
    /// transition gate held by dispatch serializes this with the next turn.
    pub(crate) fn context_transfer_prompt(
        &self,
        task: &str,
        request: &mut RunRequest,
    ) -> Result<Vec<String>, EngineError> {
        let records = ledger::records(&self.inner.store, task)?
            .into_iter()
            .filter(|record| {
                record.receipt.target.task_id == task
                    && matches!(
                        record.receipt.status,
                        ContextTransferStatus::Planned | ContextTransferStatus::Unknown
                    )
            })
            .collect::<Vec<_>>();
        if records.is_empty() {
            return Ok(Vec::new());
        }
        if records
            .iter()
            .any(|record| record.receipt.status == ContextTransferStatus::Unknown)
        {
            return Err(EngineError::Other("Context delivery is unknown and will not be reinjected. Check its receipt, or explicitly clear context before a fresh send.".into()));
        }
        if request.review.is_some() || request.goal.is_some() {
            return Err(EngineError::Other("Send the pending conversation context in an ordinary turn before starting a review or goal".into()));
        }
        let handle = self.open(task)?;
        let state = handle.doc.context_control();
        let current = self.inner.store.task_command_snapshot(task)?.snapshot;
        // The command snapshot is populated from current app binding authority;
        // a transfer committed before the first native start legitimately has
        // no binding yet.
        for record in &records {
            if record.receipt.target.binding_generation.is_some()
                && record.receipt.target.binding_generation != current.binding_generation
            {
                return Err(EngineError::Other("The destination binding changed after context was captured. Clear the pending transfer and preview again.".into()));
            }
            if record.receipt.target.context_key
                != ledger::context_key(
                    record.receipt.target.binding_id.as_deref(),
                    record.receipt.target.binding_generation,
                    &state,
                )?
            {
                return Err(EngineError::Other("The destination was compacted or rewound after the transfer. Clear it and refresh the preview.".into()));
            }
        }
        let allowance = ledger::budget(&request.prompt, self.context_transfer_usage(task)?)?;
        let total = records
            .iter()
            .map(|record| record.preview.text.len() as u64 + 2)
            .sum::<u64>();
        if total > allowance.max_bytes {
            return Err(EngineError::Other("Shared context and this prompt exceed the reserved context budget. Shorten the prompt or explicitly clear pending context.".into()));
        }
        let payload = records
            .iter()
            .map(|record| record.preview.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        request.prompt = format!("{payload}\n\n{}", request.prompt);
        Ok(records
            .into_iter()
            .map(|record| record.receipt.id)
            .collect())
    }

    pub(crate) fn mark_context_transfers_dispatched(
        &self,
        task: &str,
        message_id: &str,
        ids: &[String],
    ) -> Result<(), EngineError> {
        if ids.is_empty() {
            return Ok(());
        }
        let handle = self.open(task)?;
        let _projection = lock(&handle.queue_projection_lock);
        let mut records = Vec::new();
        for id in ids {
            let mut record = ledger::load(&self.inner.store, id)?
                .ok_or_else(|| EngineError::Other("Captured context is unavailable".into()))?;
            if record.receipt.target.task_id != task
                || record.receipt.status != ContextTransferStatus::Planned
            {
                return Err(EngineError::Other(
                    "Context transfer changed before dispatch; no reinjection was attempted".into(),
                ));
            }
            record.receipt.status = ContextTransferStatus::Unknown;
            record.receipt.correlation_id = Some(message_id.into());
            record.receipt.updated_at_ms = crate::now_ms();
            record.receipt.reason = Some("Dispatch is prepared; waiting for native acceptance. An uncertain outcome will not be replayed.".into());
            records.push(record);
        }
        self.persist_context_transfer_records(
            &handle,
            &format!("context-dispatch:{message_id}"),
            &records,
        )
    }

    /// Native Done is already journaled when this is called. Receipt and
    /// binding fences keep a late completion out of another execution.
    pub(crate) fn consume_context_transfers(
        &self,
        task: &str,
        run_id: &str,
    ) -> Result<(), EngineError> {
        let handle = self.open(task)?;
        let _projection = lock(&handle.queue_projection_lock);
        let current = self.inner.store.task_command_snapshot(task)?.snapshot;
        let mut records = Vec::new();
        for mut record in ledger::records(&self.inner.store, task)? {
            if record.receipt.target.task_id != task
                || record.receipt.status != ContextTransferStatus::Delivered
                || record.receipt.target.binding_generation != current.binding_generation
            {
                continue;
            }
            let Some(correlation) = record.receipt.correlation_id.as_deref() else {
                continue;
            };
            if !self
                .inner
                .store
                .task_command_receipt_for_correlation(task, correlation)?
                .is_some_and(|receipt| {
                    receipt.status == CommandReceiptStatus::Applied
                        && receipt.snapshot.active_turn_id.as_deref() == Some(run_id)
                })
            {
                continue;
            }
            record.receipt.status = ContextTransferStatus::Consumed;
            record.receipt.updated_at_ms = crate::now_ms();
            record.receipt.reason =
                Some("The provider completed a turn after accepting this context.".into());
            records.push(record);
        }
        if records.is_empty() {
            return Ok(());
        }
        self.persist_context_transfer_records(
            &handle,
            &format!("context-consumed:{run_id}"),
            &records,
        )
    }

    /// The caller holds the document projection lock through read and commit.
    fn persist_context_transfer_records(
        &self,
        handle: &ChatDocHandle,
        batch: &str,
        records: &[ledger::TransferRecord],
    ) -> Result<(), EngineError> {
        let prepared = self.prepare_queue_projection(handle, batch, |doc| {
            let mut state = doc.context_control();
            for record in records {
                ledger::project(&mut state, record.receipt.clone());
            }
            doc.set_context_control(&state)?;
            Ok(())
        })?;
        let records = records
            .iter()
            .map(|record| Ok((ledger::key(&record.receipt.id), serde_json::to_vec(record)?)))
            .collect::<Result<Vec<_>, EngineError>>()?;
        let record_refs = records
            .iter()
            .map(|(id, bytes)| (id.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>();
        let updates = prepared
            .batch_id
            .as_deref()
            .map(|id| vec![(id, prepared.update.as_slice())])
            .unwrap_or_default();
        self.inner.store.save_snapshot_with_local_records(
            &handle.chat_id,
            &prepared.snapshot,
            &updates,
            &prepared.authority_changes,
            &record_refs,
        )?;
        self.install_queue_projection(handle, prepared)?;
        handle.publish_messages();
        Ok(())
    }

    /// Called while the document projection lock is held. Its returned local
    /// snapshots join the native command-correlation transaction.
    pub(super) fn confirmed_context_transfer_records(
        &self,
        task: &str,
        correlation: &str,
        binding_id: Option<&str>,
        generation: Option<u32>,
        state: &ContextControlState,
    ) -> Result<Vec<ledger::TransferRecord>, EngineError> {
        let mut records = ledger::records(&self.inner.store, task)?
            .into_iter()
            .filter(|record| {
                record.receipt.target.task_id == task
                    && record.receipt.status == ContextTransferStatus::Unknown
                    && record.receipt.correlation_id.as_deref() == Some(correlation)
                    && record
                        .receipt
                        .target
                        .binding_generation
                        .is_none_or(|expected| Some(expected) == generation)
            })
            .collect::<Vec<_>>();
        for record in &mut records {
            record.receipt.status = ContextTransferStatus::Delivered;
            record.receipt.updated_at_ms = crate::now_ms();
            record.receipt.reason =
                Some("The provider confirmed the correlated destination turn.".into());
            record.receipt.target.binding_id = binding_id.map(str::to_owned);
            record.receipt.target.binding_generation = generation;
            record.receipt.target.context_key = ledger::context_key(binding_id, generation, state)?;
            record.target_native_id = self
                .workspace()
                .and_then(|workspace| workspace.chat(task).ok().flatten())
                .and_then(|chat| chat.harness_session_id);
        }
        Ok(records)
    }
}
