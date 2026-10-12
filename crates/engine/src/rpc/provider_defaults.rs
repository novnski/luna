use super::*;

impl EngineRpc {
    pub(super) async fn handle_provider_defaults(
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
                "Open Providers on the owning device to change its agent defaults".into(),
            ));
        }
        if let Some(object) = params.as_object_mut() {
            object.remove("targetDeviceId");
        }
        let service = self.provider_defaults.as_ref().ok_or_else(|| {
            RpcError::Failed("Provider defaults are unavailable on this engine".into())
        })?;
        let snapshot = if method == methods::SET_PROVIDER_DEFAULTS {
            let request: luna_proto::SetProviderDefaultsRequest = parse_params(params)?;
            if !matches!(
                (request.provider).kind(),
                luna_proto::BuiltinHarnessId::Codex
                    | luna_proto::BuiltinHarnessId::ClaudeCode
                    | luna_proto::BuiltinHarnessId::Opencode
            ) {
                return Err(RpcError::BadParams(
                    "Choose an available agent provider".into(),
                ));
            }
            if let Some(selection) = &request.defaults {
                if !self.registry.descriptors().iter().any(|entry| {
                    entry.id == request.provider
                        && entry.installed
                        && crate::registry::descriptor_enabled(entry)
                }) {
                    return Err(RpcError::Failed(
                        "The provider is not installed or enabled on this device".into(),
                    ));
                }
                let models = self
                    .model_cache
                    .list(request.provider, None, false)
                    .await
                    .map_err(RpcError::Failed)?;
                crate::provider_defaults::validate_selection(selection, &models)
                    .map_err(RpcError::BadParams)?;
            }
            service.set(request)
        } else {
            service.get()
        }
        .map_err(|error| RpcError::Failed(error.to_string()))?;
        RpcReply::value(&snapshot)
    }
}
