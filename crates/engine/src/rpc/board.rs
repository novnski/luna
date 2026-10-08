//! The single `Board*` RPC dispatcher (docs/design/project-board/pb0-contract.md
//! §5). Every board method is listed here once; the slice modules under
//! `crates/engine/src/board/` own the behavior. RPC callers are the local
//! owner acting for the human; agent tools bind their own caller and call
//! the same service functions.
use super::board_engine::{BoardCaller, board_reply};
use super::*;

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

impl EngineRpc {
    pub(super) async fn handle_board(
        &self,
        method: &str,
        mut params: Value,
    ) -> Result<RpcReply, RpcError> {
        // Owner-local in PB-0: a call addressed to another host is refused,
        // one addressed to this host is served without the routing field.
        if let Some(object) = params.as_object_mut()
            && let Some(target) = object.remove("targetDeviceId")
            && target.as_str() != Some(self.engine_info.device_id.as_str())
        {
            return Err(RpcError::Failed(
                luna_proto::BoardError::Unavailable {
                    reason: "Board methods are served only by the project's owning host.".into(),
                }
                .to_rpc_message(),
            ));
        }
        let caller = BoardCaller::user(&self.engine_info.device_id);
        let caller = &caller;
        match method {
            // ---- store / core (PB-0) ----
            methods::BOARD_LIST => {
                board_reply(self.board_list(caller, board_params(params)?).await)
            }
            methods::BOARD_READ => {
                board_reply(self.board_read(caller, board_params(params)?).await)
            }
            methods::BOARD_CREATE => {
                board_reply(self.board_create(caller, board_params(params)?).await)
            }
            methods::BOARD_UPDATE => {
                board_reply(self.board_update(caller, board_params(params)?).await)
            }
            methods::BOARD_RANK => {
                board_reply(self.board_rank(caller, board_params(params)?).await)
            }
            methods::BOARD_ARCHIVE => {
                board_reply(self.board_archive(caller, board_params(params)?).await)
            }
            methods::BOARD_EVENTS => board_reply(self.board_events(caller, board_params(params)?)),
            methods::BOARD_GET_RECEIPT => {
                board_reply(self.board_get_receipt(caller, board_params(params)?))
            }
            methods::BOARD_WATCH => self
                .board_watch(caller, board_params(params)?)
                .map(RpcReply::Stream)
                .map_err(|error| RpcError::Failed(error.to_rpc_message())),
            // ---- lifecycle (A2) ----
            methods::BOARD_MOVE => {
                board_reply(self.board_move(caller, board_params(params)?).await)
            }
            methods::BOARD_START => {
                board_reply(self.board_start(caller, board_params(params)?).await)
            }
            methods::BOARD_START_BATCH => {
                board_reply(self.board_start_batch(caller, board_params(params)?).await)
            }
            methods::BOARD_CONTROL => {
                board_reply(self.board_control(caller, board_params(params)?).await)
            }
            methods::BOARD_SET_STATUS => {
                board_reply(self.board_set_status(caller, board_params(params)?).await)
            }
            methods::BOARD_SEND_UPDATE => {
                board_reply(self.board_send_update(caller, board_params(params)?).await)
            }
            methods::BOARD_EXECUTION_OPTIONS => board_reply(
                self.board_execution_options(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_RESOLVE_SETTINGS => board_reply(
                self.board_resolve_settings(caller, board_params(params)?)
                    .await,
            ),
            // ---- chat model (A3) ----
            methods::BOARD_CHAT_IDENTITY => {
                board_reply(self.board_chat_identity(caller, board_params(params)?))
            }
            methods::BOARD_CHAT_BADGES => {
                board_reply(self.board_chat_badges(caller, board_params(params)?))
            }
            methods::BOARD_CREATE_SIDE_CHAT => board_reply(
                self.board_create_side_chat(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_CREATE_MAIN_CHAT => board_reply(
                self.board_create_main_chat(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_CONVERT_CHAT => {
                board_reply(self.board_convert_chat(caller, board_params(params)?).await)
            }
            methods::BOARD_LINK_CHAT => {
                board_reply(self.board_link_chat(caller, board_params(params)?).await)
            }
            methods::BOARD_NAVIGATION => {
                board_reply(self.board_navigation(caller, board_params(params)?))
            }
            // ---- review and evidence (A5) ----
            methods::BOARD_REVIEW_START => {
                board_reply(self.board_review_start(caller, board_params(params)?).await)
            }
            methods::BOARD_REVIEW_STATUS => {
                board_reply(self.board_review_status(caller, board_params(params)?))
            }
            methods::BOARD_REVIEW_CANCEL => board_reply(
                self.board_review_cancel(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_REVIEW_SUBMIT => board_reply(
                self.board_review_submit(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_OUTCOMES => {
                board_reply(self.board_outcomes(caller, board_params(params)?))
            }
            // ---- temporary authoring (A6) ----
            methods::BOARD_AUTHORING_START => board_reply(
                self.board_authoring_start(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_AUTHORING_SEND => board_reply(
                self.board_authoring_send(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_AUTHORING_READ => {
                board_reply(self.board_authoring_read(caller, board_params(params)?))
            }
            methods::BOARD_AUTHORING_WATCH => self
                .board_authoring_watch(caller, board_params(params)?)
                .map(RpcReply::Stream)
                .map_err(|error| RpcError::Failed(error.to_rpc_message())),
            methods::BOARD_AUTHORING_CHECKPOINT => board_reply(
                self.board_authoring_checkpoint(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_AUTHORING_INTERRUPT => board_reply(
                self.board_authoring_interrupt(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_AUTHORING_CLOSE => board_reply(
                self.board_authoring_close(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_AUTHORING_DISCARD => board_reply(
                self.board_authoring_discard(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_AUTHORING_LIST => {
                board_reply(self.board_authoring_list(caller, board_params(params)?))
            }
            // ---- GitHub issues and dependencies (A7) ----
            methods::BOARD_ISSUE_SOURCES => board_reply(
                self.board_issue_sources(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_ISSUE_SEARCH => {
                board_reply(self.board_issue_search(caller, board_params(params)?).await)
            }
            methods::BOARD_ISSUE_DETAIL => {
                board_reply(self.board_issue_detail(caller, board_params(params)?).await)
            }
            methods::BOARD_ISSUE_COMMENTS => board_reply(
                self.board_issue_comments(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_ISSUE_IMPORT => {
                board_reply(self.board_issue_import(caller, board_params(params)?).await)
            }
            methods::BOARD_ISSUE_LINK => {
                board_reply(self.board_issue_link(caller, board_params(params)?).await)
            }
            methods::BOARD_ISSUE_UNLINK => {
                board_reply(self.board_issue_unlink(caller, board_params(params)?).await)
            }
            methods::BOARD_SET_DEPENDENCY => board_reply(
                self.board_set_dependency(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_ISSUE_WRITEBACK_PREVIEW => board_reply(
                self.board_issue_writeback_preview(caller, board_params(params)?)
                    .await,
            ),
            methods::BOARD_ISSUE_WRITEBACK => board_reply(
                self.board_issue_writeback(caller, board_params(params)?)
                    .await,
            ),
            // ---- agent tools (A4) ----
            methods::BOARD_TOOL_CATALOG => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct Catalog {
                    task_id: String,
                }
                let request: Catalog = board_params(params)?;
                board_reply(self.board_tool_catalog(&request.task_id))
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
