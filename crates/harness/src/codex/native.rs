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
use zeron_proto::{ProviderForkBoundary, ProviderForkRequest, ProviderForkResult};

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
    force_file_auth: bool,
}

impl CodexNativeClient {
    pub fn new(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
            executable: None,
            force_file_auth: false,
        }
    }

    pub fn with_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.executable = Some(executable.into());
        self
    }

    pub fn codex_home(&self) -> &std::path::Path {
        &self.codex_home
    }

    pub fn for_execution_home(
        &self,
        codex_home: impl Into<PathBuf>,
        force_file_auth: bool,
    ) -> Self {
        Self {
            codex_home: codex_home.into(),
            executable: self.executable.clone(),
            force_file_auth,
        }
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

    /// Fork a Codex rollout through the supported app-server operation. The
    /// independent `thread/read` below is the completion readback; the RPC
    /// response alone is only an acknowledgement that may race persistence.
    pub async fn fork_thread(
        &self,
        request: ProviderForkRequest,
    ) -> Result<ProviderForkResult, HarnessError> {
        let source_turns = self.thread_turns(&request.source_session_id).await?;
        let expected_source = codex_fork_prefix(&source_turns, &request.boundary)?;
        let params = fork_params(&request);
        let response = self.call("thread/fork", Value::Object(params)).await?;
        let thread = response.get("thread").unwrap_or(&response);
        let provider_session_id = string(thread, "id")?;
        if provider_session_id == request.source_session_id {
            return Err(HarnessError::Protocol(
                "thread/fork returned the source Codex thread id".into(),
            ));
        }
        let mut observed = None;
        for attempt in 0..5 {
            observed = self.observe_thread(&provider_session_id).await?;
            if observed.is_some() {
                break;
            }
            if attempt < 4 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        let Some(observed) = observed else {
            return Err(HarnessError::Protocol(
                "forked Codex thread was not readable after acknowledgement".into(),
            ));
        };
        if observed.id != provider_session_id {
            return Err(HarnessError::Protocol(format!(
                "thread/read returned {} for forked Codex thread {provider_session_id}",
                observed.id
            )));
        }
        let fork_turns = self.thread_turns(&provider_session_id).await?;
        let checkpoint_map = map_codex_fork_history(&expected_source, &fork_turns)?;
        Ok(ProviderForkResult {
            provider_session_id,
            checkpoint_map,
        })
    }

    async fn thread_turns(&self, thread_id: &str) -> Result<Vec<Value>, HarnessError> {
        let value = self
            .call(
                "thread/read",
                json!({"threadId": thread_id, "includeTurns": true}),
            )
            .await?;
        let thread = value.get("thread").unwrap_or(&value);
        thread
            .get("turns")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| HarnessError::Protocol("thread/read returned no turn history".into()))
    }

    /// Start asynchronous native compaction and wait for the provider's
    /// context-compaction completion item. The request response alone is only
    /// an acknowledgement and must not be presented as completion.
    pub async fn compact_thread(
        &self,
        thread_id: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<(), HarnessError> {
        let mut connection = self.connect(Some(cwd)).await?;
        let mut resume = serde_json::Map::new();
        resume.insert("threadId".into(), thread_id.into());
        if !cwd.is_empty() {
            resume.insert("cwd".into(), cwd.into());
        }
        if let Some(model) = model.filter(|model| !model.is_empty()) {
            resume.insert("model".into(), model.into());
        }
        let resumed = connection
            .client
            .request("thread/resume", Value::Object(resume))
            .await;
        let resumed = match resumed {
            Ok(resumed) => resumed,
            Err(error) => {
                connection.close().await;
                return Err(error);
            }
        };
        if let Err(error) = validate_compaction_resume(thread_id, &resumed) {
            connection.close().await;
            return Err(error);
        }
        let started = connection
            .client
            .request("thread/compact/start", json!({"threadId": thread_id}))
            .await;
        if let Err(error) = started {
            connection.close().await;
            return Err(error);
        }
        let mut tracker = NativeCompactTracker::default();
        while let Some((method, params)) = connection.notifications.recv().await {
            match tracker.notification(thread_id, &method, &params) {
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
        let mut connection = self.connect(None).await?;
        let outcome = connection.client.request(method, params).await;
        connection.close().await;
        outcome
    }

    async fn connect(&self, cwd: Option<&str>) -> Result<NativeConnection, HarnessError> {
        let executable = match &self.executable {
            Some(path) => path.clone(),
            None => resolve_codex_executable().ok_or_else(|| {
                HarnessError::NotInstalled("codex CLI not found for native reconciliation".into())
            })?,
        };
        let mut command = Command::new(&executable);
        if self.force_file_auth {
            super::apply_file_auth_override(&mut command);
        }
        command.arg("app-server");
        command.env("CODEX_HOME", &self.codex_home);
        if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
            command.current_dir(cwd);
        }
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

fn fork_params(request: &ProviderForkRequest) -> serde_json::Map<String, Value> {
    let mut params = serde_json::Map::new();
    params.insert("threadId".into(), request.source_session_id.clone().into());
    if !request.cwd.is_empty() {
        params.insert("cwd".into(), request.cwd.clone().into());
    }
    if let Some(model) = request.model.as_ref().filter(|model| !model.is_empty()) {
        params.insert("model".into(), model.clone().into());
    }
    match &request.boundary {
        ProviderForkBoundary::Tip => {}
        ProviderForkBoundary::BeforeCheckpoint { checkpoint_id } => {
            params.insert("beforeTurnId".into(), checkpoint_id.clone().into());
        }
        ProviderForkBoundary::ThroughCheckpoint { checkpoint_id } => {
            params.insert("lastTurnId".into(), checkpoint_id.clone().into());
        }
    }
    params
}

fn codex_turn_id(turn: &Value) -> Option<&str> {
    turn.get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

fn codex_fork_prefix(
    turns: &[Value],
    boundary: &ProviderForkBoundary,
) -> Result<Vec<Value>, HarnessError> {
    for turn in turns {
        codex_turn_id(turn).ok_or_else(|| {
            HarnessError::Protocol("Codex source history contains a turn without an id".into())
        })?;
    }
    match boundary {
        ProviderForkBoundary::Tip => Ok(turns.to_vec()),
        ProviderForkBoundary::BeforeCheckpoint { checkpoint_id } => {
            let position = turns
                .iter()
                .position(|turn| codex_turn_id(turn) == Some(checkpoint_id.as_str()))
                .ok_or_else(|| {
                    HarnessError::Protocol(format!(
                        "Codex checkpoint {checkpoint_id} is not in source thread"
                    ))
                })?;
            Ok(turns[..position].to_vec())
        }
        ProviderForkBoundary::ThroughCheckpoint { checkpoint_id } => {
            let position = turns
                .iter()
                .position(|turn| codex_turn_id(turn) == Some(checkpoint_id.as_str()))
                .ok_or_else(|| {
                    HarnessError::Protocol(format!(
                        "Codex checkpoint {checkpoint_id} is not in source thread"
                    ))
                })?;
            Ok(turns[..=position].to_vec())
        }
    }
}

fn codex_history_signature(value: &Value) -> Value {
    const VOLATILE: &[&str] = &[
        "id",
        "threadId",
        "turnId",
        "createdAt",
        "updatedAt",
        "startedAt",
        "completedAt",
    ];
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| !VOLATILE.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), codex_history_signature(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(codex_history_signature).collect()),
        other => other.clone(),
    }
}

fn map_codex_fork_history(
    source: &[Value],
    fork: &[Value],
) -> Result<std::collections::BTreeMap<String, String>, HarnessError> {
    if source.len() != fork.len() {
        return Err(HarnessError::Protocol(format!(
            "Codex fork history length mismatch: expected {}, observed {}",
            source.len(),
            fork.len()
        )));
    }
    let mut checkpoints = std::collections::BTreeMap::new();
    for (source, fork) in source.iter().zip(fork) {
        let source_id = codex_turn_id(source).ok_or_else(|| {
            HarnessError::Protocol("Codex source history contains a turn without an id".into())
        })?;
        let fork_id = codex_turn_id(fork).ok_or_else(|| {
            HarnessError::Protocol("Codex fork history contains a turn without an id".into())
        })?;
        if codex_history_signature(source) != codex_history_signature(fork) {
            return Err(HarnessError::Protocol(format!(
                "Codex fork history diverged at source turn {source_id}"
            )));
        }
        checkpoints.insert(source_id.to_owned(), fork_id.to_owned());
    }
    Ok(checkpoints)
}

fn validate_compaction_resume(thread_id: &str, response: &Value) -> Result<(), HarnessError> {
    let thread = response.get("thread").unwrap_or(response);
    let resumed_id = optional_string(thread, "id")
        .ok_or_else(|| HarnessError::Protocol("thread/resume response missing thread.id".into()))?;
    if resumed_id != thread_id {
        return Err(HarnessError::Protocol(format!(
            "thread/resume loaded {resumed_id} instead of bound thread {thread_id}"
        )));
    }
    if let Some(status) = thread
        .get("status")
        .and_then(|status| status.get("type"))
        .and_then(Value::as_str)
    {
        match status {
            "active" => {
                return Err(HarnessError::Protocol(
                    "bound Codex thread is active in another owner; compaction was not started"
                        .into(),
                ));
            }
            "notLoaded" | "systemError" | "unknown" => {
                return Err(HarnessError::Protocol(format!(
                    "bound Codex thread did not load for compaction: {status}"
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum CompactNotification {
    Completed,
    Failed(String),
}

#[derive(Default)]
struct NativeCompactTracker {
    active: Option<(String, String)>,
}

impl NativeCompactTracker {
    fn notification(
        &mut self,
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
        let turn_id = params
            .get("turnId")
            .or_else(|| params.get("turn_id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if matches!(method, "item/started" | "item/completed")
            && matches!(
                params.pointer("/item/type").and_then(Value::as_str),
                Some("contextCompaction" | "context_compaction")
            )
        {
            let item_id = params
                .pointer("/item/id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if turn_id.is_empty() || item_id.is_empty() {
                return None;
            }
            if method == "item/started" {
                if self.active.is_none() {
                    self.active = Some((turn_id.to_string(), item_id.to_string()));
                }
                return None;
            }
            if self
                .active
                .as_ref()
                .is_some_and(|(turn, item)| turn == turn_id && item == item_id)
            {
                self.active = None;
                return Some(CompactNotification::Completed);
            }
            return None;
        }
        if matches!(method, "turn/failed" | "turn/aborted")
            && self
                .active
                .as_ref()
                .is_some_and(|(turn, _)| turn == turn_id)
        {
            self.active = None;
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
        let mut tracker = NativeCompactTracker::default();
        assert_eq!(
            tracker.notification(
                "thread-1",
                "item/started",
                &json!({
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "item": { "id": "compact-1", "type": "contextCompaction" }
                }),
            ),
            None
        );
        assert_eq!(
            tracker.notification(
                "thread-1",
                "item/completed",
                &json!({
                    "threadId": "thread-2",
                    "turnId": "turn-1",
                    "item": { "id": "compact-1", "type": "contextCompaction" }
                }),
            ),
            None
        );
        assert_eq!(
            tracker.notification(
                "thread-1",
                "item/completed",
                &json!({
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "item": { "id": "compact-stale", "type": "contextCompaction" }
                }),
            ),
            None
        );
        assert_eq!(
            tracker.notification(
                "thread-1",
                "item/completed",
                &json!({
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "item": { "id": "compact-1", "type": "agentMessage" }
                }),
            ),
            None
        );
        assert_eq!(
            tracker.notification(
                "thread-1",
                "item/completed",
                &json!({
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "item": { "id": "compact-1", "type": "contextCompaction" }
                }),
            ),
            Some(CompactNotification::Completed)
        );

        let mut failed = NativeCompactTracker::default();
        failed.notification(
            "thread-1",
            "item/started",
            &json!({
                "threadId": "thread-1",
                "turnId": "turn-failed",
                "item": { "id": "compact-failed", "type": "contextCompaction" }
            }),
        );
        assert_eq!(
            failed.notification(
                "thread-1",
                "turn/failed",
                &json!({
                    "threadId": "thread-1",
                    "turnId": "turn-failed",
                    "turn": { "error": { "message": "provider failed" } }
                }),
            ),
            Some(CompactNotification::Failed("provider failed".into()))
        );
    }

    #[test]
    fn fork_params_keep_before_exclusive_and_through_inclusive() {
        let request = |boundary| ProviderForkRequest {
            source_session_id: "source".into(),
            boundary,
            cwd: "/workspace".into(),
            model: Some("gpt-5.6-sol".into()),
            provider_home: None,
            force_file_auth: false,
        };
        let before = fork_params(&request(ProviderForkBoundary::BeforeCheckpoint {
            checkpoint_id: "turn-2".into(),
        }));
        assert_eq!(
            before.get("beforeTurnId").and_then(Value::as_str),
            Some("turn-2")
        );
        assert!(!before.contains_key("lastTurnId"));

        let through = fork_params(&request(ProviderForkBoundary::ThroughCheckpoint {
            checkpoint_id: "turn-1".into(),
        }));
        assert_eq!(
            through.get("lastTurnId").and_then(Value::as_str),
            Some("turn-1")
        );
        assert!(!through.contains_key("beforeTurnId"));

        let source = vec![
            json!({"id":"turn-1","status":{"type":"completed"},"items":[{"id":"item-1","type":"agentMessage","text":"one"}]}),
            json!({"id":"turn-2","status":{"type":"completed"},"items":[{"id":"item-2","type":"agentMessage","text":"two"}]}),
        ];
        let prefix = codex_fork_prefix(
            &source,
            &ProviderForkBoundary::ThroughCheckpoint {
                checkpoint_id: "turn-1".into(),
            },
        )
        .unwrap();
        let fork = vec![
            json!({"id":"fork-turn-1","status":{"type":"completed"},"items":[{"id":"fork-item-1","type":"agentMessage","text":"one"}]}),
        ];
        assert_eq!(
            map_codex_fork_history(&prefix, &fork)
                .unwrap()
                .get("turn-1")
                .map(String::as_str),
            Some("fork-turn-1")
        );
        assert!(map_codex_fork_history(&prefix, &source).is_err());
    }
}
