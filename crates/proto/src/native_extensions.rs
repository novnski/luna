//! Effective native extension configuration for one provider launch: which
//! settings sources load, the output style, hooks, and custom commands/skills
//! with their scope and precedence. Configured (file) state is kept apart from
//! what the provider itself reported for the same launch (`native*` fields),
//! so the UI never presents an unverified file as active.
use serde::{Deserialize, Serialize};

use crate::{HarnessId, PermissionMode};

/// Where a native setting or extension comes from. Ordered by Claude Code's
/// precedence for scalar settings (managed wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeSettingsScope {
    Managed,
    /// `--settings` on the launch command line.
    Flag,
    Local,
    Project,
    User,
    /// Shipped with the provider.
    BuiltIn,
}

impl NativeSettingsScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Managed => "Managed",
            Self::Flag => "Launch flag",
            Self::Local => "Local",
            Self::Project => "Project",
            Self::User => "User",
            Self::BuiltIn => "Built-in",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSettingsSourceView {
    pub scope: NativeSettingsScope,
    /// Home- or project-relative display path; never an absolute host path.
    pub display_path: String,
    pub present: bool,
    /// The launch loads this source.
    pub loaded: bool,
    /// Why it is not loaded, or why its content could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeConfiguredValue {
    pub scope: NativeSettingsScope,
    pub value: String,
    /// This value is the one the launch applies.
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeStyleOption {
    pub name: String,
    pub scope: NativeSettingsScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeOutputStyleView {
    /// Effective style from the configured precedence (`default` when unset).
    pub configured_effective: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_scope: Option<NativeSettingsScope>,
    /// The style the provider reported for this launch; None = not verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
    #[serde(default)]
    pub configured: Vec<NativeConfiguredValue>,
    /// Styles this launch can select, from files and built-ins.
    #[serde(default)]
    pub available: Vec<NativeStyleOption>,
    /// The provider's own list for this launch; None = not verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_available: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeHookView {
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// Handler type plus a program/host label. Full command text stays on the host.
    pub handler: String,
    pub scope: NativeSettingsScope,
    pub effective: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeCommandKind {
    Command,
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeCommandView {
    /// Invocation name without the leading slash (`ns:name` when namespaced).
    pub name: String,
    pub kind: NativeCommandKind,
    pub scope: NativeSettingsScope,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    /// The launch loads this file and it wins its name.
    pub configured_effective: bool,
    /// The provider listed this entry for the same launch; None = not verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadowed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeExtensionsView {
    pub provider: HarnessId,
    /// The LUNA permission mode whose launch this describes.
    pub permission_mode: PermissionMode,
    /// One sentence describing how the launch selects settings sources.
    pub launch_policy: String,
    #[serde(default)]
    pub sources: Vec<NativeSettingsSourceView>,
    #[serde(default)]
    pub output_style: NativeOutputStyleView,
    #[serde(default)]
    pub hooks: Vec<NativeHookView>,
    #[serde(default)]
    pub commands: Vec<NativeCommandView>,
    /// A provider initialize readback for the same launch succeeded.
    #[serde(default)]
    pub native_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_error: Option<String>,
    /// Changes when any relevant settings/extension file changes.
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeExtensionsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Overrides the chat's effective mode (for previewing another mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    /// Skip the cached view and run a fresh provider readback.
    #[serde(default)]
    pub refresh: bool,
}
