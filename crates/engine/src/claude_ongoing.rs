//! Claude Code goals, review and native schedules in the engine (CC-14/15).
//!
//! Every Claude path here uses Claude's own operations: `/goal <condition>`,
//! `/goal clear`, Claude's session record, `/code-review`. Codex goal or
//! review methods are never involved. Capabilities Claude does not have
//! (pause, resume, token budget, in-place goal edits, cloud ultrareview from
//! LUNA) are rejected with an explanation instead of being imitated.

use std::sync::Arc;

use luna_harness::{Harness, NativeGoalAction, NativeGoalControl};
use luna_proto::{
    ExecutionBinding, GoalOperation, GoalReadback, GoalStartRequest, GoalStatus, HarnessId,
    NativeSchedule, ReviewTarget, TaskIdentityView, ThreadGoal,
};

use crate::doc_host::DocHost;
use crate::sessions::SessionsEngine;
use crate::{EngineError, new_id, now_ms};

/// A Claude goal starts with Claude's `/goal`, which Claude runs as the next
/// prompt: queued behind a running turn it would start later, unannounced.
pub(crate) fn admit_goal_start(
    goal: &GoalStartRequest,
    turn_in_flight: bool,
) -> Result<(), EngineError> {
    if goal.token_budget.is_some() {
        return Err(EngineError::Other(
            "Claude goals have no token budget. Remove the budget and send the goal again.".into(),
        ));
    }
    if turn_in_flight {
        return Err(EngineError::Other(
            "Claude starts a goal between turns. Wait for the current turn to finish, or stop it, then send the goal."
                .into(),
        ));
    }
    Ok(())
}

/// A plain ref name for a review command line: no options, ranges or
/// spaces. `command` names the provider command in the refusal.
fn plain_review_ref(value: &str, label: &str, command: &str) -> Result<String, EngineError> {
    let value = value.trim();
    let ok = !value.is_empty()
        && value.len() <= 200
        && !value.starts_with('-')
        && !value.contains("..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/~^@-+".contains(&b));
    if ok {
        Ok(value.to_owned())
    } else {
        Err(EngineError::Other(format!(
            "{command} needs a plain git {label} name; {value:?} is not one."
        )))
    }
}

/// A review target as Claude's own `/code-review` argument. Custom
/// instructions are an ordinary chat message (Claude's command takes a
/// target, not free text). The cloud `ultra` lane is never reachable here.
pub(crate) fn review_prompt(target: &ReviewTarget) -> Result<String, EngineError> {
    target.validate().map_err(EngineError::Other)?;
    let git_ref =
        |value: &str, label: &str| plain_review_ref(value, label, "Claude's /code-review");
    Ok(match target {
        ReviewTarget::UncommittedChanges => "/code-review".into(),
        ReviewTarget::BaseBranch { branch } => {
            format!("/code-review {}...HEAD", git_ref(branch, "branch")?)
        }
        ReviewTarget::Commit { sha, .. } => {
            let sha = git_ref(sha, "commit")?;
            format!("/code-review {sha}~1...{sha}")
        }
        ReviewTarget::Custom { instructions } => instructions.trim().to_owned(),
    })
}

/// OC-15: OpenCode's built-in `/review [commit|branch]` line, the direct
/// counterpart of Claude's `/code-review` (a text review: OpenCode reports
/// no structured findings). Its template reads a commit as `git show <sha>`
/// and a branch as `git diff <branch>...HEAD`; custom instructions are an
/// ordinary message.
pub(crate) fn opencode_review_prompt(target: &ReviewTarget) -> Result<String, EngineError> {
    target.validate().map_err(EngineError::Other)?;
    let git_ref = |value: &str, label: &str| plain_review_ref(value, label, "OpenCode's /review");
    Ok(match target {
        ReviewTarget::UncommittedChanges => "/review".into(),
        ReviewTarget::BaseBranch { branch } => format!("/review {}", git_ref(branch, "branch")?),
        ReviewTarget::Commit { sha, .. } => format!("/review {}", git_ref(sha, "commit")?),
        ReviewTarget::Custom { instructions } => instructions.trim().to_owned(),
    })
}

/// The review command line for a provider whose review is its own command
/// (Claude Code, OpenCode); `None` for the others.
pub(crate) fn command_line_review(
    harness: HarnessId,
    target: &ReviewTarget,
) -> Option<Result<String, EngineError>> {
    match harness {
        HarnessId::ClaudeCode => Some(review_prompt(target)),
        HarnessId::Opencode => Some(opencode_review_prompt(target)),
        _ => None,
    }
}

/// The chat's current Claude binding on this device for one native session.
pub(crate) fn claude_binding(
    view: Option<TaskIdentityView>,
    chat_id: &str,
    device_id: &str,
    session_id: Option<&str>,
) -> Option<ExecutionBinding> {
    let view = view?;
    let binding = view.current_binding?;
    let valid = !view.task.tombstoned
        && view.workspace.environment_id == device_id
        && binding.task_id == chat_id
        && binding.environment_id == device_id
        && binding.provider_id == HarnessId::ClaudeCode
        && !binding.provider_task_id.is_empty()
        && session_id.is_none_or(|id| id == binding.provider_task_id);
    valid.then_some(binding)
}

fn readback(binding: &ExecutionBinding, goal: Option<ThreadGoal>) -> GoalReadback {
    GoalReadback {
        thread_id: binding.provider_task_id.clone(),
        binding_generation: binding.generation,
        goal,
        checked_at_ms: now_ms(),
        // Headless Claude never continues a goal on its own between turns;
        // the next message resumes evaluation.
        continuation_ready: false,
    }
}

/// A goal change observed in a Claude run. "Goal achieved" goes to the turn
/// whose own loop ended with Claude's met verdict, once per goal.
pub(crate) fn persist_run_goal(
    host: &DocHost,
    chat_id: &str,
    entry_id: &str,
    binding: &ExecutionBinding,
    goal: Option<&ThreadGoal>,
    in_turn: bool,
) {
    let achieved = goal.filter(|goal| goal.status == GoalStatus::Complete && in_turn);
    let previously_complete = achieved.is_some()
        && host
            .context_control(chat_id)
            .ok()
            .and_then(|state| state.goal)
            .and_then(|readback| readback.goal)
            .is_some_and(|previous| {
                achieved.is_some_and(|goal| {
                    previous.created_at == goal.created_at
                        && previous.status == GoalStatus::Complete
                })
            });
    if let Err(error) =
        host.persist_goal_readback(chat_id, &new_id(), &readback(binding, goal.cloned()))
    {
        tracing::warn!(chat = %chat_id, %error, "Claude goal snapshot persistence failed");
    }
    if let Some(goal) = achieved
        && !previously_complete
        && let Err(error) = host.persist_goal_mark(chat_id, &new_id(), |marks| {
            marks.mark_achieved(entry_id, goal.time_used_seconds)
        })
    {
        tracing::warn!(chat = %chat_id, %error, "Claude goal achieved mark persistence failed");
    }
}

/// Claude's own schedules for this binding (replace semantics, bounded).
pub(crate) fn persist_schedules(host: &DocHost, chat_id: &str, schedules: &[NativeSchedule]) {
    let mut schedules = schedules.to_vec();
    let excess = schedules.len().saturating_sub(NativeSchedule::LIMIT);
    schedules.drain(..excess);
    if let Err(error) = host.update_context_control(chat_id, &new_id(), move |state| {
        state.native_schedules = schedules;
    }) {
        tracing::warn!(chat = %chat_id, %error, "Claude schedule persistence failed");
    }
}

/// The outcome of a goal control on a Claude task.
pub(crate) enum ClaudeGoalControl {
    Readback(GoalReadback),
    Unsupported(String),
    Busy(String),
}

/// Goal controls Claude actually has: read its record, and its own
/// `/goal clear` between turns. A parked warm runtime is retired first so
/// exactly one Claude process writes the session.
pub(crate) async fn goal_operation(
    sessions: &SessionsEngine,
    harness: Arc<dyn Harness>,
    binding: Option<ExecutionBinding>,
    task_id: &str,
    cwd: &str,
    operation: &GoalOperation,
) -> Result<ClaudeGoalControl, EngineError> {
    let action = match operation {
        GoalOperation::Get => NativeGoalAction::Read,
        GoalOperation::Clear => NativeGoalAction::Clear,
        GoalOperation::Set { .. } => {
            return Ok(ClaudeGoalControl::Unsupported(
                "Claude goals have no pause, resume or token budget, and a new condition starts work immediately. Send a new goal to replace this one, or clear it."
                    .into(),
            ));
        }
    };
    let Some(binding) = binding else {
        return match action {
            NativeGoalAction::Read => Ok(ClaudeGoalControl::Readback(GoalReadback {
                thread_id: String::new(),
                binding_generation: 0,
                goal: None,
                checked_at_ms: now_ms(),
                continuation_ready: false,
            })),
            NativeGoalAction::Clear => Ok(ClaudeGoalControl::Busy(
                "This chat has no Claude session yet, so there is no goal to clear.".into(),
            )),
        };
    };
    // Clear holds the dispatch gate with no runtime: one native writer.
    let _exclusive = if action == NativeGoalAction::Clear {
        match sessions.exclusive_idle_runtime(task_id).await? {
            Some(guard) => Some(guard),
            None => {
                return Ok(ClaudeGoalControl::Busy(
                    "Claude clears a goal between turns. Stop the current turn first, then clear the goal."
                        .into(),
                ));
            }
        }
    } else {
        None
    };
    let readback_goal = harness
        .native_goal_control(NativeGoalControl {
            session_id: binding.provider_task_id.clone(),
            cwd: cwd.to_owned(),
            action,
        })
        .await?
        .goal;
    Ok(ClaudeGoalControl::Readback(readback(
        &binding,
        readback_goal,
    )))
}
