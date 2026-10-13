//! Bounded, read-only inventory evidence from this engine's actual root stream.
//! No inputs, outputs, credentials, child calls or command authority are retained.
use super::{Inner, RuntimeConfig};
use luna_proto::{AgentEvent, HarnessId, IntegrationCallEvidence, PermissionMode, ToolCall};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;

#[derive(Clone)]
pub(crate) struct Observation {
    pub run_id: String,
    pub provider: HarnessId,
    pub account_id: Option<String>,
    pub mode: PermissionMode,
    pub requested_mode: PermissionMode,
    pub stamp: String,
    pub session_id: Option<String>,
    pub observed_at_ms: i64,
    pub tools: Vec<String>,
    pub calls: BTreeMap<String, IntegrationCallEvidence>,
    pub successful_servers: BTreeSet<String>,
    pending: BTreeMap<String, (String, String)>,
}
#[derive(Default)]
pub(super) struct RuntimeInventory(Mutex<VecDeque<(String, Observation)>>);
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':'))
}
pub(super) fn stamp(
    inner: &Inner,
    provider: HarnessId,
    account: Option<&str>,
    cwd: &str,
    mode: PermissionMode,
) -> String {
    let execution = inner
        .agent_accounts
        .get()
        .and_then(|accounts| accounts.resolve_execution(provider, account).ok());
    let (claude_home, opencode_home) = inner
        .agent_accounts
        .get()
        .map(|a| a.skill_config_dirs())
        .unwrap_or_default();
    let roots = crate::integration_settings::SkillRoots {
        codex_home: execution.as_ref().and_then(|a| a.codex_home.clone()),
        claude_home,
        opencode_home,
        project_root: Some(cwd.into()),
        machine_id: String::new(),
    };
    let source = inner
        .registry
        .resolve(provider)
        .ok()
        .map(|h| h.model_catalog_source());
    let instructions = inner
        .integration_settings
        .get()
        .map(|settings| settings.view(None).revision);
    let claude_settings = if provider == HarnessId::ClaudeCode {
        inner
            .agent_accounts
            .get()
            .and_then(|accounts| std::fs::read(accounts.claude_settings_source()).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .map(|config| {
                let project = config
                    .get("projects")
                    .and_then(|projects| projects.get(cwd));
                let keys = [
                    "mcpServers",
                    "disabledMcpServers",
                    "enabledMcpjsonServers",
                    "disabledMcpjsonServers",
                    "enableAllProjectMcpServers",
                    "permissions",
                    "enabledPlugins",
                ];
                let selected: Vec<_> = keys
                    .iter()
                    .map(|key| {
                        (
                            *key,
                            config.get(*key),
                            project.and_then(|project| project.get(*key)),
                        )
                    })
                    .collect();
                format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&selected).unwrap_or_default())
                )
            })
    } else {
        None
    };
    let key = format!(
        "{}:{:?}:{cwd}:{mode:?}:{source:?}:{instructions:?}:{claude_settings:?}:{}",
        provider.canonical().as_str(),
        execution.map(|a| a.auth_instance_id),
        crate::integration_settings::source_revision(provider, &roots)
    );
    format!("{:x}", Sha256::digest(key.as_bytes()))
}
impl RuntimeInventory {
    pub(super) fn start(&self, inner: &Inner, task: &str, run: &str, config: &RuntimeConfig) {
        let mode = config
            .permission_policy
            .as_ref()
            .map(|p| p.effective_mode)
            .unwrap_or(if config.auto_approve {
                PermissionMode::Full
            } else {
                PermissionMode::Ask
            });
        let observation = Observation {
            run_id: run.into(),
            provider: config.harness_id,
            account_id: config.provider_account_id.clone(),
            mode,
            requested_mode: config
                .permission_policy
                .as_ref()
                .map(|p| p.requested_mode)
                .unwrap_or(mode),
            stamp: stamp(
                inner,
                config.harness_id,
                config.provider_account_id.as_deref(),
                &config.cwd,
                mode,
            ),
            session_id: None,
            observed_at_ms: 0,
            tools: Vec::new(),
            calls: BTreeMap::new(),
            successful_servers: BTreeSet::new(),
            pending: BTreeMap::new(),
        };
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|(id, _)| id != task);
        if entries.len() >= 128 {
            entries.pop_front();
        }
        entries.push_back((task.into(), observation));
    }
    pub(super) fn observe(&self, task: &str, event: &AgentEvent) {
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((_, state)) = entries.iter_mut().find(|(id, _)| id == task) else {
            return;
        };
        match event {
            AgentEvent::SessionStarted {
                harness,
                session_id,
                tools,
                ..
            } if *harness == state.provider => {
                state.session_id = Some(session_id.clone());
                state.observed_at_ms = chrono::Utc::now().timestamp_millis();
                state.tools = tools
                    .iter()
                    .filter(|s| name(s))
                    .take(512)
                    .cloned()
                    .collect();
                state.pending.clear();
            }
            AgentEvent::ToolCall {
                id,
                call: ToolCall::Mcp { server, tool, .. },
            } if state.session_id.is_some() && name(server) && name(tool) && id.len() <= 256 => {
                if state.pending.len() >= 64 {
                    state.pending.pop_first();
                }
                state
                    .pending
                    .insert(id.clone(), (server.clone(), tool.clone()));
            }
            AgentEvent::ToolResult { id, is_error, .. } => {
                if let Some((server, tool)) = state.pending.remove(id) {
                    if state.calls.len() >= 64 && !state.calls.contains_key(&server) {
                        state.calls.pop_first();
                    }
                    if !is_error {
                        if state.successful_servers.len() >= 64
                            && !state.successful_servers.contains(&server)
                        {
                            state.successful_servers.pop_first();
                        }
                        state.successful_servers.insert(server.clone());
                    }
                    state.calls.insert(
                        server,
                        IntegrationCallEvidence {
                            tool,
                            succeeded: !is_error,
                            observed_at_ms: chrono::Utc::now().timestamp_millis(),
                        },
                    );
                }
            }
            _ => {}
        }
    }
    pub(super) fn get(&self, task: &str) -> Option<Observation> {
        self.0
            .lock()
            .ok()?
            .iter()
            .find(|(id, _)| id == task)
            .map(|(_, state)| state.clone())
    }
}
impl super::SessionsEngine {
    pub(crate) fn integration_observation(&self, task: &str) -> Option<Observation> {
        self.inner.integration_evidence.get(task)
    }
    pub(crate) fn integration_source_stamp(
        &self,
        provider: HarnessId,
        account: Option<&str>,
        cwd: &str,
        mode: PermissionMode,
    ) -> String {
        stamp(&self.inner, provider, account, cwd, mode)
    }
}
