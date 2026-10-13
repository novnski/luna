//! AW-09 read-only diagnostics: `GetEngineDiagnostics` and
//! `GetStorageUsage`. Both are local-only catalog rows (owner IPC and the
//! in-process app, never a remote session) served only by the engine's local
//! service, and never forwarded to another device. Neither writes anything.

use super::*;

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::GET_ENGINE_DIAGNOSTICS | methods::GET_STORAGE_USAGE
    )
}

impl EngineRpc {
    /// Attach this engine's diagnostics (its local service only).
    pub(crate) fn with_diagnostics(mut self, diagnostics: crate::diagnostics::Diagnostics) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    pub(super) async fn handle_diagnostics(
        &self,
        method: &str,
        params: Value,
    ) -> Result<RpcReply, RpcError> {
        if params
            .get("targetDeviceId")
            .and_then(Value::as_str)
            .is_some_and(|id| id != self.engine_info.device_id)
        {
            return Err(RpcError::Failed(
                "Diagnostics are read on the device that runs the engine".into(),
            ));
        }
        let diagnostics = self.diagnostics.clone().ok_or_else(|| {
            RpcError::Failed("Diagnostics are unavailable on this connection".into())
        })?;
        match method {
            methods::GET_ENGINE_DIAGNOSTICS => RpcReply::value(&diagnostics.engine()),
            methods::GET_STORAGE_USAGE => {
                let usage = diagnostics
                    .storage_usage()
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&usage)
            }
            other => Err(RpcError::UnknownMethod(other.to_string())),
        }
    }
}
