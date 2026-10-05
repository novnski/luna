//! Redacted execution observations. A preview is never authority to dispatch.

use serde::{Deserialize, Serialize};

use crate::{
    HarnessId, PermissionMode, ProviderForkCapability, ProviderPermissionCapabilities,
    ReasoningLevel, RuntimeDelivery, SandboxLevel, SteeringMode,
};

pub const EXECUTION_SUMMARY_VERSION: u32 = 1;

/// Adapter declarations observed on the owning host. They do not establish
/// support in a particular installed CLI, account or native session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderExecutionCapabilities {
    pub provider: HarnessId,
    pub steering: SteeringMode,
    pub supports_steering: bool,
    pub authoritative_prompt_end: bool,
    pub deterministic_turn_end: bool,
    pub safe_recovery_continuation: bool,
    pub native_fork: ProviderForkCapability,
    pub runtime_delivery: RuntimeDelivery,
    pub permissions: ProviderPermissionCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSettings {
    pub provider: HarnessId,
    pub provider_account_id: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<ReasoningLevel>,
    pub model_options: serde_json::Map<String, serde_json::Value>,
    /// Requested in `requested`; enforced policy in `running`.
    pub permission_mode: PermissionMode,
    pub sandbox: SandboxLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionPhase {
    Idle,
    Starting,
    Running,
    Warm,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionSettingField {
    Provider,
    Account,
    Model,
    Reasoning,
    ModelOptions,
    Permission,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionTransitionKind {
    Reuse,
    ApplyNextTurn,
    RestartAndResume,
    /// Context transfer is a separate explicit operation; this never implies
    /// that native history will be copied automatically.
    NewBinding,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionTransition {
    pub kind: ExecutionTransitionKind,
    pub waits_for_turn_end: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionSteerKind {
    Native,
    AfterTurn,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSteerDecision {
    pub kind: ExecutionSteerKind,
    pub reason: Option<String>,
}

/// An owning-engine observation for the current task and runtime. `running`
/// is absent until a provider session start was observed and after retirement.
/// It describes the admitted runtime configuration, not a blanket assertion
/// that every option has independent provider readback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskExecutionSummary {
    pub schema_version: u32,
    pub task_id: String,
    pub observed_at_ms: i64,
    pub phase: ExecutionPhase,
    pub requested: ExecutionSettings,
    pub running: Option<ExecutionSettings>,
    pub pending: Vec<ExecutionSettingField>,
    pub transition: ExecutionTransition,
    pub steer: ExecutionSteerDecision,
    pub capabilities: Option<ProviderExecutionCapabilities>,
    /// App-owned binding generation, never a provider-native identifier.
    pub binding_generation: Option<u32>,
    /// Why an observation is incomplete, unavailable, or not independently
    /// confirmed by the native provider. Never contains request payloads.
    pub observation_note: Option<String>,
}
