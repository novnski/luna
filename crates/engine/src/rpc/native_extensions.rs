//! `GetNativeExtensions`: the effective native extension view (settings
//! sources, output style, hooks, commands and skills) for the launch a chat or
//! project would use. Local-only, like the integration inventory: the files
//! and the provider binary live on the owning device.
use luna_proto::{HarnessId, NativeExtensionsRequest, NativeExtensionsView};
use luna_rpc::RpcError;
use serde::Deserialize;

use super::{EngineRpc, FileSearchParams, require_local_integration_target};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct NativeExtensionsParams {
    #[serde(default)]
    harness: Option<HarnessId>,
    #[serde(default)]
    target_device_id: Option<String>,
    #[serde(flatten)]
    request: NativeExtensionsRequest,
}

pub(super) async fn get(
    rpc: &EngineRpc,
    p: NativeExtensionsParams,
) -> Result<NativeExtensionsView, RpcError> {
    require_local_integration_target(p.target_device_id.as_deref(), &rpc.engine_info.device_id)?;
    let request = p.request;
    let chat = request
        .chat_id
        .as_deref()
        .map(|id| rpc.workspace.chat(id))
        .transpose()
        .map_err(|error| RpcError::Failed(error.to_string()))?
        .flatten();
    if request.chat_id.is_some() && chat.is_none() {
        return Err(RpcError::BadParams("unknown task".into()));
    }
    let config = chat.as_ref().and_then(|chat| chat.config.as_ref());
    let harness = p
        .harness
        .or(config.map(|config| config.harness))
        .unwrap_or(HarnessId::ClaudeCode);
    let mode = request
        .permission_mode
        .or(config.map(|config| config.effective_permission_mode))
        .unwrap_or_default();
    let cwd = if request.chat_id.is_some() || request.space_id.is_some() || request.path.is_some() {
        Some(
            rpc.file_search_root(&FileSearchParams {
                query: String::new(),
                chat_id: request.chat_id.clone(),
                space_id: request.space_id.clone(),
                path: request.path.clone(),
            })
            .await?,
        )
    } else {
        None
    };
    let provider = rpc
        .registry
        .resolve(harness)
        .map_err(|error| RpcError::Failed(error.to_string()))?;
    provider
        .native_extensions(cwd.as_deref(), mode, request.refresh)
        .await
        .map_err(|error| RpcError::Failed(error.to_string()))
}
