//! GitHub slice (A7, PB-2/PB-3): issue sources through the existing
//! `gh`-backed source-control layer on the project's owning host;
//! list/search/detail/comments with PR filtering, bounded pagination,
//! account-scoped caches and freshness; deduplicated import/link/start;
//! blocked-by dependencies with cycle detection (never auto-dispatch); and
//! explicit previewed writeback with independent readback.
//!
//! Contract: docs/design/project-board/pb0-contract.md §7.6. Dedupe is the
//! unique `(project_id, dedupe_key)` index on active `board_issue_refs`
//! rows; imports insert the ticket and its reference in one commit via
//! `board_insert_item` (origin `Issue`). No network from tests.
use super::*;
use luna_proto::{
    BoardIssueCommentsRequest, BoardIssueDetailRequest, BoardIssueImportReceipt,
    BoardIssueImportRequest, BoardIssueLinkRequest, BoardIssueSearchRequest, BoardIssueSources,
    BoardIssueSourcesRequest, BoardIssueUnlinkRequest, BoardIssueWritebackPreviewRequest,
    BoardIssueWritebackRequest, BoardSetDependencyRequest, IssueCommentPage, IssueDetail,
    IssueSearchPage, IssueWritebackPreview, IssueWritebackReceipt,
};

impl EngineRpc {
    /// Verified repositories for the project's checkout (explicit selection
    /// when several remotes/forks exist; origin is never assumed).
    pub(crate) async fn board_issue_sources(
        &self,
        caller: &BoardCaller,
        request: BoardIssueSourcesRequest,
    ) -> BoardResult<BoardIssueSources> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.issueSources"))
    }

    /// Bounded, paginated issue search; PRs excluded; partial/failed pages
    /// disclosed; cache namespaced by host/account/repository.
    pub(crate) async fn board_issue_search(
        &self,
        caller: &BoardCaller,
        request: BoardIssueSearchRequest,
    ) -> BoardResult<IssueSearchPage> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.issueSearch"))
    }

    pub(crate) async fn board_issue_detail(
        &self,
        caller: &BoardCaller,
        request: BoardIssueDetailRequest,
    ) -> BoardResult<IssueDetail> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.issueDetail"))
    }

    pub(crate) async fn board_issue_comments(
        &self,
        caller: &BoardCaller,
        request: BoardIssueCommentsRequest,
    ) -> BoardResult<IssueCommentPage> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.issueComments"))
    }

    /// Add to board (or add and start). An already-linked issue returns the
    /// existing card (`Existing`); never a second dispatch.
    pub(crate) async fn board_issue_import(
        &self,
        caller: &BoardCaller,
        request: BoardIssueImportRequest,
    ) -> BoardResult<BoardIssueImportReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.issueImport"))
    }

    pub(crate) async fn board_issue_link(
        &self,
        caller: &BoardCaller,
        request: BoardIssueLinkRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.issueLink"))
    }

    /// Detach local provenance only; never deletes the issue or a chat.
    pub(crate) async fn board_issue_unlink(
        &self,
        caller: &BoardCaller,
        request: BoardIssueUnlinkRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.issueUnlink"))
    }

    /// Add/remove a blocked-by relationship; cycles → `DependencyCycle`.
    /// Clearing a dependency never authorizes or launches work.
    pub(crate) async fn board_set_dependency(
        &self,
        caller: &BoardCaller,
        request: BoardSetDependencyRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.setDependency"))
    }

    /// PB-3: show the exact account/repo/issue and payload before writing.
    pub(crate) async fn board_issue_writeback_preview(
        &self,
        caller: &BoardCaller,
        request: BoardIssueWritebackPreviewRequest,
    ) -> BoardResult<IssueWritebackPreview> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.issueWritebackPreview"))
    }

    /// PB-3: apply the previewed write once, read it back independently, and
    /// reconcile a lost response before any retry (no duplicate comment).
    pub(crate) async fn board_issue_writeback(
        &self,
        caller: &BoardCaller,
        request: BoardIssueWritebackRequest,
    ) -> BoardResult<IssueWritebackReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.issueWriteback"))
    }
}
