//! Native, per-process delivery of already enabled HTTP/SSE connections.
//! Definitions travel over the private control pipe, never argv or a new store.
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncWriteExt, BufReader, Lines};

use crate::HarnessError;
use crate::process::{ChildStdin, ChildStdout};

fn read(path: &Path) -> Result<Value, HarnessError> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Ok(meta) if meta.is_file() && meta.len() <= 1024 * 1024 => std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(|| {
                HarnessError::Protocol(
                    "Saved MCP configuration could not be read; check provider settings.".into(),
                )
            }),
        _ => Err(HarnessError::Protocol(
            "Saved MCP configuration is unavailable or too large; check provider settings.".into(),
        )),
    }
}

pub(super) fn selected(source: &Path, cwd: &Path) -> Result<Map<String, Value>, HarnessError> {
    let config = read(source)?;
    let cwd = super::profile::canonical_dir(cwd);
    let project = config
        .get("projects")
        .and_then(|projects| projects.get(cwd.to_string_lossy().as_ref()));
    let mut servers = config
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    // A project file is selected only by Claude's saved consent, not by presence.
    let approved = project
        .and_then(|p| p.get("enabledMcpjsonServers"))
        .and_then(Value::as_array);
    let all_approved = project
        .and_then(|p| p.get("enableAllProjectMcpServers"))
        .and_then(Value::as_bool)
        == Some(true);
    if all_approved || approved.is_some_and(|names| !names.is_empty()) {
        let project_file = read(&cwd.join(".mcp.json"))?;
        if let Some(entries) = project_file.get("mcpServers").and_then(Value::as_object) {
            servers.extend(
                entries
                    .iter()
                    .filter(|(name, _)| {
                        all_approved
                            || approved.is_some_and(|names| {
                                names.iter().any(|n| n.as_str() == Some(name.as_str()))
                            })
                    })
                    .map(|(name, config)| (name.clone(), config.clone())),
            );
        }
    }
    if let Some(local) = project
        .and_then(|p| p.get("mcpServers"))
        .and_then(Value::as_object)
    {
        servers.extend(local.clone());
    }
    let disabled: Vec<_> = [Some(&config), project]
        .into_iter()
        .flatten()
        .flat_map(|scope| {
            ["disabledMcpServers", "disabledMcpjsonServers"]
                .into_iter()
                .filter_map(move |key| scope.get(key).and_then(Value::as_array))
        })
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    servers.retain(|name, definition| {
        name != "luna"
            && crate::mcp_isolation::scoped_server_allowed(name)
            && !disabled.contains(&name.as_str())
            && matches!(
                definition.get("type").and_then(Value::as_str),
                Some("http" | "sse")
            )
    });
    if servers.len() > 64
        || servers.keys().any(|name| {
            name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        })
    {
        return Err(HarnessError::Protocol(
            "Saved MCP connection names or count are unsupported; check provider settings.".into(),
        ));
    }
    Ok(servers)
}

fn revision(servers: &Map<String, Value>) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(servers).unwrap_or_default())
    )
}

/// Only identity and a hash survive registration; no credentials are retained.
#[derive(Clone)]
pub(super) struct Guard {
    source: PathBuf,
    cwd: PathBuf,
    revision: String,
    names: Vec<String>,
}
impl Guard {
    pub(super) fn new(source: PathBuf, cwd: PathBuf, servers: &Map<String, Value>) -> Self {
        Self {
            source,
            cwd,
            revision: revision(servers),
            names: servers.keys().cloned().collect(),
        }
    }
    pub(super) fn owns(&self, tool: &str) -> bool {
        self.names
            .iter()
            .any(|server| tool.starts_with(&format!("mcp__{server}__")))
    }
    pub(super) fn current(&self) -> bool {
        selected(&self.source, &self.cwd).is_ok_and(|servers| revision(&servers) == self.revision)
    }
}

/// This installed SDK control affects only its managed servers; argv's `luna`
/// endpoint remains attached. No user prompt is sent until the ack arrives.
pub(super) async fn attach(
    stdin: &mut ChildStdin,
    lines: &mut Lines<BufReader<ChildStdout>>,
    servers: Map<String, Value>,
) -> Result<(), HarnessError> {
    let id = uuid::Uuid::new_v4().to_string();
    let request = json!({"type":"control_request", "request_id":id, "request":{"subtype":"mcp_set_servers", "servers":servers}});
    stdin.write_all(format!("{request}\n").as_bytes()).await?;
    stdin.flush().await?;
    let response = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(line) = lines.next_line().await? {
            let Ok(frame) = serde_json::from_str::<Value>(&line) else { continue };
            let response = &frame["response"];
            if frame["type"] == "control_response" && response["request_id"] == id {
                return Ok::<_, std::io::Error>(Some(response.clone()));
            }
            // Authentication/elicitation cannot acquire a grant before a root turn.
            if frame["type"] == "control_request" {
                let refusal = json!({"type":"control_response","response":{"subtype":"error","request_id":frame["request_id"],"error":"Reconnect this MCP connection through the provider's settings before starting the chat."}});
                stdin.write_all(format!("{refusal}\n").as_bytes()).await?;
                stdin.flush().await?;
            }
        }
        Ok(None)
    }).await.map_err(|_| HarnessError::Protocol("Saved MCP attachment timed out; check provider connections and resume.".into()))??;
    if response.as_ref().is_none_or(|r| {
        r["subtype"] != "success"
            || r["response"]["errors"]
                .as_object()
                .is_some_and(|errors| !errors.is_empty())
    }) {
        return Err(HarnessError::Protocol("Saved MCP connections could not be attached by this CLI; check provider connections and resume.".into()));
    }
    Ok(())
}
