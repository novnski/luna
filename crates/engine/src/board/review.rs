//! Review slice (A5, PB-1R + PB-3 evidence): workflow-owned reviewer side
//! chats (under a main ticket; a sibling under the same main for a side
//! ticket), the engine-assembled review packet with a fixed diff basis,
//! structured results, the two-round repair cap, staleness, the Done gate,
//! exact reviewer archive+close cleanup with readback, inclusion in shared
//! Stop, and outcome evidence capture (diff/PR/checks/artifacts).
//!
//! Contract: docs/design/project-board/pb0-contract.md §5.5 and §7.5.
//! Reviewers are ordinary chats with a recorded role; never a hidden job,
//! never nested, never a main chat. Cleanup authority comes from the
//! recorded run, not from the descendant-scoped `archiveThreads` guard.
//!
//! Durability: every step is a CAS-guarded `ReviewRun` transition committed
//! before its effect, and every effect uses a deterministic identity derived
//! from the run (`{run}:chat`, `{run}:packet`, `{run}:repair`,
//! `{run}:cleanup…`). The maintenance sweeper resumes any step after a
//! restart by those identities, so a reviewer is created once, a packet and
//! a repair request are delivered once, and cleanup retries the same
//! identity. A late result (Backlog, another generation, a stale basis) is
//! kept as history and never applied.
//!
//! Done: evidence is persisted first, then the lifecycle slice's
//! `board_mark_done_if_current` performs the engine-owned transition (a
//! standalone commit with the same guards when that slice is absent), then
//! the pass is recorded and exact reviewer cleanup becomes due.
//!
//! Locking: the async entry points and the sweeper take
//! `board_hub().gate(ticket)` and may then take `lifecycle:{ticket}` (Done).
//! Do not call them while holding either gate. `board_review_owned_work`,
//! `board_review_observation` and `board_capture_outcomes` take neither.
// Hooks for other slices (observation merge, project review profile setter).
#![allow(dead_code)]
use super::*;
use luna_proto::{
    BOARD_MAX_REPAIR_ROUNDS, BOARD_MAX_REVIEW_PASSES, BoardAttentionKind, BoardAttentionReason,
    BoardColumn, BoardCompletionMode, BoardEvidenceRef, BoardExecutionIntent,
    BoardNavigationTarget, BoardOperation, BoardOutcomes, BoardOutcomesRequest, BoardPhase,
    BoardReviewCancelRequest, BoardReviewStartRequest, BoardReviewStatus, BoardReviewStatusRequest,
    BoardReviewSubmitRequest, BoardStatusReportKind, OutcomeKind, OutcomeReference,
    ReviewCheckReport, ReviewCleanup, ReviewCleanupReceipt, ReviewCleanupState, ReviewFinding,
    ReviewResult, ReviewRun, ReviewRunOutcome, ReviewRunReceipt, ReviewRunState, ReviewVerdict,
    TaskLinkRole, WorkItem, WorkItemEventKind, WorkItemTaskLink,
};
use luna_sync::{
    BoardCommit, BoardEventQuery, BoardItemWrite, BoardListQuery, BoardRankPlacement, BoardRowWrite,
};

mod basis;
mod cleanup;
mod delivery;
mod evidence;
mod profile;

#[allow(unused_imports)] // for the lifecycle slice's settings preview
pub(crate) use profile::ResolvedReviewer;

/// Caller component for review automation (`engine:review` request scope).
const COMPONENT: &str = "review";
/// Actor id stamped on every task command the review slice issues.
const ACTOR_ID: &str = "luna-engine:board-review";
const RUNS_READ: usize = 32;
const FINDINGS_MAX: usize = 50;
const FINDING_TEXT_MAX: usize = 4_000;
const SUMMARY_MAX: usize = 2_000;
const CHECKS_MAX: usize = 16;
const EVIDENCE_MAX: usize = 16;
/// Consecutive stale passes (the result kept changing during review) before
/// the ticket needs attention instead of another automatic re-review.
const STALE_REVIEWS_MAX: usize = 2;
/// Automatic relaunches of a reviewer turn that ended without a structured
/// result (crash, restart, a reviewer that ignored the contract).
const RELAUNCHES_MAX: u8 = 1;
const CLEANUP_ATTEMPTS_MAX: u32 = 10;
/// An unattended reviewer turn running longer than this is stopped and the
/// pass is unresolved.
const REVIEW_TIMEOUT_MS: i64 = 45 * 60 * 1000;
const SWEEP_INTERVAL: Duration = Duration::from_millis(400);
const FULL_SCAN_INTERVAL: Duration = Duration::from_secs(60);
/// Commit retries after a concurrent writer moved the ticket/intent/run.
const MUTATE_ATTEMPTS: usize = 4;

/// Whether a chat runs review-constrained (read-only sandbox on every turn):
/// it carries an active Reviewer link to a ticket. Installed into the
/// sessions engine at assembly; the authority is the recorded review
/// ownership, never a client-supplied setting.
pub(crate) fn review_constrained(store: &DocsStore, chat_id: &str) -> bool {
    store
        .board_task_links_for_task(chat_id, true)
        .is_ok_and(|links| links.iter().any(|link| link.role == TaskLinkRole::Reviewer))
}

fn storage(error: impl std::fmt::Display) -> BoardError {
    BoardError::Storage {
        message: error.to_string(),
    }
}

fn engine_caller() -> BoardCaller {
    BoardCaller::Engine {
        component: COMPONENT,
    }
}

fn engine_actor() -> BoardActor {
    BoardActor::Engine {
        component: COMPONENT.into(),
    }
}

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(12)]
}

/// Bounded single text field; empty after trimming is `None`.
fn bounded_text(field: &str, value: Option<&str>, max_chars: usize) -> BoardResult<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > max_chars {
        return Err(BoardError::invalid(
            field,
            format!("use at most {max_chars} characters"),
        ));
    }
    Ok(Some(value.to_owned()))
}

/// Stable review-run identity from the caller-normalized request id (never
/// a chat, ticket or provider id).
fn review_run_id(normalized_request_id: &str) -> BoardResult<String> {
    Ok(format!(
        "rr-{}",
        &board_digest(&("board-review-run", normalized_request_id))?[..32]
    ))
}

/// Why a result for `generation` can no longer change this ticket, if it
/// cannot. Ready intent and the exact admitted generation are required.
fn not_current(
    item: &WorkItem,
    intent: Option<&BoardExecutionIntent>,
    generation: u64,
) -> Option<String> {
    let Some(intent) = intent else {
        return Some("The ticket has no execution intent.".into());
    };
    if item.column != BoardColumn::Ready || intent.desired != BoardColumn::Ready {
        return Some(format!(
            "The ticket is held in {}; a review result cannot complete it.",
            if item.column == BoardColumn::Ready {
                intent.desired.label()
            } else {
                item.column.label()
            }
        ));
    }
    if intent.execution_generation != generation {
        return Some(format!(
            "Execution generation {} replaced generation {generation}.",
            intent.execution_generation
        ));
    }
    if intent.stop.as_ref().is_some_and(|stop| {
        stop.execution_generation == generation
            && stop.state != luna_proto::BoardStopState::Rejected
    }) {
        return Some("Stop was requested for this execution.".into());
    }
    None
}

fn blocking_count(findings: &[ReviewFinding]) -> u32 {
    findings.iter().filter(|finding| finding.blocking).count() as u32
}

/// Same finding across rounds: file plus normalized explanation.
fn finding_fingerprint(finding: &ReviewFinding) -> String {
    let explanation = finding
        .explanation
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{}\u{1f}{}",
        finding.file.as_deref().unwrap_or_default(),
        explanation
    )
}

fn run_row(run: &ReviewRun, expected_version: Option<u64>) -> BoardRowWrite {
    BoardRowWrite::ReviewRun {
        expected_version,
        run: Box::new(run.clone()),
    }
}

fn intent_row(intent: &BoardExecutionIntent) -> BoardRowWrite {
    BoardRowWrite::Intent {
        expected_version: Some(intent.version),
        intent: Box::new(intent.clone()),
    }
}

/// One review commit: optional ticket transition (revision bump with one
/// journal event), auxiliary rows (CAS on the run/intent versions).
pub(super) struct ReviewWrite {
    /// Idempotency key in the `engine:review` scope (unique per transition).
    pub key: String,
    pub operation: BoardOperation,
    pub actor: BoardActor,
    pub rows: Vec<BoardRowWrite>,
    pub event: Option<WorkItemEventKind>,
    /// Engine-owned Done transition.
    pub column: Option<BoardColumn>,
}

impl ReviewWrite {
    fn new(key: impl Into<String>, operation: BoardOperation) -> Self {
        Self {
            key: key.into(),
            operation,
            actor: engine_actor(),
            rows: Vec::new(),
            event: None,
            column: None,
        }
    }

    fn row(mut self, row: BoardRowWrite) -> Self {
        self.rows.push(row);
        self
    }

    fn event(mut self, event: WorkItemEventKind) -> Self {
        self.event = Some(event);
        self
    }

    fn actor(mut self, actor: BoardActor) -> Self {
        self.actor = actor;
        self
    }
}

/// Fresh state a mutation plans against (re-read on every attempt).
pub(super) struct ReviewCtx {
    pub item: WorkItem,
    pub intent: Option<BoardExecutionIntent>,
    pub run: Option<ReviewRun>,
}

impl EngineRpc {
    // ---- shared helpers ----

    fn review_commit(
        &self,
        item: &WorkItem,
        write: ReviewWrite,
    ) -> BoardResult<Option<BoardMutationReceipt>> {
        let request_id = engine_caller().normalized_request_id(&write.key)?;
        let payload_hash = board_digest(&(write.operation.as_str(), &write.key))?;
        let now = crate::now_ms();
        let (item_write, receipt) = if write.event.is_some() || write.column.is_some() {
            let mut next = item.clone();
            next.revision += 1;
            next.updated_at_ms = now;
            next.updated_by = write.actor.clone();
            let placement = write
                .column
                .filter(|column| *column != item.column)
                .map(|column| {
                    next.column = column;
                    BoardRankPlacement::Top
                });
            let receipt = BoardMutationReceipt {
                request_id: String::new(),
                payload_hash: String::new(),
                operation: write.operation,
                work_item_id: next.id.clone(),
                key: next.key.clone(),
                revision: next.revision,
                column: next.column,
                archived: next.archived,
                item: next.clone(),
                event_seq: None,
                actor: write.actor.clone(),
                committed_at_ms: 0,
                task_id: None,
                navigation: self.board_default_navigation(&next),
            };
            (
                BoardItemWrite::Replace {
                    expected_revision: item.revision,
                    item: Box::new(next),
                    placement,
                },
                Some(receipt),
            )
        } else {
            (BoardItemWrite::None, None)
        };
        let event = write.event.map(|kind| {
            let activity = kind.is_activity();
            (kind, activity)
        });
        self.board_commit(BoardCommit {
            request_id,
            payload_hash,
            committed_at_ms: now,
            item: item_write,
            rows: write.rows,
            event,
            receipt,
        })
    }

    /// Plan against fresh state and commit; a concurrent writer (CAS) gets a
    /// re-read and a new plan. `plan` returns `None` for "nothing to do".
    fn review_mutate<R>(
        &self,
        work_item_id: &str,
        run_id: Option<&str>,
        mut plan: impl FnMut(&ReviewCtx) -> BoardResult<(Option<ReviewWrite>, R)>,
    ) -> BoardResult<(R, Option<BoardMutationReceipt>)> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let ctx = ReviewCtx {
                item: self.board_load(work_item_id)?,
                intent: self.store.board_intent(work_item_id).map_err(storage)?,
                run: match run_id {
                    Some(id) => self.store.board_review_run(id).map_err(storage)?,
                    None => None,
                },
            };
            let (write, result) = plan(&ctx)?;
            let Some(write) = write else {
                return Ok((result, None));
            };
            match self.review_commit(&ctx.item, write) {
                Ok(receipt) => {
                    self.board_hub()
                        .notify_observed(vec![work_item_id.to_owned()]);
                    return Ok((result, receipt));
                }
                Err(BoardError::Busy { .. }) if attempt < MUTATE_ATTEMPTS => continue,
                Err(error) => return Err(error),
            }
        }
    }

    /// Update one run (CAS) with an optional event; no-op when `change`
    /// returns false (the run moved on).
    fn review_update_run(
        &self,
        run: &ReviewRun,
        step: &str,
        event: impl Fn(&ReviewRun) -> Option<WorkItemEventKind>,
        mut change: impl FnMut(&mut ReviewRun, Option<&mut BoardExecutionIntent>) -> bool,
    ) -> BoardResult<Option<ReviewRun>> {
        let (updated, _) = self.review_mutate(&run.work_item_id, Some(&run.id), |ctx| {
            let Some(current) = ctx.run.clone() else {
                return Ok((None, None));
            };
            let mut next = current.clone();
            let mut intent = ctx.intent.clone();
            if !change(&mut next, intent.as_mut()) {
                return Ok((None, None));
            }
            next.updated_at_ms = crate::now_ms();
            let mut write = ReviewWrite::new(
                format!("{}:v{}:{step}", current.id, current.version),
                BoardOperation::ReviewSubmit,
            )
            .row(run_row(&next, Some(current.version)));
            if let (Some(before), Some(after)) = (ctx.intent.as_ref(), intent.as_ref())
                && before != after
            {
                write = write.row(intent_row(after));
            }
            if let Some(kind) = event(&next) {
                write = write.event(kind);
            }
            next.version = current.version + 1;
            Ok((Some(write), Some(next)))
        })?;
        Ok(updated)
    }

    fn review_receipt(
        &self,
        request_id: &str,
        run: ReviewRun,
        event_seq: Option<u64>,
    ) -> ReviewRunReceipt {
        let navigation =
            run.reviewer_task_id
                .clone()
                .map(|reviewer| BoardNavigationTarget::SideChat {
                    main_task_id: run.display_main_parent_id.clone(),
                    side_task_id: reviewer,
                });
        ReviewRunReceipt {
            request_id: request_id.into(),
            run,
            event_seq,
            navigation,
        }
    }

    fn review_working_link(&self, work_item_id: &str) -> BoardResult<Option<WorkItemTaskLink>> {
        Ok(self
            .store
            .board_task_links_for_item(work_item_id, true)
            .map_err(storage)?
            .into_iter()
            .find(|link| link.role == TaskLinkRole::Working))
    }

    /// Reviewer chats recorded by this ticket's runs that still carry an
    /// active Reviewer link to it. Never the primary chat or its parent.
    fn review_owned_reviewers(
        &self,
        item: &WorkItem,
        runs: &[ReviewRun],
    ) -> BoardResult<Vec<String>> {
        let links = self
            .store
            .board_task_links_for_item(&item.id, true)
            .map_err(storage)?;
        let mut ids = Vec::new();
        for run in runs {
            let Some(reviewer) = run.reviewer_task_id.as_deref() else {
                continue;
            };
            if reviewer == run.primary_task_id
                || reviewer == run.display_main_parent_id
                || ids.iter().any(|id: &String| id == reviewer)
                || !links
                    .iter()
                    .any(|link| link.task_id == reviewer && link.role == TaskLinkRole::Reviewer)
            {
                continue;
            }
            ids.push(reviewer.to_owned());
        }
        Ok(ids)
    }

    /// Whether a chat and its owned blocking work are idle. `exclude` names
    /// reviewer chats that may appear as the chat's owned work.
    async fn review_quiescent(&self, task_id: &str, exclude: &[String]) -> Result<(), String> {
        if self.sessions.active_turn_id(task_id).is_some() {
            return Err("its turn is still running".into());
        }
        if self.sessions.has_live_background_work(task_id) {
            return Err("its background work is still running".into());
        }
        if let Some(session) = self.sessions.session_status(task_id)
            && matches!(
                session.status,
                luna_proto::SessionStatus::Working
                    | luna_proto::SessionStatus::AwaitingApproval
                    | luna_proto::SessionStatus::AwaitingInput
            )
        {
            return Err("it is working or waiting for input".into());
        }
        match self
            .handle_task_work(
                methods::GET_TASK_WORK,
                serde_json::json!({"taskId": task_id, "limit": 100}),
            )
            .await
        {
            Ok(RpcReply::Value(value)) => {
                let snapshot: luna_proto::TaskWorkSnapshot =
                    serde_json::from_value(value).map_err(|error| error.to_string())?;
                let blocking = snapshot
                    .items
                    .iter()
                    .filter(|work| {
                        work.blocks_completion
                            && !work.state.terminal()
                            && !exclude.contains(&work.task_id)
                    })
                    .count();
                if blocking > 0 {
                    return Err(format!("{blocking} owned blocking work item(s) are active"));
                }
                Ok(())
            }
            Ok(_) => Err("owned work could not be read".into()),
            Err(error) => Err(format!("owned work could not be read: {error}")),
        }
    }

    // ---- contract entry points ----

    /// Create (or reuse across repair rounds) the reviewer side chat for the
    /// current generation, freeze the review basis, resolve the reviewer
    /// profile (implementation-model fallback when none is configured) and
    /// send the bounded review packet. Idempotent by request id.
    ///
    /// The call records the pass (`Queued`) and the checks the implementer
    /// reported; the launch waits until the implementation turn and its
    /// owned blocking work are quiet, so the implementer can call it from
    /// its own turn.
    pub(crate) async fn board_review_start(
        &self,
        caller: &BoardCaller,
        request: BoardReviewStartRequest,
    ) -> BoardResult<ReviewRunReceipt> {
        let item = self.board_item_for(caller, &request.work_item_id, BoardAccess::Write)?;
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let run_id = review_run_id(&request_id)?;
        let replay = |run: ReviewRun| -> BoardResult<ReviewRunReceipt> {
            if run.work_item_id != item.id {
                return Err(BoardError::IdempotencyConflict {
                    request_id: request.request_id.clone(),
                });
            }
            Ok(self.review_receipt(&request_id, run, None))
        };
        if let Some(run) = self.store.board_review_run(&run_id).map_err(storage)? {
            return replay(run);
        }
        let working = self.review_working_link(&item.id)?;
        if let BoardCaller::Agent {
            task_id,
            explicit_user_request,
            ..
        } = caller
        {
            // The working chat submits its own result; another chat only on
            // the user's explicit request; a reviewer never starts reviews.
            let is_runs_reviewer = !self
                .store
                .board_review_runs_for_reviewer(task_id)
                .map_err(storage)?
                .is_empty();
            let is_worker = working
                .as_ref()
                .is_some_and(|link| &link.task_id == task_id);
            if is_runs_reviewer || !(is_worker || *explicit_user_request) {
                return Err(BoardError::NotAuthorized {
                    reason: "Only the ticket's working chat (or the user) can submit its result for review.".into(),
                });
            }
        }
        if item.review.disabled && item.completion.mode == BoardCompletionMode::ManualAcceptance {
            return Err(BoardError::invalid(
                "review",
                "automatic review is disabled for this ticket's manual acceptance policy",
            ));
        }
        let mut checks = Vec::new();
        if request.checks.len() > CHECKS_MAX {
            return Err(BoardError::invalid(
                "checks",
                format!("report at most {CHECKS_MAX} checks"),
            ));
        }
        for check in &request.checks {
            let name = bounded_text("checks.name", Some(&check.name), 120)?
                .ok_or_else(|| BoardError::invalid("checks.name", "name every check"))?;
            checks.push(ReviewCheckReport {
                name,
                command: bounded_text("checks.command", check.command.as_deref(), 500)?,
                passed: check.passed,
                exit_code: check.exit_code,
                detail: bounded_text("checks.detail", check.detail.as_deref(), SUMMARY_MAX)?,
            });
        }
        let summary = bounded_text("summary", request.summary.as_deref(), SUMMARY_MAX)?;
        if request.evidence.len() > EVIDENCE_MAX {
            return Err(BoardError::invalid(
                "evidence",
                format!("reference at most {EVIDENCE_MAX} items"),
            ));
        }
        let _gate = self.board_hub().gate(&item.id).await;
        if let Some(run) = self.store.board_review_run(&run_id).map_err(storage)? {
            return replay(run);
        }
        let working = working.ok_or_else(|| BoardError::InvalidTransition {
            from: item.column,
            to: BoardColumn::Ready,
            reason: "Start the ticket before reviewing it; it has no working chat.".into(),
        })?;
        let primary = self
            .workspace
            .chat(&working.task_id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::Unavailable {
                reason: "The ticket's working chat is unavailable on this host.".into(),
            })?;
        let intent = self.store.board_intent(&item.id).map_err(storage)?;
        let generation = intent
            .as_ref()
            .map(|intent| intent.execution_generation)
            .unwrap_or_default();
        if generation == 0 {
            return Err(BoardError::InvalidTransition {
                from: item.column,
                to: BoardColumn::Ready,
                reason: "Start the ticket before reviewing it.".into(),
            });
        }
        if let Some(expected) = request.execution_generation
            && expected != generation
        {
            return Err(BoardError::Busy {
                reason: format!(
                    "Execution generation {expected} is no longer current (now {generation})."
                ),
            });
        }
        if let Some(reason) = not_current(&item, intent.as_ref(), generation) {
            return Err(BoardError::InvalidTransition {
                from: item.column,
                to: BoardColumn::Ready,
                reason,
            });
        }
        // Checks are tied to the snapshot that exists when they are reported.
        let check_basis = if checks.is_empty() {
            None
        } else {
            Some(
                self.review_capture(
                    primary.cwd.as_deref().unwrap_or("."),
                    None,
                    &request.evidence,
                )
                .await?
                .diff_hash,
            )
        };
        let resolved = self
            .board_resolve_review_profile(&item, &primary, request.profile.as_ref())
            .await;
        let placement = self.review_placement(&working, &primary);
        let now = crate::now_ms();
        let actor = caller.actor();
        let (receipt, mutation) = self.review_mutate(&item.id, Some(&run_id), |ctx| {
            if let Some(run) = &ctx.run {
                return Ok((None, run.clone()));
            }
            let intent = ctx.intent.as_ref().ok_or_else(|| BoardError::InvalidTransition {
                from: ctx.item.column,
                to: BoardColumn::Ready,
                reason: "Start the ticket before reviewing it.".into(),
            })?;
            if let Some(reason) = not_current(&ctx.item, Some(intent), generation) {
                return Err(BoardError::InvalidTransition {
                    from: ctx.item.column,
                    to: BoardColumn::Ready,
                    reason,
                });
            }
            let runs = self
                .store
                .board_review_runs(&ctx.item.id, RUNS_READ)
                .map_err(storage)?;
            if let Some(active) = runs.iter().find(|run| {
                run.execution_generation == generation
                    && (!run.state.terminal()
                        || (run.state == ReviewRunState::Completed && run.outcome.is_none()))
            }) {
                return Err(BoardError::Busy {
                    reason: format!(
                        "Review {} is already in progress for this result; read review_status.",
                        active.id
                    ),
                });
            }
            if intent.review_passes >= BOARD_MAX_REVIEW_PASSES {
                return Err(BoardError::NotAuthorized {
                    reason: format!(
                        "{} reached its repair cap ({BOARD_MAX_REVIEW_PASSES} review passes). Only an explicit user Retry grants another cycle.",
                        ctx.item.key
                    ),
                });
            }
            let mut check_rows = Vec::new();
            let mut check_ids = Vec::new();
            for check in &checks {
                let id = format!(
                    "oc-{}",
                    &board_digest(&("review-check", &run_id, &check.name))?[..32]
                );
                check_ids.push(id.clone());
                check_rows.push(BoardRowWrite::Outcome(Box::new(OutcomeReference {
                    id,
                    work_item_id: ctx.item.id.clone(),
                    kind: OutcomeKind::Check,
                    execution_generation: generation,
                    task_id: Some(working.task_id.clone()),
                    label: check.name.clone(),
                    basis: check_basis.clone(),
                    url: None,
                    path: None,
                    state: Some(if check.passed { "passed" } else { "failed" }.into()),
                    detail: serde_json::json!({
                        "command": check.command,
                        "exitCode": check.exit_code,
                        "detail": check.detail,
                        "reviewRunId": run_id,
                        "reportedBy": "implementer",
                    }),
                    historical: false,
                    observed_by: actor.clone(),
                    observed_at_ms: now,
                })));
            }
            let (state, detail, attention) = match &resolved.failure {
                Some((error, kind)) => (ReviewRunState::Failed, Some(error.message()), Some(*kind)),
                None => (ReviewRunState::Queued, None, None),
            };
            let run = ReviewRun {
                id: run_id.clone(),
                work_item_id: ctx.item.id.clone(),
                version: 1,
                execution_generation: generation,
                round: intent.repair_rounds,
                primary_task_id: working.task_id.clone(),
                reviewer_task_id: None,
                display_main_parent_id: placement.1.clone(),
                placement: placement.0,
                launch_request_id: format!("{run_id}:launch"),
                profile: resolved.resolution.clone(),
                basis: None,
                state,
                result: None,
                delivered_id: None,
                stop_request_id: None,
                cleanup: ReviewCleanup {
                    state: ReviewCleanupState::NotStarted,
                    cleanup_id: format!("{run_id}:cleanup"),
                    attempts: 0,
                    archived_at_ms: None,
                    detail: None,
                },
                created_at_ms: now,
                updated_at_ms: now,
                detail,
                attention,
                requested_by: Some(actor.clone()),
                result_request_id: None,
                late_result: false,
                outcome: attention.map(|kind| ReviewRunOutcome::NeedsAttention { reason: kind }),
                implementer_summary: summary.clone(),
                evidence: request.evidence.clone(),
                reported_check_ids: check_ids,
                supersedes: None,
                relaunches: 0,
            };
            let event = match attention {
                Some(kind) => WorkItemEventKind::AttentionRaised { reason: kind },
                None => WorkItemEventKind::ReviewStarted {
                    review_run_id: run_id.clone(),
                    round: run.round,
                },
            };
            let mut write = ReviewWrite::new(format!("{run_id}:create"), BoardOperation::ReviewStart)
                .actor(actor.clone())
                .row(run_row(&run, None))
                .event(event);
            write.rows.extend(check_rows);
            Ok((Some(write), run))
        })?;
        if let Some((error, _)) = resolved.failure
            && mutation.is_some()
        {
            // The unresolved run is recorded (Needs attention); the caller
            // gets the structured reason. A retry with this key replays it.
            return Err(error);
        }
        Ok(self.review_receipt(
            &request_id,
            receipt,
            mutation.and_then(|receipt| receipt.event_seq),
        ))
    }

    pub(crate) fn board_review_status(
        &self,
        caller: &BoardCaller,
        request: BoardReviewStatusRequest,
    ) -> BoardResult<BoardReviewStatus> {
        let work_item_id = match (&request.work_item_id, &request.review_run_id) {
            (_, Some(run_id)) => {
                self.store
                    .board_review_run(run_id)
                    .map_err(storage)?
                    .ok_or_else(|| BoardError::invalid("reviewRunId", "no such review run"))?
                    .work_item_id
            }
            (Some(id), None) => id.clone(),
            (None, None) => {
                return Err(BoardError::invalid(
                    "workItemId",
                    "name a ticket or a review run",
                ));
            }
        };
        let item = self.board_item_for(caller, &work_item_id, BoardAccess::Read)?;
        let runs = match &request.review_run_id {
            Some(run_id) => self
                .store
                .board_review_run(run_id)
                .map_err(storage)?
                .into_iter()
                .collect(),
            None => self
                .store
                .board_review_runs(&item.id, 20)
                .map_err(storage)?,
        };
        let intent = self.store.board_intent(&item.id).map_err(storage)?;
        let (passes, rounds) = intent.as_ref().map_or((0, 0), |intent| {
            (intent.review_passes, intent.repair_rounds)
        });
        Ok(BoardReviewStatus {
            runs,
            review_passes: passes,
            repair_rounds: rounds,
            repairs_remaining: BOARD_MAX_REPAIR_ROUNDS.saturating_sub(rounds),
        })
    }

    /// Cancel one run through the shared Stop of its exact reviewer chat.
    pub(crate) async fn board_review_cancel(
        &self,
        caller: &BoardCaller,
        request: BoardReviewCancelRequest,
    ) -> BoardResult<ReviewRunReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let run = self
            .store
            .board_review_run(&request.review_run_id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::invalid("reviewRunId", "no such review run"))?;
        let item = self.board_item_for(caller, &run.work_item_id, BoardAccess::Write)?;
        if let BoardCaller::Agent {
            task_id,
            explicit_user_request,
            ..
        } = caller
            && task_id != &run.primary_task_id
            && !*explicit_user_request
        {
            return Err(BoardError::NotAuthorized {
                reason: "Only the ticket's working chat (or the user) can cancel its review."
                    .into(),
            });
        }
        let _gate = self.board_hub().gate(&item.id).await;
        let run = self
            .store
            .board_review_run(&run.id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::invalid("reviewRunId", "no such review run"))?;
        if run.state.terminal() {
            return Ok(self.review_receipt(&request_id, run, None));
        }
        let run = self
            .review_cancel_run(&run, "Review cancelled.", Some(request_id.clone()))
            .await?;
        Ok(self.review_receipt(&request_id, run, None))
    }

    /// Stop the exact reviewer (shared Stop path) and record the run as
    /// cancelled. Never touches the primary chat or unrelated siblings.
    async fn review_cancel_run(
        &self,
        run: &ReviewRun,
        detail: &str,
        stop_request_id: Option<String>,
    ) -> BoardResult<ReviewRun> {
        if let Some(reviewer) = run.reviewer_task_id.as_deref() {
            self.review_stop_reviewer(run, reviewer).await;
        }
        let updated = self.review_update_run(
            run,
            "cancel",
            |next| {
                Some(WorkItemEventKind::ReviewStateChanged {
                    review_run_id: next.id.clone(),
                    state: next.state,
                })
            },
            |next, _| {
                if next.state.terminal() {
                    return false;
                }
                next.state = ReviewRunState::Cancelled;
                next.detail = Some(detail.into());
                next.stop_request_id = stop_request_id.clone().or(next.stop_request_id.take());
                true
            },
        )?;
        Ok(match updated {
            Some(run) => run,
            None => self
                .store
                .board_review_run(&run.id)
                .map_err(storage)?
                .unwrap_or_else(|| run.clone()),
        })
    }

    /// Structured reviewer result. Only the run's recorded reviewer chat may
    /// submit; a basis/packet mismatch or an older generation makes it stale.
    /// The engine then schedules a bounded repair, Needs attention, or Done.
    pub(crate) async fn board_review_submit(
        &self,
        caller: &BoardCaller,
        request: BoardReviewSubmitRequest,
    ) -> BoardResult<ReviewRunReceipt> {
        let BoardCaller::Agent {
            task_id,
            project_ids,
            ..
        } = caller
        else {
            return Err(BoardError::NotAuthorized {
                reason: "Only the run's reviewer chat can submit a review result.".into(),
            });
        };
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let run = self
            .store
            .board_review_run(&request.review_run_id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::invalid("reviewRunId", "no such review run"))?;
        // Authority is the recorded run, not a board scope: only its exact
        // reviewer chat, in the ticket's own project, on the owning host.
        if run.reviewer_task_id.as_deref() != Some(task_id.as_str()) {
            return Err(BoardError::NotAuthorized {
                reason: "Only the run's recorded reviewer chat can submit its result.".into(),
            });
        }
        let item = self.board_load(&run.work_item_id)?;
        if item.owner_host_id != self.engine_info.device_id
            || !project_ids.contains(&item.project_id)
        {
            return Err(BoardError::NotAuthorized {
                reason: "This review run belongs to another project or host.".into(),
            });
        }
        let summary =
            bounded_text("summary", Some(&request.summary), SUMMARY_MAX)?.ok_or_else(|| {
                BoardError::invalid(
                    "summary",
                    "summarize the review; an empty result is insufficient",
                )
            })?;
        if request.findings.len() > FINDINGS_MAX {
            return Err(BoardError::invalid(
                "findings",
                format!("report at most {FINDINGS_MAX} findings"),
            ));
        }
        let mut findings = Vec::new();
        for (index, finding) in request.findings.iter().enumerate() {
            let explanation = bounded_text(
                "findings.explanation",
                Some(&finding.explanation),
                FINDING_TEXT_MAX,
            )?
            .ok_or_else(|| BoardError::invalid("findings.explanation", "explain every finding"))?;
            let required_action = bounded_text(
                "findings.requiredAction",
                Some(&finding.required_action),
                FINDING_TEXT_MAX,
            )?
            .ok_or_else(|| {
                BoardError::invalid(
                    "findings.requiredAction",
                    "state the required correction or validation",
                )
            })?;
            let id = bounded_text("findings.id", Some(&finding.id), 64)?
                .unwrap_or_else(|| format!("f{}", index + 1));
            findings.push(ReviewFinding {
                id,
                severity: finding.severity,
                blocking: finding.blocking,
                file: bounded_text("findings.file", finding.file.as_deref(), 500)?,
                line: finding.line,
                explanation,
                required_action,
                evidence: finding
                    .evidence
                    .iter()
                    .take(8)
                    .map(|evidence| evidence.chars().take(500).collect())
                    .collect(),
                repeats_finding_id: None,
            });
        }
        let blocking = blocking_count(&findings);
        match request.verdict {
            ReviewVerdict::Passed if blocking > 0 => {
                return Err(BoardError::invalid(
                    "verdict",
                    "a passed review cannot contain blocking findings",
                ));
            }
            ReviewVerdict::ChangesRequested if blocking == 0 => {
                return Err(BoardError::invalid(
                    "findings",
                    "changesRequested needs at least one blocking finding with file/location and the required correction",
                ));
            }
            _ => {}
        }
        let _gate = self.board_hub().gate(&item.id).await;
        let run = self
            .store
            .board_review_run(&run.id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::invalid("reviewRunId", "no such review run"))?;
        if let Some(previous) = &run.result_request_id {
            if previous == &request_id {
                return Ok(self.review_receipt(&request_id, run, None));
            }
            return Err(BoardError::IdempotencyConflict {
                request_id: request.request_id,
            });
        }
        // Engine-observed execution profile while the reviewer runs.
        let running = self
            .sessions
            .execution_summary(task_id)
            .ok()
            .flatten()
            .and_then(|summary| summary.running);
        let observed_model = running.as_ref().and_then(|settings| settings.model.clone());
        let observed_harness = running.as_ref().map(|settings| settings.provider);
        let result = ReviewResult {
            verdict: request.verdict,
            basis_packet_hash: request.basis_packet_hash.clone(),
            findings,
            summary,
            observed_model: observed_model.clone(),
            submitted_at_ms: crate::now_ms(),
        };
        // Decide what the result can mean before writing it.
        let intent = self.store.board_intent(&item.id).map_err(storage)?;
        let fresh = self.board_load(&item.id)?;
        let mut state = ReviewRunState::Completed;
        let mut late = false;
        let mut detail = None;
        let mut attention = None;
        if run.state.terminal() {
            late = true;
            state = run.state;
            detail = Some(format!(
                "Result arrived after the run was {}; kept as history.",
                run.state.as_str()
            ));
        } else if let Some(reason) = not_current(&fresh, intent.as_ref(), run.execution_generation)
        {
            late = true;
            state = ReviewRunState::Cancelled;
            detail = Some(format!("{reason} The result is kept as history."));
        } else if !matches!(
            run.state,
            ReviewRunState::Running
                | ReviewRunState::Waiting
                | ReviewRunState::Starting
                | ReviewRunState::OutcomeUnknown
        ) || run.basis.is_none()
        {
            return Err(BoardError::Busy {
                reason: "This review has not been delivered to the reviewer yet.".into(),
            });
        } else if run.basis.as_ref().map(|basis| basis.packet_hash.as_str())
            != Some(request.basis_packet_hash.as_str())
        {
            state = ReviewRunState::Stale;
            detail = Some(
                "The result names a different review packet; the current basis is reviewed again."
                    .into(),
            );
        } else if observed_harness.is_some_and(|harness| harness != run.profile.resolved.harness) {
            state = ReviewRunState::Failed;
            attention = Some(BoardAttentionKind::ReviewUnavailable);
            detail = Some(format!(
                "The reviewer ran on {:?}, not the selected {:?}; the result cannot complete the ticket.",
                observed_harness.unwrap_or(run.profile.resolved.harness),
                run.profile.resolved.harness
            ));
        } else if let Some(requested) = run.profile.resolved.model.as_deref()
            && !run.profile.implementation_fallback
            && !observed_model
                .as_deref()
                .is_some_and(|observed| profile::model_matches(requested, observed))
        {
            state = ReviewRunState::Failed;
            attention = Some(BoardAttentionKind::ModelUnavailable);
            detail = Some(match observed_model.as_deref() {
                Some(observed) => format!(
                    "The reviewer ran on {observed}, not the selected {requested}; an unobserved substitution cannot complete the ticket."
                ),
                None => format!(
                    "The reviewer's model could not be observed; the selected {requested} is unverified, so the result cannot complete the ticket."
                ),
            });
        } else if let Some(basis) = run.basis.as_ref() {
            let cwd = self
                .workspace
                .chat(&run.primary_task_id)
                .map_err(storage)?
                .and_then(|chat| chat.cwd)
                .unwrap_or_else(|| basis.cwd.clone());
            if let Some(reason) = self.review_basis_changed(&run, &cwd).await? {
                state = ReviewRunState::Stale;
                detail = Some(format!("{reason} The new basis is reviewed again."));
            }
        }
        let actor = BoardActor::Reviewer {
            task_id: task_id.clone(),
            review_run_id: run.id.clone(),
        };
        let accepted = state == ReviewRunState::Completed;
        let (updated, mutation) = self.review_mutate(&item.id, Some(&run.id), |ctx| {
            let Some(current) = ctx.run.clone() else {
                return Ok((None, None));
            };
            if current.result_request_id.is_some() {
                return Ok((None, Some(current)));
            }
            let mut next = current.clone();
            next.result = Some(result.clone());
            next.result_request_id = Some(request_id.clone());
            next.late_result = late;
            next.state = state;
            next.detail = detail.clone().or(next.detail);
            next.attention = attention.or(next.attention);
            next.profile.observed_model = observed_model.clone();
            next.profile.observed_harness = observed_harness;
            next.updated_at_ms = crate::now_ms();
            if late {
                next.outcome = Some(ReviewRunOutcome::Historical);
            } else if let Some(kind) = attention {
                next.outcome = Some(ReviewRunOutcome::NeedsAttention { reason: kind });
            }
            let mut write = ReviewWrite::new(
                format!("{}:v{}:submit", current.id, current.version),
                BoardOperation::ReviewSubmit,
            )
            .actor(actor.clone())
            .row(run_row(&next, Some(current.version)));
            if accepted && let Some(intent) = &ctx.intent {
                let mut intent = intent.clone();
                intent.review_passes = intent.review_passes.saturating_add(1);
                intent.updated_at_ms = crate::now_ms();
                write = write.row(intent_row(&intent));
            }
            let blocking = blocking_count(&result.findings);
            write = write.event(match (accepted, attention) {
                (true, _) => WorkItemEventKind::ReviewResult {
                    review_run_id: current.id.clone(),
                    verdict: result.verdict,
                    blocking_findings: blocking,
                },
                (false, Some(kind)) => WorkItemEventKind::AttentionRaised { reason: kind },
                (false, None) => WorkItemEventKind::ReviewStateChanged {
                    review_run_id: current.id.clone(),
                    state,
                },
            });
            if accepted {
                let outcome = evidence::review_result_outcome(&next, &actor);
                write = write.row(BoardRowWrite::Outcome(Box::new(outcome)));
            }
            next.version = current.version + 1;
            Ok((Some(write), Some(next)))
        })?;
        let updated = updated.unwrap_or(run);
        if accepted && mutation.is_some() {
            // Apply now while the reviewer's turn is still open; the sweeper
            // retries any part a crash interrupts.
            if let Err(error) = self.review_apply_locked(&updated).await {
                tracing::warn!(run = %updated.id, %error, "review result application deferred");
            }
        }
        let updated = self
            .store
            .board_review_run(&updated.id)
            .map_err(storage)?
            .unwrap_or(updated);
        Ok(self.review_receipt(
            &request_id,
            updated,
            mutation.and_then(|receipt| receipt.event_seq),
        ))
    }

    /// After review and ticket success: archive the exact idle reviewer(s)
    /// with archive-then-close ordering, read back, journal partial results
    /// (Done · cleanup pending) and retry the same cleanup identity.
    pub(crate) async fn board_review_cleanup(
        &self,
        review_run_id: &str,
    ) -> BoardResult<ReviewCleanupReceipt> {
        let run = self
            .store
            .board_review_run(review_run_id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::invalid("reviewRunId", "no such review run"))?;
        let _gate = self.board_hub().gate(&run.work_item_id).await;
        self.review_cleanup_locked(review_run_id, true).await
    }

    /// Reviewer chat ids owned by a ticket generation, for shared Stop:
    /// reviewers of non-terminal runs of that generation, plus any recorded
    /// reviewer of the ticket whose turn is still live. Never the primary
    /// chat, its main parent or an unrelated sibling.
    pub(crate) fn board_review_owned_work(
        &self,
        work_item_id: &str,
        execution_generation: u64,
    ) -> BoardResult<Vec<String>> {
        let item = self.board_load(work_item_id)?;
        let runs = self
            .store
            .board_review_runs(work_item_id, RUNS_READ)
            .map_err(storage)?;
        let owned = self.review_owned_reviewers(&item, &runs)?;
        let mut ids = Vec::new();
        for run in &runs {
            let Some(reviewer) = run.reviewer_task_id.as_deref() else {
                continue;
            };
            if !owned.iter().any(|id| id == reviewer)
                || ids.iter().any(|id: &String| id == reviewer)
            {
                continue;
            }
            let live = self.sessions.active_turn_id(reviewer).is_some()
                || self.sessions.has_live_background_work(reviewer);
            if (run.execution_generation == execution_generation && !run.state.terminal()) || live {
                ids.push(reviewer.to_owned());
            }
        }
        Ok(ids)
    }

    /// Capture outcome evidence (diff hash, PR, checks, artifacts,
    /// documents) for a generation as `OutcomeReference` rows.
    pub(crate) async fn board_capture_outcomes(
        &self,
        work_item_id: &str,
        execution_generation: u64,
    ) -> BoardResult<Vec<OutcomeReference>> {
        let item = self.board_load(work_item_id)?;
        let runs = self
            .store
            .board_review_runs(work_item_id, RUNS_READ)
            .map_err(storage)?;
        let Some(run) = runs
            .iter()
            .find(|run| {
                run.execution_generation == execution_generation
                    && run.state == ReviewRunState::Completed
                    && !run.late_result
            })
            .or_else(|| {
                runs.iter()
                    .find(|run| run.execution_generation == execution_generation)
            })
        else {
            return Ok(Vec::new());
        };
        let outcomes = self.review_outcome_rows(&item, run)?;
        if outcomes.is_empty() {
            return Ok(outcomes);
        }
        let key = format!(
            "{}:capture:{}",
            run.id,
            short(&board_digest(
                &outcomes.iter().map(|o| &o.id).collect::<Vec<_>>()
            )?)
        );
        self.review_mutate(work_item_id, None, |_| {
            let mut write = ReviewWrite::new(key.clone(), BoardOperation::ReviewSubmit).event(
                WorkItemEventKind::OutcomeRecorded {
                    outcome_id: outcomes[0].id.clone(),
                    outcome: outcomes[0].kind,
                },
            );
            for outcome in &outcomes {
                write = write.row(BoardRowWrite::Outcome(Box::new(outcome.clone())));
            }
            Ok((Some(write), ()))
        })?;
        Ok(outcomes)
    }

    /// Evidence for the detail drawer (bounded, newest first).
    pub(crate) fn board_outcomes(
        &self,
        caller: &BoardCaller,
        request: BoardOutcomesRequest,
    ) -> BoardResult<BoardOutcomes> {
        let item = self.board_item_for(caller, &request.work_item_id, BoardAccess::Read)?;
        let limit = request.limit.unwrap_or(50).clamp(1, 200) as usize;
        Ok(BoardOutcomes {
            outcomes: self
                .store
                .board_outcomes(&item.id, limit)
                .map_err(storage)?,
            launches: self
                .store
                .board_launches(&item.id, limit)
                .map_err(storage)?,
        })
    }

    /// Review-derived phase and Needs-attention reasons for A2's
    /// `board_observe` to merge (never inferred from agent prose).
    pub(crate) fn board_review_observation(
        &self,
        item: &WorkItem,
        intent: Option<&BoardExecutionIntent>,
    ) -> BoardResult<(Option<BoardPhase>, Vec<BoardAttentionReason>)> {
        let runs = self.store.board_review_runs(&item.id, 8).map_err(storage)?;
        let mut attention = Vec::new();
        let generation = intent.map(|intent| intent.execution_generation);
        let latest = runs
            .iter()
            .find(|run| Some(run.execution_generation) == generation)
            .or_else(|| runs.first());
        if item.column == BoardColumn::Done {
            let cleanup = runs
                .iter()
                .find(|run| matches!(run.outcome, Some(ReviewRunOutcome::Completed { .. })));
            return Ok(match cleanup.map(|run| (run.cleanup.state, run)) {
                Some((ReviewCleanupState::Failed, run)) => {
                    attention.push(BoardAttentionReason {
                        kind: BoardAttentionKind::CleanupFailed,
                        detail: run
                            .cleanup
                            .detail
                            .clone()
                            .unwrap_or_else(|| "Reviewer cleanup failed.".into()),
                        attention_id: None,
                        task_id: run.reviewer_task_id.clone(),
                    });
                    (Some(BoardPhase::CleanupPending), attention)
                }
                Some((ReviewCleanupState::Pending | ReviewCleanupState::Archived, _)) => {
                    (Some(BoardPhase::CleanupPending), attention)
                }
                _ => (Some(BoardPhase::Complete), attention),
            });
        }
        let Some(run) = latest else {
            return Ok((None, attention));
        };
        if let (Some(kind), Some(ReviewRunOutcome::NeedsAttention { .. }) | None) =
            (run.attention, &run.outcome)
        {
            attention.push(BoardAttentionReason {
                kind,
                detail: run
                    .detail
                    .clone()
                    .unwrap_or_else(|| "The review needs attention.".into()),
                attention_id: None,
                task_id: run.reviewer_task_id.clone(),
            });
        }
        if Some(run.execution_generation) != generation || item.column != BoardColumn::Ready {
            return Ok((None, attention));
        }
        let phase = match (run.state, &run.outcome) {
            (
                ReviewRunState::Queued
                | ReviewRunState::Starting
                | ReviewRunState::Running
                | ReviewRunState::Waiting
                | ReviewRunState::Stale,
                _,
            ) => Some(BoardPhase::Reviewing),
            (ReviewRunState::OutcomeUnknown, _) => Some(BoardPhase::Unknown),
            (ReviewRunState::Completed, Some(ReviewRunOutcome::RepairRequested { .. })) => {
                Some(BoardPhase::Fixing)
            }
            (ReviewRunState::Completed, None) => Some(BoardPhase::Reviewing),
            _ => None,
        };
        Ok((phase, attention))
    }

    // ---- maintenance ----

    /// Resume every unsettled review step (launch, delivery, staleness,
    /// verdict application, cleanup) after start-up and on a short tick.
    pub(crate) fn start_board_review_maintenance(
        self: &std::sync::Arc<Self>,
        cancel: CancellationToken,
    ) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let weak = std::sync::Arc::downgrade(self);
        runtime.spawn(async move {
            let mut tracked: HashSet<String> = HashSet::new();
            let mut cursor: Option<u64> = None;
            let mut next_scan = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(SWEEP_INTERVAL) => {}
                }
                let Some(rpc) = weak.upgrade() else { break };
                if tokio::time::Instant::now() >= next_scan {
                    match rpc.review_scan_all() {
                        Ok((ids, head)) => {
                            tracked.extend(ids);
                            cursor = Some(cursor.map_or(head, |cursor| cursor.max(head)));
                            next_scan = tokio::time::Instant::now() + FULL_SCAN_INTERVAL;
                        }
                        Err(error) => tracing::warn!(%error, "board review scan deferred"),
                    }
                }
                if let Some(after) = cursor.as_mut() {
                    match rpc.review_scan_events(*after) {
                        Ok((ids, head)) => {
                            tracked.extend(ids);
                            *after = head;
                        }
                        Err(error) => tracing::warn!(%error, "board review journal read deferred"),
                    }
                }
                let mut settled = Vec::new();
                for id in tracked.iter().cloned().collect::<Vec<_>>() {
                    match rpc.review_sweep_ticket(&id).await {
                        Ok(true) => settled.push(id),
                        Ok(false) => {}
                        Err(BoardError::NotFound { .. }) => settled.push(id),
                        Err(error) => {
                            tracing::warn!(ticket = %id, %error, "board review step deferred")
                        }
                    }
                }
                for id in settled {
                    tracked.remove(&id);
                }
            }
        });
    }

    /// Every Ready ticket and every Done ticket with review runs (bounded).
    fn review_scan_all(&self) -> BoardResult<(Vec<String>, u64)> {
        let head = self.store.board_event_head().map_err(storage)?;
        let mut ids = Vec::new();
        let mut after = None;
        for _ in 0..20 {
            let page = self
                .store
                .board_list(&BoardListQuery {
                    owner_host_id: self.engine_info.device_id.clone(),
                    project_id: None,
                    columns: vec![BoardColumn::Ready, BoardColumn::Done],
                    archived: luna_proto::BoardArchivedFilter::Include,
                    text: None,
                    after: after.clone(),
                    limit: 500,
                })
                .map_err(storage)?;
            let done = page.len() < 500;
            if let Some(last) = page.last() {
                after = Some(luna_sync::BoardListCursor {
                    project_id: last.project_id.clone(),
                    column_order: last.column.order(),
                    rank: last.rank.clone(),
                    id: last.id.clone(),
                });
            }
            for item in page {
                if item.column == BoardColumn::Ready
                    || !self
                        .store
                        .board_review_runs(&item.id, 1)
                        .map_err(storage)?
                        .is_empty()
                {
                    ids.push(item.id);
                }
            }
            if done {
                break;
            }
        }
        Ok((ids, head))
    }

    /// Tickets with journal activity since `after`.
    fn review_scan_events(&self, after: u64) -> BoardResult<(Vec<String>, u64)> {
        let events = self
            .store
            .board_events(&BoardEventQuery {
                after_seq: after,
                work_item_id: None,
                project_id: None,
                activity_only: false,
                limit: 500,
            })
            .map_err(storage)?;
        let head = events.last().map_or(after, |event| event.seq);
        Ok((
            events.into_iter().map(|event| event.work_item_id).collect(),
            head,
        ))
    }

    /// Advance one ticket's review work. `Ok(true)` when nothing is left to
    /// watch for it.
    async fn review_sweep_ticket(&self, work_item_id: &str) -> BoardResult<bool> {
        let _gate = self.board_hub().gate(work_item_id).await;
        let item = self.board_load(work_item_id)?;
        let runs = self
            .store
            .board_review_runs(work_item_id, RUNS_READ)
            .map_err(storage)?;
        let mut settled = true;
        for run in runs.iter().rev() {
            match self.review_advance(&item, run).await {
                Ok(done) => settled &= done,
                Err(error) => {
                    settled = false;
                    tracing::warn!(run = %run.id, %error, "review step deferred");
                }
            }
        }
        if item.column == BoardColumn::Ready {
            let item = self.board_load(work_item_id)?;
            self.review_auto_trigger(&item).await?;
            // Ready tickets stay watched for reports and repairs.
            settled = false;
        }
        Ok(settled)
    }

    /// One step for one run. `Ok(true)` when the run needs no more work.
    async fn review_advance(&self, item: &WorkItem, run: &ReviewRun) -> BoardResult<bool> {
        let intent = self.store.board_intent(&item.id).map_err(storage)?;
        let stale_owner = not_current(item, intent.as_ref(), run.execution_generation);
        match run.state {
            ReviewRunState::Queued
            | ReviewRunState::Starting
            | ReviewRunState::Running
            | ReviewRunState::Waiting
            | ReviewRunState::OutcomeUnknown
                if stale_owner.is_some() =>
            {
                let reason = stale_owner.unwrap_or_default();
                let stop = intent
                    .as_ref()
                    .and_then(|intent| intent.stop.as_ref())
                    .map(|stop| stop.request_id.clone());
                self.review_cancel_run(run, &reason, stop).await?;
                Ok(true)
            }
            ReviewRunState::Queued => self.review_launch(item, run).await,
            ReviewRunState::Starting | ReviewRunState::OutcomeUnknown => {
                self.review_deliver_packet(item, run).await
            }
            ReviewRunState::Running | ReviewRunState::Waiting => self.review_watch(run).await,
            ReviewRunState::Completed if run.outcome.is_none() && !run.late_result => {
                self.review_apply_locked(run).await
            }
            ReviewRunState::Completed => {
                if let Some(ReviewRunOutcome::RepairRequested { .. }) = run.outcome
                    && run.delivered_id.is_none()
                    && stale_owner.is_none()
                {
                    return self.review_send_repair(run).await;
                }
                if matches!(run.outcome, Some(ReviewRunOutcome::Completed { .. })) {
                    return self.review_cleanup_step(run).await;
                }
                Ok(true)
            }
            ReviewRunState::Failed | ReviewRunState::Cancelled | ReviewRunState::Stale => {
                // A reviewer still finishing its turn after a terminal run
                // keeps the ticket watched until it is idle.
                Ok(run
                    .reviewer_task_id
                    .as_deref()
                    .is_none_or(|reviewer| self.sessions.active_turn_id(reviewer).is_none()))
            }
        }
    }

    /// Queued → Starting: once the implementer is quiet, capture the basis,
    /// assemble the packet and persist both before any effect.
    async fn review_launch(&self, item: &WorkItem, run: &ReviewRun) -> BoardResult<bool> {
        let runs = self
            .store
            .board_review_runs(&item.id, RUNS_READ)
            .map_err(storage)?;
        let reviewers = self.review_owned_reviewers(item, &runs)?;
        if self
            .review_quiescent(&run.primary_task_id, &reviewers)
            .await
            .is_err()
        {
            return Ok(false);
        }
        let intent = self.store.board_intent(&item.id).map_err(storage)?;
        if intent
            .as_ref()
            .is_some_and(|intent| intent.review_passes >= BOARD_MAX_REVIEW_PASSES)
        {
            self.review_fail(
                run,
                BoardAttentionKind::ReviewBlocked,
                &format!(
                    "{} reached its repair cap ({BOARD_MAX_REVIEW_PASSES} review passes); an explicit user Retry is required.",
                    item.key
                ),
            )?;
            return Ok(true);
        }
        if let Err((error, kind)) = self.review_check_available(&run.profile.resolved).await {
            self.review_fail(run, kind, &error.message())?;
            return Ok(true);
        }
        let primary = self
            .workspace
            .chat(&run.primary_task_id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::Unavailable {
                reason: "The ticket's working chat is unavailable.".into(),
            })?;
        // Reuse the base of this cycle's earlier passes so the final review
        // covers every repair, committed or not.
        let prior_base = runs
            .iter()
            .filter(|other| other.id != run.id)
            .find_map(|other| other.basis.as_ref()?.base_revision.clone());
        let cwd = primary.cwd.clone().unwrap_or_else(|| ".".into());
        let capture = match self
            .review_capture(&cwd, prior_base.as_deref(), &run.evidence)
            .await
        {
            Ok(capture) => capture,
            Err(error) => {
                self.review_fail(run, BoardAttentionKind::ReviewUnavailable, &error.message())?;
                return Ok(true);
            }
        };
        let outcomes = self.store.board_outcomes(&item.id, 200).map_err(storage)?;
        let packet = self
            .review_packet(item, run, &primary, &capture, &outcomes)
            .await?;
        self.review_store_packet(&run.id, &packet.text)?;
        let basis = packet.basis.clone();
        self.review_update_run(
            run,
            "launch",
            |next| {
                Some(WorkItemEventKind::ReviewStateChanged {
                    review_run_id: next.id.clone(),
                    state: next.state,
                })
            },
            |next, intent| {
                if next.state != ReviewRunState::Queued {
                    return false;
                }
                next.state = ReviewRunState::Starting;
                next.basis = Some(basis.clone());
                if let Some(intent) = intent {
                    intent.observed_phase = BoardPhase::Reviewing;
                    intent.updated_at_ms = crate::now_ms();
                }
                true
            },
        )?;
        let run = self
            .store
            .board_review_run(&run.id)
            .map_err(storage)?
            .ok_or_else(|| storage("review run disappeared"))?;
        self.review_deliver_packet(item, &run).await
    }

    /// Record an unresolved run (Needs attention). Never a success.
    fn review_fail(
        &self,
        run: &ReviewRun,
        kind: BoardAttentionKind,
        detail: &str,
    ) -> BoardResult<()> {
        self.review_update_run(
            run,
            "fail",
            |_| Some(WorkItemEventKind::AttentionRaised { reason: kind }),
            |next, _| {
                if next.state.terminal() {
                    return false;
                }
                next.state = ReviewRunState::Failed;
                next.attention = Some(kind);
                next.detail = Some(detail.into());
                next.outcome = Some(ReviewRunOutcome::NeedsAttention { reason: kind });
                true
            },
        )?;
        Ok(())
    }

    /// Running/Waiting: follow the reviewer's session; a turn that ended
    /// without `review_submit` is not a result.
    async fn review_watch(&self, run: &ReviewRun) -> BoardResult<bool> {
        let Some(reviewer) = run.reviewer_task_id.as_deref() else {
            return Ok(false);
        };
        let waiting = self
            .sessions
            .session_status(reviewer)
            .is_some_and(|session| {
                matches!(
                    session.status,
                    luna_proto::SessionStatus::AwaitingApproval
                        | luna_proto::SessionStatus::AwaitingInput
                )
            });
        let wanted = if waiting {
            ReviewRunState::Waiting
        } else {
            ReviewRunState::Running
        };
        if wanted != run.state {
            self.review_update_run(
                run,
                if waiting { "waiting" } else { "running" },
                |next| {
                    Some(WorkItemEventKind::ReviewStateChanged {
                        review_run_id: next.id.clone(),
                        state: next.state,
                    })
                },
                |next, _| {
                    if !matches!(
                        next.state,
                        ReviewRunState::Running | ReviewRunState::Waiting
                    ) {
                        return false;
                    }
                    next.state = wanted;
                    true
                },
            )?;
            return Ok(false);
        }
        if !self.review_turn_finished(reviewer, &delivery::packet_message_id(&run.id))? {
            // Time bound for an unattended review (a reviewer waiting for the
            // user's approval or input is not timed out).
            let started = run
                .basis
                .as_ref()
                .map_or(run.updated_at_ms, |basis| basis.captured_at_ms);
            if run.state == ReviewRunState::Running && crate::now_ms() - started > REVIEW_TIMEOUT_MS
            {
                self.review_stop_reviewer(run, reviewer).await;
                self.review_fail(
                    run,
                    BoardAttentionKind::ReviewUnavailable,
                    "The review timed out; a timeout never completes a ticket.",
                )?;
                return Ok(true);
            }
            return Ok(false);
        }
        // The turn ended and no structured result arrived (prose, provider
        // exit, crash or restart). Drop an undelivered packet row, then
        // relaunch once into the same reviewer.
        let detail = "The reviewer turn ended without calling review_submit; prose or a provider exit is not a review result.";
        self.review_stop_reviewer(run, reviewer).await;
        if run.relaunches < RELAUNCHES_MAX {
            self.review_supersede(run, detail, run.relaunches + 1)?;
        } else {
            self.review_fail(run, BoardAttentionKind::ReviewUnavailable, detail)?;
        }
        Ok(true)
    }

    /// Whether `message_id` was delivered to `task_id` and its turn ended.
    fn review_turn_finished(&self, task_id: &str, message_id: &str) -> BoardResult<bool> {
        if self.sessions.active_turn_id(task_id).is_some()
            || self
                .sessions
                .session_status(task_id)
                .is_some_and(|session| {
                    matches!(
                        session.status,
                        luna_proto::SessionStatus::Working
                            | luna_proto::SessionStatus::AwaitingApproval
                            | luna_proto::SessionStatus::AwaitingInput
                    )
                })
        {
            return Ok(false);
        }
        match self
            .doc_host
            .queued_message(task_id, message_id)
            .map_err(storage)?
        {
            // A restart left the delivery unknown and never replays it: no
            // turn is running for it, so the pass cannot finish by itself.
            Some(queued)
                if matches!(
                    queued.status,
                    luna_doc::QueueMessageStatus::DeliveryUnknown { .. }
                ) =>
            {
                return Ok(true);
            }
            Some(_) => return Ok(false),
            None => {}
        }
        let entries = self
            .doc_host
            .open(task_id)
            .map_err(storage)?
            .doc()
            .read_entries()
            .map_err(storage)?;
        let Some(position) = entries.iter().position(|entry| entry.id == message_id) else {
            return Ok(false);
        };
        // A reply (or an aborted streaming entry) after the message proves
        // the turn ran; a turn aborted before any reply is settled after a
        // grace period so dispatch never races this check.
        Ok(entries.len() > position + 1 || crate::now_ms() - entries[position].created_at > 30_000)
    }

    /// Replace a run that cannot finish (stale basis or a reviewer turn that
    /// ended without a result) with a fresh pass reusing its reviewer.
    fn review_supersede(&self, run: &ReviewRun, detail: &str, relaunches: u8) -> BoardResult<()> {
        let next_id = format!(
            "rr-{}",
            &board_digest(&("board-review-rerun", &run.id))?[..32]
        );
        let now = crate::now_ms();
        self.review_mutate(&run.work_item_id, Some(&run.id), |ctx| {
            let Some(current) = ctx.run.clone() else {
                return Ok((None, ()));
            };
            if self
                .store
                .board_review_run(&next_id)
                .map_err(storage)?
                .is_some()
            {
                return Ok((None, ()));
            }
            let mut old = current.clone();
            if !old.state.terminal() {
                old.state = ReviewRunState::Failed;
                old.detail = Some(detail.into());
            }
            old.updated_at_ms = now;
            let mut fresh = current.clone();
            fresh.id = next_id.clone();
            fresh.version = 1;
            fresh.launch_request_id = format!("{next_id}:launch");
            fresh.basis = None;
            fresh.state = ReviewRunState::Queued;
            fresh.result = None;
            fresh.result_request_id = None;
            fresh.delivered_id = None;
            fresh.stop_request_id = None;
            fresh.late_result = false;
            fresh.outcome = None;
            fresh.detail = None;
            fresh.attention = None;
            fresh.profile.observed_model = None;
            fresh.profile.observed_harness = None;
            fresh.cleanup = ReviewCleanup {
                state: ReviewCleanupState::NotStarted,
                cleanup_id: format!("{next_id}:cleanup"),
                attempts: 0,
                archived_at_ms: None,
                detail: None,
            };
            fresh.created_at_ms = now;
            fresh.updated_at_ms = now;
            fresh.requested_by = Some(engine_actor());
            fresh.supersedes = Some(current.id.clone());
            fresh.relaunches = relaunches;
            let write = ReviewWrite::new(
                format!("{}:v{}:supersede", current.id, current.version),
                BoardOperation::ReviewStart,
            )
            .row(run_row(&old, Some(current.version)))
            .row(run_row(&fresh, None))
            .event(WorkItemEventKind::ReviewStarted {
                review_run_id: next_id.clone(),
                round: fresh.round,
            });
            Ok((Some(write), ()))
        })?;
        Ok(())
    }

    /// Apply a completed, current result exactly once: Done gate, bounded
    /// repair, or Needs attention. Caller holds the ticket gate.
    async fn review_apply_locked(&self, run: &ReviewRun) -> BoardResult<bool> {
        let item = self.board_load(&run.work_item_id)?;
        let intent = self.store.board_intent(&item.id).map_err(storage)?;
        let Some(result) = run.result.clone() else {
            return Ok(true);
        };
        if not_current(&item, intent.as_ref(), run.execution_generation).is_some() {
            // A crash between the Done transition and recording this pass:
            // finish recording instead of calling it history.
            if result.verdict == ReviewVerdict::Passed
                && self.review_done_for(&item.id, run.execution_generation)?
            {
                self.review_record_completion(run)?;
                let run = self
                    .store
                    .board_review_run(&run.id)
                    .map_err(storage)?
                    .unwrap_or_else(|| run.clone());
                self.review_result_notice(&item, &run, None).await;
                return self.review_cleanup_step(&run).await;
            }
            self.review_set_outcome(run, ReviewRunOutcome::Historical, None, None)?;
            return Ok(true);
        }
        let intent = intent.expect("checked by not_current");
        match result.verdict {
            ReviewVerdict::Passed => self.review_done_gate(&item, &intent, run).await,
            ReviewVerdict::ChangesRequested => {
                let runs = self
                    .store
                    .board_review_runs(&item.id, RUNS_READ)
                    .map_err(storage)?;
                let previous = runs.iter().find(|other| {
                    other.id != run.id
                        && other.state == ReviewRunState::Completed
                        && !other.late_result
                        && other.created_at_ms <= run.created_at_ms
                        && other
                            .result
                            .as_ref()
                            .is_some_and(|result| result.verdict == ReviewVerdict::ChangesRequested)
                });
                let blocking = result
                    .findings
                    .iter()
                    .filter(|finding| finding.blocking)
                    .collect::<Vec<_>>();
                let repeated = previous.and_then(|previous| {
                    let old = previous.result.as_ref()?;
                    let same_diff = previous.basis.as_ref().map(|basis| &basis.diff_hash)
                        == run.basis.as_ref().map(|basis| &basis.diff_hash);
                    let all_repeat = blocking.iter().all(|finding| {
                        old.findings.iter().any(|before| {
                            before.blocking
                                && finding_fingerprint(before) == finding_fingerprint(finding)
                        })
                    });
                    (same_diff || all_repeat).then_some(same_diff)
                });
                if let Some(same_diff) = repeated {
                    let detail = if same_diff {
                        "No progress: the result did not change since the previous review and blocking findings remain."
                    } else {
                        "No progress: the same blocking findings remain after a repair round."
                    };
                    self.review_needs_attention(
                        &item,
                        run,
                        BoardAttentionKind::ReviewBlocked,
                        detail,
                    )
                    .await?;
                    return Ok(true);
                }
                if intent.repair_rounds >= BOARD_MAX_REPAIR_ROUNDS {
                    self.review_needs_attention(
                        &item,
                        run,
                        BoardAttentionKind::ReviewBlocked,
                        &format!(
                            "Blocking findings remain after {BOARD_MAX_REPAIR_ROUNDS} repair rounds; an explicit user Retry is required for another cycle."
                        ),
                    )
                    .await?;
                    return Ok(true);
                }
                self.review_request_repair(&item, &intent, run).await
            }
            ReviewVerdict::Blocked | ReviewVerdict::Failed | ReviewVerdict::Cancelled => {
                let detail = format!(
                    "The reviewer reported {:?}: {}",
                    result.verdict, result.summary
                );
                self.review_needs_attention(
                    &item,
                    run,
                    BoardAttentionKind::ReviewUnavailable,
                    &detail,
                )
                .await?;
                Ok(true)
            }
        }
    }

    fn review_set_outcome(
        &self,
        run: &ReviewRun,
        outcome: ReviewRunOutcome,
        attention: Option<BoardAttentionKind>,
        detail: Option<String>,
    ) -> BoardResult<Option<ReviewRun>> {
        self.review_update_run(
            run,
            "outcome",
            |next| {
                next.attention
                    .filter(|_| {
                        matches!(next.outcome, Some(ReviewRunOutcome::NeedsAttention { .. }))
                    })
                    .map(|reason| WorkItemEventKind::AttentionRaised { reason })
            },
            |next, _| {
                if next.outcome.is_some() {
                    return false;
                }
                next.outcome = Some(outcome.clone());
                if attention.is_some() {
                    next.attention = attention;
                }
                if detail.is_some() {
                    next.detail = detail.clone();
                }
                true
            },
        )
    }

    /// Stop automatic repairs; keep the findings visible in the ticket chat.
    async fn review_needs_attention(
        &self,
        item: &WorkItem,
        run: &ReviewRun,
        kind: BoardAttentionKind,
        detail: &str,
    ) -> BoardResult<()> {
        let updated = self.review_set_outcome(
            run,
            ReviewRunOutcome::NeedsAttention { reason: kind },
            Some(kind),
            Some(detail.into()),
        )?;
        let run = updated.unwrap_or_else(|| run.clone());
        self.review_result_notice(item, &run, Some(detail)).await;
        Ok(())
    }

    /// Ready tickets: queue the next pass when the engine owns the trigger
    /// (a delivered repair the implementer finished, a stale or relaunched
    /// basis, or a current-generation ReadyForReview report).
    async fn review_auto_trigger(&self, item: &WorkItem) -> BoardResult<bool> {
        let Some(intent) = self.store.board_intent(&item.id).map_err(storage)? else {
            return Ok(true);
        };
        if not_current(item, Some(&intent), intent.execution_generation).is_some() {
            return Ok(true);
        }
        let runs = self
            .store
            .board_review_runs(&item.id, RUNS_READ)
            .map_err(storage)?;
        let latest = runs
            .iter()
            .max_by_key(|run| (run.created_at_ms, run.version));
        let Some(working) = self.review_working_link(&item.id)? else {
            return Ok(true);
        };
        let reviewers = self.review_owned_reviewers(item, &runs)?;
        let trigger = match latest {
            Some(run)
                if run.state == ReviewRunState::Completed
                    && run.execution_generation == intent.execution_generation
                    && matches!(run.outcome, Some(ReviewRunOutcome::RepairRequested { .. }))
                    && run.delivered_id.is_some() =>
            {
                let message = delivery::repair_message_id(&run.id);
                if self
                    .doc_host
                    .queued_message(&working.task_id, &message)
                    .map_err(storage)?
                    .is_some_and(|queued| {
                        matches!(
                            queued.status,
                            luna_doc::QueueMessageStatus::DeliveryUnknown { .. }
                        )
                    })
                {
                    // Never re-review an unchanged result as if repaired.
                    self.review_update_run(
                        run,
                        "repair-unknown",
                        |_| {
                            Some(WorkItemEventKind::AttentionRaised {
                                reason: BoardAttentionKind::DeliveryUnknown,
                            })
                        },
                        |next, _| {
                            if next.attention == Some(BoardAttentionKind::DeliveryUnknown) {
                                return false;
                            }
                            next.attention = Some(BoardAttentionKind::DeliveryUnknown);
                            next.detail = Some(
                                "The repair request's delivery is unknown after a restart; resend it from the ticket chat's queue.".into(),
                            );
                            true
                        },
                    )?;
                    return Ok(true);
                }
                if !self.review_turn_finished(&working.task_id, &message)?
                    || self
                        .review_quiescent(&working.task_id, &reviewers)
                        .await
                        .is_err()
                {
                    return Ok(false);
                }
                // An implementer that reported a blocker after the repair
                // request is not re-reviewed automatically.
                if item.last_report.as_ref().is_some_and(|report| {
                    report.report.kind == BoardStatusReportKind::Blocked
                        && report.at_ms > run.updated_at_ms
                }) {
                    return Ok(true);
                }
                Some((
                    format!("{}:rereview", run.id),
                    run.profile.requested.clone(),
                ))
            }
            Some(run)
                if run.state == ReviewRunState::Stale
                    && run.outcome.is_none()
                    && run.execution_generation == intent.execution_generation
                    && !runs
                        .iter()
                        .any(|other| other.supersedes.as_deref() == Some(&run.id)) =>
            {
                let stale_streak = runs
                    .iter()
                    .take_while(|other| other.state == ReviewRunState::Stale)
                    .count();
                if stale_streak > STALE_REVIEWS_MAX {
                    self.review_set_outcome(
                        run,
                        ReviewRunOutcome::NeedsAttention {
                            reason: BoardAttentionKind::ReviewBlocked,
                        },
                        Some(BoardAttentionKind::ReviewBlocked),
                        Some("The result kept changing during review; stop the changes, then review again.".into()),
                    )?;
                    return Ok(true);
                }
                if self
                    .review_quiescent(&working.task_id, &reviewers)
                    .await
                    .is_err()
                {
                    return Ok(false);
                }
                self.review_supersede(
                    run,
                    run.detail.as_deref().unwrap_or("stale"),
                    run.relaunches,
                )?;
                return Ok(false);
            }
            _ => {
                let report = item.last_report.as_ref().filter(|report| {
                    report.report.kind == BoardStatusReportKind::ReadyForReview
                        && report.execution_generation == intent.execution_generation
                        && latest.is_none_or(|run| report.at_ms > run.created_at_ms)
                });
                let Some(report) = report else {
                    return Ok(true);
                };
                if self
                    .review_quiescent(&working.task_id, &reviewers)
                    .await
                    .is_err()
                {
                    return Ok(false);
                }
                Some((
                    format!("report:{}:{}", item.id, report.at_ms),
                    luna_proto::BoardReviewProfile::default(),
                ))
            }
        };
        let Some((key, profile)) = trigger else {
            return Ok(true);
        };
        let key = format!("auto:{}", short(&board_digest(&key)?));
        let request = BoardReviewStartRequest {
            request_id: key,
            work_item_id: item.id.clone(),
            execution_generation: Some(intent.execution_generation),
            profile: (profile != luna_proto::BoardReviewProfile::default()).then_some(profile),
            ..Default::default()
        };
        // Same gate already held: run the start body without re-locking.
        match self.review_start_locked(item, request).await {
            Ok(_) | Err(BoardError::Busy { .. }) => Ok(false),
            Err(BoardError::NotAuthorized { reason }) => {
                tracing::debug!(ticket = %item.id, %reason, "automatic review not started");
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }

    /// The engine's own start (automatic re-review); the caller holds the
    /// ticket gate.
    async fn review_start_locked(
        &self,
        item: &WorkItem,
        request: BoardReviewStartRequest,
    ) -> BoardResult<ReviewRunReceipt> {
        let caller = engine_caller();
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let run_id = review_run_id(&request_id)?;
        if let Some(run) = self.store.board_review_run(&run_id).map_err(storage)? {
            return Ok(self.review_receipt(&request_id, run, None));
        }
        let working =
            self.review_working_link(&item.id)?
                .ok_or_else(|| BoardError::Unavailable {
                    reason: "The ticket has no working chat.".into(),
                })?;
        let primary = self
            .workspace
            .chat(&working.task_id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::Unavailable {
                reason: "The ticket's working chat is unavailable.".into(),
            })?;
        let generation = request.execution_generation.unwrap_or_default();
        let resolved = self
            .board_resolve_review_profile(item, &primary, request.profile.as_ref())
            .await;
        let placement = self.review_placement(&working, &primary);
        let now = crate::now_ms();
        let (run, _) = self.review_mutate(&item.id, Some(&run_id), |ctx| {
            if let Some(run) = &ctx.run {
                return Ok((None, run.clone()));
            }
            let intent = ctx
                .intent
                .as_ref()
                .ok_or_else(|| BoardError::NotAuthorized {
                    reason: "No execution intent.".into(),
                })?;
            if let Some(reason) = not_current(&ctx.item, Some(intent), generation) {
                return Err(BoardError::NotAuthorized { reason });
            }
            if intent.review_passes >= BOARD_MAX_REVIEW_PASSES {
                return Err(BoardError::NotAuthorized {
                    reason: "Repair cap reached.".into(),
                });
            }
            let runs = self
                .store
                .board_review_runs(&ctx.item.id, RUNS_READ)
                .map_err(storage)?;
            if runs.iter().any(|run| {
                run.execution_generation == generation
                    && (!run.state.terminal()
                        || (run.state == ReviewRunState::Completed && run.outcome.is_none()))
            }) {
                return Err(BoardError::Busy {
                    reason: "A review is already in progress.".into(),
                });
            }
            let (state, detail, attention) = match &resolved.failure {
                Some((error, kind)) => (ReviewRunState::Failed, Some(error.message()), Some(*kind)),
                None => (ReviewRunState::Queued, None, None),
            };
            let run = ReviewRun {
                id: run_id.clone(),
                work_item_id: ctx.item.id.clone(),
                version: 1,
                execution_generation: generation,
                round: intent.repair_rounds,
                primary_task_id: working.task_id.clone(),
                reviewer_task_id: None,
                display_main_parent_id: placement.1.clone(),
                placement: placement.0,
                launch_request_id: format!("{run_id}:launch"),
                profile: resolved.resolution.clone(),
                basis: None,
                state,
                result: None,
                delivered_id: None,
                stop_request_id: None,
                cleanup: ReviewCleanup {
                    state: ReviewCleanupState::NotStarted,
                    cleanup_id: format!("{run_id}:cleanup"),
                    attempts: 0,
                    archived_at_ms: None,
                    detail: None,
                },
                created_at_ms: now,
                updated_at_ms: now,
                detail,
                attention,
                requested_by: Some(engine_actor()),
                result_request_id: None,
                late_result: false,
                outcome: attention.map(|kind| ReviewRunOutcome::NeedsAttention { reason: kind }),
                implementer_summary: ctx
                    .item
                    .last_report
                    .as_ref()
                    .filter(|report| report.execution_generation == generation)
                    .map(|report| report.report.summary.chars().take(SUMMARY_MAX).collect()),
                evidence: ctx
                    .item
                    .last_report
                    .as_ref()
                    .filter(|report| report.execution_generation == generation)
                    .map(|report| {
                        report
                            .report
                            .evidence
                            .iter()
                            .take(EVIDENCE_MAX)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default(),
                reported_check_ids: Vec::new(),
                supersedes: None,
                relaunches: 0,
            };
            let event = match attention {
                Some(kind) => WorkItemEventKind::AttentionRaised { reason: kind },
                None => WorkItemEventKind::ReviewStarted {
                    review_run_id: run_id.clone(),
                    round: run.round,
                },
            };
            Ok((
                Some(
                    ReviewWrite::new(format!("{run_id}:create"), BoardOperation::ReviewStart)
                        .row(run_row(&run, None))
                        .event(event),
                ),
                run,
            ))
        })?;
        Ok(self.review_receipt(&request_id, run, None))
    }

    /// The Done gate: current checks, current review passed, Ready intent and
    /// the matching generation, the implementer quiet, evidence persisted
    /// with the transition. Then reviewer cleanup.
    async fn review_done_gate(
        &self,
        item: &WorkItem,
        intent: &BoardExecutionIntent,
        run: &ReviewRun,
    ) -> BoardResult<bool> {
        let Some(basis) = run.basis.clone() else {
            return Ok(true);
        };
        // Required checks must pass for exactly the reviewed snapshot.
        let outcomes = self.store.board_outcomes(&item.id, 200).map_err(storage)?;
        let missing = item
            .completion
            .required_checks
            .iter()
            .filter(|name| {
                !outcomes.iter().any(|outcome| {
                    outcome.kind == OutcomeKind::Check
                        && &outcome.label == *name
                        && outcome.execution_generation == run.execution_generation
                        && outcome.basis.as_deref() == Some(basis.diff_hash.as_str())
                        && outcome.state.as_deref() == Some("passed")
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            self.review_needs_attention(
                item,
                run,
                BoardAttentionKind::Other,
                &format!(
                    "Required checks have no passing result for the reviewed snapshot: {}. Skipped or stale checks never complete a ticket.",
                    missing.join(", ")
                ),
            )
            .await?;
            return Ok(true);
        }
        if item.completion.mode == BoardCompletionMode::ManualAcceptance {
            self.review_set_outcome(run, ReviewRunOutcome::AwaitingAcceptance, None, None)?;
            self.review_result_notice(
                item,
                run,
                Some("Review passed; awaiting manual acceptance."),
            )
            .await;
            return Ok(true);
        }
        let cwd = self
            .workspace
            .chat(&run.primary_task_id)
            .map_err(storage)?
            .and_then(|chat| chat.cwd)
            .unwrap_or_else(|| basis.cwd.clone());
        if let Some(reason) = self.review_basis_changed(run, &cwd).await? {
            // The passing result covers another snapshot now.
            self.review_update_run(
                run,
                "stale",
                |next| {
                    Some(WorkItemEventKind::ReviewStateChanged {
                        review_run_id: next.id.clone(),
                        state: next.state,
                    })
                },
                |next, intent| {
                    if next.outcome.is_some() {
                        return false;
                    }
                    next.state = ReviewRunState::Stale;
                    next.detail = Some(format!("{reason} The new basis is reviewed again."));
                    if let Some(intent) = intent {
                        intent.review_passes = intent.review_passes.saturating_sub(1);
                    }
                    true
                },
            )?;
            return Ok(false);
        }
        let runs = self
            .store
            .board_review_runs(&item.id, RUNS_READ)
            .map_err(storage)?;
        let reviewers = self.review_owned_reviewers(item, &runs)?;
        if self
            .review_quiescent(&run.primary_task_id, &reviewers)
            .await
            .is_err()
        {
            return Ok(false);
        }
        let generation = run.execution_generation;
        // 1. Evidence is durable before the transition.
        let outcomes = self.review_outcome_rows(item, run)?;
        if !outcomes.is_empty() {
            let key = format!("{}:v{}:evidence", run.id, run.version);
            self.review_mutate(&item.id, None, |_| {
                let mut write = ReviewWrite::new(key.clone(), BoardOperation::ReviewSubmit);
                for outcome in &outcomes {
                    write = write.row(BoardRowWrite::Outcome(Box::new(outcome.clone())));
                }
                Ok((Some(write), ()))
            })?;
        }
        // 2. The engine-owned Done transition, through the lifecycle slice:
        // only while the Ready intent, generation and intent revision match
        // and the worker is quiet.
        match self
            .board_mark_done_if_current(&item.id, generation, intent.intent_revision)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => {
                if !self.review_done_for(&item.id, generation)? {
                    // Held, stale or still busy: re-evaluated next sweep.
                    return Ok(false);
                }
            }
            Err(BoardError::NotImplemented { .. }) => {
                if !self.review_mark_done_standalone(item, run)? {
                    return Ok(false);
                }
            }
            Err(error) => return Err(error),
        }
        // 3. Record which pass completed the ticket; cleanup becomes due.
        self.review_record_completion(run)?;
        let run = self
            .store
            .board_review_run(&run.id)
            .map_err(storage)?
            .unwrap_or_else(|| run.clone());
        let item = self.board_load(&item.id)?;
        self.review_result_notice(&item, &run, None).await;
        // Cleanup runs after the Done transition and evidence are durable.
        self.review_cleanup_step(&run).await
    }

    /// Whether the ticket is Done for exactly this execution generation
    /// (journal readback; a Done from an older generation never counts).
    fn review_done_for(&self, work_item_id: &str, generation: u64) -> BoardResult<bool> {
        let item = self.board_load(work_item_id)?;
        if item.column != BoardColumn::Done {
            return Ok(false);
        }
        let events = self
            .store
            .board_recent_events(work_item_id, 100)
            .map_err(storage)?;
        Ok(events
            .iter()
            .find_map(|event| match &event.kind {
                WorkItemEventKind::Completed {
                    execution_generation,
                } => Some(*execution_generation == generation),
                WorkItemEventKind::Reopened { .. } | WorkItemEventKind::Moved { .. } => Some(false),
                _ => None,
            })
            .unwrap_or(false))
    }

    /// The Done transition when the lifecycle slice is not present in this
    /// build: the same guards (Ready intent, generation), one commit with
    /// the canonical `Completed` event.
    fn review_mark_done_standalone(&self, item: &WorkItem, run: &ReviewRun) -> BoardResult<bool> {
        let generation = run.execution_generation;
        let (done, _) = self.review_mutate(&item.id, None, |ctx| {
            if ctx.item.column == BoardColumn::Done {
                return Ok((None, self.review_done_for(&ctx.item.id, generation)?));
            }
            if not_current(&ctx.item, ctx.intent.as_ref(), generation).is_some() {
                return Ok((None, false));
            }
            let mut intent = ctx.intent.clone().expect("checked by not_current");
            intent.observed_phase = BoardPhase::Complete;
            intent.queue = None;
            intent.needs_reconcile = false;
            intent.updated_at_ms = crate::now_ms();
            let mut write = ReviewWrite::new(
                format!("done:{}:{generation}", ctx.item.id),
                BoardOperation::ReviewSubmit,
            )
            .row(intent_row(&intent))
            .event(WorkItemEventKind::Completed {
                execution_generation: generation,
            });
            write.column = Some(BoardColumn::Done);
            Ok((Some(write), true))
        })?;
        Ok(done)
    }

    /// Mark the pass that completed the ticket; cleanup becomes due.
    fn review_record_completion(&self, run: &ReviewRun) -> BoardResult<()> {
        let generation = run.execution_generation;
        self.review_update_run(
            run,
            "completed",
            |_| None,
            |next, intent| {
                if next.outcome.is_some() {
                    return false;
                }
                next.outcome = Some(ReviewRunOutcome::Completed {
                    execution_generation: generation,
                });
                next.cleanup.state = ReviewCleanupState::Pending;
                if let Some(intent) = intent {
                    intent.observed_phase = BoardPhase::CleanupPending;
                    intent.updated_at_ms = crate::now_ms();
                }
                true
            },
        )?;
        Ok(())
    }
}
