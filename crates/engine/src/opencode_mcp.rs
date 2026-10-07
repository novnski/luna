//! OpenCode's MCP servers in the shared integration inventory, read-only
//! (OC-13/OC-14 parity with Claude's CC-11 rows in `claude_mcp`).
//!
//! Two sources, never mixed, exactly as for Claude: the provider readback is
//! `GET /api/mcp` from a short-lived OpenCode server in the selected project
//! (OpenCode loads the same configuration in every LUNA mode, so one readback
//! answers all of them), and the session evidence is the selected chat's own
//! OpenCode run readback at its start. Rows carry names, status words and
//! redacted error text only.
//!
//! LUNA never registers, replaces, enables, disables or authenticates an
//! OpenCode MCP server here; OpenCode owns that. LUNA's own `luna` server is
//! attached per run through an inline configuration layer, so it shows in a
//! session's readback and never in the project readback.

use std::path::Path;
use std::time::Duration;

use luna_harness::Harness;
use luna_harness::mcp_control::{
    McpControlOp, McpControlReply, McpControlRequest, McpLaunchReadback, ProviderMcpServer,
    SessionMcpObservation,
};
use luna_proto::{
    IntegrationActionAvailability, IntegrationActionFacts, IntegrationHelpRoute,
    IntegrationInventoryItem, IntegrationInventoryView, IntegrationKind, IntegrationMcpDetails,
    IntegrationOwner, PermissionMode, ProviderMcpAppSupport, ProviderMcpLaunchState,
    ProviderMcpSessionState, ProviderMcpStatus,
};
use sha2::{Digest, Sha256};

const READBACK_TIMEOUT: Duration = Duration::from_secs(45);
/// LUNA's own per-run server (`luna_harness::codex::luna_mcp::SERVER_NAME`).
const LUNA_SERVER: &str = "luna";
const READ_ONLY: &str = "OpenCode owns its MCP configuration and authentication; LUNA shows \
                         their status only. Manage servers in OpenCode or your MCP gateway.";

/// Opaque, source-scoped identity (the project root participates, as for
/// Claude's rows).
fn server_identity(machine_id: &str, project_root: Option<&Path>, name: &str) -> String {
    let mut hash = Sha256::new();
    let root = project_root
        .map(|root| root.to_string_lossy().into_owned())
        .unwrap_or_default();
    for value in ["luna-opencode-mcp-v1", machine_id, root.as_str(), name] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    format!("intg_mcp_v1_{:x}", hash.finalize())
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

fn provider_message(error: luna_harness::HarnessError) -> String {
    match error {
        luna_harness::HarnessError::Protocol(message) => message,
        other => other.to_string(),
    }
}

fn append_unavailable(view: &mut IntegrationInventoryView, reason: &str) {
    view.unavailable_reason = Some(match view.unavailable_reason.take() {
        Some(existing) => format!("{existing} {reason}"),
        None => reason.into(),
    });
}

fn reason_text(
    name: &str,
    row: Option<&ProviderMcpServer>,
    probed: bool,
    session: Option<Option<ProviderMcpStatus>>,
) -> String {
    let mut parts = Vec::new();
    if name == LUNA_SERVER {
        parts.push(
            "LUNA attaches its own luna server to each OpenCode run through an inline \
             configuration layer; it is never written to OpenCode's configuration."
                .to_owned(),
        );
    }
    match row {
        Some(row) => parts.push(format!(
            "OpenCode reports it {} for this project (every LUNA mode loads the same \
             configuration).",
            row.status.label()
        )),
        None if probed && name != LUNA_SERVER => {
            parts.push("OpenCode did not list it for this project.".into())
        }
        None => {}
    }
    match session {
        Some(Some(status)) => parts.push(format!(
            "The selected chat's OpenCode session reported it {} at start.",
            status.label()
        )),
        Some(None) => parts.push("The selected chat's OpenCode session did not load it.".into()),
        None => parts.push(
            "Selected-chat attachment is unknown until that chat's OpenCode session starts \
             while LUNA runs."
                .into(),
        ),
    }
    parts.join(" ")
}

/// Merge the project readback and the selected chat's session evidence into
/// the shared inventory view. Skills from the folder scan are left untouched.
pub(crate) fn merge_inventory(
    view: &mut IntegrationInventoryView,
    project_root: Option<&Path>,
    readback: Option<&McpLaunchReadback>,
    session: Option<&SessionMcpObservation>,
) {
    let mut names: Vec<String> = Vec::new();
    for name in readback
        .into_iter()
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
    for name in names {
        let row =
            readback.and_then(|readback| readback.servers.iter().find(|row| row.name == name));
        let launches: Vec<ProviderMcpLaunchState> = readback
            .map(|readback| ProviderMcpLaunchState {
                modes: readback.modes.clone(),
                restricted: false,
                status: row.map(|row| row.status),
            })
            .into_iter()
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
        view.items.push(IntegrationInventoryItem {
            identity: Some(server_identity(&view.machine_id, project_root, &name)),
            reason: Some(reason_text(&name, row, readback.is_some(), session_row)),
            source: if row.is_some() {
                "OpenCode /api/mcp (project readback)".into()
            } else {
                "OpenCode session start".into()
            },
            scope: if name == LUNA_SERVER {
                "dynamic".into()
            } else {
                "unknown".into()
            },
            name: name.clone(),
            kind: IntegrationKind::McpServer,
            configured: row.is_some(),
            available: available(status),
            attached: session.map(|session| {
                session
                    .servers
                    .iter()
                    .any(|row| row.name == name && row.status == ProviderMcpStatus::Connected)
            }),
            owner: IntegrationOwner::ProviderNative,
            actions: IntegrationActionFacts {
                enable: IntegrationActionAvailability::Unavailable,
                disable: IntegrationActionAvailability::Unavailable,
                authenticate: IntegrationActionAvailability::Unavailable,
                reason: Some(READ_ONLY.into()),
            },
            help_route: IntegrationHelpRoute::ProviderMcpConfig,
            mcp: Some(IntegrationMcpDetails {
                status,
                transport: None,
                server_version: None,
                error: row.and_then(|row| row.error.clone()),
                error_code: None,
                // OpenCode's status readback lists no per-server tools.
                tool_count: 0,
                app_tools: Vec::new(),
                launches,
                session: session_state,
                app_support: ProviderMcpAppSupport::Unavailable {
                    reason: "LUNA does not read MCP App resources through OpenCode.".into(),
                },
            }),
        });
    }
    view.items.sort_by(|a, b| {
        (&a.kind, &a.scope, &a.name, &a.identity).cmp(&(&b.kind, &b.scope, &b.name, &b.identity))
    });
    view.help = "Skills are read from OpenCode's global and selected project's skill folders. MCP \
                 rows are OpenCode's own status readback for the selected project, plus the \
                 selected chat's OpenCode session report when LUNA observed it. They are read-only: \
                 LUNA never registers, replaces or authenticates OpenCode MCP servers; manage them \
                 in OpenCode or your MCP gateway."
        .into();
}

/// Augment an OpenCode inventory view. `probe` runs the provider readback
/// (one short-lived OpenCode server in the selected project).
pub(crate) async fn augment_inventory(
    view: &mut IntegrationInventoryView,
    harness: &dyn Harness,
    project_root: Option<&Path>,
    session: Option<SessionMcpObservation>,
    probe: bool,
) {
    let mut readback = None;
    if probe {
        match project_root.filter(|root| root.is_dir()) {
            None => append_unavailable(
                view,
                "Select a project to read OpenCode's MCP servers for it.",
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
                                "Some OpenCode MCP servers were still connecting when LUNA read \
                                 them; check again for their final status.",
                            );
                        }
                        readback = found.into_iter().next();
                    }
                    Ok(Ok(_)) => {
                        append_unavailable(view, "OpenCode returned an unexpected MCP readback.")
                    }
                    Ok(Err(error)) => append_unavailable(
                        view,
                        &format!("OpenCode MCP readback failed: {}", provider_message(error)),
                    ),
                    Err(_) => append_unavailable(view, "OpenCode MCP readback timed out."),
                }
            }
        }
    }
    if readback.is_none() && session.is_none() {
        if !probe {
            view.help = format!(
                "{} Check provider reads OpenCode's MCP servers for the selected project.",
                view.help
            );
        }
        return;
    }
    merge_inventory(view, project_root, readback.as_ref(), session.as_ref());
}
