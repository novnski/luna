use super::*;
use crate::context_transfers as ledger;
use luna_proto::{
    ContextTransferBasis, ContextTransferKind, ContextTransferPreview, ContextTransferRequest,
    ContextTransferStatus,
};

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::PREVIEW_CONTEXT_TRANSFER
            | methods::LIST_CONTEXT_TRANSFERS
            | methods::GET_CONTEXT_TRANSFER
            | methods::READ_CONTEXT_TRANSFER_ITEMS
    )
}

impl EngineRpc {
    fn new_context_transfer_target(
        &self,
        request: &ContextTransferRequest,
        source: &luna_proto::Chat,
    ) -> Result<luna_proto::Chat, crate::EngineError> {
        let new = request.new_target.as_ref().ok_or_else(|| {
            crate::EngineError::Other("New destination settings are missing".into())
        })?;
        if request.kind != ContextTransferKind::PortableFork
            || request.target_config.is_some()
            || uuid::Uuid::parse_str(&request.target_task_id).is_err()
            || source.space_id.is_none()
            || self.workspace.chat(&request.target_task_id)?.is_some()
            || self.task_identity.view(&request.target_task_id).is_some()
        {
            return Err(crate::EngineError::Other("A portable new task requires an unused UUID and the source's project; existing or deleted tasks cannot be replaced".into()));
        }
        let mut target = source.clone();
        target.id = request.target_task_id.clone();
        target.title = Some(
            new.title
                .as_deref()
                .unwrap_or("New task with context")
                .chars()
                .take(160)
                .collect(),
        );
        target.config = Some(new.config.clone());
        target.child_thread = None;
        target.archived = false;
        target.settlement_override = None;
        target.pinned_order = None;
        target.last_message_preview = None;
        target.last_message_at = None;
        target.created_at = chrono::Utc::now();
        target.harness_session_id = None;
        target.harness_session_cwd = None;
        // The source's run-start repository context is its own evidence. A
        // fresh task that has never run must not claim that branch (often
        // merged by the time past work is handed off) or its pull request;
        // its first dispatch stamps the checkout it actually runs in.
        target.source_context = None;
        target.room_gen = Some(2);
        target.last_seen_at = None;
        target.manually_unread = false;
        Ok(target)
    }

    fn context_transfer_task(&self, task_id: &str) -> Result<luna_proto::Chat, crate::EngineError> {
        if !self.workspace.is_host(task_id) || self.derived_fork_document(task_id) {
            return Err(crate::EngineError::Other(
                "Context transfer requires an ordinary conversation on its owning device".into(),
            ));
        }
        self.workspace
            .chat(task_id)?
            .ok_or_else(|| crate::EngineError::Other("The conversation is unavailable".into()))
    }

    fn context_transfer_basis(
        &self,
        chat: &luna_proto::Chat,
        entries: &[luna_doc::SessionMessageEntry],
    ) -> Result<ContextTransferBasis, crate::EngineError> {
        let binding = self
            .task_identity
            .view(&chat.id)
            .and_then(|identity| identity.current_binding);
        let state = self.doc_host.context_control(&chat.id)?;
        let snapshot = self.doc_host.task_command_snapshot(&chat.id)?.snapshot;
        Ok(ContextTransferBasis {
            task_id: chat.id.clone(),
            title: chat.title.clone().unwrap_or_else(|| "Untitled chat".into()),
            provider: chat
                .config
                .as_ref()
                .ok_or_else(|| {
                    crate::EngineError::Other(
                        "Choose a provider before transferring context".into(),
                    )
                })?
                .harness,
            task_revision: snapshot.task_revision,
            binding_id: binding.as_ref().map(|binding| binding.binding_id.clone()),
            binding_generation: binding.as_ref().map(|binding| binding.generation),
            context_key: ledger::context_key(
                binding.as_ref().map(|binding| binding.binding_id.as_str()),
                binding.as_ref().map(|binding| binding.generation),
                &state,
            )?,
            history_hash: ledger::history_hash(entries)?,
        })
    }

    pub(super) fn preview_context_transfer(
        &self,
        request: ContextTransferRequest,
    ) -> Result<ContextTransferPreview, crate::EngineError> {
        let source = self.context_transfer_task(&request.source_task_id)?;
        // A portable copy is a context-carrying conversation: never from a
        // side chat (chat-model §1). Handoff and send-back stay available.
        if request.kind == ContextTransferKind::PortableFork {
            self.board_guard_legacy_creation(
                &source.id,
                luna_proto::BoardChatCreationKind::SideWithContext,
            )
            .map_err(|error| crate::EngineError::Other(Self::board_error_text(&error)))?;
        }
        let new_target = request.new_target.is_some();
        let target = if new_target {
            self.new_context_transfer_target(&request, &source)?
        } else {
            self.context_transfer_task(&request.target_task_id)?
        };
        let source_doc = self.doc_host.open(&source.id)?;
        let target_doc = if new_target {
            None
        } else {
            Some(self.doc_host.open(&target.id)?)
        };
        let source_entries = source_doc.doc().read_entries()?;
        let target_entries = target_doc
            .as_ref()
            .map(|doc| doc.doc().read_entries())
            .transpose()?
            .unwrap_or_default();
        let source_basis = self.context_transfer_basis(&source, &source_entries)?;
        let target_basis = if new_target {
            ContextTransferBasis {
                task_id: target.id.clone(),
                title: target.title.clone().unwrap_or_default(),
                provider: target.config.as_ref().unwrap().harness,
                task_revision: self
                    .doc_host
                    .task_command_snapshot(&target.id)?
                    .snapshot
                    .task_revision,
                binding_id: None,
                binding_generation: None,
                context_key: ledger::context_key(None, None, &ContextControlState::default())?,
                history_hash: ledger::history_hash(&[])?,
            }
        } else {
            self.context_transfer_basis(&target, &target_entries)?
        };
        match request.kind {
            ContextTransferKind::ProviderHandoff => {
                if source.id != target.id
                    || request
                        .target_config
                        .as_ref()
                        .is_none_or(|config| config.harness == target_basis.provider)
                {
                    return Err(crate::EngineError::Other("A provider handoff requires the same conversation and a different provider".into()));
                }
            }
            ContextTransferKind::PortableFork => {
                let binding = self
                    .task_identity
                    .view(&target.id)
                    .and_then(|identity| identity.current_binding);
                if source.id == target.id
                    || target.child_thread.is_some()
                    || !ledger::visible(&target_entries).is_empty()
                    || binding.is_some_and(|binding| !binding.provider_task_id.is_empty())
                    || self.sessions.turn_in_flight(&target.id)
                {
                    return Err(crate::EngineError::Other("Portable context requires a fresh ordinary or fork conversation; delegated child chats keep their own context contract".into()));
                }
                if request.target_config.is_some() {
                    return Err(crate::EngineError::Other(
                        "Select the fresh destination's provider before previewing context".into(),
                    ));
                }
            }
            ContextTransferKind::ForkMerge => {
                let lineage = self
                    .task_identity
                    .view(&source.id)
                    .map(|identity| identity.task.lineage)
                    .unwrap_or_default();
                let origin = lineage
                    .source_task_id
                    .or(lineage.parent_task_id)
                    .or_else(|| self.workspace.side_chat_parent(&source.id));
                if source.id == target.id
                    || origin.as_deref() != Some(target.id.as_str())
                    || request.target_config.is_some()
                {
                    return Err(crate::EngineError::Other(
                        "Send findings back targets this conversation's recorded source or parent"
                            .into(),
                    ));
                }
            }
        }
        let state = target_doc
            .as_ref()
            .map(|doc| doc.doc().context_control())
            .unwrap_or_default();
        if state.pending_rewind.is_some() || state.compact.blocks_new_turn() {
            return Err(crate::EngineError::Other(
                "Resolve the destination's pending history operation before sharing context".into(),
            ));
        }
        let prior = ledger::records(&self.store, &target.id)?;
        let incoming = prior
            .iter()
            .filter(|record| record.receipt.target.task_id == target.id)
            .collect::<Vec<_>>();
        if incoming
            .iter()
            .any(|record| record.receipt.status == ContextTransferStatus::Unknown)
        {
            return Err(crate::EngineError::Other("A context transfer has unknown delivery. Check its receipt or explicitly clear context before planning another transfer.".into()));
        }
        if incoming.iter().any(|record| {
            record.receipt.status == ContextTransferStatus::Planned
                && record.receipt.source.task_id == source.id
        }) {
            return Err(crate::EngineError::Other("Context from this source is already waiting for the destination's next send. Deliver or explicitly discard it before capturing another copy.".into()));
        }
        let usage = if request.kind != ContextTransferKind::ProviderHandoff && !new_target {
            self.doc_host.context_transfer_usage(&target.id)?
        } else {
            None
        };
        let mut budget = ledger::budget(&request.prompt_text, usage)?;
        let pending_bytes: u64 = incoming
            .iter()
            .filter(|record| record.receipt.status == ContextTransferStatus::Planned)
            .map(|record| record.receipt.bytes + 2)
            .sum();
        budget.max_bytes = budget.max_bytes.saturating_sub(pending_bytes);
        if budget.max_bytes <= 1024 {
            return Err(crate::EngineError::Other("Pending shared context already uses the available budget. Send or discard it before adding more.".into()));
        }
        ledger::plan(
            request,
            source_basis,
            target_basis,
            &source_entries,
            budget,
            &prior,
        )
    }

    pub(super) fn apply_context_transfer(
        &self,
        command_id: &str,
        task_id: &str,
        request: ContextTransferRequest,
        preview_id: &str,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        let _gate = ledger::APPLY_GATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if task_id != request.target_task_id {
            return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                "Transfer command targets a different conversation".into(),
            )));
        }
        if let Some(existing) =
            ledger::load(&self.store, preview_id).map_err(TaskProjectionError::Mutation)?
        {
            if existing.preview.request != request || existing.receipt.target.task_id != task_id {
                return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                    "Transfer identity was reused with a different selection".into(),
                )));
            }
            let (_, receipt) = self.commit_task_projection(command_id, task_id, |_| Ok(()))?;
            return Ok(Some(receipt));
        }
        let preview = self
            .preview_context_transfer(request.clone())
            .map_err(TaskProjectionError::Mutation)?;
        if preview.preview_id != preview_id {
            let snapshot = self
                .doc_host
                .task_command_snapshot(task_id)
                .map_err(TaskProjectionError::Mutation)?
                .snapshot;
            return self
                .doc_host
                .complete_task_command(
                    command_id,
                    CommandReceiptStatus::Conflict,
                    false,
                    snapshot.binding_id.as_deref(),
                    snapshot.binding_generation,
                    snapshot.active_turn_id.as_deref(),
                    Some(CommandReceiptReason {
                        code: CommandReasonCode::StaleTaskRevision,
                        message: "Conversation context changed after the preview".into(),
                    }),
                    Some("Refresh the preview. The draft and conversations have not been changed."),
                )
                .map_err(TaskProjectionError::Commit);
        }
        let source_native = self
            .task_identity
            .view(&request.source_task_id)
            .and_then(|identity| identity.current_binding)
            .map(|binding| binding.provider_task_id)
            .filter(|id| !id.is_empty());
        let target_native = self
            .task_identity
            .view(task_id)
            .and_then(|identity| identity.current_binding)
            .map(|binding| binding.provider_task_id)
            .filter(|id| !id.is_empty());
        let record = ledger::new_record(preview, source_native, target_native);
        if request.new_target.is_some() {
            let source = self
                .context_transfer_task(&request.source_task_id)
                .map_err(TaskProjectionError::Mutation)?;
            let target = self
                .new_context_transfer_target(&request, &source)
                .map_err(TaskProjectionError::Mutation)?;
            let snapshots = ledger::snapshots_for_new(&self.store, &record)
                .map_err(TaskProjectionError::Mutation)?;
            let snapshot_refs = snapshots
                .iter()
                .map(|(id, bytes)| (id.as_str(), bytes.as_slice()))
                .collect::<Vec<_>>();
            let receipt = record.receipt.clone();
            let lineage = TaskLineage {
                source_task_id: Some(source.id.clone()),
                lifecycle_owner_task_id: Some(target.id.clone()),
                ..Default::default()
            };
            let (_, committed) = self.task_identity.register_new_task_with_command(task_id, target.space_id.as_deref(), Some(&target.device_id), target.cwd.as_deref(), lineage,
                |identity_snapshot| self.doc_host.commit_context_boundary(task_id, command_id, None, None, identity_snapshot, None, &snapshot_refs, &[],
                    |doc| {
                        let mut state = doc.context_control(); ledger::project(&mut state, receipt.clone());
                        state.notice = Some("Captured visible context is ready. This independent task starts only when you send a message.".into());
                        doc.set_context_control(&state)?; self.insert_context_transfer_marker(doc, &receipt)?; Ok(())
                    }, |registry| {
                        if registry.chat(task_id)?.is_some() { return Err(crate::EngineError::Other("The new task ID was claimed before context creation".into())); }
                        let current_source = registry.chat(&source.id)?.ok_or_else(|| crate::EngineError::Other("The source disappeared before context creation".into()))?;
                        if current_source.cwd != source.cwd || current_source.space_id != source.space_id || current_source.device_id != source.device_id || current_source.config != source.config {
                            return Err(crate::EngineError::Other("The source's execution settings changed; refresh the context preview".into()));
                        }
                        registry.upsert_chat(&target)?; Ok(())
                    }))?;
            return Ok(Some(committed));
        }
        if request.kind == ContextTransferKind::ProviderHandoff {
            return self.apply_context_boundary_with_transfer(
                command_id,
                task_id,
                request.target_config.ok_or_else(|| {
                    TaskProjectionError::Mutation(crate::EngineError::Other(
                        "Provider selection is missing".into(),
                    ))
                })?,
                ContextBoundaryKind::ProviderSwitched,
                None,
                false,
                Some(record),
            );
        }
        let snapshots = ledger::snapshots_for_new(&self.store, &record)
            .map_err(TaskProjectionError::Mutation)?;
        let snapshot_refs = snapshots
            .iter()
            .map(|(id, bytes)| (id.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>();
        let binding_id = record.receipt.target.binding_id.clone();
        let generation = record.receipt.target.binding_generation;
        let receipt = record.receipt.clone();
        let (_, committed) = self.doc_host.commit_context_boundary(task_id, command_id, binding_id.as_deref(), generation,
            None, None, &snapshot_refs, &[], move |doc| {
                let mut state = doc.context_control();
                ledger::project(&mut state, receipt.clone());
                state.notice = Some("Shared conversation context is ready for the next explicit send. Files and Git changes were not merged.".into());
                doc.set_context_control(&state)?;
                self.insert_context_transfer_marker(doc, &receipt)?;
                Ok(())
            }, |_| Ok(()))?;
        Ok(Some(committed))
    }

    pub(super) fn insert_context_transfer_marker(
        &self,
        doc: &luna_doc::SessionDoc,
        receipt: &luna_proto::ContextTransferReceipt,
    ) -> Result<(), crate::EngineError> {
        let id = format!("context-transfer:{}", receipt.id);
        if doc.read_entries()?.iter().any(|entry| entry.id == id) {
            return Ok(());
        }
        doc.push_message(&luna_doc::SessionMessageEntry { id, role: luna_doc::MessageRole::System,
            parts: vec![luna_doc::MessagePart::Text { id: "context-transfer".into(), text: format!("Conversation context from {} · {} visible messages · {} omitted. Ready for the next send; files and Git changes are unchanged.", receipt.source.title, receipt.included_message_ids.len(), receipt.omitted_message_ids.len()) }],
            created_at: crate::now_ms(), device_id: self.engine_info.device_id.clone(), status: Some(luna_doc::MessageStatus::Complete), continuation_of: None })?;
        Ok(())
    }

    pub(super) fn handle_context_transfer(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        if params
            .get("targetDeviceId")
            .and_then(|value| value.as_str())
            .is_some_and(|id| id != self.engine_info.device_id)
        {
            return Err(RpcError::Failed(
                "Open the conversations on their owning device to transfer context".into(),
            ));
        }
        let fail = |error: crate::EngineError| RpcError::Failed(error.to_string());
        if method == methods::PREVIEW_CONTEXT_TRANSFER {
            return RpcReply::value(
                &self
                    .preview_context_transfer(parse_params(params)?)
                    .map_err(fail)?,
            );
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Read {
            task_id: String,
            #[serde(default)]
            transfer_id: String,
            #[serde(default)]
            offset: usize,
            #[serde(default)]
            limit: Option<usize>,
            #[serde(default)]
            before: Option<String>,
            #[serde(default)]
            message_id: Option<String>,
            #[serde(default)]
            text_offset: Option<usize>,
        }
        let p: Read = parse_params(params)?;
        self.context_transfer_task(&p.task_id).map_err(fail)?;
        let mut records = ledger::records(&self.store, &p.task_id).map_err(fail)?;
        if method == methods::LIST_CONTEXT_TRANSFERS {
            records.sort_by_key(|record| {
                (
                    std::cmp::Reverse(record.receipt.created_at_ms),
                    record.receipt.id.clone(),
                )
            });
            let start = p
                .before
                .as_deref()
                .map(|id| {
                    records
                        .iter()
                        .position(|record| record.receipt.id == id)
                        .map(|index| index + 1)
                        .ok_or_else(|| RpcError::BadParams("Transfer cursor is unavailable".into()))
                })
                .transpose()?
                .unwrap_or(0);
            let limit = p.limit.unwrap_or(50).clamp(1, 100);
            let items: Vec<_> = records
                .iter()
                .skip(start)
                .take(limit)
                .map(|record| record.receipt.clone())
                .collect();
            let before = (start + items.len() < records.len())
                .then(|| items.last().map(|receipt| receipt.id.clone()))
                .flatten();
            return RpcReply::value(&serde_json::json!({"items":items,"before":before}));
        }
        let record = records
            .into_iter()
            .find(|record| record.receipt.id == p.transfer_id)
            .ok_or_else(|| {
                RpcError::BadParams("Transfer not found for this conversation".into())
            })?;
        if method == methods::GET_CONTEXT_TRANSFER {
            return RpcReply::value(
                &serde_json::json!({"receipt":record.receipt,"preview":record.preview}),
            );
        }
        let source = self
            .context_transfer_task(&record.receipt.source.task_id)
            .ok();
        let visible = source
            .as_ref()
            .and_then(|source| self.doc_host.open(&source.id).ok())
            .and_then(|doc| doc.doc().read_entries().ok())
            .map(|entries| ledger::visible(&entries))
            .unwrap_or_default();
        let limit = p.limit.unwrap_or(20).clamp(1, 50);
        let items: Vec<_> = record.preview.omitted.iter().filter(|item| p.message_id.as_ref().is_none_or(|id| *id == item.message_id)).skip(if p.message_id.is_some() {0} else {p.offset}).take(limit).map(|item| {
            let current = visible.iter().find(|current| current.message_id == item.message_id && current.content_hash == item.content_hash);
            let range = current.and_then(|current| {
                let start = p.text_offset.unwrap_or(item.start_byte as usize).max(item.start_byte as usize);
                if start > current.text.len() || !current.text.is_char_boundary(start) { return None; }
                let mut end = (start + 12_288).min(current.text.len());
                while !current.text.is_char_boundary(end) { end -= 1; }
                Some((start,end,current.text[start..end].to_string(),end<current.text.len()))
            });
            serde_json::json!({"messageId":item.message_id,"role":item.role,"available":range.is_some(),"text":range.as_ref().map(|range| &range.2),"startByte":range.as_ref().map(|range|range.0),"endByte":range.as_ref().map(|range|range.1),"truncated":range.as_ref().is_some_and(|range|range.3),"nextTextOffset":range.as_ref().filter(|range|range.3).map(|range|range.1),"reason":if range.is_some(){item.reason.as_str()}else{"Source text was deleted, rewritten or the requested cursor is unavailable"}})
        }).collect();
        let next = p.offset.saturating_add(items.len());
        RpcReply::value(
            &serde_json::json!({"items":items,"nextOffset":(next<record.preview.omitted.len()).then_some(next)}),
        )
    }
}
