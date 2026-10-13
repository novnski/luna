//! The per-ticket lifecycle driver. Every step starts from the persisted
//! intent (desired column, intent revision, execution generation, stop
//! barrier, launch delivery) and re-reads receipts and runtime evidence
//! before acting, so a step is safe to repeat after a crash, a restart or a
//! concurrent caller. Callers hold the ticket's lifecycle gate.
use super::*;
use luna_proto::{
    BoardAttentionKind, BoardChatCreated, BoardCreateMainChatRequest, BoardCreateSideChatRequest,
    BoardTicketBinding,
};

/// A board execution that currently holds admission capacity.
struct ActiveExecution {
    project_id: String,
    harness: Option<HarnessId>,
    cwd: Option<String>,
    task_id: String,
}

/// How a just-issued (or reconciled) launch command stands.
enum LaunchOutcome {
    Pending,
    Delivered,
    Unknown(String),
    Failed(String),
}

/// The deterministic id of a reviewer interrupt issued on behalf of one
/// chat-level Stop command (shared by the chat Stop button and board Stop).
pub(crate) fn reviewer_stop_command(command_id: &str, reviewer: &str) -> String {
    let digest = format!(
        "{:x}",
        <Sha256 as sha2::Digest>::digest(format!("{command_id}\0{reviewer}").as_bytes())
    );
    format!("board-stop-rv-{}", &digest[..32])
}

/// One checkout regardless of how its path is spelled (child chats store
/// the canonical path, e.g. `/private/var/…` for `/var/…`).
fn checkout_key(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    Some(
        std::fs::canonicalize(cwd)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| cwd.to_owned()),
    )
}

fn attention_kind_for(error: &BoardError) -> BoardAttentionKind {
    match error {
        BoardError::UnavailableModel { .. } | BoardError::UnsupportedEffort { .. } => {
            BoardAttentionKind::ModelUnavailable
        }
        BoardError::UnavailableProvider { .. } | BoardError::Unavailable { .. } => {
            BoardAttentionKind::ProviderFailure
        }
        BoardError::InvalidRole { .. }
        | BoardError::InvalidDestination { .. }
        | BoardError::ParentLocked { .. }
        | BoardError::NotAuthorized { .. }
        | BoardError::WrongHost { .. } => BoardAttentionKind::CompatibilityBlocked,
        _ => BoardAttentionKind::Other,
    }
}

fn admission_label(reason: &BoardAdmissionReason) -> &'static str {
    match reason {
        BoardAdmissionReason::ProjectCapacity { .. } => "projectCapacity",
        BoardAdmissionReason::HostCapacity { .. } => "hostCapacity",
        BoardAdmissionReason::ProviderCapacity { .. } => "providerCapacity",
        BoardAdmissionReason::Budget { .. } => "budget",
        BoardAdmissionReason::Dependency { .. } => "dependency",
        BoardAdmissionReason::CheckoutBusy { .. } => "checkoutBusy",
        BoardAdmissionReason::WaitingForStop { .. } => "waitingForStop",
        BoardAdmissionReason::HostUnavailable { .. } => "hostUnavailable",
        BoardAdmissionReason::SettingsUnresolved { .. } => "settingsUnresolved",
    }
}

/// Name a message's images the way a chat message does (the composer's
/// `with_attachments` transport, which the transcript parses back into
/// thumbnails): a blank line, the `Attached images` heading, one path a line.
fn with_attachment_trailer(text: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return text.to_owned();
    }
    let refs: Vec<String> = paths.iter().map(|path| format!("- {path}")).collect();
    format!(
        "{text}\n\nAttached images (local files — open them to view):\n{}",
        refs.join("\n")
    )
}

impl EngineRpc {
    /// Advance one ticket from its persisted intent. The caller holds
    /// `board_lifecycle_gate(work_item_id)`.
    pub(crate) async fn board_drive(
        &self,
        work_item_id: &str,
        boot: bool,
    ) -> BoardResult<BoardMoveEffect> {
        let mut ticket = self.board_ticket(work_item_id)?;
        self.board_settle_stop(&mut ticket, boot).await?;
        self.board_settle_launch(&mut ticket).await?;
        let effect = match (ticket.intent.desired, ticket.item.column) {
            (_, BoardColumn::Done) => BoardMoveEffect::None,
            (BoardColumn::Ready, _) => match Box::pin(self.board_drive_ready(&mut ticket)).await {
                Ok(effect) => effect,
                Err(error) => {
                    self.board_cache_phase(&mut ticket)?;
                    return Err(error);
                }
            },
            _ => Box::pin(self.board_drive_hold(&mut ticket)).await?,
        };
        self.board_cache_phase(&mut ticket)?;
        Ok(effect)
    }

    /// Keep the restart readback cache and `needs_reconcile` current, and
    /// wake Board watchers when only the observation changed.
    pub(crate) fn board_cache_phase(&self, ticket: &mut Ticket) -> BoardResult<()> {
        let observation = self.board_observation(&ticket.item, Some(&ticket.intent));
        let attention = (!observation.attention.is_empty())
            .then(|| board_digest(&observation.attention))
            .transpose()?;
        let intent = &ticket.intent;
        let needs = intent.stop.as_ref().is_some_and(barrier_open)
            || matches!(
                intent.launch_state,
                BoardLaunchState::Unsent | BoardLaunchState::Pending
            )
            || intent.queue.is_some()
            || (intent.desired == BoardColumn::Ready
                && intent.served_intent_revision < intent.intent_revision);
        if observation.phase == intent.observed_phase
            && needs == intent.needs_reconcile
            && attention == intent.observed_attention
        {
            return Ok(());
        }
        if ticket.version.is_none()
            && observation.phase == BoardPhase::NotStarted
            && !needs
            && attention.is_none()
        {
            return Ok(());
        }
        ticket.intent.observed_phase = observation.phase;
        ticket.intent.needs_reconcile = needs;
        ticket.intent.observed_attention = attention;
        self.board_intent_only(ticket)?;
        self.board_hub()
            .notify_observed(vec![ticket.item.id.clone()]);
        Ok(())
    }

    /// Whether any of these chats' transcripts still shows a streaming
    /// reply (restart evidence that a run did not settle).
    fn board_any_streaming(&self, task_ids: &[String]) -> bool {
        task_ids.iter().any(|task| {
            self.doc_host
                .open(task)
                .ok()
                .and_then(|handle| handle.doc().read_latest_assistant_entry().ok().flatten())
                .is_some_and(|entry| entry.status == Some(MessageStatus::Streaming))
        })
    }

    /// Whether the transcript shows provider output for the turn started by
    /// `message_id`: an assistant reply after that exact user message with
    /// content other than an engine-reported error.
    fn board_turn_produced(&self, task_id: &str, message_id: &str) -> bool {
        if task_id.is_empty() {
            return false;
        }
        let Ok(handle) = self.doc_host.open(task_id) else {
            return false;
        };
        let Ok(entries) = handle.doc().read_entries() else {
            return false;
        };
        let Some(index) = entries
            .iter()
            .position(|entry| entry.id == message_id && entry.role == MessageRole::User)
        else {
            return false;
        };
        entries[index + 1..]
            .iter()
            .take_while(|entry| entry.role != MessageRole::User)
            .filter(|entry| entry.role == MessageRole::Assistant)
            .any(|entry| {
                entry.parts.iter().any(|part| match part {
                    MessagePart::Text { text, .. } => !text.trim().is_empty(),
                    MessagePart::Error { .. } => false,
                    _ => true,
                })
            })
    }

    /// Whether a chat exists on this host and is running a turn.
    pub(crate) fn board_chat_live(&self, task_id: &str) -> (bool, bool) {
        let available = self
            .workspace
            .chat(task_id)
            .ok()
            .flatten()
            .is_some_and(|chat| chat.device_id == self.engine_info.device_id);
        (
            available,
            available && self.sessions.turn_in_flight(task_id),
        )
    }

    // ---- shared Stop ----

    /// The exact workflow-owned reviewer chats of a ticket (never unrelated
    /// siblings), as the review slice records them for this generation.
    pub(crate) fn board_stop_scope(
        &self,
        work_item_id: &str,
        generation: u64,
        worker: &str,
    ) -> Vec<String> {
        let mut ids = match self.board_review_owned_work(work_item_id, generation) {
            Ok(ids) => ids,
            Err(error) => {
                tracing::warn!(%error, work_item_id, "review owned-work lookup failed");
                Vec::new()
            }
        };
        let mut seen = HashSet::new();
        ids.retain(|id| id != worker && seen.insert(id.clone()));
        ids
    }

    /// Interrupt one chat's active turn through the ordinary task command
    /// path: exactly the command the chat's Stop button sends.
    pub(crate) async fn board_interrupt(
        &self,
        task_id: &str,
        command_id: &str,
    ) -> Result<CommandReceipt, RpcError> {
        let mut command = legacy_task_command(
            task_id.into(),
            TaskCommandIntent::Session {
                command: SessionCommandPayload::Interrupt {},
                transfers: Vec::new(),
            },
        );
        command.command_id = command_id.into();
        command.actor_id = "luna-board".into();
        command.client_id = "luna-board-lifecycle".into();
        command.client_session_id = self.cleanup_boot_id.clone();
        Box::pin(self.execute_task_command(command, true)).await
    }

    /// Stop the ticket-owned reviewers that belong to one chat-level Stop.
    /// Ids derive from the Stop command, so repeating it is a no-op.
    pub(crate) async fn board_stop_reviewers(
        &self,
        work_item_id: &str,
        generation: u64,
        worker: &str,
        command_id: &str,
    ) -> Vec<String> {
        let mut stopped = Vec::new();
        for reviewer in self.board_stop_scope(work_item_id, generation, worker) {
            if !self.board_chat_live(&reviewer).1 {
                continue;
            }
            let id = reviewer_stop_command(command_id, &reviewer);
            match self.board_interrupt(&reviewer, &id).await {
                Ok(_) => stopped.push(id),
                Err(error) => {
                    tracing::warn!(%error, reviewer, "reviewer stop could not be issued")
                }
            }
        }
        stopped
    }

    /// Scope extension of the chat Stop button: stopping a ticket's working
    /// chat also stops that ticket's owned reviewer chats, so the chat and
    /// the board observe the same owned-work set. Called after any admitted
    /// session interrupt; a no-op for chats that are not ticket workers.
    pub(crate) async fn board_follow_chat_stop(&self, task_id: &str, command_id: &str) {
        if command_id.starts_with("board-stop-rv-") {
            return;
        }
        let Ok(links) = self.store.board_task_links_for_task(task_id, true) else {
            return;
        };
        let Some(link) = links
            .into_iter()
            .find(|link| link.role == TaskLinkRole::Working)
        else {
            return;
        };
        let generation = self
            .store
            .board_intent(&link.work_item_id)
            .ok()
            .flatten()
            .map_or(0, |intent| intent.execution_generation);
        self.board_stop_reviewers(&link.work_item_id, generation, task_id, command_id)
            .await;
    }

    /// Board Stop for the current generation: record the barrier (tied to
    /// this generation and request) before any effect, then send the same
    /// Stop as the chat button, including the ticket's owned reviewers.
    pub(crate) async fn board_shared_stop(
        &self,
        ticket: &mut Ticket,
        worker: &str,
        outer: Option<OuterRequest>,
    ) -> BoardResult<Option<BoardMutationReceipt>> {
        let generation = ticket.intent.execution_generation;
        let reviewers = self.board_stop_scope(&ticket.item.id, generation, worker);
        let digest = board_digest(&(
            "stop",
            &ticket.item.id,
            generation,
            &ticket.intent.last_request_id,
        ))?;
        let command_id = format!("board-stop-{}", &digest[..32]);
        let mut receipt_ids = vec![command_id.clone()];
        receipt_ids.extend(
            reviewers
                .iter()
                .map(|reviewer| reviewer_stop_command(&command_id, reviewer)),
        );
        let phase = self
            .board_observation(&ticket.item, Some(&ticket.intent))
            .phase;
        let mut targets = vec![worker.to_owned()];
        targets.extend(reviewers);
        let (available, _) = self.board_chat_live(worker);
        ticket.intent.stop = Some(BoardStopBarrier {
            execution_generation: generation,
            request_id: ticket.intent.last_request_id.clone(),
            state: if available {
                BoardStopState::Requested
            } else {
                BoardStopState::Unknown
            },
            task_ids: targets,
            stop_receipt_ids: receipt_ids,
            requested_at_ms: crate::now_ms(),
            settled_at_ms: None,
            detail: Some(if available {
                format!("{} when Stop was requested", phase.label())
            } else {
                "The working chat's host is unavailable; Stop could not be delivered.".into()
            }),
        });
        ticket.intent.needs_reconcile = true;
        let (request_id, payload_hash, operation, actor) = match outer {
            Some(outer) => (
                outer.request_id,
                outer.payload_hash,
                outer.operation,
                outer.actor,
            ),
            None => {
                let (request_id, hash) = self.board_engine_request(&(
                    "stopRequested",
                    &ticket.item.id,
                    generation,
                    &ticket.intent.last_request_id,
                ))?;
                (
                    request_id,
                    hash,
                    BoardOperation::Control,
                    lifecycle_caller().actor(),
                )
            }
        };
        let receipt = self.board_lifecycle_commit(
            ticket,
            Commit {
                request_id,
                payload_hash,
                operation,
                actor,
                touch: Some(&no_change),
                placement: None,
                intent: true,
                rows: Vec::new(),
                event: Some(WorkItemEventKind::StopRequested {
                    execution_generation: generation,
                }),
                task_id: Some(worker.to_owned()),
            },
        )?;
        if !available {
            return Ok(receipt);
        }
        let result = self.board_interrupt(worker, &command_id).await;
        // The same scope extension the chat Stop button gets: the ticket's
        // owned reviewers, with ids derived from this Stop (repeat-safe).
        self.board_stop_reviewers(&ticket.item.id, generation, worker, &command_id)
            .await;
        // The command's own receipt does not prove termination either way (a
        // turn that already ended supersedes the interrupt): the barrier
        // settles only from the run's observed end, or turns Unknown when the
        // run outlives the stop window.
        if let Some(barrier) = ticket.intent.stop.as_mut() {
            barrier.state = BoardStopState::Stopping;
            match result {
                Ok(receipt)
                    if !matches!(
                        receipt.status,
                        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
                    ) =>
                {
                    barrier.detail = Some(receipt.reason.map_or_else(
                        || "The chat did not apply the Stop command; waiting for the run to end."
                            .into(),
                        |reason| {
                            format!(
                                "The chat did not apply the Stop command ({}); waiting for the run to end.",
                                reason.message
                            )
                        },
                    ));
                }
                Ok(_) => {}
                Err(error) => {
                    barrier.detail = Some(format!(
                        "Stop could not be delivered ({error}); waiting for the run to end."
                    ));
                }
            }
        }
        self.board_intent_only(ticket)?;
        self.board_settle_stop(ticket, false).await?;
        Ok(receipt)
    }

    /// Settle the current generation's stop barrier from runtime evidence:
    /// Stopped only once no targeted run is live; Unknown when the host is
    /// gone or the run outlives the stop window.
    pub(crate) async fn board_settle_stop(
        &self,
        ticket: &mut Ticket,
        boot: bool,
    ) -> BoardResult<()> {
        let Some(barrier) = ticket.intent.stop.clone() else {
            return Ok(());
        };
        if barrier.execution_generation != ticket.intent.execution_generation
            || !barrier_open(&barrier)
        {
            return Ok(());
        }
        let mut lost = false;
        let mut live = false;
        for task in &barrier.task_ids {
            let (available, in_flight) = self.board_chat_live(task);
            lost |= !available;
            live |= in_flight;
        }
        let now = crate::now_ms();
        let (state, detail, stopped) = if lost {
            if barrier.state == BoardStopState::Unknown {
                return Ok(());
            }
            (
                BoardStopState::Unknown,
                "A stopped chat's host is unavailable; termination is unverified.".to_owned(),
                false,
            )
        } else if !live && boot && self.board_any_streaming(&barrier.task_ids) {
            if barrier.state == BoardStopState::Unknown {
                return Ok(());
            }
            (
                BoardStopState::Unknown,
                "The engine restarted while the run was still streaming; its end is unverified."
                    .to_owned(),
                false,
            )
        } else if !live {
            (
                BoardStopState::Stopped,
                if boot {
                    "The engine restarted; no live run remains.".to_owned()
                } else {
                    "The run ended after Stop.".to_owned()
                },
                true,
            )
        } else if now - barrier.requested_at_ms > STOP_UNKNOWN_AFTER_MS
            && barrier.state != BoardStopState::Unknown
        {
            (
                BoardStopState::Unknown,
                "The run did not end within a minute of Stop; termination is unverified."
                    .to_owned(),
                false,
            )
        } else {
            return Ok(());
        };
        if let Some(open) = ticket.intent.stop.as_mut() {
            open.state = state;
            open.detail = Some(detail);
            if stopped {
                open.settled_at_ms = Some(now);
            }
        }
        if stopped
            && matches!(
                ticket.intent.queue,
                Some(BoardAdmissionReason::WaitingForStop { .. })
            )
        {
            ticket.intent.queue = None;
        }
        let (request_id, payload_hash) = self.board_engine_request(&(
            "stopSettled",
            &ticket.item.id,
            barrier.execution_generation,
            &barrier.request_id,
            format!("{state:?}"),
        ))?;
        self.board_lifecycle_commit(
            ticket,
            Commit {
                request_id,
                payload_hash,
                operation: BoardOperation::Control,
                actor: lifecycle_caller().actor(),
                touch: Some(&no_change),
                placement: None,
                intent: true,
                rows: Vec::new(),
                event: Some(WorkItemEventKind::StopSettled {
                    execution_generation: barrier.execution_generation,
                    stopped,
                }),
                task_id: barrier.task_ids.first().cloned(),
            },
        )?;
        Ok(())
    }

    // ---- launch delivery ----

    /// The current generation's launch snapshot.
    pub(crate) fn board_current_launch(
        &self,
        ticket: &Ticket,
    ) -> BoardResult<Option<LaunchSnapshot>> {
        let Some(id) = &ticket.intent.launch_snapshot_id else {
            return Ok(None);
        };
        Ok(self
            .store
            .board_launches(&ticket.item.id, 64)
            .map_err(storage)?
            .into_iter()
            .find(|launch| &launch.id == id))
    }

    /// Reconcile the current launch by its original command/message id
    /// before anything else may send.
    pub(crate) async fn board_settle_launch(&self, ticket: &mut Ticket) -> BoardResult<()> {
        match ticket.intent.launch_state {
            BoardLaunchState::Unsent => {
                if ticket.intent.desired != BoardColumn::Ready
                    || ticket.item.column != BoardColumn::Ready
                {
                    ticket.intent.launch_state = BoardLaunchState::Cancelled;
                    if let FirstPromptDelivery::Pending { .. } = ticket.intent.first_prompt {
                        ticket.intent.first_prompt = FirstPromptDelivery::NotSent;
                    }
                    return self.board_intent_only(ticket);
                }
                // Persisted but never issued (crash between commit and send):
                // no receipt exists, so issuing the same command id is safe.
                Box::pin(self.board_issue_launch(ticket)).await
            }
            BoardLaunchState::Pending => {
                let (Some(command_id), Some(message_id)) = (
                    ticket.intent.active_command_id.clone(),
                    ticket.intent.active_message_id.clone(),
                ) else {
                    return Ok(());
                };
                let this_boot =
                    ticket.intent.launch_boot_id.as_deref() == Some(self.cleanup_boot_id.as_str());
                let receipt = self
                    .store
                    .task_command_receipt(&command_id)
                    .map_err(storage)?;
                // No receipt: the command never reached admission (the
                // process stopped before it), so the same id is safe to issue.
                let Some(receipt) = receipt else {
                    return Box::pin(self.board_issue_launch(ticket)).await;
                };
                // Acceptance pending from an earlier boot can no longer be
                // proven; one from this boot may still arrive.
                let outcome = self.board_classify_launch(ticket, &receipt, &message_id, this_boot);
                self.board_record_launch(ticket, outcome)
            }
            // Reconcile an uncertain delivery by its original message id:
            // recorded provider output for that exact turn settles it.
            BoardLaunchState::Unknown => {
                let message_id = ticket.intent.active_message_id.clone().unwrap_or_default();
                let worker = ticket.intent.working_task_id.clone().unwrap_or_default();
                if !message_id.is_empty() && self.board_turn_produced(&worker, &message_id) {
                    return self.board_record_launch(ticket, LaunchOutcome::Delivered);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn board_classify_launch(
        &self,
        ticket: &Ticket,
        receipt: &CommandReceipt,
        message_id: &str,
        same_boot: bool,
    ) -> LaunchOutcome {
        match receipt.status {
            CommandReceiptStatus::Applied
                if receipt.correlation_id.as_deref() == Some(message_id) =>
            {
                LaunchOutcome::Delivered
            }
            // Harness-independent evidence: the engine recorded provider
            // output for this exact turn (not every adapter reports a
            // session start the receipt can correlate).
            CommandReceiptStatus::Applied | CommandReceiptStatus::Pending
                if self.board_turn_produced(
                    ticket.intent.working_task_id.as_deref().unwrap_or_default(),
                    message_id,
                ) =>
            {
                LaunchOutcome::Delivered
            }
            CommandReceiptStatus::Applied => LaunchOutcome::Unknown(
                "The send was applied without a matching provider acknowledgement.".into(),
            ),
            CommandReceiptStatus::Rejected
            | CommandReceiptStatus::Conflict
            | CommandReceiptStatus::Failed
            | CommandReceiptStatus::Superseded => {
                LaunchOutcome::Failed(receipt.reason.as_ref().map_or_else(
                    || "The task command was refused before reaching the provider.".into(),
                    |reason| reason.message.clone(),
                ))
            }
            CommandReceiptStatus::Pending => {
                // An admitted send carries `ProviderAcceptanceUnknown` while it
                // waits for provider correlation; only a receipt the engine
                // closed as ambiguous (committed without acceptance, e.g. a
                // restart sweep) proves the outcome is unknown.
                let unknown_reason = receipt.committed_at_ms.is_some()
                    && receipt.reason.as_ref().is_some_and(|reason| {
                        reason.code == CommandReasonCode::ProviderAcceptanceUnknown
                    });
                let worker = ticket.intent.working_task_id.as_deref().unwrap_or_default();
                let live = self.board_chat_live(worker).1;
                if unknown_reason {
                    LaunchOutcome::Unknown(
                        "Provider acceptance of this instruction could not be proven.".into(),
                    )
                } else if !same_boot {
                    LaunchOutcome::Unknown(
                        "The engine restarted before provider acceptance was confirmed.".into(),
                    )
                } else if !live
                    // AW-10: a send waiting for (or just admitted from) the
                    // host line has provably not been sent.
                    && !self.sessions.turn_start_pending(worker)
                    && crate::now_ms() - receipt.accepted_at_ms > DELIVERY_GRACE_MS
                    // The startup has not ended while the session-doc
                    // executor still owns this exact launch command.
                    && !self
                        .doc_host
                        .command_still_starting(&receipt.task_id, &receipt.command_id)
                {
                    LaunchOutcome::Unknown(
                        "The run ended without confirming acceptance of this instruction.".into(),
                    )
                } else {
                    LaunchOutcome::Pending
                }
            }
        }
    }

    fn board_record_launch(&self, ticket: &mut Ticket, outcome: LaunchOutcome) -> BoardResult<()> {
        let command_id = ticket.intent.active_command_id.clone().unwrap_or_default();
        let message_id = ticket.intent.active_message_id.clone().unwrap_or_default();
        let first = matches!(&ticket.intent.first_prompt,
            FirstPromptDelivery::Pending { command_id: pending, .. }
                | FirstPromptDelivery::Unknown { command_id: pending, .. }
                if *pending == command_id);
        let worker = ticket.intent.working_task_id.clone();
        let previous = ticket.intent.launch_state;
        let mut unknown_event = false;
        match outcome {
            LaunchOutcome::Pending => {
                if previous == BoardLaunchState::Pending {
                    return Ok(());
                }
                ticket.intent.launch_state = BoardLaunchState::Pending;
            }
            LaunchOutcome::Delivered => {
                ticket.intent.launch_state = BoardLaunchState::Delivered;
                ticket.intent.launch_detail = None;
                if first {
                    ticket.intent.first_prompt = FirstPromptDelivery::Delivered {
                        command_id: command_id.clone(),
                        message_id,
                        at_ms: crate::now_ms(),
                    };
                }
            }
            LaunchOutcome::Unknown(detail) => {
                if previous == BoardLaunchState::Unknown {
                    return Ok(());
                }
                ticket.intent.launch_state = BoardLaunchState::Unknown;
                ticket.intent.launch_detail = Some(detail.clone());
                if first {
                    ticket.intent.first_prompt = FirstPromptDelivery::Unknown {
                        command_id: command_id.clone(),
                        message_id,
                        detail,
                    };
                }
                unknown_event = true;
            }
            LaunchOutcome::Failed(detail) => {
                ticket.intent.launch_state = BoardLaunchState::Failed;
                ticket.intent.launch_detail = Some(detail.clone());
                ticket.intent.launch_failure = Some(attention(
                    BoardAttentionKind::ProviderFailure,
                    detail,
                    worker.as_deref(),
                ));
                if first {
                    // Nothing reached the provider: a later admission sends
                    // the first instruction again under a new launch.
                    ticket.intent.first_prompt = FirstPromptDelivery::NotSent;
                }
            }
        }
        if !unknown_event {
            return self.board_intent_only(ticket);
        }
        let launch_request_id = self
            .board_current_launch(ticket)?
            .map(|launch| launch.launch_request_id)
            .unwrap_or_else(|| command_id.clone());
        let (request_id, payload_hash) =
            self.board_engine_request(&("launchUnknown", &ticket.item.id, &launch_request_id))?;
        self.board_lifecycle_commit(
            ticket,
            Commit {
                request_id,
                payload_hash,
                operation: BoardOperation::Start,
                actor: lifecycle_caller().actor(),
                touch: Some(&no_change),
                placement: None,
                intent: true,
                rows: Vec::new(),
                event: Some(WorkItemEventKind::LaunchUnknown { launch_request_id }),
                task_id: worker,
            },
        )?;
        Ok(())
    }

    /// Send the current launch exactly as frozen in its snapshot, through
    /// the ordinary task command path (a user turn with a correlated message
    /// id), then record the receipt.
    pub(crate) async fn board_issue_launch(&self, ticket: &mut Ticket) -> BoardResult<()> {
        let Some(launch) = self.board_current_launch(ticket)? else {
            ticket.intent.launch_state = BoardLaunchState::None;
            return self.board_intent_only(ticket);
        };
        let result = self
            .board_send_run(
                &launch.task_id,
                &launch.command_id,
                &launch.message_id,
                &launch.instruction_text,
                &launch.attachments,
                &launch.input_items,
            )
            .await;
        ticket.intent.launch_boot_id = Some(self.cleanup_boot_id.clone());
        let receipt = match result {
            Ok(receipt) => receipt,
            Err(error) => match self
                .store
                .task_command_receipt(&launch.command_id)
                .map_err(storage)?
            {
                Some(receipt) => receipt,
                None => {
                    return self.board_record_launch(
                        ticket,
                        LaunchOutcome::Failed(format!(
                            "The instruction could not be sent: {error}"
                        )),
                    );
                }
            },
        };
        ticket.intent.launch_state = BoardLaunchState::Unsent;
        let outcome = self.board_classify_launch(ticket, &receipt, &launch.message_id, true);
        self.board_record_launch(ticket, outcome)
    }

    /// One ordinary turn in `task_id` built from the chat's own execution
    /// configuration (an existing worker keeps its settings).
    pub(crate) async fn board_send_run(
        &self,
        task_id: &str,
        command_id: &str,
        message_id: &str,
        text: &str,
        attachments: &[String],
        input_items: &[luna_proto::ComposerInput],
    ) -> Result<CommandReceipt, RpcError> {
        let mut request = self
            .doc_host
            .request_from_chat_row(task_id, text)
            .ok_or_else(|| {
                RpcError::Failed("The working chat has no execution configuration.".into())
            })?;
        // The brief's images and selections, as a chat message carries them.
        request.attachments = attachments.to_vec();
        request.input_items = input_items.to_vec();
        let mut command = legacy_task_command(
            task_id.into(),
            TaskCommandIntent::Session {
                command: SessionCommandPayload::Run {
                    request,
                    message_id: message_id.into(),
                },
                transfers: Vec::new(),
            },
        );
        command.command_id = command_id.into();
        command.actor_id = "luna-board".into();
        command.client_id = "luna-board-lifecycle".into();
        command.client_session_id = self.cleanup_boot_id.clone();
        Box::pin(self.execute_task_command(command, true)).await
    }

    // ---- hold (Backlog) ----

    async fn board_drive_hold(&self, ticket: &mut Ticket) -> BoardResult<BoardMoveEffect> {
        let generation = ticket.intent.execution_generation;
        let unsent = ticket.intent.pending_start.is_some()
            || ticket.intent.queue.is_some()
            || ticket.intent.launch_state == BoardLaunchState::Unsent;
        if unsent {
            ticket.intent.pending_start = None;
            ticket.intent.queue = None;
            if ticket.intent.launch_state == BoardLaunchState::Unsent {
                ticket.intent.launch_state = BoardLaunchState::Cancelled;
            }
            ticket.intent.served_intent_revision = ticket.intent.intent_revision;
            self.board_intent_only(ticket)?;
        }
        let quiet = if unsent {
            BoardMoveEffect::CancelledUnsent
        } else {
            BoardMoveEffect::None
        };
        let Some(worker) = self.board_worker(&ticket.item.id)? else {
            return Ok(quiet);
        };
        if let Some(barrier) = ticket
            .intent
            .stop
            .as_ref()
            .filter(|barrier| barrier.execution_generation == generation)
        {
            if barrier_open(barrier) {
                return Ok(if unsent {
                    quiet
                } else {
                    BoardMoveEffect::Coalesced
                });
            }
            if barrier.request_id == ticket.intent.last_request_id {
                return Ok(quiet);
            }
        }
        let (available, live) = self.board_chat_live(&worker.task_id);
        let reviewers_live = self
            .board_stop_scope(&ticket.item.id, generation, &worker.task_id)
            .iter()
            .any(|reviewer| self.board_chat_live(reviewer).1);
        let active = !available
            || live
            || reviewers_live
            || ticket.intent.launch_state == BoardLaunchState::Pending;
        if !active {
            return Ok(quiet);
        }
        self.board_shared_stop(ticket, &worker.task_id, None)
            .await?;
        Ok(BoardMoveEffect::StopRequested {
            execution_generation: generation,
        })
    }

    // ---- run (Ready) ----

    fn board_default_pending(&self, ticket: &Ticket) -> BoardPendingStart {
        BoardPendingStart {
            request_id: ticket.intent.last_request_id.clone(),
            actor: lifecycle_caller().actor(),
            caller_task_id: None,
            destination: BoardDestination::Auto,
            resolved_destination: BoardDestination::NewMain,
            context: BoardContextMode::Fresh,
            execution: None,
            follow_up: BoardFollowUp::None,
            resolved: None,
            unresolved: Vec::new(),
            retry: false,
            adopt_latest_brief: false,
            accepted_at_ms: crate::now_ms(),
        }
    }

    /// Re-bind the recorded requester for a deferred dispatch. Authority
    /// was checked when the request was accepted and stays confined to the
    /// ticket's project.
    pub(crate) fn board_pending_caller(
        &self,
        pending: &BoardPendingStart,
        item: &WorkItem,
    ) -> BoardCaller {
        match &pending.actor {
            BoardActor::User { device_id } => BoardCaller::User {
                device_id: device_id.clone(),
            },
            BoardActor::Agent { task_id, .. } => BoardCaller::Agent {
                task_id: task_id.clone(),
                project_ids: HashSet::from([item.project_id.clone()]),
                granted_scopes: HashSet::from([
                    BOARD_SCOPE_READ.to_owned(),
                    BOARD_SCOPE_WRITE.to_owned(),
                ]),
                explicit_user_request: false,
            },
            BoardActor::Authoring { session_id } => BoardCaller::Authoring {
                session_id: session_id.clone(),
                project_id: item.project_id.clone(),
            },
            BoardActor::Reviewer { .. } | BoardActor::Engine { .. } => lifecycle_caller(),
        }
    }

    /// Record why this dispatch attempt could not run; it is not retried
    /// automatically (a new transition or Retry clears it).
    fn board_record_failure(&self, ticket: &mut Ticket, error: &BoardError) -> BoardResult<()> {
        ticket.intent.launch_failure =
            Some(attention(attention_kind_for(error), error.message(), None));
        ticket.intent.served_intent_revision = ticket.intent.intent_revision;
        ticket.intent.queue = None;
        ticket.intent.pending_start = None;
        self.board_intent_only(ticket)
    }

    async fn board_drive_ready(&self, ticket: &mut Ticket) -> BoardResult<BoardMoveEffect> {
        let generation = ticket.intent.execution_generation;
        if ticket.intent.stop.as_ref().is_some_and(|barrier| {
            barrier.execution_generation == generation && barrier_open(barrier)
        }) {
            let reason = BoardAdmissionReason::WaitingForStop {
                execution_generation: generation,
            };
            if ticket.intent.queue.as_ref() != Some(&reason) {
                ticket.intent.queue = Some(reason.clone());
                self.board_intent_only(ticket)?;
            }
            return Ok(BoardMoveEffect::Queued { reason });
        }
        // PB-2 dependencies are holds, not automatic scheduling. Once a
        // start was blocked, clearing/completing a prerequisite (or restart)
        // cannot launch it. A fresh Start clears this queue in accept_ready.
        if matches!(
            ticket.intent.queue,
            Some(BoardAdmissionReason::Dependency { .. })
        ) {
            let mut changed = ticket.intent.served_intent_revision != ticket.intent.intent_revision
                || ticket.intent.pending_start.is_some();
            ticket.intent.served_intent_revision = ticket.intent.intent_revision;
            ticket.intent.pending_start = None;
            if self.board_open_blockers(&ticket.item.id)?.is_empty() {
                changed = true;
                ticket.intent.queue = None;
                ticket.intent.launch_failure = Some(attention(
                    BoardAttentionKind::DependencyBlocked,
                    "The prerequisites are clear. Choose Start or Retry to run this held ticket.",
                    None,
                ));
            }
            if changed {
                self.board_intent_only(ticket)?;
            }
            return Ok(match ticket.intent.queue.clone() {
                Some(reason) => BoardMoveEffect::Queued { reason },
                None => BoardMoveEffect::None,
            });
        }
        if ticket.intent.served_intent_revision >= ticket.intent.intent_revision {
            return Ok(BoardMoveEffect::Coalesced);
        }
        let mut pending = ticket
            .intent
            .pending_start
            .clone()
            .unwrap_or_else(|| self.board_default_pending(ticket));
        let unknown = ticket.intent.launch_state == BoardLaunchState::Unknown
            || matches!(
                ticket.intent.first_prompt,
                FirstPromptDelivery::Unknown { .. }
            );
        if unknown && !pending.retry {
            return Ok(BoardMoveEffect::Reconciling);
        }
        if matches!(
            ticket.intent.launch_state,
            BoardLaunchState::Pending | BoardLaunchState::Unsent
        ) {
            return Ok(BoardMoveEffect::Coalesced);
        }
        let worker = self.board_worker(&ticket.item.id)?;
        if let Some(link) = &worker {
            let chat = self.workspace.chat(&link.task_id).map_err(storage)?;
            let (available, live) = self.board_chat_live(&link.task_id);
            if !available {
                let reason = BoardAdmissionReason::HostUnavailable {
                    host_device_id: chat.map_or_else(|| "unknown".into(), |chat| chat.device_id),
                };
                if ticket.intent.queue.as_ref() != Some(&reason) {
                    ticket.intent.queue = Some(reason.clone());
                    self.board_intent_only(ticket)?;
                }
                return Ok(BoardMoveEffect::Queued { reason });
            }
            if live && !pending.retry {
                // Already working (a converted active chat, a turn the user
                // started, or this ticket's own run): reuse it in place.
                ticket.intent.served_intent_revision = ticket.intent.intent_revision;
                ticket.intent.pending_start = None;
                ticket.intent.queue = None;
                self.board_intent_only(ticket)?;
                return Ok(BoardMoveEffect::Coalesced);
            }
            if live {
                let reason = BoardAdmissionReason::CheckoutBusy {
                    task_id: link.task_id.clone(),
                };
                if ticket.intent.queue.as_ref() != Some(&reason) {
                    ticket.intent.queue = Some(reason.clone());
                    ticket.intent.pending_start = Some(pending);
                    self.board_intent_only(ticket)?;
                }
                return Ok(BoardMoveEffect::Queued { reason });
            }
        } else if pending.resolved.is_none() || !pending.unresolved.is_empty() {
            // Re-resolve: deferred settings may have been chosen since.
            let caller = self.board_pending_caller(&pending, &ticket.item);
            let resolved = match self.board_plan_destination(
                &caller,
                &ticket.item,
                &pending.destination,
                pending.caller_task_id.as_deref(),
            ) {
                Ok(plan) => self
                    .board_resolve_new_worker(
                        &caller,
                        &ticket.item,
                        pending.execution.as_ref(),
                        &plan,
                    )
                    .await
                    .map(|resolution| (plan, resolution)),
                Err(error) => Err(error),
            };
            match resolved {
                Ok((plan, resolution)) => {
                    pending.resolved_destination = plan.destination;
                    pending.resolved = Some(resolution.resolved);
                    pending.unresolved = resolution.unresolved;
                }
                Err(error) => {
                    self.board_record_failure(ticket, &error)?;
                    return Err(error);
                }
            }
            if !pending.unresolved.is_empty() {
                let reason = BoardAdmissionReason::SettingsUnresolved {
                    fields: pending.unresolved.clone(),
                };
                if ticket.intent.queue.as_ref() != Some(&reason) {
                    ticket.intent.queue = Some(reason.clone());
                }
                ticket.intent.pending_start = Some(pending);
                self.board_intent_only(ticket)?;
                return Ok(BoardMoveEffect::Queued { reason });
            }
        }
        // One admission decision at a time across the host, so two tickets
        // cannot both claim the last slot or the same checkout.
        let _admission = self.board_hub().gate("lifecycle:admission").await;
        if let Some(reason) = self.board_admission(ticket, worker.as_ref(), &pending)? {
            if ticket.intent.queue.as_ref() != Some(&reason) {
                let newly = ticket.intent.queue.is_none();
                ticket.intent.queue = Some(reason.clone());
                ticket.intent.pending_start = Some(pending);
                if newly {
                    let (request_id, payload_hash) = self.board_engine_request(&(
                        "queued",
                        &ticket.item.id,
                        ticket.intent.intent_revision,
                        admission_label(&reason),
                        ticket.version,
                    ))?;
                    self.board_lifecycle_commit(
                        ticket,
                        Commit {
                            request_id,
                            payload_hash,
                            operation: BoardOperation::Start,
                            actor: lifecycle_caller().actor(),
                            touch: Some(&no_change),
                            placement: None,
                            intent: true,
                            rows: Vec::new(),
                            event: Some(WorkItemEventKind::LaunchQueued {
                                reason: admission_label(&reason).into(),
                            }),
                            task_id: None,
                        },
                    )?;
                } else {
                    self.board_intent_only(ticket)?;
                }
            }
            return Ok(BoardMoveEffect::Queued { reason });
        }
        match Box::pin(self.board_dispatch(ticket, worker, pending)).await {
            Ok(effect) => Ok(effect),
            Err(error) => {
                if ticket.intent.launch_failure.is_none() {
                    self.board_reload(ticket)?;
                    self.board_record_failure(ticket, &error)?;
                }
                Err(error)
            }
        }
    }

    /// Admission for one Ready ticket: visible dependency blockers, bounded
    /// project/host/provider concurrency and one live writer per checkout.
    fn board_admission(
        &self,
        ticket: &Ticket,
        worker: Option<&WorkItemTaskLink>,
        pending: &BoardPendingStart,
    ) -> BoardResult<Option<BoardAdmissionReason>> {
        let mut blocked_by = Vec::new();
        for dependency in self
            .store
            .board_dependencies(&ticket.item.id)
            .map_err(storage)?
        {
            if let Some(blocker) = self
                .store
                .board_item(&dependency.blocked_by_id)
                .map_err(storage)?
                && blocker.column != BoardColumn::Done
                && !blocker.archived
            {
                blocked_by.push(blocker.id);
            }
        }
        if !blocked_by.is_empty() {
            return Ok(Some(BoardAdmissionReason::Dependency { blocked_by }));
        }
        let (harness, cwd) = match worker {
            Some(link) => {
                let chat = self.workspace.chat(&link.task_id).map_err(storage)?;
                (
                    chat.as_ref()
                        .and_then(|chat| chat.config.as_ref())
                        .map(|config| config.harness),
                    chat.and_then(|chat| chat.cwd),
                )
            }
            None => {
                let new_worktree = matches!(
                    self.board_checkout_choice(&ticket.item, pending)?,
                    luna_proto::BoardCheckoutChoice::NewWorktree { .. }
                );
                let resolved = pending.resolved.as_ref();
                (
                    resolved.map(|resolved| resolved.harness),
                    resolved
                        .filter(|_| !new_worktree)
                        .map(|resolved| resolved.cwd.clone()),
                )
            }
        };
        let active = self.board_active_executions(&ticket.item.id)?;
        if active
            .iter()
            .filter(|execution| execution.project_id == ticket.item.project_id)
            .count()
            >= BOARD_PROJECT_CAPACITY as usize
        {
            return Ok(Some(BoardAdmissionReason::ProjectCapacity {
                limit: BOARD_PROJECT_CAPACITY,
            }));
        }
        if active.len() >= BOARD_HOST_CAPACITY as usize {
            return Ok(Some(BoardAdmissionReason::HostCapacity {
                limit: BOARD_HOST_CAPACITY,
            }));
        }
        if let Some(harness) = harness
            && active
                .iter()
                .filter(|execution| execution.harness == Some(harness))
                .count()
                >= BOARD_PROVIDER_CAPACITY as usize
        {
            return Ok(Some(BoardAdmissionReason::ProviderCapacity {
                harness,
                limit: BOARD_PROVIDER_CAPACITY,
            }));
        }
        if let Some(cwd) = cwd.as_deref().and_then(checkout_key)
            && let Some(other) = active
                .iter()
                .find(|execution| execution.cwd.as_deref() == Some(cwd.as_str()))
        {
            return Ok(Some(BoardAdmissionReason::CheckoutBusy {
                task_id: other.task_id.clone(),
            }));
        }
        Ok(None)
    }

    /// Board executions currently holding capacity: Ready tickets whose
    /// worker has a live run or a launch not yet confirmed.
    fn board_active_executions(&self, exclude: &str) -> BoardResult<Vec<ActiveExecution>> {
        let mut active = Vec::new();
        for intent in self
            .store
            .board_intents_needing_reconcile(RECONCILE_BATCH)
            .map_err(storage)?
        {
            if intent.work_item_id == exclude || intent.desired != BoardColumn::Ready {
                continue;
            }
            let task = match intent.working_task_id.clone() {
                Some(task) => task,
                None => match self.board_worker(&intent.work_item_id)? {
                    Some(link) => link.task_id,
                    None => continue,
                },
            };
            let live = matches!(
                intent.launch_state,
                BoardLaunchState::Unsent | BoardLaunchState::Pending
            ) || self.sessions.turn_in_flight(&task);
            if !live {
                continue;
            }
            let Some(item) = self
                .store
                .board_item(&intent.work_item_id)
                .map_err(storage)?
            else {
                continue;
            };
            let chat = self.workspace.chat(&task).map_err(storage)?;
            active.push(ActiveExecution {
                project_id: item.project_id,
                harness: chat
                    .as_ref()
                    .and_then(|chat| chat.config.as_ref())
                    .map(|config| config.harness),
                cwd: chat
                    .and_then(|chat| chat.cwd)
                    .as_deref()
                    .and_then(checkout_key),
                task_id: task,
            });
        }
        Ok(active)
    }

    /// The checkout a new worker gets: a new worktree only when chosen
    /// (Start, ticket, or the project default for a new main chat), never
    /// silently; otherwise the parent's or project's current checkout.
    fn board_checkout_choice(
        &self,
        item: &WorkItem,
        pending: &BoardPendingStart,
    ) -> BoardResult<luna_proto::BoardCheckoutChoice> {
        let project_checkout = self
            .workspace
            .space(&item.project_id)
            .map_err(storage)?
            .and_then(|space| space.execution_defaults.checkout)
            .filter(|_| matches!(pending.resolved_destination, BoardDestination::NewMain))
            .map(|choice| match choice {
                luna_proto::ProjectCheckout::Local => luna_proto::BoardCheckoutChoice::Current,
                luna_proto::ProjectCheckout::NewWorktree => {
                    luna_proto::BoardCheckoutChoice::NewWorktree {
                        branch: None,
                        base: None,
                    }
                }
            });
        Ok(pending
            .execution
            .as_ref()
            .and_then(|execution| execution.checkout.clone())
            .or_else(|| item.implementation.checkout.clone())
            .or(project_checkout)
            .unwrap_or(luna_proto::BoardCheckoutChoice::Current))
    }

    /// Create the ticket's first working chat once, through the chat-model
    /// slice, bound to the ticket (frozen original container).
    async fn board_create_worker(
        &self,
        ticket: &mut Ticket,
        pending: &BoardPendingStart,
    ) -> BoardResult<BoardChatCreated> {
        let item = ticket.item.clone();
        let resolved = pending
            .resolved
            .clone()
            .ok_or_else(|| BoardError::Unavailable {
                reason: "The ticket's settings were not resolved before dispatch.".into(),
            })?;
        let caller = self.board_pending_caller(pending, &item);
        let checkout = self.board_checkout_choice(&item, pending)?;
        let execution = BoardExecutionProfile {
            harness: Some(resolved.harness),
            provider_account_id: resolved.provider_account_id.clone(),
            model: resolved.model.clone(),
            reasoning: resolved.reasoning,
            model_options: resolved.model_options.clone(),
            permission_mode: Some(resolved.permission_mode),
            checkout: Some(checkout),
            deferred: false,
        };
        // One creation identity per ticket: a retry after an uncertain
        // result returns the same chat instead of another one.
        let request_id = format!("lcw.{}", &board_digest(&("worker", &item.id))?[..40]);
        let agent_source = match &pending.actor {
            BoardActor::Agent { task_id, .. } => Some(task_id.clone()),
            _ => None,
        };
        for attempt in 0..3 {
            let binding = Some(BoardTicketBinding {
                work_item_id: item.id.clone(),
                expected_revision: ticket.item.revision,
            });
            let created = match &pending.resolved_destination {
                BoardDestination::SideOf { main_task_id } => {
                    Box::pin(self.board_create_side_chat(
                        &caller,
                        BoardCreateSideChatRequest {
                            request_id: request_id.clone(),
                            caller_task_id: pending
                                .caller_task_id
                                .clone()
                                .or_else(|| agent_source.clone())
                                .unwrap_or_else(|| main_task_id.clone()),
                            context: pending.context.clone(),
                            title: Some(item.title.clone()),
                            prompt: None,
                            start: false,
                            ticket: binding,
                            execution: Some(execution.clone()),
                        },
                    ))
                    .await
                }
                BoardDestination::SiblingOf { side_task_id } => {
                    Box::pin(self.board_create_side_chat(
                        &caller,
                        BoardCreateSideChatRequest {
                            request_id: request_id.clone(),
                            caller_task_id: side_task_id.clone(),
                            context: BoardContextMode::Fresh,
                            title: Some(item.title.clone()),
                            prompt: None,
                            start: false,
                            ticket: binding,
                            execution: Some(execution.clone()),
                        },
                    ))
                    .await
                }
                BoardDestination::NewMain | BoardDestination::Auto => {
                    Box::pin(self.board_create_main_chat(
                        &caller,
                        BoardCreateMainChatRequest {
                            request_id: request_id.clone(),
                            project_id: item.project_id.clone(),
                            from_task_id: agent_source.clone(),
                            handoff_brief: agent_source.as_ref().map(|_| {
                                format!(
                                    "Ticket {}: {}. The ticket brief (revision {}) arrives as this chat's first board instruction.",
                                    item.key, item.title, item.brief_revision
                                )
                            }),
                            title: Some(item.title.clone()),
                            start: false,
                            ticket: binding,
                            execution: Some(execution.clone()),
                        },
                    ))
                    .await
                }
            };
            match created {
                Err(BoardError::StaleRevision { .. } | BoardError::Busy { .. }) if attempt < 2 => {
                    self.board_reload(ticket)?;
                    continue;
                }
                other => {
                    self.board_reload(ticket)?;
                    return other;
                }
            }
        }
        unreachable!("the loop returns on its last attempt")
    }

    /// Facts a continuation reports about the work already done.
    fn board_continuation_context(
        &self,
        ticket: &Ticket,
        launches: &[LaunchSnapshot],
    ) -> ContinuationContext {
        let intent = &ticket.intent;
        let stop_reason = intent.stop.as_ref().map(|barrier| match barrier.state {
            BoardStopState::Stopped => format!(
                "the previous run (generation {}) was stopped by the user and ended",
                barrier.execution_generation
            ),
            BoardStopState::Unknown => format!(
                "a Stop of generation {} could not be verified",
                barrier.execution_generation
            ),
            _ => format!(
                "a Stop of generation {} was requested",
                barrier.execution_generation
            ),
        });
        let stop_reason = stop_reason.or_else(|| match intent.launch_state {
            BoardLaunchState::Failed => {
                Some("the previous instruction could not be delivered".into())
            }
            BoardLaunchState::Delivered => Some("the previous run ended".into()),
            _ => None,
        });
        let runs = self.board_review_runs_of(&ticket.item.id);
        let latest_review = runs.iter().find_map(|run| {
            run.result.as_ref().map(|result| {
                (
                    result.verdict,
                    result
                        .findings
                        .iter()
                        .filter(|finding| finding.blocking)
                        .count() as u32,
                )
            })
        });
        ContinuationContext {
            last_phase: intent.observed_phase,
            stop_reason,
            last_sent_brief_revision: launches.iter().map(|launch| launch.brief_revision).max(),
            review_passes: intent.review_passes,
            repair_rounds: intent.repair_rounds,
            latest_review,
            converted: intent.first_prompt == FirstPromptDelivery::NotApplicable
                && launches.is_empty(),
        }
    }

    /// Admit one execution: create the worker if needed, freeze the launch
    /// snapshot and intent before sending, then send once.
    async fn board_dispatch(
        &self,
        ticket: &mut Ticket,
        worker: Option<WorkItemTaskLink>,
        pending: BoardPendingStart,
    ) -> BoardResult<BoardMoveEffect> {
        // A saved issue may have changed since import. Record fresh source
        // state before dispatch, but keep the user's approved brief intact.
        if self
            .board_issue_revalidate(&ticket.item.id)
            .await?
            .is_some()
        {
            self.board_reload(ticket)?;
        }
        let (task_id, main_parent_id, resolved, destination) = match worker {
            Some(link) => {
                let main = match &link.original_container {
                    Some(OriginalContainer::SideOf { main_task_id }) => Some(main_task_id.clone()),
                    _ => None,
                };
                let destination = match &main {
                    Some(main) => BoardDestination::SideOf {
                        main_task_id: main.clone(),
                    },
                    None => BoardDestination::NewMain,
                };
                let resolved = self.board_worker_settings(&ticket.item, &link.task_id)?;
                (link.task_id, main, resolved, destination)
            }
            None => {
                let created = Box::pin(self.board_create_worker(ticket, &pending)).await?;
                let mut resolved = created.settings.clone();
                if let Some(frozen) = &pending.resolved {
                    resolved.sources = frozen.sources.clone();
                    if resolved.cwd.is_empty() {
                        resolved.cwd = frozen.cwd.clone();
                    }
                }
                (
                    created.task_id,
                    created.main_parent_id,
                    resolved,
                    pending.resolved_destination.clone(),
                )
            }
        };
        let item = ticket.item.clone();
        let generation = ticket.intent.execution_generation + 1;
        let kind = if pending.retry {
            BoardInstructionKind::Retry
        } else {
            match ticket.intent.first_prompt {
                FirstPromptDelivery::Delivered { .. } | FirstPromptDelivery::NotApplicable => {
                    BoardInstructionKind::Continuation
                }
                _ => BoardInstructionKind::FirstStart,
            }
        };
        let launches = self.store.board_launches(&item.id, 64).map_err(storage)?;
        let previous = launches.first();
        let (title, description, brief_revision) =
            match previous.filter(|_| pending.retry && !pending.adopt_latest_brief) {
                Some(previous) => (
                    previous.sent_title.clone(),
                    previous.sent_description.clone(),
                    previous.brief_revision,
                ),
                None => (
                    item.title.clone(),
                    item.description.clone(),
                    item.brief_revision,
                ),
            };
        let continuation = (kind != BoardInstructionKind::FirstStart)
            .then(|| self.board_continuation_context(ticket, &launches));
        let reviewer = if item.review.harness.is_some() || item.review.model.is_some() {
            "the configured reviewer"
        } else {
            "the implementation model in a separate reviewer chat"
        };
        let text = build_instruction(&InstructionInput {
            kind,
            key: &item.key,
            work_item_id: &item.id,
            generation,
            brief_revision,
            title: &title,
            description: &description,
            completion: &item.completion,
            reviewer,
            context_refs: &item.context_refs,
            continuation,
        });
        // The brief's images and `@`/`$` selections go with the first prompt
        // the working chat receives (a first start, or its retry), named in
        // the text like a chat message's attachments.
        let first_prompt = !matches!(
            ticket.intent.first_prompt,
            FirstPromptDelivery::Delivered { .. } | FirstPromptDelivery::NotApplicable
        );
        let (attachments, input_items) = if first_prompt {
            match previous.filter(|_| pending.retry && !pending.adopt_latest_brief) {
                Some(previous) => (previous.attachments.clone(), previous.input_items.clone()),
                None => (item.attachments.clone(), item.input_items.clone()),
            }
        } else {
            (Vec::new(), Vec::new())
        };
        let text = with_attachment_trailer(&text, &attachments);
        let digest = board_digest(&(
            "launch",
            &item.id,
            generation,
            ticket.intent.intent_revision,
        ))?;
        let review_base = self
            .board_resolve_review_base(&item.id, &resolved.cwd)
            .await?;
        let launch = LaunchSnapshot {
            id: format!("ls-{}", &digest[..32]),
            work_item_id: item.id.clone(),
            launch_request_id: format!("bl-{}", &digest[..40]),
            execution_generation: generation,
            intent_revision: ticket.intent.intent_revision,
            brief_revision,
            kind,
            sent_title: title,
            sent_description: description,
            instruction_hash: instruction_hash(&text),
            instruction_text: text,
            context_refs: item.context_refs.clone(),
            attachments,
            input_items,
            issue_context: self.board_issue_launch_context(&item.id)?,
            review_base,
            requested: pending
                .execution
                .clone()
                .filter(|execution| !execution.is_inherited())
                .unwrap_or_else(|| item.implementation.clone()),
            resolved,
            destination,
            context_mode: pending.context.clone(),
            task_id: task_id.clone(),
            main_parent_id,
            command_id: format!("board-launch-{}", &digest[..32]),
            message_id: format!("board-msg-{}", &digest[..32]),
            command_receipt: None,
            follow_up: pending.follow_up,
            result_recipient_task_id: (pending.follow_up != BoardFollowUp::None)
                .then(|| self.board_result_recipient(&pending))
                .flatten(),
            created_by: pending.actor.clone(),
            created_at_ms: crate::now_ms(),
        };
        let intent = &mut ticket.intent;
        intent.execution_generation = generation;
        intent.launch_snapshot_id = Some(launch.id.clone());
        intent.active_command_id = Some(launch.command_id.clone());
        intent.active_message_id = Some(launch.message_id.clone());
        intent.active_turn_id = None;
        intent.working_task_id = Some(task_id.clone());
        intent.launch_state = BoardLaunchState::Unsent;
        intent.launch_detail = None;
        intent.launch_boot_id = Some(self.cleanup_boot_id.clone());
        intent.launch_failure = None;
        intent.served_intent_revision = intent.intent_revision;
        intent.queue = None;
        intent.pending_start = None;
        intent.stop = None;
        intent.needs_reconcile = true;
        let sends_brief = match kind {
            BoardInstructionKind::FirstStart => true,
            BoardInstructionKind::Retry => !matches!(
                intent.first_prompt,
                FirstPromptDelivery::Delivered { .. } | FirstPromptDelivery::NotApplicable
            ),
            _ => false,
        };
        if sends_brief {
            intent.first_prompt = FirstPromptDelivery::Pending {
                command_id: launch.command_id.clone(),
                message_id: launch.message_id.clone(),
            };
        }
        if pending.retry {
            // A deliberate user-authorized retry opens a new review cycle.
            intent.review_passes = 0;
            intent.repair_rounds = 0;
        }
        let (request_id, payload_hash) = self.board_engine_request(&("launch", &launch.id))?;
        let launch_id = launch.id.clone();
        self.board_lifecycle_commit(
            ticket,
            Commit {
                request_id,
                payload_hash,
                operation: BoardOperation::Start,
                actor: lifecycle_caller().actor(),
                touch: Some(&no_change),
                placement: None,
                intent: true,
                rows: vec![BoardRowWrite::Launch(Box::new(launch))],
                event: Some(WorkItemEventKind::LaunchAccepted {
                    launch_snapshot_id: launch_id.clone(),
                    instruction: kind,
                    execution_generation: generation,
                    task_id: task_id.clone(),
                }),
                task_id: Some(task_id),
            },
        )?;
        Box::pin(self.board_issue_launch(ticket)).await?;
        Ok(if kind == BoardInstructionKind::FirstStart {
            BoardMoveEffect::FirstStart {
                launch_snapshot_id: launch_id,
            }
        } else {
            BoardMoveEffect::Continue {
                launch_snapshot_id: launch_id,
            }
        })
    }

    /// The chat that asked for a report (the acting chat of the start).
    pub(crate) fn board_result_recipient(&self, pending: &BoardPendingStart) -> Option<String> {
        match &pending.actor {
            BoardActor::Agent { task_id, .. } => Some(task_id.clone()),
            _ => pending.caller_task_id.clone(),
        }
    }
}
