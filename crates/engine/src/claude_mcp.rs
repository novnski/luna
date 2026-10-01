//! CC-11: Claude's effective MCP state in the shared integration inventory,
//! provider-owned enable/disable, and inert MCP Apps resource inspection.
//!
//! Sources, never mixed:
//! - provider readback: `mcp_status` from a control-only Claude process in the
//!   selected project, once per distinct launch configuration (LUNA's Ask/Auto
//!   runs use Claude's restricted mode, which loads no configured servers);
//! - session evidence: the selected chat's own Claude `system:init` report.
//! Configuration files are never read or written here; Claude owns its
//! config, and LUNA never registers or replaces MCP servers.

use std::path::Path;
use std::time::Duration;

use luna_harness::Harness;
use luna_harness::mcp_control::{
    McpControlOp, McpControlReply, McpControlRequest, McpLaunchReadback, McpWriteOutcome,
    ProviderMcpServer, SessionMcpObservation,
};
use luna_proto::{
    IntegrationActionAvailability, IntegrationActionFacts, IntegrationHelpRoute,
    IntegrationInventoryItem, IntegrationInventoryView, IntegrationKind, IntegrationMcpDetails,
    IntegrationOwner, PermissionMode, ProviderMcpActionOutcome, ProviderMcpActionReceipt,
    ProviderMcpAppResourceView, ProviderMcpAppSupport, ProviderMcpLaunchState,
    ProviderMcpSessionState, ProviderMcpStatus,
};
use sha2::{Digest, Sha256};

/// Provider text without the adapter's internal error-category prefix.
fn provider_message(error: luna_harness::HarnessError) -> String {
    match error {
        luna_harness::HarnessError::Protocol(message) => message,
        other => other.to_string(),
    }
}

const READBACK_TIMEOUT: Duration = Duration::from_secs(45);
const ACTION_TIMEOUT: Duration = Duration::from_secs(45);
/// One provider-owned MCP write at a time per engine.
static ACTION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const TOGGLE_EFFECT: &str = "Enable/disable asks Claude to save its own per-project preference. It applies to new Claude sessions in this project; a running chat keeps its current connection until it restarts.";
const AUTH_UNAVAILABLE: &str =
    "Authentication stays with Claude: run /mcp in Claude Code to sign in.";

/// Opaque, source-scoped identity. The project root participates so an
/// action for project A can never address project B's preference.
pub(crate) fn server_identity(machine_id: &str, project_root: Option<&Path>, name: &str) -> String {
    let mut hash = Sha256::new();
    let root = project_root
        .map(|root| root.to_string_lossy().into_owned())
        .unwrap_or_default();
    for value in ["luna-claude-mcp-v1", machine_id, root.as_str(), name] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    format!("intg_mcp_v1_{:x}", hash.finalize())
}

fn mode_label(modes: &[PermissionMode]) -> String {
    modes
        .iter()
        .map(|mode| match mode {
            PermissionMode::Ask => "Ask",
            PermissionMode::Auto => "Auto",
            PermissionMode::Full => "Full access",
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn available(status: ProviderMcpStatus) -> Option<bool> {
    match status {
        ProviderMcpStatus::Connected => Some(true),
        ProviderMcpStatus::Failed | ProviderMcpStatus::Disabled => Some(false),
        ProviderMcpStatus::NeedsAuth | ProviderMcpStatus::Pending | ProviderMcpStatus::Unknown => {
            None
        }
    }
}

fn actions(row: Option<&ProviderMcpServer>, project: bool) -> IntegrationActionFacts {
    let unavailable = IntegrationActionAvailability::Unavailable;
    let Some(row) = row.filter(|row| project && row.toggleable) else {
        let reason = if !project {
            "Select a project: Claude saves enable/disable per project."
        } else if row.is_some() {
            "Claude does not offer a per-project enable/disable for this server's scope."
        } else {
            "Check provider to read this server from Claude before changing it."
        };
        return IntegrationActionFacts {
            enable: unavailable,
            disable: unavailable,
            authenticate: unavailable,
            reason: Some(format!("{reason} {AUTH_UNAVAILABLE}")),
        };
    };
    let disabled = row.status == ProviderMcpStatus::Disabled;
    IntegrationActionFacts {
        enable: if disabled {
            IntegrationActionAvailability::Supported
        } else {
            unavailable
        },
        disable: if disabled {
            unavailable
        } else {
            IntegrationActionAvailability::Supported
        },
        authenticate: unavailable,
        reason: Some(format!("{TOGGLE_EFFECT} {AUTH_UNAVAILABLE}")),
    }
}

fn app_support(
    readbacks: &[McpLaunchReadback],
    session: Option<&SessionMcpObservation>,
) -> ProviderMcpAppSupport {
    if session.is_some_and(|session| session.app_read_capability)
        || readbacks.iter().any(|r| r.app_read_supported == Some(true))
    {
        ProviderMcpAppSupport::ReadSupported
    } else if readbacks
        .iter()
        .any(|r| r.app_read_supported == Some(false))
    {
        ProviderMcpAppSupport::Unavailable {
            reason: "This Claude CLI does not support MCP Apps resource reads.".into(),
        }
    } else {
        ProviderMcpAppSupport::Unknown
    }
}

fn reason_text(
    row: Option<&ProviderMcpServer>,
    launches: &[ProviderMcpLaunchState],
    session: Option<Option<ProviderMcpStatus>>,
) -> String {
    let mut parts = Vec::new();
    if let Some(row) = row {
        parts.push(format!(
            "Claude reports it {} for this project{}.",
            row.status.label(),
            row.scope
                .as_deref()
                .map(|scope| format!(" ({scope} scope)"))
                .unwrap_or_default()
        ));
    }
    for launch in launches {
        let label = mode_label(&launch.modes);
        parts.push(match launch.status {
            Some(status) => format!("{label} chats: {}.", status.label()),
            None if launch.restricted => format!(
                "{label} chats: not loaded (LUNA starts these with Claude's restricted mode)."
            ),
            None => format!("{label} chats: not loaded."),
        });
    }
    match session {
        Some(Some(status)) => parts.push(format!(
            "The selected chat's Claude session reported it {} at start.",
            status.label()
        )),
        Some(None) => {
            parts.push("The selected chat's Claude session did not load it.".into())
        }
        None => parts.push(
            "Selected-chat attachment is unknown until that chat's Claude session starts while LUNA runs."
                .into(),
        ),
    }
    parts.join(" ")
}

/// Merge provider readbacks and the selected chat's session evidence into the
/// shared inventory view. Skills from the folder scan are left untouched.
pub(crate) fn merge_inventory(
    view: &mut IntegrationInventoryView,
    project_root: Option<&Path>,
    readbacks: &[McpLaunchReadback],
    session: Option<&SessionMcpObservation>,
) {
    // Most complete readback first: unrestricted, then restricted.
    let mut ordered: Vec<&McpLaunchReadback> = readbacks.iter().collect();
    ordered.sort_by_key(|readback| readback.restricted);
    let mut names: Vec<String> = Vec::new();
    for name in ordered
        .iter()
        .flat_map(|readback| readback.servers.iter().map(|row| row.name.clone()))
        .chain(
            session
                .into_iter()
                .flat_map(|session| session.servers.iter().map(|row| row.name.clone())),
        )
    {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let support = app_support(readbacks, session);
    for name in names {
        let row = ordered
            .iter()
            .find_map(|readback| readback.servers.iter().find(|row| row.name == name));
        let launches: Vec<ProviderMcpLaunchState> = readbacks
            .iter()
            .map(|readback| ProviderMcpLaunchState {
                modes: readback.modes.clone(),
                restricted: readback.restricted,
                status: readback
                    .servers
                    .iter()
                    .find(|row| row.name == name)
                    .map(|row| row.status),
            })
            .collect();
        let session_row = session.map(|session| {
            session
                .servers
                .iter()
                .find(|row| row.name == name)
                .map(|row| row.status)
        });
        let session_state = session.and_then(|session| {
            session
                .servers
                .iter()
                .find(|row| row.name == name)
                .map(|row| ProviderMcpSessionState {
                    status: row.status,
                    observed_at_ms: session.observed_at_ms,
                })
        });
        let status = row
            .map(|row| row.status)
            .or_else(|| session_state.as_ref().map(|state| state.status))
            .unwrap_or_default();
        let app_tools: Vec<_> = row
            .map(|row| {
                row.tools
                    .iter()
                    .filter(|tool| tool.ui_resource_uri.is_some())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let reason = reason_text(row, &launches, session_row);
        let source_scope = session.and_then(|session| {
            session
                .servers
                .iter()
                .find(|row| row.name == name)
                .and_then(|row| row.source.clone())
        });
        view.items.push(IntegrationInventoryItem {
            identity: Some(server_identity(&view.machine_id, project_root, &name)),
            name: name.clone(),
            kind: IntegrationKind::McpServer,
            source: if row.is_some() {
                "Claude CLI / mcp_status".into()
            } else {
                "Claude session start".into()
            },
            scope: row
                .and_then(|row| row.scope.clone())
                .or(source_scope)
                .unwrap_or_else(|| "unknown".into()),
            configured: row.is_some(),
            available: available(status),
            attached: session.map(|session| {
                session
                    .servers
                    .iter()
                    .any(|row| row.name == name && row.status == ProviderMcpStatus::Connected)
            }),
            reason: Some(reason),
            owner: IntegrationOwner::ProviderNative,
            actions: actions(row, project_root.is_some()),
            help_route: IntegrationHelpRoute::ProviderMcpConfig,
            mcp: Some(IntegrationMcpDetails {
                status,
                transport: row.and_then(|row| row.transport.clone()),
                server_version: row.and_then(|row| row.server_version.clone()),
                error: row.and_then(|row| row.error.clone()),
                error_code: row.and_then(|row| row.error_code.clone()),
                tool_count: row.map_or(0, |row| row.tools.len()),
                app_tools,
                launches,
                session: session_state,
                app_support: support.clone(),
            }),
        });
    }
    view.items.sort_by(|a, b| {
        (&a.kind, &a.scope, &a.name, &a.identity).cmp(&(&b.kind, &b.scope, &b.name, &b.identity))
    });
    view.help = "Skills are read from Claude's global and selected project's skill folders. MCP rows are Claude's own readback for the selected project, per LUNA launch mode, plus the selected chat's Claude session report when LUNA observed it. LUNA never registers, replaces or authenticates MCP servers; manage them in Claude or your MCP gateway.".into();
}

fn append_unavailable(view: &mut IntegrationInventoryView, reason: &str) {
    view.unavailable_reason = Some(match view.unavailable_reason.take() {
        Some(existing) => format!("{existing} {reason}"),
        None => reason.into(),
    });
}

/// Augment a Claude inventory view. `probe` runs the provider readback (one
/// short control-only Claude process per launch configuration).
pub(crate) async fn augment_inventory(
    view: &mut IntegrationInventoryView,
    harness: &dyn Harness,
    project_root: Option<&Path>,
    session: Option<SessionMcpObservation>,
    probe: bool,
) {
    let mut readbacks = Vec::new();
    if probe {
        match project_root.filter(|root| root.is_dir()) {
            None => append_unavailable(
                view,
                "Select a project to read Claude's effective MCP servers for it.",
            ),
            Some(root) => {
                let request = McpControlRequest {
                    cwd: root.to_path_buf(),
                    op: McpControlOp::Status {
                        modes: vec![
                            PermissionMode::Ask,
                            PermissionMode::Auto,
                            PermissionMode::Full,
                        ],
                    },
                };
                match tokio::time::timeout(READBACK_TIMEOUT, harness.mcp_control(request)).await {
                    Ok(Ok(McpControlReply::Status(found))) => {
                        if found.iter().any(|readback| !readback.settled) {
                            append_unavailable(
                                view,
                                "Some Claude MCP servers were still connecting when LUNA read them; check again for their final status.",
                            );
                        }
                        readbacks = found;
                    }
                    Ok(Ok(_)) => {
                        append_unavailable(view, "Claude returned an unexpected MCP readback.")
                    }
                    Ok(Err(error)) => append_unavailable(
                        view,
                        &format!("Claude MCP readback failed: {}", provider_message(error)),
                    ),
                    Err(_) => append_unavailable(view, "Claude MCP readback timed out."),
                }
            }
        }
    }
    if readbacks.is_empty() && session.is_none() {
        if !probe {
            view.help = format!(
                "{} Check provider reads Claude's effective MCP servers for the selected project.",
                view.help
            );
        }
        return;
    }
    merge_inventory(view, project_root, &readbacks, session.as_ref());
}

fn verified(server: &ProviderMcpServer, enabled: bool) -> bool {
    if enabled {
        server.status != ProviderMcpStatus::Disabled
    } else {
        server.status == ProviderMcpStatus::Disabled
    }
}

/// Provider-owned per-project enable/disable with a fresh readback.
pub(crate) async fn set_enabled(
    harness: &dyn Harness,
    machine_id: &str,
    project_root: Option<&Path>,
    identity: &str,
    server: &str,
    enabled: bool,
) -> Result<ProviderMcpActionReceipt, String> {
    let root = project_root
        .filter(|root| root.is_dir())
        .ok_or("Select a project: Claude saves enable/disable per project.")?;
    if server_identity(machine_id, Some(root), server) != identity {
        return Err("This MCP server identity does not belong to the selected project; check provider again.".into());
    }
    let _guard = ACTION_LOCK.lock().await;
    let request = McpControlRequest {
        cwd: root.to_path_buf(),
        op: McpControlOp::SetEnabled {
            server: server.to_owned(),
            enabled,
        },
    };
    let receipt = |outcome, observed_status, message: String| ProviderMcpActionReceipt {
        identity: identity.to_owned(),
        server: server.to_owned(),
        requested_enabled: enabled,
        outcome,
        observed_status,
        message,
    };
    let reply = match tokio::time::timeout(ACTION_TIMEOUT, harness.mcp_control(request)).await {
        Ok(Ok(reply)) => reply,
        // A spawn/initialize failure happens before the toggle is written.
        Ok(Err(error)) => {
            return Ok(receipt(
                ProviderMcpActionOutcome::Rejected,
                None,
                format!(
                    "Claude could not start the change: {}",
                    provider_message(error)
                ),
            ));
        }
        Err(_) => {
            return Ok(receipt(
                ProviderMcpActionOutcome::Indeterminate,
                None,
                "Claude did not answer in time; the change may have been applied. Check provider to see the current state.".into(),
            ));
        }
    };
    let McpControlReply::SetEnabled {
        outcome,
        readback,
        message,
    } = reply
    else {
        return Err("Claude returned an unexpected reply".into());
    };
    let observed = readback
        .as_ref()
        .and_then(|rows| rows.iter().find(|row| row.name == server));
    let verb = if enabled { "enabled" } else { "disabled" };
    Ok(match outcome {
        McpWriteOutcome::Rejected => receipt(
            ProviderMcpActionOutcome::Rejected,
            observed.map(|row| row.status),
            message.unwrap_or_else(|| "Claude rejected the change.".into()),
        ),
        McpWriteOutcome::Indeterminate => receipt(
            ProviderMcpActionOutcome::Indeterminate,
            None,
            message.unwrap_or_else(|| "The change may have been applied.".into()),
        ),
        McpWriteOutcome::Applied => match observed {
            Some(row) if verified(row, enabled) => receipt(
                ProviderMcpActionOutcome::Verified,
                Some(row.status),
                format!(
                    "Claude reports {server} {verb} for this project (now {}). {TOGGLE_EFFECT}",
                    row.status.label()
                ),
            ),
            Some(row) => receipt(
                ProviderMcpActionOutcome::Mismatch,
                Some(row.status),
                format!(
                    "Claude accepted the change, but now reports {server} {}.",
                    row.status.label()
                ),
            ),
            None => receipt(
                ProviderMcpActionOutcome::Indeterminate,
                None,
                "Claude accepted the change, but LUNA could not read it back. Check provider to see the current state.".into(),
            ),
        },
    })
}

/// Read one declared MCP Apps `ui://` resource as inert, inspectable facts.
pub(crate) async fn read_app_resource(
    harness: &dyn Harness,
    machine_id: &str,
    project_root: Option<&Path>,
    identity: &str,
    server: &str,
    uri: &str,
) -> Result<ProviderMcpAppResourceView, String> {
    let root = project_root
        .filter(|root| root.is_dir())
        .ok_or("Select a project to read its MCP App resources.")?;
    if server_identity(machine_id, Some(root), server) != identity {
        return Err("This MCP server identity does not belong to the selected project; check provider again.".into());
    }
    if !uri.starts_with("ui://") {
        return Err("Only MCP Apps ui:// resources can be inspected.".into());
    }
    let request = McpControlRequest {
        cwd: root.to_path_buf(),
        op: McpControlOp::ReadAppResource {
            server: server.to_owned(),
            uri: uri.to_owned(),
        },
    };
    match tokio::time::timeout(ACTION_TIMEOUT, harness.mcp_control(request)).await {
        Ok(Ok(McpControlReply::AppResource(mut view))) => {
            view.identity = identity.to_owned();
            Ok(view)
        }
        Ok(Ok(_)) => Err("Claude returned an unexpected reply".into()),
        Ok(Err(error)) => Err(provider_message(error)),
        Err(_) => Err("Claude did not return the MCP App resource in time.".into()),
    }
}

/// Params for the two provider MCP control RPCs (owner-local only).
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderMcpServerParams {
    pub provider: luna_proto::HarnessId,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub target_device_id: Option<String>,
    pub identity: String,
    pub server: String,
    /// `SetProviderMcpServerEnabled` only.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// `ReadProviderMcpAppResource` only.
    #[serde(default)]
    pub uri: Option<String>,
}

/// Route one provider MCP control RPC. Claude is the only implemented
/// provider; no other provider's method names are ever sent to Claude.
pub(crate) async fn dispatch(
    method: &str,
    params: ProviderMcpServerParams,
    harness: &dyn Harness,
    machine_id: &str,
    project_root: Option<&Path>,
) -> Result<serde_json::Value, String> {
    if params.provider != luna_proto::HarnessId::ClaudeCode {
        return Err("Provider MCP control is implemented for Claude only.".into());
    }
    let value = match method {
        luna_rpc::methods::SET_PROVIDER_MCP_SERVER_ENABLED => {
            let enabled = params.enabled.ok_or("enabled is required")?;
            serde_json::to_value(
                set_enabled(
                    harness,
                    machine_id,
                    project_root,
                    &params.identity,
                    &params.server,
                    enabled,
                )
                .await?,
            )
        }
        luna_rpc::methods::READ_PROVIDER_MCP_APP_RESOURCE => {
            let uri = params.uri.as_deref().ok_or("uri is required")?;
            serde_json::to_value(
                read_app_resource(
                    harness,
                    machine_id,
                    project_root,
                    &params.identity,
                    &params.server,
                    uri,
                )
                .await?,
            )
        }
        _ => return Err("unknown provider MCP method".into()),
    };
    value.map_err(|error| error.to_string())
}
