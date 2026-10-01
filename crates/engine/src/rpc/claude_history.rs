//! CC-10 Claude same-session rewind and native file checkpoints.
//!
//! Provider-backed forks, earlier-message edits and recovery restores reuse
//! the shared `fork_provider_history` path. This module adds the two Claude
//! operations that are not forks: `rewind_conversation` on the same native
//! session (reusing the shared pending-rewind presentation, send blocker and
//! Reconcile action) and the CLI's own file checkpoints, which stay separate
//! from conversation rewind and from LUNA's Git checkpoints.

use super::*;
use luna_harness::{
    NativeFileRewindRequest, NativeFileRewindResult, NativeHistoryScope, NativeHistoryTurns,
    NativeRewindOutcome, NativeRewindProgress, NativeRewindRequest,
};
use luna_proto::NativeFileRewindView;
use sha2::Digest as _;

const CLAUDE_REWIND_PREFIX: &str = "luna-claude-rewind-v1";
const NATIVE_TIMEOUT: Duration = Duration::from_secs(30);
const NATIVE_FILE_LIMITATIONS: &str = "Claude restores only files its own Edit, Write and NotebookEdit tools changed in this session. Bash commands, edits outside Claude and most subagent edits are not restored. LUNA Git checkpoints stay separate.";

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::PREVIEW_NATIVE_FILE_REWIND | methods::APPLY_NATIVE_FILE_REWIND
    )
}

/// Durable intent written before the native request, so a restart or an
/// uncertain outcome can be reconciled by readback without resending it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeRewindRecord {
    command_id: String,
    task_id: String,
    binding_id: String,
    binding_generation: u32,
    provider_account_id: Option<String>,
    provider_home: Option<String>,
    cwd: String,
    session_id: String,
    message_id: String,
    target_checkpoint: String,
    last_seen_checkpoint: String,
    original_turns: Vec<String>,
    retained_turns: Vec<String>,
    entries: Vec<SessionMessageEntry>,
    checkpoints: HashMap<String, String>,
}

fn record_id(task_id: &str, command_id: &str) -> String {
    format!("{CLAUDE_REWIND_PREFIX}:{task_id}:{command_id}")
}

fn other(message: impl Into<String>) -> crate::EngineError {
    crate::EngineError::Other(message.into())
}

fn mutation(message: impl Into<String>) -> TaskProjectionError {
    TaskProjectionError::Mutation(other(message))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileRewindParams {
    task_id: String,
    message_id: String,
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    target_device_id: Option<String>,
}

fn file_view(
    params: &FileRewindParams,
    checkpoint: &str,
    result: &NativeFileRewindResult,
    dry_run: bool,
) -> NativeFileRewindView {
    let mut files = result.files_changed.clone();
    files.sort();
    let digest = Sha256::digest(
        serde_json::json!({
            "checkpoint": checkpoint, "canRewind": result.can_rewind, "error": result.error,
            "files": files, "insertions": result.insertions, "deletions": result.deletions,
        })
        .to_string()
        .as_bytes(),
    );
    NativeFileRewindView {
        task_id: params.task_id.clone(),
        message_id: params.message_id.clone(),
        provider: Some(HarnessId::ClaudeCode),
        dry_run,
        can_rewind: result.can_rewind,
        error: result.error.clone(),
        files_changed: files,
        insertions: result.insertions,
        deletions: result.deletions,
        digest: format!("{digest:x}"),
        limitations: NATIVE_FILE_LIMITATIONS.into(),
    }
}

impl EngineRpc {
    fn claude_scope(
        &self,
        chat: &Chat,
        binding: &luna_proto::ExecutionBinding,
    ) -> NativeHistoryScope {
        NativeHistoryScope {
            session_id: binding.provider_task_id.clone(),
            cwd: chat.cwd.clone().unwrap_or_else(|| ".".into()),
            provider_home: Some(
                self.agent_accounts
                    .skill_config_dirs()
                    .0
                    .to_string_lossy()
                    .into_owned(),
            ),
        }
    }

    async fn claude_turns(
        &self,
        scope: NativeHistoryScope,
    ) -> Result<NativeHistoryTurns, crate::EngineError> {
        let harness = self.registry.resolve(HarnessId::ClaudeCode)?;
        tokio::time::timeout(NATIVE_TIMEOUT, harness.native_history_turns(scope))
            .await
            .map_err(|_| other("Claude history readback timed out"))?
            .map_err(crate::EngineError::from)
    }

    fn claude_rewind_ready(
        &self,
        task_id: &str,
    ) -> Result<(Chat, luna_proto::ExecutionBinding), crate::EngineError> {
        let chat = self
            .workspace
            .chat(task_id)?
            .ok_or_else(|| other("rewind task no longer exists"))?;
        if chat.archived || self.sessions.active_turn_id(task_id).is_some() {
            return Err(other(
                "wait for an idle, unarchived task before rewind reconciliation",
            ));
        }
        let binding = self
            .task_identity
            .view(task_id)
            .and_then(|view| view.current_binding)
            .ok_or_else(|| other("rewind task has no binding"))?;
        if binding.provider_id != HarnessId::ClaudeCode
            || chat.config.as_ref().map(|config| config.harness) != Some(HarnessId::ClaudeCode)
        {
            return Err(other("rewind task provider changed"));
        }
        Ok((chat, binding))
    }

    fn clear_claude_rewind(
        &self,
        task_id: &str,
        command_id: &str,
        notice: String,
    ) -> Result<(), TaskProjectionError> {
        let mut context = self
            .doc_host
            .context_control(task_id)
            .map_err(TaskProjectionError::Mutation)?;
        context.pending_rewind = None;
        context.notice = Some(notice);
        self.doc_host
            .persist_context_control(task_id, command_id, &context)
            .map_err(TaskProjectionError::Mutation)
    }

    /// Same-chat rewind on the same Claude session. The caller has already
    /// required Keep files and a ready history operation.
    pub(super) async fn rewind_claude_conversation(
        &self,
        command_id: &str,
        task_id: &str,
        message_id: String,
        review: HistoryReviewToken,
        chat: Chat,
        binding: luna_proto::ExecutionBinding,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        self.validate_history_review(task_id, &chat, &binding, &review)
            .map_err(TaskProjectionError::Mutation)?;
        if review.message_id.as_deref() != Some(message_id.as_str()) {
            return Err(mutation("rewind review selected a different message"));
        }
        // A parked warm Claude process holds the pre-rewind conversation in
        // memory; retire it and keep dispatch excluded until this finishes.
        let _rewind_transition = self
            .sessions
            .prepare_native_rewind(task_id)
            .await
            .map_err(TaskProjectionError::Mutation)?;
        let handle = self
            .doc_host
            .open(task_id)
            .map_err(TaskProjectionError::Mutation)?;
        let plan = plan_history(
            handle.doc(),
            HarnessId::ClaudeCode,
            ConversationForkBoundaryKind::BeforeUserMessage,
            Some(&message_id),
        )
        .map_err(TaskProjectionError::Mutation)?;
        let luna_proto::ProviderForkBoundary::BeforeCheckpoint {
            checkpoint_id: target,
        } = plan.provider_boundary.clone()
        else {
            return Err(mutation("rewind has no exact before-turn boundary"));
        };
        let scope = self.claude_scope(&chat, &binding);
        let native = self
            .claude_turns(scope.clone())
            .await
            .map_err(TaskProjectionError::Mutation)?;
        if native.compacted {
            return Err(mutation(
                "Claude compacted this session, so earlier turns cannot be rewound exactly. Start a new task with context instead.",
            ));
        }
        let entries = handle
            .doc()
            .read_entries()
            .map_err(crate::EngineError::from)
            .map_err(TaskProjectionError::Mutation)?;
        // Every completed LUNA turn must be the same ordered native turn.
        // External terminal turns, native wakes and imported history refuse.
        let document_turns = entries
            .iter()
            .filter(|entry| entry.role == MessageRole::Assistant)
            .map(|entry| handle.doc().provider_checkpoint(&entry.id))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| mutation("LUNA history lacks exact Claude turn identities"))?;
        if document_turns != native.turn_starts {
            return Err(mutation(
                "Claude and LUNA turn history differ; rewind was not sent",
            ));
        }
        let position = native
            .turn_starts
            .iter()
            .position(|id| id == &target)
            .ok_or_else(|| mutation("the selected turn is not in Claude's active history"))?;
        let cutoff = entries
            .iter()
            .position(|entry| entry.id == message_id)
            .ok_or_else(|| mutation("rewind message vanished"))?;
        let retained_checkpoints = entries[..cutoff]
            .iter()
            .filter_map(|entry| {
                handle
                    .doc()
                    .provider_checkpoint(&entry.id)
                    .map(|id| (entry.id.clone(), id))
            })
            .collect::<HashMap<_, _>>();
        let retained_turns = entries[..cutoff]
            .iter()
            .filter(|entry| entry.role == MessageRole::Assistant)
            .filter_map(|entry| retained_checkpoints.get(&entry.id).cloned())
            .collect::<Vec<_>>();
        if retained_turns[..] != native.turn_starts[..position] {
            return Err(mutation(
                "rewind retained turns differ from Claude's native prefix",
            ));
        }
        let last_seen = native
            .turn_starts
            .last()
            .cloned()
            .unwrap_or_else(|| target.clone());
        // Re-check the reviewed task after asynchronous preflight.
        let (current_chat, current_binding) = self
            .ensure_history_operation_ready(task_id)
            .map_err(TaskProjectionError::Mutation)?;
        self.validate_history_review(task_id, &current_chat, &current_binding, &review)
            .map_err(TaskProjectionError::Mutation)?;
        if current_binding.binding_id != binding.binding_id
            || current_binding.provider_task_id != binding.provider_task_id
        {
            return Err(mutation("rewind binding changed during preflight"));
        }
        if self.sessions.has_live_run(task_id) {
            return Err(mutation(
                "a provider runtime started during rewind preflight; rewind was not sent",
            ));
        }
        let snapshot = handle
            .doc()
            .export_snapshot()
            .map_err(crate::EngineError::from)
            .map_err(TaskProjectionError::Mutation)?;
        if SessionDoc::from_snapshot(&snapshot)
            .map_err(crate::EngineError::from)
            .map_err(TaskProjectionError::Mutation)?
            .read_entries()
            .map_err(crate::EngineError::from)
            .map_err(TaskProjectionError::Mutation)?
            != entries
        {
            return Err(mutation("LUNA transcript changed during rewind preflight"));
        }
        let record = ClaudeRewindRecord {
            command_id: command_id.into(),
            task_id: task_id.into(),
            binding_id: binding.binding_id.clone(),
            binding_generation: binding.generation,
            provider_account_id: chat
                .config
                .as_ref()
                .and_then(|config| config.provider_account_id.clone()),
            provider_home: scope.provider_home.clone(),
            cwd: scope.cwd.clone(),
            session_id: binding.provider_task_id.clone(),
            message_id,
            target_checkpoint: target.clone(),
            last_seen_checkpoint: last_seen.clone(),
            original_turns: native.turn_starts.clone(),
            retained_turns,
            entries: entries[..cutoff].to_vec(),
            checkpoints: retained_checkpoints,
        };
        let id = record_id(task_id, command_id);
        let bytes = serde_json::to_vec(&record)
            .map_err(|error| mutation(format!("rewind intent: {error}")))?;
        // Both writes complete before the send blocker and the native request.
        self.store
            .save_snapshot(&format!("{id}:transcript"), &snapshot)
            .and_then(|()| self.store.save_snapshot(&id, &bytes))
            .map_err(|error| TaskProjectionError::Mutation(error.into()))?;
        let mut context = handle.doc().context_control();
        context.pending_rewind = Some(command_id.into());
        context.notice = Some("Claude conversation rewind is awaiting readback. Later history remains visible; new turns are blocked. Use Reconcile rewind after a restart or uncertain result.".into());
        self.doc_host
            .persist_context_control(task_id, command_id, &context)
            .map_err(TaskProjectionError::Mutation)?;

        let harness = self
            .registry
            .resolve(HarnessId::ClaudeCode)
            .map_err(|error| TaskProjectionError::Mutation(error.into()))?;
        let written = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tracked = written.clone();
        let progress = move |event: NativeRewindProgress| {
            let NativeRewindProgress::RequestWritten = event;
            tracked.store(true, std::sync::atomic::Ordering::Release);
        };
        let outcome = tokio::time::timeout(
            NATIVE_TIMEOUT,
            harness.rewind_native_conversation(
                NativeRewindRequest {
                    scope: scope.clone(),
                    target_checkpoint: target,
                    last_seen_checkpoint: last_seen,
                },
                &progress,
            ),
        )
        .await;
        let written = written.load(std::sync::atomic::Ordering::Acquire);
        match outcome {
            Ok(Ok(NativeRewindOutcome::Rewound)) => match self.claude_turns(scope).await {
                Ok(turns) if !turns.compacted && turns.turn_starts == record.retained_turns => Ok(
                    Some(self.commit_claude_rewind(task_id, command_id, &record, true)?),
                ),
                _ => Err(TaskProjectionError::ProviderUnknown(other(
                    "Claude acknowledged the rewind but readback did not show the retained prefix. Later history stays visible; reconcile before sending.",
                ))),
            },
            Ok(Ok(NativeRewindOutcome::Refused { reason, message })) => {
                self.clear_claude_rewind(
                    task_id,
                    command_id,
                    format!("Claude refused the rewind ({reason}): {message}. No history was removed; files kept."),
                )?;
                Err(mutation(format!(
                    "Claude refused the rewind ({reason}): {message}"
                )))
            }
            Ok(Err(error)) if !written => {
                self.clear_claude_rewind(
                    task_id,
                    command_id,
                    format!("Rewind was not sent: {error}"),
                )?;
                Err(TaskProjectionError::Mutation(error.into()))
            }
            Ok(Err(error)) => Err(TaskProjectionError::ProviderUnknown(error.into())),
            Err(_) if !written => {
                self.clear_claude_rewind(
                    task_id,
                    command_id,
                    "Rewind was not sent: Claude did not start in time.".into(),
                )?;
                Err(mutation("Claude rewind timed out before it was sent"))
            }
            Err(_) => Err(TaskProjectionError::ProviderUnknown(other(
                "Claude rewind completion timed out; later history remains visible. Reconcile without repeating the rewind.",
            ))),
        }
    }

    /// `Ok(None)` when `rewind_command_id` is not a Claude rewind, so the
    /// shared reconcile arm continues with its Codex readback.
    pub(super) async fn reconcile_claude_rewind(
        &self,
        command_id: &str,
        task_id: &str,
        rewind_command_id: &str,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        let Some(bytes) = self
            .store
            .load_snapshot(&record_id(task_id, rewind_command_id))
            .map_err(|error| TaskProjectionError::Mutation(error.into()))?
        else {
            return Ok(None);
        };
        let record: ClaudeRewindRecord = serde_json::from_slice(&bytes)
            .map_err(|error| mutation(format!("rewind intent: {error}")))?;
        if record.task_id != task_id || record.command_id != rewind_command_id {
            return Err(mutation("rewind intent identity differs"));
        }
        let (chat, binding) = self
            .claude_rewind_ready(task_id)
            .map_err(TaskProjectionError::Mutation)?;
        let context = self
            .doc_host
            .context_control(task_id)
            .map_err(TaskProjectionError::Mutation)?;
        if context.pending_rewind.as_deref() != Some(rewind_command_id) {
            return Err(mutation("No matching unresolved rewind"));
        }
        let scope = self.claude_scope(&chat, &binding);
        if binding.binding_id != record.binding_id
            || binding.generation != record.binding_generation
            || binding.provider_task_id != record.session_id
            || scope.cwd != record.cwd
            || scope.provider_home != record.provider_home
            || chat
                .config
                .as_ref()
                .and_then(|config| config.provider_account_id.as_ref())
                != record.provider_account_id.as_ref()
        {
            return Err(mutation(
                "Rewind identity, account or workspace changed; keep later history visible",
            ));
        }
        let observed = self
            .claude_turns(scope)
            .await
            .map_err(TaskProjectionError::Mutation)?;
        if !observed.compacted && observed.turn_starts == record.retained_turns {
            Ok(Some(self.commit_claude_rewind(
                task_id, command_id, &record, true,
            )?))
        } else if !observed.compacted && observed.turn_starts == record.original_turns {
            Ok(Some(self.commit_claude_rewind(
                task_id, command_id, &record, false,
            )?))
        } else {
            Err(mutation(
                "Claude history differs from both saved prefixes. Later LUNA history stays visible and sends remain blocked.",
            ))
        }
    }

    fn commit_claude_rewind(
        &self,
        task_id: &str,
        command_id: &str,
        record: &ClaudeRewindRecord,
        reverted: bool,
    ) -> Result<CommandReceipt, TaskProjectionError> {
        let (_, binding) = self
            .claude_rewind_ready(task_id)
            .map_err(TaskProjectionError::ProviderUnknown)?;
        if binding.binding_id != record.binding_id
            || binding.generation != record.binding_generation
        {
            return Err(TaskProjectionError::ProviderUnknown(other(
                "rewind binding changed; retained history was not projected",
            )));
        }
        let id = record_id(task_id, &record.command_id);
        let source_bytes = self
            .store
            .load_snapshot(&format!("{id}:transcript"))
            .map_err(|error| TaskProjectionError::ProviderUnknown(error.into()))?
            .ok_or_else(|| {
                TaskProjectionError::ProviderUnknown(other("rewind source snapshot is unavailable"))
            })?;
        let original = SessionDoc::from_snapshot(&source_bytes)
            .map_err(crate::EngineError::from)
            .map_err(TaskProjectionError::ProviderUnknown)?
            .read_entries()
            .map_err(crate::EngineError::from)
            .map_err(TaskProjectionError::ProviderUnknown)?;
        let marker = self.history_marker(
            format!("history-rewind:{}", record.command_id),
            format!(
                "Rewound this conversation before message {} in the same Claude Code session. Files and external effects were kept. Audit snapshot: {id}.",
                record.message_id
            ),
        );
        let (_, receipt) = self
            .doc_host
            .commit_context_boundary(
                task_id,
                command_id,
                Some(&binding.binding_id),
                Some(binding.generation),
                None,
                None,
                &[],
                &[],
                |doc| {
                    if doc.context_control().pending_rewind.as_deref()
                        != Some(record.command_id.as_str())
                    {
                        return Err(other("rewind intent changed before projection"));
                    }
                    if reverted {
                        if doc.read_entries()? != original {
                            return Err(other(
                                "LUNA transcript changed while rewind was pending; keep later rows visible",
                            ));
                        }
                        doc.replace_conversation_entries(&record.entries, &record.checkpoints)?;
                        doc.push_message(&marker)?;
                        doc.clear_context_usage()?;
                    }
                    let mut context = doc.context_control();
                    context.pending_rewind = None;
                    context.provider_summary = None;
                    context.notice = Some(if reverted {
                        "Conversation rewound in the same Claude Code session. Files kept; Claude file checkpoints and Git checkpoints are separate.".into()
                    } else {
                        "Rewind readback verified the original Claude conversation. No history was removed; files kept.".into()
                    });
                    doc.set_context_control(&context)?;
                    Ok(())
                },
                |_| Ok(()),
            )
            .map_err(|error| {
                TaskProjectionError::ProviderUnknown(other(error.message().to_string()))
            })?;
        // The binding and session id stay current: continuation resumes the
        // same Claude session from its persisted rewind anchor.
        Ok(receipt)
    }

    /// The native prompt uuid that starts the selected user row's turn.
    fn claude_file_checkpoint(
        &self,
        task_id: &str,
        message_id: &str,
    ) -> Result<String, crate::EngineError> {
        let handle = self.doc_host.open(task_id)?;
        let entries = handle.doc().read_entries()?;
        let index = entries
            .iter()
            .position(|entry| entry.id == message_id && entry.role == MessageRole::User)
            .ok_or_else(|| other("choose one of your messages in this task"))?;
        if let Some(previous) = entries[..index]
            .iter()
            .rev()
            .find(|entry| matches!(entry.role, MessageRole::User | MessageRole::Assistant))
            && (previous.role != MessageRole::Assistant
                || handle.doc().provider_checkpoint(&previous.id).is_none())
        {
            return Err(other(
                "this message was folded into a running Claude turn; choose the turn's first message",
            ));
        }
        let assistant = entries[index + 1..]
            .iter()
            .find(|entry| matches!(entry.role, MessageRole::User | MessageRole::Assistant))
            .filter(|entry| entry.role == MessageRole::Assistant)
            .ok_or_else(|| other("this message has no completed Claude turn"))?;
        handle
            .doc()
            .provider_checkpoint(&assistant.id)
            .ok_or_else(|| {
                other("this turn has no Claude checkpoint (it did not complete in LUNA)")
            })
    }

    pub(super) async fn handle_native_file_rewind(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        let fail = |error: &dyn std::fmt::Display| RpcError::Failed(error.to_string());
        let params: FileRewindParams = parse_params(params)?;
        if params
            .target_device_id
            .as_deref()
            .is_some_and(|target| target != self.doc_host.device_id())
        {
            return Err(RpcError::Failed(
                "Remote Claude file checkpoints are unavailable".into(),
            ));
        }
        let (chat, binding) = self
            .ensure_history_operation_ready(&params.task_id)
            .map_err(|error| fail(&error))?;
        if binding.provider_id != HarnessId::ClaudeCode {
            return Err(RpcError::Failed(
                "Native file checkpoints are available only for Claude Code tasks".into(),
            ));
        }
        let checkpoint = self
            .claude_file_checkpoint(&params.task_id, &params.message_id)
            .map_err(|error| fail(&error))?;
        let scope = self.claude_scope(&chat, &binding);
        let harness = self
            .registry
            .resolve(HarnessId::ClaudeCode)
            .map_err(|error| fail(&error))?;
        let request = |dry_run| NativeFileRewindRequest {
            scope: scope.clone(),
            checkpoint: checkpoint.clone(),
            dry_run,
        };
        let preview =
            tokio::time::timeout(NATIVE_TIMEOUT, harness.rewind_native_files(request(true)))
                .await
                .map_err(|_| RpcError::Failed("Claude file checkpoint preview timed out".into()))?
                .map_err(|error| fail(&error))?;
        let preview = file_view(&params, &checkpoint, &preview, true);
        if method == methods::PREVIEW_NATIVE_FILE_REWIND {
            return RpcReply::value(&preview);
        }
        if params.digest.as_deref() != Some(preview.digest.as_str()) {
            return Err(RpcError::Failed(
                "Files changed since the preview. Review the new preview before restoring.".into(),
            ));
        }
        if !preview.can_rewind {
            return Err(RpcError::Failed(
                preview
                    .error
                    .unwrap_or_else(|| "Claude cannot restore these files".into()),
            ));
        }
        // Retire a parked warm process before files change underneath it.
        let _transition = self
            .sessions
            .prepare_native_rewind(&params.task_id)
            .await
            .map_err(|error| fail(&error))?;
        let applied = tokio::time::timeout(NATIVE_TIMEOUT, harness.rewind_native_files(request(false)))
            .await
            .map_err(|_| {
                RpcError::Failed(
                    "Claude file restore did not confirm in time; preview again to see the current files".into(),
                )
            })?
            .map_err(|error| fail(&error))?;
        RpcReply::value(&file_view(&params, &checkpoint, &applied, false))
    }
}
