//! Durable, acceptance-aware retry policy for failed turns.
//!
//! This is deliberately not a general scheduler. One local SQLite row per
//! task records the frozen original request and exact CAS fence. Timers are
//! process-local; restart reloads the row and reconciles before arming them.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zeron_proto::{
    HarnessId, RecoveryAcceptance, RecoveryAction, RecoveryFailure, RecoveryStatus, RunRequest,
    TaskRecovery,
};
use zeron_sync::{DocsStore, StoredTaskRecovery, TaskRecoveryExpectation, TaskRecoveryTransition};

use crate::EngineError;

pub const MAX_AUTO_RETRIES: u8 = 4;
pub const MAX_AUTO_WAIT_MS: i64 = 6 * 60 * 60 * 1_000;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryRecord {
    pub view: TaskRecovery,
    pub harness: HarnessId,
    /// Immutable original request. Continuation prompts and cleared
    /// attachments never replace this value.
    pub request: RunRequest,
    pub original_acceptance: RecoveryAcceptance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_acceptance: Option<RecoveryAcceptance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RecoveryChange {
    pub expected: RecoveryRecord,
    pub next: RecoveryRecord,
}

impl RecoveryChange {
    pub fn storage_transition(&self) -> Result<TaskRecoveryTransition, EngineError> {
        Ok(TaskRecoveryTransition {
            expected: Some(RecoveryStore::expectation(&self.expected)?),
            next: RecoveryStore::stored(&self.next)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryDecision {
    pub action: RecoveryAction,
    pub status: RecoveryStatus,
    pub automatic: bool,
    pub retry_at_ms: Option<i64>,
    pub detail: String,
}

/// Stable policy calculation. `jitter_seed` is caller-provided so tests and
/// the native fixture can control exact boundaries without timer races.
#[allow(clippy::too_many_arguments)]
pub fn recovery_decision(
    acceptance: RecoveryAcceptance,
    failure: &RecoveryFailure,
    now_ms: i64,
    created_at_ms: i64,
    attempts: u8,
    auto_enabled: bool,
    safe_continuation: bool,
    jitter_seed: u64,
) -> RecoveryDecision {
    let action = match acceptance {
        RecoveryAcceptance::RejectedBeforeAcceptance => RecoveryAction::ResendOriginal,
        RecoveryAcceptance::Accepted if safe_continuation => RecoveryAction::ContinueSession,
        RecoveryAcceptance::Accepted => RecoveryAction::ObserveOnly,
        RecoveryAcceptance::Unknown => RecoveryAction::ObserveOnly,
    };
    if acceptance == RecoveryAcceptance::Unknown {
        return RecoveryDecision {
            action,
            status: RecoveryStatus::CheckingDelivery,
            automatic: false,
            retry_at_ms: None,
            detail: "Checking delivery. Provider acceptance is unknown, so LUNA will not replay the request."
                .into(),
        };
    }
    if acceptance == RecoveryAcceptance::Accepted && !safe_continuation {
        return RecoveryDecision {
            action,
            status: RecoveryStatus::RecoveryNeeded,
            automatic: false,
            retry_at_ms: None,
            detail: "Recovery needed. This adapter cannot prove a safe same-session continuation; completed output and effects were preserved."
                .into(),
        };
    }

    match failure {
        RecoveryFailure::AuthenticationExpired => RecoveryDecision {
            action: RecoveryAction::SignInThenContinue,
            status: RecoveryStatus::WaitingForSignIn,
            automatic: false,
            retry_at_ms: None,
            detail: "Authentication expired. Sign in on the owning environment, refresh readiness, then continue safely."
                .into(),
        },
        RecoveryFailure::CreditsOrSpendLimit => RecoveryDecision {
            action,
            status: RecoveryStatus::RecoveryNeeded,
            automatic: false,
            retry_at_ms: None,
            detail: "Credits or the configured spend limit must be resolved before continuing. Automatic retry is disabled for this failure."
                .into(),
        },
        RecoveryFailure::InvalidConfiguration => RecoveryDecision {
            action: RecoveryAction::FixConfiguration,
            status: RecoveryStatus::RecoveryNeeded,
            automatic: false,
            retry_at_ms: None,
            detail: "The task configuration is invalid. Correct it before continuing; LUNA will not loop on the same configuration."
                .into(),
        },
        RecoveryFailure::UnsupportedModel => RecoveryDecision {
            action: RecoveryAction::FixConfiguration,
            status: RecoveryStatus::RecoveryNeeded,
            automatic: false,
            retry_at_ms: None,
            detail: "The selected model is unsupported or unavailable. Choose a supported model before continuing."
                .into(),
        },
        RecoveryFailure::Overloaded => schedule_bounded(
            action,
            now_ms,
            created_at_ms,
            attempts,
            auto_enabled,
            overload_delay_ms(attempts, jitter_seed),
            "Provider overloaded",
        ),
        RecoveryFailure::SubscriptionWindow { reset_at_ms } => {
            let Some(reset_at_ms) = reset_at_ms else {
                return RecoveryDecision {
                    action,
                    status: RecoveryStatus::RecoveryNeeded,
                    automatic: false,
                    retry_at_ms: None,
                    detail: "The subscription window is exhausted, but the provider did not supply a reset time. Continue only after checking the provider account."
                        .into(),
                };
            };
            let buffer = 30_000 + (jitter_seed % 30_001) as i64;
            schedule_at(
                action,
                now_ms,
                created_at_ms,
                attempts,
                auto_enabled,
                reset_at_ms.saturating_add(buffer),
                "Subscription window exhausted",
            )
        }
        RecoveryFailure::Interrupted => schedule_bounded(
            action,
            now_ms,
            created_at_ms,
            attempts,
            auto_enabled,
            1_000 + (jitter_seed % 1_001) as i64,
            "Run interrupted",
        ),
        RecoveryFailure::Transport | RecoveryFailure::Other => RecoveryDecision {
            action,
            status: RecoveryStatus::RecoveryNeeded,
            automatic: false,
            retry_at_ms: None,
            detail: "Recovery needed. Review the preserved partial output and current provider state before continuing."
                .into(),
        },
    }
}

fn schedule_bounded(
    action: RecoveryAction,
    now_ms: i64,
    created_at_ms: i64,
    attempts: u8,
    enabled: bool,
    delay_ms: i64,
    label: &str,
) -> RecoveryDecision {
    schedule_at(
        action,
        now_ms,
        created_at_ms,
        attempts,
        enabled,
        now_ms.saturating_add(delay_ms),
        label,
    )
}

#[allow(clippy::too_many_arguments)]
fn schedule_at(
    action: RecoveryAction,
    now_ms: i64,
    created_at_ms: i64,
    attempts: u8,
    enabled: bool,
    retry_at_ms: i64,
    label: &str,
) -> RecoveryDecision {
    if attempts >= MAX_AUTO_RETRIES {
        return RecoveryDecision {
            action,
            status: RecoveryStatus::Exhausted,
            automatic: false,
            retry_at_ms: None,
            detail: format!("{label}. The four-attempt automatic retry limit was reached."),
        };
    }
    if !enabled {
        return RecoveryDecision {
            action,
            status: RecoveryStatus::RecoveryNeeded,
            automatic: false,
            retry_at_ms: None,
            detail: format!("{label}. Automatic retries are disabled in Settings."),
        };
    }
    let deadline = created_at_ms.saturating_add(MAX_AUTO_WAIT_MS);
    if retry_at_ms > deadline || retry_at_ms.saturating_sub(now_ms) > MAX_AUTO_WAIT_MS {
        return RecoveryDecision {
            action,
            status: RecoveryStatus::RecoveryNeeded,
            automatic: false,
            retry_at_ms: None,
            detail: format!(
                "{label}. The next safe retry falls outside the six-hour automatic window."
            ),
        };
    }
    RecoveryDecision {
        action,
        status: RecoveryStatus::Scheduled,
        automatic: true,
        retry_at_ms: Some(retry_at_ms.max(now_ms)),
        detail: format!("{label}. A bounded retry is scheduled and can be sent now or cancelled."),
    }
}

fn overload_delay_ms(attempts: u8, jitter_seed: u64) -> i64 {
    let base = 5_000_i64.saturating_mul(1_i64 << attempts.min(6));
    let jitter_cap = (base / 4).max(1);
    base.saturating_add((jitter_seed % jitter_cap as u64) as i64)
}

pub fn classify_failure(message: &str) -> RecoveryFailure {
    let lower = message.to_ascii_lowercase();
    if lower.contains("subscription") && (lower.contains("reset") || lower.contains("window")) {
        return RecoveryFailure::SubscriptionWindow {
            reset_at_ms: explicit_reset_at_ms(&lower),
        };
    }
    if lower.contains("overload")
        || lower.contains("too many requests")
        || lower.contains("temporarily unavailable")
        || lower.contains("http 429")
    {
        return RecoveryFailure::Overloaded;
    }
    if lower.contains("authentication expired")
        || lower.contains("reauthentication required")
        || lower.contains("unauthorized")
        || lower.contains("invalid_grant")
    {
        return RecoveryFailure::AuthenticationExpired;
    }
    if lower.contains("credit") || lower.contains("spend limit") || lower.contains("billing limit")
    {
        return RecoveryFailure::CreditsOrSpendLimit;
    }
    if lower.contains("unsupported model")
        || lower.contains("model is not supported")
        || lower.contains("model not found")
    {
        return RecoveryFailure::UnsupportedModel;
    }
    if lower.contains("invalid configuration")
        || lower.contains("invalid config")
        || lower.contains("missing configuration")
    {
        return RecoveryFailure::InvalidConfiguration;
    }
    if lower.contains("connection") || lower.contains("transport") || lower.contains("timeout") {
        return RecoveryFailure::Transport;
    }
    RecoveryFailure::Other
}

/// Synthetic adapters may include an explicit `reset_at_ms=<epoch>` marker.
/// Ordinary provider prose without that typed value remains unknown.
fn explicit_reset_at_ms(message: &str) -> Option<i64> {
    let tail = message.split("reset_at_ms=").nth(1)?;
    tail.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

pub(crate) struct RecoveryStore {
    store: Arc<DocsStore>,
}

impl RecoveryStore {
    pub fn new(store: Arc<DocsStore>) -> Self {
        Self { store }
    }

    fn status(status: RecoveryStatus) -> Result<String, EngineError> {
        Ok(serde_json::to_value(status)?
            .as_str()
            .ok_or_else(|| EngineError::Other("recovery status did not serialize as text".into()))?
            .to_string())
    }

    fn stored(record: &RecoveryRecord) -> Result<StoredTaskRecovery, EngineError> {
        Ok(StoredTaskRecovery {
            task_id: record.view.task_id.clone(),
            chain_id: record.view.chain_id.clone(),
            attempt_count: record.view.attempt_count,
            status: Self::status(record.view.status)?,
            record_json: serde_json::to_vec(record)?,
            updated_at: record.view.updated_at_ms,
        })
    }

    fn expectation(record: &RecoveryRecord) -> Result<TaskRecoveryExpectation, EngineError> {
        Ok(TaskRecoveryExpectation {
            chain_id: record.view.chain_id.clone(),
            attempt_count: record.view.attempt_count,
            status: Self::status(record.view.status)?,
        })
    }

    fn decode(stored: StoredTaskRecovery) -> Result<RecoveryRecord, EngineError> {
        let record: RecoveryRecord = serde_json::from_slice(&stored.record_json)?;
        if record.view.task_id != stored.task_id
            || record.view.chain_id != stored.chain_id
            || record.view.attempt_count != stored.attempt_count
            || Self::status(record.view.status)? != stored.status
        {
            return Err(EngineError::Other(
                "recovery scalar fence does not match its private record".into(),
            ));
        }
        Ok(record)
    }

    pub fn current(&self, task_id: &str) -> Result<Option<RecoveryRecord>, EngineError> {
        Ok(self
            .latest(task_id)?
            .filter(|record| record.view.status.active()))
    }

    pub fn latest(&self, task_id: &str) -> Result<Option<RecoveryRecord>, EngineError> {
        self.store
            .task_recovery(task_id)?
            .map(Self::decode)
            .transpose()
    }

    pub fn put(&self, record: RecoveryRecord) -> Result<RecoveryRecord, EngineError> {
        self.store.put_task_recovery(&Self::stored(&record)?)?;
        Ok(record)
    }

    pub fn prepare_change(&self, expected: RecoveryRecord, next: RecoveryRecord) -> RecoveryChange {
        RecoveryChange { expected, next }
    }

    pub fn apply_change(&self, change: &RecoveryChange) -> Result<bool, EngineError> {
        Ok(self
            .store
            .transition_task_recovery(&change.storage_transition()?)?)
    }

    pub fn update<F>(
        &self,
        task_id: &str,
        chain_id: &str,
        update: F,
    ) -> Result<Option<RecoveryRecord>, EngineError>
    where
        F: FnOnce(&mut RecoveryRecord),
    {
        let Some(current) = self.latest(task_id)? else {
            return Ok(None);
        };
        if current.view.chain_id != chain_id {
            return Ok(None);
        }
        let mut next = current.clone();
        update(&mut next);
        let change = self.prepare_change(current, next.clone());
        Ok(self.apply_change(&change)?.then_some(next))
    }

    /// Latest durable chain per task, including terminal cancellation,
    /// supersession, exhaustion, and completion. Reloading must not erase the
    /// last known outcome even though only active scheduled chains get timers.
    pub fn latest_records(&self) -> Result<Vec<RecoveryRecord>, EngineError> {
        self.store
            .task_recoveries()?
            .into_iter()
            .map(Self::decode)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_matrix_never_replays_unknown_or_action_required_failures() {
        let now = 1_000_000;
        let cases = [
            (
                RecoveryAcceptance::Unknown,
                RecoveryFailure::Overloaded,
                RecoveryStatus::CheckingDelivery,
            ),
            (
                RecoveryAcceptance::RejectedBeforeAcceptance,
                RecoveryFailure::AuthenticationExpired,
                RecoveryStatus::WaitingForSignIn,
            ),
            (
                RecoveryAcceptance::RejectedBeforeAcceptance,
                RecoveryFailure::CreditsOrSpendLimit,
                RecoveryStatus::RecoveryNeeded,
            ),
            (
                RecoveryAcceptance::RejectedBeforeAcceptance,
                RecoveryFailure::UnsupportedModel,
                RecoveryStatus::RecoveryNeeded,
            ),
        ];
        for (acceptance, failure, status) in cases {
            let decision = recovery_decision(acceptance, &failure, now, now, 0, true, true, 7);
            assert_eq!(decision.status, status);
            assert!(!decision.automatic);
            assert!(decision.retry_at_ms.is_none());
        }
    }

    #[test]
    fn overload_backoff_is_exponential_jittered_and_capped_at_four_attempts() {
        let now = 1_000_000;
        let delays: Vec<i64> = (0..4)
            .map(|attempt| {
                recovery_decision(
                    RecoveryAcceptance::RejectedBeforeAcceptance,
                    &RecoveryFailure::Overloaded,
                    now,
                    now,
                    attempt,
                    true,
                    true,
                    123,
                )
                .retry_at_ms
                .unwrap()
                    - now
            })
            .collect();
        assert!(delays.windows(2).all(|pair| pair[1] > pair[0]));
        assert_eq!(
            recovery_decision(
                RecoveryAcceptance::RejectedBeforeAcceptance,
                &RecoveryFailure::Overloaded,
                now,
                now,
                MAX_AUTO_RETRIES,
                true,
                true,
                123,
            )
            .status,
            RecoveryStatus::Exhausted
        );
    }

    #[test]
    fn subscription_reset_must_be_known_and_inside_initial_six_hours() {
        let now = 1_000_000;
        let unknown = recovery_decision(
            RecoveryAcceptance::RejectedBeforeAcceptance,
            &RecoveryFailure::SubscriptionWindow { reset_at_ms: None },
            now,
            now,
            0,
            true,
            true,
            0,
        );
        assert_eq!(unknown.status, RecoveryStatus::RecoveryNeeded);
        let too_late = recovery_decision(
            RecoveryAcceptance::RejectedBeforeAcceptance,
            &RecoveryFailure::SubscriptionWindow {
                reset_at_ms: Some(now + MAX_AUTO_WAIT_MS + 1),
            },
            now,
            now,
            0,
            true,
            true,
            0,
        );
        assert_eq!(too_late.status, RecoveryStatus::RecoveryNeeded);

        let disabled = recovery_decision(
            RecoveryAcceptance::RejectedBeforeAcceptance,
            &RecoveryFailure::Overloaded,
            now,
            now,
            0,
            false,
            true,
            0,
        );
        assert_eq!(disabled.status, RecoveryStatus::RecoveryNeeded);
        assert!(!disabled.automatic);
        assert!(disabled.retry_at_ms.is_none());
    }
}
