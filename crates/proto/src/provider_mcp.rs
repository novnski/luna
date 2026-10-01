//! Provider-reported MCP server state (CC-11).
//!
//! Everything here is a redacted readback of what the provider itself reported:
//! a server's name, status words, scope/transport words, tool names and MCP Apps
//! (`ui://`) metadata. Configuration values (URLs, headers, commands, args, env)
//! never enter these types.

use serde::{Deserialize, Serialize};

use crate::PermissionMode;

/// Connection status words the provider reports. `Unknown` is any word this
/// LUNA build does not recognise; it is never a health claim.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderMcpStatus {
    Connected,
    Failed,
    NeedsAuth,
    Pending,
    Disabled,
    #[default]
    Unknown,
}

impl ProviderMcpStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Failed => "failed",
            Self::NeedsAuth => "needs authentication",
            Self::Pending => "still connecting",
            Self::Disabled => "disabled",
            Self::Unknown => "unrecognised status",
        }
    }
}

/// One MCP tool as the provider listed it. `ui_resource_uri` is the MCP Apps
/// (SEP-1865) `_meta.ui.resourceUri`, kept only when it is a `ui://` URI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderMcpTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_resource_uri: Option<String>,
    /// `_meta.ui.visibility` words (`model`, `app`), bounded.
    pub ui_visibility: Vec<String>,
}

/// One provider readback for a set of LUNA launch modes that share the same
/// settings-shaping launch flags. `status: None` means the readback for those
/// modes did not include this server at all (not loaded).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderMcpLaunchState {
    pub modes: Vec<PermissionMode>,
    /// The run uses the provider's restricted settings mode for these modes.
    pub restricted: bool,
    pub status: Option<ProviderMcpStatus>,
}

/// The selected chat's own provider session report, taken from that session's
/// start frame. It is session evidence, not a configuration read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderMcpSessionState {
    pub status: ProviderMcpStatus,
    /// Unix milliseconds when LUNA observed the session start frame.
    pub observed_at_ms: i64,
}

/// MCP Apps support as evidenced for this provider. Rendering is separate:
/// LUNA has no isolated MCP Apps host yet, so app views are never rendered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProviderMcpAppSupport {
    #[default]
    Unknown,
    /// The provider advertised or accepted `ui://` resource reads.
    ReadSupported,
    Unavailable {
        reason: String,
    },
}

/// Redacted provider-reported details attached to an MCP inventory item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IntegrationMcpDetails {
    /// Status from the most complete provider readback (the unrestricted one
    /// when LUNA ran it), or the session's status when only that exists.
    pub status: ProviderMcpStatus,
    /// `stdio`, `http`, `sse`, `sdk`, `claudeai-proxy`, ... (word only).
    pub transport: Option<String>,
    /// `name version` reported by the server when connected.
    pub server_version: Option<String>,
    /// Bounded provider error text with paths and URLs redacted.
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub tool_count: usize,
    /// Tools that declare an MCP Apps view.
    pub app_tools: Vec<ProviderMcpTool>,
    pub launches: Vec<ProviderMcpLaunchState>,
    pub session: Option<ProviderMcpSessionState>,
    pub app_support: ProviderMcpAppSupport,
}

/// Outcome of a provider-owned enable/disable request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderMcpActionOutcome {
    /// A fresh provider readback shows the requested state.
    Verified,
    /// The provider accepted the request, but the readback disagrees.
    Mismatch,
    /// The provider rejected the request before changing anything.
    Rejected,
    /// The request may have been applied; inspect provider state.
    Indeterminate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderMcpActionReceipt {
    pub identity: String,
    pub server: String,
    pub requested_enabled: bool,
    pub outcome: ProviderMcpActionOutcome,
    #[serde(default)]
    pub observed_status: Option<ProviderMcpStatus>,
    pub message: String,
}

/// A provider-read MCP Apps `ui://` resource, reduced to inspectable facts.
/// `rendered` is always false in this build: no isolated MCP Apps host yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderMcpAppResourceView {
    pub identity: String,
    pub server: String,
    pub uri: String,
    pub mime_type: Option<String>,
    /// Bytes received for the first content item (text UTF-8 or decoded blob).
    pub byte_len: usize,
    pub sha256: String,
    pub binary: bool,
    /// Escaped-for-display source preview; never executed.
    pub text_preview: Option<String>,
    pub truncated: bool,
    pub content_count: usize,
    pub csp_connect_domains: Vec<String>,
    pub csp_resource_domains: Vec<String>,
    pub csp_frame_domains: Vec<String>,
    pub csp_base_uri_domains: Vec<String>,
    pub permissions: Vec<String>,
    pub prefers_border: Option<bool>,
    pub rendered: bool,
    pub render_unavailable_reason: String,
}
