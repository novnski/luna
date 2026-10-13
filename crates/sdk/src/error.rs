//! Typed SDK errors. Domain refusals are receipts, never errors: an `Err`
//! means the call could not be completed or verified.

use std::time::Duration;

use luna_rpc::RpcError;

#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    /// The transport failed before a reply. A read is safe to retry; for a
    /// command that may have been admitted, see [`SdkError::UnknownOutcome`].
    #[error("{0}")]
    Offline(RpcError),
    /// No reply within the client's deadline.
    #[error("{operation} timed out after {}ms", .after.as_millis())]
    Timeout {
        operation: &'static str,
        after: Duration,
    },
    /// The engine does not serve this operation (an older or newer engine).
    #[error("unknown method: {method}")]
    Unsupported {
        operation: &'static str,
        method: String,
    },
    /// The engine and this SDK speak different contract versions.
    #[error("{detail}")]
    Incompatible { detail: String },
    /// The engine refused the request's shape (`bad params`).
    #[error("bad params: {message}")]
    InvalidRequest {
        operation: &'static str,
        message: String,
    },
    /// The reply does not match the operation's published schema, or it
    /// answers for another task or command. Never decoded as success.
    #[error("{operation} returned an invalid response: {detail}")]
    InvalidResponse {
        operation: &'static str,
        detail: String,
    },
    /// The engine's handler failed.
    #[error("{0}")]
    Engine(String),
    /// A command may or may not have been admitted, and its receipt could not
    /// be read. Read it again by `command_id`; never resend under a new id.
    #[error("outcome of command {command_id} is unknown: {source}")]
    UnknownOutcome {
        command_id: String,
        source: Box<SdkError>,
    },
}

impl SdkError {
    /// Every [`SdkError::kind`], shared with the TypeScript SDK.
    pub const KINDS: &'static [&'static str] = &[
        "offline",
        "timeout",
        "unsupported",
        "incompatible",
        "invalidRequest",
        "invalidResponse",
        "engine",
        "unknownOutcome",
    ];

    /// Stable, language-neutral error kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Offline(_) => "offline",
            Self::Timeout { .. } => "timeout",
            Self::Unsupported { .. } => "unsupported",
            Self::Incompatible { .. } => "incompatible",
            Self::InvalidRequest { .. } => "invalidRequest",
            Self::InvalidResponse { .. } => "invalidResponse",
            Self::Engine(_) => "engine",
            Self::UnknownOutcome { .. } => "unknownOutcome",
        }
    }

    /// Classify a wire error for `operation`.
    pub(crate) fn from_rpc(operation: &'static str, error: RpcError) -> Self {
        match error {
            RpcError::Transport(_) | RpcError::Closed => Self::Offline(error),
            RpcError::UnknownMethod(method) => Self::Unsupported { operation, method },
            RpcError::BadParams(message) => Self::InvalidRequest { operation, message },
            RpcError::Failed(message) => Self::Engine(message),
        }
    }

    pub(crate) fn invalid(operation: &'static str, detail: impl Into<String>) -> Self {
        Self::InvalidResponse {
            operation,
            detail: detail.into(),
        }
    }

    /// The equivalent transport error, for callers that still speak
    /// [`RpcError`] (the desktop app). An unknown outcome becomes the error
    /// that left it unknown.
    pub fn into_rpc(self) -> RpcError {
        match self {
            Self::Offline(error) => error,
            Self::Timeout { .. } => RpcError::Transport(self.to_string()),
            Self::Unsupported { method, .. } => RpcError::UnknownMethod(method),
            Self::InvalidRequest { message, .. } => RpcError::BadParams(message),
            Self::InvalidResponse { detail, .. } => RpcError::BadParams(detail),
            Self::Incompatible { detail } => RpcError::Failed(detail),
            Self::Engine(message) => RpcError::Failed(message),
            Self::UnknownOutcome { source, .. } => source.into_rpc(),
        }
    }
}
