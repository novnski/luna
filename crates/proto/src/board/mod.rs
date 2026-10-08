//! Project board (PB-0..PB-3) wire records.
//!
//! A board **ticket** is a [`WorkItem`]: an app-owned description of an
//! outcome that exists before, during and after the chats that work on it.
//! It is unrelated to [`crate::TaskWorkItem`], which is a projection of a
//! chat's background execution (child chats, shells, services).
//!
//! The engine of the project's owning host is the only writer. Every record
//! here is stored in that host's SQLite profile store as JSON next to a few
//! indexed columns, so later milestones add optional fields without a schema
//! migration. All additions must be `#[serde(default)]` and never rename or
//! remove a field: receipts and payload hashes are compared byte-for-byte.
//!
//! Authority is never a request field. The engine binds the caller (the local
//! user, a provider run, a temporary authoring session or engine-internal
//! automation) from the transport or the active run; see
//! `docs/design/project-board/pb0-contract.md`.

use serde::{Deserialize, Serialize};

mod chat;
mod error;
mod events;
mod item;
mod ops;
mod records;
mod tools;

pub use chat::*;
pub use error::*;
pub use events::*;
pub use item::*;
pub use ops::*;
pub use records::*;
pub use tools::*;

/// Version stamped on every stored board record.
pub const BOARD_SCHEMA_VERSION: u32 = 1;
/// Ticket titles are short, single-line labels.
pub const BOARD_TITLE_MAX_CHARS: usize = 200;
/// The editable brief (Markdown). Acceptance criteria live in it as a section.
pub const BOARD_DESCRIPTION_MAX_BYTES: usize = 64 * 1024;
pub const BOARD_LABELS_MAX: usize = 20;
pub const BOARD_LABEL_MAX_CHARS: usize = 50;
pub const BOARD_CONTEXT_REFS_MAX: usize = 32;
/// Default and maximum page sizes for bounded list reads.
pub const BOARD_LIST_DEFAULT_LIMIT: u32 = 200;
pub const BOARD_LIST_MAX_LIMIT: u32 = 500;
pub const BOARD_EVENTS_MAX_LIMIT: u32 = 500;
/// Idempotency keys and client-chosen ids: `[A-Za-z0-9_.:-]{1,128}`.
pub const BOARD_REQUEST_ID_MAX_BYTES: usize = 128;
/// Automatic repair rounds after the initial review (confirmed product
/// default): at most three review passes per cycle.
pub const BOARD_MAX_REPAIR_ROUNDS: u8 = 2;
pub const BOARD_MAX_REVIEW_PASSES: u8 = BOARD_MAX_REPAIR_ROUNDS + 1;
/// Most tickets one explicit multi-ticket start may name.
pub const BOARD_BATCH_START_MAX: usize = 10;

/// Workflow column: the user's execution **intent**, never proof of runtime
/// state. Observed state is [`BoardPhase`]; blockers are the Needs-attention
/// overlay ([`BoardAttentionReason`]).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum BoardColumn {
    /// Held or stopped. Saving a ticket lands here and creates no chat.
    #[default]
    Backlog,
    /// Enabled to run: entering requests Start/Continue; the ticket stays here
    /// through queueing, work, checks, review and repairs.
    Ready,
    /// Current checks and configured review passed for the current result.
    Done,
}

impl BoardColumn {
    pub const ALL: [BoardColumn; 3] = [Self::Backlog, Self::Ready, Self::Done];

    /// Stable storage/order index (Backlog, Ready, Done).
    pub fn order(self) -> i64 {
        match self {
            Self::Backlog => 0,
            Self::Ready => 1,
            Self::Done => 2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Ready => "ready",
            Self::Done => "done",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Backlog => "Backlog",
            Self::Ready => "Ready",
            Self::Done => "Done",
        }
    }

    /// Column help shown in the board header.
    pub fn help(self) -> &'static str {
        match self {
            Self::Backlog => "Held or stopped",
            Self::Ready => "Runs automatically",
            Self::Done => "Checked and reviewed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|column| column.as_str() == value)
    }
}

/// Which projects a read covers. `AllProjects` aggregates this host's
/// projects without merging their ownership, keys or execution targets; every
/// returned item still names its own project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardScope {
    Project {
        project_id: String,
    },
    #[default]
    AllProjects,
}

impl BoardScope {
    pub fn project_id(&self) -> Option<&str> {
        match self {
            Self::Project { project_id } => Some(project_id),
            Self::AllProjects => None,
        }
    }
}

/// Who performed a mutation, recorded on events and receipts. Derived by the
/// engine from the actual caller; never accepted from a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardActor {
    /// The human, through the local UI (or a forwarded owner call).
    User { device_id: String },
    /// A provider run working in an ordinary main or side chat.
    Agent {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<BoardChatRole>,
    },
    /// A temporary reviewer side chat reporting for its review run.
    Reviewer {
        task_id: String,
        review_run_id: String,
    },
    /// A temporary Board-only authoring session (never a chat).
    Authoring { session_id: String },
    /// Engine-internal reconciliation, lifecycle or review automation.
    Engine { component: String },
}

impl BoardActor {
    /// The chat behind this actor, when there is one.
    pub fn task_id(&self) -> Option<&str> {
        match self {
            Self::Agent { task_id, .. } | Self::Reviewer { task_id, .. } => Some(task_id),
            _ => None,
        }
    }
}

/// Validate a client-chosen request id / idempotency key.
pub fn board_request_id_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= BOARD_REQUEST_ID_MAX_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-:.".contains(&b))
}

/// Parse a human ticket key (`LUN-42`) into prefix and number.
pub fn parse_board_key(key: &str) -> Option<(&str, u64)> {
    let (prefix, number) = key.rsplit_once('-')?;
    let valid_prefix = !prefix.is_empty()
        && prefix.len() <= 8
        && prefix
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_uppercase())
        && prefix
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let number: u64 = number.parse().ok()?;
    (valid_prefix && number > 0).then_some((prefix, number))
}
