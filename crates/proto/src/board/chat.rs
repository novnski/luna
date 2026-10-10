//! Canonical main/side chat identity and the chat-creation operations that
//! Board Start, agents and UI share.
//!
//! Roles are derived by the engine from durable lineage (workspace side-chat
//! rows, `ChildThread` metadata, `TaskLineage`), never from tab/layout state,
//! titles or model claims. A side chat has exactly one main parent and no
//! side children; it can create a fresh sibling under that main parent or a
//! distinct new main chat with a written brief, and nothing else.
use serde::{Deserialize, Serialize};

use super::{BoardActor, BoardExecutionProfile, OriginalContainer, ResolvedExecution};
use crate::HarnessId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardChatRole {
    /// An independent sidebar chat.
    Main,
    /// A parent-created chat displayed in its main parent's workbench
    /// (today's side chats and child chats, unified).
    Side,
}

impl BoardChatRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Side => "side",
        }
    }
}

/// What the new conversation starts with. "Fresh" means no inherited
/// transcript; the brief, references, project instructions and explicit
/// prompt still apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardContextMode {
    #[default]
    Fresh,
    /// Main callers only: a native fork where supported, else a visible
    /// context snapshot, from an optional message checkpoint.
    WithContext {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint_message_id: Option<String>,
    },
}

/// How the started conversation actually obtained its context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardContextOrigin {
    Fresh,
    NativeFork,
    ContextSnapshot,
    /// A new main chat created with an explicit written handoff brief.
    HandoffBrief,
}

/// Where a first working chat goes. Validated against the caller's canonical
/// role; an explicit conflicting destination for an existing worker returns
/// the canonical location instead of moving or duplicating it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardDestination {
    /// Resolve from the caller: Board UI → new main; a main chat → side chat
    /// under it; a side chat → fresh sibling under its main parent.
    #[default]
    Auto,
    /// A new independent main sidebar chat.
    NewMain,
    /// A side chat under this validated main chat.
    SideOf { main_task_id: String },
    /// From a side caller: a fresh sibling under the caller's main parent.
    SiblingOf { side_task_id: String },
}

/// What the starter wants back. Without a requested continuation the source
/// chat gets a receipt/activity entry and stays idle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardFollowUp {
    #[default]
    None,
    /// Deliver the outcome (success, failure, cancellation, intervention)
    /// once to the requesting chat.
    ReportBack,
    /// Deliver once and queue one continuation turn in the requester.
    ReportAndContinue,
}

/// Special purposes. Temporary authoring sessions are not chats at all and
/// therefore never appear here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChatPurpose {
    Ordinary,
    /// The primary working chat of a ticket.
    TicketWorker {
        work_item_id: String,
        key: String,
    },
    /// A temporary reviewer side chat (archived after success).
    Reviewer {
        work_item_id: String,
        key: String,
        review_run_id: String,
    },
}

/// Creation provenance, distinct from the parent. A side-created sibling
/// records the side creator here while its parent is the main chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatProvenance {
    pub task_id: String,
    pub relation: ChatProvenanceRelation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatProvenanceRelation {
    /// The chat that asked for this one to be created.
    CreatedBy,
    /// Native fork / context snapshot source.
    ForkedFrom,
    /// "Handed off from …" (new main chat with a brief; not a fork).
    HandedOffFrom,
}

/// The actions the engine permits for this chat right now. Tool schemas show
/// only applicable modes; engine checks enforce them for every caller.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChatPermittedActions {
    pub create_side_fresh: bool,
    pub create_side_with_context: bool,
    pub create_sibling_fresh: bool,
    pub create_main: bool,
    pub create_main_with_handoff: bool,
    pub convert_to_ticket: bool,
    pub start_ticket: bool,
    /// Always false for ticket workers; reparenting is not a feature.
    pub reparent: bool,
}

/// The engine's canonical view of one chat's identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalChatIdentity {
    pub task_id: String,
    pub role: BoardChatRole,
    /// The single main parent of a side chat (the top-level main ancestor for
    /// legacy nested records). `None` for main chats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_parent_id: Option<String>,
    /// The workbench that displays it: itself for a main chat, its main
    /// parent for a side chat.
    pub display_container_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<ChatProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ChatProvenance>,
    pub context_origin: BoardContextOrigin,
    pub purpose: ChatPurpose,
    /// Ticket workers: the frozen original container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_container: Option<OriginalContainer>,
    /// True for ticket workers: no role may reparent them.
    pub parent_locked: bool,
    /// The main parent row is archived, deleted or otherwise unavailable;
    /// navigation still opens the chat directly.
    #[serde(default)]
    pub parent_unavailable: bool,
    /// Legacy nested child/fork record awaiting (or exempt from) migration.
    #[serde(default)]
    pub legacy_nested: bool,
    pub permitted: ChatPermittedActions,
}

/// Bind a new chat to a ticket as its worker in the same operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardTicketBinding {
    pub work_item_id: String,
    pub expected_revision: u64,
}

/// `create_side_chat`: from a main chat (fresh or with context) or, from a
/// side chat, a fresh sibling under the same main parent. UI requests name
/// the chat the user is acting in; agent tools bind it from the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardCreateSideChatRequest {
    pub request_id: String,
    /// The calling chat (UI only; tool calls bind it and reject a mismatch).
    pub caller_task_id: String,
    #[serde(default)]
    pub context: BoardContextMode,
    #[serde(default)]
    pub title: Option<String>,
    /// Sent once only when `start` is true.
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub start: bool,
    #[serde(default)]
    pub ticket: Option<BoardTicketBinding>,
    #[serde(default)]
    pub execution: Option<BoardExecutionProfile>,
}

/// `create_main_chat`: a distinct new independent sidebar chat (Cmd+N
/// semantics). From a side chat it carries a written brief, never the
/// transcript; the caller stays exactly where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardCreateMainChatRequest {
    pub request_id: String,
    pub project_id: String,
    /// Optional "Handed off from …" source (UI: the chat acting; tools:
    /// bound from the run).
    #[serde(default)]
    pub from_task_id: Option<String>,
    /// Goal, completed work, files/artifacts, decisions, constraints, next
    /// step. Required when `from_task_id` is a side chat.
    #[serde(default)]
    pub handoff_brief: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// Create/save only (false) versus start working there (true).
    #[serde(default)]
    pub start: bool,
    #[serde(default)]
    pub ticket: Option<BoardTicketBinding>,
    #[serde(default)]
    pub execution: Option<BoardExecutionProfile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardChatLaunchState {
    /// Created and saved; nothing sent.
    Created,
    /// The first message is durably queued (not provider acceptance).
    Queued,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardChatCreated {
    pub request_id: String,
    pub task_id: String,
    pub role: BoardChatRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<ChatProvenance>,
    pub context_origin: BoardContextOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub launch: BoardChatLaunchState,
    pub settings: ResolvedExecution,
    pub navigation: BoardNavigationTarget,
    /// The ticket already had a worker: this is that chat in its original
    /// location (never moved, never sent a second first message).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reused: bool,
}

/// "Convert this chat to a ticket": create a ticket and link the same chat as
/// its worker in place. Never sends, forks, reparents or changes settings.
/// Temporary authoring sessions are not convertible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardConvertChatRequest {
    pub request_id: String,
    /// UI only; tool calls bind the caller.
    pub task_id: String,
    /// Default: the chat's current title.
    #[serde(default)]
    pub title: Option<String>,
    /// The agreed brief (prior messages are never rewritten).
    pub description: String,
    #[serde(default)]
    pub implementation: Option<BoardExecutionProfile>,
    #[serde(default)]
    pub review: Option<super::BoardReviewProfile>,
    #[serde(default)]
    pub completion: Option<super::BoardCompletionPolicy>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardConvertReceipt {
    pub mutation: super::BoardMutationReceipt,
    pub link: super::WorkItemTaskLink,
    pub identity: CanonicalChatIdentity,
    /// The exact task/binding/run association recorded at conversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<BoardRunAssociation>,
}

/// What was running in a chat when it became a ticket's worker. Recorded
/// once; current state is always re-derived (conversion never marks work
/// complete and never resends anything).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardRunAssociation {
    pub task_id: String,
    /// A provider run or turn was active at conversion.
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_message_id: Option<String>,
    /// LUNA execution binding (never a provider session id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_generation: Option<u32>,
    pub observed_at_ms: i64,
}

/// `board_link_thread`: link an existing chat as source/reference/attempt.
/// Never resends context merely by linking; Working links come only from
/// Start or conversion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardLinkChatRequest {
    pub request_id: String,
    pub work_item_id: String,
    pub expected_revision: u64,
    pub task_id: String,
    pub role: super::TaskLinkRole,
    /// False removes local provenance only (never deletes chat or issue).
    #[serde(default = "default_true")]
    pub linked: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardChatIdentityRequest {
    pub task_id: String,
}

/// Go to chat: resolve from a ticket's canonical relationships or a chat id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardNavigationRequest {
    #[serde(default)]
    pub work_item_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
}

/// Navigation reopens a view; it never relaunches, resends, reparents or
/// promotes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardNavigationTarget {
    /// Open this main chat.
    MainChat { task_id: String },
    /// Open the main parent's workbench and select the exact side chat.
    SideChat {
        main_task_id: String,
        side_task_id: String,
    },
    /// The worker is reachable but its original parent is unavailable: open
    /// the chat directly without rewriting the parent.
    Direct {
        task_id: String,
        original_parent_id: Option<String>,
    },
    /// The ticket has no working chat yet (offer Start).
    NoChat { work_item_id: String },
}

/// Which surface renamed: the ticket or its current primary working chat.
/// One engine operation updates both; parent/source titles never change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BoardTitleOrigin {
    Ticket { work_item_id: String },
    WorkingChat { task_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardTitleSyncReceipt {
    pub work_item_id: String,
    pub task_id: Option<String>,
    pub title: String,
    pub revision: u64,
}

/// Which creation path is asking the shared guard (legacy UI/agent creation
/// entry points call it so depth/context rules hold everywhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardChatCreationKind {
    SideFresh,
    SideWithContext,
    NativeFork,
    MessageFork,
    ChildThread,
    MainChat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardLegacyMigrationReport {
    pub examined: u64,
    pub migrated: u64,
    /// Active descendants deferred until verified quiescence.
    pub deferred: Vec<String>,
    pub unchanged: u64,
}

/// A compact summary used by chat headers, sidebar rows and side-chat tabs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTicketBadge {
    #[serde(default)]
    pub archived: bool,
    pub task_id: String,
    pub work_item_id: String,
    pub key: String,
    pub title: String,
    pub column: super::BoardColumn,
    pub phase: super::BoardPhase,
    /// `Working` → "Ticket LUN-42"; `Reviewer` → "Reviewer · LUN-42".
    pub role: super::TaskLinkRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessId>,
}

/// Ticket badges for many chats at once (sidebar rows, tab strips).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardChatBadgesRequest {
    pub task_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardChatBadges {
    pub badges: Vec<ChatTicketBadge>,
    /// Source discussions: creation/result receipts, not a working badge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<ChatTicketBadge>,
}

impl BoardActor {
    /// The actor for a chat acting through its own run.
    pub fn agent(task_id: &str, role: Option<BoardChatRole>) -> Self {
        Self::Agent {
            task_id: task_id.into(),
            role,
        }
    }
}
