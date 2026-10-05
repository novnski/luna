//! Owning-host execution observations and the shared pending-settings guard.
//! These snapshots never grant permission to run a provider operation.

use luna_proto::{
    Chat, ChatConfig, EXECUTION_SUMMARY_VERSION, ExecutionPhase, ExecutionSettingField as Field,
    ExecutionSettings, ExecutionSteerDecision, ExecutionSteerKind, ExecutionTransition,
    ExecutionTransitionKind as Transition, ProviderExecutionCapabilities, SteeringMode,
    TaskExecutionSummary,
};

use super::{EngineError, RunHandle, RuntimeConfig, SessionsEngine, lock};
use luna_proto::{HarnessId, PermissionMode, SandboxLevel};

#[derive(Default)]
pub(super) struct RunExecutionObservation {
    pub defaults: Option<ExecutionSettings>,
    pub staged_defaults: Option<ExecutionSettings>,
    pub confirmed: Option<RuntimeConfig>,
    pub observed_model: Option<String>,
}

fn private_option(key: &str) -> bool {
    key.starts_with("__") || key.starts_with("luna")
}

fn public_options(
    options: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    options
        .iter()
        .filter(|(key, _)| !private_option(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn requested(config: &ChatConfig) -> ExecutionSettings {
    ExecutionSettings {
        provider: config.harness,
        provider_account_id: config.provider_account_id.clone(),
        model: config.model.clone(),
        reasoning: config.reasoning,
        model_options: public_options(&config.model_options),
        permission_mode: config.permission_mode,
        sandbox: config.sandbox,
    }
}

fn admitted(config: &RuntimeConfig) -> ExecutionSettings {
    ExecutionSettings {
        provider: config.harness_id,
        provider_account_id: config.provider_account_id.clone(),
        model: config.model.clone(),
        reasoning: config.reasoning,
        model_options: public_options(&config.model_options),
        permission_mode: config
            .permission_policy
            .as_ref()
            .map(|policy| policy.effective_mode)
            .unwrap_or_default(),
        sandbox: config.sandbox,
    }
}

fn pending(chat: &Chat, run: &RunHandle) -> Vec<Field> {
    let Some(config) = chat.config.as_ref() else {
        return Vec::new();
    };
    let now = requested(config);
    // Saved defaults retain the difference between an inherited value and
    // its resolution, so a default account/model is not a pending switch.
    let Some(before) = run.execution.defaults.as_ref() else {
        return Vec::new();
    };
    let mut changed = Vec::new();
    if now.provider != before.provider {
        changed.push(Field::Provider);
    }
    if now.provider_account_id != before.provider_account_id {
        changed.push(Field::Account);
    }
    if now.model != before.model {
        changed.push(Field::Model);
    }
    if now.reasoning != before.reasoning {
        changed.push(Field::Reasoning);
    }
    if now.model_options != before.model_options {
        changed.push(Field::ModelOptions);
    }
    if now.permission_mode != before.permission_mode || now.sandbox != before.sandbox {
        changed.push(Field::Permission);
    }
    if chat
        .cwd
        .as_deref()
        .is_some_and(|cwd| super::expand_home(cwd) != run.runtime_config.cwd)
    {
        changed.push(Field::Workspace);
    }
    changed
}

impl SessionsEngine {
    /// Resolve local admission without contacting a provider or interrupting a
    /// turn. Dispatch repeats this under its account and task transition gates.
    pub(crate) fn prepare_execution_request(
        &self,
        chat_id: &str,
        harness_id: HarnessId,
        request: &mut luna_proto::RunRequest,
    ) -> Result<(), EngineError> {
        let mut prepare = || -> Result<(), EngineError> {
            crate::provider_inputs::admit(harness_id, &request.attachments, &request.input_items)?;
            self.inner.registry.resolve(harness_id)?;
            request.cwd = super::expand_home(&request.cwd);
            if matches!(harness_id, HarnessId::ClaudeCode | HarnessId::Codex)
                && let Some(accounts) = self.inner.agent_accounts.get()
            {
                let execution = accounts
                    .resolve_execution(harness_id, request.provider_account_id.as_deref())?;
                request.provider_account_id = Some(execution.account_id);
                request.model_options.insert(
                    luna_harness::LUNA_PROVIDER_AUTH_INSTANCE_OPTION.into(),
                    execution.auth_instance_id.into(),
                );
                if let Some(home) = execution.codex_home {
                    let home = home.to_str().ok_or_else(|| {
                        EngineError::Other("Codex account home is not valid UTF-8".into())
                    })?;
                    request.model_options.insert(
                        luna_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
                        home.into(),
                    );
                }
                if execution.codex_file_auth {
                    request.model_options.insert(
                        luna_harness::codex::LUNA_CODEX_FILE_AUTH_OPTION.into(),
                        true.into(),
                    );
                }
            }
            self.apply_execution_policy(chat_id, harness_id, request)
        };
        prepare().map_err(|error| EngineError::DispatchPreflight(error.to_string()))
    }

    /// The exact dispatch policy resolver, also used by read-only previews.
    pub(super) fn apply_execution_policy(
        &self,
        chat_id: &str,
        harness_id: luna_proto::HarnessId,
        request: &mut luna_proto::RunRequest,
    ) -> Result<(), EngineError> {
        if !crate::permissions::provider_can_run(harness_id) {
            return Err(EngineError::Other(format!(
                "{} permission enforcement is unavailable; choose a provider with a verified LUNA permission contract",
                format!("{harness_id:?}").to_lowercase()
            )));
        }
        let child = self
            .inner
            .workspace()
            .and_then(|workspace| workspace.chat(chat_id).ok().flatten())
            .and_then(|chat| chat.child_thread);
        if let Some(child) = &child {
            let ceiling = self.child_permission_ceiling(child);
            request.parent_permission_ceiling = Some(
                request
                    .parent_permission_ceiling
                    .map_or(ceiling, |requested| std::cmp::min(requested, ceiling)),
            );
        }
        // A migrated fork retains its own legacy read-only sandbox until an
        // explicit permission change. Source changes never alter this thread.
        let side_sandbox = self
            .inner
            .side_chat_lookup
            .get()
            .filter(|lookup| lookup(chat_id))
            .map(|_| request.sandbox)
            .or_else(|| child.as_ref().and_then(|child| child.sandbox_ceiling));
        if side_sandbox == Some(SandboxLevel::ReadOnly) {
            if !matches!(harness_id, HarnessId::Codex | HarnessId::Mock) {
                return Err(EngineError::Other(
                    "This provider cannot preserve this thread's read-only sandbox".into(),
                ));
            }
            request.permission_mode = PermissionMode::Ask;
        }
        let mut permission_policy = crate::permissions::resolve_run_policy(harness_id, request);
        if let Some(sandbox) = side_sandbox {
            if sandbox == SandboxLevel::ReadOnly
                || (sandbox == SandboxLevel::WorkspaceWrite
                    && request.sandbox == SandboxLevel::DangerFullAccess)
            {
                request.sandbox = sandbox;
            }
        }
        if harness_id == HarnessId::Codex && request.sandbox == SandboxLevel::ReadOnly {
            // A legacy child/side-chat read-only ceiling also keeps Codex's
            // read-only network policy; do not report workspace networking.
            permission_policy.network_policy = luna_proto::PermissionNetworkPolicy::Restricted;
            request.permission_policy = Some(permission_policy.clone());
        }
        if crate::permissions::hidden_ask_fallback(&permission_policy) {
            return Err(EngineError::Other(
                "requested permission mode would fall back to hidden Ask; choose an available Approve for me or Full access mode"
                    .into(),
            ));
        }
        Ok(())
    }

    fn pending_execution_fields(&self, chat: &Chat, run: &RunHandle) -> Vec<Field> {
        let mut fields = pending(chat, run);
        if let Some(child) = &chat.child_thread
            && let Some(policy) = &run.runtime_config.permission_policy
            && self.child_permission_ceiling(child) < policy.effective_mode
            && !fields.contains(&Field::Permission)
        {
            fields.push(Field::Permission);
        }
        fields
    }

    pub(super) fn execution_defaults(&self, chat_id: &str) -> Option<ExecutionSettings> {
        self.inner
            .workspace()?
            .chat(chat_id)
            .ok()
            .flatten()?
            .config
            .as_ref()
            .map(requested)
    }

    /// Called under the same per-task transition gate as native steering.
    pub(super) fn pending_execution_change(&self, chat_id: &str) -> Result<bool, EngineError> {
        if let Some(host) = self.inner.doc_host()
            && host.has_pending_context_transfer(chat_id)?
        {
            return Ok(true);
        }
        let Some(workspace) = self.inner.workspace() else {
            return Ok(false);
        };
        let Some(chat) = workspace.chat(chat_id)? else {
            return Ok(false);
        };
        Ok(lock(&self.inner.runs)
            .get(chat_id)
            .is_some_and(|run| !self.pending_execution_fields(&chat, run).is_empty()))
    }

    /// No provider is started or contacted. Adapter facts are declarations;
    /// native operation admission and readback remain authoritative.
    pub fn execution_summary(
        &self,
        chat_id: &str,
    ) -> Result<Option<TaskExecutionSummary>, EngineError> {
        let Some(workspace) = self.inner.workspace() else {
            return Ok(None);
        };
        if !workspace.is_host(chat_id) {
            return Ok(None);
        }
        let Some(chat) = workspace.chat(chat_id)? else {
            return Ok(None);
        };
        let Some(config) = chat.config.as_ref() else {
            return Ok(None);
        };
        let mut selected = requested(config);
        let adapter = self.inner.registry.resolve(config.harness).ok();
        let binding = self
            .inner
            .task_identity
            .get()
            .and_then(|identity| identity.view(chat_id))
            .and_then(|view| view.current_binding);
        let active = self.turn_in_flight(chat_id);
        let observed = {
            let runs = lock(&self.inner.runs);
            runs.get(chat_id)
                .filter(|run| {
                    !run.interrupt_token.is_cancelled()
                        && !*run.cancel.borrow()
                        && !run
                            .idle_stream_closed
                            .load(std::sync::atomic::Ordering::Acquire)
                })
                .map(|run| {
                    (
                        run.runtime_config.clone(),
                        run.execution.confirmed.clone(),
                        run.execution.observed_model.clone(),
                        self.pending_execution_fields(&chat, run),
                        run.steerable && !run.steer_tx.is_closed(),
                    )
                })
        };
        let resolved_model = config.model.as_deref().or_else(|| {
            observed
                .as_ref()
                .filter(|(runtime, ..)| runtime.harness_id == config.harness)
                .and_then(|(_, _, model, ..)| model.as_deref())
        });
        let mut preview = crate::permissions::preview_request_for(
            config.harness,
            resolved_model,
            config.permission_mode,
        );
        preview.cwd = super::expand_home(chat.cwd.as_deref().unwrap_or("."));
        preview.sandbox = config.sandbox;
        let policy_error = self
            .apply_execution_policy(chat_id, config.harness, &mut preview)
            .err();
        selected.sandbox = preview.sandbox;
        let capabilities = adapter
            .as_ref()
            .map(|adapter| ProviderExecutionCapabilities {
                provider: adapter.id(),
                steering: adapter.steering_mode(),
                supports_steering: adapter.supports_steering(),
                authoritative_prompt_end: adapter.authoritative_prompt_end(),
                deterministic_turn_end: adapter.deterministic_turn_end(),
                safe_recovery_continuation: adapter.supports_safe_recovery_continuation(),
                native_fork: adapter.fork_capability(),
                runtime_delivery: adapter.luna_runtime_delivery(),
                permissions: crate::permissions::provider_capabilities_for(
                    config.harness,
                    resolved_model,
                ),
            });
        let (phase, running, changed, mut transition, mut steer, note) = if let Some((
            runtime,
            confirmed,
            observed_model,
            changed,
            reusable,
        )) = observed
        {
            let awaiting_settings = confirmed.as_ref().is_some_and(|config| config != &runtime);
            let phase = if confirmed.is_none() || awaiting_settings {
                ExecutionPhase::Starting
            } else if active {
                ExecutionPhase::Running
            } else {
                ExecutionPhase::Warm
            };
            let running = confirmed.as_ref().map(|confirmed| {
                let mut settings = admitted(confirmed);
                if observed_model.is_some() {
                    settings.model = observed_model.clone();
                }
                settings
            });
            let identity_change = changed
                .iter()
                .any(|field| matches!(field, Field::Provider | Field::Account | Field::Workspace));
            let mut desired = runtime.clone();
            desired.model = selected.model.clone();
            desired.reasoning = selected.reasoning;
            desired.model_options.retain(|key, _| private_option(key));
            desired.model_options.extend(selected.model_options.clone());
            let live_selection = !changed.is_empty()
                && changed.iter().all(|field| {
                    matches!(field, Field::Model | Field::Reasoning | Field::ModelOptions)
                })
                && runtime.differs_only_in_live_selection(&desired)
                && adapter.as_ref().is_some_and(|adapter| {
                    adapter
                        .can_reconfigure_live(&runtime.live_selection(), &desired.live_selection())
                });
            let (kind, reason) = if awaiting_settings {
                (
                    Transition::ApplyNextTurn,
                    "A native settings change is awaiting acknowledgment. The last confirmed selection remains authoritative.",
                )
            } else if changed.is_empty() && reusable {
                (
                    Transition::Reuse,
                    "The current runtime configuration matches the saved settings.",
                )
            } else if identity_change {
                (
                    Transition::NewBinding,
                    "Provider, account or workspace changes require a new compatible binding. Context transfer is separate and explicit.",
                )
            } else if live_selection {
                (
                    Transition::ApplyNextTurn,
                    "The adapter can apply this selection at the next turn boundary; native acknowledgment is required before sending.",
                )
            } else {
                (
                    Transition::RestartAndResume,
                    "The current runtime must retire before the next turn can use these settings and resume the provider conversation.",
                )
            };
            let steer = if confirmed.is_none() || awaiting_settings {
                ExecutionSteerDecision { kind: ExecutionSteerKind::Unavailable, reason: Some("The provider is still starting or confirming its settings. Wait for confirmation before steering.".into()) }
            } else if !changed.is_empty() {
                ExecutionSteerDecision { kind: ExecutionSteerKind::Unavailable, reason: Some("Settings are pending. Wait for this turn to finish, or stop it, before sending with the new settings.".into()) }
            } else if active
                && reusable
                && capabilities.as_ref().is_some_and(|caps| {
                    caps.supports_steering && caps.steering == SteeringMode::StepBoundary
                })
            {
                ExecutionSteerDecision {
                    kind: ExecutionSteerKind::Native,
                    reason: None,
                }
            } else {
                ExecutionSteerDecision {
                    kind: ExecutionSteerKind::AfterTurn,
                    reason: Some("This input can run as a follow-up at a turn boundary.".into()),
                }
            };
            (phase, running, changed, ExecutionTransition { kind, waits_for_turn_end: active && kind != Transition::Reuse, reason: reason.into() }, steer,
                Some("Running values describe the admitted runtime after its session start. Adapter capabilities and options are not all independently confirmed by native readback.".into()))
        } else {
            let available = adapter.as_ref().is_some_and(|adapter| adapter.installed())
                && crate::permissions::provider_can_run(config.harness);
            let (kind, reason) = if !available {
                (
                    Transition::Unavailable,
                    "The selected adapter is unavailable on this host.",
                )
            } else if binding.is_some() || chat.harness_session_id.is_some() {
                (
                    Transition::RestartAndResume,
                    "No runtime is live. The next send must validate and resume the saved provider binding.",
                )
            } else {
                (
                    Transition::NewBinding,
                    "No provider session has started. The next send creates a binding; context transfer is separate and explicit.",
                )
            };
            (if available { ExecutionPhase::Idle } else { ExecutionPhase::Unavailable }, None, Vec::new(),
                ExecutionTransition { kind, waits_for_turn_end: false, reason: reason.into() },
                ExecutionSteerDecision { kind: ExecutionSteerKind::Unavailable, reason: Some("There is no active provider turn to steer.".into()) },
                Some("No live runtime observation is available. Historical native sessions are not treated as running.".into()))
        };
        if let Some(error) = policy_error {
            transition.kind = Transition::Unavailable;
            transition.reason = error.to_string();
            steer = ExecutionSteerDecision {
                kind: ExecutionSteerKind::Unavailable,
                reason: Some(
                    "The requested execution policy cannot be applied on this host.".into(),
                ),
            };
        }
        if let Some(host) = self.inner.doc_host()
            && host.has_pending_context_transfer(chat_id)?
        {
            steer = ExecutionSteerDecision { kind: ExecutionSteerKind::Unavailable,
                reason: Some("Shared conversation context is pending. Wait for this turn to finish, then send explicitly; uncertain context delivery is never replayed.".into()) };
        }
        Ok(Some(TaskExecutionSummary {
            schema_version: EXECUTION_SUMMARY_VERSION,
            task_id: chat_id.into(),
            observed_at_ms: crate::now_ms(),
            phase,
            requested: selected,
            running,
            pending: changed,
            transition,
            steer,
            capabilities,
            binding_generation: binding.map(|binding| binding.generation),
            observation_note: note,
        }))
    }
}
