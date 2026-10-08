//! Run-context slice (A4, with A3's identity): the compact canonical board
//! context injected at start, resume, handoff and compaction — chat id,
//! main/side role, main-parent id, permitted creation/context/transfer
//! actions, ticket id/role/brief revision/completion policy, parent lock.
//! Never inferred from titles, layout or transcript text.
//!
//! Contract: docs/design/project-board/pb0-contract.md §8.2. Add it to the
//! runtime contract as an optional, additive field.
// Entry points the dispatcher does not call yet are wired by the owning slice.
#![allow(dead_code)]
use super::*;
use luna_proto::BoardRuntimeContext;

impl EngineRpc {
    /// Canonical board context for an ordinary main/side chat run (`None`
    /// when the board has nothing to add).
    pub(crate) fn board_runtime_context(
        &self,
        task_id: &str,
    ) -> BoardResult<Option<BoardRuntimeContext>> {
        let _ = task_id;
        Err(BoardError::not_implemented("board.runtimeContext"))
    }

    /// Scoped context for a temporary authoring session (purpose/actor scope
    /// with project access and authoring tools; never a main/side role).
    pub(crate) fn board_authoring_runtime_context(
        &self,
        session_id: &str,
    ) -> BoardResult<BoardRuntimeContext> {
        let _ = session_id;
        Err(BoardError::not_implemented("board.authoringRuntimeContext"))
    }
}
