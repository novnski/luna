//! Durable, acceptance-aware retry policy for failed turns.
//!
//! This is deliberately not a general scheduler. One small ledger per task
//! records the latest failed original request plus bounded history. Timers are
//! process-local; restart reloads the ledger and reconciles before arming them.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use serde::{Deserialize, Serialize};
use zeron_proto::{
    HarnessId, RecoveryAcceptance, RecoveryAction, RecoveryFailure, RecoveryStatus, RunRequest,
    TaskRecovery,
};

use crate::{EngineError, new_id};

pub const MAX_AUTO_RETRIES: u8 = 4;
pub const MAX_AUTO_WAIT_MS: i64 = 6 * 60 * 60 * 1_000;
const HISTORY_LIMIT: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryRecord {
    pub view: TaskRecovery,
    pub harness: HarnessId,
    pub request: RunRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryLedger {
    #[serde(default)]
    records: Vec<RecoveryRecord>,
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
    dir: PathBuf,
    gate: Mutex<()>,
}

impl RecoveryStore {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            gate: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.gate.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn path(&self, task_id: &str) -> PathBuf {
        let safe: String = task_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{safe}.json"))
    }

    fn read_locked(&self, task_id: &str) -> Result<RecoveryLedger, EngineError> {
        match std::fs::read(self.path(task_id)) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(RecoveryLedger::default())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn write_locked(&self, task_id: &str, ledger: &RecoveryLedger) -> Result<(), EngineError> {
        std::fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        let path = self.path(task_id);
        let temp = path.with_extension(format!("{}.tmp", new_id()));
        let bytes = serde_json::to_vec(ledger)?;
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)?;
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    pub fn current(&self, task_id: &str) -> Result<Option<RecoveryRecord>, EngineError> {
        let _guard = self.lock();
        Ok(self
            .read_locked(task_id)?
            .records
            .into_iter()
            .rev()
            .find(|record| record.view.status.active()))
    }

    pub fn latest(&self, task_id: &str) -> Result<Option<RecoveryRecord>, EngineError> {
        let _guard = self.lock();
        Ok(self.read_locked(task_id)?.records.into_iter().next_back())
    }

    pub fn put(&self, record: RecoveryRecord) -> Result<RecoveryRecord, EngineError> {
        let _guard = self.lock();
        let mut ledger = self.read_locked(&record.view.task_id)?;
        for previous in ledger
            .records
            .iter_mut()
            .filter(|previous| previous.view.status.active())
        {
            if previous.view.chain_id != record.view.chain_id {
                previous.view.status = RecoveryStatus::Superseded;
                previous.view.retry_at_ms = None;
                previous.view.automatic = false;
                previous.view.detail =
                    "Superseded by newer user activity or a newer failed original request.".into();
                previous.view.updated_at_ms = record.view.updated_at_ms;
            }
        }
        if let Some(existing) = ledger
            .records
            .iter_mut()
            .find(|existing| existing.view.chain_id == record.view.chain_id)
        {
            *existing = record.clone();
        } else {
            ledger.records.push(record.clone());
        }
        if ledger.records.len() > HISTORY_LIMIT {
            let remove = ledger.records.len() - HISTORY_LIMIT;
            ledger.records.drain(0..remove);
        }
        self.write_locked(&record.view.task_id, &ledger)?;
        Ok(record)
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
        let _guard = self.lock();
        let mut ledger = self.read_locked(task_id)?;
        let Some(record) = ledger
            .records
            .iter_mut()
            .find(|record| record.view.chain_id == chain_id)
        else {
            return Ok(None);
        };
        update(record);
        let result = record.clone();
        self.write_locked(task_id, &ledger)?;
        Ok(Some(result))
    }

    /// Atomically claim one active chain for a provider attempt. A newer user
    /// turn can supersede the chain before this claim; in that case no stale
    /// in-memory snapshot may resurrect it.
    pub fn start_attempt(
        &self,
        task_id: &str,
        chain_id: &str,
        attempt_count: u8,
        updated_at_ms: i64,
    ) -> Result<Option<RecoveryRecord>, EngineError> {
        let _guard = self.lock();
        let mut ledger = self.read_locked(task_id)?;
        let Some(record) = ledger
            .records
            .iter_mut()
            .find(|record| record.view.chain_id == chain_id && record.view.status.active())
        else {
            return Ok(None);
        };
        record.view.status = RecoveryStatus::Running;
        record.view.automatic = false;
        record.view.retry_at_ms = None;
        record.view.attempt_count = attempt_count;
        record.view.updated_at_ms = updated_at_ms;
        let result = record.clone();
        self.write_locked(task_id, &ledger)?;
        Ok(Some(result))
    }

    /// Latest durable chain per task, including terminal cancellation,
    /// supersession, exhaustion, and completion. Reloading must not erase the
    /// last known outcome even though only active scheduled chains get timers.
    pub fn latest_records(&self) -> Result<Vec<RecoveryRecord>, EngineError> {
        let _guard = self.lock();
        std::fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        let mut records = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(entry.path())?;
            let ledger: RecoveryLedger = serde_json::from_slice(&bytes)?;
            if let Some(record) = ledger.records.into_iter().next_back() {
                records.push(record);
            }
        }
        records.sort_by(|a, b| a.view.task_id.cmp(&b.view.task_id));
        Ok(records)
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
