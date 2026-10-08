//! Review slice (A5, PB-1R + PB-3 evidence): workflow-owned reviewer side
//! chats (under a main ticket; a sibling under the same main for a side
//! ticket), the engine-assembled review packet with a fixed diff basis,
//! structured results, the two-round repair cap, staleness, the Done gate,
//! exact reviewer archive+close cleanup with readback, inclusion in shared
//! Stop, and outcome evidence capture (diff/PR/checks/artifacts).
//!
//! Contract: docs/design/project-board/pb0-contract.md §7.4. Reviewers are
//! ordinary chats with a recorded role; never a hidden job, never nested,
//! never a main chat. Cleanup authority comes from the recorded run, not
//! from the descendant-scoped `archiveThreads` guard.
// Entry points the dispatcher does not call yet are wired by the owning slice.
#![allow(dead_code)]
use super::*;
use luna_proto::{
    BoardOutcomes, BoardOutcomesRequest, BoardReviewCancelRequest, BoardReviewStartRequest,
    BoardReviewStatus, BoardReviewStatusRequest, BoardReviewSubmitRequest, OutcomeReference,
    ReviewCleanupReceipt, ReviewRunReceipt,
};

impl EngineRpc {
    /// Create (or reuse across repair rounds) the reviewer side chat for the
    /// current generation, freeze the review basis, resolve the reviewer
    /// profile (implementation-model fallback when none is configured) and
    /// send the bounded review packet. Idempotent by request id.
    pub(crate) async fn board_review_start(
        &self,
        caller: &BoardCaller,
        request: BoardReviewStartRequest,
    ) -> BoardResult<ReviewRunReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.reviewStart"))
    }

    pub(crate) fn board_review_status(
        &self,
        caller: &BoardCaller,
        request: BoardReviewStatusRequest,
    ) -> BoardResult<BoardReviewStatus> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.reviewStatus"))
    }

    /// Cancel one run through the shared Stop of its exact reviewer chat.
    pub(crate) async fn board_review_cancel(
        &self,
        caller: &BoardCaller,
        request: BoardReviewCancelRequest,
    ) -> BoardResult<ReviewRunReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.reviewCancel"))
    }

    /// Structured reviewer result. Only the run's recorded reviewer chat may
    /// submit; a basis/packet mismatch or an older generation makes it stale.
    /// The engine then schedules a bounded repair, Needs attention, or Done.
    pub(crate) async fn board_review_submit(
        &self,
        caller: &BoardCaller,
        request: BoardReviewSubmitRequest,
    ) -> BoardResult<ReviewRunReceipt> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.reviewSubmit"))
    }

    /// After review and ticket success: archive the exact idle reviewer(s)
    /// with archive-then-close ordering, read back, journal partial results
    /// (Done · cleanup pending) and retry the same cleanup identity.
    pub(crate) async fn board_review_cleanup(
        &self,
        review_run_id: &str,
    ) -> BoardResult<ReviewCleanupReceipt> {
        let _ = review_run_id;
        Err(BoardError::not_implemented("board.reviewCleanup"))
    }

    /// Reviewer chat ids owned by a ticket generation, for shared Stop.
    pub(crate) fn board_review_owned_work(
        &self,
        work_item_id: &str,
        execution_generation: u64,
    ) -> BoardResult<Vec<String>> {
        let _ = (work_item_id, execution_generation);
        Err(BoardError::not_implemented("board.reviewOwnedWork"))
    }

    /// Capture outcome evidence (diff hash, PR, checks, artifacts,
    /// documents) for a generation as `OutcomeReference` rows.
    pub(crate) async fn board_capture_outcomes(
        &self,
        work_item_id: &str,
        execution_generation: u64,
    ) -> BoardResult<Vec<OutcomeReference>> {
        let _ = (work_item_id, execution_generation);
        Err(BoardError::not_implemented("board.captureOutcomes"))
    }

    /// Evidence for the detail drawer (bounded, newest first).
    pub(crate) fn board_outcomes(
        &self,
        caller: &BoardCaller,
        request: BoardOutcomesRequest,
    ) -> BoardResult<BoardOutcomes> {
        let _ = (caller, request);
        Err(BoardError::not_implemented("board.outcomes"))
    }
}
