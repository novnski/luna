//! L25 cross-task authorization and bounded tool handlers.
//!
//! This module is deliberately independent of provider adapters. The backend
//! must resolve LUNA task IDs and use the existing L07/L09 command and queue
//! transaction; attaching these handlers to a runtime is a separate step.

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use zeron_proto::{
    CommandReceiptStatus, CrossTaskListPage, CrossTaskListRequest, CrossTaskReadPage,
    CrossTaskReadRequest, CrossTaskSendMode, CrossTaskSendReceipt, CrossTaskSendRequest,
    CrossTaskSummary, CrossTaskWaitEvent, CrossTaskWaitRequest, CrossTaskWaitResult,
    OmittedRuntimeTool, RuntimeDelivery, RuntimeToolDefinition,
};

const MAX_PAGE: usize = 20;
const MAX_WAIT_TARGETS: usize = 8;
const MAX_WAIT_MS: u32 = 30_000;
const MAX_MESSAGE_CHARS: usize = 16_000;
const MAX_SUMMARY_CHARS: usize = 240;
const MAX_TITLE_CHARS: usize = 160;
const MAX_CURSOR_CHARS: usize = 256;
const MAX_OUTPUT_BYTES: usize = 16_384;
const BACKEND_DEADLINE_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy, Default)]
pub struct CrossTaskCapabilities {
    pub list: bool,
    pub read: bool,
    pub durable_queue: bool,
    pub event_wait: bool,
}

/// Populated only from the active, engine-resolved run and its actual grants.
/// Never deserialize this value from model arguments or task history.
#[derive(Debug, Clone)]
pub struct CrossTaskAuthority {
    pub caller_task_id: String,
    pub local_environment_id: String,
    pub visible_project_ids: HashSet<String>,
    pub allow_projectless: bool,
    pub granted_scopes: HashSet<String>,
    pub explicit_user_request: bool,
    pub delivery: RuntimeDelivery,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CrossTaskError {
    #[error("cross-task tool is not available on this provider/runtime")]
    Unavailable,
    #[error("required LUNA scope is missing")]
    Scope,
    #[error("target task is outside the active project or environment scope")]
    TargetScope,
    #[error("an explicit user request is required for cross-task mutation")]
    UserRequest,
    #[error("invalid or unbounded cross-task input")]
    Input,
    #[error("cross-task operation is unsupported by the current delivery path")]
    Unsupported,
    #[error("the backend returned an invalid or unverified result")]
    InvalidBackendResult,
    #[error("cross-task call was cancelled")]
    Cancelled,
    #[error("cross-task backend deadline exceeded")]
    Deadline,
    #[error("cross-task backend failed: {0}")]
    Backend(String),
}

/// Implementations must use authoritative task identity, not provider IDs.
/// `enqueue_queue` must commit the queue row and L07 receipt atomically (or
/// return an existing matching receipt) before returning. It must return a
/// conflict for the same idempotency key with a different payload.
#[async_trait]
pub trait CrossTaskBackend: Send + Sync {
    fn capabilities(&self) -> CrossTaskCapabilities;
    async fn task(&self, task_id: &str) -> Result<Option<CrossTaskSummary>, CrossTaskError>;
    /// Apply engine-supplied visibility before pagination. The handler rejects
    /// any page containing an invisible row, including its cursor.
    async fn list(
        &self,
        request: CrossTaskListRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<CrossTaskListPage, CrossTaskError>;
    async fn read(
        &self,
        request: CrossTaskReadRequest,
    ) -> Result<CrossTaskReadPage, CrossTaskError>;
    async fn enqueue_queue(
        &self,
        request: CrossTaskSendRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<CrossTaskSendReceipt, CrossTaskError>;
    async fn wait_event(
        &self,
        request: CrossTaskWaitRequest,
    ) -> Result<Option<CrossTaskWaitEvent>, CrossTaskError>;
}

pub struct CrossTaskTools<B> {
    backend: B,
}

impl<B: CrossTaskBackend> CrossTaskTools<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn catalog(
        &self,
        authority: &CrossTaskAuthority,
    ) -> (Vec<RuntimeToolDefinition>, Vec<OmittedRuntimeTool>) {
        let capabilities = self.backend.capabilities();
        let candidates = [
            (
                "list_threads",
                "threads.read",
                capabilities.list,
                "read-only",
                2_000,
            ),
            (
                "read_thread",
                "threads.read",
                capabilities.read,
                "read-only",
                2_000,
            ),
            (
                "send_message_to_thread",
                "threads.send",
                capabilities.durable_queue,
                "caller-keyed durable queue receipt",
                5_000,
            ),
            (
                "wait_threads",
                "threads.read",
                capabilities.event_wait,
                "read-only",
                30_000,
            ),
        ];
        let mut attached = Vec::new();
        let mut omitted = Vec::new();
        for (name, scope, supported, idempotency, deadline_ms) in candidates {
            let reason = if authority.delivery != RuntimeDelivery::CodexDynamicTools {
                Some("the provider has no verified dynamic tool delivery")
            } else if !authority.granted_scopes.contains(scope) {
                Some("required LUNA scope is missing")
            } else if name == "send_message_to_thread" && !authority.explicit_user_request {
                Some("sending requires an explicit user request in this run")
            } else if !supported {
                Some("the engine backend has no verified local implementation")
            } else {
                None
            };
            if let Some(reason) = reason {
                omitted.push(OmittedRuntimeTool {
                    name: name.into(),
                    reason: reason.into(),
                });
            } else {
                attached.push(RuntimeToolDefinition {
                    name: name.into(),
                    version: 1,
                    description: tool_description(name).into(),
                    input_schema: input_schema(name),
                    output_schema: output_schema(name),
                    required_scopes: vec![scope.into()],
                    deadline_ms,
                    idempotency: idempotency.into(),
                    presentation: "cross-task".into(),
                    activity_classification: "luna-cross-task".into(),
                });
            }
        }
        for name in [
            "create_thread",
            "interrupt_thread",
            "archive_thread",
            "open_thread",
            "fork_thread",
            "move_thread",
        ] {
            omitted.push(OmittedRuntimeTool {
                name: name.into(),
                reason: "no verified cross-task command adapter is attached".into(),
            });
        }
        (attached, omitted)
    }

    pub async fn list(
        &self,
        authority: &CrossTaskAuthority,
        mut request: CrossTaskListRequest,
        cancel: &CancellationToken,
    ) -> Result<CrossTaskListPage, CrossTaskError> {
        self.check(authority, "threads.read", self.backend.capabilities().list)?;
        check_cursor(&request.cursor)?;
        if request
            .project_id
            .as_ref()
            .is_some_and(|id| !authority.visible_project_ids.contains(id))
        {
            return Err(CrossTaskError::TargetScope);
        }
        if request
            .text
            .as_ref()
            .is_some_and(|text| text.chars().count() > 160)
        {
            return Err(CrossTaskError::Input);
        }
        request.limit = Some(bound_page(request.limit)?);
        let limit = usize::from(request.limit.unwrap());
        let mut page = with_cancel(cancel, self.backend.list(request, authority)).await?;
        if page.tasks.len() > limit || !valid_page_cursor(page.has_more, &page.next_cursor) {
            return Err(CrossTaskError::InvalidBackendResult);
        }
        if page.tasks.iter().any(|task| !visible(authority, task)) {
            return Err(CrossTaskError::InvalidBackendResult);
        }
        for task in &mut page.tasks {
            task.title = bounded(&task.title, MAX_TITLE_CHARS);
        }
        check_output(&page)?;
        Ok(page)
    }

    pub async fn read(
        &self,
        authority: &CrossTaskAuthority,
        mut request: CrossTaskReadRequest,
        cancel: &CancellationToken,
    ) -> Result<CrossTaskReadPage, CrossTaskError> {
        self.check(authority, "threads.read", self.backend.capabilities().read)?;
        check_cursor(&request.cursor)?;
        self.target(authority, &request.task_id, cancel).await?;
        request.limit = Some(bound_page(request.limit)?);
        let limit = usize::from(request.limit.unwrap());
        let mut page = with_cancel(cancel, self.backend.read(request.clone())).await?;
        if page.task.task_id != request.task_id
            || !visible(authority, &page.task)
            || page.turns.len() > limit
            || !valid_page_cursor(page.has_more, &page.next_cursor)
        {
            return Err(CrossTaskError::InvalidBackendResult);
        }
        page.task.title = bounded(&page.task.title, MAX_TITLE_CHARS);
        for turn in &mut page.turns {
            turn.summary = bounded(&turn.summary, MAX_SUMMARY_CHARS);
        }
        page.content_untrusted = true;
        check_output(&page)?;
        Ok(page)
    }

    pub async fn send(
        &self,
        authority: &CrossTaskAuthority,
        request: CrossTaskSendRequest,
        cancel: &CancellationToken,
    ) -> Result<CrossTaskSendReceipt, CrossTaskError> {
        self.check(
            authority,
            "threads.send",
            self.backend.capabilities().durable_queue,
        )?;
        if !authority.explicit_user_request {
            return Err(CrossTaskError::UserRequest);
        }
        if request.mode != CrossTaskSendMode::Queue {
            return Err(CrossTaskError::Unsupported);
        }
        if request.message.trim().is_empty()
            || request.message.chars().count() > MAX_MESSAGE_CHARS
            || !valid_key(&request.idempotency_key)
        {
            return Err(CrossTaskError::Input);
        }
        self.target(authority, &request.task_id, cancel).await?;
        let receipt = with_cancel(
            cancel,
            self.backend.enqueue_queue(request.clone(), authority),
        )
        .await?;
        if receipt.task_id != request.task_id
            || receipt.command.task_id != request.task_id
            || receipt.command.command_id != request.idempotency_key
            || receipt.command.status != CommandReceiptStatus::Applied
            || receipt.command.intent != "queueMessage"
            || receipt.command.snapshot.task_id != request.task_id
            || receipt.command.committed_at_ms.is_none()
            || receipt.command.event_sequence.is_none()
            || receipt.queued_message_id.is_empty()
            || receipt.activity_cursor.is_empty()
            || receipt.delivery_state != "queued"
        {
            return Err(CrossTaskError::InvalidBackendResult);
        }
        check_output(&receipt)?;
        Ok(receipt)
    }

    pub async fn wait(
        &self,
        authority: &CrossTaskAuthority,
        mut request: CrossTaskWaitRequest,
        cancel: &CancellationToken,
    ) -> Result<CrossTaskWaitResult, CrossTaskError> {
        self.check(
            authority,
            "threads.read",
            self.backend.capabilities().event_wait,
        )?;
        if request.targets.is_empty() || request.targets.len() > MAX_WAIT_TARGETS {
            return Err(CrossTaskError::Input);
        }
        let mut ids = HashSet::new();
        for target in &request.targets {
            if !ids.insert(target.task_id.clone()) {
                return Err(CrossTaskError::Input);
            }
            check_cursor(&target.after_cursor)?;
            self.target(authority, &target.task_id, cancel).await?;
        }
        let timeout_ms = request.timeout_ms.unwrap_or(MAX_WAIT_MS);
        if timeout_ms == 0 || timeout_ms > MAX_WAIT_MS {
            return Err(CrossTaskError::Input);
        }
        request.timeout_ms = Some(timeout_ms);
        let requested_cursors = request
            .targets
            .iter()
            .map(|target| (target.task_id.clone(), target.after_cursor.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        let result = tokio::select! {
            _ = cancel.cancelled() => return Err(CrossTaskError::Cancelled),
            result = tokio::time::timeout(Duration::from_millis(u64::from(timeout_ms)), self.backend.wait_event(request)) => result,
        };
        let mut event = match result {
            Ok(Ok(event)) => event,
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                return Ok(CrossTaskWaitResult {
                    event: None,
                    timed_out: true,
                });
            }
        };
        if let Some(ref event) = event
            && (!ids.contains(&event.task.task_id)
                || !visible(authority, &event.task)
                || (!event.completed && !event.needs_attention)
                || event.cursor.is_empty()
                || event.cursor.chars().count() > MAX_CURSOR_CHARS
                || requested_cursors
                    .get(&event.task.task_id)
                    .is_some_and(|after| after.as_deref() == Some(event.cursor.as_str())))
        {
            return Err(CrossTaskError::InvalidBackendResult);
        }
        if let Some(ref mut event) = event {
            event.task.title = bounded(&event.task.title, MAX_TITLE_CHARS);
        }
        let result = CrossTaskWaitResult {
            timed_out: event.is_none(),
            event,
        };
        check_output(&result)?;
        Ok(result)
    }

    fn check(
        &self,
        authority: &CrossTaskAuthority,
        scope: &str,
        supported: bool,
    ) -> Result<(), CrossTaskError> {
        if authority.delivery != RuntimeDelivery::CodexDynamicTools || !supported {
            return Err(CrossTaskError::Unavailable);
        }
        if !authority.granted_scopes.contains(scope) {
            return Err(CrossTaskError::Scope);
        }
        Ok(())
    }

    async fn target(
        &self,
        authority: &CrossTaskAuthority,
        task_id: &str,
        cancel: &CancellationToken,
    ) -> Result<CrossTaskSummary, CrossTaskError> {
        if task_id.is_empty() || task_id.len() > 128 {
            return Err(CrossTaskError::Input);
        }
        let task = with_cancel(cancel, self.backend.task(task_id))
            .await?
            .ok_or(CrossTaskError::TargetScope)?;
        if task.task_id != task_id {
            return Err(CrossTaskError::InvalidBackendResult);
        }
        if !visible(authority, &task) {
            return Err(CrossTaskError::TargetScope);
        }
        Ok(task)
    }
}

fn visible(authority: &CrossTaskAuthority, task: &CrossTaskSummary) -> bool {
    task.environment_id == authority.local_environment_id
        && match &task.project_id {
            Some(id) => authority.visible_project_ids.contains(id),
            None => authority.allow_projectless,
        }
}

fn bound_page(limit: Option<u16>) -> Result<u16, CrossTaskError> {
    let limit = limit.unwrap_or(10);
    if limit == 0 || usize::from(limit) > MAX_PAGE {
        Err(CrossTaskError::Input)
    } else {
        Ok(limit)
    }
}

fn check_cursor(cursor: &Option<String>) -> Result<(), CrossTaskError> {
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.is_empty() || cursor.chars().count() > MAX_CURSOR_CHARS)
    {
        Err(CrossTaskError::Input)
    } else {
        Ok(())
    }
}

fn valid_page_cursor(has_more: bool, cursor: &Option<String>) -> bool {
    (!has_more || cursor.as_ref().is_some_and(|cursor| !cursor.is_empty()))
        && cursor
            .as_ref()
            .is_none_or(|cursor| cursor.chars().count() <= MAX_CURSOR_CHARS)
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 128 && key.bytes().all(|byte| byte.is_ascii_graphic())
}

fn bounded(value: &str, chars: usize) -> String {
    value.chars().take(chars).collect()
}

fn check_output(value: &impl serde::Serialize) -> Result<(), CrossTaskError> {
    let bytes = serde_json::to_vec(value).map_err(|_| CrossTaskError::InvalidBackendResult)?;
    if bytes.len() > MAX_OUTPUT_BYTES {
        Err(CrossTaskError::InvalidBackendResult)
    } else {
        Ok(())
    }
}

async fn with_cancel<T>(
    cancel: &CancellationToken,
    future: impl std::future::Future<Output = Result<T, CrossTaskError>>,
) -> Result<T, CrossTaskError> {
    tokio::select! {
        _ = cancel.cancelled() => Err(CrossTaskError::Cancelled),
        result = tokio::time::timeout(Duration::from_millis(BACKEND_DEADLINE_MS), future) => result.map_err(|_| CrossTaskError::Deadline)?,
    }
}

fn tool_description(name: &str) -> &'static str {
    match name {
        "list_threads" => "List visible local LUNA tasks with bounded metadata and opaque paging.",
        "read_thread" => "Read bounded, untrusted turn summaries for one local LUNA task.",
        "send_message_to_thread" => {
            "Queue an explicitly requested follow-up and return a durable receipt; queued is not delivered."
        }
        "wait_threads" => "Wait for completion or attention on up to eight local LUNA tasks.",
        _ => "",
    }
}

fn input_schema(name: &str) -> serde_json::Value {
    match name {
        "list_threads" => serde_json::json!({"type":"object","properties":{
            "projectId":{"type":"string"},"text":{"type":"string","maxLength":160},
            "cursor":{"type":"string","maxLength":256},"limit":{"type":"integer","minimum":1,"maximum":20}
        },"additionalProperties":false}),
        "read_thread" => serde_json::json!({"type":"object","properties":{
            "taskId":{"type":"string"},"cursor":{"type":"string","maxLength":256},
            "limit":{"type":"integer","minimum":1,"maximum":20}
        },"required":["taskId"],"additionalProperties":false}),
        "send_message_to_thread" => serde_json::json!({"type":"object","properties":{
            "taskId":{"type":"string"},"message":{"type":"string","maxLength":16000},
            "mode":{"const":"queue"},"idempotencyKey":{"type":"string","maxLength":128}
        },"required":["taskId","message","idempotencyKey"],"additionalProperties":false}),
        "wait_threads" => serde_json::json!({"type":"object","properties":{
            "targets":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"object","properties":{
                "taskId":{"type":"string"},"afterCursor":{"type":"string","maxLength":256}
            },"required":["taskId"],"additionalProperties":false}},
            "timeoutMs":{"type":"integer","minimum":1,"maximum":30000}
        },"required":["targets"],"additionalProperties":false}),
        _ => serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
    }
}

fn output_schema(name: &str) -> serde_json::Value {
    let task = serde_json::json!({"type":"object","properties":{
        "taskId":{"type":"string"},"projectId":{"type":["string","null"]},
        "environmentId":{"type":"string"},"title":{"type":"string","maxLength":160},
        "status":{"type":"string"},"archived":{"type":"boolean"},
        "updatedAtMs":{"type":"integer"}
    },"required":["taskId","projectId","environmentId","title","status","archived","updatedAtMs"],
    "additionalProperties":false});
    match name {
        "list_threads" => serde_json::json!({"type":"object","properties":{
            "tasks":{"type":"array","maxItems":20,"items":task},
            "nextCursor":{"type":["string","null"]},"hasMore":{"type":"boolean"}
        },"required":["tasks","nextCursor","hasMore"],"additionalProperties":false}),
        "read_thread" => serde_json::json!({"type":"object","properties":{
            "task":task,"turns":{"type":"array","maxItems":20,"items":{"type":"object","properties":{
                "turnId":{"type":"string"},"status":{"type":"string"},
                "summary":{"type":"string","maxLength":240}
            },"required":["turnId","status","summary"],"additionalProperties":false}},
            "nextCursor":{"type":["string","null"]},"hasMore":{"type":"boolean"},
            "contentUntrusted":{"const":true}
        },"required":["task","turns","nextCursor","hasMore","contentUntrusted"],"additionalProperties":false}),
        "send_message_to_thread" => serde_json::json!({"type":"object","properties":{
            "taskId":{"type":"string"},"queuedMessageId":{"type":"string"},
            "command":{"type":"object"},"activityCursor":{"type":"string"},
            "deliveryState":{"const":"queued"}
        },"required":["taskId","queuedMessageId","command","activityCursor","deliveryState"],
        "additionalProperties":false}),
        "wait_threads" => serde_json::json!({"type":"object","properties":{
            "event":{"type":["object","null"]},"timedOut":{"type":"boolean"}
        },"required":["event","timedOut"],"additionalProperties":false}),
        _ => serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use zeron_proto::{CommandReceipt, TaskCommandSnapshot};

    #[derive(Clone, Default)]
    struct FakeBackend {
        queued: Arc<Mutex<HashMap<String, (String, CrossTaskSendReceipt)>>>,
    }

    fn task(id: &str, environment: &str) -> CrossTaskSummary {
        CrossTaskSummary {
            task_id: id.into(),
            project_id: Some("project-a".into()),
            environment_id: environment.into(),
            title: "Synthetic task".into(),
            status: "idle".into(),
            archived: false,
            updated_at_ms: 1,
        }
    }

    fn authority() -> CrossTaskAuthority {
        CrossTaskAuthority {
            caller_task_id: "caller".into(),
            local_environment_id: "local".into(),
            visible_project_ids: HashSet::from(["project-a".into()]),
            allow_projectless: false,
            granted_scopes: HashSet::from(["threads.read".into(), "threads.send".into()]),
            explicit_user_request: true,
            delivery: RuntimeDelivery::CodexDynamicTools,
        }
    }

    #[async_trait]
    impl CrossTaskBackend for FakeBackend {
        fn capabilities(&self) -> CrossTaskCapabilities {
            CrossTaskCapabilities {
                list: true,
                read: true,
                durable_queue: true,
                event_wait: true,
            }
        }

        async fn task(&self, task_id: &str) -> Result<Option<CrossTaskSummary>, CrossTaskError> {
            Ok(match task_id {
                "task-a" => Some(task(task_id, "local")),
                "remote-task" => Some(task(task_id, "remote")),
                _ => None,
            })
        }

        async fn list(
            &self,
            _request: CrossTaskListRequest,
            _authority: &CrossTaskAuthority,
        ) -> Result<CrossTaskListPage, CrossTaskError> {
            Ok(CrossTaskListPage {
                tasks: vec![task("task-a", "local")],
                next_cursor: None,
                has_more: false,
            })
        }

        async fn read(
            &self,
            request: CrossTaskReadRequest,
        ) -> Result<CrossTaskReadPage, CrossTaskError> {
            Ok(CrossTaskReadPage {
                task: task(&request.task_id, "local"),
                turns: vec![zeron_proto::CrossTaskTurnSummary {
                    turn_id: "turn-a".into(),
                    status: "completed".into(),
                    summary: "x".repeat(500),
                }],
                next_cursor: None,
                has_more: false,
                content_untrusted: false,
            })
        }

        async fn enqueue_queue(
            &self,
            request: CrossTaskSendRequest,
            _authority: &CrossTaskAuthority,
        ) -> Result<CrossTaskSendReceipt, CrossTaskError> {
            let mut queued = self.queued.lock().await;
            if let Some((text, receipt)) = queued.get(&request.idempotency_key) {
                return if text == &request.message {
                    Ok(receipt.clone())
                } else {
                    Err(CrossTaskError::Backend("command ID conflict".into()))
                };
            }
            let receipt = CrossTaskSendReceipt {
                task_id: request.task_id.clone(),
                queued_message_id: format!("queue:{}", request.idempotency_key),
                command: CommandReceipt {
                    command_id: request.idempotency_key.clone(),
                    task_id: request.task_id.clone(),
                    intent: "queueMessage".into(),
                    payload_hash: "synthetic-hash".into(),
                    status: CommandReceiptStatus::Applied,
                    snapshot: TaskCommandSnapshot {
                        task_id: request.task_id.clone(),
                        ..Default::default()
                    },
                    event_sequence: Some(1),
                    actor_id: "synthetic-actor".into(),
                    client_id: "synthetic-client".into(),
                    client_session_id: "synthetic-session".into(),
                    execution_environment: Some("local".into()),
                    correlation_id: None,
                    reason: None,
                    recovery_action: None,
                    accepted_at_ms: 1,
                    committed_at_ms: Some(1),
                    observed_at_ms: 1,
                },
                activity_cursor: "receipt:1".into(),
                delivery_state: "queued".into(),
            };
            queued.insert(request.idempotency_key, (request.message, receipt.clone()));
            Ok(receipt)
        }

        async fn wait_event(
            &self,
            _request: CrossTaskWaitRequest,
        ) -> Result<Option<CrossTaskWaitEvent>, CrossTaskError> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn list_and_read_are_local_bounded_and_untrusted() {
        let tools = CrossTaskTools::new(FakeBackend::default());
        let cancel = CancellationToken::new();
        let page = tools
            .list(
                &authority(),
                CrossTaskListRequest {
                    project_id: None,
                    text: None,
                    cursor: None,
                    limit: None,
                },
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(page.tasks.len(), 1);
        assert_eq!(page.tasks[0].task_id, "task-a");
        let read = tools
            .read(
                &authority(),
                CrossTaskReadRequest {
                    task_id: "task-a".into(),
                    cursor: None,
                    limit: None,
                },
                &cancel,
            )
            .await
            .unwrap();
        assert!(read.content_untrusted);
        assert_eq!(read.turns[0].summary.chars().count(), MAX_SUMMARY_CHARS);
        assert_eq!(
            tools
                .read(
                    &authority(),
                    CrossTaskReadRequest {
                        task_id: "remote-task".into(),
                        cursor: None,
                        limit: None,
                    },
                    &cancel
                )
                .await
                .unwrap_err(),
            CrossTaskError::TargetScope
        );
    }

    #[tokio::test]
    async fn send_requires_user_request_and_returns_only_durable_queue_state() {
        let tools = CrossTaskTools::new(FakeBackend::default());
        let cancel = CancellationToken::new();
        let request = CrossTaskSendRequest {
            task_id: "task-a".into(),
            message: "follow up".into(),
            mode: CrossTaskSendMode::Queue,
            idempotency_key: "command-a".into(),
        };
        let mut unauthorized = authority();
        unauthorized.explicit_user_request = false;
        assert_eq!(
            tools
                .send(&unauthorized, request.clone(), &cancel)
                .await
                .unwrap_err(),
            CrossTaskError::UserRequest
        );
        let first = tools
            .send(&authority(), request.clone(), &cancel)
            .await
            .unwrap();
        let retry = tools
            .send(&authority(), request.clone(), &cancel)
            .await
            .unwrap();
        assert_eq!(first, retry);
        assert_eq!(first.delivery_state, "queued");
        assert_eq!(tools.backend.queued.lock().await.len(), 1);
        let mut changed = request.clone();
        changed.message = "different".into();
        assert!(matches!(
            tools.send(&authority(), changed, &cancel).await,
            Err(CrossTaskError::Backend(_))
        ));
        let mut steer = request;
        steer.mode = CrossTaskSendMode::Steer;
        assert_eq!(
            tools.send(&authority(), steer, &cancel).await.unwrap_err(),
            CrossTaskError::Unsupported
        );
    }

    #[tokio::test]
    async fn wait_is_bounded_and_cancellation_aware() {
        let tools = CrossTaskTools::new(FakeBackend::default());
        let request = CrossTaskWaitRequest {
            targets: vec![zeron_proto::CrossTaskWaitTarget {
                task_id: "task-a".into(),
                after_cursor: None,
            }],
            timeout_ms: Some(5),
        };
        let result = tools
            .wait(&authority(), request.clone(), &CancellationToken::new())
            .await
            .unwrap();
        assert!(result.timed_out);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            tools
                .wait(&authority(), request, &cancelled)
                .await
                .unwrap_err(),
            CrossTaskError::Cancelled
        );
    }

    #[test]
    fn catalog_omits_unverified_delivery_and_controls() {
        let tools = CrossTaskTools::new(FakeBackend::default());
        let mut authority = authority();
        authority.delivery = RuntimeDelivery::ClaudeInstructionsOnly;
        let (attached, omitted) = tools.catalog(&authority);
        assert!(attached.is_empty());
        assert!(omitted.iter().any(|tool| tool.name == "fork_thread"));
        authority.delivery = RuntimeDelivery::CodexDynamicTools;
        let (attached, _) = tools.catalog(&authority);
        assert_eq!(attached.len(), 4);
        authority.explicit_user_request = false;
        let (attached, omitted) = tools.catalog(&authority);
        assert!(
            !attached
                .iter()
                .any(|tool| tool.name == "send_message_to_thread")
        );
        assert!(
            omitted
                .iter()
                .any(|tool| tool.name == "send_message_to_thread")
        );
    }
}
