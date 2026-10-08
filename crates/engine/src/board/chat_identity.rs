//! Chat-model slice (A3, PB-1): canonical main/side role resolution from
//! durable lineage; `create_side_chat` (fresh/context, main callers only;
//! side callers get a fresh sibling under the same main) and
//! `create_main_chat` (independent root, written handoff brief from a side
//! chat); conversion in place; source/reference links; Go-to-chat
//! navigation; ticket ↔ working-chat title sync; the fixed original parent;
//! legacy nested child/fork migration; unavailable-parent recovery.
//!
//! Contract: docs/design/project-board/pb0-contract.md §4 and §6. The rules
//! are enforced here for every caller (UI, tools, RPC); hiding a control is
//! never the enforcement. Reuse `child_threads.rs`/`side_chats.rs`
//! machinery for creation; never reparent a ticket worker.
// Entry points the dispatcher does not call yet are wired by the owning slice.
#![allow(dead_code)]
use super::*;
use luna_proto::{
    BoardChatBadges, BoardChatBadgesRequest, BoardChatCreated, BoardChatCreationKind,
    BoardChatIdentityRequest, BoardConvertChatRequest, BoardConvertReceipt,
    BoardCreateMainChatRequest, BoardCreateSideChatRequest, BoardLegacyMigrationReport,
    BoardLinkChatRequest, BoardNavigationRequest, BoardNavigationTarget, BoardTitleOrigin,
    BoardTitleSyncReceipt, CanonicalChatIdentity, WorkItem,
};

impl EngineRpc {
    /// Canonical identity of one chat, derived from the workspace side-chat
    /// row, `ChildThread` metadata and `TaskLineage` (walking legacy nesting
    /// up to the top-level main). Never from tabs, titles or model claims.
    /// Temporary authoring sessions and derived native-agent documents are
    /// not chats: `NotFound`/`InvalidRole`.
    pub(crate) fn board_chat_identity(
        &self,
        caller: &BoardCaller,
        request: BoardChatIdentityRequest,
    ) -> BoardResult<CanonicalChatIdentity> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.chatIdentity"))
    }

    /// Ticket badges for chats (sidebar rows, tabs, headers). Working links
    /// give the ticket badge, reviewer links the reviewer badge; source
    /// discussions only get `related` receipts.
    pub(crate) fn board_chat_badges(
        &self,
        caller: &BoardCaller,
        request: BoardChatBadgesRequest,
    ) -> BoardResult<BoardChatBadges> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.chatBadges"))
    }

    /// From a main caller: one side chat under it (fresh or with permitted
    /// context). From a side caller: a fresh sibling under the caller's main
    /// parent (context requests → `InvalidRole` with allowed alternatives);
    /// creator provenance records the side caller; access never exceeds the
    /// creator's ceiling. Optional ticket binding links the worker with a
    /// frozen `OriginalContainer::SideOf`.
    pub(crate) async fn board_create_side_chat(
        &self,
        caller: &BoardCaller,
        request: BoardCreateSideChatRequest,
    ) -> BoardResult<BoardChatCreated> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.createSideChat"))
    }

    /// A distinct new independent main chat (Cmd+N semantics) with an
    /// optional "Handed off from…" link that is neither parentage nor fork
    /// provenance. The caller never moves, never becomes its child, gains no
    /// lifecycle ownership. `start: false` saves without sending.
    pub(crate) async fn board_create_main_chat(
        &self,
        caller: &BoardCaller,
        request: BoardCreateMainChatRequest,
    ) -> BoardResult<BoardChatCreated> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.createMainChat"))
    }

    /// Convert the caller's existing ordinary chat to a ticket in place: one
    /// atomic create + Working link (`board_insert_item` with origin
    /// `Converted` and event `Converted`). Preserves id, history, provider
    /// binding, placement, draft and active run; sends nothing.
    pub(crate) async fn board_convert_chat(
        &self,
        caller: &BoardCaller,
        request: BoardConvertChatRequest,
    ) -> BoardResult<BoardConvertReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.convertChat"))
    }

    /// Link/unlink a Source, Reference or Attempt chat. Working links only
    /// come from Start or conversion; linking never resends context.
    pub(crate) async fn board_link_chat(
        &self,
        caller: &BoardCaller,
        request: BoardLinkChatRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.linkChat"))
    }

    /// Go to chat: the main chat, or its main parent's workbench with the
    /// exact side chat selected, or a direct view when the parent is
    /// unavailable. Never relaunches, resends, reparents or promotes.
    pub(crate) fn board_navigation(
        &self,
        caller: &BoardCaller,
        request: BoardNavigationRequest,
    ) -> BoardResult<BoardNavigationTarget> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.navigation"))
    }

    /// One rename operation for a ticket and its current primary working
    /// chat (from either surface). Parent/source titles never change;
    /// provider auto-titling must route through here so it cannot drift.
    pub(crate) async fn board_sync_title(
        &self,
        caller: &BoardCaller,
        origin: BoardTitleOrigin,
        title: &str,
    ) -> BoardResult<BoardTitleSyncReceipt> {
        let _ = (caller, origin, title);
        Err(BoardError::not_implemented("board.syncTitle"))
    }

    /// Hook called by `board_update` after a committed title change (the
    /// ticket side of the sync). `store.rs` ignores `NotImplemented`.
    pub(crate) async fn board_title_changed(
        &self,
        item: &WorkItem,
        origin: BoardTitleOrigin,
    ) -> BoardResult<()> {
        let _ = (item, origin);
        Err(BoardError::not_implemented("board.titleChanged"))
    }

    /// Shared guard for every legacy creation entry point (side chats,
    /// message forks, native forks, `create_thread`): returns the caller's
    /// canonical identity when `kind` is allowed, else `InvalidRole` /
    /// `ParentLocked`. Wire it into those paths so depth/context rules hold
    /// even without the board.
    pub(crate) fn board_validate_creation(
        &self,
        caller_task_id: &str,
        kind: BoardChatCreationKind,
    ) -> BoardResult<CanonicalChatIdentity> {
        let _ = (caller_task_id, kind);
        Err(BoardError::not_implemented("board.validateCreation"))
    }

    /// Non-destructive migration of legacy nested children/forks under their
    /// top-level main chat; active descendants are deferred until verified
    /// quiescence. Never recreates chats, rewrites provider ids or replays.
    pub(crate) async fn board_migrate_legacy_nesting(
        &self,
    ) -> BoardResult<BoardLegacyMigrationReport> {
        Err(BoardError::not_implemented("board.migrateLegacyNesting"))
    }
}
