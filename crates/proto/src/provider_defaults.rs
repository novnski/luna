//! Device/profile-owned execution defaults for fresh conversations.
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{HarnessId, ReasoningLevel};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderExecutionDefault {
    /// None selects the provider catalog's default model.
    pub model: Option<String>,
    /// None uses LUNA's shared default for this model's reasoning ladder.
    pub reasoning: Option<ReasoningLevel>,
    /// Provider catalog option IDs and choice IDs, including speed where offered.
    pub model_options: serde_json::Map<String, serde_json::Value>,
}

/// The same fresh-chat fallback for the desktop composer and agent tools.
pub fn default_reasoning_level(ladder: &[ReasoningLevel]) -> Option<ReasoningLevel> {
    [ReasoningLevel::High, ReasoningLevel::Medium]
        .into_iter()
        .find(|level| ladder.contains(level))
        .or_else(|| ladder.first().copied())
}

pub fn default_model_options(model: &crate::Model) -> serde_json::Map<String, serde_json::Value> {
    model
        .options
        .iter()
        .map(|option| {
            (
                option.id.clone(),
                serde_json::Value::String(option.default_choice.clone()),
            )
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderDefaultsSnapshot {
    pub revision: u64,
    pub providers: HashMap<HarnessId, ProviderExecutionDefault>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetProviderDefaultsRequest {
    pub expected_revision: u64,
    pub provider: HarnessId,
    /// None removes the saved override and restores provider defaults.
    pub defaults: Option<ProviderExecutionDefault>,
}
