//! CC-06 engine hooks for `sessions` (a child module so it can read the run
//! registry without widening `sessions.rs` types).
//!
//! - Live accounting: `AgentEvent::ProviderAccounting` is recorded in the
//!   per-engine [`crate::live_usage::LiveUsageLedger`] and never journaled or
//!   folded into a transcript.
//! - Live model/effort switch: a PARKED warm Claude run whose follow-up differs
//!   only in model / Low..Max effort / contextWindow / fastMode / thinking is
//!   routed warm after the adapter staged a native switch. The adapter writes
//!   the prompt only after acks + `get_settings` readback; on failure it ends
//!   the run without writing, and the existing orphan path re-dispatches the
//!   same message as a fresh run — so `last_requests` must already hold the
//!   new configuration.

use luna_proto::{HarnessId, RunRequest};

use super::{RuntimeConfig, SessionsEngine, lock};

impl RuntimeConfig {
    /// True when `next` differs from this live runtime only in what a native
    /// live switch can change. Everything else (account, cwd, sandbox,
    /// approvals, policy, worktree, runtime contract, auth instance, every
    /// other model option) must be identical.
    fn differs_only_in_live_selection(&self, next: &RuntimeConfig) -> bool {
        let others = |options: &serde_json::Map<String, serde_json::Value>| {
            options
                .iter()
                .filter(|(key, _)| {
                    !luna_harness::claude::LIVE_SWITCH_OPTIONS.contains(&key.as_str())
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<serde_json::Map<_, _>>()
        };
        self.harness_id == next.harness_id
            && self.provider_account_id == next.provider_account_id
            && self.cwd == next.cwd
            && self.sandbox == next.sandbox
            && self.auto_approve == next.auto_approve
            && self.permission_policy == next.permission_policy
            && self.worktree == next.worktree
            && others(&self.model_options) == others(&next.model_options)
            && self != next
    }

    fn live_selection(&self) -> luna_harness::LiveRuntimeSelection {
        luna_harness::LiveRuntimeSelection {
            model: self.model.clone(),
            reasoning: self.reasoning,
            model_options: self.model_options.clone(),
        }
    }
}

impl SessionsEngine {
    /// Per-engine live accounting for the shared Usage page.
    pub fn live_usage(&self) -> &crate::live_usage::LiveUsageLedger {
        &self.inner.live_usage
    }

    /// Stage a native live switch for `message_id` on the parked run
    /// `run_id`. Returns true only when the adapter accepted the plan; the
    /// handle's runtime config and the re-dispatch request then describe the
    /// NEW configuration.
    pub(super) fn stage_live_switch(
        &self,
        chat_id: &str,
        run_id: &str,
        harness_id: HarnessId,
        request: &RunRequest,
        message_id: &str,
        workspace_repair_revision: u64,
    ) -> bool {
        if harness_id != HarnessId::ClaudeCode
            || request.review.is_some()
            || !request.attachments.is_empty()
            || self.turn_in_flight(chat_id)
            || self.active_turn_id(chat_id).is_some()
        {
            return false;
        }
        let next = RuntimeConfig::from_request(harness_id, request);
        let from = {
            let runs = lock(&self.inner.runs);
            let Some(handle) = runs.get(chat_id).filter(|handle| {
                handle.run_id == run_id
                    && handle.steerable
                    && handle.workspace_repair_revision == workspace_repair_revision
            }) else {
                return false;
            };
            if !handle.runtime_config.differs_only_in_live_selection(&next) {
                return false;
            }
            handle.runtime_config.live_selection()
        };
        let Ok(harness) = self.inner.registry.resolve(harness_id) else {
            return false;
        };
        if !harness.stage_live_reconfiguration(message_id, &from, &next.live_selection()) {
            return false;
        }
        {
            let mut runs = lock(&self.inner.runs);
            let Some(handle) = runs
                .get_mut(chat_id)
                .filter(|handle| handle.run_id == run_id)
            else {
                return false;
            };
            handle.runtime_config = next;
        }
        lock(&self.inner.last_requests).insert(chat_id.to_owned(), request.clone());
        tracing::info!(chat = %chat_id, "routing follow-up through a native live model/effort switch");
        true
    }
}
