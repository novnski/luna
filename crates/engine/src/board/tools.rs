//! Agent-tools slice (A4): board, review and chat-creation tools in the
//! engine-owned internal catalog (`cross_task_tools.rs`), delivered through
//! Codex `luna_mcp`, Claude `luna_tools` and OpenCode's private endpoint.
//! Caller authority is bound from the run (`BoardCaller::from_authority`),
//! never from tool arguments; tool argument structs carry no caller fields
//! and a model-supplied caller id that differs from the run is rejected.
//!
//! Contract: docs/design/project-board/pb0-contract.md §8. Tool names are
//! `luna_proto::BOARD_TOOL_NAMES`. Grant `board.read`/`board.write` to
//! ordinary main/side chat runs (and scoped authoring sessions) in the
//! runtime contract; advertise only verified delivery paths.
// Entry points the dispatcher does not call yet are wired by the owning slice.
#![allow(dead_code)]
use super::*;
use luna_proto::{BoardToolAudience, RuntimeToolDefinition};

impl EngineRpc {
    /// The board tool definitions attached for an audience (only modes that
    /// apply to the caller's canonical role; engine checks still enforce).
    pub(crate) fn board_tool_definitions(
        &self,
        audience: BoardToolAudience,
    ) -> BoardResult<Vec<RuntimeToolDefinition>> {
        let _ = audience;
        Err(BoardError::not_implemented("board.toolDefinitions"))
    }

    /// Execute one board tool call for a bound caller: parse the tool's own
    /// argument struct, translate to the matching board request (binding
    /// caller ids from the run) and call the same service function as the
    /// RPC dispatcher. Returns the structured receipt or `BoardError` JSON.
    pub(crate) async fn board_tool_call(
        &self,
        caller: &BoardCaller,
        tool: &str,
        arguments: Value,
    ) -> BoardResult<Value> {
        let _ = (caller, tool, arguments);
        Err(BoardError::not_implemented("board.toolCall"))
    }

    /// Diagnostics for `BoardToolCatalog`: the board tools actually attached
    /// to a chat's runs and the delivery path that verified them.
    pub(crate) fn board_tool_catalog(&self, task_id: &str) -> BoardResult<Value> {
        let _ = task_id;
        Err(BoardError::not_implemented("board.toolCatalog"))
    }
}
