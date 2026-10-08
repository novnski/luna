//! Structured board errors. Over RPC they travel as the JSON text of
//! [`BoardError`] inside the ordinary failure string (see
//! [`BoardError::to_rpc_message`]); agent tools return the same JSON. Every
//! variant is actionable: it names the current state or allowed alternative.
use serde::{Deserialize, Serialize};

use super::{BoardColumn, BoardNavigationTarget, WorkItemView};
use crate::{HarnessId, ReasoningLevel};

/// Marker that prefixes an encoded board error in an RPC failure string.
pub const BOARD_ERROR_PREFIX: &str = "board-error:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "code",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardError {
    NotFound {
        work_item_id: String,
    },
    /// The caller's expected revision is stale for a field it changes. The
    /// current state is returned so the caller can adjust instead of
    /// blindly retrying.
    StaleRevision {
        work_item_id: String,
        expected: u64,
        current: Box<WorkItemView>,
    },
    /// The request id was already used for a different payload.
    IdempotencyConflict {
        request_id: String,
    },
    InvalidInput {
        field: String,
        reason: String,
    },
    ProjectNotFound {
        project_id: String,
    },
    /// The project is owned by another host; address that host.
    WrongHost {
        project_id: String,
        owner_host_id: String,
    },
    /// Creating from All projects must name a project; never guessed.
    AmbiguousProject,
    InsufficientScope {
        required: String,
    },
    NotAuthorized {
        reason: String,
    },
    UnavailableProvider {
        harness: HarnessId,
    },
    /// No silent fallback: the requested settings are kept and the
    /// supported choices are returned.
    UnavailableModel {
        harness: HarnessId,
        model: String,
        #[serde(default)]
        supported: Vec<String>,
    },
    UnsupportedEffort {
        model: String,
        effort: ReasoningLevel,
        #[serde(default)]
        supported: Vec<ReasoningLevel>,
    },
    /// The caller's canonical role does not allow this operation/mode.
    InvalidRole {
        operation: String,
        reason: String,
        #[serde(default)]
        allowed: Vec<String>,
    },
    /// Destination conflicts with the canonical worker location; the
    /// existing location is returned rather than moving/duplicating.
    InvalidDestination {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        canonical: Option<BoardNavigationTarget>,
    },
    NotConvertible {
        reason: String,
    },
    /// Ticket workers always keep their original parent.
    ParentLocked {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        original_parent_id: Option<String>,
    },
    InvalidTransition {
        from: BoardColumn,
        to: BoardColumn,
        reason: String,
    },
    /// A concurrent operation holds this ticket (e.g. stop barrier); retry
    /// with the same request id.
    Busy {
        reason: String,
    },
    DependencyCycle {
        path: Vec<String>,
    },
    /// The external issue is already linked to a ticket in this project.
    DuplicateIssue {
        existing_work_item_id: String,
    },
    /// GitHub auth/rate-limit/unavailable, scoped to the selected account.
    SourceUnavailable {
        reason: String,
        #[serde(default)]
        retry_after_ms: Option<u64>,
    },
    /// An earlier attempt's outcome is unknown; reconcile with this key.
    OutcomeUnknown {
        request_id: String,
    },
    /// The capability is not delivered on this provider/host.
    Unavailable {
        reason: String,
    },
    Storage {
        message: String,
    },
    /// A contract operation whose slice has not landed yet.
    NotImplemented {
        operation: String,
    },
}

impl BoardError {
    pub fn not_implemented(operation: &str) -> Self {
        Self::NotImplemented {
            operation: operation.into(),
        }
    }

    pub fn invalid(field: &str, reason: impl Into<String>) -> Self {
        Self::InvalidInput {
            field: field.into(),
            reason: reason.into(),
        }
    }

    /// Stable machine code (`staleRevision`, …).
    pub fn code(&self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|value| value.get("code")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into())
    }

    /// Short human sentence for UI notices.
    pub fn message(&self) -> String {
        match self {
            Self::NotFound { .. } => "This ticket no longer exists.".into(),
            Self::StaleRevision { current, .. } => format!(
                "{} changed since you opened it. Review the latest version and try again.",
                current.item.key
            ),
            Self::IdempotencyConflict { .. } => {
                "This request id was already used for a different change.".into()
            }
            Self::InvalidInput { field, reason } => format!("{field}: {reason}"),
            Self::ProjectNotFound { .. } => "The project no longer exists.".into(),
            Self::WrongHost { .. } => {
                "This project belongs to another host. Open its board from that host.".into()
            }
            Self::AmbiguousProject => "Choose a project for this ticket.".into(),
            Self::InsufficientScope { required } => {
                format!("This chat lacks the {required} permission for that board action.")
            }
            Self::NotAuthorized { reason }
            | Self::NotConvertible { reason }
            | Self::Busy { reason }
            | Self::Unavailable { reason } => reason.clone(),
            Self::UnavailableProvider { harness } => {
                format!("{harness:?} is not available on this host.")
            }
            Self::UnavailableModel { model, .. } => {
                format!("{model} is not available for this account.")
            }
            Self::UnsupportedEffort { model, effort, .. } => {
                format!("{model} does not support {effort:?} reasoning.")
            }
            Self::InvalidRole { reason, .. } | Self::InvalidDestination { reason, .. } => {
                reason.clone()
            }
            Self::ParentLocked { .. } => {
                "Ticket chats always keep their original parent; open or continue it instead."
                    .into()
            }
            Self::InvalidTransition { reason, .. } => reason.clone(),
            Self::DependencyCycle { .. } => "That dependency would create a cycle.".into(),
            Self::DuplicateIssue { .. } => "That issue is already on this board.".into(),
            Self::SourceUnavailable { reason, .. } => reason.clone(),
            Self::OutcomeUnknown { .. } => {
                "The earlier attempt's outcome is unknown; reconciling before retrying.".into()
            }
            Self::Storage { message } => format!("Board storage failed: {message}"),
            Self::NotImplemented { operation } => {
                format!("{operation} is not available in this build yet.")
            }
        }
    }

    /// Encode for an RPC failure string.
    pub fn to_rpc_message(&self) -> String {
        format!(
            "{BOARD_ERROR_PREFIX}{}",
            serde_json::to_string(self).unwrap_or_else(|_| "{\"code\":\"storage\"}".into())
        )
    }

    /// Decode an RPC failure string produced by [`Self::to_rpc_message`].
    pub fn from_rpc_message(message: &str) -> Option<Self> {
        serde_json::from_str(message.strip_prefix(BOARD_ERROR_PREFIX)?).ok()
    }
}

impl std::fmt::Display for BoardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for BoardError {}

pub type BoardResult<T> = Result<T, BoardError>;
