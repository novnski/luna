//! Per-process MCP launch isolation for metadata probes and disposable QA.
//!
//! Ordinary user sessions keep their provider-owned MCP configuration. The QA
//! flag is stamped into a QA bundle's signed environment, not user config.

use std::ffi::OsStr;

use crate::HarnessError;
use crate::codex::native::CodexMcpConfigEntry;
use crate::process::Command;

pub(crate) const QA_ENV: &str = "LUNA_QA_MCP_ISOLATION";
const EMPTY_CLAUDE_MCP: &str = "{\"mcpServers\":{}}";

pub(crate) fn qa_enabled() -> bool {
    std::env::var_os(QA_ENV).as_deref() == Some(OsStr::new("1"))
}

/// Metadata-only Claude processes have no reason to start saved MCP servers.
pub(crate) fn claude_strict_empty(command: &mut Command) {
    command.args(["--strict-mcp-config", "--mcp-config", EMPTY_CLAUDE_MCP]);
}

/// Arguments for a Claude launch with no inherited MCP and only the supplied
/// per-process `--mcp-config` entries (if any).
pub(crate) fn claude_strict_flag(command: &mut Command) {
    command.arg("--strict-mcp-config");
}

/// Codex merges an empty `mcp_servers` table with configured entries. Disable
/// each effective name explicitly before thread/start, then allow the local
/// `luna` endpoint override to re-enable itself last.
pub(crate) fn codex_disable_args(
    servers: &[CodexMcpConfigEntry],
) -> Result<Vec<String>, HarnessError> {
    let mut args = Vec::with_capacity(servers.len() * 2);
    for server in servers {
        if !server
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            || server.name.is_empty()
        {
            return Err(HarnessError::Protocol(
                "MCP isolation cannot safely override this Codex server name".into(),
            ));
        }
        args.push("-c".into());
        args.push(format!("mcp_servers.{}.enabled=false", server.name));
    }
    Ok(args)
}
