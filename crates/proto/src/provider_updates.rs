use serde::{Deserialize, Serialize};

use crate::HarnessId;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUpdateStatus {
    pub harness: HarnessId,
    pub installed_version: Option<String>,
    pub latest_version: Option<String>,
    pub update_available: bool,
    /// The exact command LUNA can run for this installation. None means manual.
    pub update_command: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUpdateResult {
    pub harness: HarnessId,
    pub installed_version: String,
    /// A running turn keeps its old process and reports why reload was deferred.
    pub runtime_reload_error: Option<String>,
    pub model_refresh_error: Option<String>,
}
