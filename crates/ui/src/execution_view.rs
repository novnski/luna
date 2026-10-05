//! Wording and predicates for an engine's [`TaskExecutionSummary`], shared by
//! the composer chips, the queue's Steer action and Settings' Execution card.
//! Everything here reads the owning engine's observation; nothing derives
//! provider behavior locally, and an absent summary claims nothing.

use luna_proto::{
    ExecutionPhase, ExecutionSettingField, ExecutionSteerDecision, ExecutionSteerKind,
    ExecutionTransition, ExecutionTransitionKind, PermissionMode, TaskExecutionSummary,
};

/// A turn is starting or running on the observed runtime: only then do the
/// chips show `running` instead of the requested settings.
pub(crate) fn turn_active(summary: &TaskExecutionSummary) -> bool {
    matches!(
        summary.phase,
        ExecutionPhase::Starting | ExecutionPhase::Running
    ) && summary.running.is_some()
}

/// Pending fields the model chip summarizes.
pub(crate) fn model_pending(summary: &TaskExecutionSummary) -> bool {
    summary.pending.iter().any(|field| {
        matches!(
            field,
            ExecutionSettingField::Provider
                | ExecutionSettingField::Account
                | ExecutionSettingField::Model
                | ExecutionSettingField::Reasoning
                | ExecutionSettingField::ModelOptions
        )
    })
}

pub(crate) fn permission_pending(summary: &TaskExecutionSummary) -> bool {
    summary.pending.contains(&ExecutionSettingField::Permission)
}

/// How the observed runtime's settings are named: in use by a turn, or held
/// by a parked warm runtime between turns.
pub(crate) fn running_heading(summary: &TaskExecutionSummary) -> &'static str {
    if turn_active(summary) {
        "Running"
    } else {
        "Loaded"
    }
}

pub(crate) fn phase_label(phase: ExecutionPhase) -> &'static str {
    match phase {
        ExecutionPhase::Idle => "Idle",
        ExecutionPhase::Starting => "Starting a turn",
        ExecutionPhase::Running => "Running a turn",
        ExecutionPhase::Warm => "Loaded, between turns",
        ExecutionPhase::Unavailable => "Unavailable",
    }
}

/// How pending changes take effect, in the UI's words, then the engine's
/// reason as its own line. A new binding never implies context transfer.
pub(crate) fn transition_lines(transition: &ExecutionTransition) -> Vec<String> {
    let after = transition.waits_for_turn_end;
    let headline = match transition.kind {
        ExecutionTransitionKind::Reuse if after => {
            "Applies in the current session after this turn."
        }
        ExecutionTransitionKind::Reuse => "Applies in the current session.",
        ExecutionTransitionKind::ApplyNextTurn => "Applies when the next turn starts.",
        ExecutionTransitionKind::RestartAndResume if after => {
            "After this turn, the session restarts and resumes with it."
        }
        ExecutionTransitionKind::RestartAndResume => "The session restarts and resumes with it.",
        ExecutionTransitionKind::NewBinding if after => {
            "After this turn, a new provider session starts. Earlier context is not carried over automatically."
        }
        ExecutionTransitionKind::NewBinding => {
            "A new provider session starts. Earlier context is not carried over automatically."
        }
        ExecutionTransitionKind::Unavailable => "This change can't be applied.",
    };
    let mut lines = vec![headline.to_string()];
    let reason = transition.reason.trim();
    if !reason.is_empty() {
        lines.push(reason.to_string());
    }
    lines
}

pub(crate) fn transition_unavailable(summary: &TaskExecutionSummary) -> bool {
    summary.transition.kind == ExecutionTransitionKind::Unavailable
}

/// The steering line for the Execution card.
pub(crate) fn steer_label(steer: &ExecutionSteerDecision) -> String {
    let base = match steer.kind {
        ExecutionSteerKind::Native => "In the running turn",
        ExecutionSteerKind::AfterTurn => "After this turn",
        ExecutionSteerKind::Unavailable => "Unavailable",
    };
    match steer
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        Some(reason) => format!("{base}. {}", sentence(reason)),
        None => base.to_string(),
    }
}

/// A queued Steer row's explanation when the engine does not admit native
/// steering now (the row then offers only Send now, which interrupts).
pub(crate) fn steer_row_note(steer: &ExecutionSteerDecision) -> Option<String> {
    let reason = steer
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty());
    let lead = match steer.kind {
        ExecutionSteerKind::Native => return None,
        ExecutionSteerKind::AfterTurn => "Steering applies only after this turn",
        ExecutionSteerKind::Unavailable => "Steer unavailable",
    };
    Some(match reason {
        Some(reason) => format!(
            "{lead}: {} Send now interrupts the active response.",
            sentence(reason)
        ),
        None => format!("{lead}. Send now interrupts the active response."),
    })
}

/// `text` ending as a sentence (engine reasons may omit the full stop).
pub(crate) fn sentence(text: &str) -> String {
    let text = text.trim();
    if text.ends_with(['.', '!', '?']) {
        text.to_string()
    } else {
        format!("{text}.")
    }
}

/// The access level's name, as the access chip and its menu say it.
pub(crate) fn permission_title(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Limited access",
        PermissionMode::Auto => "Approve for me",
        PermissionMode::Full => "Full access",
    }
}
