//! The versioned, provider-visible LUNA runtime contract.
//!
//! This wire type contains instruction text while a run is being dispatched.
//! Persist and display only [`RuntimeContractDiagnostics`], never this value.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{HarnessId, ReasoningLevel};

pub const LUNA_RUNTIME_CONTRACT_VERSION: u32 = 1;
/// Engine-owned model option. Incoming client values are always discarded.
pub const LUNA_RUNTIME_CONTRACT_OPTION: &str = "__luna_runtime_contract";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContext {
    pub app_version: String,
    pub protocol_version: u32,
    pub thread_id: String,
    pub parent_thread_id: Option<String>,
    pub source_thread_id: Option<String>,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub environment_id: String,
    pub machine_id: String,
    pub machine_name: String,
    pub workspace_root: String,
    pub worktree_id: Option<String>,
    #[serde(default)]
    pub binding_generation: Option<u32>,
    #[serde(default)]
    pub provider_profile_id: Option<String>,
    pub branch: Option<String>,
    pub harness: HarnessId,
    pub model: Option<String>,
    pub reasoning: Option<ReasoningLevel>,
    pub execution_route: String,
    pub granted_scopes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeInstructionSource {
    LunaCore,
    Tool,
    User,
    Workspace,
    Context,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInstructionSection {
    pub name: String,
    pub source: RuntimeInstructionSource,
    pub provenance: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeToolDefinition {
    pub name: String,
    pub version: u32,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub required_scopes: Vec<String>,
    pub deadline_ms: u64,
    pub idempotency: String,
    pub presentation: String,
    pub activity_classification: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeDelivery {
    CodexDynamicTools,
    CodexInstructionsOnly,
    ClaudeInstructionsOnly,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmittedRuntimeTool {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LunaAgentRuntimeContract {
    pub version: u32,
    pub content_hash: String,
    pub context: RuntimeContext,
    /// Non-secret, stable identity supplied to the provider process.
    pub environment: BTreeMap<String, String>,
    /// Empty until L24/L26 install a verified local tool or CLI endpoint.
    pub local_endpoints: Vec<String>,
    pub instructions: Vec<RuntimeInstructionSection>,
    pub tools: Vec<RuntimeToolDefinition>,
    pub omitted_tools: Vec<OmittedRuntimeTool>,
    pub delivery: RuntimeDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInstructionSummary {
    pub name: String,
    pub source: RuntimeInstructionSource,
    pub provenance: String,
    pub character_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeToolSummary {
    pub name: String,
    pub version: u32,
    pub required_scopes: Vec<String>,
}

/// Safe readback. No instruction body, secret, endpoint, or grant is persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContractDiagnostics {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at_ms: Option<i64>,
    pub contract_version: u32,
    pub contract_hash: String,
    pub delivery: RuntimeDelivery,
    pub instructions: Vec<RuntimeInstructionSummary>,
    pub attached_tools: Vec<RuntimeToolSummary>,
    pub omitted_tools: Vec<OmittedRuntimeTool>,
    pub restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContractView {
    pub task_id: String,
    pub provider: Option<HarnessId>,
    pub provider_delivery: RuntimeDelivery,
    pub effective: Option<RuntimeContractDiagnostics>,
    pub unavailable_reason: Option<String>,
}

/// LUNA-owned instructions. Provider-native instruction files stay with their
/// provider and are deliberately absent from this editable settings value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionSettingsView {
    pub revision: u64,
    pub global: String,
    pub project_id: Option<String>,
    pub project: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IntegrationKind {
    Skill,
    McpServer,
}

/// Configuration owner, not the upstream server's identity. A Codex MCP
/// entry remains provider-owned even if its name happens to mention a gateway.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IntegrationOwner {
    #[default]
    Unknown,
    ProviderNative,
    Gateway,
}

/// LUNA action availability. `Unavailable` means this LUNA adapter has no
/// verified action; it does not claim the provider lacks its own controls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IntegrationActionAvailability {
    #[default]
    Unknown,
    Unavailable,
    Supported,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IntegrationActionFacts {
    pub enable: IntegrationActionAvailability,
    pub disable: IntegrationActionAvailability,
    pub authenticate: IntegrationActionAvailability,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IntegrationSkillScope {
    Global,
    Project,
}

/// A destination class, never a path, URL, command, or credential. LUNA only
/// emits routes whose owner it can resolve from the inventory source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IntegrationHelpRoute {
    #[default]
    Unknown,
    SkillSource {
        scope: IntegrationSkillScope,
    },
    ProviderMcpConfig,
    GatewayConnection {
        connection_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationInventoryItem {
    /// Opaque, source-scoped identity. None only for legacy inventory JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    pub name: String,
    pub kind: IntegrationKind,
    /// Provider-reported or provider-owned config, never a LUNA copy.
    pub source: String,
    pub scope: String,
    pub configured: bool,
    /// None means availability has not been verified with the provider.
    pub available: Option<bool>,
    /// None means the provider has no trustworthy per-session attachment readback.
    pub attached: Option<bool>,
    pub reason: Option<String>,
    #[serde(default)]
    pub owner: IntegrationOwner,
    #[serde(default)]
    pub actions: IntegrationActionFacts,
    #[serde(default)]
    pub help_route: IntegrationHelpRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationInventoryView {
    pub provider: HarnessId,
    pub machine_id: String,
    pub items: Vec<IntegrationInventoryItem>,
    pub unavailable_reason: Option<String>,
    pub help: String,
}

/// A provider-owned SKILL.md read through the managed library. The identity
/// resolves only within the selected provider, device and project scope; the
/// file path stays on the engine. A revision is required for every edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDocumentView {
    pub provider: HarnessId,
    pub scope: IntegrationSkillScope,
    pub identity: String,
    pub name: String,
    pub content: String,
    pub revision: String,
    /// Relative names for files alongside SKILL.md, never absolute paths.
    pub supporting_files: Vec<String>,
    /// Symlinks and other externally owned entries remain inspectable.
    pub read_only_reason: Option<String>,
}

impl LunaAgentRuntimeContract {
    pub fn diagnostics(
        &self,
        provider_session_id: impl Into<String>,
    ) -> RuntimeContractDiagnostics {
        RuntimeContractDiagnostics {
            thread_id: self.context.thread_id.clone(),
            provider_session_id: provider_session_id.into(),
            run_id: None,
            accepted_at_ms: None,
            contract_version: self.version,
            contract_hash: self.content_hash.clone(),
            delivery: self.delivery,
            instructions: self
                .instructions
                .iter()
                .map(|section| {
                    let (name, provenance) = match section.source {
                        RuntimeInstructionSource::User => {
                            ("User instruction".into(), "user settings".into())
                        }
                        RuntimeInstructionSource::Workspace => {
                            ("Workspace instruction".into(), "workspace settings".into())
                        }
                        _ => (section.name.clone(), section.provenance.clone()),
                    };
                    RuntimeInstructionSummary {
                        name,
                        source: section.source,
                        provenance,
                        character_count: section.text.chars().count(),
                    }
                })
                .collect(),
            attached_tools: self
                .tools
                .iter()
                .map(|tool| RuntimeToolSummary {
                    name: tool.name.clone(),
                    version: tool.version,
                    required_scopes: tool.required_scopes.clone(),
                })
                .collect(),
            omitted_tools: self.omitted_tools.clone(),
            restart_required: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_inventory_json_decodes_with_unknown_contract_facts() {
        let old = serde_json::json!({
            "provider": "codex",
            "machineId": "synthetic-machine",
            "items": [{
                "name": "sample",
                "kind": "mcpServer",
                "source": "Codex home / config.toml",
                "scope": "global",
                "configured": true,
                "available": null,
                "attached": null,
                "reason": null
            }],
            "unavailableReason": null,
            "help": "legacy help"
        });
        let view: IntegrationInventoryView = serde_json::from_value(old).unwrap();
        let item = &view.items[0];
        assert_eq!(item.identity, None);
        assert_eq!(item.owner, IntegrationOwner::Unknown);
        assert_eq!(item.actions.enable, IntegrationActionAvailability::Unknown);
        assert_eq!(item.actions.disable, IntegrationActionAvailability::Unknown);
        assert_eq!(
            item.actions.authenticate,
            IntegrationActionAvailability::Unknown
        );
        assert_eq!(item.help_route, IntegrationHelpRoute::Unknown);
    }

    #[test]
    fn public_diagnostics_omit_scrubbed_provider_session_id() {
        let diagnostics = RuntimeContractDiagnostics {
            thread_id: "luna-task".into(),
            provider_session_id: String::new(),
            run_id: Some("luna-run".into()),
            accepted_at_ms: Some(1),
            contract_version: 1,
            contract_hash: "synthetic-hash".into(),
            delivery: RuntimeDelivery::CodexDynamicTools,
            instructions: Vec::new(),
            attached_tools: Vec::new(),
            omitted_tools: Vec::new(),
            restart_required: false,
        };
        let value = serde_json::to_value(diagnostics).unwrap();
        assert!(value.get("providerSessionId").is_none());
        assert_eq!(value["threadId"], "luna-task");
    }
}
