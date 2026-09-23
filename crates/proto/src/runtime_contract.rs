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
