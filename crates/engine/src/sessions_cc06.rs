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
//! - Known limit: the "LUNA context" section appended at launch keeps the
//!   launch model/reasoning until the next restart (the CLI's own system
//!   prompt follows the switched model).

use luna_proto::{HarnessId, RunRequest};

use super::{RuntimeConfig, SessionsEngine, lock};

/// The LUNA runtime contract embeds `context.model/reasoning` (and the
/// "LUNA context" instruction section rendered from that context), so a model
/// change alters its hash. For a live switch those derived parts are the only
/// permitted difference: environment, tools, delivery, every other context
/// field and every other instruction section must still be identical.
fn contract_without_selection(value: &serde_json::Value) -> serde_json::Value {
    let Ok(mut contract) =
        serde_json::from_value::<luna_proto::LunaAgentRuntimeContract>(value.clone())
    else {
        return value.clone();
    };
    contract.content_hash.clear();
    contract.context.model = None;
    contract.context.reasoning = None;
    contract
        .instructions
        .retain(|section| section.source != luna_proto::RuntimeInstructionSource::Context);
    serde_json::to_value(contract).unwrap_or_else(|_| value.clone())
}

impl RuntimeConfig {
    /// True when `next` differs from this live runtime only in what a native
    /// live switch can change. Everything else (account, cwd, sandbox,
    /// approvals, policy, worktree, auth instance, the runtime contract apart
    /// from its model/reasoning context, every other model option) must be
    /// identical.
    pub(super) fn differs_only_in_live_selection(&self, next: &RuntimeConfig) -> bool {
        let others = |options: &serde_json::Map<String, serde_json::Value>| {
            options
                .iter()
                .filter(|(key, _)| {
                    !luna_harness::claude::LIVE_SWITCH_OPTIONS.contains(&key.as_str())
                })
                .map(|(key, value)| {
                    let value = if key == luna_proto::LUNA_RUNTIME_CONTRACT_OPTION {
                        contract_without_selection(value)
                    } else {
                        value.clone()
                    };
                    (key.clone(), value)
                })
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

    /// Only the switchable selection goes to the adapter; the engine already
    /// proved everything else unchanged.
    pub(super) fn live_selection(&self) -> luna_harness::LiveRuntimeSelection {
        luna_harness::LiveRuntimeSelection {
            model: self.model.clone(),
            reasoning: self.reasoning,
            model_options: self
                .model_options
                .iter()
                .filter(|(key, _)| {
                    luna_harness::claude::LIVE_SWITCH_OPTIONS.contains(&key.as_str())
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        }
    }
}

impl SessionsEngine {
    /// Per-engine live accounting for the shared Usage page.
    pub fn live_usage(&self) -> &crate::live_usage::LiveUsageLedger {
        &self.inner.live_usage
    }

    /// Retained accounting is not proof that a provider still runs. Filter
    /// the outgoing live projection, preserving the ledger and warm idle feeds.
    pub(crate) fn retain_owned_live_usage_rows(
        &self,
        rows: &mut Vec<luna_proto::LiveSessionUsage>,
    ) {
        use luna_proto::UsageProvider;
        let runs = lock(&self.inner.runs);
        rows.retain(|row| {
            let Some(run) = runs.get(&row.task_id) else {
                return false;
            };
            let provider_matches = matches!(
                (run.runtime_config.harness_id, row.provider),
                (HarnessId::Codex, UsageProvider::Codex)
                    | (HarnessId::ClaudeCode, UsageProvider::Claude)
                    | (HarnessId::Opencode, UsageProvider::OpenCodeGo)
            );
            let session = run
                .run_provider_binding
                .as_ref()
                .map(|binding| binding.provider_thread_id.as_str())
                .or(run.observed_provider_session_id.as_deref());
            provider_matches
                && session.is_some()
                && row.native_session_id.as_deref() == session
                && !run.interrupt_token.is_cancelled()
                && !*run.cancel.borrow()
                && !run
                    .idle_stream_closed
                    .load(std::sync::atomic::Ordering::Acquire)
                && (!run.steerable || !run.steer_tx.is_closed())
        });
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
        if !luna_harness::Harness::stage_live_reconfiguration(
            harness.as_ref(),
            message_id,
            &from,
            &next.live_selection(),
        ) {
            return false;
        }
        let execution_defaults = self.execution_defaults(chat_id);
        {
            let mut runs = lock(&self.inner.runs);
            let Some(handle) = runs
                .get_mut(chat_id)
                .filter(|handle| handle.run_id == run_id)
            else {
                return false;
            };
            handle.runtime_config = next;
            handle.execution.staged_defaults = execution_defaults;
        }
        lock(&self.inner.last_requests).insert(chat_id.to_owned(), request.clone());
        tracing::info!(chat = %chat_id, "routing follow-up through a native live model/effort switch");
        true
    }
}
