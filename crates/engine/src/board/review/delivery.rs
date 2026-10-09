//! Reviewer placement and creation (through the chat-model slice), packet
//! delivery to the reviewer, repair requests back to the implementer and
//! the result cards in the ticket chat. Every effect has a deterministic
//! identity derived from the run, so a retry after a crash or a lost
//! response reads the original receipt instead of acting twice.
use super::*;
use luna_proto::{
    BoardChatIdentityRequest, BoardChatRole, BoardContextMode, BoardCreateSideChatRequest,
    BoardDestination, BoardExecutionProfile, BoardFollowUp, BoardInstructionKind, Chat,
    LaunchSnapshot, OriginalContainer, ResolvedExecution, ReviewerPlacement, SandboxLevel,
};
use sha2::Sha256;

pub(super) fn packet_message_id(run_id: &str) -> String {
    format!("{run_id}:packet")
}

pub(super) fn repair_message_id(run_id: &str) -> String {
    format!("{run_id}:repair")
}

fn result_notice_id(run_id: &str) -> String {
    format!("{run_id}:result")
}

/// Main ticket T → a side chat directly under T. Side ticket T under main M
/// → a fresh sibling under M (the ticket stays the workflow owner). From the
/// frozen original container first, then durable lineage.
pub(super) fn placement(
    working: &WorkItemTaskLink,
    primary: &Chat,
    workspace: &WorkspaceHost,
) -> (ReviewerPlacement, String) {
    match &working.original_container {
        Some(OriginalContainer::SideOf { main_task_id }) => {
            (ReviewerPlacement::SiblingUnderMain, main_task_id.clone())
        }
        Some(OriginalContainer::MainChat) => (ReviewerPlacement::UnderTicket, primary.id.clone()),
        None => {
            let parent = workspace.side_chat_parent(&primary.id).or_else(|| {
                primary
                    .child_thread
                    .as_ref()
                    .map(|child| child.parent_task_id.clone())
            });
            match parent {
                Some(main) => {
                    // Legacy nesting: walk to the top-level main ancestor.
                    let mut main = main;
                    for _ in 0..16 {
                        let next = workspace.side_chat_parent(&main).or_else(|| {
                            workspace
                                .chat(&main)
                                .ok()
                                .flatten()
                                .and_then(|chat| chat.child_thread.map(|c| c.parent_task_id))
                        });
                        match next {
                            Some(next) if next != main => main = next,
                            _ => break,
                        }
                    }
                    (ReviewerPlacement::SiblingUnderMain, main)
                }
                None => (ReviewerPlacement::UnderTicket, primary.id.clone()),
            }
        }
    }
}

/// The reviewer's chat configuration: the resolved profile with
/// review-constrained access (Ask approvals, read-only sandbox).
fn reviewer_config(resolved: &ResolvedExecution) -> ChatConfig {
    ChatConfig {
        harness: resolved.harness,
        provider_account_id: resolved.provider_account_id.clone(),
        model: resolved.model.clone(),
        reasoning: resolved.reasoning,
        model_options: resolved.model_options.clone(),
        sandbox: SandboxLevel::ReadOnly,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn engine_command(task_id: &str, command_id: &str, intent: TaskCommandIntent) -> TaskCommandParams {
    let mut command = legacy_task_command(task_id.into(), intent);
    command.command_id = command_id.into();
    command.actor_id = ACTOR_ID.into();
    command.client_id = "luna-board-review".into();
    command
}

fn harness_label(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::ClaudeCode => "Claude Code",
        HarnessId::Codex => "Codex",
        HarnessId::Opencode => "OpenCode",
        HarnessId::Mock => "Test harness",
        _ => "Provider",
    }
}

/// "Claude Code / opus-5.5" from what was observed, else what was selected.
pub(super) fn reviewer_label(run: &ReviewRun) -> String {
    let harness = run
        .profile
        .observed_harness
        .unwrap_or(run.profile.resolved.harness);
    let model = run
        .profile
        .observed_model
        .as_deref()
        .or(run.profile.resolved.model.as_deref())
        .unwrap_or("provider default");
    format!("{} / {model}", harness_label(harness))
}

fn findings_text(findings: &[ReviewFinding], limit: usize) -> String {
    let mut text = String::new();
    for (index, finding) in findings.iter().take(limit).enumerate() {
        let location = match (&finding.file, finding.line) {
            (Some(file), Some(line)) => format!("{file}:{line}"),
            (Some(file), None) => file.clone(),
            _ => "(no location)".into(),
        };
        text.push_str(&format!(
            "{}. [{:?}{}] {location} — {}\n   Required: {}\n",
            index + 1,
            finding.severity,
            if finding.blocking { ", blocking" } else { "" },
            finding.explanation.replace('\n', " "),
            finding.required_action.replace('\n', " ")
        ));
    }
    if findings.len() > limit {
        text.push_str(&format!(
            "… {} more finding(s) in the review run.\n",
            findings.len() - limit
        ));
    }
    text
}

impl EngineRpc {
    /// Starting/OutcomeUnknown: make sure the reviewer chat exists (once),
    /// constrain its access, then queue the stored packet (once).
    pub(super) async fn review_deliver_packet(
        &self,
        item: &WorkItem,
        run: &ReviewRun,
    ) -> BoardResult<bool> {
        let Some(reviewer) = self.review_ensure_reviewer(item, run).await? else {
            return Ok(false);
        };
        let run = self
            .store
            .board_review_run(&run.id)
            .map_err(storage)?
            .ok_or_else(|| storage("review run disappeared"))?;
        if run.state.terminal() {
            return Ok(true);
        }
        let chat = self
            .workspace
            .chat(&reviewer)
            .map_err(storage)?
            .ok_or_else(|| BoardError::Unavailable {
                reason: "The reviewer chat disappeared.".into(),
            })?;
        let config = reviewer_config(&run.profile.resolved);
        if chat.config.as_ref().is_none_or(|current| {
            current.harness != config.harness
                || current.sandbox != SandboxLevel::ReadOnly
                || current.permission_mode != PermissionMode::Ask
        }) {
            if chat
                .config
                .as_ref()
                .is_some_and(|current| current.harness != config.harness)
            {
                self.review_fail(
                    &run,
                    BoardAttentionKind::ReviewUnavailable,
                    "The reviewer chat runs on a different provider than the selected reviewer.",
                )?;
                return Ok(true);
            }
            let command = engine_command(
                &reviewer,
                &format!("{}:access", run.id),
                TaskCommandIntent::SetConfig {
                    config: config.clone(),
                },
            );
            match self.execute_task_command(command, true).await {
                Ok(receipt) if receipt.status == CommandReceiptStatus::Applied => {}
                Ok(receipt) => {
                    let reason = receipt
                        .reason
                        .map(|reason| reason.message)
                        .unwrap_or_else(|| format!("{:?}", receipt.status));
                    self.review_fail(
                        &run,
                        BoardAttentionKind::ReviewUnavailable,
                        &format!("Reviewer access could not be constrained to read-only: {reason}"),
                    )?;
                    return Ok(true);
                }
                Err(error) => {
                    self.review_mark_unknown(
                        &run,
                        &format!("Reviewer access change is unconfirmed: {error}"),
                    )?;
                    return Ok(false);
                }
            }
        }
        let command_id = packet_message_id(&run.id);
        let receipt = match self
            .doc_host
            .task_command_receipt(&command_id)
            .map_err(storage)?
        {
            Some(receipt) => Ok(receipt),
            None => {
                let Some(text) = self.review_load_packet(&run.id)? else {
                    self.review_fail(
                        &run,
                        BoardAttentionKind::ReviewUnavailable,
                        "The review packet was not persisted; the pass is reviewed again.",
                    )?;
                    return Ok(true);
                };
                self.execute_task_command(
                    engine_command(
                        &reviewer,
                        &command_id,
                        TaskCommandIntent::QueueMessage {
                            message_id: command_id.clone(),
                            text,
                            attachments: Vec::new(),
                            input_items: Vec::new(),
                            hold_for_turn_end: true,
                            follow_up_behavior: FollowUpBehavior::Queue,
                            execution: Some(config),
                        },
                    ),
                    true,
                )
                .await
            }
        };
        match receipt {
            Ok(receipt) if receipt.status == CommandReceiptStatus::Applied => {
                self.review_update_run(
                    &run,
                    "running",
                    |next| {
                        Some(WorkItemEventKind::ReviewStateChanged {
                            review_run_id: next.id.clone(),
                            state: next.state,
                        })
                    },
                    |next, _| {
                        if !matches!(
                            next.state,
                            ReviewRunState::Starting | ReviewRunState::OutcomeUnknown
                        ) {
                            return false;
                        }
                        next.state = ReviewRunState::Running;
                        next.detail = None;
                        true
                    },
                )?;
                Ok(false)
            }
            Ok(receipt) if receipt.status == CommandReceiptStatus::Pending => {
                self.review_mark_unknown(
                    &run,
                    "The review packet's delivery is not confirmed yet.",
                )?;
                Ok(false)
            }
            Ok(receipt) => {
                let reason = receipt
                    .reason
                    .map(|reason| reason.message)
                    .unwrap_or_else(|| format!("{:?}", receipt.status));
                self.review_fail(
                    &run,
                    BoardAttentionKind::ReviewUnavailable,
                    &format!("The review packet was not delivered: {reason}"),
                )?;
                Ok(true)
            }
            Err(error) => {
                // Unknown: the next sweep reads the receipt by the same id.
                self.review_mark_unknown(
                    &run,
                    &format!("Packet delivery outcome unknown: {error}"),
                )?;
                Ok(false)
            }
        }
    }

    fn review_mark_unknown(&self, run: &ReviewRun, detail: &str) -> BoardResult<()> {
        self.review_update_run(
            run,
            "unknown",
            |next| {
                Some(WorkItemEventKind::ReviewStateChanged {
                    review_run_id: next.id.clone(),
                    state: next.state,
                })
            },
            |next, _| {
                if next.state != ReviewRunState::Starting {
                    if next.state == ReviewRunState::OutcomeUnknown {
                        next.detail = Some(detail.into());
                    }
                    return false;
                }
                next.state = ReviewRunState::OutcomeUnknown;
                next.detail = Some(detail.into());
                true
            },
        )?;
        Ok(())
    }

    /// Reuse a compatible idle reviewer of this ticket, or create exactly one
    /// through the chat-model slice (`ChatPurpose::Reviewer` comes from the
    /// Reviewer link). `None` while creation is unconfirmed.
    async fn review_ensure_reviewer(
        &self,
        item: &WorkItem,
        run: &ReviewRun,
    ) -> BoardResult<Option<String>> {
        if let Some(reviewer) = &run.reviewer_task_id {
            return Ok(Some(reviewer.clone()));
        }
        let runs = self
            .store
            .board_review_runs(&item.id, RUNS_READ)
            .map_err(storage)?;
        let owned = self.review_owned_reviewers(item, &runs)?;
        let reuse = runs.iter().find_map(|other| {
            let reviewer = other.reviewer_task_id.as_ref()?;
            let compatible = other.id != run.id
                && owned.contains(reviewer)
                && other.placement == run.placement
                && other.display_main_parent_id == run.display_main_parent_id
                && other.profile.resolved.harness == run.profile.resolved.harness
                && other.profile.resolved.provider_account_id
                    == run.profile.resolved.provider_account_id
                && other.profile.resolved.model == run.profile.resolved.model
                && other.profile.resolved.reasoning == run.profile.resolved.reasoning;
            let chat = self.workspace.chat(reviewer).ok().flatten()?;
            (compatible
                && !chat.archived
                && chat.config.as_ref().map(|config| config.harness)
                    == Some(run.profile.resolved.harness))
            .then(|| reviewer.clone())
        });
        let reviewer = match reuse {
            // Reuse across rounds; a reviewer still finishing its previous
            // turn is waited for, never replaced by a second chat.
            Some(reviewer)
                if self.sessions.active_turn_id(&reviewer).is_some()
                    || self.sessions.has_live_background_work(&reviewer) =>
            {
                return Ok(None);
            }
            Some(reviewer) => reviewer,
            None => {
                let resolved = &run.profile.resolved;
                let request = BoardCreateSideChatRequest {
                    request_id: format!("{}:chat", run.id),
                    caller_task_id: run.primary_task_id.clone(),
                    context: BoardContextMode::Fresh,
                    title: Some(format!("Review · {} · {}", item.key, item.title)),
                    prompt: None,
                    start: false,
                    ticket: None,
                    execution: Some(BoardExecutionProfile {
                        harness: Some(resolved.harness),
                        provider_account_id: resolved.provider_account_id.clone(),
                        model: resolved.model.clone(),
                        reasoning: resolved.reasoning,
                        model_options: resolved.model_options.clone(),
                        permission_mode: Some(PermissionMode::Ask),
                        checkout: None,
                        deferred: false,
                    }),
                };
                match self.board_create_side_chat(&engine_caller(), request).await {
                    Ok(created) => {
                        let parent_ok = created.role == BoardChatRole::Side
                            && created.main_parent_id.as_deref()
                                == Some(run.display_main_parent_id.as_str());
                        if !parent_ok || created.task_id == run.primary_task_id {
                            self.review_fail(
                                run,
                                BoardAttentionKind::ReviewUnavailable,
                                &format!(
                                    "The reviewer chat was placed under {:?}, not {}; it is not used.",
                                    created.main_parent_id, run.display_main_parent_id
                                ),
                            )?;
                            return Ok(None);
                        }
                        created.task_id
                    }
                    Err(
                        error @ (BoardError::OutcomeUnknown { .. }
                        | BoardError::Busy { .. }
                        | BoardError::Storage { .. }),
                    ) => {
                        // Retried with the same creation key: never a second chat.
                        self.review_mark_unknown(
                            run,
                            &format!("Reviewer creation is unconfirmed: {}", error.message()),
                        )?;
                        return Ok(None);
                    }
                    Err(error) => {
                        self.review_fail(
                            run,
                            BoardAttentionKind::ReviewUnavailable,
                            &format!(
                                "The reviewer side chat could not be created: {}",
                                error.message()
                            ),
                        )?;
                        return Ok(None);
                    }
                }
            }
        };
        let link_id = format!(
            "wl-{}",
            &board_digest(&("board-reviewer-link", &item.id, &reviewer))?[..32]
        );
        // One Reviewer link per reviewer chat, whoever recorded it (the
        // chat-model slice's reviewer creation may link it itself).
        let linked = self
            .store
            .board_task_links_for_item(&item.id, true)
            .map_err(storage)?
            .into_iter()
            .any(|link| link.task_id == reviewer && link.role == TaskLinkRole::Reviewer);
        let link = WorkItemTaskLink {
            id: link_id,
            work_item_id: item.id.clone(),
            task_id: reviewer.clone(),
            role: TaskLinkRole::Reviewer,
            active: true,
            chat_role: Some(BoardChatRole::Side),
            original_container: None,
            linked_by: engine_actor(),
            linked_at_ms: crate::now_ms(),
            unlinked_at_ms: None,
            source_message_id: None,
            context_refs: Vec::new(),
            launch_snapshot_id: None,
            result_subscriptions: Vec::new(),
        };
        let reviewer_for_plan = reviewer.clone();
        self.review_mutate(&item.id, Some(&run.id), |ctx| {
            let Some(current) = ctx.run.clone() else {
                return Ok((None, ()));
            };
            if current.reviewer_task_id.is_some() {
                return Ok((None, ()));
            }
            let mut next = current.clone();
            next.reviewer_task_id = Some(reviewer_for_plan.clone());
            next.updated_at_ms = crate::now_ms();
            let mut write = ReviewWrite::new(
                format!("{}:v{}:reviewer", current.id, current.version),
                BoardOperation::ReviewStart,
            )
            .row(run_row(&next, Some(current.version)));
            if !linked {
                write = write
                    .row(BoardRowWrite::TaskLink(Box::new(link.clone())))
                    .event(WorkItemEventKind::ChatLinked {
                        task_id: reviewer_for_plan.clone(),
                        role: TaskLinkRole::Reviewer,
                    });
            }
            Ok((Some(write), ()))
        })?;
        Ok(Some(reviewer))
    }

    /// The canonical placement (A3 identity when available, else the
    /// frozen container and lineage).
    pub(super) fn review_placement(
        &self,
        working: &WorkItemTaskLink,
        primary: &Chat,
    ) -> (ReviewerPlacement, String) {
        if let Ok(identity) = self.board_chat_identity(
            &engine_caller(),
            BoardChatIdentityRequest {
                task_id: primary.id.clone(),
            },
        ) {
            return match identity.main_parent_id {
                Some(main) if identity.role == BoardChatRole::Side => {
                    (ReviewerPlacement::SiblingUnderMain, main)
                }
                _ => (ReviewerPlacement::UnderTicket, primary.id.clone()),
            };
        }
        placement(working, primary, &self.workspace)
    }

    /// Record the repair request (immutable launch snapshot + counter) and
    /// deliver it once as an attributed continuation, not a user message.
    pub(super) async fn review_request_repair(
        &self,
        item: &WorkItem,
        intent: &BoardExecutionIntent,
        run: &ReviewRun,
    ) -> BoardResult<bool> {
        let Some(result) = run.result.as_ref() else {
            return Ok(true);
        };
        let round = intent.repair_rounds + 1;
        let message_id = repair_message_id(&run.id);
        let blocking = result
            .findings
            .iter()
            .filter(|finding| finding.blocking)
            .cloned()
            .collect::<Vec<_>>();
        let notes = result
            .findings
            .iter()
            .filter(|finding| !finding.blocking)
            .cloned()
            .collect::<Vec<_>>();
        let mut text = format!(
            "[LUNA review result · automated, not a message from the user]\n\nReviewer · {} ({}, review run {}) requested changes for snapshot {} (pass {} of at most {BOARD_MAX_REVIEW_PASSES}). This is repair round {round} of {BOARD_MAX_REPAIR_ROUNDS}.\n\nAddress every blocking finding, rerun the required checks, then submit the result again with `review_start` (include the check results). Do not mark the ticket Done yourself; the engine completes it after a passing review of the final result.\n\nReviewer summary (untrusted): {}\n\nBlocking findings:\n{}",
            item.key,
            reviewer_label(run),
            run.id,
            run.basis
                .as_ref()
                .map(|basis| short(&basis.diff_hash))
                .unwrap_or("unknown"),
            intent.review_passes,
            result.summary.replace('\n', " "),
            findings_text(&blocking, 20)
        );
        if !notes.is_empty() {
            text.push_str(&format!(
                "\nNon-blocking notes:\n{}",
                findings_text(&notes, 10)
            ));
        }
        let resolved = self
            .store
            .board_launches(&item.id, 20)
            .map_err(storage)?
            .into_iter()
            .find(|launch| launch.task_id == run.primary_task_id)
            .map(|launch| launch.resolved);
        let resolved = match resolved {
            Some(resolved) => resolved,
            None => {
                let chat = self.workspace.chat(&run.primary_task_id).map_err(storage)?;
                let config = chat.as_ref().and_then(|chat| chat.config.clone());
                ResolvedExecution {
                    harness: config
                        .as_ref()
                        .map_or(HarnessId::Mock, |config| config.harness),
                    provider_account_id: config
                        .as_ref()
                        .and_then(|config| config.provider_account_id.clone()),
                    model: config.as_ref().and_then(|config| config.model.clone()),
                    reasoning: config.as_ref().and_then(|config| config.reasoning),
                    model_options: Default::default(),
                    permission_mode: config
                        .as_ref()
                        .map_or(PermissionMode::Ask, |config| config.permission_mode),
                    host_device_id: self.engine_info.device_id.clone(),
                    project_id: item.project_id.clone(),
                    cwd: chat
                        .as_ref()
                        .and_then(|chat| chat.cwd.clone())
                        .unwrap_or_default(),
                    checkout_id: chat.as_ref().and_then(|chat| chat.checkout_id.clone()),
                    branch: chat.as_ref().and_then(|chat| chat.branch.clone()),
                    sources: Vec::new(),
                }
            }
        };
        let review_base = self
            .board_resolve_review_base(&item.id, &resolved.cwd)
            .await?;
        let launch = LaunchSnapshot {
            id: format!(
                "ls-{}",
                &board_digest(&("board-review-repair", &run.id))?[..32]
            ),
            work_item_id: item.id.clone(),
            launch_request_id: message_id.clone(),
            execution_generation: run.execution_generation,
            intent_revision: intent.intent_revision,
            brief_revision: item.brief_revision,
            kind: BoardInstructionKind::RepairRequest,
            sent_title: item.title.clone(),
            sent_description: String::new(),
            instruction_hash: crate::repos::hex(&Sha256::digest(text.as_bytes())),
            instruction_text: text,
            attachments: Vec::new(),
            input_items: Vec::new(),
            context_refs: Vec::new(),
            issue_context: self.board_issue_launch_context(&item.id)?,
            review_base,
            requested: item.implementation.clone(),
            resolved,
            destination: BoardDestination::Auto,
            context_mode: BoardContextMode::Fresh,
            task_id: run.primary_task_id.clone(),
            main_parent_id: (run.placement == ReviewerPlacement::SiblingUnderMain)
                .then(|| run.display_main_parent_id.clone()),
            command_id: message_id.clone(),
            message_id: message_id.clone(),
            command_receipt: None,
            follow_up: BoardFollowUp::None,
            result_recipient_task_id: None,
            created_by: engine_actor(),
            created_at_ms: crate::now_ms(),
        };
        let generation = run.execution_generation;
        let (applied, _) = self.review_mutate(&item.id, Some(&run.id), |ctx| {
            let Some(current) = ctx.run.clone() else {
                return Ok((None, false));
            };
            if current.outcome.is_some()
                || not_current(&ctx.item, ctx.intent.as_ref(), generation).is_some()
            {
                return Ok((None, false));
            }
            let mut intent = ctx.intent.clone().expect("checked by not_current");
            if intent.repair_rounds >= BOARD_MAX_REPAIR_ROUNDS {
                return Ok((None, false));
            }
            intent.repair_rounds += 1;
            intent.observed_phase = BoardPhase::Fixing;
            intent.updated_at_ms = crate::now_ms();
            let mut next = current.clone();
            next.outcome = Some(ReviewRunOutcome::RepairRequested {
                launch_snapshot_id: launch.id.clone(),
                repair_round: intent.repair_rounds,
            });
            next.updated_at_ms = crate::now_ms();
            Ok((
                Some(
                    ReviewWrite::new(
                        format!("{}:v{}:repair", current.id, current.version),
                        BoardOperation::ReviewSubmit,
                    )
                    .row(BoardRowWrite::Launch(Box::new(launch.clone())))
                    .row(intent_row(&intent))
                    .row(run_row(&next, Some(current.version)))
                    .event(WorkItemEventKind::LaunchAccepted {
                        launch_snapshot_id: launch.id.clone(),
                        instruction: BoardInstructionKind::RepairRequest,
                        execution_generation: generation,
                        task_id: current.primary_task_id.clone(),
                    }),
                ),
                true,
            ))
        })?;
        if !applied {
            return Ok(true);
        }
        let run = self
            .store
            .board_review_run(&run.id)
            .map_err(storage)?
            .ok_or_else(|| storage("review run disappeared"))?;
        self.review_send_repair(&run).await
    }

    /// Deliver a recorded repair request once (same command/message id).
    pub(super) async fn review_send_repair(&self, run: &ReviewRun) -> BoardResult<bool> {
        let Some(ReviewRunOutcome::RepairRequested {
            launch_snapshot_id, ..
        }) = &run.outcome
        else {
            return Ok(true);
        };
        if run.delivered_id.is_some() {
            return Ok(true);
        }
        let item = self.board_load(&run.work_item_id)?;
        let Some(launch) = self
            .store
            .board_launches(&item.id, 200)
            .map_err(storage)?
            .into_iter()
            .find(|launch| &launch.id == launch_snapshot_id)
        else {
            return Err(storage("the repair launch snapshot is missing"));
        };
        self.review_result_notice(&item, run, None).await;
        let receipt = match self
            .doc_host
            .task_command_receipt(&launch.command_id)
            .map_err(storage)?
        {
            Some(receipt) => receipt,
            None => match self
                .execute_task_command(
                    engine_command(
                        &run.primary_task_id,
                        &launch.command_id,
                        TaskCommandIntent::QueueMessage {
                            message_id: launch.message_id.clone(),
                            text: launch.instruction_text.clone(),
                            attachments: Vec::new(),
                            input_items: Vec::new(),
                            hold_for_turn_end: true,
                            follow_up_behavior: FollowUpBehavior::Queue,
                            execution: None,
                        },
                    ),
                    true,
                )
                .await
            {
                Ok(receipt) => receipt,
                Err(error) => {
                    tracing::warn!(run = %run.id, %error, "repair delivery outcome unknown; retrying by id");
                    return Ok(false);
                }
            },
        };
        let delivered = match receipt.status {
            CommandReceiptStatus::Applied => true,
            // Already claimed for delivery under this id: never a duplicate.
            CommandReceiptStatus::Conflict
                if receipt
                    .reason
                    .as_ref()
                    .is_some_and(|reason| reason.code == CommandReasonCode::QueueClaimed) =>
            {
                true
            }
            CommandReceiptStatus::Pending => return Ok(false),
            _ => {
                let reason = receipt
                    .reason
                    .map(|reason| reason.message)
                    .unwrap_or_else(|| format!("{:?}", receipt.status));
                self.review_update_run(
                    run,
                    "repair-failed",
                    |_| {
                        Some(WorkItemEventKind::AttentionRaised {
                            reason: BoardAttentionKind::DeliveryUnknown,
                        })
                    },
                    |next, _| {
                        if next.delivered_id.is_some() || next.attention.is_some() {
                            return false;
                        }
                        next.attention = Some(BoardAttentionKind::DeliveryUnknown);
                        next.detail =
                            Some(format!("The repair request was not delivered: {reason}"));
                        true
                    },
                )?;
                return Ok(false);
            }
        };
        if delivered {
            let generation = run.execution_generation;
            let recipient = run.primary_task_id.clone();
            let command_id = launch.command_id.clone();
            self.review_update_run(
                run,
                "repair-delivered",
                |_| {
                    Some(WorkItemEventKind::ResultDelivered {
                        recipient_task_id: recipient.clone(),
                        execution_generation: generation,
                    })
                },
                |next, _| {
                    if next.delivered_id.is_some() {
                        return false;
                    }
                    next.delivered_id = Some(command_id.clone());
                    next.attention = None;
                    true
                },
            )?;
        }
        Ok(false)
    }

    /// A result card in the ticket's primary chat (system notice, once per
    /// run). Best effort: the durable record is the review run itself.
    pub(super) async fn review_result_notice(
        &self,
        item: &WorkItem,
        run: &ReviewRun,
        detail: Option<&str>,
    ) {
        let Some(result) = run.result.as_ref() else {
            return;
        };
        let blocking = blocking_count(&result.findings);
        let headline = match (result.verdict, &run.outcome) {
            (ReviewVerdict::Passed, Some(ReviewRunOutcome::Completed { .. })) => {
                if item.completion.required_checks.is_empty() {
                    "Review passed · Done".to_string()
                } else {
                    "Review passed · required checks passed · Done".to_string()
                }
            }
            (ReviewVerdict::Passed, _) => "Review passed".to_string(),
            (ReviewVerdict::ChangesRequested, _) => {
                format!("Review result — {blocking} blocking finding(s)")
            }
            (verdict, _) => format!("Review {verdict:?}"),
        };
        let mut text = format!(
            "{headline}\nReviewer · {} ({}) · review run {} · snapshot {}\n{}\n",
            item.key,
            reviewer_label(run),
            run.id,
            run.basis
                .as_ref()
                .map(|basis| short(&basis.diff_hash))
                .unwrap_or("unknown"),
            result.summary
        );
        if let Some(detail) = detail {
            text.push_str(&format!("Needs attention: {detail}\n"));
        }
        if !result.findings.is_empty() {
            text.push_str(&findings_text(&result.findings, 10));
        }
        let id = result_notice_id(&run.id);
        let task_id = run.primary_task_id.clone();
        let push = || -> Result<(), crate::EngineError> {
            let Some(chat) = self.workspace.chat(&task_id)? else {
                return Ok(());
            };
            if chat.device_id != self.engine_info.device_id {
                return Ok(());
            }
            let handle = self.doc_host.open(&task_id)?;
            if handle
                .doc()
                .read_entries()?
                .iter()
                .any(|entry| entry.id == id)
            {
                return Ok(());
            }
            handle.doc().push_message(&SessionMessageEntry {
                id: id.clone(),
                role: MessageRole::System,
                parts: vec![MessagePart::Text {
                    id: "text".into(),
                    text: text.clone(),
                }],
                created_at: crate::now_ms(),
                device_id: self.engine_info.device_id.clone(),
                status: Some(MessageStatus::Complete),
                continuation_of: None,
            })?;
            self.doc_host.flush_chat_checked(&task_id)
        };
        let _lane = self.doc_host.task_command_gate(&run.primary_task_id).await;
        if let Err(error) = push() {
            tracing::warn!(run = %run.id, %error, "review result card deferred");
        }
    }

    /// Shared Stop for the exact reviewer: drop an unsent packet, interrupt
    /// a live turn. Never touches the primary chat or other siblings.
    pub(super) async fn review_stop_reviewer(&self, run: &ReviewRun, reviewer: &str) {
        let packet = packet_message_id(&run.id);
        if let Ok(Some(queued)) = self.doc_host.queued_message(reviewer, &packet) {
            let command = engine_command(
                reviewer,
                &format!("{}:unqueue", run.id),
                TaskCommandIntent::RemoveQueuedMessage {
                    message_id: packet.clone(),
                    expected_queue_revision: queued.revision,
                },
            );
            if let Err(error) = self.execute_task_command(command, true).await {
                tracing::warn!(run = %run.id, %error, "unsent review packet removal deferred");
            }
        }
        if let Some(turn) = self.sessions.active_turn_id(reviewer) {
            let command = engine_command(
                reviewer,
                &format!(
                    "{}:stop:{}",
                    run.id,
                    short(&board_digest(&turn).unwrap_or_default())
                ),
                TaskCommandIntent::Session {
                    command: SessionCommandPayload::Interrupt {},
                    transfers: Vec::new(),
                },
            );
            if let Err(error) = self.execute_task_command(command, true).await {
                tracing::warn!(run = %run.id, %error, "reviewer stop deferred");
            }
        }
    }
}
