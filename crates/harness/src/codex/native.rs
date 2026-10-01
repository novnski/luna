//! Bounded Codex app-server metadata operations for native-thread reconciliation.
//!
//! This client never opens rollout files or Codex databases. Every operation
//! goes through the installed app-server contract and an explicit `CODEX_HOME`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

use crate::codex::resolve_codex_executable;
use crate::jsonrpc::{Incoming, RpcClient};
use crate::process::{Child, Command, Stdio};
use crate::{
    HarnessError, ProviderForkProgress, ProviderForkProgressReporter, compose_child_path,
    shutdown_child,
};
use luna_proto::{ProviderForkBoundary, ProviderForkRequest, ProviderForkResult};

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
    "thread/revert",
    "thread/turns/list",
    "thread/compact/start",
    "review/start",
];

#[derive(Debug, Clone, PartialEq)]
pub struct CodexQuotaWindow {
    pub limit_id: String,
    pub limit_name: Option<String>,
    pub position: &'static str,
    pub used_percent: f32,
    pub duration_minutes: Option<u64>,
    pub resets_at_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexQuotaRead {
    /// Backend `accountId` when supplied, SHA-256 hashed before leaving this adapter.
    /// Email from `account/read` is never used as a de-duplication identity.
    pub verified_account_id: Option<String>,
    /// Display only. This is not used as the quota cache identity.
    pub account_label: Option<String>,
    pub plan: Option<String>,
    pub checked_at_ms: i64,
    pub windows: Vec<CodexQuotaWindow>,
    pub reset_credits: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CodexQuotaObservation {
    Ready(CodexQuotaRead),
    Unsupported,
    /// A previously verified account can retain its own last good bars.
    Failed {
        verified_account_id: Option<String>,
    },
}

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

/// Bounded, provider-reported catalog entry. A skill being enabled in this
/// response does not prove that an existing thread has loaded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSkillCatalogEntry {
    pub name: String,
    pub path: PathBuf,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexMcpConfigEntry {
    pub name: String,
    pub enabled: bool,
}

/// Durable, exact history expectations for a same-thread revert. Stored only
/// on the execution host; raw provider items never enter synced transcript text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRewindPlan {
    pub thread_id: String,
    pub before_turn_id: String,
    pub original: Vec<Value>,
    pub retained: Vec<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexRewindProgress {
    MutationStarted,
    Acknowledged,
}

pub type CodexRewindProgressReporter =
    dyn Fn(CodexRewindProgress) -> Result<(), HarnessError> + Send + Sync;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexRewindObservation {
    Original,
    Reverted,
    Diverged,
}

/// Short-lived supported-API client scoped to one provider home.
#[derive(Debug, Clone)]
pub struct CodexNativeClient {
    codex_home: PathBuf,
    executable: Option<PathBuf>,
    force_file_auth: bool,
}

impl CodexNativeClient {
    /// Read installed plugins/apps/skills without starting or resuming a thread.
    pub async fn read_composer_catalog(
        &self,
        cwd: &Path,
        thread_id: Option<&str>,
    ) -> Result<luna_proto::ComposerCatalog, HarnessError> {
        let cwd = cwd.canonicalize().map_err(HarnessError::Io)?;
        let mut connection = self.connect(cwd.to_str()).await?;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            super::composer_catalog::read(&connection.client, &cwd, thread_id),
        )
        .await
        .map_err(|_| HarnessError::Protocol("composer discovery timed out".into()));
        connection.close().await;
        result?
    }

    /// Do not cold-retry a failed live mutation: its delivery can be unknown.
    /// Cold controls do not load/resume a thread or start a model turn.
    pub async fn goal_operation(
        &self,
        thread: &str,
        cwd: &str,
        operation: &luna_proto::GoalOperation,
    ) -> Result<(Option<luna_proto::ThreadGoal>, bool), HarnessError> {
        if let Some(client) = super::goals::live_client(&self.codex_home, thread) {
            return super::goals::execute(&client, thread, operation)
                .await
                .map(|goal| (goal, true));
        }
        let mut connection = self.connect(Some(cwd)).await?;
        let result = super::goals::execute(&connection.client, thread, operation).await;
        connection.close().await;
        result.map(|goal| (goal, false))
    }

    /// Read the provider's native catalog for one explicit workspace and home.
    /// No thread is started or resumed. The result never asserts attachment.
    pub async fn read_skill_catalog(
        &self,
        cwd: &Path,
    ) -> Result<Vec<CodexSkillCatalogEntry>, HarnessError> {
        let canonical_cwd = cwd.canonicalize().map_err(HarnessError::Io)?;
        let cwd = canonical_cwd
            .to_str()
            .ok_or_else(|| HarnessError::Protocol("Codex skill workspace is not UTF-8".into()))?;
        let mut connection = self.connect(Some(cwd)).await?;
        let response = connection
            .client
            .request("skills/list", json!({"cwds": [cwd]}))
            .await;
        connection.close().await;
        parse_skill_catalog(&response?, cwd)
    }

    /// Read provider MCP configuration for an explicit workspace. Codex reads
    /// project config relative to the app-server process cwd; `config/read`
    /// has no separate cwd parameter in this adapter's validated contract.
    /// Commands, environment and auth fields never leave this method.
    pub async fn read_mcp_config(
        &self,
        cwd: &Path,
    ) -> Result<Vec<CodexMcpConfigEntry>, HarnessError> {
        let canonical_cwd = cwd.canonicalize().map_err(HarnessError::Io)?;
        let cwd = canonical_cwd
            .to_str()
            .ok_or_else(|| HarnessError::Protocol("Codex MCP workspace is not UTF-8".into()))?;
        let mut connection = self.connect(Some(cwd)).await?;
        let response = connection
            .client
            .request("config/read", json!({"includeLayers": false}))
            .await;
        connection.close().await;
        parse_mcp_config(&response?)
    }
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

    /// Read only through Codex's supported app-server account API. The same
    /// connection brackets the quota read with non-refreshing account reads;
    /// a changed identity never contributes a snapshot or a last-good match.
    pub async fn read_account_rate_limits(&self) -> Result<CodexQuotaObservation, HarnessError> {
        let mut connection = self.connect(None).await?;
        let result = async {
            let before = connection
                .client
                .request("account/read", json!({"refreshToken": false}))
                .await?;
            let Some(account) = codex_account_marker(&before) else {
                return Ok(CodexQuotaObservation::Unsupported);
            };
            let limits = match connection
                .client
                .request(
                    "account/rateLimits/read",
                    json!({"excludeResetCreditDetails": true}),
                )
                .await
            {
                Ok(limits) => limits,
                Err(_) => {
                    return Ok(CodexQuotaObservation::Failed {
                        verified_account_id: None,
                    });
                }
            };
            let after = connection
                .client
                .request("account/read", json!({"refreshToken": false}))
                .await;
            if after
                .ok()
                .and_then(|value| codex_account_marker(&value))
                .is_none_or(|observed| observed.signature != account.signature)
            {
                return Ok(CodexQuotaObservation::Failed {
                    verified_account_id: None,
                });
            }
            let has_limits = limits
                .get("rateLimits")
                .is_some_and(|value| !value.is_null())
                || limits
                    .get("rateLimitsByLimitId")
                    .is_some_and(|value| !value.is_null());
            if !has_limits {
                return Ok(CodexQuotaObservation::Unsupported);
            }
            let Some((windows, reset_credits, limit_plan)) = codex_quota_windows(&limits) else {
                return Ok(CodexQuotaObservation::Failed {
                    verified_account_id: quota_account_id(&limits),
                });
            };
            let checked_at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() as i64);
            Ok(CodexQuotaObservation::Ready(CodexQuotaRead {
                verified_account_id: quota_account_id(&limits),
                account_label: account.email,
                plan: limit_plan.or(account.plan),
                checked_at_ms,
                windows,
                reset_credits,
            }))
        }
        .await;
        connection.close().await;
        result
    }

    pub async fn observe_thread(
        &self,
        thread_id: &str,
    ) -> Result<Option<CodexThreadObservation>, HarnessError> {
        self.observe_thread_archive_state(thread_id, true).await
    }

    /// Reconciliation of a LUNA task that is not archived only needs to know
    /// whether the provider thread is archived: an unarchived or unlisted
    /// thread cannot conflict with it. Skipping the unarchived `thread/list`
    /// search avoids the slowest native read (it scans every session in the
    /// home) and leaves `archived` as `None` (not proven) instead of
    /// `Some(false)`. Archived tasks still prove both states.
    pub async fn observe_thread_for_task(
        &self,
        thread_id: &str,
        task_archived: bool,
    ) -> Result<Option<CodexThreadObservation>, HarnessError> {
        self.observe_thread_archive_state(thread_id, task_archived)
            .await
    }

    async fn observe_thread_archive_state(
        &self,
        thread_id: &str,
        prove_unarchived: bool,
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
        } else if prove_unarchived
            && self
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
        self.fork_thread_staged(request, &|_| Ok(())).await
    }

    pub async fn fork_thread_staged(
        &self,
        request: ProviderForkRequest,
        progress: &ProviderForkProgressReporter,
    ) -> Result<ProviderForkResult, HarnessError> {
        let source_turns = self.thread_turns(&request.source_session_id).await?;
        let expected_source = codex_fork_prefix(&source_turns, &request.boundary)?;
        let params = fork_params(&request);
        progress(ProviderForkProgress::MutationStarted)?;
        let response = self.call("thread/fork", Value::Object(params)).await?;
        let thread = response.get("thread").unwrap_or(&response);
        let provider_session_id = string(thread, "id")?;
        if provider_session_id == request.source_session_id {
            return Err(HarnessError::Protocol(
                "thread/fork returned the source Codex thread id".into(),
            ));
        }
        progress(ProviderForkProgress::Acknowledged {
            provider_session_id: provider_session_id.clone(),
        })?;
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
        if let Some(name) = request.target_name.as_deref() {
            self.set_name(&provider_session_id, name).await?;
            let mut renamed = false;
            for attempt in 0..5 {
                if self
                    .observe_thread(&provider_session_id)
                    .await?
                    .is_some_and(|thread| thread.name.as_deref() == Some(name))
                {
                    renamed = true;
                    break;
                }
                if attempt < 4 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
            if !renamed {
                return Err(HarnessError::Protocol(
                    "forked Codex thread title did not match after rename".into(),
                ));
            }
        }
        Ok(ProviderForkResult {
            provider_session_id,
            checkpoint_map,
        })
    }

    /// Read the entire bounded persisted history in ascending order. The
    /// metadata read proves paginated/non-ephemeral history before mutation.
    pub async fn prepare_rewind(
        &self,
        thread_id: &str,
        before_turn_id: &str,
        cwd: &str,
    ) -> Result<CodexRewindPlan, HarnessError> {
        let mut connection = self.connect(Some(cwd)).await?;
        let outcome = async {
            let original = read_rewind_history(&connection.client, thread_id).await?;
            let retained = codex_fork_prefix(
                &original,
                &ProviderForkBoundary::BeforeCheckpoint {
                    checkpoint_id: before_turn_id.into(),
                },
            )?;
            Ok(CodexRewindPlan {
                thread_id: thread_id.into(),
                before_turn_id: before_turn_id.into(),
                original,
                retained,
            })
        }
        .await;
        connection.close().await;
        outcome
    }

    /// The engine persists the plan and a send blocker before this call.
    /// Once MutationStarted is reported, any failure is conservatively unknown.
    pub async fn revert_thread(
        &self,
        plan: &CodexRewindPlan,
        cwd: &str,
        progress: &CodexRewindProgressReporter,
    ) -> Result<(), HarnessError> {
        let mut connection = self.connect(Some(cwd)).await?;
        let outcome = async {
            let resumed = connection
                .client
                .request(
                    "thread/resume",
                    json!({
                        "threadId": plan.thread_id, "cwd": cwd, "excludeTurns": true
                    }),
                )
                .await?;
            validate_compaction_resume(&plan.thread_id, &resumed)?;
            let observed = read_rewind_history(&connection.client, &plan.thread_id).await?;
            if !same_rewind_history(&observed, &plan.original) {
                return Err(HarnessError::Protocol(
                    "Codex history changed after rewind review; revert was not sent".into(),
                ));
            }
            progress(CodexRewindProgress::MutationStarted)?;
            let response = connection
                .client
                .request(
                    "thread/revert",
                    json!({
                        "threadId": plan.thread_id, "beforeTurnId": plan.before_turn_id
                    }),
                )
                .await?;
            if response.pointer("/thread/id").and_then(Value::as_str)
                != Some(plan.thread_id.as_str())
            {
                return Err(HarnessError::Protocol(
                    "thread/revert returned a different thread".into(),
                ));
            }
            progress(CodexRewindProgress::Acknowledged)?;
            let observed = read_rewind_history(&connection.client, &plan.thread_id).await?;
            if !same_rewind_history(&observed, &plan.retained) {
                return Err(HarnessError::Protocol(
                    "Codex retained history did not match the exact rewind prefix".into(),
                ));
            }
            Ok(())
        }
        .await;
        connection.close().await;
        outcome
    }

    /// Restart/lost-response recovery is read-only. Never resubmit thread/revert.
    pub async fn observe_rewind(
        &self,
        plan: &CodexRewindPlan,
        cwd: &str,
    ) -> Result<CodexRewindObservation, HarnessError> {
        let mut connection = self.connect(Some(cwd)).await?;
        let outcome = read_rewind_history(&connection.client, &plan.thread_id).await;
        connection.close().await;
        let observed = outcome?;
        Ok(if same_rewind_history(&observed, &plan.retained) {
            CodexRewindObservation::Reverted
        } else if same_rewind_history(&observed, &plan.original) {
            CodexRewindObservation::Original
        } else {
            CodexRewindObservation::Diverged
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
                    tracing::debug!(target: "luna_harness::codex_native", "stderr: {line}");
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
                        tracing::trace!(target: "luna_harness::codex_native", %method, ?params, "notification");
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

fn parse_skill_catalog(
    response: &Value,
    cwd: &str,
) -> Result<Vec<CodexSkillCatalogEntry>, HarnessError> {
    let groups = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| HarnessError::Protocol("Codex skills/list has no data array".into()))?;
    if groups.len() > 64 {
        return Err(HarnessError::Protocol(
            "Codex skills/list exceeds group limit".into(),
        ));
    }
    let group = groups
        .iter()
        .find(|group| group.get("cwd").and_then(Value::as_str) == Some(cwd))
        .ok_or_else(|| HarnessError::Protocol("Codex skills/list omitted requested cwd".into()))?;
    let skills = group
        .get("skills")
        .and_then(Value::as_array)
        .ok_or_else(|| HarnessError::Protocol("Codex skills/list has no skills array".into()))?;
    if skills.len() > 256 {
        return Err(HarnessError::Protocol(
            "Codex skills/list exceeds skill limit".into(),
        ));
    }
    let mut entries = Vec::new();
    for skill in skills {
        let (Some(name), Some(path), Some(enabled)) = (
            skill.get("name").and_then(Value::as_str),
            skill.get("path").and_then(Value::as_str),
            skill.get("enabled").and_then(Value::as_bool),
        ) else {
            return Err(HarnessError::Protocol(
                "Codex skill entry lacks identity or enabled state".into(),
            ));
        };
        if name.is_empty()
            || name.len() > 128
            || !name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
            })
            || path.len() > 2048
            || !Path::new(path).is_absolute()
        {
            return Err(HarnessError::Protocol(
                "Codex skill entry has invalid identity".into(),
            ));
        }
        entries.push(CodexSkillCatalogEntry {
            name: name.into(),
            path: path.into(),
            enabled,
        });
    }
    Ok(entries)
}

fn parse_mcp_config(response: &Value) -> Result<Vec<CodexMcpConfigEntry>, HarnessError> {
    let config = response
        .get("config")
        .and_then(Value::as_object)
        .ok_or_else(|| HarnessError::Protocol("Codex config/read has no config object".into()))?;
    let Some(servers) = config.get("mcp_servers") else {
        return Ok(Vec::new());
    };
    let servers = servers
        .as_object()
        .ok_or_else(|| HarnessError::Protocol("Codex MCP configuration is malformed".into()))?;
    if servers.len() > 256 {
        return Err(HarnessError::Protocol(
            "Codex MCP configuration exceeds server limit".into(),
        ));
    }
    let mut entries = Vec::with_capacity(servers.len());
    for (name, value) in servers {
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err(HarnessError::Protocol(
                "Codex MCP server has invalid name".into(),
            ));
        }
        let enabled = value
            .as_object()
            .ok_or_else(|| HarnessError::Protocol("Codex MCP server entry is malformed".into()))?
            .get("enabled")
            .map(|enabled| {
                enabled.as_bool().ok_or_else(|| {
                    HarnessError::Protocol("Codex MCP server enabled flag is malformed".into())
                })
            })
            .transpose()?
            .unwrap_or(true);
        entries.push(CodexMcpConfigEntry {
            name: name.clone(),
            enabled,
        });
    }
    Ok(entries)
}

async fn read_rewind_history(
    client: &RpcClient,
    thread_id: &str,
) -> Result<Vec<Value>, HarnessError> {
    let metadata = client
        .request(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": false}),
        )
        .await?;
    let thread = metadata.get("thread").unwrap_or(&metadata);
    if thread.get("id").and_then(Value::as_str) != Some(thread_id)
        || thread.get("historyMode").and_then(Value::as_str) != Some("paginated")
        || thread.get("ephemeral").and_then(Value::as_bool) == Some(true)
    {
        return Err(HarnessError::Protocol(
            "Same-chat rewind requires a persisted paginated Codex thread; legacy history cannot be reverted".into(),
        ));
    }
    if thread.pointer("/status/type").and_then(Value::as_str) == Some("active") {
        return Err(HarnessError::Protocol(
            "Codex thread is active in another client".into(),
        ));
    }
    let mut turns = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen_cursors = std::collections::HashSet::new();
    let mut seen_turns = std::collections::HashSet::new();
    let mut bytes = 0usize;
    for _ in 0..100 {
        let page = client
            .request(
                "thread/turns/list",
                json!({
                    "threadId": thread_id, "cursor": cursor, "limit": 100,
                    "sortDirection": "asc", "itemsView": "full"
                }),
            )
            .await?;
        let data = page
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| HarnessError::Protocol("Codex turn page has no data".into()))?;
        if data.len() > 100 {
            return Err(HarnessError::Protocol(
                "Codex turn page exceeded requested size".into(),
            ));
        }
        for turn in data {
            let id = codex_turn_id(turn)
                .ok_or_else(|| HarnessError::Protocol("Codex turn page has no turn id".into()))?;
            if !seen_turns.insert(id.to_owned())
                || turn.get("status").and_then(Value::as_str) != Some("completed")
            {
                return Err(HarnessError::Protocol(
                    "Codex rewind history contains duplicate or incomplete turns".into(),
                ));
            }
            bytes += serde_json::to_vec(turn)
                .map_err(|e| HarnessError::Protocol(e.to_string()))?
                .len();
            if bytes > 32 * 1024 * 1024 {
                return Err(HarnessError::Protocol(
                    "Codex rewind history exceeds 32 MiB verification limit".into(),
                ));
            }
            turns.push(turn.clone());
        }
        cursor = match page.get("nextCursor") {
            Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            _ => {
                return Err(HarnessError::Protocol(
                    "Codex turn page omitted its pagination cursor".into(),
                ));
            }
        };
        let Some(next) = cursor.as_ref() else {
            return Ok(turns);
        };
        if next.is_empty() || !seen_cursors.insert(next.clone()) {
            return Err(HarnessError::Protocol(
                "Codex turn pagination repeated a cursor".into(),
            ));
        }
    }
    Err(HarnessError::Protocol(
        "Codex rewind history exceeds 100 pages".into(),
    ))
}

fn same_rewind_history(left: &[Value], right: &[Value]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            codex_turn_id(left) == codex_turn_id(right)
                && codex_history_signature(left) == codex_history_signature(right)
        })
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
    let mut signature = value.clone();
    let Some(turn) = signature.as_object_mut() else {
        return signature;
    };
    for field in [
        "id",
        "threadId",
        "createdAt",
        "updatedAt",
        "startedAt",
        "completedAt",
    ] {
        turn.remove(field);
    }
    if let Some(items) = turn.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            if let Some(item) = item.as_object_mut() {
                for field in ["id", "threadId", "turnId", "createdAt", "updatedAt"] {
                    item.remove(field);
                }
            }
        }
    }
    signature
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

struct CodexAccountMarker {
    signature: String,
    plan: Option<String>,
    email: Option<String>,
}

fn codex_account_marker(value: &Value) -> Option<CodexAccountMarker> {
    let account = value.get("account")?;
    if account.get("type")?.as_str()? != "chatgpt" {
        return None;
    }
    // This is only a before/after change marker. Account/read exposes an
    // optional email, not a verified identity for quota cache matching.
    let signature = format!("{:x}", Sha256::digest(account.to_string().as_bytes()));
    let plan = account
        .get("planType")
        .and_then(Value::as_str)
        .filter(|plan| !plan.is_empty() && plan.len() <= 80)
        .map(str::to_owned);
    let email = account
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|email| !email.is_empty() && email.len() <= 320)
        .map(str::to_owned);
    Some(CodexAccountMarker {
        signature,
        plan,
        email,
    })
}

fn quota_account_id(value: &Value) -> Option<String> {
    let id = value.get("accountId")?.as_str()?.trim();
    if id.is_empty() || id.len() > 320 {
        return None;
    }
    Some(format!("codex:{:x}", Sha256::digest(id.as_bytes())))
}

fn codex_quota_windows(
    value: &Value,
) -> Option<(Vec<CodexQuotaWindow>, Option<u64>, Option<String>)> {
    let mut buckets: Vec<(&str, &Value)> = if let Some(by_id) = value
        .get("rateLimitsByLimitId")
        .and_then(Value::as_object)
        .filter(|map| !map.is_empty())
    {
        by_id
            .iter()
            .map(|(key, bucket)| (key.as_str(), bucket))
            .collect()
    } else {
        vec![("codex", value.get("rateLimits")?)]
    };
    buckets.sort_by(|(left, _), (right, _)| {
        (left != &"codex", *left).cmp(&(right != &"codex", *right))
    });
    let mut windows = Vec::new();
    let mut plan = None;
    for (fallback_id, bucket) in buckets.into_iter().take(8) {
        if bucket.is_null() {
            continue;
        }
        let id = bucket
            .get("limitId")
            .and_then(Value::as_str)
            .unwrap_or(fallback_id);
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return None;
        }
        if id == "codex" {
            plan = bucket
                .get("planType")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= 80)
                .map(str::to_owned);
        }
        let name = bucket
            .get("limitName")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 100)
            .map(str::to_owned);
        for (position, field) in [("primary", "primary"), ("secondary", "secondary")] {
            let Some(window) = bucket.get(field).filter(|window| !window.is_null()) else {
                continue;
            };
            let used = window.get("usedPercent")?.as_f64()?;
            if !used.is_finite() || !(0.0..=100.0).contains(&used) {
                return None;
            }
            windows.push(CodexQuotaWindow {
                limit_id: id.to_owned(),
                limit_name: name.clone(),
                position,
                used_percent: used as f32,
                duration_minutes: window
                    .get("windowDurationMins")
                    .and_then(Value::as_u64)
                    .filter(|duration| *duration > 0),
                resets_at_seconds: window
                    .get("resetsAt")
                    .and_then(Value::as_i64)
                    .filter(|at| *at > 0),
            });
        }
    }
    if windows.is_empty() {
        return None;
    }
    let reset_credits = value
        .get("rateLimitResetCredits")
        .and_then(|credits| credits.get("availableCount"))
        .and_then(Value::as_u64);
    Some((windows, reset_credits, plan))
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
    fn only_quota_backend_id_is_a_verified_account_identity() {
        let account =
            json!({"account":{"type":"chatgpt","email":"Test@Example.invalid","planType":"pro"}});
        let marker = codex_account_marker(&account).unwrap();
        assert!(!marker.signature.contains("Test@Example.invalid"));
        assert_eq!(marker.plan.as_deref(), Some("pro"));
        assert_eq!(
            marker.signature,
            codex_account_marker(&account).unwrap().signature
        );
        assert!(
            codex_account_marker(
                &json!({"account":{"type":"chatgpt","email":null,"planType":"pro"}})
            )
            .is_some()
        );
        let verified = quota_account_id(&json!({"accountId":"provider-account"})).unwrap();
        assert!(verified.starts_with("codex:"));
        assert!(!verified.contains("provider-account"));
        assert!(quota_account_id(&json!({"accountId":null})).is_none());
        assert!(codex_account_marker(&json!({"account":{"type":"apiKey"}})).is_none());
    }

    #[test]
    fn sparse_codex_windows_keep_reported_duration_and_other_bucket_separate() {
        let read = json!({
            "rateLimits": {"limitId":"codex","primary":{"usedPercent":99,"windowDurationMins":300}},
            "rateLimitsByLimitId": {
                "codex": {"limitId":"codex","primary":null,"secondary":{"usedPercent":42,"windowDurationMins":10080,"resetsAt":1780000000}},
                "codex_other": {"limitId":"codex_other","limitName":"Other model","primary":{"usedPercent":12,"windowDurationMins":60,"resetsAt":1780003600}}
            },
            "rateLimitResetCredits":{"availableCount":2}
        });
        let (windows, credits, _) = codex_quota_windows(&read).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].limit_id, "codex");
        assert_eq!(windows[0].position, "secondary");
        assert_eq!(windows[0].duration_minutes, Some(10080));
        assert_eq!(windows[1].limit_id, "codex_other");
        assert_eq!(windows[1].limit_name.as_deref(), Some("Other model"));
        assert_eq!(credits, Some(2));
        assert!(
            codex_quota_windows(
                &json!({"rateLimits":{"limitId":"codex","primary":{"usedPercent":null}}})
            )
            .is_none()
        );
    }

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
            target_name: None,
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
            json!({"id":"turn-1","status":{"type":"completed"},"items":[{"id":"item-1","type":"agentMessage","text":"one","payload":{"id":"semantic-id","time":17}}]}),
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
            json!({"id":"fork-turn-1","status":{"type":"completed"},"items":[{"id":"fork-item-1","type":"agentMessage","text":"one","payload":{"id":"semantic-id","time":17}}]}),
        ];
        assert_eq!(
            map_codex_fork_history(&prefix, &fork)
                .unwrap()
                .get("turn-1")
                .map(String::as_str),
            Some("fork-turn-1")
        );
        assert!(map_codex_fork_history(&prefix, &source).is_err());
        let mut semantic_mismatch = fork.clone();
        semantic_mismatch[0]["items"][0]["payload"]["id"] = "changed".into();
        assert!(map_codex_fork_history(&prefix, &semantic_mismatch).is_err());
        let mut time_mismatch = fork;
        time_mismatch[0]["items"][0]["payload"]["time"] = 18.into();
        assert!(map_codex_fork_history(&prefix, &time_mismatch).is_err());
    }
}
