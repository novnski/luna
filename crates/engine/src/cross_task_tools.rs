//! L25 cross-task authorization and bounded tool handlers.
//!
//! This module is deliberately independent of provider adapters. The backend
//! must resolve LUNA task IDs and use the existing L07/L09 command and queue
//! transaction; attaching these handlers to a runtime is a separate step.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use luna_proto::{
    CommandReasonCode, CommandReceipt, CommandReceiptStatus, CrossTaskControlAction,
    CrossTaskControlReceipt, CrossTaskControlRequest, CrossTaskListPage, CrossTaskListRequest,
    CrossTaskReadPage, CrossTaskReadRequest, CrossTaskSendMode, CrossTaskSendReceipt,
    CrossTaskSendRequest, CrossTaskSummary, CrossTaskWaitEvent, CrossTaskWaitRequest,
    CrossTaskWaitResult, OmittedRuntimeTool, RuntimeContext, RuntimeDelivery,
    RuntimeToolDefinition, TaskCommandSnapshot,
};
use tokio_util::sync::CancellationToken;

const MAX_PAGE: usize = 20;
const MAX_WAIT_TARGETS: usize = 8;
const MAX_WAIT_MS: u32 = 30_000;
const MAX_MESSAGE_CHARS: usize = 16_000;
pub(crate) const MAX_APPROVED_MESSAGE_CHARS: usize = 2_000;
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
    pub approved_queue: bool,
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

impl CrossTaskAuthority {
    /// The run context is assembled by the engine from its task binding. This
    /// grant is deliberately limited to the caller's local project; a model
    /// cannot widen it through tool arguments or another task's content.
    pub fn from_context(
        context: &RuntimeContext,
        delivery: RuntimeDelivery,
        explicit_user_request: bool,
    ) -> Self {
        Self {
            caller_task_id: context.thread_id.clone(),
            local_environment_id: context.environment_id.clone(),
            visible_project_ids: context.project_id.iter().cloned().collect(),
            allow_projectless: false,
            granted_scopes: context.granted_scopes.iter().cloned().collect(),
            explicit_user_request,
            delivery,
        }
    }
}

/// Exact, short-lived authorization minted from a trusted user action by a
/// future caller. No current provider-run path mints one, so controls stay out
/// of model catalogs. This value is never deserialized from tool arguments.
#[derive(Debug, Clone)]
pub struct CrossTaskControlGrant {
    pub caller_task_id: String,
    pub request: CrossTaskControlRequest,
    pub expires_at_ms: i64,
}

/// Engine-minted evidence for one approved provider call. This never crosses
/// the tool wire; the backend checks it again at the durable queue commit.
#[derive(Debug, Clone)]
pub struct ApprovedSendFence {
    pub(crate) source_task_id: String,
    pub(crate) source_run_id: String,
    pub(crate) source_binding_id: String,
    pub(crate) source_binding_generation: u32,
    pub(crate) source_provider_thread_id: String,
    pub(crate) source_provider_turn_id: String,
    pub(crate) source_provider_call_id: String,
    pub(crate) target_snapshot: TaskCommandSnapshot,
    pub(crate) request: CrossTaskSendRequest,
    pub(crate) expires_at_ms: i64,
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
    #[error("the target task changed since the captured snapshot")]
    StaleSnapshot,
    #[error("cross-task call was cancelled")]
    Cancelled,
    #[error("cross-task backend deadline exceeded")]
    Deadline,
    #[error("cross-task backend failed: {0}")]
    Backend(String),
    #[error("cross-task command ended in {status:?} ({reason:?})")]
    CommandOutcome {
        status: CommandReceiptStatus,
        reason: Option<CommandReasonCode>,
    },
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
    async fn enqueue_approved_queue(
        &self,
        _request: CrossTaskSendRequest,
        _authority: &CrossTaskAuthority,
        _fence: &ApprovedSendFence,
    ) -> Result<CrossTaskSendReceipt, CrossTaskError> {
        Err(CrossTaskError::Unsupported)
    }
    async fn wait_event(
        &self,
        request: CrossTaskWaitRequest,
    ) -> Result<Option<CrossTaskWaitEvent>, CrossTaskError>;
    /// Read-only evidence for a prospective control. No grant or command is
    /// created; implementations must enforce the local authority scope.
    async fn control_target_snapshot(
        &self,
        _task_id: &str,
        _authority: &CrossTaskAuthority,
    ) -> Result<TaskCommandSnapshot, CrossTaskError> {
        Err(CrossTaskError::Unavailable)
    }
    /// None means no durable receipt was found, not that execution is known
    /// not to have happened. No command may be replayed from this result.
    async fn control_command_receipt(
        &self,
        _task_id: &str,
        _command_id: &str,
        _authority: &CrossTaskAuthority,
    ) -> Result<Option<CommandReceipt>, CrossTaskError> {
        Err(CrossTaskError::Unavailable)
    }
    async fn control(
        &self,
        _request: CrossTaskControlRequest,
        _authority: &CrossTaskAuthority,
        _grant: &CrossTaskControlGrant,
    ) -> Result<CrossTaskControlReceipt, CrossTaskError> {
        Err(CrossTaskError::Unsupported)
    }
}

#[async_trait]
impl<T: CrossTaskBackend + ?Sized> CrossTaskBackend for Arc<T> {
    fn capabilities(&self) -> CrossTaskCapabilities {
        (**self).capabilities()
    }
    async fn task(&self, task_id: &str) -> Result<Option<CrossTaskSummary>, CrossTaskError> {
        (**self).task(task_id).await
    }
    async fn list(
        &self,
        request: CrossTaskListRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<CrossTaskListPage, CrossTaskError> {
        (**self).list(request, authority).await
    }
    async fn read(
        &self,
        request: CrossTaskReadRequest,
    ) -> Result<CrossTaskReadPage, CrossTaskError> {
        (**self).read(request).await
    }
    async fn enqueue_queue(
        &self,
        request: CrossTaskSendRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<CrossTaskSendReceipt, CrossTaskError> {
        (**self).enqueue_queue(request, authority).await
    }
    async fn enqueue_approved_queue(
        &self,
        request: CrossTaskSendRequest,
        authority: &CrossTaskAuthority,
        fence: &ApprovedSendFence,
    ) -> Result<CrossTaskSendReceipt, CrossTaskError> {
        (**self)
            .enqueue_approved_queue(request, authority, fence)
            .await
    }
    async fn wait_event(
        &self,
        request: CrossTaskWaitRequest,
    ) -> Result<Option<CrossTaskWaitEvent>, CrossTaskError> {
        (**self).wait_event(request).await
    }
    async fn control_target_snapshot(
        &self,
        task_id: &str,
        authority: &CrossTaskAuthority,
    ) -> Result<TaskCommandSnapshot, CrossTaskError> {
        (**self).control_target_snapshot(task_id, authority).await
    }
    async fn control_command_receipt(
        &self,
        task_id: &str,
        command_id: &str,
        authority: &CrossTaskAuthority,
    ) -> Result<Option<CommandReceipt>, CrossTaskError> {
        (**self)
            .control_command_receipt(task_id, command_id, authority)
            .await
    }
    async fn control(
        &self,
        request: CrossTaskControlRequest,
        authority: &CrossTaskAuthority,
        grant: &CrossTaskControlGrant,
    ) -> Result<CrossTaskControlReceipt, CrossTaskError> {
        (**self).control(request, authority, grant).await
    }
}

pub struct CrossTaskTools<B> {
    backend: B,
}

impl<B: CrossTaskBackend> CrossTaskTools<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Internal evidence only. A caller must separately obtain a trusted
    /// per-request local UI grant before any future control dispatch.
    #[allow(dead_code)]
    pub(crate) async fn control_target_snapshot(
        &self,
        authority: &CrossTaskAuthority,
        task_id: &str,
        cancel: &CancellationToken,
    ) -> Result<TaskCommandSnapshot, CrossTaskError> {
        self.check(authority, "threads.read", self.backend.capabilities().read)?;
        let target = self
            .control_evidence_target(authority, task_id, cancel)
            .await?;
        let snapshot = with_cancel(
            cancel,
            self.backend.control_target_snapshot(task_id, authority),
        )
        .await?;
        if snapshot.task_id != task_id
            || snapshot.binding_id.as_deref() == Some("")
            || snapshot.active_turn_id.as_deref() == Some("")
        {
            return Err(CrossTaskError::InvalidBackendResult);
        }
        let after = self
            .control_evidence_target(authority, task_id, cancel)
            .await?;
        if after.project_id != target.project_id || after.environment_id != target.environment_id {
            return Err(CrossTaskError::StaleSnapshot);
        }
        check_output(&snapshot)?;
        Ok(snapshot)
    }

    /// Re-read all snapshot fields, including task sequence, binding ID and
    /// the engine turn. A changed field is stale evidence, never a new grant.
    #[allow(dead_code)]
    pub(crate) async fn revalidate_control_target_snapshot(
        &self,
        authority: &CrossTaskAuthority,
        task_id: &str,
        expected: &TaskCommandSnapshot,
        cancel: &CancellationToken,
    ) -> Result<(), CrossTaskError> {
        if &self
            .control_target_snapshot(authority, task_id, cancel)
            .await?
            != expected
        {
            return Err(CrossTaskError::StaleSnapshot);
        }
        Ok(())
    }

    /// None is an unknown/missing readback. It must never be interpreted as
    /// proof that a previously submitted command did not execute.
    #[allow(dead_code)]
    pub(crate) async fn control_command_receipt(
        &self,
        authority: &CrossTaskAuthority,
        task_id: &str,
        command_id: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<CommandReceipt>, CrossTaskError> {
        self.check(authority, "threads.read", self.backend.capabilities().read)?;
        if !valid_key(command_id) {
            return Err(CrossTaskError::Input);
        }
        let target = self
            .control_evidence_target(authority, task_id, cancel)
            .await?;
        let receipt = with_cancel(
            cancel,
            self.backend
                .control_command_receipt(task_id, command_id, authority),
        )
        .await?;
        let after = self
            .control_evidence_target(authority, task_id, cancel)
            .await?;
        if after.project_id != target.project_id || after.environment_id != target.environment_id {
            return Err(CrossTaskError::StaleSnapshot);
        }
        if let Some(receipt) = &receipt {
            if receipt.command_id != command_id
                || receipt.task_id != task_id
                || receipt.snapshot.task_id != task_id
                || receipt.execution_environment.as_deref()
                    != Some(authority.local_environment_id.as_str())
                || receipt.client_id != "luna-cross-task"
                || receipt.actor_id != format!("luna-tool:{}", authority.caller_task_id)
                || receipt.client_session_id != authority.caller_task_id
                || !matches!(
                    receipt.intent.as_str(),
                    "createChat" | "stop" | "setArchived" | "moveProject"
                )
            {
                return Err(CrossTaskError::InvalidBackendResult);
            }
            check_output(receipt)?;
        }
        Ok(receipt)
    }

    /// Internal control path. A model request alone can never supply `grant`.
    pub async fn control(
        &self,
        authority: &CrossTaskAuthority,
        grant: &CrossTaskControlGrant,
        request: CrossTaskControlRequest,
        cancel: &CancellationToken,
    ) -> Result<CrossTaskControlReceipt, CrossTaskError> {
        self.check(authority, "threads.manage", true)?;
        if !authority.explicit_user_request
            || grant.caller_task_id != authority.caller_task_id
            || grant.request != request
            || grant.expires_at_ms <= crate::now_ms()
            || grant.expires_at_ms > crate::now_ms().saturating_add(60_000)
        {
            return Err(CrossTaskError::UserRequest);
        }
        if request.environment_id != authority.local_environment_id
            || !authority.visible_project_ids.contains(&request.project_id)
        {
            return Err(CrossTaskError::TargetScope);
        }
        if request.task_id.is_empty()
            || request.task_id.len() > 128
            || request.project_id.is_empty()
            || request.project_id.len() > 128
            || !valid_key(&request.command_id)
        {
            return Err(CrossTaskError::Input);
        }
        match request.action {
            CrossTaskControlAction::Create => {
                if uuid::Uuid::parse_str(&request.task_id).is_err()
                    || request.expected_task_revision.is_some()
                    || request.expected_binding_generation.is_some()
                    || request.expected_turn_id.is_some()
                    || request.destination_project_id.is_some()
                {
                    return Err(CrossTaskError::Input);
                }
            }
            CrossTaskControlAction::Interrupt
            | CrossTaskControlAction::Archive
            | CrossTaskControlAction::Unarchive
            | CrossTaskControlAction::Move => {
                if request.task_id == authority.caller_task_id
                    || request.expected_task_revision.is_none()
                {
                    return Err(CrossTaskError::Input);
                }
                let target = self.target(authority, &request.task_id, cancel).await?;
                if target.project_id.as_deref() != Some(request.project_id.as_str()) {
                    return Err(CrossTaskError::TargetScope);
                }
                match request.action {
                    CrossTaskControlAction::Interrupt => {
                        if request
                            .expected_turn_id
                            .as_deref()
                            .is_none_or(str::is_empty)
                            || request.destination_project_id.is_some()
                        {
                            return Err(CrossTaskError::Input);
                        }
                    }
                    CrossTaskControlAction::Move => {
                        if request.expected_turn_id.is_some()
                            || request.destination_project_id.as_ref().is_none_or(|id| {
                                id == &request.project_id
                                    || !authority.visible_project_ids.contains(id)
                            })
                        {
                            return Err(CrossTaskError::TargetScope);
                        }
                    }
                    _ => {
                        if request.expected_turn_id.is_some()
                            || request.destination_project_id.is_some()
                        {
                            return Err(CrossTaskError::Input);
                        }
                    }
                }
            }
        }
        let receipt = with_cancel(
            cancel,
            self.backend.control(request.clone(), authority, grant),
        )
        .await?;
        let intent = match request.action {
            CrossTaskControlAction::Create => "createChat",
            CrossTaskControlAction::Interrupt => "stop",
            CrossTaskControlAction::Archive | CrossTaskControlAction::Unarchive => "setArchived",
            CrossTaskControlAction::Move => "moveProject",
        };
        if receipt.task_id != request.task_id
            || receipt.command.task_id != request.task_id
            || receipt.command.command_id != request.command_id
            || receipt.command.intent != intent
            || receipt.observed_status.chars().count() > 80
            || receipt.settled && !receipt.command.status.is_terminal()
        {
            return Err(CrossTaskError::InvalidBackendResult);
        }
        check_output(&receipt)?;
        Ok(receipt)
    }

    pub async fn invoke(
        &self,
        authority: &CrossTaskAuthority,
        name: &str,
        arguments: serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<serde_json::Value, CrossTaskError> {
        let output = match name {
            "list_threads" => {
                serde_json::to_value(self.list(authority, parse(arguments)?, cancel).await?)
            }
            "read_thread" => {
                serde_json::to_value(self.read(authority, parse(arguments)?, cancel).await?)
            }
            "send_message_to_thread" => {
                serde_json::to_value(self.send(authority, parse(arguments)?, cancel).await?)
            }
            "wait_threads" => {
                serde_json::to_value(self.wait(authority, parse(arguments)?, cancel).await?)
            }
            _ => return Err(CrossTaskError::Unavailable),
        }
        .map_err(|_| CrossTaskError::InvalidBackendResult)?;
        check_output(&output)?;
        Ok(output)
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
                "threads.read",
                capabilities.approved_queue,
                "one-use local approval, then durable queue receipt",
                40_000,
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
            } else if name == "send_message_to_thread"
                && (authority.allow_projectless || authority.visible_project_ids.len() != 1)
            {
                Some("approved send requires one exact local project")
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
        Self::validate_send_receipt(&request, &receipt)?;
        Ok(receipt)
    }

    /// Product send path. A provider call supplies only the request; the
    /// engine supplies the approved source and target fence separately.
    pub(crate) async fn send_approved(
        &self,
        authority: &CrossTaskAuthority,
        request: CrossTaskSendRequest,
        fence: &ApprovedSendFence,
        cancel: &CancellationToken,
    ) -> Result<CrossTaskSendReceipt, CrossTaskError> {
        self.check(
            authority,
            "threads.read",
            self.backend.capabilities().approved_queue,
        )?;
        if fence.source_task_id != authority.caller_task_id
            || fence.request != request
            || fence.target_snapshot.task_id != request.task_id
            || fence.expires_at_ms <= crate::now_ms()
            || request.mode != CrossTaskSendMode::Queue
            || request.message.trim().is_empty()
            || request.message.chars().count() > MAX_APPROVED_MESSAGE_CHARS
            || !valid_key(&request.idempotency_key)
        {
            return Err(CrossTaskError::UserRequest);
        }
        self.target(authority, &request.task_id, cancel).await?;
        let receipt = with_cancel(
            cancel,
            self.backend
                .enqueue_approved_queue(request.clone(), authority, fence),
        )
        .await?;
        Self::validate_send_receipt(&request, &receipt)?;
        Ok(receipt)
    }

    fn validate_send_receipt(
        request: &CrossTaskSendRequest,
        receipt: &CrossTaskSendReceipt,
    ) -> Result<(), CrossTaskError> {
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
        check_output(receipt)
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

    async fn control_evidence_target(
        &self,
        authority: &CrossTaskAuthority,
        task_id: &str,
        cancel: &CancellationToken,
    ) -> Result<CrossTaskSummary, CrossTaskError> {
        if task_id == authority.caller_task_id {
            return Err(CrossTaskError::TargetScope);
        }
        let caller = self
            .target(authority, &authority.caller_task_id, cancel)
            .await?;
        let target = self.target(authority, task_id, cancel).await?;
        if caller.project_id.is_none() || target.project_id != caller.project_id {
            return Err(CrossTaskError::TargetScope);
        }
        Ok(target)
    }
}

fn visible(authority: &CrossTaskAuthority, task: &CrossTaskSummary) -> bool {
    task.environment_id == authority.local_environment_id
        && match &task.project_id {
            Some(id) => authority.visible_project_ids.contains(id),
            None => false,
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

fn parse<T: serde::de::DeserializeOwned>(
    arguments: serde_json::Value,
) -> Result<T, CrossTaskError> {
    serde_json::from_value(arguments).map_err(|_| CrossTaskError::Input)
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
            "Request one exact local approval, then queue a follow-up. The receipt proves persistence, not provider delivery."
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
            "taskId":{"type":"string"},"message":{"type":"string","maxLength":MAX_APPROVED_MESSAGE_CHARS},
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
    use luna_proto::{CommandReceipt, TaskCommandSnapshot};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[derive(Clone, Default)]
    struct FakeBackend {
        queued: Arc<Mutex<HashMap<String, (String, CrossTaskSendReceipt)>>>,
        snapshot: Arc<Mutex<Option<TaskCommandSnapshot>>>,
        receipts: Arc<Mutex<HashMap<String, CommandReceipt>>>,
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

    fn control_snapshot() -> TaskCommandSnapshot {
        TaskCommandSnapshot {
            task_id: "task-a".into(),
            task_revision: 7,
            task_sequence: 12,
            binding_id: Some("binding-a".into()),
            binding_generation: Some(2),
            active_turn_id: Some("run-a".into()),
        }
    }

    fn control_receipt(command_id: &str) -> CommandReceipt {
        CommandReceipt {
            command_id: command_id.into(),
            task_id: "task-a".into(),
            intent: "stop".into(),
            payload_hash: "synthetic-hash".into(),
            status: CommandReceiptStatus::Pending,
            snapshot: control_snapshot(),
            event_sequence: None,
            actor_id: "luna-tool:caller".into(),
            client_id: "luna-cross-task".into(),
            client_session_id: "caller".into(),
            execution_environment: Some("local".into()),
            correlation_id: None,
            reason: None,
            recovery_action: None,
            accepted_at_ms: 1,
            committed_at_ms: None,
            observed_at_ms: 1,
        }
    }

    #[async_trait]
    impl CrossTaskBackend for FakeBackend {
        fn capabilities(&self) -> CrossTaskCapabilities {
            CrossTaskCapabilities {
                list: true,
                read: true,
                durable_queue: true,
                approved_queue: false,
                event_wait: true,
            }
        }

        async fn task(&self, task_id: &str) -> Result<Option<CrossTaskSummary>, CrossTaskError> {
            Ok(match task_id {
                "caller" | "task-a" => Some(task(task_id, "local")),
                "remote-task" => Some(task(task_id, "remote")),
                "foreign-project" => Some(CrossTaskSummary {
                    project_id: Some("project-b".into()),
                    ..task(task_id, "local")
                }),
                _ => None,
            })
        }

        async fn control_target_snapshot(
            &self,
            _task_id: &str,
            _authority: &CrossTaskAuthority,
        ) -> Result<TaskCommandSnapshot, CrossTaskError> {
            self.snapshot
                .lock()
                .await
                .clone()
                .ok_or(CrossTaskError::Unavailable)
        }

        async fn control_command_receipt(
            &self,
            _task_id: &str,
            command_id: &str,
            _authority: &CrossTaskAuthority,
        ) -> Result<Option<CommandReceipt>, CrossTaskError> {
            if command_id == "lookup-error" {
                return Err(CrossTaskError::Backend("synthetic readback failure".into()));
            }
            Ok(self.receipts.lock().await.get(command_id).cloned())
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
                turns: vec![luna_proto::CrossTaskTurnSummary {
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
    async fn control_evidence_snapshot_is_local_exact_and_revalidates_every_field() {
        let backend = FakeBackend::default();
        *backend.snapshot.lock().await = Some(control_snapshot());
        let delegated: Arc<dyn CrossTaskBackend> = Arc::new(backend.clone());
        let tools = CrossTaskTools::new(delegated);
        let cancel = CancellationToken::new();
        let captured = tools
            .control_target_snapshot(&authority(), "task-a", &cancel)
            .await
            .unwrap();
        assert_eq!(captured, control_snapshot());
        tools
            .revalidate_control_target_snapshot(&authority(), "task-a", &captured, &cancel)
            .await
            .unwrap();

        backend
            .snapshot
            .lock()
            .await
            .as_mut()
            .unwrap()
            .task_sequence += 1;
        assert_eq!(
            tools
                .revalidate_control_target_snapshot(&authority(), "task-a", &captured, &cancel)
                .await,
            Err(CrossTaskError::StaleSnapshot)
        );
        for task_id in ["missing", "remote-task", "foreign-project", "caller"] {
            assert_eq!(
                tools
                    .control_target_snapshot(&authority(), task_id, &cancel)
                    .await,
                Err(CrossTaskError::TargetScope),
                "{task_id}"
            );
        }
        *backend.snapshot.lock().await = None;
        assert_eq!(
            tools
                .control_target_snapshot(&authority(), "task-a", &cancel)
                .await,
            Err(CrossTaskError::Unavailable)
        );
        *backend.snapshot.lock().await = Some(TaskCommandSnapshot {
            task_id: "other-task".into(),
            ..control_snapshot()
        });
        assert_eq!(
            tools
                .control_target_snapshot(&authority(), "task-a", &cancel)
                .await,
            Err(CrossTaskError::InvalidBackendResult)
        );
        let mut no_read = authority();
        no_read.granted_scopes.remove("threads.read");
        assert_eq!(
            tools
                .control_target_snapshot(&no_read, "task-a", &cancel)
                .await,
            Err(CrossTaskError::Scope)
        );
        let stopped = CancellationToken::new();
        stopped.cancel();
        assert_eq!(
            tools
                .control_target_snapshot(&authority(), "task-a", &stopped)
                .await,
            Err(CrossTaskError::Cancelled)
        );
    }

    #[tokio::test]
    async fn control_evidence_receipt_distinguishes_unknown_error_and_mismatch() {
        let backend = FakeBackend::default();
        let receipt = control_receipt("command-a");
        backend
            .receipts
            .lock()
            .await
            .insert("command-a".into(), receipt.clone());
        let delegated: Arc<dyn CrossTaskBackend> = Arc::new(backend.clone());
        let tools = CrossTaskTools::new(delegated);
        let cancel = CancellationToken::new();
        assert_eq!(
            tools
                .control_command_receipt(&authority(), "task-a", "command-a", &cancel)
                .await,
            Ok(Some(receipt.clone()))
        );
        assert_eq!(receipt.status, CommandReceiptStatus::Pending);
        assert_eq!(
            tools
                .control_command_receipt(&authority(), "task-a", "command-missing", &cancel)
                .await,
            Ok(None),
            "no receipt is unknown, not proof of non-execution"
        );
        assert!(matches!(
            tools
                .control_command_receipt(&authority(), "task-a", "lookup-error", &cancel)
                .await,
            Err(CrossTaskError::Backend(_))
        ));
        for task_id in ["missing", "remote-task", "foreign-project", "caller"] {
            assert_eq!(
                tools
                    .control_command_receipt(&authority(), task_id, "command-a", &cancel)
                    .await,
                Err(CrossTaskError::TargetScope)
            );
        }
        assert_eq!(
            tools
                .control_command_receipt(&authority(), "task-a", "", &cancel)
                .await,
            Err(CrossTaskError::Input)
        );
        for case in 0..8 {
            let mut mismatched = receipt.clone();
            match case {
                0 => mismatched.command_id = "other-command".into(),
                1 => mismatched.task_id = "other-task".into(),
                2 => mismatched.snapshot.task_id = "other-task".into(),
                3 => mismatched.execution_environment = Some("remote".into()),
                4 => mismatched.client_id = "other-client".into(),
                5 => mismatched.actor_id = "luna-tool:other-caller".into(),
                6 => mismatched.client_session_id = "other-caller".into(),
                7 => mismatched.intent = "queueMessage".into(),
                _ => unreachable!(),
            }
            backend
                .receipts
                .lock()
                .await
                .insert("command-a".into(), mismatched);
            assert_eq!(
                tools
                    .control_command_receipt(&authority(), "task-a", "command-a", &cancel)
                    .await,
                Err(CrossTaskError::InvalidBackendResult),
                "case {case}"
            );
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
            targets: vec![luna_proto::CrossTaskWaitTarget {
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
        assert_eq!(attached.len(), 3);
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
        assert!(!attached.iter().any(|tool| tool.name == "create_thread"));
        assert!(!attached.iter().any(|tool| tool.name == "interrupt_thread"));
    }

    #[tokio::test]
    async fn control_requires_exact_short_lived_user_grant() {
        let tools = CrossTaskTools::new(FakeBackend::default());
        let mut authority = authority();
        authority.granted_scopes.insert("threads.manage".into());
        let request = CrossTaskControlRequest {
            action: CrossTaskControlAction::Archive,
            task_id: "task-a".into(),
            project_id: "project-a".into(),
            environment_id: "local".into(),
            command_id: "archive-a".into(),
            destination_project_id: None,
            expected_task_revision: Some(1),
            expected_binding_generation: None,
            expected_turn_id: None,
        };
        let grant = CrossTaskControlGrant {
            caller_task_id: "caller".into(),
            request: request.clone(),
            expires_at_ms: crate::now_ms() + 30_000,
        };
        let cancel = CancellationToken::new();
        let changed = CrossTaskControlRequest {
            command_id: "archive-b".into(),
            ..request.clone()
        };
        assert_eq!(
            tools
                .control(&authority, &grant, changed, &cancel)
                .await
                .unwrap_err(),
            CrossTaskError::UserRequest
        );
        let far_future = CrossTaskControlGrant {
            expires_at_ms: crate::now_ms() + 120_000,
            ..grant.clone()
        };
        assert_eq!(
            tools
                .control(&authority, &far_future, request.clone(), &cancel)
                .await
                .unwrap_err(),
            CrossTaskError::UserRequest
        );
        authority.explicit_user_request = false;
        assert_eq!(
            tools
                .control(&authority, &grant, request, &cancel)
                .await
                .unwrap_err(),
            CrossTaskError::UserRequest
        );
    }

    #[tokio::test]
    async fn compiled_runtime_dispatches_only_attached_local_reads() {
        let backend: Arc<dyn CrossTaskBackend> = Arc::new(FakeBackend::default());
        let mut authority = authority();
        authority.explicit_user_request = false;
        let (cross_task_tools, cross_task_omissions) =
            CrossTaskTools::new(backend.clone()).catalog(&authority);
        let context = luna_proto::RuntimeContext {
            app_version: "synthetic".into(),
            protocol_version: 1,
            thread_id: "caller".into(),
            parent_thread_id: None,
            source_thread_id: None,
            project_id: Some("project-a".into()),
            project_name: Some("Synthetic".into()),
            environment_id: "local".into(),
            machine_id: "local".into(),
            machine_name: "Synthetic".into(),
            workspace_root: "/synthetic/workspace".into(),
            worktree_id: None,
            binding_generation: None,
            provider_profile_id: None,
            branch: None,
            harness: luna_proto::HarnessId::Codex,
            model: None,
            reasoning: None,
            execution_route: "local".into(),
            granted_scopes: vec!["threads.read".into()],
        };
        let contract = crate::runtime_contract::compile(crate::runtime_contract::CompileInput {
            context,
            provider_delivery: RuntimeDelivery::CodexDynamicTools,
            user_instructions: Vec::new(),
            workspace_instructions: Vec::new(),
            resume: false,
            acknowledged_contract: None,
            cross_task_tools,
            cross_task_omissions,
        })
        .unwrap()
        .contract;
        let cancel = CancellationToken::new();
        let list = crate::runtime_contract::invoke_with_backend(
            &contract,
            "list_threads",
            serde_json::json!({}),
            &cancel,
            Some(backend.clone()),
        )
        .await
        .unwrap();
        assert_eq!(list["tasks"][0]["taskId"], "task-a");
        assert!(
            crate::runtime_contract::invoke_with_backend(
                &contract,
                "send_message_to_thread",
                serde_json::json!({"taskId":"task-a","message":"synthetic","idempotencyKey":"one"}),
                &cancel,
                Some(backend),
            )
            .await
            .is_err()
        );
    }
}
