//! Cleanup after success (automatic-review §7): only after the ticket's
//! Done transition, review result and evidence are durable. Resolve only
//! the reviewer chat ids this ticket's runs recorded (and still link as
//! Reviewer), wait until each is idle, archive it through the ordinary
//! `setArchived` task command, read the archived state back, and only then
//! close their views (archive-then-close, as the existing archive-with-close
//! operation orders it). The authority is the recorded run ownership; the
//! descendant guard of `manage_workspace.archiveThreads` is not involved or
//! weakened. Partial and unknown outcomes are journaled and retried under
//! the same cleanup identity; the ticket shows Done · cleanup pending.
use super::*;
use luna_proto::{ChatWorkbenchCommand, MutateChatWorkbenchRequest};

fn backoff_ms(attempts: u32) -> i64 {
    (500i64 << attempts.min(6)).min(30_000)
}

impl EngineRpc {
    /// One cleanup attempt for the run that completed the ticket.
    /// `Ok(true)` once cleanup is settled (closed, or failed for attention).
    pub(super) async fn review_cleanup_step(&self, run: &ReviewRun) -> BoardResult<bool> {
        match run.cleanup.state {
            ReviewCleanupState::Closed | ReviewCleanupState::Failed => return Ok(true),
            ReviewCleanupState::NotStarted
                if !matches!(run.outcome, Some(ReviewRunOutcome::Completed { .. })) =>
            {
                return Ok(true);
            }
            _ => {}
        }
        if run.cleanup.attempts > 0
            && crate::now_ms() - run.updated_at_ms < backoff_ms(run.cleanup.attempts)
        {
            return Ok(false);
        }
        let receipt = self.review_cleanup_locked(&run.id, false).await?;
        Ok(matches!(
            receipt.state,
            ReviewCleanupState::Closed | ReviewCleanupState::Failed
        ))
    }

    /// The cleanup body; the caller holds the ticket gate. `explicit`
    /// retries a failed cleanup under the same identity.
    pub(super) async fn review_cleanup_locked(
        &self,
        review_run_id: &str,
        explicit: bool,
    ) -> BoardResult<ReviewCleanupReceipt> {
        let run = self
            .store
            .board_review_run(review_run_id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::invalid("reviewRunId", "no such review run"))?;
        let item = self.board_load(&run.work_item_id)?;
        let runs = self
            .store
            .board_review_runs(&item.id, RUNS_READ)
            .map_err(storage)?;
        let targets = self.review_owned_reviewers(&item, &runs)?;
        let not_eligible = !matches!(run.outcome, Some(ReviewRunOutcome::Completed { .. }))
            || item.column != BoardColumn::Done;
        if not_eligible
            || run.cleanup.state == ReviewCleanupState::Closed
            || (run.cleanup.state == ReviewCleanupState::Failed && !explicit)
        {
            let archived = targets
                .iter()
                .filter(|id| {
                    self.workspace
                        .chat(id)
                        .ok()
                        .flatten()
                        .is_none_or(|chat| chat.archived)
                })
                .cloned()
                .collect();
            return Ok(ReviewCleanupReceipt {
                review_run_id: run.id.clone(),
                state: run.cleanup.state,
                archived_task_ids: archived,
                closed_views: 0,
            });
        }
        let attempt = run.cleanup.attempts;
        let mut archived = Vec::new();
        let mut waiting = Vec::new();
        let mut failures = Vec::new();
        for target in &targets {
            // Defense in depth: the recorded owner chats are never targets.
            if target == &run.primary_task_id || target == &run.display_main_parent_id {
                continue;
            }
            let Some(chat) = self.workspace.chat(target).map_err(storage)? else {
                archived.push(target.clone());
                continue;
            };
            if chat.archived {
                archived.push(target.clone());
                continue;
            }
            // Only idle/terminal reviewers: a startup receipt or the
            // reviewer saying "done" is not idleness.
            if self.sessions.active_turn_id(target).is_some()
                || self.sessions.has_live_background_work(target)
                || self
                    .doc_host
                    .open(target)
                    .ok()
                    .and_then(|handle| handle.doc().read_queue().ok())
                    .is_some_and(|queue| !queue.is_empty())
            {
                waiting.push(target.clone());
                continue;
            }
            let command_id = format!(
                "{}:archive:{attempt}:{}",
                run.cleanup.cleanup_id,
                short(&board_digest(target)?)
            );
            let receipt = match self
                .doc_host
                .task_command_receipt(&command_id)
                .map_err(storage)?
            {
                Some(receipt) => Ok(receipt),
                None => {
                    let mut command = legacy_task_command(
                        target.clone(),
                        TaskCommandIntent::SetArchived { archived: true },
                    );
                    command.command_id = command_id.clone();
                    command.actor_id = ACTOR_ID.into();
                    command.client_id = "luna-board-review".into();
                    self.execute_task_command(command, true).await
                }
            };
            match receipt {
                Ok(receipt) if receipt.status == CommandReceiptStatus::Applied => {
                    // Independent readback of the archived state.
                    if self
                        .workspace
                        .chat(target)
                        .map_err(storage)?
                        .is_none_or(|chat| chat.archived)
                    {
                        archived.push(target.clone());
                    } else {
                        failures.push(format!("{target}: archive applied but not read back"));
                    }
                }
                Ok(receipt) => failures.push(format!(
                    "{target}: {}",
                    receipt
                        .reason
                        .map(|reason| reason.message)
                        .unwrap_or_else(|| format!("{:?}", receipt.status))
                )),
                Err(error) => failures.push(format!("{target}: outcome unknown ({error})")),
            }
        }
        let all_archived = archived.len() == targets.len();
        let mut closed_views = 0;
        let mut views_closed = false;
        if all_archived {
            match self.review_close_views(&run, &targets, attempt).await {
                Ok(count) => {
                    closed_views = count;
                    views_closed = true;
                }
                Err(error) => failures.push(error),
            }
        }
        let failed_attempt = !failures.is_empty();
        let attempts = if failed_attempt { attempt + 1 } else { attempt };
        let state = if views_closed {
            ReviewCleanupState::Closed
        } else if failed_attempt && attempts >= CLEANUP_ATTEMPTS_MAX {
            ReviewCleanupState::Failed
        } else if all_archived {
            ReviewCleanupState::Archived
        } else {
            ReviewCleanupState::Pending
        };
        let detail = if !failures.is_empty() {
            Some(format!("Cleanup pending: {}", failures.join("; ")))
        } else if !waiting.is_empty() {
            Some(format!(
                "Waiting for {} reviewer chat(s) to finish before archiving.",
                waiting.len()
            ))
        } else {
            None
        };
        let run_id = run.id.clone();
        self.review_mutate(&item.id, Some(&run.id), |ctx| {
            let Some(current) = ctx.run.clone() else {
                return Ok((None, ()));
            };
            let mut next = current.clone();
            if next.cleanup.state == ReviewCleanupState::Closed {
                return Ok((None, ()));
            }
            let changed = next.cleanup.state != state
                || next.cleanup.attempts != attempts
                || next.cleanup.detail != detail;
            if !changed {
                return Ok((None, ()));
            }
            next.cleanup.state = state;
            next.cleanup.attempts = attempts;
            next.cleanup.detail = detail.clone();
            if all_archived && next.cleanup.archived_at_ms.is_none() {
                next.cleanup.archived_at_ms = Some(crate::now_ms());
            }
            next.updated_at_ms = crate::now_ms();
            let mut write = ReviewWrite::new(
                format!("{}:v{}:cleanup", current.id, current.version),
                BoardOperation::ReviewSubmit,
            )
            .row(run_row(&next, Some(current.version)));
            if state == ReviewCleanupState::Closed
                && let Some(intent) = &ctx.intent
            {
                let mut intent = intent.clone();
                intent.observed_phase = BoardPhase::Complete;
                intent.updated_at_ms = crate::now_ms();
                write = write.row(intent_row(&intent));
            }
            write = match state {
                ReviewCleanupState::Closed => write.event(WorkItemEventKind::ReviewerCleaned {
                    review_run_id: run_id.clone(),
                    archived: true,
                }),
                ReviewCleanupState::Failed => write.event(WorkItemEventKind::AttentionRaised {
                    reason: BoardAttentionKind::CleanupFailed,
                }),
                // Journal partial/unknown outcomes; waiting is not journaled.
                _ if failed_attempt => write.event(WorkItemEventKind::ReviewerCleaned {
                    review_run_id: run_id.clone(),
                    archived: false,
                }),
                _ => write,
            };
            Ok((Some(write), ()))
        })?;
        Ok(ReviewCleanupReceipt {
            review_run_id: run.id,
            state,
            archived_task_ids: archived,
            closed_views,
        })
    }

    /// Close every open view of the archived reviewers in the workbenches
    /// that display them, then read the layouts back.
    async fn review_close_views(
        &self,
        run: &ReviewRun,
        targets: &[String],
        attempt: u32,
    ) -> Result<u32, String> {
        if self.chat_workbenches.is_none() || targets.is_empty() {
            // No local desktop layout on this engine: nothing displays them.
            return Ok(0);
        }
        let profile_id = self
            .engine_info
            .profile_id
            .clone()
            .ok_or_else(|| "the workbench profile is unavailable".to_string())?;
        let mut mains = vec![run.display_main_parent_id.clone()];
        for target in targets {
            if let Some(parent) = self.workspace.side_chat_parent(target)
                && !mains.contains(&parent)
            {
                mains.push(parent);
            }
        }
        let mut closed = 0u32;
        for main in mains {
            if self
                .workspace
                .chat(&main)
                .ok()
                .flatten()
                .is_none_or(|chat| chat.archived)
            {
                continue;
            }
            let layout = self
                .read_chat_workbench(&main)
                .map_err(|error| format!("{main}: layout unreadable ({error})"))?;
            let open = targets
                .iter()
                .filter(|target| layout.root.pane_for_chat(target).is_some())
                .cloned()
                .collect::<Vec<_>>();
            if !open.is_empty() {
                let command_id = format!(
                    "{}:close:{attempt}:{}",
                    run.cleanup.cleanup_id,
                    short(&board_digest(&main).map_err(|error| error.to_string())?)
                );
                self.mutate_chat_workbench_latest(MutateChatWorkbenchRequest {
                    main_chat_id: main.clone(),
                    profile_id: profile_id.clone(),
                    expected_revision: layout.revision,
                    command_id,
                    command: ChatWorkbenchCommand::CloseTabs {
                        chat_ids: open.clone(),
                    },
                })
                .map_err(|error| format!("{main}: views not closed ({error})"))?;
                closed += open.len() as u32;
            }
            let after = self
                .read_chat_workbench(&main)
                .map_err(|error| format!("{main}: layout unreadable ({error})"))?;
            if let Some(still) = targets
                .iter()
                .find(|target| after.root.pane_for_chat(target).is_some())
            {
                return Err(format!("{still}: view still open after close"));
            }
        }
        Ok(closed)
    }
}
