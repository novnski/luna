//! Provider-native history operations beyond forks: transcript readback,
//! same-session truncation and native file checkpoints.
//!
//! These contracts carry provider-owned identities only. Each adapter decides
//! whether it supports an operation; the trait defaults are explicit
//! unsupported errors, never a substitute LUNA-side rewrite.

/// One provider session, resolved by the engine. `provider_home` is the
/// engine-resolved provider configuration root and is never accepted from UI
/// or remote callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryScope {
    pub session_id: String,
    pub cwd: String,
    pub provider_home: Option<String>,
}

/// The provider's own record of which turns its active history contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryTurns {
    /// Ordered turn-start identities on the active history chain. These are
    /// the same identities the adapter reports through `ProviderCheckpoint`.
    pub turn_starts: Vec<String>,
    /// A native compaction (or another summary boundary) hides earlier turns
    /// from the provider's live context, so a turn list cannot prove an exact
    /// mapping for earlier rows.
    pub compacted: bool,
}

/// Remove `target_checkpoint`'s turn and every later turn from the same
/// provider session. `last_seen_checkpoint` is the newest turn the caller has
/// reviewed; the provider refuses when it holds a later turn the caller has
/// not seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRewindRequest {
    pub scope: NativeHistoryScope,
    pub target_checkpoint: String,
    pub last_seen_checkpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeRewindOutcome {
    /// The provider acknowledged and persisted the truncation.
    Rewound,
    /// The provider refused before any durable change. `reason` is the
    /// provider's own machine-readable reason.
    Refused { reason: String, message: String },
}

/// Reported once the mutating request has been written to the provider. An
/// error after this point leaves the outcome unknown; the caller reconciles
/// by readback and must never resend the mutation blindly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeRewindProgress {
    RequestWritten,
}

pub type NativeRewindProgressReporter = dyn Fn(NativeRewindProgress) + Send + Sync;

/// Restore (or, with `dry_run`, preview restoring) files the provider
/// checkpointed at `checkpoint`'s turn start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFileRewindRequest {
    pub scope: NativeHistoryScope,
    pub checkpoint: String,
    pub dry_run: bool,
}

/// The provider's own file-rewind result. `can_rewind == false` always comes
/// with the provider's reason in `error`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NativeFileRewindResult {
    pub can_rewind: bool,
    pub error: Option<String>,
    pub files_changed: Vec<String>,
    pub insertions: u64,
    pub deletions: u64,
    pub skipped_links: Option<u64>,
}
