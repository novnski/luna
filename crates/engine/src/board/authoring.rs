//! Authoring slice (A6): Discuss with AI as a temporary, project-aware
//! session that is never a chat — excluded from every chat registry,
//! listing, search, sync projection, Settled/Archive and provider-history
//! import. Board-draft checkpoint/resume/discard, runtime stop on close and
//! owned cleanup. The assistant saves tickets through `board_create` with a
//! `BoardCaller::Authoring` binding.
//!
//! Contract: docs/design/project-board/pb0-contract.md §7.5. Storage:
//! `board_authoring_sessions` / `board_authoring_entries` (already migrated).
//! Ordinary-chat surfaces must call `DocsStore::is_board_authoring_session`
//! to exclude these ids.
use super::*;
use luna_proto::{
    BoardAuthoringCheckpointRequest, BoardAuthoringList, BoardAuthoringListRequest,
    BoardAuthoringPage, BoardAuthoringReadRequest, BoardAuthoringSendRequest,
    BoardAuthoringSessionRequest, BoardAuthoringStartRequest, TicketAuthoringSession,
};

impl EngineRpc {
    /// Start a new session or resume a Board draft in the same project with
    /// the recorded discussion and current validated access. Idempotent by
    /// request id; the discussion profile is independent of the ticket's
    /// implementation/review profiles.
    pub(crate) async fn board_authoring_start(
        &self,
        caller: &BoardCaller,
        request: BoardAuthoringStartRequest,
    ) -> BoardResult<TicketAuthoringSession> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.authoringStart"))
    }

    /// Send one user message (no-replay by `message_id`) through the
    /// project-aware harness run with the engine-owned authoring
    /// instructions and authoring tool catalog.
    pub(crate) async fn board_authoring_send(
        &self,
        caller: &BoardCaller,
        request: BoardAuthoringSendRequest,
    ) -> BoardResult<TicketAuthoringSession> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.authoringSend"))
    }

    pub(crate) fn board_authoring_read(
        &self,
        caller: &BoardCaller,
        request: BoardAuthoringReadRequest,
    ) -> BoardResult<BoardAuthoringPage> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.authoringRead"))
    }

    /// Stream of `BoardAuthoringPage` frames for the session's composer.
    pub(crate) fn board_authoring_watch(
        &self,
        caller: &BoardCaller,
        request: BoardAuthoringSessionRequest,
    ) -> BoardResult<BoxStream<'static, Value>> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.authoringWatch"))
    }

    /// The assistant's draft proposal/configuration (read-only to the user).
    /// Creates no ticket and no chat.
    pub(crate) async fn board_authoring_checkpoint(
        &self,
        caller: &BoardCaller,
        request: BoardAuthoringCheckpointRequest,
    ) -> BoardResult<TicketAuthoringSession> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.authoringCheckpoint"))
    }

    /// Stop the session's current turn without closing it.
    pub(crate) async fn board_authoring_interrupt(
        &self,
        caller: &BoardCaller,
        request: BoardAuthoringSessionRequest,
    ) -> BoardResult<TicketAuthoringSession> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.authoringInterrupt"))
    }

    /// Close: stop the exact runtime and its endpoint; unfinished → `Draft`
    /// (resumable Board draft only); saved → `Ended` with owned cleanup.
    pub(crate) async fn board_authoring_close(
        &self,
        caller: &BoardCaller,
        request: BoardAuthoringSessionRequest,
    ) -> BoardResult<TicketAuthoringSession> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.authoringClose"))
    }

    /// Discard: stop the runtime and remove the draft (keeping only minimal
    /// exclusion metadata so its provider session is never re-imported).
    pub(crate) async fn board_authoring_discard(
        &self,
        caller: &BoardCaller,
        request: BoardAuthoringSessionRequest,
    ) -> BoardResult<TicketAuthoringSession> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.authoringDiscard"))
    }

    /// Resumable drafts and open sessions (never ended/discarded).
    pub(crate) fn board_authoring_list(
        &self,
        caller: &BoardCaller,
        request: BoardAuthoringListRequest,
    ) -> BoardResult<BoardAuthoringList> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.authoringList"))
    }
}
