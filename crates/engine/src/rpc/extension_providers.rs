//! CORE-08 `ListExtensionProviders`: the provider contributions of this
//! profile's enabled extension packages, registered in the harness registry
//! or refused with the reason, with each declaration's omitted capabilities
//! and the last `describe`. A local-only catalog row served by the engine's
//! local service; it starts nothing.

use super::*;

pub(super) fn handles(method: &str) -> bool {
    method == methods::LIST_EXTENSION_PROVIDERS
}

impl EngineRpc {
    pub(super) fn handle_extension_providers(&self, params: &Value) -> Result<RpcReply, RpcError> {
        if params
            .get("targetDeviceId")
            .and_then(Value::as_str)
            .is_some_and(|id| id != self.engine_info.device_id)
        {
            return Err(RpcError::Failed(
                "Read extension providers on the device that installs them".into(),
            ));
        }
        let manager = self
            .extensions
            .clone()
            .ok_or_else(|| RpcError::Failed("Extensions are unavailable on this engine".into()))?;
        RpcReply::value(&manager.provider_snapshot())
    }
}
