//! Provider-native MCP control-plane contract (CC-11).
//!
//! Adapters opt in through [`crate::Harness::mcp_control`] and
//! [`crate::Harness::session_mcp_observation`]; the defaults are honest
//! unsupported/unknown answers. Every value is a redacted provider readback:
//! configuration URLs, headers, commands, arguments and environment never
//! enter these types. No operation here may start a model turn.

use std::path::PathBuf;

use luna_proto::{PermissionMode, ProviderMcpAppResourceView, ProviderMcpStatus, ProviderMcpTool};

/// One MCP server row exactly as far as the provider reported it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderMcpServer {
    pub name: String,
    pub status: ProviderMcpStatus,
    /// Provider scope word (`user`, `project`, `local`, `dynamic`, ...).
    pub scope: Option<String>,
    /// Transport word only (`stdio`, `http`, ...).
    pub transport: Option<String>,
    pub server_version: Option<String>,
    /// Bounded, path/URL-redacted provider error text.
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub tools: Vec<ProviderMcpTool>,
    /// The provider supports a per-project enable/disable for this row.
    pub toggleable: bool,
}

/// One readback for the launch modes that share identical settings-shaping
/// launch flags. `settled` is false when a server was still `pending` at the
/// deadline; those rows stay pending rather than being guessed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpLaunchReadback {
    pub modes: Vec<PermissionMode>,
    pub restricted: bool,
    pub settled: bool,
    pub servers: Vec<ProviderMcpServer>,
    /// The provider accepted the MCP Apps resource-read control in this build,
    /// or `None` when the readback could not tell.
    pub app_read_supported: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpControlOp {
    /// One provider readback per distinct launch configuration of `modes`.
    Status { modes: Vec<PermissionMode> },
    /// Provider-owned persistent enable/disable for the request's cwd, with a
    /// fresh status readback in the same provider process.
    SetEnabled { server: String, enabled: bool },
    /// Read one MCP Apps `ui://` resource through the provider's own client.
    ReadAppResource { server: String, uri: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpControlRequest {
    /// Absolute working directory (the selected project) for the provider.
    pub cwd: PathBuf,
    pub op: McpControlOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpWriteOutcome {
    /// The provider answered success.
    Applied,
    /// The provider answered an error before changing anything.
    Rejected,
    /// No answer arrived after the request was written: it may have applied.
    Indeterminate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpControlReply {
    Status(Vec<McpLaunchReadback>),
    SetEnabled {
        outcome: McpWriteOutcome,
        /// Fresh readback after the write (None when unavailable).
        readback: Option<Vec<ProviderMcpServer>>,
        /// Redacted provider message for a rejection or failure.
        message: Option<String>,
    },
    /// `identity` is left empty for the engine to fill.
    AppResource(ProviderMcpAppResourceView),
}

/// One server row from a session's own start frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMcpServer {
    pub name: String,
    pub status: ProviderMcpStatus,
    pub source: Option<String>,
}

/// What a provider session reported about MCP at its (latest) start. Kept in
/// memory only: after a LUNA restart there is no session evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMcpObservation {
    pub session_id: String,
    pub cwd: String,
    pub observed_at_ms: i64,
    pub servers: Vec<SessionMcpServer>,
    /// The start frame carried a capability list at all.
    pub capabilities_reported: bool,
    pub app_read_capability: bool,
    pub app_tool_meta_capability: bool,
}
