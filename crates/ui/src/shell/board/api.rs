//! Typed async calls for every `Board*` RPC method. UI slices talk to the
//! engine only through these: the engine owns validation, authority,
//! receipts and state, and returns structured [`BoardError`]s.
use luna_proto::{
    BoardArchiveRequest, BoardAuthoringCheckpointRequest, BoardAuthoringList,
    BoardAuthoringListRequest, BoardAuthoringPage, BoardAuthoringReadRequest,
    BoardAuthoringSendRequest, BoardAuthoringSessionRequest, BoardAuthoringStartRequest,
    BoardChatBadges, BoardChatBadgesRequest, BoardChatCreated, BoardChatIdentityRequest,
    BoardControlReceipt, BoardControlRequest, BoardConvertChatRequest, BoardConvertReceipt,
    BoardCreateMainChatRequest, BoardCreateRequest, BoardCreateSideChatRequest, BoardDeleteRequest,
    BoardError, BoardEventPage, BoardEventsRequest, BoardExecutionOptions,
    BoardExecutionOptionsRequest, BoardIssueCommentsRequest, BoardIssueDetailRequest,
    BoardIssueImportReceipt, BoardIssueImportRequest, BoardIssueLinkRequest,
    BoardIssueSearchRequest, BoardIssueSources, BoardIssueSourcesRequest, BoardIssueUnlinkRequest,
    BoardIssueWritebackPreviewRequest, BoardIssueWritebackRequest, BoardItemDetail,
    BoardLinkChatRequest, BoardListPage, BoardListRequest, BoardMoveReceipt, BoardMoveRequest,
    BoardMutationReceipt, BoardNavigationRequest, BoardNavigationTarget, BoardOutcomes,
    BoardOutcomesRequest, BoardRankRequest, BoardReadRequest, BoardReceiptRequest,
    BoardResolveSettingsRequest, BoardResolvedSettings, BoardReviewCancelRequest,
    BoardReviewStartRequest, BoardReviewStatus, BoardReviewStatusRequest, BoardReviewSubmitRequest,
    BoardSendUpdateReceipt, BoardSendUpdateRequest, BoardSetDependencyRequest,
    BoardSetStatusRequest, BoardStartBatchReceipt, BoardStartBatchRequest, BoardStartReceipt,
    BoardStartRequest, BoardUpdateRequest, BoardWatchRequest, CanonicalChatIdentity,
    IssueCommentPage, IssueDetail, IssueSearchPage, IssueWritebackPreview, IssueWritebackReceipt,
    ReviewRunReceipt, TicketAuthoringSession,
};
use luna_rpc::{RpcError, RpcSubscription, methods};

use crate::state::EngineHandle;

/// Why a board call did not return its value.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardCallError {
    /// The engine refused with a structured, actionable error.
    Board(BoardError),
    /// The answer was lost in transport. Mutations must be reconciled with
    /// `receipt(request_id)` before any retry; never resend blindly.
    Lost,
    /// The host does not serve the board (older engine).
    Unsupported,
    /// Anything else (bad params, unexpected failure).
    Other(String),
}

impl BoardCallError {
    pub fn message(&self) -> String {
        match self {
            Self::Board(error) => error.message(),
            Self::Lost => "The answer was lost. Checking whether the change was saved…".into(),
            Self::Unsupported => "This host does not support the Board yet.".into(),
            Self::Other(message) => message.clone(),
        }
    }

    pub fn board(&self) -> Option<&BoardError> {
        match self {
            Self::Board(error) => Some(error),
            _ => None,
        }
    }

    fn from_rpc(error: RpcError) -> Self {
        match error {
            RpcError::Failed(message) => BoardError::from_rpc_message(&message)
                .map(Self::Board)
                .unwrap_or(Self::Other(message)),
            RpcError::UnknownMethod(_) => Self::Unsupported,
            RpcError::Transport(_) | RpcError::Closed => Self::Lost,
            RpcError::BadParams(message) => Self::Other(message),
        }
    }
}

/// The board API bound to one engine connection.
#[derive(Clone)]
pub struct BoardApi {
    engine: EngineHandle,
}

impl BoardApi {
    pub fn new(engine: EngineHandle) -> Self {
        Self { engine }
    }

    async fn call<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        request: &Req,
    ) -> Result<Resp, BoardCallError> {
        let params = serde_json::to_value(request)
            .map_err(|error| BoardCallError::Other(error.to_string()))?;
        self.engine
            .client()
            .call_as(method, params)
            .await
            .map_err(BoardCallError::from_rpc)
    }

    async fn subscribe<Req: serde::Serialize>(
        &self,
        method: &str,
        request: &Req,
    ) -> Result<RpcSubscription, BoardCallError> {
        let params = serde_json::to_value(request)
            .map_err(|error| BoardCallError::Other(error.to_string()))?;
        self.engine
            .client()
            .subscribe_checked(method, params)
            .await
            .map_err(BoardCallError::from_rpc)
    }

    /// `BoardWatch`: frames of `BoardWatchFrame`.
    pub async fn watch(
        &self,
        request: BoardWatchRequest,
    ) -> Result<RpcSubscription, BoardCallError> {
        self.subscribe(methods::BOARD_WATCH, &request).await
    }

    /// `BoardAuthoringWatch`: frames of `BoardAuthoringPage`.
    pub async fn authoring_watch(
        &self,
        request: BoardAuthoringSessionRequest,
    ) -> Result<RpcSubscription, BoardCallError> {
        self.subscribe(methods::BOARD_AUTHORING_WATCH, &request)
            .await
    }

    /// `BoardToolCatalog` diagnostics for one chat.
    // Part of the typed surface (one method per Board* RPC, contract §9);
    // no UI surface shows tool diagnostics yet.
    #[allow(dead_code)]
    pub async fn tool_catalog(&self, task_id: &str) -> Result<serde_json::Value, BoardCallError> {
        self.call(
            methods::BOARD_TOOL_CATALOG,
            &serde_json::json!({ "taskId": task_id }),
        )
        .await
    }
}

/// One typed method per unary board RPC.
macro_rules! board_calls {
    ($($(#[$doc:meta])* $name:ident($request:ty) -> $response:ty = $method:ident;)+) => {
        impl BoardApi {
            $(
                $(#[$doc])*
                // The typed surface covers every unary Board* RPC (contract
                // §9); not every method has a UI caller yet (agent-only or
                // engine-driven operations).
                #[allow(dead_code)]
                pub async fn $name(&self, request: $request) -> Result<$response, BoardCallError> {
                    self.call(methods::$method, &request).await
                }
            )+
        }
    };
}

board_calls! {
    // ---- store / core ----
    list(BoardListRequest) -> BoardListPage = BOARD_LIST;
    read(BoardReadRequest) -> BoardItemDetail = BOARD_READ;
    /// Save only: no chat, no launch.
    create(BoardCreateRequest) -> BoardMutationReceipt = BOARD_CREATE;
    update(BoardUpdateRequest) -> BoardMutationReceipt = BOARD_UPDATE;
    rank(BoardRankRequest) -> BoardMutationReceipt = BOARD_RANK;
    archive(BoardArchiveRequest) -> BoardMutationReceipt = BOARD_ARCHIVE;
    delete(BoardDeleteRequest) -> BoardMutationReceipt = BOARD_DELETE;
    events(BoardEventsRequest) -> BoardEventPage = BOARD_EVENTS;
    /// Reconcile a lost mutation by its original request id.
    receipt(BoardReceiptRequest) -> Option<BoardMutationReceipt> = BOARD_GET_RECEIPT;
    // ---- lifecycle ----
    move_item(BoardMoveRequest) -> BoardMoveReceipt = BOARD_MOVE;
    start(BoardStartRequest) -> BoardStartReceipt = BOARD_START;
    start_batch(BoardStartBatchRequest) -> BoardStartBatchReceipt = BOARD_START_BATCH;
    control(BoardControlRequest) -> BoardControlReceipt = BOARD_CONTROL;
    set_status(BoardSetStatusRequest) -> BoardMutationReceipt = BOARD_SET_STATUS;
    send_update(BoardSendUpdateRequest) -> BoardSendUpdateReceipt = BOARD_SEND_UPDATE;
    execution_options(BoardExecutionOptionsRequest) -> BoardExecutionOptions = BOARD_EXECUTION_OPTIONS;
    generate_title(luna_proto::BoardGenerateTitleRequest) -> luna_proto::BoardGeneratedTitle = BOARD_GENERATE_TITLE;
    resolve_settings(BoardResolveSettingsRequest) -> BoardResolvedSettings = BOARD_RESOLVE_SETTINGS;
    // ---- chat model ----
    chat_identity(BoardChatIdentityRequest) -> CanonicalChatIdentity = BOARD_CHAT_IDENTITY;
    chat_badges(BoardChatBadgesRequest) -> BoardChatBadges = BOARD_CHAT_BADGES;
    create_side_chat(BoardCreateSideChatRequest) -> BoardChatCreated = BOARD_CREATE_SIDE_CHAT;
    create_main_chat(BoardCreateMainChatRequest) -> BoardChatCreated = BOARD_CREATE_MAIN_CHAT;
    convert_chat(BoardConvertChatRequest) -> BoardConvertReceipt = BOARD_CONVERT_CHAT;
    link_chat(BoardLinkChatRequest) -> BoardMutationReceipt = BOARD_LINK_CHAT;
    navigation(BoardNavigationRequest) -> BoardNavigationTarget = BOARD_NAVIGATION;
    // ---- review and evidence ----
    review_start(BoardReviewStartRequest) -> ReviewRunReceipt = BOARD_REVIEW_START;
    review_status(BoardReviewStatusRequest) -> BoardReviewStatus = BOARD_REVIEW_STATUS;
    review_cancel(BoardReviewCancelRequest) -> ReviewRunReceipt = BOARD_REVIEW_CANCEL;
    review_submit(BoardReviewSubmitRequest) -> ReviewRunReceipt = BOARD_REVIEW_SUBMIT;
    outcomes(BoardOutcomesRequest) -> BoardOutcomes = BOARD_OUTCOMES;
    // ---- temporary authoring ----
    authoring_start(BoardAuthoringStartRequest) -> TicketAuthoringSession = BOARD_AUTHORING_START;
    authoring_send(BoardAuthoringSendRequest) -> TicketAuthoringSession = BOARD_AUTHORING_SEND;
    authoring_read(BoardAuthoringReadRequest) -> BoardAuthoringPage = BOARD_AUTHORING_READ;
    authoring_checkpoint(BoardAuthoringCheckpointRequest) -> TicketAuthoringSession = BOARD_AUTHORING_CHECKPOINT;
    authoring_interrupt(BoardAuthoringSessionRequest) -> TicketAuthoringSession = BOARD_AUTHORING_INTERRUPT;
    authoring_close(BoardAuthoringSessionRequest) -> TicketAuthoringSession = BOARD_AUTHORING_CLOSE;
    authoring_discard(BoardAuthoringSessionRequest) -> TicketAuthoringSession = BOARD_AUTHORING_DISCARD;
    authoring_list(BoardAuthoringListRequest) -> BoardAuthoringList = BOARD_AUTHORING_LIST;
    // ---- GitHub issues and dependencies ----
    issue_sources(BoardIssueSourcesRequest) -> BoardIssueSources = BOARD_ISSUE_SOURCES;
    issue_search(BoardIssueSearchRequest) -> IssueSearchPage = BOARD_ISSUE_SEARCH;
    issue_detail(BoardIssueDetailRequest) -> IssueDetail = BOARD_ISSUE_DETAIL;
    issue_comments(BoardIssueCommentsRequest) -> IssueCommentPage = BOARD_ISSUE_COMMENTS;
    issue_import(BoardIssueImportRequest) -> BoardIssueImportReceipt = BOARD_ISSUE_IMPORT;
    issue_link(BoardIssueLinkRequest) -> BoardMutationReceipt = BOARD_ISSUE_LINK;
    issue_unlink(BoardIssueUnlinkRequest) -> BoardMutationReceipt = BOARD_ISSUE_UNLINK;
    set_dependency(BoardSetDependencyRequest) -> BoardMutationReceipt = BOARD_SET_DEPENDENCY;
    issue_writeback_preview(BoardIssueWritebackPreviewRequest) -> IssueWritebackPreview = BOARD_ISSUE_WRITEBACK_PREVIEW;
    issue_writeback(BoardIssueWritebackRequest) -> IssueWritebackReceipt = BOARD_ISSUE_WRITEBACK;
}
