//! CORE-12 native UI contributions: `ListUiContributions`,
//! `WatchUiContributions`, `GetUiContributionView` and `InvokeUiAction`.
//! Local-only catalog rows served by the engine's local service (the
//! in-process app and owner IPC); a remote session and an extension worker
//! never reach them. Listing starts nothing; a worker view or action starts
//! one supervised instance of the exact package and stops it again.

use super::*;

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_UI_CONTRIBUTIONS
            | methods::WATCH_UI_CONTRIBUTIONS
            | methods::GET_UI_CONTRIBUTION_VIEW
            | methods::INVOKE_UI_ACTION
    )
}

impl EngineRpc {
    pub(super) async fn handle_extension_ui(
        &self,
        method: &str,
        mut params: Value,
    ) -> Result<RpcReply, RpcError> {
        if params
            .get("targetDeviceId")
            .and_then(Value::as_str)
            .is_some_and(|id| id != self.engine_info.device_id)
        {
            return Err(RpcError::Failed(
                "Extension views come from the device that installs them".into(),
            ));
        }
        if let Some(object) = params.as_object_mut() {
            object.remove("targetDeviceId");
        }
        let manager = self
            .extensions
            .clone()
            .ok_or_else(|| RpcError::Failed("Extensions are unavailable on this engine".into()))?;
        match method {
            methods::LIST_UI_CONTRIBUTIONS => {
                let snapshot = tokio::task::spawn_blocking(move || manager.ui_snapshot())
                    .await
                    .map_err(|error| {
                        RpcError::Failed(format!("the contribution listing stopped: {error}"))
                    })?;
                RpcReply::value(&snapshot)
            }
            methods::WATCH_UI_CONTRIBUTIONS => {
                let stream = manager
                    .ui_changes()
                    .filter_map(|snapshot| async move { serde_json::to_value(snapshot).ok() })
                    .boxed();
                Ok(RpcReply::Stream(stream))
            }
            methods::GET_UI_CONTRIBUTION_VIEW => {
                let request: luna_proto::GetUiContributionViewRequest = parse_params(params)?;
                let outcome = manager.ui_view(request).await.map_err(RpcError::Failed)?;
                RpcReply::value(&outcome)
            }
            methods::INVOKE_UI_ACTION => {
                let request: luna_proto::InvokeUiActionRequest = parse_params(params)?;
                let outcome = manager.ui_action(request).await.map_err(RpcError::Failed)?;
                RpcReply::value(&outcome)
            }
            other => Err(RpcError::UnknownMethod(other.to_string())),
        }
    }
}
