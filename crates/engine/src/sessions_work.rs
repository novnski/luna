//! Exact live work control fences, separate from ancestry and native IDs.
use super::*;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct WorkRuntime {
    pub run_id: String,
    pub message_id: String,
    pub provider: HarnessId,
    pub native_session_id: Option<String>,
    pub binding_generation: Option<u32>,
}
impl SessionsEngine {
    pub(crate) fn work_observed(&self, chat: &str, kind: &str, id: &str) -> bool {
        lock(&self.inner.runs)
            .get(chat)
            .is_some_and(|run| run.work_observations.contains(&format!("{kind}:{id}")))
    }
    pub(crate) fn work_runtime(&self, chat: &str) -> Option<WorkRuntime> {
        lock(&self.inner.runs).get(chat).map(|run| WorkRuntime {
            run_id: run.run_id.clone(),
            message_id: run.work_message_id.clone(),
            provider: run.runtime_config.harness_id,
            native_session_id: run.observed_provider_session_id.clone(),
            binding_generation: self.task_binding_generation(chat),
        })
    }
    pub(crate) fn work_origin(&self, chat: &str, message: &str) -> luna_proto::WorkOrigin {
        let inherited = self
            .inner
            .workspace()
            .and_then(|workspace| workspace.chat(chat).ok().flatten())
            .and_then(|row| row.child_thread)
            .filter(|child| child.initial_message_id == message)
            .and_then(|child| child.work_origin);
        luna_proto::WorkOrigin {
            owner_task_id: inherited
                .as_ref()
                .map_or_else(|| chat.into(), |o| o.owner_task_id.clone()),
            owner_turn_id: inherited
                .as_ref()
                .map_or_else(|| message.into(), |o| o.owner_turn_id.clone()),
            launch_task_id: chat.into(),
            launch_turn_id: message.into(),
        }
    }
    pub(crate) async fn stop_work_turn(
        &self,
        chat: &str,
        expected: &WorkRuntime,
    ) -> Result<bool, EngineError> {
        // Dispatch and warm steering use this same lane. A later message cannot
        // sneak into a reused native process between the fence and interruption.
        let _transition = self.transition_gate(chat).lock_owned().await;
        let matches = lock(&self.inner.runs).get(chat).is_some_and(|run| {
            run.run_id == expected.run_id && run.work_message_id == expected.message_id
        });
        let matches = matches && self.task_binding_generation(chat) == expected.binding_generation;
        if !matches {
            return Ok(false);
        }
        // This control has its own durable receipt, not a TaskCommand Stop
        // projection. Use the interruption path that holds recovery itself;
        // otherwise the delegated request could recover after being stopped.
        self.interrupt(chat).await
    }
    pub(crate) async fn stop_work_native(
        &self,
        chat: &str,
        expected: &WorkRuntime,
        tool_id: &str,
    ) -> Result<luna_harness::NativeTaskStop, EngineError> {
        let _transition = self.transition_gate(chat).lock_owned().await;
        let runtime = self.work_runtime(chat).filter(|r| {
            r.run_id == expected.run_id
                && r.message_id == expected.message_id
                && r.native_session_id == expected.native_session_id
                && r.binding_generation == expected.binding_generation
        });
        let Some(runtime) = runtime else {
            return Ok(luna_harness::NativeTaskStop::Rejected(
                "The launching runtime is no longer live".into(),
            ));
        };
        Ok(self
            .inner
            .registry
            .resolve(runtime.provider)?
            .stop_native_child(
                expected.native_session_id.as_deref().unwrap_or_default(),
                tool_id,
            )
            .await?)
    }
}

impl Inner {
    /// Native work of this run is still live: an unsettled native child
    /// (subagent chip) or an active native task. After the root turn's Done
    /// this is background work, whose own requests are real.
    pub(super) fn has_live_native_work(&self, chat: &str, run_id: &str) -> bool {
        lock(&self.runs)
            .get(chat)
            .filter(|run| run.run_id == run_id)
            .is_some_and(|run| !run.work_observations.is_empty())
    }

    pub(super) fn note_work_observation(
        &self,
        chat: &str,
        run_id: &str,
        kind: &str,
        id: &str,
        active: bool,
    ) {
        if let Some(run) = lock(&self.runs)
            .get_mut(chat)
            .filter(|run| run.run_id == run_id)
        {
            let key = format!("{kind}:{id}");
            if !active {
                run.work_observations.remove(&key);
            } else if run.work_observations.len() < 256 {
                run.work_observations.insert(key);
            }
        }
    }
}
