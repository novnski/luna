//! Host-owned provider discovery. Capability claims come from the adapter,
//! while models/options retain the native provider's existing typed shape.
use crate::{
    HarnessId, Model, ProviderForkCapability, ReasoningLevel, RuntimeDelivery, SteeringMode,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub version: u32,
    pub steering: bool,
    pub steering_mode: SteeringMode,
    pub reasoning_levels: Vec<ReasoningLevel>,
    pub runtime_delivery: RuntimeDelivery,
    pub fork: ProviderForkCapability,
    pub authoritative_prompt_end: bool,
    pub safe_recovery: bool,
    pub native_child_stop: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogProvenance {
    pub host_id: String,
    /// Unique for this registry lifetime, including engine restarts.
    pub registry_id: String,
    pub generation: u64,
    pub provider_id: HarnessId,
    pub account_id: Option<String>,
    /// Opaque non-secret execution scope; never credentials.
    pub auth_instance_id: String,
    pub executable: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModels {
    pub provenance: ProviderCatalogProvenance,
    pub models: Vec<Model>,
}
