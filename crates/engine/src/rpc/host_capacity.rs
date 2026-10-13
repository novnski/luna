//! AW-10 host turn capacity: `GetHostCapacity` and `SetHostCapacity`. Both
//! are local-only catalog rows (owner IPC and the in-process app, never a
//! paired session) answered by the engine that runs the turns, and never
//! forwarded to another device. The line itself is owned by the run table.

use super::*;

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::GET_HOST_CAPACITY | methods::SET_HOST_CAPACITY
    )
}

impl EngineRpc {
    pub(super) fn handle_host_capacity(
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
                "Host capacity is read and changed on the device that runs the engine".into(),
            ));
        }
        match method {
            methods::GET_HOST_CAPACITY => RpcReply::value(&self.sessions.host_capacity()),
            methods::SET_HOST_CAPACITY => {
                let request: luna_proto::SetHostCapacityRequest = serde_json::from_value(params)
                    .map_err(|error| RpcError::Failed(format!("Invalid host capacity: {error}")))?;
                let capacity = self
                    .sessions
                    .set_host_turn_limit(request.limit)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&capacity)
            }
            other => Err(RpcError::UnknownMethod(other.to_string())),
        }
    }
}
