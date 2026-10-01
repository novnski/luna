//! Project overrides for new tasks. Absence always means inherit; existing
//! tasks retain their own ChatConfig and checkout identity.
use crate::{HarnessId, PermissionMode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectExecutionDefaults {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProjectProviderDefault>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout: Option<ProjectCheckout>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectProviderDefault {
    pub harness: HarnessId,
    /// None explicitly uses this provider's catalog default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectCheckout {
    Local,
    NewWorktree,
}

impl ProjectExecutionDefaults {
    pub fn is_inherited(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(provider) = &self.provider {
            if !matches!(
                provider.harness,
                HarnessId::Codex | HarnessId::ClaudeCode | HarnessId::Opencode
            ) {
                return Err("Choose Codex, Claude Code, or OpenCode as the project provider.");
            }
            if let Some(model) = &provider.model
                && (model.trim().is_empty()
                    || model.len() > 512
                    || model.trim() != model
                    || model.chars().any(char::is_control))
            {
                return Err("Project model must be a nonempty model ID of at most 512 bytes.");
            }
        }
        if self.permission_mode == Some(PermissionMode::Ask) {
            return Err("Choose an available permission mode or inherit the global default.");
        }
        Ok(())
    }
}

/// Shared desktop/engine selection policy. A selected project provider with no
/// model requests its catalog default; it never copies the source chat model.
pub struct ResolvedProjectExecution<'a> {
    pub harness: Option<HarnessId>,
    pub model: Option<&'a str>,
    pub permission_mode: Option<PermissionMode>,
    pub checkout: ProjectCheckout,
}
impl ProjectExecutionDefaults {
    pub fn resolve<'a>(
        &'a self,
        global_harness: Option<HarnessId>,
        global_model: Option<&'a str>,
        global_permission: Option<PermissionMode>,
    ) -> ResolvedProjectExecution<'a> {
        ResolvedProjectExecution {
            harness: self
                .provider
                .as_ref()
                .map(|provider| provider.harness)
                .or(global_harness),
            model: self
                .provider
                .as_ref()
                .map(|provider| provider.model.as_deref())
                .unwrap_or(global_model),
            permission_mode: self.permission_mode.or(global_permission),
            checkout: self.checkout.unwrap_or(ProjectCheckout::Local),
        }
    }
}
