//! Acceptance-aware turn recovery shared by the engine and desktop.

use serde::{Deserialize, Serialize};

/// What LUNA can prove about delivery of the original user request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryAcceptance {
    RejectedBeforeAcceptance,
    Accepted,
    Unknown,
}

/// Typed failure classes used by the bounded retry policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RecoveryFailure {
    Overloaded,
    SubscriptionWindow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset_at_ms: Option<i64>,
    },
    AuthenticationExpired,
    CreditsOrSpendLimit,
    InvalidConfiguration,
    UnsupportedModel,
    Interrupted,
    Transport,
    Other,
}

/// The only safe provider operation available for the failed original request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryAction {
    ResendOriginal,
    ContinueSession,
    ObserveOnly,
    SignInThenContinue,
    FixConfiguration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryStatus {
    Scheduled,
    Running,
    CheckingDelivery,
    RecoveryNeeded,
    WaitingForSignIn,
    Held,
    Cancelled,
    Superseded,
    Exhausted,
    Completed,
}

impl RecoveryStatus {
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Scheduled
                | Self::Running
                | Self::CheckingDelivery
                | Self::RecoveryNeeded
                | Self::WaitingForSignIn
                | Self::Held
        )
    }
}

/// Redacted, user-visible recovery state. The frozen prompt, attachments, and
/// execution choices remain engine-owned and are never copied into this view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecovery {
    pub chain_id: String,
    pub task_id: String,
    pub original_message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_command_id: Option<String>,
    pub acceptance: RecoveryAcceptance,
    pub failure: RecoveryFailure,
    pub action: RecoveryAction,
    pub status: RecoveryStatus,
    /// Number of recovery attempts already dispatched. The original run is not
    /// counted; automatic attempts never exceed `max_attempts`.
    pub attempt_count: u8,
    pub max_attempts: u8,
    pub automatic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<i64>,
    pub automatic_deadline_ms: i64,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_generation: Option<u32>,
    pub detail: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
