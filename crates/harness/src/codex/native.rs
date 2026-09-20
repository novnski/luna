//! Bounded Codex app-server metadata operations for native-thread reconciliation.
//!
//! This client never opens rollout files or Codex databases. Every operation
//! goes through the installed app-server contract and an explicit `CODEX_HOME`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

use crate::codex::resolve_codex_executable;
use crate::jsonrpc::{Incoming, RpcClient};
use crate::process::{Child, Command, Stdio};
use crate::{HarnessError, compose_child_path, shutdown_child};

pub const VALIDATED_CODEX_CLI_VERSION: &str = "0.154.0";
pub const NATIVE_CAPABILITIES: &[&str] = &[
    "thread/start",
    "thread/resume",
    "thread/read",
    "thread/list",
    "thread/name/set",
    "thread/archive",
    "thread/unarchive",
    "thread/fork",
    "thread/compact/start",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexThreadObservation {
    pub id: String,
    pub name: Option<String>,
    pub cwd: String,
    pub status: String,
    pub archived: Option<bool>,
    pub updated_at: Option<i64>,
    pub cli_version: Option<String>,
    pub project_id: Option<String>,
    pub ephemeral: bool,
}

/// Short-lived supported-API client scoped to one provider home.
#[derive(Debug, Clone)]
pub struct CodexNativeClient {
    codex_home: PathBuf,
    executable: Option<PathBuf>,
}

impl CodexNativeClient {
    pub fn new(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
            executable: None,
        }
    }

    pub fn with_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.executable = Some(executable.into());
        self
    }

    pub fn codex_home(&self) -> &std::path::Path {
        &self.codex_home
    }

    pub async fn observe_thread(
        &self,
        thread_id: &str,
    ) -> Result<Option<CodexThreadObservation>, HarnessError> {
        let value = match self
            .call(
                "thread/read",
                json!({"threadId": thread_id, "includeTurns": false}),
            )
            .await
        {
            Ok(value) => value,
            Err(error) if missing_thread(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let thread = value.get("thread").unwrap_or(&value);
        let id = string(thread, "id")?;
        let name = optional_string(thread, "name");
        let archived = if self
            .listed_with_archived(&id, name.as_deref(), true)
            .await?
        {
            Some(true)
        } else if self
            .listed_with_archived(&id, name.as_deref(), false)
            .await?
        {
            Some(false)
        } else {
            None
        };
        Ok(Some(CodexThreadObservation {
            id,
            name,
            cwd: optional_string(thread, "cwd").unwrap_or_default(),
            status: thread
                .get("status")
                .and_then(|status| status.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            archived,
            updated_at: thread.get("updatedAt").and_then(Value::as_i64),
            cli_version: optional_string(thread, "cliVersion"),
            project_id: optional_string(thread, "projectId"),
            ephemeral: thread
                .get("ephemeral")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }))
    }

    pub async fn set_name(&self, thread_id: &str, name: &str) -> Result<(), HarnessError> {
        self.call(
            "thread/name/set",
            json!({"threadId": thread_id, "name": name}),
        )
        .await?;
        Ok(())
    }

    pub async fn set_archived(&self, thread_id: &str, archived: bool) -> Result<(), HarnessError> {
        let method = if archived {
            "thread/archive"
        } else {
            "thread/unarchive"
        };
        self.call(method, json!({"threadId": thread_id})).await?;
        Ok(())
    }

    /// Start asynchronous native compaction and wait for the provider's
    /// context-compaction completion item. The request response alone is only
    /// an acknowledgement and must not be presented as completion.
    pub async fn compact_thread(&self, thread_id: &str) -> Result<(), HarnessError> {
        let mut connection = self.connect().await?;
        let started = connection
            .client
            .request("thread/compact/start", json!({"threadId": thread_id}))
            .await;
        if let Err(error) = started {
            connection.close().await;
            return Err(error);
        }
        while let Some((method, params)) = connection.notifications.recv().await {
            match compact_notification(thread_id, &method, &params) {
                Some(CompactNotification::Completed) => {
                    connection.close().await;
                    return Ok(());
                }
                Some(CompactNotification::Failed(detail)) => {
                    connection.close().await;
                    return Err(HarnessError::Protocol(detail));
                }
                None => {}
            }
        }
        connection.close().await;
        Err(HarnessError::Protocol(
            "Codex compaction completion became unknown".into(),
        ))
    }

    async fn listed_with_archived(
        &self,
        thread_id: &str,
        name: Option<&str>,
        archived: bool,
    ) -> Result<bool, HarnessError> {
        let mut params = serde_json::Map::new();
        params.insert("archived".into(), archived.into());
        params.insert("limit".into(), 100.into());
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            params.insert("searchTerm".into(), name.into());
        }
        let page = self.call("thread/list", Value::Object(params)).await?;
        Ok(page
            .get("data")
            .and_then(Value::as_array)
            .is_some_and(|threads| {
                threads
                    .iter()
                    .any(|thread| thread.get("id").and_then(Value::as_str) == Some(thread_id))
            }))
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, HarnessError> {
        let mut connection = self.connect().await?;
        let outcome = connection.client.request(method, params).await;
        connection.close().await;
        outcome
    }

    async fn connect(&self) -> Result<NativeConnection, HarnessError> {
        let executable = match &self.executable {
            Some(path) => path.clone(),
            None => resolve_codex_executable().ok_or_else(|| {
                HarnessError::NotInstalled("codex CLI not found for native reconciliation".into())
            })?,
        };
        let mut command = Command::new(&executable);
        command.arg("app-server");
        command.env("CODEX_HOME", &self.codex_home);
        compose_child_path(&mut command, &executable);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Protocol("codex child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("codex child has no stdout".into()))?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "zeron_harness::codex_native", "stderr: {line}");
                }
            });
        }
        let (client, mut incoming) = RpcClient::new(stdin, stdout);
        let responder = client.clone();
        let (notification_tx, notification_rx) = mpsc::channel(64);
        let drain = tokio::spawn(async move {
            while let Some(message) = incoming.recv().await {
                match message {
                    Incoming::Request { id, method, .. } => responder.respond_error(
                        &id,
                        -32601,
                        &format!("unsupported reconciliation request: {method}"),
                    ),
                    Incoming::Notification { method, params } => {
                        tracing::trace!(target: "zeron_harness::codex_native", %method, ?params, "notification");
                        let _ = notification_tx.send((method, params)).await;
                    }
                    Incoming::Eof => break,
                }
            }
        });
        if let Err(error) = client
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "luna-reconciler",
                        "title": "LUNA",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {"experimentalApi": true}
                }),
            )
            .await
        {
            shutdown_child(&mut child, std::time::Duration::from_millis(500)).await;
            drain.abort();
            let _ = drain.await;
            return Err(error);
        }
        client.notify("initialized", None);
        Ok(NativeConnection {
            child,
            client,
            drain,
            notifications: notification_rx,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CompactNotification {
    Completed,
    Failed(String),
}

fn compact_notification(
    thread_id: &str,
    method: &str,
    params: &Value,
) -> Option<CompactNotification> {
    let event_thread = params
        .get("threadId")
        .or_else(|| params.pointer("/thread/id"))
        .and_then(Value::as_str);
    if event_thread != Some(thread_id) {
        return None;
    }
    if method == "item/completed"
        && params.pointer("/item/type").and_then(Value::as_str) == Some("contextCompaction")
    {
        return Some(CompactNotification::Completed);
    }
    if matches!(method, "turn/failed" | "turn/aborted") {
        return Some(CompactNotification::Failed(
            params
                .pointer("/turn/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Codex compaction did not complete")
                .to_string(),
        ));
    }
    None
}

struct NativeConnection {
    child: Child,
    client: RpcClient,
    drain: tokio::task::JoinHandle<()>,
    notifications: mpsc::Receiver<(String, Value)>,
}

impl NativeConnection {
    async fn close(&mut self) {
        shutdown_child(&mut self.child, std::time::Duration::from_millis(500)).await;
        self.drain.abort();
        let _ = (&mut self.drain).await;
    }
}

fn optional_string(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn string(value: &Value, field: &str) -> Result<String, HarnessError> {
    optional_string(value, field).ok_or_else(|| {
        HarnessError::Protocol(format!("thread/read response missing thread.{field}"))
    })
}

fn missing_thread(error: &HarnessError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    [
        "not found",
        "no rollout",
        "unknown thread",
        "thread does not exist",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_completion_requires_matching_native_context_item() {
        assert_eq!(
            compact_notification(
                "thread-1",
                "item/completed",
                &json!({
                    "threadId": "thread-1",
                    "item": { "type": "contextCompaction" }
                }),
            ),
            Some(CompactNotification::Completed)
        );
        assert_eq!(
            compact_notification(
                "thread-1",
                "item/completed",
                &json!({
                    "threadId": "thread-2",
                    "item": { "type": "contextCompaction" }
                }),
            ),
            None
        );
        assert_eq!(
            compact_notification(
                "thread-1",
                "item/completed",
                &json!({ "item": { "type": "contextCompaction" } }),
            ),
            None
        );
        assert_eq!(
            compact_notification(
                "thread-1",
                "item/completed",
                &json!({
                    "threadId": "thread-1",
                    "item": { "type": "agentMessage" }
                }),
            ),
            None
        );
        assert_eq!(
            compact_notification(
                "thread-1",
                "turn/failed",
                &json!({
                    "threadId": "thread-1",
                    "turn": { "error": { "message": "provider failed" } }
                }),
            ),
            Some(CompactNotification::Failed("provider failed".into()))
        );
    }
}
