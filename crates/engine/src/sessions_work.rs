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
        self.inner
            .runs
            .lock()
            .get(chat)
            .is_some_and(|run| run.work_observations.contains(&format!("{kind}:{id}")))
    }
    pub(crate) fn work_runtime(&self, chat: &str) -> Option<WorkRuntime> {
        self.inner.runs.lock().get(chat).map(|run| WorkRuntime {
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
        let matches = self.inner.runs.lock().get(chat).is_some_and(|run| {
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
    /// Whether this provider's adapter can stop one native child of a live
    /// run (its own capability, not a provider list), which decides whether
    /// the work panel offers per-child Stop.
    pub(crate) fn native_child_stop_supported(&self, provider: HarnessId) -> bool {
        self.inner
            .registry
            .resolve(provider)
            .is_ok_and(|harness| harness.supports_native_child_stop())
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
    /// The authority shape every control evidence needs: the claimed caller on
    /// this host, only the automatic read grant, one project, another task.
    pub(super) fn control_evidence_authority(
        &self,
        claim: &ClaimedProviderCall,
        authority: &crate::cross_task_tools::CrossTaskAuthority,
        target_task_id: &str,
    ) -> bool {
        authority.caller_task_id == claim.source_task_id
            && authority.local_environment_id == self.inner.device_id
            && authority.delivery.attaches_luna_tools()
            && !authority.explicit_user_request
            && !authority.allow_projectless
            // Only the automatic read grant; the board scopes never reach
            // another chat (board tools check them on their own path).
            && authority
                .granted_scopes
                .iter()
                .filter(|scope| !scope.starts_with("board."))
                .count()
                == 1
            && authority.granted_scopes.contains("threads.read")
            && authority.visible_project_ids.len() == 1
            && target_task_id != claim.source_task_id
    }
    /// Stop evidence for a claimed call: the authority shape and the target's
    /// project/host scope of `prepare_luna_control_evidence`, without its
    /// two-read snapshot equality. That equality fences a send's commit to
    /// the target's revision; a stop commits through the work view's run
    /// fence instead, so a busy target's commands, transcript or read state
    /// must not refuse it. The returned binding generation is the identity
    /// the stop plan must still find.
    async fn prepare_luna_stop_evidence(
        &self,
        claim: &ClaimedProviderCall,
        authority: &crate::cross_task_tools::CrossTaskAuthority,
        target_task_id: &str,
    ) -> Result<Option<u32>, crate::cross_task_tools::CrossTaskError> {
        use crate::cross_task_tools::{CrossTaskError, CrossTaskTools};
        if !self.control_evidence_authority(claim, authority, target_task_id) {
            return Err(CrossTaskError::TargetScope);
        }
        let (interrupt, mut cancel) = self
            .claimed_call_cancel_state(claim)
            .ok_or(CrossTaskError::Unavailable)?;
        let tools = CrossTaskTools::new(
            self.cross_task_backend()
                .ok_or(CrossTaskError::Unavailable)?,
        );
        let snapshot = tokio::select! {
            biased;
            _ = interrupt.cancelled() => return Err(CrossTaskError::Cancelled),
            _ = cancel.wait_for(|value| *value) => return Err(CrossTaskError::Cancelled),
            result = tools.control_target_snapshot(authority, target_task_id, &interrupt) => result,
        }?;
        if !self.claimed_call_is_live(claim) {
            return Err(CrossTaskError::Unavailable);
        }
        Ok(snapshot.binding_generation)
    }
    /// `interrupt_thread` from a provider call. The call is claimed once for
    /// this run and the target passes the existing control-evidence scope;
    /// the claim must still be live when the stop intent becomes durable.
    /// Launch ownership and the receipt belong to the host's work view.
    pub(super) async fn invoke_luna_thread_stop(
        &self,
        chat_id: &str,
        run_id: &str,
        contract: &luna_proto::LunaAgentRuntimeContract,
        invocation: luna_harness::LunaToolInvocation,
        backend: Option<Arc<dyn crate::cross_task_tools::CrossTaskBackend>>,
    ) -> Result<serde_json::Value, EngineError> {
        use crate::cross_task_tools::{
            CrossTaskAuthority, CrossTaskError, CrossTaskTools, ThreadStopRequest,
        };
        let fail = |error: CrossTaskError| EngineError::Other(error.to_string());
        let Some(tool) = contract
            .tools
            .iter()
            .find(|tool| tool.name == invocation.name)
        else {
            return Err(fail(CrossTaskError::Unavailable));
        };
        if contract.context.thread_id != chat_id
            || !contract.delivery.attaches_luna_tools()
            || !tool
                .required_scopes
                .iter()
                .all(|scope| contract.context.granted_scopes.contains(scope))
        {
            return Err(fail(CrossTaskError::Scope));
        }
        let request: ThreadStopRequest = serde_json::from_value(invocation.arguments.clone())
            .map_err(|_| fail(CrossTaskError::Input))?;
        let backend = backend.ok_or_else(|| fail(CrossTaskError::Unavailable))?;
        let authority =
            CrossTaskAuthority::from_context(&contract.context, contract.delivery, false);
        let claim = self
            .wait_and_claim_luna_provider_invocation(
                chat_id,
                run_id,
                &invocation.context,
                tokio::time::Instant::now() + std::time::Duration::from_millis(500),
            )
            .await
            .ok_or_else(|| fail(CrossTaskError::Unavailable))?;
        let binding_generation = self
            .prepare_luna_stop_evidence(&claim, &authority, &request.task_id)
            .await
            .map_err(fail)?;
        let source_live = || self.claimed_call_is_live(&claim);
        let result = CrossTaskTools::new(backend)
            .stop_thread(&authority, request, binding_generation, &source_live)
            .await
            .map_err(fail)?;
        serde_json::to_value(result).map_err(Into::into)
    }
}

impl Inner {
    /// Native work of this run is still live: an unsettled native child
    /// (subagent chip) or an active native task. After the root turn's Done
    /// this is background work, whose own requests are real.
    pub(super) fn has_live_native_work(&self, chat: &str, run_id: &str) -> bool {
        self.runs
            .lock()
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
        if let Some(run) = self
            .runs
            .lock()
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
