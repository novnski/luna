//! Agent-facing board tool names and the compact canonical context injected
//! into runs at start, resume, handoff and compaction.
//!
//! Tools are delivered through the engine-owned internal catalog (Codex
//! `luna_mcp`, Claude `luna_tools`, OpenCode's private endpoint). A schema
//! existing is not delivery; each provider path must be verified.
use serde::{Deserialize, Serialize};

use super::{BoardChatRole, BoardColumn, BoardCompletionMode, ChatPermittedActions, TaskLinkRole};

pub const BOARD_TOOL_LIST: &str = "board_list";
pub const BOARD_TOOL_READ: &str = "board_read";
pub const BOARD_TOOL_EXECUTION_OPTIONS: &str = "board_execution_options";
pub const BOARD_TOOL_CREATE: &str = "board_create";
pub const BOARD_TOOL_UPDATE: &str = "board_update";
pub const BOARD_TOOL_MOVE: &str = "board_move";
pub const BOARD_TOOL_START: &str = "board_start";
pub const BOARD_TOOL_CONTROL: &str = "board_control";
pub const BOARD_TOOL_SET_STATUS: &str = "board_set_status";
pub const BOARD_TOOL_SEND_UPDATE: &str = "board_send_update";
pub const BOARD_TOOL_LINK_THREAD: &str = "board_link_thread";
pub const BOARD_TOOL_CONVERT_CHAT: &str = "board_convert_chat";
pub const BOARD_TOOL_REVIEW_START: &str = "review_start";
pub const BOARD_TOOL_REVIEW_STATUS: &str = "review_status";
pub const BOARD_TOOL_REVIEW_CANCEL: &str = "review_cancel";
pub const BOARD_TOOL_REVIEW_SUBMIT: &str = "review_submit";
pub const BOARD_TOOL_CREATE_SIDE_CHAT: &str = "create_side_chat";
pub const BOARD_TOOL_CREATE_MAIN_CHAT: &str = "create_main_chat";
pub const BOARD_TOOL_AUTHORING_CHECKPOINT: &str = "board_draft_checkpoint";
pub const BOARD_TOOL_ISSUE_SEARCH: &str = "board_issue_search";
pub const BOARD_TOOL_ISSUE_IMPORT: &str = "board_issue_import";
pub const BOARD_TOOL_SET_DEPENDENCY: &str = "board_set_dependency";

/// Every board tool name, in catalog order.
pub const BOARD_TOOL_NAMES: &[&str] = &[
    BOARD_TOOL_LIST,
    BOARD_TOOL_READ,
    BOARD_TOOL_EXECUTION_OPTIONS,
    BOARD_TOOL_CREATE,
    BOARD_TOOL_UPDATE,
    BOARD_TOOL_MOVE,
    BOARD_TOOL_START,
    BOARD_TOOL_CONTROL,
    BOARD_TOOL_SET_STATUS,
    BOARD_TOOL_SEND_UPDATE,
    BOARD_TOOL_LINK_THREAD,
    BOARD_TOOL_CONVERT_CHAT,
    BOARD_TOOL_REVIEW_START,
    BOARD_TOOL_REVIEW_STATUS,
    BOARD_TOOL_REVIEW_CANCEL,
    BOARD_TOOL_REVIEW_SUBMIT,
    BOARD_TOOL_CREATE_SIDE_CHAT,
    BOARD_TOOL_CREATE_MAIN_CHAT,
    BOARD_TOOL_AUTHORING_CHECKPOINT,
    BOARD_TOOL_ISSUE_SEARCH,
    BOARD_TOOL_ISSUE_IMPORT,
    BOARD_TOOL_SET_DEPENDENCY,
];

/// The caller purpose a tool catalog is built for. Authoring sessions get
/// their own scoped catalog and can never claim a main/side role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardToolAudience {
    MainChat,
    SideChat,
    TicketWorker,
    Reviewer,
    Authoring,
}

/// The ticket a run is working on or reviewing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardTicketContext {
    pub work_item_id: String,
    pub key: String,
    pub title: String,
    pub project_id: String,
    pub link_role: TaskLinkRole,
    pub brief_revision: u32,
    pub column: BoardColumn,
    pub execution_generation: u64,
    pub completion: BoardCompletionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_run_id: Option<String>,
    /// Requested outcome in one line (from the brief), untrusted text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_outcome: Option<String>,
}

/// Compact canonical context for a run. Refreshed after resume, handoff and
/// compaction; never inferred from titles, layout or transcript text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardRuntimeContext {
    pub audience: BoardToolAudience,
    /// `None` for temporary authoring sessions (not chats).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<BoardChatRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authoring_session_id: Option<String>,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<BoardTicketContext>,
    pub permitted: ChatPermittedActions,
    /// Ticket workers: always true (no reparenting capability exists).
    pub parent_locked: bool,
    /// Board tool names actually attached to this run.
    pub tools: Vec<String>,
}
