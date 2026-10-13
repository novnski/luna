//! CORE-06 extension management: `ListExtensions`, `InspectExtension`,
//! `InstallExtension` and `SetExtensionEnabled`. All four are local-only
//! catalog rows (owner IPC and the in-process app; never a remote session)
//! and are served only by the engine's local service. Package I/O runs on a
//! blocking thread; the command admission stays held until it finishes.

use super::*;

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_EXTENSIONS
            | methods::INSPECT_EXTENSION
            | methods::INSTALL_EXTENSION
            | methods::SET_EXTENSION_ENABLED
    )
}

impl EngineRpc {
    pub(super) async fn handle_extensions(
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
                "Manage extensions on the device that installs them".into(),
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
            methods::LIST_EXTENSIONS => {
                let snapshot = blocking(move || manager.snapshot()).await?;
                RpcReply::value(&snapshot)
            }
            methods::INSPECT_EXTENSION => {
                let request: luna_proto::InspectExtensionRequest = parse_params(params)?;
                let view = blocking(move || manager.inspect(&request.package_hash)).await?;
                RpcReply::value(&view)
            }
            methods::INSTALL_EXTENSION => {
                let request: luna_proto::InstallExtensionRequest = parse_params(params)?;
                let view = blocking(move || manager.install(&request)).await?;
                RpcReply::value(&view)
            }
            methods::SET_EXTENSION_ENABLED => {
                let request: luna_proto::SetExtensionEnabledRequest = parse_params(params)?;
                let view = blocking(move || manager.set_enabled(&request)).await?;
                RpcReply::value(&view)
            }
            other => Err(RpcError::UnknownMethod(other.to_string())),
        }
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, RpcError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| RpcError::Failed(format!("extension operation stopped: {error}")))?
        .map_err(RpcError::Failed)
}
