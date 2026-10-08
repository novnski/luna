//! Observed phase and the Needs-attention overlay, derived only from engine
//! evidence: the working chat's live run, durable command receipts, the
//! existing attention projection, review runs and the stop barrier. Agent
//! prose (status reports) only contributes an explicit "agent blocked"
//! reason; it never decides the phase.
use super::*;
use luna_proto::{
    AttentionKind, BoardAttentionKind, BoardAttentionReason, ReviewCleanupState, ReviewRun,
    SessionStatus,
};

/// What the engine can observe about one chat right now.
pub(crate) struct ChatRuntime {
    /// The row exists and this host executes it.
    pub available: bool,
    pub in_flight: bool,
    pub status: Option<SessionStatus>,
    pub attention: Vec<BoardAttentionReason>,
}

pub(crate) fn attention(
    kind: BoardAttentionKind,
    detail: impl Into<String>,
    task_id: Option<&str>,
) -> BoardAttentionReason {
    BoardAttentionReason {
        kind,
        detail: detail.into(),
        attention_id: None,
        task_id: task_id.map(str::to_owned),
    }
}

fn map_attention(kind: AttentionKind) -> Option<BoardAttentionKind> {
    Some(match kind {
        AttentionKind::ApprovalRequired => BoardAttentionKind::Approval,
        AttentionKind::UserInputRequired => BoardAttentionKind::Question,
        AttentionKind::TaskFailed => BoardAttentionKind::ProviderFailure,
        AttentionKind::QueuedSendFailed => BoardAttentionKind::DeliveryUnknown,
        AttentionKind::ProviderAuthenticationRequired => BoardAttentionKind::AuthRequired,
        AttentionKind::CompatibilityBlocked => BoardAttentionKind::CompatibilityBlocked,
        AttentionKind::HostDisconnected | AttentionKind::ConnectivityUnavailable => {
            BoardAttentionKind::HostUnavailable
        }
        _ => return None,
    })
}

/// Whether a stop barrier still holds (Ready must wait behind it).
pub(crate) fn barrier_open(barrier: &BoardStopBarrier) -> bool {
    matches!(
        barrier.state,
        BoardStopState::Requested | BoardStopState::Stopping | BoardStopState::Unknown
    )
}

impl EngineRpc {
    /// The ticket's current primary working chat link.
    pub(crate) fn board_worker(&self, work_item_id: &str) -> BoardResult<Option<WorkItemTaskLink>> {
        Ok(self
            .store
            .board_task_links_for_item(work_item_id, true)
            .map_err(storage)?
            .into_iter()
            .find(|link| link.role == TaskLinkRole::Working))
    }

    /// Live runtime evidence for one chat, including the existing engine
    /// attention items (approvals, questions, failures) routed by id.
    pub(crate) fn board_runtime(&self, task_id: &str) -> ChatRuntime {
        let chat = self.workspace.chat(task_id).ok().flatten();
        let available = chat
            .as_ref()
            .is_some_and(|chat| chat.device_id == self.engine_info.device_id);
        let in_flight = available && self.sessions.turn_in_flight(task_id);
        let session = available
            .then(|| self.sessions.session_status(task_id))
            .flatten();
        let mut reasons = Vec::new();
        if let Some(chat) = chat.as_ref().filter(|_| available)
            && let Ok(handle) = self.doc_host.open(task_id)
        {
            let latest = handle.doc().read_latest_assistant_entry().ok().flatten();
            let queue = handle.doc().read_queue().unwrap_or_default();
            for item in crate::attention::project_task(
                &self.engine_info.device_id,
                chat,
                session.as_ref(),
                latest.as_slice(),
                &queue,
            ) {
                if !item.unresolved() {
                    continue;
                }
                if let Some(kind) = map_attention(item.kind) {
                    reasons.push(BoardAttentionReason {
                        kind,
                        detail: item.summary.clone(),
                        attention_id: Some(item.id.clone()),
                        task_id: Some(task_id.to_owned()),
                    });
                }
            }
        }
        ChatRuntime {
            status: session.map(|session| session.status),
            available,
            in_flight,
            attention: reasons,
        }
    }

    /// Review runs of this ticket (newest first, bounded).
    pub(crate) fn board_review_runs_of(&self, work_item_id: &str) -> Vec<ReviewRun> {
        self.store
            .board_review_runs(work_item_id, 20)
            .unwrap_or_default()
    }

    /// Engine-observed phase/attention/queue for one ticket.
    pub(crate) fn board_observation(
        &self,
        item: &WorkItem,
        intent: Option<&BoardExecutionIntent>,
    ) -> BoardObservation {
        let worker = self.board_worker(&item.id).ok().flatten();
        let runtime = worker
            .as_ref()
            .map(|link| (link.task_id.clone(), self.board_runtime(&link.task_id)));
        let generation = intent.map_or(0, |intent| intent.execution_generation);
        let runs = self.board_review_runs_of(&item.id);
        let current_runs: Vec<&ReviewRun> = runs
            .iter()
            .filter(|run| run.execution_generation == generation)
            .collect();
        let reviewing = current_runs.iter().any(|run| !run.state.terminal());
        let latest_result = current_runs
            .iter()
            .find_map(|run| run.result.as_ref().map(|result| (*run, result)));
        let mut reasons = Vec::new();
        let worker_task = runtime.as_ref().map(|(task, _)| task.as_str());
        let in_flight = runtime.as_ref().is_some_and(|(_, rt)| rt.in_flight);
        let available = runtime.as_ref().is_none_or(|(_, rt)| rt.available);
        if let Some((task, rt)) = &runtime {
            if !rt.available {
                reasons.push(attention(
                    BoardAttentionKind::HostUnavailable,
                    "The working chat's host is unavailable; its run cannot be observed.",
                    Some(task),
                ));
            }
            reasons.extend(rt.attention.iter().cloned());
        }
        if let Some(intent) = intent {
            if let Some(failure) = &intent.launch_failure {
                reasons.push(failure.clone());
            }
            if intent.launch_state == BoardLaunchState::Unknown
                || matches!(intent.first_prompt, FirstPromptDelivery::Unknown { .. })
            {
                reasons.push(attention(
                    BoardAttentionKind::DeliveryUnknown,
                    intent.launch_detail.clone().unwrap_or_else(|| {
                        "Delivery of the last instruction could not be confirmed; LUNA will not resend it automatically. Inspect the chat, then Retry if needed.".into()
                    }),
                    worker_task,
                ));
            }
            if let Some(barrier) = &intent.stop
                && barrier.state == BoardStopState::Unknown
            {
                reasons.push(attention(
                    BoardAttentionKind::StopUnknown,
                    barrier.detail.clone().unwrap_or_else(|| {
                        "Stop was requested but the run's end could not be verified.".into()
                    }),
                    worker_task,
                ));
            }
            match &intent.queue {
                Some(BoardAdmissionReason::SettingsUnresolved { fields }) => {
                    reasons.push(attention(
                        BoardAttentionKind::SettingsUnresolved,
                        format!("Choose {} before this ticket can start.", fields.join(", ")),
                        None,
                    ))
                }
                Some(BoardAdmissionReason::Dependency { blocked_by }) => reasons.push(attention(
                    BoardAttentionKind::DependencyBlocked,
                    format!("Waiting for {} blocking ticket(s).", blocked_by.len()),
                    None,
                )),
                Some(BoardAdmissionReason::HostUnavailable { .. }) => reasons.push(attention(
                    BoardAttentionKind::HostUnavailable,
                    "The working chat's host is unavailable.",
                    worker_task,
                )),
                _ => {}
            }
            if intent.repair_rounds >= luna_proto::BOARD_MAX_REPAIR_ROUNDS
                && latest_result.is_some_and(|(_, result)| {
                    matches!(
                        result.verdict,
                        ReviewVerdict::ChangesRequested | ReviewVerdict::Blocked
                    )
                })
            {
                reasons.push(attention(
                    BoardAttentionKind::ReviewBlocked,
                    "Blocking review findings remain after the repair limit; an explicit Retry is needed.",
                    worker_task,
                ));
            }
        }
        if let Some(run) = current_runs.iter().find(|run| {
            matches!(
                run.state,
                ReviewRunState::Failed | ReviewRunState::OutcomeUnknown
            )
        }) {
            reasons.push(attention(
                BoardAttentionKind::ReviewUnavailable,
                format!("Review {} is {}.", run.id, run.state.as_str()),
                run.reviewer_task_id.as_deref(),
            ));
        }
        if let Some(cleanup) = runs
            .iter()
            .find(|run| run.cleanup.state == ReviewCleanupState::Failed)
        {
            reasons.push(attention(
                BoardAttentionKind::CleanupFailed,
                cleanup
                    .cleanup
                    .detail
                    .clone()
                    .unwrap_or_else(|| "Reviewer cleanup failed; LUNA retries it.".into()),
                cleanup.reviewer_task_id.as_deref(),
            ));
        }
        if let Some(report) = &item.last_report
            && report.report.kind == BoardStatusReportKind::Blocked
            && report.execution_generation == generation
        {
            reasons.push(attention(
                BoardAttentionKind::AgentBlocked,
                report.report.summary.clone(),
                report.actor.task_id(),
            ));
        }
        let mut seen = HashSet::new();
        reasons.retain(|reason| {
            seen.insert((
                format!("{:?}", reason.kind),
                reason.attention_id.clone(),
                reason.task_id.clone(),
            ))
        });

        let phase = self.board_phase(
            item,
            intent,
            worker.is_some(),
            available,
            in_flight,
            &runtime,
            reviewing,
            latest_result.map(|(_, result)| result.verdict),
        );
        BoardObservation {
            work_item_id: item.id.clone(),
            phase,
            attention: reasons,
            queue_reason: intent.and_then(|intent| intent.queue.clone()),
            working_chat: None,
            observed_at_ms: intent.map_or(item.updated_at_ms, |intent| intent.updated_at_ms),
        }
    }

    #[allow(clippy::too_many_arguments)] // one projection over separately gathered evidence
    fn board_phase(
        &self,
        item: &WorkItem,
        intent: Option<&BoardExecutionIntent>,
        has_worker: bool,
        available: bool,
        in_flight: bool,
        runtime: &Option<(String, ChatRuntime)>,
        reviewing: bool,
        latest_verdict: Option<ReviewVerdict>,
    ) -> BoardPhase {
        let generation = intent.map_or(0, |intent| intent.execution_generation);
        if item.column == BoardColumn::Done {
            let completed = intent
                .and_then(|intent| intent.completed_generation)
                .unwrap_or(generation);
            let pending_cleanup = self.board_review_runs_of(&item.id).iter().any(|run| {
                run.execution_generation == completed
                    && run.reviewer_task_id.is_some()
                    && run
                        .result
                        .as_ref()
                        .is_some_and(|result| result.verdict == ReviewVerdict::Passed)
                    && matches!(
                        run.cleanup.state,
                        ReviewCleanupState::NotStarted
                            | ReviewCleanupState::Pending
                            | ReviewCleanupState::Failed
                    )
            });
            return if pending_cleanup {
                BoardPhase::CleanupPending
            } else {
                BoardPhase::Complete
            };
        }
        if has_worker && !available {
            return BoardPhase::Unknown;
        }
        let status = runtime.as_ref().and_then(|(_, rt)| rt.status);
        let Some(intent) = intent else {
            return match (has_worker, in_flight) {
                (false, _) => BoardPhase::NotStarted,
                (true, true) => BoardPhase::Working,
                (true, false) => BoardPhase::Idle,
            };
        };
        if let Some(barrier) = intent
            .stop
            .as_ref()
            .filter(|barrier| barrier.execution_generation == generation && barrier_open(barrier))
        {
            return if intent.desired == BoardColumn::Ready {
                BoardPhase::WaitingForStop
            } else {
                match barrier.state {
                    BoardStopState::Requested => BoardPhase::StopRequested,
                    BoardStopState::Stopping => BoardPhase::Stopping,
                    _ => BoardPhase::Unknown,
                }
            };
        }
        let stopped = intent.stop.as_ref().is_some_and(|barrier| {
            barrier.execution_generation == generation && barrier.state == BoardStopState::Stopped
        });
        let unknown_delivery = intent.launch_state == BoardLaunchState::Unknown
            || matches!(intent.first_prompt, FirstPromptDelivery::Unknown { .. });
        if intent.desired == BoardColumn::Backlog {
            return if in_flight {
                // The chat is running (for example the user typed in it);
                // the board never hides observed work.
                BoardPhase::Working
            } else if unknown_delivery {
                BoardPhase::Unknown
            } else if stopped {
                BoardPhase::Stopped
            } else if !has_worker
                || (generation == 0 && intent.first_prompt == FirstPromptDelivery::NotSent)
            {
                BoardPhase::NotStarted
            } else if intent.launch_state == BoardLaunchState::Failed {
                BoardPhase::Failed
            } else {
                BoardPhase::Idle
            };
        }
        // Ready.
        if let Some(reason) = &intent.queue {
            return match reason {
                BoardAdmissionReason::WaitingForStop { .. } => BoardPhase::WaitingForStop,
                BoardAdmissionReason::HostUnavailable { .. } => BoardPhase::Unknown,
                _ => BoardPhase::Queued,
            };
        }
        if unknown_delivery {
            return BoardPhase::Unknown;
        }
        if matches!(
            intent.launch_state,
            BoardLaunchState::Pending | BoardLaunchState::Unsent
        ) {
            return BoardPhase::Starting;
        }
        if in_flight {
            let repairing =
                intent.repair_rounds > 0 && latest_verdict == Some(ReviewVerdict::ChangesRequested);
            return if repairing {
                BoardPhase::Fixing
            } else {
                BoardPhase::Working
            };
        }
        if reviewing {
            return BoardPhase::Reviewing;
        }
        if intent.launch_state == BoardLaunchState::Failed
            || (intent.launch_failure.is_some() && !has_worker)
            || status == Some(SessionStatus::Errored)
        {
            return BoardPhase::Failed;
        }
        if intent.served_intent_revision < intent.intent_revision {
            return if has_worker || intent.pending_start.is_some() {
                BoardPhase::Queued
            } else {
                BoardPhase::NotStarted
            };
        }
        if item.last_report.as_ref().is_some_and(|report| {
            report.report.kind == BoardStatusReportKind::ReadyForReview
                && report.execution_generation == generation
        }) {
            return BoardPhase::Checking;
        }
        if !has_worker {
            return if intent.launch_failure.is_some() {
                BoardPhase::Failed
            } else {
                BoardPhase::NotStarted
            };
        }
        if stopped {
            return BoardPhase::Stopped;
        }
        BoardPhase::Idle
    }
}
