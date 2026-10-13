//! RM-03 `WatchRemoteTasks` and `WatchRemoteTask`: the engine's
//! host-authoritative task projections (`crate::remote_projections`). Remote
//! sessions reach them with the `tasks-read` scope through the per-socket
//! guard; both serve only this engine's own tasks, never another device's.

use super::*;
use luna_proto::remote_projection::{RemoteTaskParams, RemoteTasksParams};

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::WATCH_REMOTE_TASKS | methods::WATCH_REMOTE_TASK
    )
}

impl EngineRpc {
    pub(crate) fn with_remote_projections(
        mut self,
        projections: std::sync::Arc<crate::remote_projections::RemoteProjections>,
    ) -> Self {
        self.remote_projections = Some(projections);
        self
    }

    pub(super) fn handle_remote_projection(
        &self,
        method: &str,
        params: Value,
    ) -> Result<RpcReply, RpcError> {
        let projections = self.remote_projections.clone().ok_or_else(|| {
            RpcError::Failed("Remote task projections are unavailable on this engine".into())
        })?;
        if method == methods::WATCH_REMOTE_TASKS {
            let params: RemoteTasksParams = parse_params(params)?;
            return Ok(RpcReply::Stream(projections.watch_tasks(params.after)));
        }
        let params: RemoteTaskParams = parse_params(params)?;
        Ok(RpcReply::Stream(
            projections.watch_task(params.task_id, params.after)?,
        ))
    }
}
