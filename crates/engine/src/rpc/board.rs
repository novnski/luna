//! The single `Board*` RPC dispatcher (docs/design/project-board/pb0-contract.md
//! §5). Every board method is listed here once; the slice modules under
//! `crates/engine/src/board/` own the behavior. RPC callers are the local
//! owner acting for the human; agent tools bind their own caller and call
//! the same service functions.
//!
//! Stack discipline: `handle_board` is a plain function returning one boxed
//! future per method. Each arm's service future is created and polled inside
//! its own heap allocation, so neither this dispatcher nor the main RPC
//! handler holds every arm's callee future (and its debug-build
//! temporaries) in one poll frame. Inlined, the board arms alone took
//! ~834 KiB of a 2 MiB tokio worker stack and `BoardStart` overflowed it.
use super::board_engine::{BoardCaller, board_reply};
use super::*;

/// One boxed dispatcher future.
pub(super) type BoardRpcFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<RpcReply, RpcError>> + Send + 'a>>;

/// Whether `method` belongs to the board family.
pub(super) fn handles(method: &str) -> bool {
    methods::BOARD_METHODS.contains(&method)
}

fn board_params<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    parse_params(if params.is_null() {
        serde_json::json!({})
    } else {
        params
    })
}

/// A unary method served by an async board function, in its own boxed
/// future. The caller is bound inside the future (the local owner).
macro_rules! board_async {
    ($this:ident, $params:ident, $function:ident) => {
        Box::pin(async move {
            let caller = BoardCaller::user(&$this.engine_info.device_id);
            let request = board_params($params)?;
            board_reply($this.$function(&caller, request).await)
        })
    };
}

/// A unary method served by a synchronous board function.
macro_rules! board_sync {
    ($this:ident, $params:ident, $function:ident) => {
        Box::pin(async move {
            let caller = BoardCaller::user(&$this.engine_info.device_id);
            let request = board_params($params)?;
            board_reply($this.$function(&caller, request))
        })
    };
}

/// A streaming method served by a board function returning a stream.
macro_rules! board_stream {
    ($this:ident, $params:ident, $function:ident) => {
        Box::pin(async move {
            let caller = BoardCaller::user(&$this.engine_info.device_id);
            let request = board_params($params)?;
            $this
                .$function(&caller, request)
                .map(RpcReply::Stream)
                .map_err(|error| RpcError::Failed(error.to_rpc_message()))
        })
    };
}

impl EngineRpc {
    pub(super) fn handle_board<'a>(
        &'a self,
        method: &'a str,
        mut params: Value,
    ) -> BoardRpcFuture<'a> {
        // Owner-local in PB-0: a call addressed to another host is refused,
        // one addressed to this host is served without the routing field.
        if let Some(object) = params.as_object_mut()
            && let Some(target) = object.remove("targetDeviceId")
            && target.as_str() != Some(self.engine_info.device_id.as_str())
        {
            return Box::pin(async move {
                Err(RpcError::Failed(
                    luna_proto::BoardError::Unavailable {
                        reason: "Board methods are served only by the project's owning host."
                            .into(),
                    }
                    .to_rpc_message(),
                ))
            });
        }
        let this = self;
        match method {
            // ---- store / core (PB-0) ----
            methods::BOARD_LIST => board_async!(this, params, board_list),
            methods::BOARD_READ => board_async!(this, params, board_read),
            methods::BOARD_CREATE => board_async!(this, params, board_create),
            methods::BOARD_UPDATE => board_async!(this, params, board_update),
            methods::BOARD_RANK => board_async!(this, params, board_rank),
            methods::BOARD_ARCHIVE => board_async!(this, params, board_archive),
            methods::BOARD_DELETE => board_async!(this, params, board_delete),
            methods::BOARD_EVENTS => board_sync!(this, params, board_events),
            methods::BOARD_GET_RECEIPT => board_sync!(this, params, board_get_receipt),
            methods::BOARD_WATCH => board_stream!(this, params, board_watch),
            // ---- lifecycle (A2) ----
            methods::BOARD_MOVE => board_async!(this, params, board_move),
            methods::BOARD_START => board_async!(this, params, board_start),
            methods::BOARD_START_BATCH => board_async!(this, params, board_start_batch),
            methods::BOARD_CONTROL => board_async!(this, params, board_control),
            methods::BOARD_SET_STATUS => board_async!(this, params, board_set_status),
            methods::BOARD_SEND_UPDATE => board_async!(this, params, board_send_update),
            methods::BOARD_EXECUTION_OPTIONS => {
                board_async!(this, params, board_execution_options)
            }
            methods::BOARD_RESOLVE_SETTINGS => board_async!(this, params, board_resolve_settings),
            methods::BOARD_GENERATE_TITLE => board_async!(this, params, board_generate_title),
            // ---- chat model (A3) ----
            methods::BOARD_CHAT_IDENTITY => board_sync!(this, params, board_chat_identity),
            methods::BOARD_CHAT_BADGES => board_sync!(this, params, board_chat_badges),
            methods::BOARD_CREATE_SIDE_CHAT => board_async!(this, params, board_create_side_chat),
            methods::BOARD_CREATE_MAIN_CHAT => board_async!(this, params, board_create_main_chat),
            methods::BOARD_CONVERT_CHAT => board_async!(this, params, board_convert_chat),
            methods::BOARD_LINK_CHAT => board_async!(this, params, board_link_chat),
            methods::BOARD_NAVIGATION => board_sync!(this, params, board_navigation),
            // ---- review and evidence (A5) ----
            methods::BOARD_REVIEW_START => board_async!(this, params, board_review_start),
            methods::BOARD_REVIEW_STATUS => board_sync!(this, params, board_review_status),
            methods::BOARD_REVIEW_CANCEL => board_async!(this, params, board_review_cancel),
            methods::BOARD_REVIEW_SUBMIT => board_async!(this, params, board_review_submit),
            methods::BOARD_OUTCOMES => board_sync!(this, params, board_outcomes),
            // ---- temporary authoring (A6) ----
            methods::BOARD_AUTHORING_START => board_async!(this, params, board_authoring_start),
            methods::BOARD_AUTHORING_SEND => board_async!(this, params, board_authoring_send),
            methods::BOARD_AUTHORING_READ => board_sync!(this, params, board_authoring_read),
            methods::BOARD_AUTHORING_WATCH => board_stream!(this, params, board_authoring_watch),
            methods::BOARD_AUTHORING_CHECKPOINT => {
                board_async!(this, params, board_authoring_checkpoint)
            }
            methods::BOARD_AUTHORING_INTERRUPT => {
                board_async!(this, params, board_authoring_interrupt)
            }
            methods::BOARD_AUTHORING_CLOSE => board_async!(this, params, board_authoring_close),
            methods::BOARD_AUTHORING_DISCARD => board_async!(this, params, board_authoring_discard),
            methods::BOARD_AUTHORING_LIST => board_sync!(this, params, board_authoring_list),
            // ---- GitHub issues and dependencies (A7) ----
            methods::BOARD_ISSUE_SOURCES => board_async!(this, params, board_issue_sources),
            methods::BOARD_ISSUE_SEARCH => board_async!(this, params, board_issue_search),
            methods::BOARD_ISSUE_DETAIL => board_async!(this, params, board_issue_detail),
            methods::BOARD_ISSUE_COMMENTS => board_async!(this, params, board_issue_comments),
            methods::BOARD_ISSUE_IMPORT => board_async!(this, params, board_issue_import),
            methods::BOARD_ISSUE_LINK => board_async!(this, params, board_issue_link),
            methods::BOARD_ISSUE_UNLINK => board_async!(this, params, board_issue_unlink),
            methods::BOARD_SET_DEPENDENCY => board_async!(this, params, board_set_dependency),
            methods::BOARD_ISSUE_WRITEBACK_PREVIEW => {
                board_async!(this, params, board_issue_writeback_preview)
            }
            methods::BOARD_ISSUE_WRITEBACK => board_async!(this, params, board_issue_writeback),
            // ---- agent tools (A4) ----
            methods::BOARD_TOOL_CATALOG => Box::pin(async move {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct Catalog {
                    task_id: String,
                }
                let request: Catalog = board_params(params)?;
                board_reply(this.board_tool_catalog(&request.task_id))
            }),
            _ => Box::pin(async move { Err(RpcError::UnknownMethod(method.into())) }),
        }
    }
}
