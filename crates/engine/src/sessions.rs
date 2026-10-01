//! SessionsEngine — per-chat agent runs: dispatch, steering, interrupts, input bridging,
//! journal + broadcast fan-out, and 120ms coalesced doc streaming.
//!
//! Pragmatic port of luna's `sessions.ts` (spec: feature-inventory §3.2):
//! - every `AgentEvent` is (a) appended to the on-disk run journal, (b) broadcast to
//!   in-process subscribers, (c) folded via `fold_event_into_parts` and diffed into the
//!   chat's `SessionDoc` through `SegmentWriter` on a coalesced `STREAM_COMMIT_MS` timer;
//! - the user message entry is pushed to the doc immediately on dispatch (id = the
//!   command's client-minted message id, so optimistic echoes never flicker);
//! - a `Steered` event splits the assistant entry at the exact boundary;
//! - recovery (interrupt or a stale journal at boot) stamps the streaming entry `aborted`.
//!
//! Scope notes: sessions are keyed by chat id (one live run per chat). Luna's pulse
//! loop is ported as the 15s liveness heartbeat in `drive_run`; its stall watchdog is
//! deliberately NOT ported (rejected in review — agents may legitimately wait on
//! something for far longer than any timeout, and a live child IS the working signal).
//! Every dying path must instead carry its own visible error (child crash with stderr,
//! spawn failure, stream error, engine-restart recovery).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use chrono::Utc;
use futures::StreamExt;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, broadcast, mpsc, oneshot, watch};

use luna_doc::{
    DocError, MessagePart, MessageRole, MessageStatus, STREAM_COMMIT_MS, SegmentWriter,
    SessionCommandPayload, SessionCommandStatus, SessionDoc, SessionMessageEntry,
    fold_event_into_parts, sanitize_tool_call,
};
use luna_harness::{
    CancellationToken, Harness, HarnessError, RunControls, SteerAcceptance, SteerMessage,
};
use luna_proto::{
    AgentEvent, ChatConfig, CommandReceiptStatus, CompactMechanism, CompactOperation,
    ContextControlState, ContextOperationStatus, DoneStatus, HarnessId, PermissionAuditEntry,
    PermissionMode, PermissionPolicy, RecoveryAcceptance, RecoveryFailure, RecoveryStatus,
    RunRequest, SandboxLevel, Session, SessionStatus, TaskRecovery, UserInputAnswer,
    UserInputQuestion,
};

use crate::device_write::DeviceWritePermit;
use luna_sync::DocsStore;

use crate::doc_host::{ChatDocHandle, DocHost};
use crate::recovery::{
    MAX_AUTO_RETRIES, MAX_AUTO_WAIT_MS, RecoveryChange, RecoveryRecord, RecoveryStore,
    classify_failure, recovery_decision,
};
use crate::registry::{HarnessDescriptor, HarnessRegistry};
use crate::run_journal::RunJournal;
use crate::{EngineError, new_id, now_ms};

mod native_subagents;

/// One journaled event: the durable seq plus the event, as broadcast to subscribers.
#[derive(Debug, Clone)]
pub struct JournaledEvent {
    pub seq: u64,
    pub event: AgentEvent,
}

/// Transport previews are bounded independently of retained host payloads.
fn tool_event_preview(event: &AgentEvent) -> AgentEvent {
    fn cap(text: &str, limit: usize) -> String {
        if text.len() <= limit {
            return text.to_owned();
        }
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!(
            "{}\n… [preview limited; full received output is on the host]",
            &text[..end]
        )
    }
    fn diff_preview(diff: &luna_proto::ToolDiff) -> luna_proto::ToolDiff {
        luna_proto::ToolDiff {
            path: diff.path.clone(),
            old_text: diff.old_text.as_deref().map(|t| cap(t, 64 * 1024)),
            new_text: cap(&diff.new_text, 64 * 1024),
            unified_diff: diff.unified_diff.as_deref().map(|t| cap(t, 64 * 1024)),
        }
    }
    match event {
        AgentEvent::ToolCall { id, call } => AgentEvent::ToolCall {
            id: id.clone(),
            call: sanitize_tool_call(call),
        },
        AgentEvent::ToolResult {
            id,
            is_error,
            output,
            diff,
        } => AgentEvent::ToolResult {
            id: id.clone(),
            is_error: *is_error,
            output: output.as_deref().map(|t| {
                luna_doc::rich_tool_output_summary(t, *is_error)
                    .unwrap_or_else(|| cap(t, 64 * 1024))
            }),
            diff: diff.as_ref().map(diff_preview),
        },
        AgentEvent::ToolActivity { id, kind, text } => {
            let text = if *kind == luna_proto::ToolActivityKind::Patch {
                serde_json::from_str::<luna_proto::ToolDiff>(text)
                    .ok()
                    .and_then(|d| serde_json::to_string(&diff_preview(&d)).ok())
                    .unwrap_or_default()
            } else {
                cap(text, luna_doc::TOOL_ACTIVITY_PREVIEW_BYTES)
            };
            AgentEvent::ToolActivity {
                id: id.clone(),
                kind: *kind,
                text,
            }
        }
        AgentEvent::Subagent {
            parent_tool_use_id,
            event,
        } => AgentEvent::Subagent {
            parent_tool_use_id: parent_tool_use_id.clone(),
            event: Box::new(tool_event_preview(event)),
        },
        _ => event.clone(),
    }
}

/// Outcome of a steer attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerOutcome {
    /// The provider accepted a native in-turn steering request.
    Accepted,
    /// No live steerable run.
    NotSteerable,
    /// A prior native steer is unsettled or the mailbox is full; this prompt
    /// was definitely not sent and must not start a competing turn.
    Busy,
    /// The provider definitively rejected native in-turn steering.
    Rejected,
    /// Acceptance could not be proved; callers must preserve the durable row
    /// and must not replay it automatically.
    Unknown,
}

/// Prepared registry metadata plus the transition guards that keep runtime
/// dispatch excluded until the caller durably commits the task projection.
pub(crate) struct PreparedTaskConfig {
    pub(crate) config: ChatConfig,
    _account_transition: Option<OwnedMutexGuard<()>>,
    _chat_transition: OwnedMutexGuard<()>,
}

struct PendingInput {
    kind: PendingInputKind,
    questions: Vec<UserInputQuestion>,
    resolver: oneshot::Sender<Vec<UserInputAnswer>>,
}

#[derive(Debug)]
enum PendingInputKind {
    Provider,
    LunaSend {
        run_id: String,
        proposal: Box<LunaSendProposal>,
        grant_tx: oneshot::Sender<crate::cross_task_tools::ApprovedSendFence>,
    },
    // Only engine-owned registration may select this kind. The run binding is
    // never derived from a serialized question or client-provided source hint.
    #[allow(dead_code)]
    LunaCrossTask {
        run_id: String,
        proposal: Box<LunaSyntheticArchiveProposal>,
        grant_tx: oneshot::Sender<LunaSyntheticArchiveGrant>,
    },
    /// Dormant prospective Interrupt question. Only the private local-UI
    /// response seam can send its one-shot grant; no command path consumes it.
    #[allow(dead_code)]
    LunaProspectiveInterrupt {
        run_id: String,
        proposal: Box<LunaProspectiveInterruptProposal>,
        grant_tx: oneshot::Sender<LunaProspectiveInterruptGrant>,
    },
}

#[derive(Debug)]
struct LunaSendProposal {
    child_request: Option<luna_proto::CreateChildThreadRequest>,
    request_id: String,
    claim: ClaimedProviderCall,
    fence: crate::cross_task_tools::ApprovedSendFence,
    project_id: String,
    environment_id: String,
}

impl LunaSendProposal {
    fn question(&self) -> UserInputQuestion {
        if let Some(request) = &self.child_request {
            return UserInputQuestion {
                id: format!("question-{}", self.request_id),
                header: "LUNA approval".into(),
                question: format!(
                    "Create a fresh persistent child {} under {} in project {} on {}?\nExisting checkout: {}\nModel: {:?}; reasoning: {:?}; title: {:?}\nResults: {}; queue parent continuation: {}. Permissions are capped by the parent. Only the explicit prompt is copied.\n\n{}",
                    self.fence.request.task_id,
                    self.claim.source_task_id,
                    self.project_id,
                    self.environment_id,
                    request.cwd.as_deref().unwrap_or("parent checkout"),
                    request.model,
                    request.reasoning,
                    request.title,
                    request.report_results,
                    request.continue_parent,
                    request.prompt
                ),
                options: vec!["Deny".into(), "Allow once".into()],
                multi_select: false,
                mcp: None,
                approval: Some(luna_proto::ApprovalRequest {
                    provider_request_id: self.claim.provider_call_id.clone(),
                    provider: HarnessId::Codex,
                    turn_id: Some(self.claim.provider_turn_id.clone()),
                    machine_name: self.environment_id.clone(),
                    action_kind: luna_proto::ApprovalActionKind::LunaCrossTask,
                    summary: "Create a persistent child chat".into(),
                    requested_scope: luna_proto::PermissionScope::Workspace,
                    expires_at_ms: self.fence.expires_at_ms,
                    supports_task_allowance: false,
                    details: None,
                }),
                input: None,
            };
        }
        UserInputQuestion {
            id: format!("question-{}", self.request_id),
            header: "LUNA approval".into(),
            question: format!(
                "Queue this exact message to task {} in project {} on {} at revision {} and turn {:?}?\n\n{}",
                self.fence.request.task_id,
                self.project_id,
                self.environment_id,
                self.fence.target_snapshot.task_revision,
                self.fence.target_snapshot.active_turn_id,
                self.fence.request.message,
            ),
            options: vec!["Deny".into(), "Allow once".into()],
            multi_select: false,
            mcp: None,
            approval: Some(luna_proto::ApprovalRequest {
                provider_request_id: self.claim.provider_call_id.clone(),
                provider: HarnessId::Codex,
                turn_id: Some(self.claim.provider_turn_id.clone()),
                machine_name: self.environment_id.clone(),
                action_kind: luna_proto::ApprovalActionKind::LunaCrossTask,
                summary: format!("Queue a follow-up to task {}", self.fence.request.task_id),
                requested_scope: luna_proto::PermissionScope::Workspace,
                expires_at_ms: self.fence.expires_at_ms,
                supports_task_allowance: false,
                details: None,
            }),
            input: None,
        }
    }
}

/// Fake-backed proof payload only. No production run registers one yet, and
/// its serialized question is never used as authority.
#[derive(Debug)]
#[allow(dead_code)]
struct LunaSyntheticArchiveProposal {
    request_id: String,
    source_task_id: String,
    source_project_id: String,
    source_environment_id: String,
    source_binding: ProviderBindingKey,
    source_run_id: String,
    source_turn_id: String,
    source_call_id: String,
    target_task_id: String,
    target_project_id: String,
    target_environment_id: String,
    target_snapshot: luna_proto::TaskCommandSnapshot,
    request: luna_proto::CrossTaskControlRequest,
    request_digest: [u8; 32],
    expires_at_ms: i64,
}

impl LunaSyntheticArchiveProposal {
    fn request_digest(request: &luna_proto::CrossTaskControlRequest) -> Option<[u8; 32]> {
        use sha2::Digest as _;
        let bytes = serde_json::to_vec(request).ok()?;
        Some(sha2::Sha256::digest(bytes).into())
    }

    fn question(&self) -> UserInputQuestion {
        UserInputQuestion {
            id: format!("question-{}", self.request_id),
            header: "LUNA approval".into(),
            question: format!(
                "Archive task {} in project {} on {} at revision {} and binding {:?} once?",
                self.target_task_id,
                self.target_project_id,
                self.target_environment_id,
                self.target_snapshot.task_revision,
                self.target_snapshot.binding_generation,
            ),
            options: vec!["Deny".into(), "Allow once".into()],
            multi_select: false,
            mcp: None,
            approval: Some(luna_proto::ApprovalRequest {
                provider_request_id: self.source_call_id.clone(),
                provider: HarnessId::Codex,
                turn_id: Some(self.source_turn_id.clone()),
                machine_name: "synthetic".into(),
                action_kind: luna_proto::ApprovalActionKind::LunaCrossTask,
                summary: "Synthetic archive".into(),
                requested_scope: luna_proto::PermissionScope::Workspace,
                expires_at_ms: self.expires_at_ms,
                supports_task_allowance: false,
                details: None,
            }),
            input: None,
        }
    }

    fn matches_pending(
        &self,
        chat_id: &str,
        run_id: &str,
        request_id: &str,
        question: &UserInputQuestion,
    ) -> bool {
        self.request_id == request_id
            && self.source_task_id == chat_id
            && self.source_run_id == run_id
            && question == &self.question()
            && self.source_binding.task_id == self.source_task_id
            && !self.source_binding.binding_id.is_empty()
            && !self.source_binding.provider_thread_id.is_empty()
            && !self.source_turn_id.is_empty()
            && !self.source_call_id.is_empty()
            && self.source_task_id != self.target_task_id
            && self.source_project_id == self.target_project_id
            && self.source_environment_id == self.target_environment_id
            && self.request.action == luna_proto::CrossTaskControlAction::Archive
            && self.request.task_id == self.target_task_id
            && self.request.project_id == self.target_project_id
            && self.request.environment_id == self.target_environment_id
            && self.request.expected_task_revision == Some(self.target_snapshot.task_revision)
            && self.request.expected_binding_generation == self.target_snapshot.binding_generation
            && self.request.expected_turn_id.is_none()
            && self.request.destination_project_id.is_none()
            && self.target_snapshot.task_id == self.target_task_id
            && self.target_snapshot.binding_id.is_some()
            && self.target_snapshot.binding_generation.is_some()
            && self.target_snapshot.active_turn_id.is_none()
            && Self::request_digest(&self.request) == Some(self.request_digest)
    }

    fn matches_live_claim(
        &self,
        run: &RunHandle,
        current_binding: Option<&ProviderBindingKey>,
    ) -> bool {
        self.source_run_id == run.run_id
            && run.runtime_config.harness_id == HarnessId::Codex
            && current_binding == Some(&self.source_binding)
            && run.run_provider_binding.as_ref() == Some(&self.source_binding)
            && *run.provider_turn_readiness.borrow() == ProviderTurnReadiness::Ready
            && !run.interrupt_token.is_cancelled()
            && !*run.cancel.subscribe().borrow()
            && run.provider_turn.as_ref().is_some_and(|turn| {
                turn.run_id == self.source_run_id
                    && turn.binding == self.source_binding
                    && turn.provider_turn_id == self.source_turn_id
                    && turn.claimed_call_ids.contains(&self.source_call_id)
            })
    }
}

/// Moved through one private channel after a direct local UI AllowOnce. This
/// cannot be cloned or serialized into provider/model arguments.
#[derive(Debug)]
struct LunaSyntheticArchiveGrant {
    #[allow(dead_code)]
    proposal: LunaSyntheticArchiveProposal,
}

/// One private, non-cloneable AllowOnce result. It contains no command ID or
/// dispatcher and cannot be reconstructed from a serialized UI answer.
#[derive(Debug)]
#[allow(dead_code)]
struct LunaProspectiveInterruptGrant {
    proposal: LunaProspectiveInterruptProposal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LunaInterruptDispatchStatus {
    Applied,
    Rejected,
    Conflict,
    Pending,
    Unknown,
}

/// A private readback, not a model-visible tool result. Applied and settled
/// remain separate, and Unknown never authorizes a new command ID or retry.
#[derive(Debug)]
#[allow(dead_code)]
struct LunaInterruptDispatchResult {
    command_id: String,
    status: LunaInterruptDispatchStatus,
    receipt: Option<luna_proto::CommandReceipt>,
    settled: bool,
}

/// Result of the in-memory secret answer path ([`SessionsEngine::respond_secret_input`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretInputOutcome {
    /// Handed to the live provider resolver for exactly this request.
    Delivered,
    /// The request had no live resolver (turn ended, restart): its card was
    /// closed as cancelled and the answer was dropped, never stored.
    Closed,
    /// The request is not a pending secret question; nothing was sent.
    NotSecret,
    /// No such open request for this task.
    Unavailable,
}

fn has_secret_question(questions: &[UserInputQuestion]) -> bool {
    questions.iter().any(UserInputQuestion::is_secret)
}

fn has_answer_text(answers: &[UserInputAnswer]) -> bool {
    answers
        .iter()
        .any(|answer| answer.labels.iter().any(|label| !label.is_empty()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LunaApprovalOutcome {
    AllowedOnce,
    Denied,
    Expired,
    Stale,
    RunEnded,
}

type PendingInputs = Arc<Mutex<HashMap<String, PendingInput>>>;

// Adapter cancellation closes the receiver. Remove the corresponding UI
// prompt promptly, including when the provider finishes a turn or disconnects.
fn schedule_mcp_input_cleanup(
    pending: PendingInputs,
    engine_tx: mpsc::UnboundedSender<AgentEvent>,
    request_id: String,
    questions: &[UserInputQuestion],
) {
    let Some(expires_at_ms) = questions
        .iter()
        .filter_map(|q| q.mcp.as_ref().map(|m| m.expires_at_ms))
        .min()
    else {
        return;
    };
    let pending = Arc::downgrade(&pending);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            let Some(pending) = pending.upgrade() else {
                break;
            };
            let invalid = {
                let map = lock(&pending);
                let Some(input) = map.get(&request_id) else {
                    break;
                };
                input.resolver.is_closed() || now_ms() >= expires_at_ms
            };
            if invalid {
                expire_pending_approval(&pending, &engine_tx, &request_id);
                break;
            }
        }
    });
}

fn schedule_approval_expiry(
    pending: PendingInputs,
    engine_tx: mpsc::UnboundedSender<AgentEvent>,
    request_id: String,
    questions: &[UserInputQuestion],
) {
    let Some(expires_at_ms) = questions
        .iter()
        .filter_map(|question| {
            question
                .approval
                .as_ref()
                .map(|approval| approval.expires_at_ms)
        })
        .min()
    else {
        return;
    };
    // A request with native choices, or a Codex request that waits without a
    // provider deadline (v2 sends none), is retired as soon as the adapter
    // closes it (`serverRequest/resolved`, Stop or the end of its turn).
    let native_choices = questions.iter().any(|question| {
        question.approval.as_ref().is_some_and(|approval| {
            approval.details.is_some()
                || (approval.provider == HarnessId::Codex && approval.expires().is_none())
        })
    });
    tokio::spawn(async move {
        let delay_ms = expires_at_ms.saturating_sub(now_ms()).max(0) as u64;
        if native_choices {
            // The adapter closes its answer receiver on remote resolution or
            // turn replacement. Retire that exact UI request promptly too.
            let deadline = tokio::time::sleep(std::time::Duration::from_millis(delay_ms));
            tokio::pin!(deadline);
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    _ = interval.tick() => {
                        let closed = lock(&pending).get(&request_id).map(|input| input.resolver.is_closed());
                        match closed {
                            None => return,
                            Some(true) => break,
                            Some(false) => {},
                        }
                    }
                }
            }
        } else {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        expire_pending_approval(&pending, &engine_tx, &request_id);
    });
}

fn schedule_luna_prospective_cleanup(
    pending: &PendingInputs,
    engine_tx: mpsc::UnboundedSender<AgentEvent>,
    request_id: String,
    expires_at_ms: i64,
    interrupt: CancellationToken,
    mut cancel: watch::Receiver<bool>,
    mut readiness: watch::Receiver<ProviderTurnReadiness>,
) {
    // A weak map cannot keep a dead run, its responder, or a backend alive.
    let pending = Arc::downgrade(pending);
    tokio::spawn(async move {
        let delay_ms = expires_at_ms.saturating_sub(now_ms()).max(0) as u64;
        tokio::select! {
            biased;
            _ = interrupt.cancelled() => {},
            _ = cancel.wait_for(|value| *value) => {},
            _ = readiness.changed() => {},
            _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {},
        }
        if let Some(pending) = pending.upgrade() {
            let is_prospective = lock(&pending).get(&request_id).is_some_and(|entry| {
                matches!(
                    &entry.kind,
                    PendingInputKind::LunaProspectiveInterrupt { .. }
                )
            });
            if is_prospective {
                expire_pending_approval(&pending, &engine_tx, &request_id);
            }
        }
    });
}

fn expire_pending_approval(
    pending: &PendingInputs,
    engine_tx: &mpsc::UnboundedSender<AgentEvent>,
    request_id: &str,
) -> bool {
    let Some(pending) = lock(pending).remove(request_id) else {
        return false;
    };
    // Empty answers are the provider-neutral fail-closed response. Each
    // adapter maps it to deny/cancel on its native wire.
    let _ = pending.resolver.send(Vec::new());
    let _ = engine_tx.send(AgentEvent::InputResolved {
        request_id: request_id.to_string(),
    });
    true
}

fn take_pending_ordinary_input(
    pending_inputs: &PendingInputs,
    request_id: &str,
) -> Option<PendingInput> {
    let mut pending_inputs = lock(pending_inputs);
    let pending = pending_inputs.get(request_id)?;
    if matches!(
        &pending.kind,
        PendingInputKind::LunaCrossTask { .. }
            | PendingInputKind::LunaProspectiveInterrupt { .. }
            | PendingInputKind::LunaSend { .. }
    ) || pending
        .questions
        .iter()
        .any(|question| question.approval.is_some())
    {
        // Typed approvals cannot leave through ordinary input. Provider
        // approvals use `respond_approval`; LUNA approvals use the in-process
        // local UI route.
        return None;
    }
    pending_inputs.remove(request_id)
}

fn push_permission_audit(config: &mut ChatConfig, entry: PermissionAuditEntry) {
    config.permission_audit.push(entry);
    if config.permission_audit.len() > 100 {
        let excess = config.permission_audit.len() - 100;
        config.permission_audit.drain(..excess);
    }
}

fn apply_permission_policy(config: &mut ChatConfig, policy: &PermissionPolicy, actor: String) {
    let previous = config.effective_permission_mode;
    config.effective_permission_mode = policy.effective_mode;
    config.permission_ceiling_source = policy.ceiling_source;
    config.permission_degradation_reason = policy.degradation_reason;
    let already_recorded = config.permission_audit.last().is_some_and(|entry| {
        entry.requested_mode == policy.requested_mode
            && entry.effective_mode == policy.effective_mode
            && entry.provider_mapping == policy.provider_mapping
            && entry.ceiling_source == policy.ceiling_source
    });
    if !already_recorded {
        push_permission_audit(
            config,
            PermissionAuditEntry {
                actor,
                machine: policy.machine_name.clone(),
                requested_mode: policy.requested_mode,
                effective_mode: policy.effective_mode,
                previous_mode: Some(previous),
                provider_mapping: policy.provider_mapping.clone(),
                ceiling_source: policy.ceiling_source,
                reason: policy.degradation_reason,
                timestamp_ms: now_ms(),
            },
        );
    }
}

/// A harness-native session id plus the cwd it was created under. Harness
/// session stores are cwd-scoped (claude keys conversations by project
/// directory — luna sessions.ts:563 "harness session stores are keyed by
/// cwd"), so resume is only injected for runs launched from the same cwd.
#[derive(Debug, Clone)]
struct HarnessSessionRef {
    session_id: String,
    cwd: String,
}

/// Configuration baked into a live harness runtime. The steering mailbox only
/// carries prompt text, so routing a request whose model/effort/sandbox changed
/// would silently run it with the old process configuration. Such requests
/// must replace the runtime and resume its harness-native session instead.
#[derive(Debug, Clone, PartialEq)]
struct RuntimeConfig {
    harness_id: HarnessId,
    provider_account_id: Option<String>,
    model: Option<String>,
    reasoning: Option<luna_proto::ReasoningLevel>,
    model_options: serde_json::Map<String, serde_json::Value>,
    cwd: String,
    sandbox: luna_proto::SandboxLevel,
    auto_approve: bool,
    permission_policy: Option<luna_proto::PermissionPolicy>,
    worktree: Option<luna_proto::WorktreeSpec>,
}

impl RuntimeConfig {
    fn from_request(harness_id: HarnessId, request: &RunRequest) -> Self {
        Self {
            harness_id,
            provider_account_id: request.provider_account_id.clone(),
            model: request.model.clone(),
            reasoning: request.reasoning,
            // Per-message correlation is not runtime configuration: a stored
            // first-message id must never force a warm runtime to restart.
            model_options: request
                .model_options
                .iter()
                .filter(|(key, _)| {
                    !matches!(
                        key.as_str(),
                        luna_harness::codex::LUNA_STEER_RECONCILE_OPTION
                            | luna_harness::codex::LUNA_CLIENT_USER_MESSAGE_ID_OPTION
                    )
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            cwd: request.cwd.clone(),
            sandbox: request.sandbox,
            auto_approve: request.auto_approve,
            permission_policy: request.permission_policy.clone(),
            worktree: request.worktree.clone(),
        }
    }

    /// A Codex goal start runs on the loaded runtime when its execution
    /// configuration matches, including the runtime contract (a changed
    /// contract still needs a fresh runtime at this safe boundary). Only the
    /// native thread-name and strict-resume options are ignored: they describe
    /// how a COLD runtime binds its thread and are added after routing.
    fn can_route_goal(&self, harness_id: HarnessId, request: &RunRequest) -> bool {
        let execution = |config: &Self| {
            let mut config = config.clone();
            for key in [
                luna_harness::codex::LUNA_NATIVE_THREAD_NAME_OPTION,
                luna_harness::codex::LUNA_STRICT_RESUME_OPTION,
            ] {
                config.model_options.remove(key);
            }
            config
        };
        harness_id == HarnessId::Codex
            && request.review.is_none()
            && request.goal.is_some()
            && execution(self) == execution(&Self::from_request(harness_id, request))
    }

    fn can_route(&self, harness_id: HarnessId, request: &RunRequest) -> bool {
        // Codex's warm mailbox carries typed inputs and attachments. Other
        // harnesses' typed inputs take a fresh (resumed) dispatch, whose
        // rejection is recoverable; the warm fallback is at-least-once.
        // Native review targets require a fresh adapter dispatch (resuming
        // the same binding) at a safe boundary.
        request.review.is_none()
            && (harness_id == HarnessId::Codex
                || (request.attachments.is_empty() && request.input_items.is_empty()))
            && self == &Self::from_request(harness_id, request)
    }
}

struct RunHandle {
    run_id: String,
    workspace_repair_revision: u64,
    /// Positive evidence that a parked adapter stream reached EOF. A terminal
    /// Done or removal after an error does not prove its child released native
    /// writer ownership yet.
    idle_stream_closed: Arc<AtomicBool>,
    /// In-memory provider turn evidence for a future per-request LUNA grant.
    /// Its presence also proves a turn is active, while `active_turn_id()`
    /// returns the engine run ID for command fencing.
    provider_turn: Option<ProviderTurnFence>,
    /// The binding durably published for this provider run's SessionStarted.
    /// A warm steer may reuse it, but never adopt a changed generation.
    run_provider_binding: Option<ProviderBindingKey>,
    provider_turn_readiness: watch::Sender<ProviderTurnReadiness>,
    steerable: bool,
    runtime_config: RuntimeConfig,
    steer_tx: mpsc::Sender<SteerMessage>,
    /// Harness-level cancellation (protocol interrupt + child teardown).
    interrupt_token: CancellationToken,
    /// Engine-level cancel: arms the run task's grace deadline so a harness that
    /// ignores its token can never strand the run.
    cancel: watch::Sender<bool>,
    /// Ordinary shutdown/interruption holds recovery. A durable Stop command
    /// clears this so its receipt and Held transition commit atomically later.
    hold_recovery_on_interrupt: Arc<AtomicBool>,
    engine_tx: mpsc::UnboundedSender<AgentEvent>,
    pending_inputs: PendingInputs,
    /// Steers accepted into the mailbox but not yet confirmed by a `Steered`
    /// event. Ordinary warm routes retain their at-least-once follow-up-turn
    /// fallback. Explicit native steers never replay: the run task converts
    /// any unconfirmed explicit entry to delivery-unknown at exit.
    routed_steers: Arc<Mutex<std::collections::VecDeque<RoutedSteer>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderBindingKey {
    task_id: String,
    binding_id: String,
    generation: u32,
    provider_thread_id: String,
}

/// Evidence from one adapter-validated call claimed against this live run.
/// This is private, non-cloneable, and never an approval or command authority.
#[derive(Debug)]
struct ClaimedProviderCall {
    source_task_id: String,
    source_run_id: String,
    source_binding: ProviderBindingKey,
    provider_turn_id: String,
    provider_call_id: String,
}

impl ClaimedProviderCall {
    fn matches_live(&self, run: &RunHandle, binding: Option<&ProviderBindingKey>) -> bool {
        self.source_run_id == run.run_id
            && self.source_binding.task_id == self.source_task_id
            && run.runtime_config.harness_id == HarnessId::Codex
            && binding == Some(&self.source_binding)
            && run.run_provider_binding.as_ref() == Some(&self.source_binding)
            && *run.provider_turn_readiness.borrow() == ProviderTurnReadiness::Ready
            && !run.interrupt_token.is_cancelled()
            && !*run.cancel.subscribe().borrow()
            && run.provider_turn.as_ref().is_some_and(|turn| {
                turn.run_id == self.source_run_id
                    && turn.binding == self.source_binding
                    && turn.provider_turn_id == self.provider_turn_id
                    && turn.claimed_call_ids.contains(&self.provider_call_id)
            })
    }
}

/// Read-only evidence for a possible later control proposal. No request ID,
/// user grant, command ID, or dispatcher is created by this stage.
#[derive(Debug)]
#[allow(dead_code)]
struct PreparedControlEvidence {
    claim: ClaimedProviderCall,
    source_project_id: String,
    source_environment_id: String,
    target_task_id: String,
    target_snapshot: luna_proto::TaskCommandSnapshot,
}

impl PreparedControlEvidence {
    fn read_only_authority(&self) -> crate::cross_task_tools::CrossTaskAuthority {
        crate::cross_task_tools::CrossTaskAuthority {
            caller_task_id: self.claim.source_task_id.clone(),
            local_environment_id: self.source_environment_id.clone(),
            visible_project_ids: HashSet::from([self.source_project_id.clone()]),
            allow_projectless: false,
            granted_scopes: HashSet::from(["threads.read".into()]),
            explicit_user_request: false,
            delivery: luna_proto::RuntimeDelivery::CodexDynamicTools,
        }
    }
}

/// One exact, read-only proposal. Keeping the full snapshot and source claim
/// private prevents a serialized question from becoming control authority.
#[derive(Debug)]
#[allow(dead_code)]
struct LunaProspectiveInterruptProposal {
    request_id: String,
    action: luna_proto::CrossTaskControlAction,
    evidence: PreparedControlEvidence,
    expires_at_ms: i64,
}

impl LunaProspectiveInterruptProposal {
    fn question(&self) -> UserInputQuestion {
        let evidence = &self.evidence;
        UserInputQuestion {
            id: format!("question-{}", self.request_id),
            header: "LUNA approval".into(),
            question: format!(
                "Prospective Interrupt of the active turn in task {} in project {} at revision {}, sequence {}, binding generation {:?}. This approval preview cannot execute the interruption yet.",
                evidence.target_task_id,
                evidence.source_project_id,
                evidence.target_snapshot.task_revision,
                evidence.target_snapshot.task_sequence,
                evidence.target_snapshot.binding_generation,
            ),
            options: vec!["Deny".into(), "Allow once".into()],
            multi_select: false,
            mcp: None,
            approval: Some(luna_proto::ApprovalRequest {
                provider_request_id: evidence.claim.provider_call_id.clone(),
                provider: HarnessId::Codex,
                turn_id: Some(evidence.claim.provider_turn_id.clone()),
                machine_name: evidence.source_environment_id.clone(),
                action_kind: luna_proto::ApprovalActionKind::LunaCrossTask,
                summary: format!(
                    "Prospective Interrupt of the active turn in task {} (preview only)",
                    evidence.target_task_id
                ),
                requested_scope: luna_proto::PermissionScope::Workspace,
                expires_at_ms: self.expires_at_ms,
                supports_task_allowance: false,
                details: None,
            }),
            input: None,
        }
    }

    fn matches_pending(
        &self,
        source_task_id: &str,
        run_id: &str,
        request_id: &str,
        question: &UserInputQuestion,
    ) -> bool {
        let evidence = &self.evidence;
        self.action == luna_proto::CrossTaskControlAction::Interrupt
            && self.request_id == request_id
            && evidence.claim.source_task_id == source_task_id
            && evidence.claim.source_run_id == run_id
            && evidence.claim.source_binding.task_id == source_task_id
            && !evidence.source_project_id.is_empty()
            && !evidence.source_environment_id.is_empty()
            && evidence.target_task_id != source_task_id
            && evidence.target_snapshot.task_id == evidence.target_task_id
            && evidence
                .target_snapshot
                .active_turn_id
                .as_deref()
                .is_some_and(|turn| !turn.is_empty())
            && self.expires_at_ms > now_ms()
            && question == &self.question()
    }

    fn matches_local_ui_allow_once(
        &self,
        run: &RunHandle,
        binding: Option<&ProviderBindingKey>,
        source_task_id: &str,
        request_id: &str,
        question: &UserInputQuestion,
    ) -> bool {
        let Some(approval) = question.approval.as_ref() else {
            return false;
        };
        self.matches_pending(source_task_id, &run.run_id, request_id, question)
            && approval.is_luna_cross_task()
            && !approval.supports_task_allowance
            && approval.provider_request_id == self.evidence.claim.provider_call_id
            && approval.turn_id.as_deref() == Some(self.evidence.claim.provider_turn_id.as_str())
            && self.expires_at_ms <= now_ms().saturating_add(30_000)
            && self.evidence.claim.matches_live(run, binding)
    }
}

/// A checkpoint is evidence for one active provider turn, never authorization.
/// Claiming a call ID only reserves it for a future engine-owned proposal; it
/// does not mint a grant or dispatch a tool.
#[derive(Debug)]
struct ProviderTurnFence {
    run_id: String,
    binding: ProviderBindingKey,
    provider_turn_id: String,
    claimed_call_ids: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderTurnReadiness {
    AwaitingBinding,
    AwaitingCheckpoint,
    Ready,
    Unavailable,
    Ended,
}

impl RunHandle {
    fn set_provider_turn_readiness(&self, next: ProviderTurnReadiness) {
        if *self.provider_turn_readiness.borrow() != next {
            self.provider_turn_readiness.send_replace(next);
        }
    }

    fn observe_codex_binding(
        &mut self,
        task_id: &str,
        run_id: &str,
        provider_thread_id: &str,
        binding: Option<&ProviderBindingKey>,
    ) -> bool {
        if self.run_id != run_id {
            return false;
        }
        let ready = binding.is_some_and(|binding| {
            self.runtime_config.harness_id == HarnessId::Codex
                && !self.interrupt_token.is_cancelled()
                && !*self.cancel.subscribe().borrow()
                && binding.task_id == task_id
                && !binding.binding_id.is_empty()
                && !provider_thread_id.is_empty()
                && binding.provider_thread_id == provider_thread_id
        });
        self.provider_turn = None;
        self.run_provider_binding = if ready { binding.cloned() } else { None };
        self.set_provider_turn_readiness(if ready {
            ProviderTurnReadiness::AwaitingCheckpoint
        } else {
            ProviderTurnReadiness::Unavailable
        });
        ready
    }

    fn rearm_codex_checkpoint(
        &mut self,
        task_id: &str,
        run_id: &str,
        provider_thread_id: Option<&str>,
        binding: Option<&ProviderBindingKey>,
    ) -> bool {
        if self.run_id != run_id {
            return false;
        }
        let ready = self.runtime_config.harness_id == HarnessId::Codex
            && !self.interrupt_token.is_cancelled()
            && !*self.cancel.subscribe().borrow()
            && binding.is_some_and(|binding| {
                self.run_provider_binding.as_ref() == Some(binding)
                    && binding.task_id == task_id
                    && provider_thread_id == Some(binding.provider_thread_id.as_str())
            });
        self.provider_turn = None;
        self.set_provider_turn_readiness(if ready {
            ProviderTurnReadiness::AwaitingCheckpoint
        } else {
            ProviderTurnReadiness::Unavailable
        });
        ready
    }

    fn observe_codex_checkpoint(
        &mut self,
        task_id: &str,
        run_id: &str,
        provider_thread_id: Option<&str>,
        provider_turn_id: &str,
        binding: Option<&ProviderBindingKey>,
    ) -> bool {
        if self.run_id != run_id {
            return false;
        }
        let Some(binding) = binding.filter(|binding| {
            matches!(
                *self.provider_turn_readiness.borrow(),
                ProviderTurnReadiness::AwaitingCheckpoint | ProviderTurnReadiness::Ready
            ) && self.runtime_config.harness_id == HarnessId::Codex
                && !self.interrupt_token.is_cancelled()
                && !*self.cancel.subscribe().borrow()
                && self.run_provider_binding.as_ref() == Some(binding)
                && binding.task_id == task_id
                && !binding.binding_id.is_empty()
                && !binding.provider_thread_id.is_empty()
                && provider_thread_id == Some(binding.provider_thread_id.as_str())
                && !provider_turn_id.is_empty()
        }) else {
            self.provider_turn = None;
            self.set_provider_turn_readiness(ProviderTurnReadiness::Unavailable);
            return false;
        };
        if self.provider_turn.as_ref().is_some_and(|turn| {
            turn.run_id == run_id
                && turn.binding == *binding
                && turn.provider_turn_id == provider_turn_id
        }) {
            // A duplicate checkpoint must not reopen an already claimed call.
            return true;
        }
        self.provider_turn = Some(ProviderTurnFence {
            run_id: run_id.to_owned(),
            binding: binding.clone(),
            provider_turn_id: provider_turn_id.to_owned(),
            claimed_call_ids: HashSet::new(),
        });
        self.set_provider_turn_readiness(ProviderTurnReadiness::Ready);
        true
    }

    fn claim_codex_invocation(
        &mut self,
        task_id: &str,
        run_id: &str,
        binding: Option<&ProviderBindingKey>,
        provider_thread_id: &str,
        provider_turn_id: &str,
        provider_call_id: &str,
    ) -> bool {
        const MAX_CALL_ID_BYTES: usize = 256;
        const MAX_CLAIMS_PER_TURN: usize = 4_096;
        if self.run_id != run_id
            || self.runtime_config.harness_id != HarnessId::Codex
            || self.interrupt_token.is_cancelled()
            || *self.cancel.subscribe().borrow()
            || *self.provider_turn_readiness.borrow() != ProviderTurnReadiness::Ready
            || provider_call_id.is_empty()
            || provider_call_id.len() > MAX_CALL_ID_BYTES
        {
            return false;
        }
        let Some(binding) = binding else {
            return false;
        };
        let Some(turn) = self.provider_turn.as_mut() else {
            return false;
        };
        turn.run_id == run_id
            && turn.binding.task_id == task_id
            && self.run_provider_binding.as_ref() == Some(binding)
            && turn.binding == *binding
            && turn.binding.provider_thread_id == provider_thread_id
            && turn.provider_turn_id == provider_turn_id
            && turn.claimed_call_ids.len() < MAX_CLAIMS_PER_TURN
            && turn.claimed_call_ids.insert(provider_call_id.to_owned())
    }
}

/// One accepted-but-unconfirmed steer. `delivery` decides whether run exit may
/// re-dispatch it (legacy warm route) or must preserve it as unknown.
/// Codex correlation IDs never authorize automatic replay.
#[derive(Debug, Clone)]
struct RoutedSteer {
    prompt: String,
    attachments: Vec<String>,
    input_items: Vec<luna_proto::ComposerInput>,
    message_id: String,
    delivery: luna_harness::SteerDelivery,
}

struct Inner {
    device_id: String,
    journal: Arc<RunJournal>,
    recovery: Arc<RecoveryStore>,
    registry: Arc<HarnessRegistry>,
    /// Set-once (first wins), cleared on runtime retirement: sessions and
    /// doc-host reference each other through Arcs, so this back-edge must be
    /// severable for a replaced engine graph to drop.
    doc_host: Mutex<Option<DocHost>>,
    /// chat_id → live run.
    runs: Mutex<HashMap<String, RunHandle>>,
    /// Dispatches between run-ticket admission and run registration. Shutdown
    /// waits for them so a run registered after its first sweep (for example
    /// a queue drain already in flight) is still interrupted instead of
    /// outliving the engine with its device-write ticket and InstanceLock.
    dispatches_in_flight: std::sync::atomic::AtomicUsize,
    /// Serializes runtime replacement/start with permission-mode mutation for
    /// one chat. Without this, an async idle retirement can cancel a fresh run
    /// that started after the mutation returned.
    transition_gates: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// chat_id → broadcast hub (retained across runs so subscribers survive turns).
    hubs: Mutex<HashMap<String, broadcast::Sender<JournaledEvent>>>,
    statuses: Mutex<HashMap<String, Session>>,
    sessions_tx: watch::Sender<Vec<Session>>,
    /// One process-local wakeup per durable active chain. The ledger remains
    /// authoritative across restart; these handles are only timers.
    recovery_tasks: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    recovery_settings_path: Mutex<Option<std::path::PathBuf>>,
    /// Last dispatched request per chat — the steer→new-turn fallback re-derives its
    /// run config from this (chat config rows land with the workspace doc in M4).
    last_requests: Mutex<HashMap<String, RunRequest>>,
    /// CC-06 live provider accounting (Usage page; never journaled).
    live_usage: crate::live_usage::LiveUsageLedger,
    /// Harness-native session ids per chat (resume continuity across turns) —
    /// the live-process cache over the durable copy on the workspace chat row
    /// (luna kept the same pair on `chats.harness_session_id`). An empty
    /// session id is the "do not resume" tombstone after a rejected resume.
    harness_sessions: Mutex<HashMap<String, HarnessSessionRef>>,
    /// Auto-titler for untitled chats (wired at engine assembly; absent in bare tests).
    titles: OnceLock<crate::titles::TitleGenerator>,
    /// Stable task/provider bindings (wired at engine assembly; absent in bare tests).
    task_identity: OnceLock<crate::task_identity::TaskIdentityService>,
    /// A weak edge to the RPC-owned L07/L09 command path. A provider run must
    /// not keep the RPC service (which owns this engine) alive indefinitely.
    cross_task_backends: Mutex<Vec<Weak<dyn crate::cross_task_tools::CrossTaskBackend>>>,
    /// Local provider-auth selection and lifecycle state. L06 keeps using its
    /// canonical session-home identity; this service only resolves execution auth.
    agent_accounts: OnceLock<crate::agent_accounts::AgentAccounts>,
    integration_settings: OnceLock<crate::integration_settings::IntegrationSettings>,
    /// Serializes provider-account changes with runtime routing/startup.
    account_transition: Arc<AsyncMutex<()>>,
    generated_images: OnceLock<(crate::uploads::Uploads, std::path::PathBuf)>,
    /// Fired with `(chat_id, cwd)` when a user prompt starts a turn (fresh
    /// dispatch or accepted steer) — the diff sync snapshots the checkout tree
    /// for the Changes pane's "Latest turn" scope. Absent in bare tests.
    turn_listener: OnceLock<TurnListener>,
    git_checkpoints: OnceLock<crate::git_checkpoints::GitCheckpoints>,
    side_chat_lookup: OnceLock<SideChatLookup>,
}

/// Turn-start hook: called with `(chat_id, cwd)`.
pub type TurnListener = Arc<dyn Fn(&str, &str) + Send + Sync>;
type SideChatLookup = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// CC-06 live accounting and native live model/effort switch hooks.
#[path = "sessions_cc06.rs"]
mod cc06;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Counts one `dispatch_inner` call until it returns or is dropped.
struct DispatchInFlight<'a>(&'a Inner);

impl<'a> DispatchInFlight<'a> {
    fn enter(inner: &'a Inner) -> Self {
        inner.dispatches_in_flight.fetch_add(1, Ordering::AcqRel);
        Self(inner)
    }
}

impl Drop for DispatchInFlight<'_> {
    fn drop(&mut self) {
        self.0.dispatches_in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
pub struct SessionsEngine {
    inner: Arc<Inner>,
}

impl SessionsEngine {
    pub fn new(
        device_id: String,
        journal: Arc<RunJournal>,
        registry: Arc<HarnessRegistry>,
        store: Arc<DocsStore>,
    ) -> Self {
        let (sessions_tx, _) = watch::channel(Vec::new());
        Self {
            inner: Arc::new(Inner {
                device_id,
                journal,
                recovery: Arc::new(RecoveryStore::new(store)),
                registry,
                doc_host: Mutex::new(None),
                runs: Mutex::new(HashMap::new()),
                transition_gates: Mutex::new(HashMap::new()),
                hubs: Mutex::new(HashMap::new()),
                statuses: Mutex::new(HashMap::new()),
                sessions_tx,
                recovery_tasks: Mutex::new(HashMap::new()),
                dispatches_in_flight: std::sync::atomic::AtomicUsize::new(0),
                recovery_settings_path: Mutex::new(None),
                last_requests: Mutex::new(HashMap::new()),
                live_usage: Default::default(),
                harness_sessions: Mutex::new(HashMap::new()),
                titles: OnceLock::new(),
                task_identity: OnceLock::new(),
                cross_task_backends: Mutex::new(Vec::new()),
                agent_accounts: OnceLock::new(),
                integration_settings: OnceLock::new(),
                account_transition: Arc::new(AsyncMutex::new(())),
                generated_images: OnceLock::new(),
                turn_listener: OnceLock::new(),
                git_checkpoints: OnceLock::new(),
                side_chat_lookup: OnceLock::new(),
            }),
        }
    }

    pub fn set_recovery_settings_path(&self, path: std::path::PathBuf) {
        *lock(&self.inner.recovery_settings_path) = Some(path);
    }

    /// Wire the doc host (called once at engine assembly; the two services are mutually
    /// referential by design — sessions stream into docs, docs execute commands here).
    pub fn set_doc_host(&self, host: DocHost) {
        // First set wins (the OnceLock contract this slot replaced).
        let mut slot = lock(&self.inner.doc_host);
        if slot.is_none() {
            *slot = Some(host);
        }
    }

    /// Sever the doc-host back-edge (runtime retirement; the doc host's
    /// `shutdown_workers` clears its own sessions edge). Every access site
    /// already treats a missing doc host as "not wired".
    pub fn clear_doc_host(&self) {
        lock(&self.inner.doc_host).take();
    }

    /// Read a completed tool's full output only when its detail is opened.
    /// The journal is host-local; it never adds full output to the synced doc.
    pub fn tool_output(&self, chat_id: &str, part_id: &str) -> Result<Option<String>, EngineError> {
        if chat_id.is_empty()
            || chat_id.len() > 128
            || !chat_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            || part_id.is_empty()
            || part_id.len() > 200
            || !part_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:#~-".contains(&byte))
        {
            return Err(EngineError::Other("invalid tool output ref".into()));
        }
        let (part_id, diff) = part_id
            .strip_suffix(".diff")
            .map_or((part_id, false), |id| (id, true));
        let output = if diff {
            self.inner.journal.tool_diff(chat_id, part_id)?
        } else {
            self.inner.journal.tool_output(chat_id, part_id)?
        };
        if output.is_none()
            && let Some((parent, _)) = chat_id.split_once("--sub--")
        {
            return Ok(self
                .inner
                .journal
                .subagent_tool_blob(parent, chat_id, part_id, diff)?);
        }
        Ok(output)
    }

    /// Bind generated-image intake to the same profile store used by attachment RPCs.
    pub fn set_generated_images(
        &self,
        uploads: crate::uploads::Uploads,
        allowed_root: std::path::PathBuf,
    ) {
        let _ = self.inner.generated_images.set((uploads, allowed_root));
    }

    /// Wire the chat auto-titler (called once at engine assembly). After each
    /// completed exchange the run task fires it for still-untitled chats.
    pub fn set_titles(&self, titles: crate::titles::TitleGenerator) {
        let _ = self.inner.titles.set(titles);
    }

    pub fn set_task_identity(&self, task_identity: crate::task_identity::TaskIdentityService) {
        let _ = self.inner.task_identity.set(task_identity);
    }

    pub fn set_cross_task_backend(
        &self,
        backend: Weak<dyn crate::cross_task_tools::CrossTaskBackend>,
    ) {
        let mut backends = lock(&self.inner.cross_task_backends);
        backends.retain(|existing| existing.strong_count() > 0);
        backends.push(backend);
    }

    fn cross_task_backend(&self) -> Option<Arc<dyn crate::cross_task_tools::CrossTaskBackend>> {
        lock(&self.inner.cross_task_backends)
            .iter()
            .rev()
            .find_map(Weak::upgrade)
    }

    pub(crate) fn cross_task_catalog(
        &self,
        context: &luna_proto::RuntimeContext,
        delivery: luna_proto::RuntimeDelivery,
    ) -> (
        Vec<luna_proto::RuntimeToolDefinition>,
        Vec<luna_proto::OmittedRuntimeTool>,
    ) {
        let Some(backend) = self.cross_task_backend() else {
            return (Vec::new(), Vec::new());
        };
        let authority =
            crate::cross_task_tools::CrossTaskAuthority::from_context(context, delivery, false);
        crate::cross_task_tools::CrossTaskTools::new(backend).catalog(&authority)
    }

    pub fn set_agent_accounts(&self, accounts: crate::agent_accounts::AgentAccounts) {
        let _ = self.inner.agent_accounts.set(accounts);
    }

    pub fn set_integration_settings(
        &self,
        settings: crate::integration_settings::IntegrationSettings,
    ) {
        let _ = self.inner.integration_settings.set(settings);
    }

    pub fn integration_settings(
        &self,
    ) -> Option<&crate::integration_settings::IntegrationSettings> {
        self.inner.integration_settings.get()
    }

    /// Wire the turn-start listener (called once at engine assembly).
    pub fn set_turn_listener(&self, listener: TurnListener) {
        let _ = self.inner.turn_listener.set(listener);
    }

    pub fn set_git_checkpoints(&self, checkpoints: crate::git_checkpoints::GitCheckpoints) {
        let _ = self.inner.git_checkpoints.set(checkpoints);
    }

    pub(crate) fn git_checkpoints(&self) -> Option<&crate::git_checkpoints::GitCheckpoints> {
        self.inner.git_checkpoints.get()
    }

    fn note_turn_start(&self, chat_id: &str, cwd: &str) {
        if let Some(listener) = self.inner.turn_listener.get() {
            listener(chat_id, cwd);
        }
    }

    fn doc_handle(&self, chat_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        let host = self
            .inner
            .doc_host()
            .ok_or_else(|| EngineError::Other("doc host not wired into sessions engine".into()))?;
        host.open(chat_id)
    }

    /// Status watch: the full session list, re-sent on every transition.
    pub fn watch_sessions(&self) -> watch::Receiver<Vec<Session>> {
        self.inner.sessions_tx.subscribe()
    }

    pub fn session_status(&self, chat_id: &str) -> Option<Session> {
        lock(&self.inner.statuses).get(chat_id).cloned()
    }

    pub fn has_live_run(&self, chat_id: &str) -> bool {
        lock(&self.inner.runs).contains_key(chat_id)
    }

    /// The execution-lane fence for an active turn. A steerable provider keeps
    /// its run handle warm after Done, but that parked handle is not an active
    /// turn and must not block history forks or other idle-only commands.
    pub fn active_turn_id(&self, chat_id: &str) -> Option<String> {
        let (run_id, pending_steer, provider_turn) = {
            let runs = lock(&self.inner.runs);
            let run = runs.get(chat_id)?;
            (
                run.run_id.clone(),
                !lock(&run.routed_steers).is_empty(),
                run.provider_turn.is_some(),
            )
        };
        let active_status = self.session_status(chat_id).is_some_and(|session| {
            matches!(
                session.status,
                SessionStatus::Working
                    | SessionStatus::AwaitingApproval
                    | SessionStatus::AwaitingInput
            )
        });
        (pending_steer || provider_turn || active_status).then_some(run_id)
    }

    /// Await this run's engine-observed binding and checkpoint, then reserve
    /// one adapter-validated call for a future LUNA approval proposal. This
    /// context is evidence only, never a user grant or a tool dispatch.
    #[allow(dead_code)]
    async fn wait_and_claim_luna_provider_invocation(
        &self,
        chat_id: &str,
        run_id: &str,
        context: &luna_harness::LunaToolInvocationContext,
        deadline: tokio::time::Instant,
    ) -> Option<ClaimedProviderCall> {
        self.wait_and_claim_luna_provider_call_evidence(
            chat_id,
            run_id,
            context.provider_thread_id(),
            context.provider_turn_id(),
            context.provider_call_id(),
            deadline,
        )
        .await
    }

    /// Raw IDs stay private to this engine seam for hermetic tests. Production
    /// callers must use the typed adapter context above, not model arguments.
    async fn wait_and_claim_luna_provider_call_evidence(
        &self,
        chat_id: &str,
        run_id: &str,
        provider_thread_id: &str,
        provider_turn_id: &str,
        provider_call_id: &str,
        caller_deadline: tokio::time::Instant,
    ) -> Option<ClaimedProviderCall> {
        const MAX_WAIT: std::time::Duration = std::time::Duration::from_millis(500);
        let deadline = caller_deadline.min(tokio::time::Instant::now() + MAX_WAIT);
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        let (mut readiness, mut cancel, interrupt) = {
            let runs = lock(&self.inner.runs);
            let run = runs.get(chat_id).filter(|run| run.run_id == run_id)?;
            (
                run.provider_turn_readiness.subscribe(),
                run.cancel.subscribe(),
                run.interrupt_token.clone(),
            )
        };
        loop {
            if tokio::time::Instant::now() >= deadline
                || interrupt.is_cancelled()
                || *cancel.borrow()
            {
                return None;
            }
            let state = *readiness.borrow_and_update();
            match state {
                ProviderTurnReadiness::Ready => {
                    return self.claim_luna_provider_call_evidence(
                        chat_id,
                        run_id,
                        provider_thread_id,
                        provider_turn_id,
                        provider_call_id,
                    );
                }
                ProviderTurnReadiness::Unavailable | ProviderTurnReadiness::Ended => return None,
                ProviderTurnReadiness::AwaitingBinding
                | ProviderTurnReadiness::AwaitingCheckpoint => {}
            }
            tokio::select! {
                biased;
                _ = interrupt.cancelled() => return None,
                changed = cancel.changed() => if changed.is_err() { return None; },
                changed = readiness.changed() => if changed.is_err() { return None; },
                _ = tokio::time::sleep_until(deadline) => return None,
            }
        }
    }

    fn claim_luna_provider_call_evidence(
        &self,
        chat_id: &str,
        run_id: &str,
        provider_thread_id: &str,
        provider_turn_id: &str,
        provider_call_id: &str,
    ) -> Option<ClaimedProviderCall> {
        let binding = self.inner.current_codex_binding_key(chat_id)?;
        let claimed = lock(&self.inner.runs).get_mut(chat_id).is_some_and(|run| {
            run.claim_codex_invocation(
                chat_id,
                run_id,
                Some(&binding),
                provider_thread_id,
                provider_turn_id,
                provider_call_id,
            )
        });
        // A binding may advance between the first lookup and the run lock.
        // Consuming that call ID and failing closed is safer than accepting it.
        if !claimed || self.inner.current_codex_binding_key(chat_id).as_ref() != Some(&binding) {
            return None;
        }
        Some(ClaimedProviderCall {
            source_task_id: chat_id.to_owned(),
            source_run_id: run_id.to_owned(),
            source_binding: binding,
            provider_turn_id: provider_turn_id.to_owned(),
            provider_call_id: provider_call_id.to_owned(),
        })
    }

    #[cfg(test)]
    async fn wait_and_claim_luna_provider_call(
        &self,
        chat_id: &str,
        run_id: &str,
        provider_thread_id: &str,
        provider_turn_id: &str,
        provider_call_id: &str,
        deadline: tokio::time::Instant,
    ) -> bool {
        self.wait_and_claim_luna_provider_call_evidence(
            chat_id,
            run_id,
            provider_thread_id,
            provider_turn_id,
            provider_call_id,
            deadline,
        )
        .await
        .is_some()
    }

    #[cfg(test)]
    fn claim_luna_provider_call(
        &self,
        chat_id: &str,
        run_id: &str,
        provider_thread_id: &str,
        provider_turn_id: &str,
        provider_call_id: &str,
    ) -> bool {
        self.claim_luna_provider_call_evidence(
            chat_id,
            run_id,
            provider_thread_id,
            provider_turn_id,
            provider_call_id,
        )
        .is_some()
    }

    fn claimed_call_cancel_state(
        &self,
        claim: &ClaimedProviderCall,
    ) -> Option<(CancellationToken, watch::Receiver<bool>)> {
        let binding = self.inner.current_codex_binding_key(&claim.source_task_id);
        let runs = lock(&self.inner.runs);
        let run = runs.get(&claim.source_task_id)?;
        claim
            .matches_live(run, binding.as_ref())
            .then(|| (run.interrupt_token.clone(), run.cancel.subscribe()))
    }

    fn claimed_call_is_live(&self, claim: &ClaimedProviderCall) -> bool {
        self.claimed_call_cancel_state(claim).is_some()
    }

    async fn invoke_approved_luna_send(
        &self,
        chat_id: &str,
        run_id: &str,
        contract: &luna_proto::LunaAgentRuntimeContract,
        invocation: luna_harness::LunaToolInvocation,
        cancel: &CancellationToken,
        backend: Option<Arc<dyn crate::cross_task_tools::CrossTaskBackend>>,
    ) -> Result<serde_json::Value, EngineError> {
        use crate::cross_task_tools::{
            ApprovedSendFence, CrossTaskAuthority, CrossTaskError, CrossTaskTools,
        };
        use luna_proto::{CrossTaskSendMode, CrossTaskSendRequest, RuntimeDelivery};
        let fail = |error: CrossTaskError| EngineError::Other(error.to_string());
        let Some(tool) = contract
            .tools
            .iter()
            .find(|tool| tool.name == invocation.name)
        else {
            return Err(fail(CrossTaskError::Unavailable));
        };
        if contract.context.thread_id != chat_id
            || contract.delivery != RuntimeDelivery::CodexDynamicTools
            || !tool
                .required_scopes
                .iter()
                .all(|scope| contract.context.granted_scopes.contains(scope))
        {
            return Err(fail(CrossTaskError::Scope));
        }
        let child_request = if invocation.name == "create_thread" {
            let request: luna_proto::CreateChildThreadRequest =
                serde_json::from_value(invocation.arguments.clone())
                    .map_err(|_| fail(CrossTaskError::Input))?;
            crate::cross_task_tools::validate_child_request(&request).map_err(fail)?;
            Some(request)
        } else {
            None
        };
        let request: CrossTaskSendRequest = if let Some(child) = &child_request {
            CrossTaskSendRequest {
                task_id: crate::cross_task_tools::child_task_id(chat_id, &child.idempotency_key),
                message: child.prompt.clone(),
                mode: CrossTaskSendMode::Queue,
                idempotency_key: child.idempotency_key.clone(),
            }
        } else {
            serde_json::from_value(invocation.arguments).map_err(|_| fail(CrossTaskError::Input))?
        };
        if request.mode != CrossTaskSendMode::Queue
            || request.message.trim().is_empty()
            || request.message.chars().count()
                > if child_request.is_some() {
                    crate::cross_task_tools::MAX_CHILD_PROMPT_CHARS
                } else {
                    crate::cross_task_tools::MAX_APPROVED_MESSAGE_CHARS
                }
            || request.idempotency_key.is_empty()
            || request.idempotency_key.len() > 128
            || !request
                .idempotency_key
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
        {
            return Err(fail(CrossTaskError::Input));
        }
        let backend = backend.ok_or_else(|| fail(CrossTaskError::Unavailable))?;
        let authority =
            CrossTaskAuthority::from_context(&contract.context, contract.delivery, false);
        let claim = self
            .wait_and_claim_luna_provider_invocation(
                chat_id,
                run_id,
                &invocation.context,
                tokio::time::Instant::now() + std::time::Duration::from_millis(500),
            )
            .await
            .ok_or_else(|| fail(CrossTaskError::Unavailable))?;
        let (claim, target_snapshot, project_id, environment_id) =
            if let Some(child) = &child_request {
                let snapshot = backend
                    .child_creation_snapshot(child, &authority)
                    .await
                    .map_err(fail)?;
                (
                    claim,
                    snapshot,
                    contract
                        .context
                        .project_id
                        .clone()
                        .ok_or_else(|| fail(CrossTaskError::TargetScope))?,
                    contract.context.environment_id.clone(),
                )
            } else {
                let evidence = self
                    .prepare_luna_control_evidence(claim, &authority, &request.task_id)
                    .await
                    .map_err(fail)?;
                (
                    evidence.claim,
                    evidence.target_snapshot,
                    evidence.source_project_id,
                    evidence.source_environment_id,
                )
            };
        let expires_at_ms = now_ms().saturating_add(30_000);
        let fence = ApprovedSendFence {
            source_task_id: claim.source_task_id.clone(),
            source_run_id: claim.source_run_id.clone(),
            source_binding_id: claim.source_binding.binding_id.clone(),
            source_binding_generation: claim.source_binding.generation,
            source_provider_thread_id: claim.source_binding.provider_thread_id.clone(),
            source_provider_turn_id: claim.provider_turn_id.clone(),
            source_provider_call_id: claim.provider_call_id.clone(),
            target_snapshot,
            request: request.clone(),
            expires_at_ms,
        };
        let proposal = LunaSendProposal {
            child_request: child_request.clone(),
            request_id: new_id(),
            claim,
            fence,
            project_id,
            environment_id,
        };
        let request_id = proposal.request_id.clone();
        let question = proposal.question();
        let (resolver, _answer_rx) = oneshot::channel();
        let (grant_tx, grant_rx) = oneshot::channel();
        let binding = self.inner.current_codex_binding_key(chat_id);
        let (pending, engine_tx) = {
            let runs = lock(&self.inner.runs);
            let run = runs
                .get(chat_id)
                .filter(|run| proposal.claim.matches_live(run, binding.as_ref()))
                .ok_or_else(|| fail(CrossTaskError::Unavailable))?;
            let pending = run.pending_inputs.clone();
            lock(&pending).insert(
                request_id.clone(),
                PendingInput {
                    kind: PendingInputKind::LunaSend {
                        run_id: run_id.to_owned(),
                        proposal: Box::new(proposal),
                        grant_tx,
                    },
                    questions: vec![question.clone()],
                    resolver,
                },
            );
            (pending, run.engine_tx.clone())
        };
        if engine_tx
            .send(AgentEvent::InputRequested {
                request_id: request_id.clone(),
                questions: vec![question],
            })
            .is_err()
        {
            expire_pending_approval(&pending, &engine_tx, &request_id);
            return Err(fail(CrossTaskError::Unavailable));
        }
        let grant = tokio::select! {
            _ = cancel.cancelled() => Err(CrossTaskError::Cancelled),
            result = tokio::time::timeout(std::time::Duration::from_secs(30), grant_rx) => {
                match result {
                    Ok(Ok(fence)) => Ok(fence),
                    Ok(Err(_)) => Err(CrossTaskError::UserRequest),
                    Err(_) => Err(CrossTaskError::Deadline),
                }
            }
        };
        expire_pending_approval(&pending, &engine_tx, &request_id);
        let fence = grant.map_err(fail)?;
        self.with_live_approved_send(&fence, || Ok(()))?;
        if let Some(child) = child_request {
            let child_fence = crate::cross_task_tools::ApprovedChildFence {
                source: fence,
                request: child.clone(),
            };
            let receipt = backend
                .create_child(child, &authority, &child_fence)
                .await
                .map_err(fail)?;
            return serde_json::to_value(receipt).map_err(Into::into);
        }
        let tools = CrossTaskTools::new(backend);
        tools
            .revalidate_control_target_snapshot(
                &authority,
                &request.task_id,
                &fence.target_snapshot,
                cancel,
            )
            .await
            .map_err(fail)?;
        let receipt = tools
            .send_approved(&authority, request, &fence, cancel)
            .await
            .map_err(fail)?;
        serde_json::to_value(receipt).map_err(Into::into)
    }

    /// Keep the source run fenced through the synchronous queue transaction.
    /// The caller must first acquire the target command/drain lanes; there is
    /// no await while this run lock is held.
    pub(crate) fn with_live_approved_send<T>(
        &self,
        fence: &crate::cross_task_tools::ApprovedSendFence,
        commit: impl FnOnce() -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
        if fence.expires_at_ms <= now_ms() {
            return Err(EngineError::Other("cross-task approval expired".into()));
        }
        let binding = self.inner.current_codex_binding_key(&fence.source_task_id);
        let target_active_turn = self.active_turn_id(&fence.request.task_id);
        let runs = lock(&self.inner.runs);
        let valid = runs.get(&fence.source_task_id).is_some_and(|run| {
            run.run_id == fence.source_run_id
                && run.runtime_config.harness_id == HarnessId::Codex
                && !run.interrupt_token.is_cancelled()
                && !*run.cancel.subscribe().borrow()
                && *run.provider_turn_readiness.borrow() == ProviderTurnReadiness::Ready
                && run.run_provider_binding.as_ref() == binding.as_ref()
                && binding.as_ref().is_some_and(|binding| {
                    binding.binding_id == fence.source_binding_id
                        && binding.generation == fence.source_binding_generation
                        && binding.provider_thread_id == fence.source_provider_thread_id
                })
                && run.provider_turn.as_ref().is_some_and(|turn| {
                    turn.run_id == fence.source_run_id
                        && turn.provider_turn_id == fence.source_provider_turn_id
                        && turn
                            .claimed_call_ids
                            .contains(&fence.source_provider_call_id)
                        && Some(&turn.binding) == binding.as_ref()
                })
        });
        if !valid {
            return Err(EngineError::Other(
                "source provider call is no longer live".into(),
            ));
        }
        let host = self
            .inner
            .doc_host()
            .ok_or_else(|| EngineError::Other("task command host is unavailable".into()))?;
        let mut target = host.task_command_snapshot(&fence.request.task_id)?.snapshot;
        if let Some(binding) = self
            .inner
            .task_identity
            .get()
            .and_then(|service| service.view(&fence.request.task_id))
            .and_then(|view| view.current_binding)
        {
            target.binding_id = Some(binding.binding_id);
            target.binding_generation = Some(binding.generation);
        }
        target.active_turn_id = target_active_turn;
        if target.task_revision != fence.target_snapshot.task_revision
            || target.binding_id != fence.target_snapshot.binding_id
            || target.binding_generation != fence.target_snapshot.binding_generation
            || target.active_turn_id != fence.target_snapshot.active_turn_id
        {
            return Err(EngineError::Other(
                "target task changed after approval".into(),
            ));
        }
        commit()
    }

    /// Read-only target evidence for an adapter-claimed provider call. The
    /// send path still needs a separate, exact local UI approval.
    async fn prepare_luna_control_evidence(
        &self,
        claim: ClaimedProviderCall,
        authority: &crate::cross_task_tools::CrossTaskAuthority,
        target_task_id: &str,
    ) -> Result<PreparedControlEvidence, crate::cross_task_tools::CrossTaskError> {
        use crate::cross_task_tools::{CrossTaskError, CrossTaskTools};
        if authority.caller_task_id != claim.source_task_id
            || authority.local_environment_id != self.inner.device_id
            || authority.delivery != luna_proto::RuntimeDelivery::CodexDynamicTools
            || authority.explicit_user_request
            || authority.allow_projectless
            || authority.granted_scopes.len() != 1
            || !authority.granted_scopes.contains("threads.read")
            || authority.visible_project_ids.len() != 1
            || target_task_id == claim.source_task_id
        {
            return Err(CrossTaskError::TargetScope);
        }
        let source_project_id = authority
            .visible_project_ids
            .iter()
            .next()
            .expect("checked one visible project")
            .clone();
        let (interrupt, mut cancel) = self
            .claimed_call_cancel_state(&claim)
            .ok_or(CrossTaskError::Unavailable)?;
        let backend = self
            .cross_task_backend()
            .ok_or(CrossTaskError::Unavailable)?;
        let tools = CrossTaskTools::new(backend);
        let target_snapshot = tokio::select! {
            biased;
            _ = interrupt.cancelled() => return Err(CrossTaskError::Cancelled),
            _ = cancel.wait_for(|value| *value) => return Err(CrossTaskError::Cancelled),
            result = tools.control_target_snapshot(authority, target_task_id, &interrupt) => result,
        }?;
        if !self.claimed_call_is_live(&claim) {
            return Err(CrossTaskError::Unavailable);
        }
        tokio::select! {
            biased;
            _ = interrupt.cancelled() => return Err(CrossTaskError::Cancelled),
            _ = cancel.wait_for(|value| *value) => return Err(CrossTaskError::Cancelled),
            result = tools.revalidate_control_target_snapshot(
                authority,
                target_task_id,
                &target_snapshot,
                &interrupt,
            ) => result,
        }?;
        if !self.claimed_call_is_live(&claim) {
            return Err(CrossTaskError::Unavailable);
        }
        Ok(PreparedControlEvidence {
            claim,
            source_project_id,
            source_environment_id: authority.local_environment_id.clone(),
            target_task_id: target_task_id.to_owned(),
            target_snapshot,
        })
    }

    fn luna_prospective_pending_is_live(
        &self,
        source_task_id: &str,
        request_id: &str,
        pending: &PendingInputs,
    ) -> bool {
        let binding = self.inner.current_codex_binding_key(source_task_id);
        let runs = lock(&self.inner.runs);
        let Some(run) = runs.get(source_task_id) else {
            return false;
        };
        if !Arc::ptr_eq(&run.pending_inputs, pending) {
            return false;
        }
        let entries = lock(pending);
        let Some(entry) = entries.get(request_id) else {
            return false;
        };
        let PendingInputKind::LunaProspectiveInterrupt {
            run_id, proposal, ..
        } = &entry.kind
        else {
            return false;
        };
        entry.questions.len() == 1
            && proposal.matches_pending(source_task_id, run_id, request_id, &entry.questions[0])
            && proposal.evidence.claim.matches_live(run, binding.as_ref())
    }

    /// Dormant registration proof. No provider callback or catalog calls it;
    /// the private grant receiver has no production consumer in this stage.
    #[allow(dead_code)]
    async fn register_luna_prospective_interrupt(
        &self,
        evidence: PreparedControlEvidence,
    ) -> Result<
        (
            String,
            oneshot::Receiver<Vec<UserInputAnswer>>,
            oneshot::Receiver<LunaProspectiveInterruptGrant>,
        ),
        crate::cross_task_tools::CrossTaskError,
    > {
        use crate::cross_task_tools::{CrossTaskError, CrossTaskTools};
        if evidence.source_project_id.is_empty()
            || evidence.source_environment_id != self.inner.device_id
            || evidence.target_task_id == evidence.claim.source_task_id
            || evidence.target_snapshot.task_id != evidence.target_task_id
            || evidence
                .target_snapshot
                .active_turn_id
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(CrossTaskError::TargetScope);
        }
        let authority = evidence.read_only_authority();
        let (interrupt, mut cancel) = self
            .claimed_call_cancel_state(&evidence.claim)
            .ok_or(CrossTaskError::Unavailable)?;
        let backend = self
            .cross_task_backend()
            .ok_or(CrossTaskError::Unavailable)?;
        let tools = CrossTaskTools::new(backend);
        tokio::select! {
            biased;
            _ = interrupt.cancelled() => return Err(CrossTaskError::Cancelled),
            _ = cancel.wait_for(|value| *value) => return Err(CrossTaskError::Cancelled),
            result = tools.revalidate_control_target_snapshot(
                &authority,
                &evidence.target_task_id,
                &evidence.target_snapshot,
                &interrupt,
            ) => result,
        }?;
        if !self.claimed_call_is_live(&evidence.claim) {
            return Err(CrossTaskError::Unavailable);
        }

        let source_task_id = evidence.claim.source_task_id.clone();
        let source_run_id = evidence.claim.source_run_id.clone();
        let source_binding = evidence.claim.source_binding.clone();
        let request_id = new_id();
        let proposal = LunaProspectiveInterruptProposal {
            request_id: request_id.clone(),
            action: luna_proto::CrossTaskControlAction::Interrupt,
            evidence,
            expires_at_ms: now_ms().saturating_add(30_000),
        };
        let expires_at_ms = proposal.expires_at_ms;
        let question = proposal.question();
        let (resolver, answer) = oneshot::channel();
        let (grant_tx, grant) = oneshot::channel();
        let binding = self.inner.current_codex_binding_key(&source_task_id);
        let (pending, engine_tx, readiness, interrupt, cancel) = {
            let runs = lock(&self.inner.runs);
            let run = runs
                .get(&source_task_id)
                .filter(|run| proposal.evidence.claim.matches_live(run, binding.as_ref()))
                .ok_or(CrossTaskError::Unavailable)?;
            let pending = run.pending_inputs.clone();
            let mut entries = lock(&pending);
            if entries.contains_key(&request_id) {
                return Err(CrossTaskError::Unavailable);
            }
            entries.insert(
                request_id.clone(),
                PendingInput {
                    kind: PendingInputKind::LunaProspectiveInterrupt {
                        run_id: source_run_id,
                        proposal: Box::new(proposal),
                        grant_tx,
                    },
                    questions: vec![question.clone()],
                    resolver,
                },
            );
            (
                pending.clone(),
                run.engine_tx.clone(),
                run.provider_turn_readiness.subscribe(),
                run.interrupt_token.clone(),
                run.cancel.subscribe(),
            )
        };
        if self.inner.current_codex_binding_key(&source_task_id) != Some(source_binding.clone())
            || !self.luna_prospective_pending_is_live(&source_task_id, &request_id, &pending)
        {
            if let Some(entry) = lock(&pending).remove(&request_id) {
                let _ = entry.resolver.send(Vec::new());
            }
            return Err(CrossTaskError::Unavailable);
        }
        if engine_tx
            .send(AgentEvent::InputRequested {
                request_id: request_id.clone(),
                questions: vec![question],
            })
            .is_err()
        {
            if let Some(entry) = lock(&pending).remove(&request_id) {
                let _ = entry.resolver.send(Vec::new());
            }
            return Err(CrossTaskError::Unavailable);
        }
        if self.inner.current_codex_binding_key(&source_task_id) != Some(source_binding)
            || !self.luna_prospective_pending_is_live(&source_task_id, &request_id, &pending)
        {
            expire_pending_approval(&pending, &engine_tx, &request_id);
            return Err(CrossTaskError::Unavailable);
        }
        schedule_luna_prospective_cleanup(
            &pending,
            engine_tx,
            request_id.clone(),
            expires_at_ms,
            interrupt,
            cancel,
            readiness,
        );
        Ok((request_id, answer, grant))
    }

    fn deny_luna_prospective_interrupt_local_ui(
        &self,
        source_task_id: &str,
        request_id: &str,
        decision: luna_proto::ApprovalDecision,
    ) -> LunaApprovalOutcome {
        let runs = lock(&self.inner.runs);
        let Some(run) = runs.get(source_task_id) else {
            return LunaApprovalOutcome::Stale;
        };
        let mut entries = lock(&run.pending_inputs);
        let Some(entry) = entries.get(request_id) else {
            return LunaApprovalOutcome::Stale;
        };
        let PendingInputKind::LunaProspectiveInterrupt {
            run_id, proposal, ..
        } = &entry.kind
        else {
            return LunaApprovalOutcome::Stale;
        };
        if run_id != &run.run_id || entry.questions.len() != 1 {
            return LunaApprovalOutcome::Stale;
        }
        let expired = proposal.expires_at_ms <= now_ms();
        let pending = entries
            .remove(request_id)
            .expect("checked prospective input");
        drop(entries);
        let _ = run.engine_tx.send(AgentEvent::InputResolved {
            request_id: request_id.to_owned(),
        });
        let answer = if expired {
            Vec::new()
        } else {
            vec![UserInputAnswer {
                question_id: pending.questions[0].id.clone(),
                labels: vec![if decision == luna_proto::ApprovalDecision::Cancel {
                    "Cancel".into()
                } else {
                    "Deny".into()
                }],
            }]
        };
        if pending.resolver.send(answer).is_err() {
            return LunaApprovalOutcome::RunEnded;
        }
        if expired {
            LunaApprovalOutcome::Expired
        } else {
            LunaApprovalOutcome::Denied
        }
    }

    /// Private, dormant local-UI proof. Nothing in the provider, RPC, or UI
    /// calls this yet. It only moves one exact proposal into a private grant.
    #[allow(dead_code)]
    async fn respond_luna_prospective_interrupt_local_ui(
        &self,
        source_task_id: &str,
        request_id: &str,
        decision: luna_proto::ApprovalDecision,
    ) -> LunaApprovalOutcome {
        use crate::cross_task_tools::{CrossTaskError, CrossTaskTools};
        if decision != luna_proto::ApprovalDecision::AllowOnce {
            return self.deny_luna_prospective_interrupt_local_ui(
                source_task_id,
                request_id,
                decision,
            );
        }
        let binding = self.inner.current_codex_binding_key(source_task_id);
        let (pending, engine_tx, authority, target_task_id, snapshot, interrupt, mut cancel) = {
            let runs = lock(&self.inner.runs);
            let Some(run) = runs.get(source_task_id) else {
                return LunaApprovalOutcome::Stale;
            };
            let pending = run.pending_inputs.clone();
            let entries = lock(&pending);
            let Some(entry) = entries.get(request_id) else {
                return LunaApprovalOutcome::Stale;
            };
            let PendingInputKind::LunaProspectiveInterrupt {
                run_id,
                proposal,
                grant_tx,
            } = &entry.kind
            else {
                return LunaApprovalOutcome::Stale;
            };
            if run_id != &run.run_id || entry.questions.len() != 1 {
                return LunaApprovalOutcome::Stale;
            }
            if proposal.expires_at_ms <= now_ms() {
                return LunaApprovalOutcome::Expired;
            }
            if !proposal.matches_local_ui_allow_once(
                run,
                binding.as_ref(),
                source_task_id,
                request_id,
                &entry.questions[0],
            ) {
                return LunaApprovalOutcome::Stale;
            }
            if grant_tx.is_closed() || entry.resolver.is_closed() {
                return LunaApprovalOutcome::RunEnded;
            }
            (
                pending.clone(),
                run.engine_tx.clone(),
                proposal.evidence.read_only_authority(),
                proposal.evidence.target_task_id.clone(),
                proposal.evidence.target_snapshot.clone(),
                run.interrupt_token.clone(),
                run.cancel.subscribe(),
            )
        };
        let Some(backend) = self.cross_task_backend() else {
            expire_pending_approval(&pending, &engine_tx, request_id);
            return LunaApprovalOutcome::Stale;
        };
        let tools = CrossTaskTools::new(backend);
        let result = tokio::select! {
            biased;
            _ = interrupt.cancelled() => Err(CrossTaskError::Cancelled),
            _ = cancel.wait_for(|value| *value) => Err(CrossTaskError::Cancelled),
            result = tools.revalidate_control_target_snapshot(
                &authority,
                &target_task_id,
                &snapshot,
                &interrupt,
            ) => result,
        };
        if result.is_err()
            || !self.luna_prospective_pending_is_live(source_task_id, request_id, &pending)
        {
            expire_pending_approval(&pending, &engine_tx, request_id);
            return if matches!(result, Err(CrossTaskError::Cancelled)) {
                LunaApprovalOutcome::RunEnded
            } else {
                LunaApprovalOutcome::Stale
            };
        }

        let binding = self.inner.current_codex_binding_key(source_task_id);
        // Keep the run map locked through one-shot delivery so a replacement
        // cannot inherit this answer or grant after the final source check.
        let runs = lock(&self.inner.runs);
        let Some(run) = runs.get(source_task_id) else {
            return LunaApprovalOutcome::Stale;
        };
        if !Arc::ptr_eq(&run.pending_inputs, &pending) {
            return LunaApprovalOutcome::Stale;
        }
        let mut entries = lock(&pending);
        let Some(entry) = entries.get(request_id) else {
            return LunaApprovalOutcome::Stale;
        };
        let PendingInputKind::LunaProspectiveInterrupt {
            run_id,
            proposal,
            grant_tx,
        } = &entry.kind
        else {
            return LunaApprovalOutcome::Stale;
        };
        if run_id != &run.run_id
            || entry.questions.len() != 1
            || proposal.evidence.target_snapshot != snapshot
            || !proposal.matches_local_ui_allow_once(
                run,
                binding.as_ref(),
                source_task_id,
                request_id,
                &entry.questions[0],
            )
        {
            return LunaApprovalOutcome::Stale;
        }
        if grant_tx.is_closed() || entry.resolver.is_closed() {
            let pending = entries
                .remove(request_id)
                .expect("checked prospective input");
            drop(entries);
            let _ = pending.resolver.send(Vec::new());
            let _ = run.engine_tx.send(AgentEvent::InputResolved {
                request_id: request_id.to_owned(),
            });
            return LunaApprovalOutcome::RunEnded;
        }
        let pending = entries
            .remove(request_id)
            .expect("checked prospective input");
        drop(entries);
        let PendingInput {
            kind,
            questions,
            resolver,
        } = pending;
        let PendingInputKind::LunaProspectiveInterrupt {
            proposal, grant_tx, ..
        } = kind
        else {
            unreachable!("checked prospective kind")
        };
        let final_binding = self.inner.current_codex_binding_key(source_task_id);
        if !proposal
            .evidence
            .claim
            .matches_live(run, final_binding.as_ref())
        {
            let _ = resolver.send(Vec::new());
            let _ = run.engine_tx.send(AgentEvent::InputResolved {
                request_id: request_id.to_owned(),
            });
            return LunaApprovalOutcome::Stale;
        }
        if run
            .engine_tx
            .send(AgentEvent::InputResolved {
                request_id: request_id.to_owned(),
            })
            .is_err()
        {
            let _ = resolver.send(Vec::new());
            return LunaApprovalOutcome::RunEnded;
        }
        if resolver
            .send(vec![UserInputAnswer {
                question_id: questions[0].id.clone(),
                labels: vec!["Allow once".into()],
            }])
            .is_err()
        {
            return LunaApprovalOutcome::RunEnded;
        }
        // Two oneshot channels cannot commit atomically. A receiver closing
        // after the answer succeeds drops the grant; this is not product success.
        if grant_tx
            .send(LunaProspectiveInterruptGrant {
                proposal: *proposal,
            })
            .is_err()
        {
            return LunaApprovalOutcome::RunEnded;
        }
        LunaApprovalOutcome::AllowedOnce
    }

    /// Fake-only, private one-shot proof. No provider callback, UI, or catalog
    /// calls this stage. The grant is consumed by value and a failed/unknown
    /// submission is never retried with this or a replacement command ID.
    #[allow(dead_code)]
    async fn dispatch_luna_interrupt_grant_once(
        &self,
        grant: LunaProspectiveInterruptGrant,
    ) -> Result<LunaInterruptDispatchResult, crate::cross_task_tools::CrossTaskError> {
        use crate::cross_task_tools::{CrossTaskControlGrant, CrossTaskError, CrossTaskTools};
        use luna_proto::{CommandReceiptStatus, CrossTaskControlAction, CrossTaskControlRequest};

        let proposal = grant.proposal;
        let evidence = &proposal.evidence;
        if proposal.expires_at_ms <= now_ms()
            || proposal.expires_at_ms > now_ms().saturating_add(30_000)
        {
            return Err(CrossTaskError::UserRequest);
        }
        if proposal.action != CrossTaskControlAction::Interrupt
            || evidence.source_project_id.is_empty()
            || evidence.source_environment_id != self.inner.device_id
            || evidence.target_task_id == evidence.claim.source_task_id
            || evidence.target_snapshot.task_id != evidence.target_task_id
            || evidence
                .target_snapshot
                .active_turn_id
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(CrossTaskError::TargetScope);
        }
        let (interrupt, mut cancel) = self
            .claimed_call_cancel_state(&evidence.claim)
            .ok_or(CrossTaskError::Unavailable)?;
        let backend = self
            .cross_task_backend()
            .ok_or(CrossTaskError::Unavailable)?;
        let tools = CrossTaskTools::new(backend);
        let read_authority = evidence.read_only_authority();
        tokio::select! {
            biased;
            _ = interrupt.cancelled() => return Err(CrossTaskError::Cancelled),
            _ = cancel.wait_for(|value| *value) => return Err(CrossTaskError::Cancelled),
            result = tools.revalidate_control_target_snapshot(
                &read_authority,
                &evidence.target_task_id,
                &evidence.target_snapshot,
                &interrupt,
            ) => result,
        }?;
        if !self.claimed_call_is_live(&evidence.claim) {
            return Err(CrossTaskError::Unavailable);
        }

        let command_id = new_id();
        let request = CrossTaskControlRequest {
            action: CrossTaskControlAction::Interrupt,
            task_id: evidence.target_task_id.clone(),
            project_id: evidence.source_project_id.clone(),
            environment_id: evidence.source_environment_id.clone(),
            command_id: command_id.clone(),
            destination_project_id: None,
            expected_task_revision: Some(evidence.target_snapshot.task_revision),
            expected_binding_generation: evidence.target_snapshot.binding_generation,
            expected_turn_id: evidence.target_snapshot.active_turn_id.clone(),
        };
        let mut manage_authority = read_authority.clone();
        manage_authority.granted_scopes = HashSet::from(["threads.manage".into()]);
        manage_authority.explicit_user_request = true;
        let control_grant = CrossTaskControlGrant {
            caller_task_id: evidence.claim.source_task_id.clone(),
            request: request.clone(),
            expires_at_ms: proposal.expires_at_ms,
        };
        if !self.claimed_call_is_live(&evidence.claim) || proposal.expires_at_ms <= now_ms() {
            return Err(CrossTaskError::Unavailable);
        }
        // Once this await begins, an error or cancellation may mean the
        // backend committed. The only follow-up is same-ID readback.
        let submitted = tokio::select! {
            biased;
            _ = interrupt.cancelled() => Err(CrossTaskError::Cancelled),
            _ = cancel.wait_for(|value| *value) => Err(CrossTaskError::Cancelled),
            result = tools.control(
                &manage_authority,
                &control_grant,
                request,
                &interrupt,
            ) => result,
        };
        // Ignore source cancellation after a possible commit, but do not wait
        // indefinitely for receipt readback. An unknown result is never replayed.
        let readback = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tools.control_command_receipt(
                &read_authority,
                &evidence.target_task_id,
                &command_id,
                &CancellationToken::new(),
            ),
        )
        .await;
        let Some(receipt) = readback.ok().and_then(Result::ok).flatten() else {
            return Ok(LunaInterruptDispatchResult {
                command_id,
                status: LunaInterruptDispatchStatus::Unknown,
                receipt: None,
                settled: false,
            });
        };
        if receipt.intent != "stop"
            || receipt.snapshot != evidence.target_snapshot
            || submitted.as_ref().is_ok_and(|result| {
                result.command.payload_hash != receipt.payload_hash
                    || result.command.snapshot != receipt.snapshot
            })
        {
            return Ok(LunaInterruptDispatchResult {
                command_id,
                status: LunaInterruptDispatchStatus::Unknown,
                receipt: None,
                settled: false,
            });
        }
        let settled = submitted.as_ref().is_ok_and(|result| {
            result.command.command_id == command_id
                && result.command.payload_hash == receipt.payload_hash
                && result.command.status == receipt.status
                && result.settled
        });
        let status = match receipt.status {
            CommandReceiptStatus::Applied => LunaInterruptDispatchStatus::Applied,
            CommandReceiptStatus::Rejected
            | CommandReceiptStatus::Superseded
            | CommandReceiptStatus::Failed => LunaInterruptDispatchStatus::Rejected,
            CommandReceiptStatus::Conflict => LunaInterruptDispatchStatus::Conflict,
            CommandReceiptStatus::Pending => LunaInterruptDispatchStatus::Pending,
        };
        Ok(LunaInterruptDispatchResult {
            command_id,
            status,
            receipt: Some(receipt),
            settled,
        })
    }

    pub fn task_binding_generation(&self, chat_id: &str) -> Option<u32> {
        self.inner
            .task_identity
            .get()
            .and_then(|identity| identity.view(chat_id))
            .and_then(|view| view.current_binding)
            .map(|binding| binding.generation)
    }

    pub fn runtime_contract_diagnostics(
        &self,
        chat_id: &str,
        provider_session_id: &str,
    ) -> Result<Option<luna_proto::RuntimeContractDiagnostics>, EngineError> {
        Ok(self
            .inner
            .journal
            .runtime_contract(chat_id, provider_session_id)?)
    }

    pub(crate) fn runtime_context_for(
        &self,
        chat_id: &str,
        harness: HarnessId,
        request: &RunRequest,
    ) -> Result<luna_proto::RuntimeContext, EngineError> {
        let workspace = self.inner.workspace().ok_or_else(|| {
            EngineError::Other("LUNA runtime requires an authoritative workspace host".into())
        })?;
        let chat = workspace.chat(chat_id)?;
        let space = chat
            .as_ref()
            .and_then(|chat| chat.space_id.as_deref())
            .map(|id| workspace.space(id))
            .transpose()?
            .flatten();
        let identity = self
            .inner
            .task_identity
            .get()
            .and_then(|service| service.view(chat_id));
        let binding = identity
            .as_ref()
            .and_then(|view| view.current_binding.as_ref());
        let machine_id = workspace.device_id().to_string();
        let machine_name = workspace
            .read_devices()?
            .into_iter()
            .find(|device| device.id == machine_id)
            .map(|device| device.name)
            .unwrap_or_else(|| machine_id.clone());
        let local_managed_task = identity.as_ref().is_some_and(|view| {
            !view.task.tombstoned && view.workspace.environment_id == machine_id
        }) && chat
            .as_ref()
            .is_some_and(|chat| chat.device_id == machine_id);
        let read_granted = local_managed_task && self.cross_task_backend().is_some();
        Ok(luna_proto::RuntimeContext {
            app_version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: 1,
            thread_id: chat_id.into(),
            parent_thread_id: identity
                .as_ref()
                .and_then(|view| view.task.lineage.parent_task_id.clone()),
            source_thread_id: identity
                .as_ref()
                .and_then(|view| view.task.lineage.source_task_id.clone()),
            project_id: space.as_ref().map(|space| space.id.clone()),
            project_name: space.as_ref().map(|space| space.display_name().to_string()),
            environment_id: identity
                .as_ref()
                .map(|view| view.workspace.environment_id.clone())
                .unwrap_or_else(|| machine_id.clone()),
            machine_id,
            machine_name,
            workspace_root: space
                .as_ref()
                .map(|space| space.path.clone())
                .unwrap_or_else(|| request.cwd.clone()),
            worktree_id: binding.and_then(|binding| binding.worktree_id.clone()),
            binding_generation: binding.map(|binding| binding.generation),
            provider_profile_id: binding.and_then(|binding| binding.provider_profile_id.clone()),
            branch: chat.and_then(|chat| chat.branch),
            harness,
            model: request.model.clone(),
            reasoning: request.reasoning,
            execution_route: "local".into(),
            // Only same-project local reads are granted automatically. No
            // model-visible content or user prompt grants mutation scopes.
            granted_scopes: if read_granted {
                vec!["threads.read".into()]
            } else {
                Vec::new()
            },
        })
    }

    /// Whether this harness takes a prompt *during* a turn, rather than only at
    /// the boundary between turns. The queue drain's central question: a
    /// mid-turn agent gets the message now, everything else holds it until the
    /// turn ends. Catalog lookup only — never spawns.
    pub fn steers_mid_turn(&self, harness: HarnessId) -> bool {
        self.inner
            .registry
            .descriptors()
            .iter()
            .find(|d| d.id == harness)
            .is_some_and(HarnessDescriptor::steers_mid_turn)
    }

    /// Whether this chat has a turn in flight — streaming, or parked on a
    /// question it is still owed an answer to (see [`is_active`]). A persistent
    /// session parked BETWEEN turns is not: it holds a warm child with nothing
    /// outstanding, and takes the next prompt straight through its mailbox.
    pub fn turn_in_flight(&self, chat_id: &str) -> bool {
        lock(&self.inner.statuses)
            .get(chat_id)
            .is_some_and(is_active)
    }

    fn transition_gate(&self, chat_id: &str) -> Arc<AsyncMutex<()>> {
        lock(&self.inner.transition_gates)
            .entry(chat_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// CC-14: hold the dispatch transition gate with no runtime for this
    /// chat (a parked warm runtime is retired first, as permission changes
    /// do), so a one-shot provider control is the only native writer.
    /// `None` while a turn is running.
    pub(crate) async fn exclusive_idle_runtime(
        &self,
        chat_id: &str,
    ) -> Result<Option<OwnedMutexGuard<()>>, EngineError> {
        let transition = self.transition_gate(chat_id).lock_owned().await;
        if self.turn_in_flight(chat_id) {
            return Ok(None);
        }
        if self.has_live_run(chat_id) {
            self.interrupt(chat_id).await?;
            if self.has_live_run(chat_id) {
                return Err(EngineError::Other(
                    "the parked provider runtime did not retire before the deadline".into(),
                ));
            }
        }
        Ok(Some(transition))
    }

    pub(crate) fn prepare_workspace_repair_execution(
        &self,
        chat_id: &str,
    ) -> Result<OwnedMutexGuard<()>, EngineError> {
        let guard = self
            .transition_gate(chat_id)
            .try_lock_owned()
            .map_err(|_| {
                EngineError::Other("task dispatch or runtime mutation is in progress".into())
            })?;
        // First repair version blocks even idle warm handles. Reusing or
        // retiring a provider runtime is outside association-only correction.
        if self.has_live_run(chat_id) || self.active_turn_id(chat_id).is_some() {
            return Err(EngineError::Other(
                "task has an active or idle warm runtime".into(),
            ));
        }
        if self.inner.recovery.current(chat_id)?.is_some() {
            return Err(EngineError::Other(
                "task recovery must be resolved before workspace repair".into(),
            ));
        }
        Ok(guard)
    }

    pub(crate) async fn prepare_workspace_materialization(
        &self,
        task_id: &str,
        spec: &luna_proto::WorktreeSpec,
    ) -> Result<Option<crate::task_identity::WorkspaceMaterializationContext>, EngineError> {
        self.inner
            .task_identity
            .get()
            .ok_or_else(|| {
                EngineError::Other("task identity is unavailable for worktree preparation".into())
            })?
            .prepare_workspace_materialization(task_id, spec)
            .await
    }

    pub(crate) async fn commit_workspace_materialization(
        &self,
        context: crate::task_identity::WorkspaceMaterializationContext,
        command_id: &str,
        fence: &luna_sync::WorkspaceMaterializationFence,
        resolved_cwd: &str,
        branch: Option<&str>,
    ) -> Result<(), crate::workspace_host::TaskProjectionError> {
        self.inner
            .task_identity
            .get()
            .ok_or_else(|| {
                crate::workspace_host::TaskProjectionError::Mutation(EngineError::Other(
                    "task identity is unavailable for worktree commit".into(),
                ))
            })?
            .commit_workspace_materialization(context, command_id, fence, resolved_cwd, branch)
            .await
    }

    /// Release only this task's idle native writer, keeping dispatch excluded
    /// through the caller's metadata operation and durable registry commit.
    pub(crate) async fn prepare_native_archive(
        &self,
        chat_id: &str,
    ) -> Result<OwnedMutexGuard<()>, EngineError> {
        self.release_idle_native_writer(chat_id, "archiving", "archive")
            .await
    }

    /// Same-chat rewind mutates the provider thread through a separate native
    /// client. A parked warm runtime still holds the pre-rewind history in
    /// memory and would continue from it, so release it first and keep
    /// dispatch excluded through the revert and its projection commit.
    pub(crate) async fn prepare_native_rewind(
        &self,
        chat_id: &str,
    ) -> Result<OwnedMutexGuard<()>, EngineError> {
        self.release_idle_native_writer(chat_id, "rewinding", "rewind")
            .await
    }

    async fn release_idle_native_writer(
        &self,
        chat_id: &str,
        gerund: &str,
        operation: &str,
    ) -> Result<OwnedMutexGuard<()>, EngineError> {
        let transition = self.transition_gate(chat_id).lock_owned().await;
        if self.active_turn_id(chat_id).is_some() {
            return Err(EngineError::Other(format!(
                "The task has an active or pending turn; stop it before {gerund}."
            )));
        }
        let closing = {
            let mut runs = lock(&self.inner.runs);
            runs.get_mut(chat_id).map(|run| {
                // Codex's existing idle mailbox-EOF path shuts down its child
                // before closing the event stream. Do not interrupt: its Done
                // notification can precede child teardown.
                let (closed_tx, closed_rx) = mpsc::channel(1);
                drop(closed_rx);
                let previous = std::mem::replace(&mut run.steer_tx, closed_tx);
                let closing = (run.run_id.clone(), run.idle_stream_closed.clone());
                drop(previous);
                closing
            })
        };
        if let Some((run_id, closed)) = closing {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while self.is_live(chat_id, &run_id) {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .map_err(|_| EngineError::Other(format!(
                "The idle native writer did not close before the {operation} deadline; {operation} was not attempted.",
            )))?;
            if !closed.load(Ordering::Acquire) {
                return Err(EngineError::Other(format!(
                    "The native runtime ended without confirmed idle stream closure; {operation} was not attempted.",
                )));
            }
        }
        Ok(transition)
    }

    /// Stage a requested permission change and apply it only when the current
    /// provider runtime is no longer active. A live turn keeps its effective
    /// policy and diagnostics until a later dispatch actually starts under the
    /// requested policy. An idle/parked runtime is retired under the same gate
    /// used by dispatch, then the new effective policy is published.
    pub async fn set_permission_mode(
        &self,
        chat_id: &str,
        mode: PermissionMode,
        actor: Option<String>,
    ) -> Result<(), EngineError> {
        let prepared = self.prepare_permission_mode(chat_id, mode, actor).await?;
        let workspace = self
            .inner
            .workspace()
            .ok_or_else(|| EngineError::Other("workspace host not wired".into()))?;
        workspace.set_chat_config(chat_id, &prepared.config)?;
        Ok(())
    }

    /// Resolve and stage a permission-mode change while holding the same
    /// per-task transition guard used by dispatch. No registry state is
    /// published; the caller owns the durable projection commit.
    pub(crate) async fn prepare_permission_mode(
        &self,
        chat_id: &str,
        mode: PermissionMode,
        actor: Option<String>,
    ) -> Result<PreparedTaskConfig, EngineError> {
        let gate = self.transition_gate(chat_id);
        let transition = gate.lock_owned().await;
        let workspace = self
            .inner
            .workspace()
            .ok_or_else(|| EngineError::Other("workspace host not wired".into()))?;
        let Some(chat) = workspace.chat(chat_id)? else {
            return Err(EngineError::Other("chat not found".into()));
        };
        let Some(original) = chat.config else {
            return Err(EngineError::Other(
                "chat has no provider configuration".into(),
            ));
        };
        let mut config = original.clone();
        let harness = config.harness;
        let policy =
            crate::permissions::resolve_preview_for(harness, config.model.as_deref(), mode);
        if crate::permissions::hidden_ask_fallback(&policy) {
            return Err(EngineError::Other(
                "this provider, model, or execution ceiling cannot enforce Approve for me or Full access; choose an available mode"
                    .into(),
            ));
        }
        let previous = config.effective_permission_mode;
        let ceiling_source = config.permission_ceiling_source;
        let degradation_reason = config.permission_degradation_reason;
        config.permission_mode = mode;
        push_permission_audit(
            &mut config,
            PermissionAuditEntry {
                actor: actor.unwrap_or_else(|| "local-user".into()),
                machine: policy.machine_name.clone(),
                requested_mode: mode,
                // This entry records the request, not an application event.
                // Keep the policy the running provider is actually enforcing.
                effective_mode: previous,
                previous_mode: Some(previous),
                provider_mapping: format!("pending: {}", policy.provider_mapping),
                ceiling_source,
                reason: degradation_reason,
                timestamp_ms: now_ms(),
            },
        );
        if self.turn_in_flight(chat_id) {
            return Ok(PreparedTaskConfig {
                config,
                _account_transition: None,
                _chat_transition: transition,
            });
        }

        // A parked child still owns the old runtime signature. Retire it while
        // dispatch is excluded by `_transition`, so a new run cannot slip in
        // and be cancelled by a delayed retirement task.
        self.interrupt(chat_id).await?;
        if lock(&self.inner.runs).contains_key(chat_id) {
            return Err(EngineError::Other(
                "permission runtime did not retire before the deadline".into(),
            ));
        }

        let Some(chat) = workspace.chat(chat_id)? else {
            return Err(EngineError::Other("chat not found after retirement".into()));
        };
        let Some(current) = chat.config else {
            return Err(EngineError::Other(
                "chat lost provider configuration during retirement".into(),
            ));
        };
        if current != original {
            return Err(EngineError::Other(
                "chat configuration changed while preparing the permission update".into(),
            ));
        }
        apply_permission_policy(
            &mut config,
            &policy,
            format!("engine:{}", self.inner.device_id),
        );
        Ok(PreparedTaskConfig {
            config,
            _account_transition: None,
            _chat_transition: transition,
        })
    }

    /// Establish a provider-wide lifecycle boundary. Active turns are never
    /// interrupted for an account change; the caller must stop them first.
    /// Idle warm runtimes are retired while dispatch is excluded, and the
    /// returned guard keeps that exclusion in force through the metadata or
    /// provider-login mutation performed by the RPC layer.
    pub async fn prepare_provider_account_change(
        &self,
        harness: HarnessId,
    ) -> Result<OwnedMutexGuard<()>, EngineError> {
        let guard = self.inner.account_transition.clone().lock_owned().await;
        let chats = lock(&self.inner.runs)
            .iter()
            .filter(|(_, run)| run.runtime_config.harness_id == harness)
            .map(|(chat_id, _)| chat_id.clone())
            .collect::<Vec<_>>();
        if chats.iter().any(|chat_id| self.turn_in_flight(chat_id)) {
            return Err(EngineError::Other(
                "An active provider turn is still running. Stop it before changing the account."
                    .into(),
            ));
        }
        for chat_id in &chats {
            self.interrupt(chat_id).await?;
        }
        if lock(&self.inner.runs)
            .values()
            .any(|run| run.runtime_config.harness_id == harness)
        {
            return Err(EngineError::Other(
                "A warm provider runtime did not retire before the account-change deadline.".into(),
            ));
        }
        Ok(guard)
    }

    /// Change one chat's auth instance without changing global provider login
    /// or its canonical provider-session binding. The next send launches a
    /// fresh authenticated runtime and resumes the same managed session/cwd.
    pub async fn set_provider_account(
        &self,
        chat_id: &str,
        account_id: &str,
    ) -> Result<(), EngineError> {
        let prepared = self
            .prepare_task_provider_account(chat_id, account_id)
            .await?;
        let workspace = self
            .inner
            .workspace()
            .ok_or_else(|| EngineError::Other("workspace host not wired".into()))?;
        workspace.set_chat_config(chat_id, &prepared.config)?;
        self.invalidate_provider_account_caches(prepared.config.harness);
        Ok(())
    }

    /// Prepare a chat-level provider-account switch without publishing the
    /// registry row. Both provider-wide and per-task transition guards remain
    /// held in the returned value through the caller's durable commit.
    pub(crate) async fn prepare_task_provider_account(
        &self,
        chat_id: &str,
        account_id: &str,
    ) -> Result<PreparedTaskConfig, EngineError> {
        let account_transition = self.inner.account_transition.clone().lock_owned().await;
        let gate = self.transition_gate(chat_id);
        let chat_transition = gate.lock_owned().await;
        let workspace = self
            .inner
            .workspace()
            .ok_or_else(|| EngineError::Other("workspace host not wired".into()))?;
        let Some(chat) = workspace.chat(chat_id)? else {
            return Err(EngineError::Other("chat not found".into()));
        };
        let Some(mut config) = chat.config else {
            return Err(EngineError::Other(
                "chat has no provider configuration".into(),
            ));
        };
        if config.harness != HarnessId::Codex {
            return Err(EngineError::Other(
                "Chat-level account selection is currently supported for Codex only.".into(),
            ));
        }
        let accounts = self
            .inner
            .agent_accounts
            .get()
            .ok_or_else(|| EngineError::Other("provider accounts are not wired".into()))?;
        let resolved = accounts.resolve_execution(config.harness, Some(account_id))?;
        if config.provider_account_id.as_deref() == Some(resolved.account_id.as_str()) {
            return Ok(PreparedTaskConfig {
                config,
                _account_transition: Some(account_transition),
                _chat_transition: chat_transition,
            });
        }
        if self.turn_in_flight(chat_id) {
            return Err(EngineError::Other(
                "This chat has an active turn. Stop it before switching accounts.".into(),
            ));
        }
        self.interrupt(chat_id).await?;
        if lock(&self.inner.runs).contains_key(chat_id) {
            return Err(EngineError::Other(
                "The warm runtime did not retire before the account-change deadline.".into(),
            ));
        }
        config.provider_account_id = Some(resolved.account_id);
        Ok(PreparedTaskConfig {
            config,
            _account_transition: Some(account_transition),
            _chat_transition: chat_transition,
        })
    }

    pub(crate) fn invalidate_provider_account_caches(&self, harness: HarnessId) {
        if let Some(accounts) = self.inner.agent_accounts.get() {
            accounts.invalidate_caches(harness);
        }
    }

    /// Any run currently working or blocked on input — the auto-updater's
    /// "don't restart from under a session" gate.
    pub fn any_active(&self) -> bool {
        lock(&self.inner.statuses).values().any(is_active)
    }

    /// The last request dispatched for a chat (steer→new-turn fallback).
    pub fn last_request(&self, chat_id: &str) -> Option<RunRequest> {
        lock(&self.inner.last_requests).get(chat_id).cloned()
    }

    /// Subscribe to a chat's live event stream: returns the journal replay after
    /// `after_seq` plus a live receiver. Subscribe-then-replay ordering means overlap
    /// (dedupe by seq) rather than gaps.
    pub fn subscribe(
        &self,
        chat_id: &str,
        after_seq: u64,
    ) -> Result<(Vec<JournaledEvent>, broadcast::Receiver<JournaledEvent>), EngineError> {
        let rx = {
            let mut hubs = lock(&self.inner.hubs);
            hubs.entry(chat_id.to_string())
                .or_insert_with(|| broadcast::channel(1024).0)
                .subscribe()
        };
        let replay = self
            .inner
            .journal
            .replay(chat_id, after_seq)?
            .into_iter()
            .map(|(seq, event)| JournaledEvent {
                seq,
                event: tool_event_preview(&event),
            })
            .collect();
        Ok((replay, rx))
    }

    /// Historical runtime metadata that was durably recorded by the harness.
    /// The assistant message id is the authoritative join key; current task
    /// configuration and provider-account selection are intentionally ignored.
    pub(crate) fn activity_run_metadata(
        &self,
        chat_id: &str,
    ) -> HashMap<String, crate::activity_history::ActivityRunMetadata> {
        let events = match self.inner.journal.replay(chat_id, 0) {
            Ok(events) => events,
            Err(error) => {
                tracing::warn!(chat = %chat_id, %error, "session history could not read recorded run metadata");
                return HashMap::new();
            }
        };
        events
            .into_iter()
            .filter_map(|(_, event)| match event {
                AgentEvent::SessionStarted {
                    harness,
                    model,
                    assistant_message_id,
                    ..
                } => Some((
                    assistant_message_id,
                    crate::activity_history::ActivityRunMetadata {
                        harness: Some(harness),
                        model: (!model.trim().is_empty()).then_some(model),
                    },
                )),
                _ => None,
            })
            .collect()
    }

    /// Start (or route) a run for `chat_id`.
    ///
    /// - The user message entry is written to the doc immediately (id = `message_id`).
    /// - A live steerable run receives the prompt as its next turn via the mailbox
    ///   (luna's persistent-session routing); otherwise any live run is interrupted
    ///   first — never two runtimes driving one chat.
    pub(crate) fn release_side_chat_runtime(&self, chat_id: &str) -> Result<(), EngineError> {
        if self.has_live_run(chat_id) {
            return Err(EngineError::Other(
                "Side chat runtime has not stopped".into(),
            ));
        }
        lock(&self.inner.last_requests).remove(chat_id);
        lock(&self.inner.harness_sessions).remove(chat_id);
        lock(&self.inner.hubs).remove(chat_id);
        lock(&self.inner.transition_gates).remove(chat_id);
        if let Some(task) = lock(&self.inner.recovery_tasks).remove(chat_id) {
            task.abort();
        }
        let mut statuses = lock(&self.inner.statuses);
        statuses.remove(chat_id);
        let mut rows: Vec<_> = statuses.values().cloned().collect();
        rows.sort_by(|left, right| left.chat_id.cmp(&right.chat_id));
        self.inner.sessions_tx.send_replace(rows);
        Ok(())
    }

    pub(crate) fn freeze_side_chat_account(
        &self,
        config: &mut ChatConfig,
    ) -> Result<(), EngineError> {
        if matches!(config.harness, HarnessId::Codex | HarnessId::ClaudeCode) {
            let accounts = self
                .inner
                .agent_accounts
                .get()
                .ok_or_else(|| EngineError::Other("Provider accounts unavailable".into()))?;
            config.provider_account_id = Some(
                accounts
                    .resolve_execution(config.harness, config.provider_account_id.as_deref())?
                    .account_id,
            );
        }
        Ok(())
    }

    pub(crate) fn set_side_chat_lookup(&self, lookup: SideChatLookup) {
        let _ = self.inner.side_chat_lookup.set(lookup);
    }

    pub(crate) async fn dispatch_side_chat(
        &self,
        chat_id: &str,
        request: RunRequest,
        message_id: Option<String>,
    ) -> Result<String, EngineError> {
        let harness = request
            .harness
            .ok_or_else(|| EngineError::Other("Side chat provider missing".into()))?;
        let workspace_revision = self.inner.recovery.workspace_repair_revision(chat_id)?;
        self.dispatch_with_context(
            chat_id,
            harness,
            request,
            message_id,
            None,
            workspace_revision,
        )
        .await
    }

    pub async fn dispatch(
        &self,
        chat_id: &str,
        harness_id: HarnessId,
        request: RunRequest,
        message_id: Option<String>,
    ) -> Result<String, EngineError> {
        if self
            .inner
            .side_chat_lookup
            .get()
            .is_some_and(|lookup| lookup(chat_id))
        {
            return Err(EngineError::Other(
                "Side chats require an active side-panel lease".into(),
            ));
        }
        let workspace_revision = self.inner.recovery.workspace_repair_revision(chat_id)?;
        self.dispatch_at_workspace_revision(
            chat_id,
            harness_id,
            request,
            message_id,
            workspace_revision,
        )
        .await
    }

    /// Server-side original admission evidence, never read from RunRequest or
    /// caller-controlled model options. Deferred executable rows retain W.
    #[doc(hidden)]
    pub async fn dispatch_at_workspace_revision(
        &self,
        chat_id: &str,
        harness_id: HarnessId,
        request: RunRequest,
        message_id: Option<String>,
        workspace_repair_revision: u64,
    ) -> Result<String, EngineError> {
        if self
            .inner
            .side_chat_lookup
            .get()
            .is_some_and(|lookup| lookup(chat_id))
        {
            return Err(EngineError::Other(
                "Side chats require an active side-panel lease".into(),
            ));
        }
        self.dispatch_with_context(
            chat_id,
            harness_id,
            request,
            message_id,
            None,
            workspace_repair_revision,
        )
        .await
    }

    fn mark_goal_sent(&self, chat_id: &str, user_id: &str) {
        let Some(host) = self.inner.doc_host() else {
            return;
        };
        let id = user_id.to_owned();
        if let Err(error) =
            host.persist_goal_mark(chat_id, &new_id(), move |marks| marks.mark_sent(&id))
        {
            tracing::warn!(chat = %chat_id, %error, "goal sent mark persistence failed");
        }
    }

    /// Warm "send as goal": the loaded Codex runtime runs Codex's clear+set on
    /// its own connection and starts (or extends) the turn itself; its
    /// `turn/started` arrives as an autonomous turn. `Ok(false)` means the
    /// mailbox is gone and the caller replaces the runtime. A correlated
    /// rejection removes the echo; an unknown outcome is never retried.
    async fn start_goal_on_live_runtime(
        &self,
        chat_id: &str,
        run_id: &str,
        steer_tx: &mpsc::Sender<SteerMessage>,
        user_id: &str,
        request: &RunRequest,
        goal: luna_proto::GoalStartRequest,
    ) -> Result<bool, EngineError> {
        let handle = self.doc_handle(chat_id)?;
        // The message precedes the turn Codex starts within milliseconds.
        handle.write_user_message(user_id, &request.prompt, now_ms())?;
        let (acceptance_tx, acceptance_rx) = tokio::sync::oneshot::channel();
        let message = SteerMessage {
            prompt: request.prompt.clone(),
            attachments: Vec::new(),
            input_items: Vec::new(),
            message_id: Some(user_id.to_owned()),
            delivery: luna_harness::SteerDelivery::AllowFollowUpTurn,
            acceptance: Some(acceptance_tx),
            goal: Some(goal),
        };
        if steer_tx.try_send(message).is_err() {
            handle
                .doc()
                .remove_rejected_user_message(user_id, &request.prompt)?;
            return Ok(false);
        }
        self.mark_goal_sent(chat_id, user_id);
        let acceptance =
            tokio::time::timeout(std::time::Duration::from_secs(15), acceptance_rx).await;
        match acceptance {
            Ok(Ok(SteerAcceptance::Accepted)) => {
                self.inner.note_message(chat_id, &request.prompt);
                if self.is_live(chat_id, run_id) {
                    tracing::debug!(chat = %chat_id, "goal started on the loaded Codex runtime");
                }
                Ok(true)
            }
            Ok(Ok(SteerAcceptance::Rejected(message))) => {
                handle
                    .doc()
                    .remove_rejected_user_message(user_id, &request.prompt)?;
                Err(EngineError::Other(format!("Codex rejected the goal: {message}")))
            }
            Ok(Ok(SteerAcceptance::Unknown(message))) => Err(EngineError::Other(format!(
                "Codex goal start is unconfirmed and was not retried: {message}. Refresh the goal before changing it."
            ))),
            Ok(Err(_)) | Err(_) => Err(EngineError::Other(
                "Codex goal start is unconfirmed and was not retried. Refresh the goal before changing it."
                    .into(),
            )),
        }
    }

    fn dispatch_with_context<'a>(
        &'a self,
        chat_id: &'a str,
        harness_id: HarnessId,
        request: RunRequest,
        message_id: Option<String>,
        recovery: Option<RecoveryAttemptContext>,
        workspace_repair_revision: u64,
    ) -> futures::future::BoxFuture<'a, Result<String, EngineError>> {
        Box::pin(self.dispatch_inner(
            chat_id,
            harness_id,
            request,
            message_id,
            recovery,
            workspace_repair_revision,
        ))
    }

    async fn dispatch_inner(
        &self,
        chat_id: &str,
        harness_id: HarnessId,
        mut request: RunRequest,
        mut message_id: Option<String>,
        recovery: Option<RecoveryAttemptContext>,
        workspace_repair_revision: u64,
    ) -> Result<String, EngineError> {
        let _in_flight = DispatchInFlight::enter(&self.inner);
        crate::provider_inputs::admit(harness_id, &request.attachments, &request.input_items)?;
        if recovery.is_some()
            && self
                .inner
                .side_chat_lookup
                .get()
                .is_some_and(|lookup| lookup(chat_id))
        {
            return Err(EngineError::Other(
                "Side chat recovery never replays a send".into(),
            ));
        }
        if let Some(goal) = request.goal.as_ref() {
            if !matches!(harness_id, HarnessId::Codex | HarnessId::ClaudeCode) {
                return Err(EngineError::Other(
                    "Goals need a Codex or Claude Code task".into(),
                ));
            }
            if harness_id == HarnessId::ClaudeCode {
                crate::claude_ongoing::admit_goal_start(goal, self.turn_in_flight(chat_id))?;
            }
            goal.validate()
                .map_err(|error| EngineError::Other(error.into()))?;
            if request.review.is_some()
                || !request.attachments.is_empty()
                || !request.input_items.is_empty()
            {
                return Err(EngineError::Other(
                    "A goal is text only. Remove attachments, mentions and comments, then send it again."
                        .into(),
                ));
            }
            // The transcript message is exactly the objective Codex received.
            request.prompt = goal.objective.clone();
        }
        // CC-15: Claude's review is its own `/code-review` line (or an
        // ordinary message for custom instructions), never `review/start`.
        if harness_id == HarnessId::ClaudeCode
            && let Some(review) = request.review.take()
        {
            if self.turn_in_flight(chat_id) {
                return Err(EngineError::Other(
                    "Wait for the active turn to finish before starting a review".into(),
                ));
            }
            if !request.attachments.is_empty() || request.worktree.is_some() {
                return Err(EngineError::Other(
                    "A review uses the task's existing checkout without attachments".into(),
                ));
            }
            request.prompt = crate::claude_ongoing::review_prompt(&review.target)?;
        }
        if let Some(review) = request.review.as_ref() {
            if harness_id != HarnessId::Codex {
                return Err(EngineError::Other(
                    "Native code review requires Codex".into(),
                ));
            }
            review.target.validate().map_err(EngineError::Other)?;
            if self.turn_in_flight(chat_id) {
                return Err(EngineError::Other(
                    "Wait for the active turn to finish before starting a native review".into(),
                ));
            }
            if !request.attachments.is_empty() || request.worktree.is_some() {
                return Err(EngineError::Other(
                    "A native review uses the task's existing checkout without attachments".into(),
                ));
            }
        }
        // A run owns one ticket for its entire lifetime, including parked
        // persistent sessions. Acquire before account/chat gates and before
        // accepting a user message or starting a provider.
        let run_permit = self
            .inner
            .journal
            .admit_async(std::time::Duration::from_secs(5))
            .await?;
        // Provider selection and runtime routing/startup are one boundary: an
        // account mutation cannot race a warm steer or a fresh process spawn.
        let _account_transition = self.inner.account_transition.lock().await;
        let transition_gate = self.transition_gate(chat_id);
        let _transition = transition_gate.lock().await;
        if request.review.is_some() && self.turn_in_flight(chat_id) {
            return Err(EngineError::Other(
                "Wait for the active turn to finish before starting a native review".into(),
            ));
        }
        if request.review.is_some()
            && let Some(workspace) = self.inner.workspace()
        {
            let chat = workspace
                .chat(chat_id)?
                .ok_or_else(|| EngineError::Other("Review task is unavailable".into()))?;
            let config = chat.config.ok_or_else(|| {
                EngineError::Other("Review task has no provider configuration".into())
            })?;
            if config.harness != HarnessId::Codex
                || config.provider_account_id != request.provider_account_id
                || chat.cwd.as_deref() != Some(request.cwd.as_str())
            {
                return Err(EngineError::Other(
                    "Review account or checkout changed; reopen the review picker".into(),
                ));
            }
        }
        if self.inner.recovery.workspace_repair_revision(chat_id)? != workspace_repair_revision {
            return Err(EngineError::Other("Workspace association changed after dispatch admission; review and submit a new message.".into()));
        }
        // A queue drain or already accepted command may have waited behind
        // native archive after freezing its request. Read the committed row
        // now, before starting a provider. A deliberate new user send revives
        // the row at command acceptance and remains allowed through this gate.
        if let Some(workspace) = self.inner.workspace()
            && workspace.chat(chat_id)?.is_some_and(|chat| chat.archived)
        {
            return Err(EngineError::Other(
                "The task was archived before dispatch; submit a new message explicitly to resume it."
                    .into(),
            ));
        }
        let original_message_id = message_id.get_or_insert_with(new_id).clone();
        self.inner
            .recovery
            .record_execution_workspace_repair_revision(
                chat_id,
                &original_message_id,
                workspace_repair_revision,
            )?;
        // Runtime contracts are host authority, never client-provided model
        // options. Strip even a syntactically valid forged contract before any
        // request snapshot, recovery record, or provider dispatch.
        request
            .model_options
            .remove(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION);
        let context_control = self.doc_handle(chat_id)?.doc().context_control();
        if context_control.pending_rewind.is_some() {
            return Err(EngineError::Other(
                "Conversation rewind is unresolved. Reconcile its provider history before sending another turn.".into(),
            ));
        }
        if context_control.compact.blocks_new_turn() {
            return Err(EngineError::Other(
                context_control
                    .compact
                    .detail
                    .unwrap_or_else(|| {
                        "Context compaction is unresolved. Clear context to create a fresh binding, or wait for provider readback."
                            .into()
                    }),
            ));
        }
        if request.review.is_some() && context_control.pending_handoff.is_some() {
            return Err(EngineError::Other("Send the pending context handoff in an ordinary turn before starting a native review".into()));
        }
        if request.goal.is_some() && context_control.pending_handoff.is_some() {
            return Err(EngineError::Other(
                "Send the pending context handoff in an ordinary turn before starting a goal"
                    .into(),
            ));
        }
        if let Some(handoff) = context_control.pending_handoff.as_ref()
            && !request
                .prompt
                .starts_with("[LUNA user-selected context handoff]\n")
            && self
                .inner
                .task_identity
                .get()
                .is_some_and(|identity| identity.current_binding_pending(chat_id))
        {
            request.prompt = format!(
                "[LUNA user-selected context handoff]\n{}\n[End handoff]\n\n{}",
                handoff.text, request.prompt
            );
        }
        // Recovery and replays never re-run a goal start (an uncertain
        // provider mutation); they continue the provider session instead.
        let mut frozen_request = request.clone();
        frozen_request.goal = None;
        if let Some(recovery) = recovery.as_ref() {
            let Some(claimed) = self.inner.recovery.latest(chat_id)? else {
                return Err(EngineError::Other(
                    "recovery chain was cancelled or superseded before dispatch".into(),
                ));
            };
            if claimed.view.chain_id != recovery.chain_id
                || claimed.view.attempt_count != recovery.attempt_count
                || claimed.view.status != RecoveryStatus::Running
            {
                return Err(EngineError::Other(
                    "recovery chain was cancelled or superseded before dispatch".into(),
                ));
            }
            if recovery.automatic {
                let (status, detail) = if !self.automatic_retries_enabled() {
                    (
                        RecoveryStatus::Held,
                        "Automatic retries were disabled before provider dispatch. Review and use Send now if recovery is still needed.",
                    )
                } else if now_ms() > claimed.view.automatic_deadline_ms {
                    (
                        RecoveryStatus::RecoveryNeeded,
                        "The initial six-hour automatic recovery window expired before provider dispatch. Partial output and completed effects remain preserved.",
                    )
                } else {
                    (RecoveryStatus::Running, "")
                };
                if status != RecoveryStatus::Running {
                    let mut next = claimed.clone();
                    next.view.status = status;
                    next.view.automatic = false;
                    next.view.retry_at_ms = None;
                    next.view.detail = detail.into();
                    next.view.updated_at_ms = now_ms();
                    let change = self.inner.recovery.prepare_change(claimed, next);
                    if self.inner.recovery.apply_change(&change)? {
                        self.publish_committed_recovery(&change.next);
                    }
                    return Err(EngineError::Other(detail.into()));
                }
            }
        } else {
            self.supersede_recovery(chat_id, "Superseded by newer user activity.")?;
        }
        // Project-less chats store cwd `~` (the creating device can't know the
        // host's home); expand it here, on the host, where the run spawns.
        request.cwd = expand_home(&request.cwd);
        if matches!(harness_id, HarnessId::ClaudeCode | HarnessId::Codex)
            && let Some(accounts) = self.inner.agent_accounts.get()
        {
            let execution =
                accounts.resolve_execution(harness_id, request.provider_account_id.as_deref())?;
            request.provider_account_id = Some(execution.account_id);
            request.model_options.insert(
                luna_harness::LUNA_PROVIDER_AUTH_INSTANCE_OPTION.into(),
                execution.auth_instance_id.into(),
            );
            if let Some(home) = execution.codex_home {
                let home = home.to_str().ok_or_else(|| {
                    EngineError::Other("Codex account home is not valid UTF-8".into())
                })?;
                request.model_options.insert(
                    luna_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
                    home.into(),
                );
            }
            if execution.codex_file_auth {
                request.model_options.insert(
                    luna_harness::codex::LUNA_CODEX_FILE_AUTH_OPTION.into(),
                    true.into(),
                );
            }
        }
        if !crate::permissions::provider_can_run(harness_id) {
            return Err(EngineError::Other(format!(
                "{} permission enforcement is unavailable; choose a provider with a verified LUNA permission contract",
                format!("{harness_id:?}").to_lowercase()
            )));
        }
        let child = self
            .inner
            .workspace()
            .and_then(|workspace| workspace.chat(chat_id).ok().flatten())
            .and_then(|chat| chat.child_thread);
        if let Some(child) = &child {
            let current_parent_ceiling = self
                .inner
                .workspace()
                .and_then(|workspace| workspace.chat(&child.parent_task_id).ok().flatten())
                .and_then(|parent| parent.config)
                .map(|config| config.effective_permission_mode)
                .unwrap_or(child.permission_ceiling);
            let ceiling = std::cmp::min(child.permission_ceiling, current_parent_ceiling);
            request.parent_permission_ceiling = Some(
                request
                    .parent_permission_ceiling
                    .map_or(ceiling, |requested| std::cmp::min(requested, ceiling)),
            );
        }
        // Side conversations preserve even a legacy stricter sandbox. The
        // ordinary resolver intentionally derives sandbox from product mode;
        // carrying a parent's ReadOnly value through that resolver would
        // otherwise widen it to WorkspaceWrite. Only adapters with an actual
        // read-only wire contract can retain that ceiling.
        let side_sandbox = self
            .inner
            .side_chat_lookup
            .get()
            .filter(|lookup| lookup(chat_id))
            .map(|_| request.sandbox)
            .or_else(|| child.as_ref().and_then(|child| child.sandbox_ceiling));
        if side_sandbox == Some(SandboxLevel::ReadOnly) {
            if !matches!(harness_id, HarnessId::Codex | HarnessId::Mock) {
                return Err(EngineError::Other(
                    "This provider cannot preserve the parent's read-only sandbox in a side chat"
                        .into(),
                ));
            }
            request.permission_mode = PermissionMode::Ask;
        }
        let permission_policy = crate::permissions::resolve_run_policy(harness_id, &mut request);
        if let Some(sandbox) = side_sandbox {
            if sandbox == SandboxLevel::ReadOnly
                || (sandbox == SandboxLevel::WorkspaceWrite
                    && request.sandbox == SandboxLevel::DangerFullAccess)
            {
                request.sandbox = sandbox;
            }
        }
        if crate::permissions::hidden_ask_fallback(&permission_policy) {
            return Err(EngineError::Other(
                "requested permission mode would fall back to hidden Ask; choose an available Approve for me or Full access mode"
                    .into(),
            ));
        }
        if let Some(explicit_resume) = request.resume.as_deref()
            && let Some(task_identity) = self.inner.task_identity.get()
        {
            // Caller-supplied ids must pass the managed ownership boundary
            // before a warm route can steer an already-running provider.
            task_identity.validate_explicit_resume(
                chat_id,
                harness_id,
                &request.cwd,
                explicit_resume,
            )?;
        }
        if matches!(harness_id, HarnessId::Codex | HarnessId::ClaudeCode)
            && self.inner.task_identity.get().is_some()
        {
            let known_session_id = self
                .inner
                .task_identity
                .get()
                .and_then(|identity| identity.view(chat_id))
                .and_then(|view| view.current_binding)
                .and_then(|binding| {
                    (!binding.provider_task_id.is_empty()).then_some(binding.provider_task_id)
                })
                .or_else(|| self.inner.resume_for(chat_id, &request.cwd));
            let prior = known_session_id
                .as_deref()
                .map(|session_id| self.inner.journal.runtime_contract(chat_id, session_id))
                .transpose()?
                .flatten();
            let context = self.runtime_context_for(chat_id, harness_id, &request)?;
            let (user_instructions, workspace_instructions) = self
                .integration_settings()
                .map(|settings| settings.contributions(context.project_id.as_deref()))
                .unwrap_or_default();
            let provider_delivery = self
                .inner
                .registry
                .resolve(harness_id)?
                .luna_runtime_delivery();
            let (cross_task_tools, cross_task_omissions) =
                self.cross_task_catalog(&context, provider_delivery);
            let compiled =
                crate::runtime_contract::compile(crate::runtime_contract::CompileInput {
                    context,
                    provider_delivery,
                    user_instructions,
                    workspace_instructions,
                    resume: known_session_id.is_some(),
                    acknowledged_contract: prior,
                    cross_task_tools,
                    cross_task_omissions,
                })?;
            if compiled.must_reconstruct {
                return Err(EngineError::Other(
                    "LUNA runtime instructions or tools changed for this provider session; start a fresh provider binding at a safe boundary before continuing".into()
                ));
            }
            if compiled.requires_safe_boundary && self.turn_in_flight(chat_id) {
                return Err(EngineError::Other(
                    "LUNA runtime instructions changed during the active provider turn; wait for it to settle before sending with the new contract".into()
                ));
            }
            request.model_options.insert(
                luna_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
                serde_json::to_value(&compiled.contract)?,
            );
        }
        let routed = lock(&self.inner.runs).get(chat_id).map(|h| {
            (
                h.run_id.clone(),
                h.steerable,
                h.workspace_repair_revision == workspace_repair_revision
                    && h.runtime_config.can_route(harness_id, &request),
                h.steer_tx.clone(),
                h.routed_steers.clone(),
            )
        });
        let goal_runtime = request.goal.is_some()
            && lock(&self.inner.runs).get(chat_id).is_some_and(|h| {
                h.workspace_repair_revision == workspace_repair_revision
                    && h.runtime_config.can_route_goal(harness_id, &request)
            });
        if routed.as_ref().is_some_and(|(_, _, _, _, ledger)| {
            lock(ledger)
                .iter()
                .any(|steer| steer.delivery == luna_harness::SteerDelivery::RequireActiveTurn)
        }) {
            return Err(EngineError::Other(
                "native steer incorporation is still unconfirmed; wait for the current turn boundary"
                    .into(),
            ));
        }
        // The Changes pane's latest-turn view remains an eager best-effort
        // snapshot. Durable checkpoints start only after a fresh run is ready
        // or a warm provider confirms its steer boundary.
        self.note_turn_start(chat_id, &request.cwd);
        if let Some((run_id, steerable, same_runtime, steer_tx, ledger)) = routed {
            let user_id = message_id.clone().unwrap_or_else(new_id);
            if let Some(goal) = request.goal.clone()
                && steerable
                && goal_runtime
                && self
                    .start_goal_on_live_runtime(
                        chat_id, &run_id, &steer_tx, &user_id, &request, goal,
                    )
                    .await?
            {
                return Ok(run_id);
            }
            // CC-06: a parked Claude run applies a model/effort-only change
            // natively (acks + readback before the prompt) instead of restarting.
            let same_runtime = same_runtime
                || (steerable
                    && self.stage_live_switch(
                        chat_id,
                        &run_id,
                        harness_id,
                        &request,
                        &user_id,
                        workspace_repair_revision,
                    ));
            // A Claude goal is Claude's own `/goal` line through the same
            // warm mailbox (the harness writes it); Codex goals never are.
            let claude_goal = request
                .goal
                .clone()
                .filter(|_| harness_id == HarnessId::ClaudeCode);
            let accepted =
                if steerable && same_runtime && (request.goal.is_none() || claude_goal.is_some()) {
                    // Warm dispatch uses the same mailbox as explicit steering.
                    // Register acceptance before a fast boundary can retire it.
                    let mut pending = lock(&ledger);
                    let message = SteerMessage {
                        input_items: request.input_items.clone(),
                        attachments: request.attachments.clone(),
                        prompt: request.prompt.clone(),
                        message_id: Some(user_id.clone()),
                        delivery: luna_harness::SteerDelivery::AllowFollowUpTurn,
                        acceptance: None,
                        goal: claude_goal,
                    };
                    if steer_tx.try_send(message).is_ok() {
                        pending.push_back(RoutedSteer {
                            attachments: request.attachments.clone(),
                            input_items: request.input_items.clone(),
                            prompt: request.prompt.clone(),
                            message_id: user_id.clone(),
                            delivery: luna_harness::SteerDelivery::AllowFollowUpTurn,
                        });
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
            if accepted {
                let handle = self.doc_handle(chat_id)?;
                handle.write_user_message(&user_id, &request.prompt, now_ms())?;
                if request.goal.is_some() {
                    self.mark_goal_sent(chat_id, &user_id);
                }
                if self.is_live(chat_id, &run_id) {
                    // Working BEFORE the lastMessageAt bump: both ride the
                    // workspace doc from this one peer, so causal order makes it
                    // impossible for an observer to hold [new message, old status]
                    // — that gap read as unseen-with-no-live-run = a phantom
                    // "completed" flash on every remote send (2026-07-31).
                    self.set_status(chat_id, SessionStatus::Working, false);
                    self.inner.note_message(chat_id, &request.prompt);
                    return Ok(run_id);
                }
                if harness_id == HarnessId::Codex {
                    return Err(EngineError::Other(
                        "Native steering delivery is unknown after runtime exit; reconcile provider history before retrying. Input was not replayed.".into(),
                    ));
                }
                // The run died around the send. If its exit drain already
                // claimed the entry, that re-dispatch owns the message —
                // otherwise reclaim it and fall through to a fresh run.
                let reclaimed = {
                    let mut ledger = lock(&ledger);
                    let before = ledger.len();
                    ledger.retain(|s| s.message_id != user_id);
                    ledger.len() != before
                };
                if !reclaimed {
                    self.inner.note_message(chat_id, &request.prompt);
                    return Ok(run_id);
                }
                // Keep the already-written doc entry's id for the fresh run
                // below (write_user_message dedupes by id).
                message_id = Some(user_id);
            }
            if !same_runtime {
                tracing::debug!(
                    chat = %chat_id,
                    "restarting live harness to apply changed run configuration"
                );
            }
            // Mailbox closed (runtime mid-teardown / non-steering harness) or
            // the routed run died with the message reclaimed, or configuration
            // changed beyond what the text-only mailbox can carry: replace it.
            self.interrupt(chat_id).await?;
            if lock(&self.inner.runs).contains_key(chat_id) {
                return Err(EngineError::Other(
                    "previous runtime did not retire before replacement startup".into(),
                ));
            }
        }

        let harness = self.inner.registry.resolve(harness_id)?;
        let handle = self.doc_handle(chat_id)?;
        let user_id = message_id.unwrap_or_else(new_id);
        handle.write_user_message(&user_id, &request.prompt, now_ms())?;
        if request.goal.is_some() {
            self.mark_goal_sent(chat_id, &user_id);
        }
        if harness_id == HarnessId::Codex {
            // The first turn/start carries the same correlation id as every
            // steer, so native readback can prove this message's delivery.
            request.model_options.insert(
                luna_harness::codex::LUNA_CLIENT_USER_MESSAGE_ID_OPTION.into(),
                user_id.clone().into(),
            );
        }

        // Engine-owned resume (luna sessions.ts:736 — every dispatch read the
        // chat's stored harness session): callers always send `resume: None`;
        // the engine threads the chat's prior harness session back in so a new
        // process (app restart) continues the same harness conversation. The
        // startup-crash retry injects too — a stale id is the harness's
        // problem now (`session/load` falls back to `session/new` internally),
        // and starting the retry fresh silently dropped a good conversation.
        let explicit_resume = request.resume.clone();
        let managed = if let Some(task_identity) = self.inner.task_identity.get() {
            task_identity
                .prepare_resume(chat_id, harness_id, &request.cwd)
                .await?
        } else {
            None
        };
        if let Some(expected) = managed {
            if let Some(explicit) = explicit_resume.as_deref()
                && explicit != expected
            {
                return Err(EngineError::Other(format!(
                    "explicit resume id does not match the managed provider binding for task {chat_id}"
                )));
            }
            if explicit_resume.is_none() {
                request.resume = Some(expected);
            }
            if harness_id == HarnessId::Codex {
                request.model_options.insert(
                    luna_harness::codex::LUNA_STRICT_RESUME_OPTION.into(),
                    true.into(),
                );
            }
        } else if request.resume.is_none() {
            request.resume = self.inner.resume_for(chat_id, &request.cwd);
        }
        // The compiler's no-tools legacy resume decision is valid only for
        // the exact provider session inspected before warm routing.
        if let Some(contract) = request
            .model_options
            .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
            .and_then(|value| {
                serde_json::from_value::<luna_proto::LunaAgentRuntimeContract>(value.clone()).ok()
            })
            && contract.delivery == luna_proto::RuntimeDelivery::CodexDynamicTools
            && request.resume.is_some()
            && self
                .inner
                .journal
                .runtime_contract(chat_id, request.resume.as_deref().unwrap())?
                .is_none()
        {
            return Err(EngineError::Other(
                "Codex resume has no verified LUNA tool contract; start a fresh provider binding"
                    .into(),
            ));
        }
        if harness_id == HarnessId::Codex
            && let Some(workspace) = self.inner.workspace()
            && let Ok(Some(chat)) = workspace.chat(chat_id)
        {
            let title = chat
                .title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .unwrap_or("New task");
            request.model_options.insert(
                luna_harness::codex::LUNA_NATIVE_THREAD_NAME_OPTION.into(),
                format!("[LUNA] {title}").into(),
            );
        }
        if harness_id == HarnessId::Codex && request.resume.is_some() {
            let ids = handle
                .doc()
                .read_queue()?
                .into_iter()
                .filter(|row| {
                    matches!(
                        row.status,
                        luna_doc::QueueMessageStatus::Claimed { .. }
                            | luna_doc::QueueMessageStatus::SteeringAccepted { .. }
                            | luna_doc::QueueMessageStatus::DeliveryUnknown { .. }
                    )
                })
                .map(|row| row.id)
                .collect::<Vec<_>>();
            if !ids.is_empty() {
                request.model_options.insert(
                    luna_harness::codex::LUNA_STEER_RECONCILE_OPTION.into(),
                    serde_json::json!(ids),
                );
            }
        }
        let mut remembered = request.clone();
        remembered.goal = None;
        lock(&self.inner.last_requests).insert(chat_id.to_string(), remembered);

        let run_id = new_id();
        let (steer_tx, steer_rx) = mpsc::channel::<SteerMessage>(32);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (engine_tx, engine_rx) = mpsc::unbounded_channel::<AgentEvent>();
        let pending_inputs: PendingInputs = Arc::new(Mutex::new(HashMap::new()));

        // Input bridge: the harness asks questions; we mint the request id, park the
        // resolver for `respond_input`, and surface the event through the run pipeline.
        let request_input = {
            let pending = pending_inputs.clone();
            let engine_tx = engine_tx.clone();
            Box::new(move |questions: Vec<UserInputQuestion>| {
                let (tx, rx) = oneshot::channel();
                let request_id = new_id();
                lock(&pending).insert(
                    request_id.clone(),
                    PendingInput {
                        kind: PendingInputKind::Provider,
                        questions: questions.clone(),
                        resolver: tx,
                    },
                );
                // The full URL stays only in the live pending resolver. Strip
                // it even from in-memory events before journal/doc projection.
                let mut visible_questions = questions.clone();
                for question in &mut visible_questions {
                    if let Some(meta) = &mut question.mcp {
                        meta.launch_url = None;
                    }
                }
                let _ = engine_tx.send(AgentEvent::InputRequested {
                    request_id: request_id.clone(),
                    questions: visible_questions,
                });
                schedule_mcp_input_cleanup(
                    pending.clone(),
                    engine_tx.clone(),
                    request_id.clone(),
                    &questions,
                );
                schedule_approval_expiry(
                    pending.clone(),
                    engine_tx.clone(),
                    request_id,
                    &questions,
                );
                rx
            })
        };
        let interrupt_token = CancellationToken::new();
        let hold_recovery_on_interrupt = Arc::new(AtomicBool::new(true));
        let tool_sessions = self.clone();
        let tool_chat_id = chat_id.to_owned();
        let tool_run_id = run_id.clone();
        let controls = RunControls {
            request_input,
            steering: steer_rx,
            interrupt: interrupt_token.clone(),
            invoke_luna_tool: request
                .model_options
                .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
                .and_then(|value| {
                    serde_json::from_value::<luna_proto::LunaAgentRuntimeContract>(value.clone())
                        .ok()
                })
                .filter(|contract| !contract.tools.is_empty())
                .map(|contract| {
                    let cancel = interrupt_token.clone();
                    let sessions = tool_sessions.clone();
                    let chat_id = tool_chat_id.clone();
                    let run_id = tool_run_id.clone();
                    let backend = lock(&self.inner.cross_task_backends)
                        .iter()
                        .rev()
                        .find(|backend| backend.strong_count() > 0)
                        .cloned();
                    std::sync::Arc::new(move |invocation: luna_harness::LunaToolInvocation| {
                        let contract = contract.clone();
                        let cancel = cancel.clone();
                        let backend = backend.as_ref().and_then(Weak::upgrade);
                        let sessions = sessions.clone();
                        let chat_id = chat_id.clone();
                        let run_id = run_id.clone();
                        Box::pin(async move {
                            let result = if matches!(
                                invocation.name.as_str(),
                                "send_message_to_thread" | "create_thread"
                            ) {
                                sessions
                                    .invoke_approved_luna_send(
                                        &chat_id, &run_id, &contract, invocation, &cancel, backend,
                                    )
                                    .await
                            } else {
                                crate::runtime_contract::invoke_with_backend(
                                    &contract,
                                    &invocation.name,
                                    invocation.arguments,
                                    &cancel,
                                    backend,
                                )
                                .await
                            };
                            result.map_err(|error| {
                                luna_harness::HarnessError::Protocol(error.to_string())
                            })
                        })
                            as futures::future::BoxFuture<
                                'static,
                                Result<serde_json::Value, luna_harness::HarnessError>,
                            >
                    }) as luna_harness::LunaToolExecutor
                }),
        };

        if let Some(checkpoints) = self.git_checkpoints() {
            checkpoints.start(chat_id, &run_id, &request.cwd).await;
        }
        lock(&self.inner.runs).insert(
            chat_id.to_string(),
            RunHandle {
                workspace_repair_revision,
                idle_stream_closed: Arc::new(AtomicBool::new(false)),
                run_id: run_id.clone(),
                provider_turn: None,
                run_provider_binding: None,
                provider_turn_readiness: tokio::sync::watch::channel(
                    ProviderTurnReadiness::AwaitingBinding,
                )
                .0,
                steerable: harness.supports_steering(),
                runtime_config: RuntimeConfig::from_request(harness_id, &request),
                steer_tx,
                interrupt_token,
                cancel: cancel_tx,
                hold_recovery_on_interrupt: hold_recovery_on_interrupt.clone(),
                engine_tx,
                pending_inputs,
                routed_steers: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            },
        );
        self.set_status(chat_id, SessionStatus::Working, true);
        // AFTER Working (same causal-order guarantee as the steer path): the
        // lastMessageAt bump must never be observable ahead of the live run.
        self.inner.note_message(chat_id, &request.prompt);

        // Name the chat NOW, off the first prompt — not after the first
        // exchange completes ("called New session for a long time for no
        // reason"; the titler only needs the prompt and skips titled chats;
        // the Done-time call below stays as the retry for a failed
        // generation).
        if let Some(titles) = self.inner.titles.get() {
            titles.maybe_generate(chat_id, harness_id, &request.prompt, &request.cwd);
        }

        tokio::spawn(drive_run(
            self.inner.clone(),
            chat_id.to_string(),
            run_id.clone(),
            harness,
            request,
            frozen_request,
            handle.doc_arc(),
            controls,
            engine_rx,
            cancel_rx,
            hold_recovery_on_interrupt,
            RunResumeState {
                user_message_id: user_id,
                recovery,
            },
            run_permit,
        ));
        Ok(run_id)
    }

    /// Run a verified harness-native compaction operation for non-Codex
    /// adapters. Codex uses TaskIdentityService so its provider-home binding
    /// is validated before the short-lived native client starts.
    pub async fn compact_context(
        &self,
        harness_id: HarnessId,
        session_id: String,
        cwd: String,
        model: Option<String>,
    ) -> Result<(), EngineError> {
        let harness = self.inner.registry.resolve(harness_id)?;
        harness
            .compact_context(luna_harness::ContextCompactRequest {
                session_id,
                cwd,
                model,
            })
            .await?;
        Ok(())
    }

    /// Install the explicit no-resume tombstone in the in-memory route after
    /// an atomic clear/provider-switch boundary commits.
    pub fn retire_context_binding(&self, chat_id: &str, cwd: &str) {
        lock(&self.inner.harness_sessions).insert(
            chat_id.to_string(),
            HarnessSessionRef {
                session_id: String::new(),
                cwd: cwd.to_string(),
            },
        );
        lock(&self.inner.last_requests).remove(chat_id);
    }

    /// Push a steer prompt into the live run's mailbox. `NotSteerable` when no live
    /// steerable run exists — the caller (command executor) dispatches a new turn.
    pub async fn steer(
        &self,
        chat_id: &str,
        prompt: &str,
        message_id: Option<String>,
    ) -> Result<SteerOutcome, EngineError> {
        let revision = self.inner.recovery.workspace_repair_revision(chat_id)?;
        self.steer_at_workspace_revision(chat_id, prompt, message_id, revision)
            .await
    }

    pub(crate) async fn steer_at_workspace_revision(
        &self,
        chat_id: &str,
        prompt: &str,
        message_id: Option<String>,
        workspace_repair_revision: u64,
    ) -> Result<SteerOutcome, EngineError> {
        self.steer_with_inputs_at_workspace_revision(
            chat_id,
            prompt,
            &[],
            &[],
            message_id,
            workspace_repair_revision,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn steer_with_inputs_at_workspace_revision(
        &self,
        chat_id: &str,
        prompt: &str,
        attachments: &[String],
        input_items: &[luna_proto::ComposerInput],
        message_id: Option<String>,
        workspace_repair_revision: u64,
    ) -> Result<SteerOutcome, EngineError> {
        let _transition = self.transition_gate(chat_id).lock_owned().await;
        if self.inner.recovery.workspace_repair_revision(chat_id)? != workspace_repair_revision {
            return Err(EngineError::Other("Workspace association changed after steer admission; review and submit a new message.".into()));
        }
        // A Codex/Claude run handle stays warm between turns. Its mailbox is
        // still open, but RequireActiveTurn is invalid until another provider
        // turn starts. Let the caller dispatch a follow-up turn instead.
        let Some(active_turn_id) = self.active_turn_id(chat_id) else {
            return Ok(SteerOutcome::NotSteerable);
        };
        let target = lock(&self.inner.runs)
            .get(chat_id)
            .filter(|h| {
                h.steerable
                    && h.run_id == active_turn_id
                    && h.workspace_repair_revision == workspace_repair_revision
            })
            .map(|h| {
                (
                    h.runtime_config.harness_id,
                    h.run_id.clone(),
                    h.steer_tx.clone(),
                    h.routed_steers.clone(),
                )
            });
        let Some((harness_id, run_id, steer_tx, ledger)) = target else {
            return Ok(SteerOutcome::NotSteerable);
        };
        if (!attachments.is_empty() || !input_items.is_empty())
            && !crate::provider_inputs::steers_native_inputs(harness_id)
        {
            return Ok(SteerOutcome::Rejected);
        }
        crate::provider_inputs::admit(harness_id, attachments, input_items)?;
        let user_id = message_id.unwrap_or_else(new_id);
        self.inner
            .recovery
            .record_execution_workspace_repair_revision(
                chat_id,
                &user_id,
                workspace_repair_revision,
            )?;
        let (acceptance_tx, acceptance_rx) = tokio::sync::oneshot::channel();
        let message = SteerMessage {
            input_items: input_items.to_vec(),
            attachments: attachments.to_vec(),
            prompt: prompt.to_string(),
            message_id: Some(user_id.clone()),
            delivery: luna_harness::SteerDelivery::RequireActiveTurn,
            acceptance: Some(acceptance_tx),
            goal: None,
        };
        {
            // Serialize mailbox acceptance with confirmation and Done-time
            // inspection: a fast consumer must never outrun its ledger entry.
            // Codex confirms exact client IDs and can accept multiple pending
            // inputs. Legacy identity-free adapters remain single-flight.
            let mut pending = lock(&ledger);
            if harness_id != HarnessId::Codex && !pending.is_empty() {
                return Ok(SteerOutcome::Busy);
            }
            if steer_tx.try_send(message).is_err() {
                return Ok(SteerOutcome::Busy);
            }
            pending.push_back(RoutedSteer {
                attachments: attachments.to_vec(),
                input_items: input_items.to_vec(),
                prompt: prompt.to_string(),
                message_id: user_id.clone(),
                delivery: luna_harness::SteerDelivery::RequireActiveTurn,
            });
        }
        let acceptance =
            tokio::time::timeout(std::time::Duration::from_secs(5), acceptance_rx).await;
        match acceptance {
            Ok(Ok(SteerAcceptance::Accepted)) if self.is_live(chat_id, &run_id) => {
                Ok(SteerOutcome::Accepted)
            }
            Ok(Ok(SteerAcceptance::Rejected(_))) => {
                let mut ledger = lock(&ledger);
                ledger.retain(|steer| steer.message_id != user_id);
                Ok(SteerOutcome::Rejected)
            }
            Ok(Ok(SteerAcceptance::Unknown(_)))
            | Ok(Err(_))
            | Err(_)
            | Ok(Ok(SteerAcceptance::Accepted)) => Ok(SteerOutcome::Unknown),
        }
    }

    /// Interrupt the live run, if any. The run settles with a synthetic
    /// `Done{interrupted}` and its streaming entry stamped `aborted`; this waits
    /// (bounded) for that settlement so callers observe a consistent doc.
    pub async fn interrupt(&self, chat_id: &str) -> Result<bool, EngineError> {
        self.interrupt_with_recovery_hold(chat_id, true).await
    }

    pub async fn interrupt_for_stop(&self, chat_id: &str) -> Result<bool, EngineError> {
        self.interrupt_with_recovery_hold(chat_id, false).await
    }

    async fn interrupt_with_recovery_hold(
        &self,
        chat_id: &str,
        hold_recovery: bool,
    ) -> Result<bool, EngineError> {
        let target = lock(&self.inner.runs).get_mut(chat_id).map(|h| {
            h.provider_turn = None;
            h.set_provider_turn_readiness(ProviderTurnReadiness::Ended);
            (
                h.run_id.clone(),
                h.interrupt_token.clone(),
                h.cancel.clone(),
                h.pending_inputs.clone(),
                h.hold_recovery_on_interrupt.clone(),
            )
        });
        let Some((run_id, token, cancel, pending, hold_on_interrupt)) = target else {
            return Ok(false);
        };
        hold_on_interrupt.store(hold_recovery, Ordering::Release);
        // Unpark any blocked question FIRST (mirrors luna: harness teardown can await a
        // parked question callback — a run stuck on a question would deadlock the stop).
        let parked: Vec<_> = lock(&pending)
            .drain()
            .map(|(_, pending)| pending.resolver)
            .collect();
        for resolver in parked {
            let _ = resolver.send(Vec::new());
        }
        // Harness-level interrupt (protocol + child teardown) …
        token.cancel();
        // … plus the engine-side grace deadline in the run task, so a harness that
        // ignores its token still settles with a synthesized Done{interrupted}.
        let _ = cancel.send(true);
        // Bounded settle wait (the run task appends Done + stamps `aborted`).
        for _ in 0..500 {
            if !self.is_live(chat_id, &run_id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok(true)
    }

    /// Deliver an answer to a provider question flagged secret (Codex
    /// `isSecret`) in memory only: no session command, journal row, receipt,
    /// transcript part or log carries it. Only the in-process local UI calls
    /// this. Without a live resolver (the turn ended or LUNA restarted) the
    /// request is closed as cancelled and the answer is dropped; it is never
    /// replayed as a new turn.
    pub fn respond_secret_input(
        &self,
        chat_id: &str,
        request_id: &str,
        answers: Vec<UserInputAnswer>,
    ) -> SecretInputOutcome {
        {
            let runs = lock(&self.inner.runs);
            if let Some(handle) = runs.get(chat_id) {
                let live = lock(&handle.pending_inputs)
                    .get(request_id)
                    .map(|input| has_secret_question(&input.questions));
                match live {
                    Some(false) => return SecretInputOutcome::NotSecret,
                    Some(true) => {
                        let Some(pending) =
                            take_pending_ordinary_input(&handle.pending_inputs, request_id)
                        else {
                            return SecretInputOutcome::NotSecret;
                        };
                        let valid = answers.iter().all(|answer| {
                            pending.questions.iter().any(|q| q.id == answer.question_id)
                        });
                        let delivered = valid && pending.resolver.send(answers).is_ok();
                        let _ = handle.engine_tx.send(AgentEvent::InputResolved {
                            request_id: request_id.to_string(),
                        });
                        return if delivered {
                            SecretInputOutcome::Delivered
                        } else {
                            SecretInputOutcome::Closed
                        };
                    }
                    None => {}
                }
            }
        }
        drop(answers);
        // No live resolver: close the open projection without the answer.
        let Some(handle) = self
            .inner
            .doc_host()
            .and_then(|host| host.open(chat_id).ok())
        else {
            return SecretInputOutcome::Unavailable;
        };
        let open_secret = handle.doc().read_entries().is_ok_and(|entries| {
            entries.iter().any(|entry| {
                entry.parts.iter().any(|part| {
                    matches!(part, MessagePart::Input { request_id: id, questions, resolved: false, .. }
                        if id == request_id && has_secret_question(questions))
                })
            })
        });
        if !open_secret {
            return SecretInputOutcome::Unavailable;
        }
        match handle.doc().resolve_input(request_id) {
            Ok(_) => SecretInputOutcome::Closed,
            Err(error) => {
                tracing::warn!(chat = %chat_id, request = %request_id, %error,
                    "secret request close failed");
                SecretInputOutcome::Unavailable
            }
        }
    }

    /// Resolve a pending `request_input` question set. Returns `false` when no such
    /// request is pending (unknown id, or the run already settled).
    pub fn respond_input(
        &self,
        chat_id: &str,
        request_id: &str,
        answers: Vec<UserInputAnswer>,
    ) -> Result<bool, EngineError> {
        // Hold the run map through delivery so a replacement run cannot
        // receive responses belonging to the old provider binding.
        let runs = lock(&self.inner.runs);
        let Some(handle) = runs.get(chat_id) else {
            return Ok(false);
        };
        {
            let pending = lock(&handle.pending_inputs);
            let Some(input) = pending.get(request_id) else {
                return Ok(false);
            };
            // A secret answer never travels through this path: its callers
            // are durable commands and replicated panels.
            if has_secret_question(&input.questions) && has_answer_text(&answers) {
                return Err(EngineError::Other(
                    "Secret answers are sent from the LUNA window running this task and are never stored"
                        .into(),
                ));
            }
            if input.questions.iter().any(|q| q.mcp.is_some()) {
                if handle.interrupt_token.is_cancelled()
                    || *handle.cancel.subscribe().borrow()
                    || input.resolver.is_closed()
                    || input
                        .questions
                        .iter()
                        .filter_map(|q| q.mcp.as_ref())
                        .any(|m| now_ms() >= m.expires_at_ms)
                {
                    return Ok(false);
                }
                luna_proto::mcp_response(&input.questions, &answers)
                    .map_err(|message| EngineError::Other(message.into()))?;
            }
        }
        let Some(pending) = take_pending_ordinary_input(&handle.pending_inputs, request_id) else {
            return Ok(false);
        };
        let delivered = pending.resolver.send(answers).is_ok();
        let _ = handle.engine_tx.send(AgentEvent::InputResolved {
            request_id: request_id.to_string(),
        });
        Ok(delivered)
    }

    /// Return the URL only for a live exact request. It is not a durable
    /// command, does not accept the request, and is unavailable after restart.
    pub fn mcp_elicitation_url(
        &self,
        chat_id: &str,
        request_id: &str,
    ) -> Result<String, EngineError> {
        let runs = lock(&self.inner.runs);
        let handle = runs
            .get(chat_id)
            .ok_or_else(|| EngineError::Other("MCP request is no longer pending".into()))?;
        let pending = lock(&handle.pending_inputs);
        let input = pending
            .get(request_id)
            .ok_or_else(|| EngineError::Other("MCP request is no longer pending".into()))?;
        if !matches!(input.kind, PendingInputKind::Provider)
            || handle.interrupt_token.is_cancelled()
            || *handle.cancel.subscribe().borrow()
            || input.resolver.is_closed()
        {
            return Err(EngineError::Other(
                "MCP request is no longer pending".into(),
            ));
        }
        let meta = input
            .questions
            .iter()
            .filter_map(|q| q.mcp.as_ref())
            .find(|meta| matches!(meta.kind, luna_proto::McpInputKind::Url { .. }))
            .filter(|meta| now_ms() < meta.expires_at_ms)
            .ok_or_else(|| {
                EngineError::Other("MCP URL request expired or is unavailable".into())
            })?;
        meta.launch_url
            .clone()
            .ok_or_else(|| EngineError::Other("MCP URL request is unavailable".into()))
    }

    /// A live server-owned kind, independent of the serialized question's
    /// action/source hint. Used to keep generic command and recovery paths
    /// from consuming a future LUNA approval.
    pub(crate) fn is_luna_pending_approval(&self, chat_id: &str, request_id: &str) -> bool {
        let pending = lock(&self.inner.runs)
            .get(chat_id)
            .map(|handle| handle.pending_inputs.clone());
        pending.as_ref().is_some_and(|pending| {
            lock(pending).get(request_id).is_some_and(|item| {
                matches!(
                    &item.kind,
                    PendingInputKind::LunaCrossTask { .. }
                        | PendingInputKind::LunaProspectiveInterrupt { .. }
                        | PendingInputKind::LunaSend { .. }
                )
            })
        })
    }

    /// The only response path for a server-owned LUNA approval. The caller
    /// must be an in-process UI handle; this method is not exposed by RPC.
    pub fn respond_luna_local_ui_approval(
        &self,
        chat_id: &str,
        request_id: &str,
        decision: luna_proto::ApprovalDecision,
    ) -> LunaApprovalOutcome {
        let is_send = lock(&self.inner.runs).get(chat_id).is_some_and(|run| {
            lock(&run.pending_inputs)
                .get(request_id)
                .is_some_and(|item| matches!(item.kind, PendingInputKind::LunaSend { .. }))
        });
        if is_send {
            return self.respond_luna_send_local_ui_approval(chat_id, request_id, decision);
        }
        let live_binding = self.inner.current_codex_binding_key(chat_id);
        // Hold the run map through response delivery: a replacement cannot
        // accept an answer for a resolver owned by the previous run.
        let runs = lock(&self.inner.runs);
        let Some(handle) = runs.get(chat_id) else {
            return LunaApprovalOutcome::Stale;
        };
        let cancelling = *handle.cancel.subscribe().borrow();
        let (pending, expired) = {
            let mut pending_inputs = lock(&handle.pending_inputs);
            let Some(pending) = pending_inputs.get(request_id) else {
                return LunaApprovalOutcome::Stale;
            };
            // This synchronous Archive responder cannot consume a prospective
            // Interrupt; its private async path performs target revalidation.
            if !matches!(&pending.kind, PendingInputKind::LunaCrossTask { run_id: bound, .. } if bound == &handle.run_id)
                || pending.questions.len() != 1
            {
                return LunaApprovalOutcome::Stale;
            }
            let Some(approval) = pending.questions[0].approval.as_ref() else {
                return LunaApprovalOutcome::Stale;
            };
            if !approval.is_luna_cross_task() || approval.supports_task_allowance {
                return LunaApprovalOutcome::Stale;
            }
            let PendingInputKind::LunaCrossTask { proposal, .. } = &pending.kind else {
                return LunaApprovalOutcome::Stale;
            };
            let now = now_ms();
            if !proposal.matches_pending(chat_id, &handle.run_id, request_id, &pending.questions[0])
                || approval.provider_request_id != proposal.source_call_id
                || approval.turn_id.as_deref() != Some(proposal.source_turn_id.as_str())
                || decision == luna_proto::ApprovalDecision::AllowOnce
                    && approval.expires_at_ms > now
                    && !cancelling
                    && !handle.interrupt_token.is_cancelled()
                    && !proposal.matches_live_claim(handle, live_binding.as_ref())
            {
                return LunaApprovalOutcome::Stale;
            }
            if approval.expires_at_ms > now.saturating_add(60_000) {
                return LunaApprovalOutcome::Stale;
            }
            let expired = approval.expires_at_ms <= now;
            let pending = pending_inputs
                .remove(request_id)
                .expect("checked pending LUNA approval");
            (pending, expired)
        };
        if expired || cancelling || handle.interrupt_token.is_cancelled() {
            let _ = pending.resolver.send(Vec::new());
            let _ = handle.engine_tx.send(AgentEvent::InputResolved {
                request_id: request_id.to_string(),
            });
            return if expired {
                LunaApprovalOutcome::Expired
            } else {
                LunaApprovalOutcome::RunEnded
            };
        }
        let (label, outcome) = match decision {
            luna_proto::ApprovalDecision::AllowOnce => {
                ("Allow once", LunaApprovalOutcome::AllowedOnce)
            }
            luna_proto::ApprovalDecision::Cancel => ("Cancel", LunaApprovalOutcome::Denied),
            luna_proto::ApprovalDecision::Deny
            | luna_proto::ApprovalDecision::AllowTask
            | luna_proto::ApprovalDecision::ProviderChoice { .. } => {
                ("Deny", LunaApprovalOutcome::Denied)
            }
        };
        let PendingInput {
            kind,
            questions,
            resolver,
        } = pending;
        let PendingInputKind::LunaCrossTask {
            proposal, grant_tx, ..
        } = kind
        else {
            unreachable!("checked server-owned LUNA approval")
        };
        // A binding or turn can advance after the first lookup. The private
        // grant still needs a fresh live claim immediately before delivery.
        if outcome == LunaApprovalOutcome::AllowedOnce
            && !proposal.matches_live_claim(
                handle,
                self.inner.current_codex_binding_key(chat_id).as_ref(),
            )
        {
            let _ = resolver.send(Vec::new());
            let _ = handle.engine_tx.send(AgentEvent::InputResolved {
                request_id: request_id.to_string(),
            });
            return LunaApprovalOutcome::Stale;
        }
        if outcome == LunaApprovalOutcome::AllowedOnce && grant_tx.is_closed() {
            let _ = resolver.send(Vec::new());
            let _ = handle.engine_tx.send(AgentEvent::InputResolved {
                request_id: request_id.to_string(),
            });
            return LunaApprovalOutcome::RunEnded;
        }
        if handle
            .engine_tx
            .send(AgentEvent::InputResolved {
                request_id: request_id.to_string(),
            })
            .is_err()
        {
            let _ = resolver.send(Vec::new());
            return LunaApprovalOutcome::RunEnded;
        }
        if resolver
            .send(vec![UserInputAnswer {
                question_id: questions[0].id.clone(),
                labels: vec![label.into()],
            }])
            .is_err()
        {
            return LunaApprovalOutcome::RunEnded;
        }
        if outcome == LunaApprovalOutcome::AllowedOnce
            && grant_tx
                .send(LunaSyntheticArchiveGrant {
                    proposal: *proposal,
                })
                .is_err()
        {
            return LunaApprovalOutcome::RunEnded;
        }
        outcome
    }

    fn respond_luna_send_local_ui_approval(
        &self,
        chat_id: &str,
        request_id: &str,
        decision: luna_proto::ApprovalDecision,
    ) -> LunaApprovalOutcome {
        let binding = self.inner.current_codex_binding_key(chat_id);
        let runs = lock(&self.inner.runs);
        let Some(run) = runs.get(chat_id) else {
            return LunaApprovalOutcome::Stale;
        };
        let mut entries = lock(&run.pending_inputs);
        let Some(entry) = entries.get(request_id) else {
            return LunaApprovalOutcome::Stale;
        };
        let PendingInputKind::LunaSend {
            run_id,
            proposal,
            grant_tx,
        } = &entry.kind
        else {
            return LunaApprovalOutcome::Stale;
        };
        if run_id != &run.run_id
            || entry.questions.len() != 1
            || entry.questions[0] != proposal.question()
            || !proposal.claim.matches_live(run, binding.as_ref())
            || proposal.fence.source_task_id != chat_id
            || proposal.fence.source_run_id != run.run_id
            || grant_tx.is_closed()
            || entry.resolver.is_closed()
        {
            return LunaApprovalOutcome::Stale;
        }
        let expired = proposal.fence.expires_at_ms <= now_ms();
        let pending = entries.remove(request_id).expect("checked LUNA send input");
        drop(entries);
        let PendingInputKind::LunaSend {
            proposal, grant_tx, ..
        } = pending.kind
        else {
            unreachable!("checked LUNA send input")
        };
        let approved = decision == luna_proto::ApprovalDecision::AllowOnce && !expired;
        let outcome = if expired {
            LunaApprovalOutcome::Expired
        } else if approved {
            LunaApprovalOutcome::AllowedOnce
        } else {
            LunaApprovalOutcome::Denied
        };
        let _ = run.engine_tx.send(AgentEvent::InputResolved {
            request_id: request_id.to_owned(),
        });
        let _ = pending.resolver.send(vec![UserInputAnswer {
            question_id: pending.questions[0].id.clone(),
            labels: vec![if approved { "Allow once" } else { "Deny" }.into()],
        }]);
        if approved && grant_tx.send(proposal.fence).is_err() {
            return LunaApprovalOutcome::RunEnded;
        }
        outcome
    }

    pub fn respond_approval(
        &self,
        chat_id: &str,
        request_id: &str,
        decision: luna_proto::ApprovalDecision,
    ) -> Result<bool, EngineError> {
        let target = lock(&self.inner.runs)
            .get(chat_id)
            .map(|handle| (handle.pending_inputs.clone(), handle.engine_tx.clone()));
        let Some((pending_inputs, engine_tx)) = target else {
            return Ok(false);
        };
        let (pending, answer) = {
            let mut pending_inputs = lock(&pending_inputs);
            let Some(pending) = pending_inputs.get(request_id) else {
                return Ok(false);
            };
            if !matches!(&pending.kind, PendingInputKind::Provider) || pending.resolver.is_closed()
            {
                return Ok(false);
            }
            let Some(question) = pending.questions.iter().find(|question| {
                question
                    .approval
                    .as_ref()
                    .is_some_and(|approval| !approval.is_luna_cross_task())
            }) else {
                // A malformed approval response must not consume an ordinary
                // user question with the same request id.
                return Ok(false);
            };
            let Some(answer) = approval_answer(question, decision) else {
                return Ok(false);
            };
            let Some(pending) = pending_inputs.remove(request_id) else {
                return Ok(false);
            };
            (pending, answer)
        };
        let delivered = pending.resolver.send(vec![answer]).is_ok();
        let _ = engine_tx.send(AgentEvent::InputResolved {
            request_id: request_id.to_string(),
        });
        Ok(delivered)
    }

    fn automatic_retries_enabled(&self) -> bool {
        let Some(path) = lock(&self.inner.recovery_settings_path).clone() else {
            return true;
        };
        std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| {
                value
                    .get("automaticRetriesEnabled")
                    .and_then(|value| value.as_bool())
            })
            .unwrap_or(true)
    }

    fn safe_recovery_continuation(&self, harness: HarnessId, session_id: Option<&str>) -> bool {
        session_id.is_some()
            && self
                .inner
                .registry
                .resolve(harness)
                .is_ok_and(|harness| harness.supports_safe_recovery_continuation())
    }

    #[allow(clippy::too_many_arguments)]
    fn record_recovery_failure(
        &self,
        chat_id: &str,
        harness: HarnessId,
        request: RunRequest,
        message_id: String,
        acceptance: RecoveryAcceptance,
        failure: RecoveryFailure,
        detail: String,
        provider_session_id: Option<String>,
        attempt: Option<&RecoveryAttemptContext>,
    ) -> Result<Option<TaskRecovery>, EngineError> {
        // The side-chat admission ledger deliberately has no automatic replay
        // or recovery continuation. Its transcript retains the actual failure.
        if self
            .inner
            .side_chat_lookup
            .get()
            .is_some_and(|lookup| lookup(chat_id))
        {
            return Ok(None);
        }
        let now = now_ms();
        let existing = if let Some(attempt) = attempt {
            let Some(record) = self.inner.recovery.latest(chat_id)? else {
                return Ok(None);
            };
            if record.view.chain_id != attempt.chain_id
                || record.view.attempt_count != attempt.attempt_count
                || record.view.status != RecoveryStatus::Running
            {
                return Ok(Some(record.view));
            }
            Some(record)
        } else {
            None
        };
        let original_acceptance = existing
            .as_ref()
            .map(|record| record.original_acceptance)
            .unwrap_or(acceptance);
        let effective_acceptance = if existing.is_some() {
            match acceptance {
                RecoveryAcceptance::Unknown
                    if existing
                        .as_ref()
                        .and_then(|record| record.attempt_acceptance)
                        == Some(RecoveryAcceptance::Accepted) =>
                {
                    RecoveryAcceptance::Accepted
                }
                RecoveryAcceptance::Unknown => RecoveryAcceptance::Unknown,
                RecoveryAcceptance::Accepted => RecoveryAcceptance::Accepted,
                RecoveryAcceptance::RejectedBeforeAcceptance
                    if original_acceptance == RecoveryAcceptance::Accepted =>
                {
                    RecoveryAcceptance::Accepted
                }
                RecoveryAcceptance::RejectedBeforeAcceptance => {
                    RecoveryAcceptance::RejectedBeforeAcceptance
                }
            }
        } else {
            acceptance
        };
        let chain_id = existing
            .as_ref()
            .map(|record| record.view.chain_id.clone())
            .unwrap_or_else(new_id);
        let created_at_ms = existing
            .as_ref()
            .map(|record| record.view.created_at_ms)
            .unwrap_or(now);
        let attempt_count = existing
            .as_ref()
            .map(|record| record.view.attempt_count)
            .or_else(|| attempt.map(|attempt| attempt.attempt_count))
            .unwrap_or(0);
        let receipt = if existing.is_none() {
            self.inner
                .doc_host()
                .map(|host| host.task_command_receipt_for_correlation(chat_id, &message_id))
                .transpose()?
                .flatten()
        } else {
            None
        };
        let binding = existing
            .as_ref()
            .map(|record| {
                (
                    record.view.environment_id.clone(),
                    record.view.binding_id.clone(),
                    record.view.binding_generation,
                )
            })
            .or_else(|| {
                self.inner
                    .task_identity
                    .get()
                    .and_then(|identity| identity.view(chat_id))
                    .and_then(|view| view.current_binding)
                    .map(|binding| {
                        (
                            binding.environment_id,
                            Some(binding.binding_id),
                            Some(binding.generation),
                        )
                    })
            });
        let provider_session_id = provider_session_id.or_else(|| {
            existing
                .as_ref()
                .and_then(|record| record.provider_session_id.clone())
        });
        let safe_continuation =
            self.safe_recovery_continuation(harness, provider_session_id.as_deref());
        let jitter_seed = chain_id.bytes().fold(attempt_count as u64, |state, byte| {
            state.wrapping_mul(131).wrapping_add(byte as u64)
        });
        let original_w = if let Some(record) = existing.as_ref() {
            record.workspace_repair_revision
        } else {
            self.inner
                .recovery
                .execution_workspace_repair_revision(chat_id, &message_id)?
        };
        let mut decision = recovery_decision(
            effective_acceptance,
            &failure,
            now,
            created_at_ms,
            attempt_count,
            self.automatic_retries_enabled(),
            safe_continuation,
            jitter_seed,
        );
        if original_w != self.inner.recovery.workspace_repair_revision(chat_id)? {
            decision.status = RecoveryStatus::Held;
            decision.action = luna_proto::RecoveryAction::ObserveOnly;
            decision.automatic = false;
            decision.retry_at_ms = None;
            decision.detail = "Workspace association changed. Review the preserved request and use a new explicit message.".into();
        }
        let view = TaskRecovery {
            chain_id: chain_id.clone(),
            task_id: chat_id.to_string(),
            original_message_id: existing
                .as_ref()
                .map(|record| record.view.original_message_id.clone())
                .unwrap_or(message_id),
            original_command_id: existing
                .as_ref()
                .and_then(|record| record.view.original_command_id.clone())
                .or_else(|| receipt.as_ref().map(|receipt| receipt.command_id.clone())),
            original_acceptance,
            acceptance: effective_acceptance,
            failure,
            action: decision.action,
            status: decision.status,
            attempt_count,
            max_attempts: MAX_AUTO_RETRIES,
            automatic: decision.automatic,
            retry_at_ms: decision.retry_at_ms,
            automatic_deadline_ms: created_at_ms.saturating_add(MAX_AUTO_WAIT_MS),
            environment_id: binding
                .as_ref()
                .map(|binding| binding.0.clone())
                .unwrap_or_else(|| self.inner.device_id.clone()),
            binding_id: binding.as_ref().and_then(|binding| binding.1.clone()),
            binding_generation: binding.as_ref().and_then(|binding| binding.2),
            detail: if detail.trim().is_empty() {
                decision.detail
            } else {
                format!("{} {}", decision.detail, detail.trim())
            },
            created_at_ms,
            updated_at_ms: now,
        };
        let record = RecoveryRecord {
            workspace_repair_revision: original_w,
            view: view.clone(),
            harness: existing
                .as_ref()
                .map(|record| record.harness)
                .unwrap_or(harness),
            request: existing
                .as_ref()
                .map(|record| record.request.clone())
                .unwrap_or(request),
            original_acceptance,
            attempt_acceptance: attempt.map(|_| acceptance),
            attempt_started_at_ms: existing
                .as_ref()
                .and_then(|record| record.attempt_started_at_ms),
            attempt_automatic: existing
                .as_ref()
                .is_some_and(|record| record.attempt_automatic),
            provider_session_id,
        };
        let record = if let Some(existing) = existing {
            let change = self.inner.recovery.prepare_change(existing, record.clone());
            if self.inner.recovery.apply_change(&change)? {
                record
            } else {
                return Ok(self
                    .inner
                    .recovery
                    .latest(chat_id)?
                    .map(|record| record.view));
            }
        } else {
            self.inner.recovery.put(record)?
        };
        self.inner
            .publish_recovery(chat_id, Some(record.view.clone()));
        if record.view.status == RecoveryStatus::Scheduled {
            self.arm_recovery(record);
        }
        Ok(Some(view))
    }

    fn arm_recovery(&self, record: RecoveryRecord) {
        match self
            .inner
            .recovery
            .workspace_repair_revision(&record.view.task_id)
        {
            Ok(current) if current != record.workspace_repair_revision => {
                let _ = self.hold_recovery(&record.view.task_id,
                    "Workspace association changed; review the preserved request before a new explicit send.");
                return;
            }
            Err(error) => {
                tracing::warn!(task = %record.view.task_id, %error, "recovery workspace authority unavailable");
                return;
            }
            _ => {}
        }
        let Some(retry_at_ms) = record.view.retry_at_ms else {
            return;
        };
        if let Some(previous) = lock(&self.inner.recovery_tasks).remove(&record.view.task_id) {
            previous.abort();
        }
        let engine = self.clone();
        let task_id = record.view.task_id.clone();
        let chain_id = record.view.chain_id.clone();
        let attempt_count = record.view.attempt_count;
        let task_key = task_id.clone();
        let handle = tokio::spawn(async move {
            let wait_ms = retry_at_ms.saturating_sub(now_ms()).max(0) as u64;
            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
            lock(&engine.inner.recovery_tasks).remove(&task_key);
            if let Err(error) = engine
                .execute_automatic_recovery(&task_id, &chain_id, attempt_count, retry_at_ms)
                .await
            {
                tracing::warn!(task = %task_id, chain = %chain_id, %error, "automatic recovery failed");
            }
        });
        lock(&self.inner.recovery_tasks).insert(record.view.task_id, handle);
    }

    async fn execute_automatic_recovery(
        &self,
        task_id: &str,
        chain_id: &str,
        expected_attempt_count: u8,
        expected_retry_at_ms: i64,
    ) -> Result<(), EngineError> {
        let Some(host) = self.inner.doc_host() else {
            return Ok(());
        };
        let _lane = host.task_command_gate(task_id).await;
        let Some(current) = self.inner.recovery.latest(task_id)? else {
            return Ok(());
        };
        if current.view.chain_id != chain_id
            || current.view.attempt_count != expected_attempt_count
            || current.view.status != RecoveryStatus::Scheduled
            || !current.view.automatic
            || current.view.retry_at_ms != Some(expected_retry_at_ms)
        {
            return Ok(());
        }
        let now = now_ms();
        if now < expected_retry_at_ms {
            self.arm_recovery(current);
            return Ok(());
        }
        let mut next = current.clone();
        let dispatch = if !self.automatic_retries_enabled() {
            next.view.status = RecoveryStatus::Held;
            next.view.automatic = false;
            next.view.retry_at_ms = None;
            next.view.detail = "Automatic retries were disabled in Settings. Review and use Send now if recovery is still needed.".into();
            false
        } else if now > current.view.automatic_deadline_ms {
            next.view.status = RecoveryStatus::RecoveryNeeded;
            next.view.automatic = false;
            next.view.retry_at_ms = None;
            next.view.detail = "The initial six-hour automatic recovery window expired before dispatch. Partial output and completed effects remain preserved.".into();
            false
        } else if current.view.attempt_count >= current.view.max_attempts
            || current.view.attempt_count >= MAX_AUTO_RETRIES
        {
            next.view.status = RecoveryStatus::Exhausted;
            next.view.automatic = false;
            next.view.retry_at_ms = None;
            next.view.detail =
                "The four-attempt automatic retry limit was reached before dispatch.".into();
            false
        } else {
            next.view.status = RecoveryStatus::Running;
            next.view.automatic = false;
            next.view.retry_at_ms = None;
            next.view.attempt_count = next.view.attempt_count.saturating_add(1);
            next.attempt_acceptance = None;
            next.attempt_started_at_ms = Some(now);
            next.attempt_automatic = true;
            true
        };
        next.view.updated_at_ms = now;
        let change = self.inner.recovery.prepare_change(current, next);
        if !self.inner.recovery.apply_change(&change)? {
            return Ok(());
        }
        self.publish_committed_recovery(&change.next);
        if dispatch {
            self.execute_claimed_recovery(change.next).await?;
        }
        Ok(())
    }

    pub(crate) fn prepare_recovery_attempt(
        &self,
        task_id: &str,
        chain_id: &str,
    ) -> Result<Option<RecoveryChange>, EngineError> {
        let Some(current) = self.inner.recovery.latest(task_id)? else {
            return Ok(None);
        };
        if current.workspace_repair_revision
            != self.inner.recovery.workspace_repair_revision(task_id)?
        {
            return Err(EngineError::Other("Recovery belongs to an earlier workspace association; review and send a new message.".into()));
        }
        if current.view.chain_id != chain_id
            || !matches!(
                current.view.status,
                RecoveryStatus::Scheduled
                    | RecoveryStatus::Held
                    | RecoveryStatus::RecoveryNeeded
                    | RecoveryStatus::WaitingForSignIn
            )
        {
            return Ok(None);
        }
        if matches!(
            current.view.action,
            luna_proto::RecoveryAction::ObserveOnly | luna_proto::RecoveryAction::FixConfiguration
        ) {
            return Ok(None);
        }
        let mut next = current.clone();
        next.view.status = RecoveryStatus::Running;
        next.view.automatic = false;
        next.view.retry_at_ms = None;
        next.view.attempt_count = next.view.attempt_count.saturating_add(1);
        next.view.updated_at_ms = now_ms();
        next.attempt_acceptance = None;
        next.attempt_started_at_ms = Some(next.view.updated_at_ms);
        next.attempt_automatic = false;
        Ok(Some(self.inner.recovery.prepare_change(current, next)))
    }

    pub(crate) fn publish_committed_recovery(&self, record: &RecoveryRecord) {
        if let Some(task) = lock(&self.inner.recovery_tasks).remove(&record.view.task_id) {
            task.abort();
        }
        self.inner
            .publish_recovery(&record.view.task_id, Some(record.view.clone()));
    }

    pub(crate) async fn execute_claimed_recovery(
        &self,
        record: RecoveryRecord,
    ) -> Result<(), EngineError> {
        let task_id = record.view.task_id.as_str();
        let chain_id = record.view.chain_id.as_str();
        if record.workspace_repair_revision
            != self.inner.recovery.workspace_repair_revision(task_id)?
        {
            return self.hold_recovery(
                task_id,
                "Workspace association changed; the original recovery was not dispatched.",
            );
        }
        let current = self.inner.recovery.latest(task_id)?;
        if !current.as_ref().is_some_and(|current| {
            current.view.chain_id == chain_id
                && current.view.attempt_count == record.view.attempt_count
                && current.view.status == RecoveryStatus::Running
        }) {
            return Ok(());
        }
        if self.has_live_run(task_id) {
            return self.hold_recovery(task_id, "Held while another turn is active.");
        }
        if let Some(binding) = self
            .inner
            .task_identity
            .get()
            .and_then(|identity| identity.view(task_id))
            .and_then(|view| view.current_binding)
            && (record.view.binding_id.as_deref() != Some(binding.binding_id.as_str())
                || record.view.binding_generation != Some(binding.generation))
        {
            return self.supersede_recovery(
                task_id,
                "Recovery cancelled because the provider binding changed.",
            );
        }
        if let Some(chat) = self
            .inner
            .workspace()
            .and_then(|workspace| workspace.chat(task_id).ok().flatten())
        {
            let config_changed = chat.config.as_ref().is_some_and(|config| {
                config.harness != record.harness
                    || config.provider_account_id != record.request.provider_account_id
                    || config.model != record.request.model
                    || config.reasoning != record.request.reasoning
                    || config.model_options != record.request.model_options
                    || config.sandbox != record.request.sandbox
                    || config.permission_mode != record.request.permission_mode
            });
            let cwd_changed = chat
                .cwd
                .as_deref()
                .is_some_and(|cwd| expand_home(cwd) != expand_home(&record.request.cwd));
            if config_changed || cwd_changed {
                return self.supersede_recovery(
                    task_id,
                    "Recovery cancelled because task configuration or working directory changed before dispatch.",
                );
            }
        }
        let mut request = record.request.clone();
        if matches!(
            record.view.action,
            luna_proto::RecoveryAction::ContinueSession
                | luna_proto::RecoveryAction::SignInThenContinue
        ) && record.view.acceptance == RecoveryAcceptance::Accepted
        {
            let Some(session_id) = record.provider_session_id.clone() else {
                return self.hold_recovery(
                    task_id,
                    "Recovery needs the original provider session, but no verified session identity is available.",
                );
            };
            if !self.safe_recovery_continuation(record.harness, Some(&session_id)) {
                return self.hold_recovery(
                    task_id,
                    "Recovery needs a same-session continuation contract that this adapter does not provide.",
                );
            }
            request.review = None;
            request.goal = None;
            request.prompt = "Continue from the preserved provider session. Do not repeat completed tool or external effects; first inspect the current state and resume only unfinished work.".into();
            request.attachments.clear();
            request.input_items.clear();
            request.resume = Some(session_id);
        }
        let context = RecoveryAttemptContext {
            chain_id: record.view.chain_id.clone(),
            attempt_count: record.view.attempt_count,
            automatic: record.attempt_automatic,
        };
        if let Err(error) = self
            .dispatch_with_context(
                task_id,
                record.harness,
                request.clone(),
                Some(record.view.original_message_id.clone()),
                Some(context.clone()),
                record.workspace_repair_revision,
            )
            .await
        {
            let still_current = self.inner.recovery.latest(task_id)?.is_some_and(|latest| {
                latest.view.chain_id == context.chain_id
                    && latest.view.attempt_count == context.attempt_count
                    && latest.view.status == RecoveryStatus::Running
            });
            if !still_current {
                return Ok(());
            }
            self.record_recovery_failure(
                task_id,
                record.harness,
                record.request,
                record.view.original_message_id,
                match &error {
                    EngineError::Harness(HarnessError::RejectedBeforeAcceptance(_)) => {
                        RecoveryAcceptance::RejectedBeforeAcceptance
                    }
                    _ => RecoveryAcceptance::Unknown,
                },
                classify_failure(&error.to_string()),
                error.to_string(),
                record.provider_session_id,
                Some(&context),
            )?;
        }
        Ok(())
    }

    pub(crate) async fn dispatch_prepared_recovery(
        &self,
        change: RecoveryChange,
    ) -> Result<(), EngineError> {
        self.publish_committed_recovery(&change.next);
        self.execute_claimed_recovery(change.next).await
    }

    pub(crate) fn prepare_cancel_recovery(
        &self,
        task_id: &str,
        chain_id: &str,
    ) -> Result<Option<RecoveryChange>, EngineError> {
        let Some(current) = self.inner.recovery.latest(task_id)? else {
            return Ok(None);
        };
        if current.view.chain_id != chain_id
            || !current.view.status.active()
            || current.view.status == RecoveryStatus::Running
        {
            return Ok(None);
        }
        let mut next = current.clone();
        next.view.status = RecoveryStatus::Cancelled;
        next.view.automatic = false;
        next.view.retry_at_ms = None;
        next.view.detail =
            "Automatic recovery cancelled. Partial output and completed effects were preserved."
                .into();
        next.view.updated_at_ms = now_ms();
        Ok(Some(self.inner.recovery.prepare_change(current, next)))
    }

    pub fn hold_recovery(&self, task_id: &str, detail: &str) -> Result<(), EngineError> {
        let Some(change) = self.prepare_hold_recovery(task_id, detail)? else {
            return Ok(());
        };
        if self.inner.recovery.apply_change(&change)? {
            self.publish_committed_recovery(&change.next);
        }
        Ok(())
    }

    pub(crate) fn prepare_hold_recovery(
        &self,
        task_id: &str,
        detail: &str,
    ) -> Result<Option<RecoveryChange>, EngineError> {
        let Some(current) = self.inner.recovery.current(task_id)? else {
            return Ok(None);
        };
        let mut next = current.clone();
        next.view.status = RecoveryStatus::Held;
        next.view.automatic = false;
        next.view.retry_at_ms = None;
        next.view.detail = detail.into();
        next.view.updated_at_ms = now_ms();
        Ok(Some(self.inner.recovery.prepare_change(current, next)))
    }

    pub fn supersede_recovery(&self, task_id: &str, detail: &str) -> Result<(), EngineError> {
        let Some(change) = self.prepare_supersede_recovery(task_id, detail)? else {
            return Ok(());
        };
        if self.inner.recovery.apply_change(&change)? {
            self.publish_committed_recovery(&change.next);
        }
        Ok(())
    }

    pub(crate) fn prepare_supersede_recovery(
        &self,
        task_id: &str,
        detail: &str,
    ) -> Result<Option<RecoveryChange>, EngineError> {
        let Some(current) = self.inner.recovery.current(task_id)? else {
            return Ok(None);
        };
        let mut next = current.clone();
        next.view.status = RecoveryStatus::Superseded;
        next.view.automatic = false;
        next.view.retry_at_ms = None;
        next.view.detail = detail.into();
        next.view.updated_at_ms = now_ms();
        Ok(Some(self.inner.recovery.prepare_change(current, next)))
    }

    fn mark_recovery_accepted(
        &self,
        task_id: &str,
        attempt: Option<&RecoveryAttemptContext>,
        session_id: &str,
    ) -> Result<(), EngineError> {
        let Some(attempt) = attempt else {
            return Ok(());
        };
        let Some(current) = self.inner.recovery.latest(task_id)? else {
            return Ok(());
        };
        if current.view.chain_id != attempt.chain_id
            || current.view.attempt_count != attempt.attempt_count
            || current.view.status != RecoveryStatus::Running
        {
            return Ok(());
        }
        let mut next = current.clone();
        next.view.acceptance = RecoveryAcceptance::Accepted;
        next.attempt_acceptance = Some(RecoveryAcceptance::Accepted);
        next.provider_session_id = Some(session_id.into());
        next.view.updated_at_ms = now_ms();
        let change = self.inner.recovery.prepare_change(current, next.clone());
        if self.inner.recovery.apply_change(&change)? {
            self.inner.publish_recovery(task_id, Some(next.view));
        }
        Ok(())
    }

    fn complete_recovery(
        &self,
        task_id: &str,
        attempt: Option<&RecoveryAttemptContext>,
    ) -> Result<(), EngineError> {
        let Some(attempt) = attempt else {
            return Ok(());
        };
        let Some(current) = self.inner.recovery.latest(task_id)? else {
            return Ok(());
        };
        if current.view.chain_id != attempt.chain_id
            || current.view.attempt_count != attempt.attempt_count
            || current.view.status != RecoveryStatus::Running
        {
            return Ok(());
        }
        let mut next = current.clone();
        next.view.status = RecoveryStatus::Completed;
        next.view.automatic = false;
        next.view.retry_at_ms = None;
        next.view.detail = "Recovery completed on the preserved request/session without replaying completed effects.".into();
        next.view.updated_at_ms = now_ms();
        let change = self.inner.recovery.prepare_change(current, next.clone());
        if self.inner.recovery.apply_change(&change)? {
            self.inner.publish_recovery(task_id, Some(next.view));
        }
        Ok(())
    }

    /// Boot recovery closes abandoned journals and creates an acceptance-aware
    /// chain from the frozen Run command, provider/session journal, L06 binding,
    /// and L07 receipt. It never replays the last transcript text blindly.
    pub fn recover_stale(&self) -> Result<usize, EngineError> {
        let permit = self.inner.journal.try_admit()?;
        let stale = self.inner.journal.stale_sessions_filtered(|id| {
            !self
                .inner
                .side_chat_lookup
                .get()
                .is_some_and(|lookup| lookup(id))
        })?;
        let mut recovered = 0usize;
        let mut reconciled_running = std::collections::HashSet::new();
        // Stop is authoritative even when the process died after the doc
        // command landed but before receipt/recovery finalization. Restore the
        // hold before any stale/terminal sweep can make the chain runnable.
        for record in self.inner.recovery.latest_records()? {
            if self
                .inner
                .side_chat_lookup
                .get()
                .is_some_and(|lookup| lookup(&record.view.task_id))
            {
                continue;
            }
            if !record.view.status.active() {
                continue;
            }
            let handle = self.doc_handle(&record.view.task_id)?;
            if frozen_run_for_message(&handle, &record.view.original_message_id)?
                .is_some_and(|run| run.stopped)
            {
                self.hold_recovery(
                    &record.view.task_id,
                    "Recovery held by Stop. Queued work and partial output were preserved.",
                )?;
            }
        }
        for chat_id in stale {
            if self
                .inner
                .side_chat_lookup
                .get()
                .is_some_and(|lookup| lookup(&chat_id))
            {
                continue;
            }
            if lock(&self.inner.runs).contains_key(&chat_id) {
                continue; // a live run owns this journal
            }
            let handle = self.doc_handle(&chat_id)?;
            let replay = self.inner.journal.replay(&chat_id, 0)?;
            let last_done = replay
                .iter()
                .rposition(|(_, event)| matches!(event, AgentEvent::Done { .. }));
            let run_events = replay.iter().skip(last_done.map_or(0, |index| index + 1));
            let accepted_session = run_events
                .filter_map(|(_, event)| match event {
                    AgentEvent::SessionStarted {
                        harness,
                        session_id,
                        ..
                    } => Some((*harness, session_id.clone())),
                    _ => None,
                })
                .next_back();
            if let Some((_, session_id)) = accepted_session.as_ref()
                && let Some((_, cwd)) = self.inner.journal_harness_session(&chat_id)
            {
                self.inner
                    .remember_harness_session(&chat_id, session_id, &cwd);
            }
            let message_id = latest_user_message_id(&handle)?;
            let note = "Run interrupted by engine restart — reconciling delivery";
            let done = AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: Some(note.into()),
                session_id: None,
            };
            self.inner.publish(&chat_id, &done, permit.as_ref())?;
            let stamped = handle.mark_abandoned_streams(note)?.len();
            self.set_status(&chat_id, SessionStatus::Idle, false);
            tracing::info!(chat = %chat_id, stamped, "reconciled stale session journal");
            recovered += 1;

            if let Some(record) = self.inner.recovery.latest(&chat_id)?
                && message_id.as_deref() == Some(record.view.original_message_id.as_str())
            {
                if record.view.status == RecoveryStatus::Running {
                    let attempt = RecoveryAttemptContext {
                        chain_id: record.view.chain_id.clone(),
                        attempt_count: record.view.attempt_count,
                        automatic: record.attempt_automatic,
                    };
                    let acceptance = if accepted_session.is_some() {
                        RecoveryAcceptance::Accepted
                    } else {
                        RecoveryAcceptance::Unknown
                    };
                    self.record_recovery_failure(
                        &chat_id,
                        record.harness,
                        record.request.clone(),
                        record.view.original_message_id.clone(),
                        acceptance,
                        RecoveryFailure::Interrupted,
                        note.into(),
                        accepted_session
                            .as_ref()
                            .map(|(_, session_id)| session_id.clone())
                            .or(record.provider_session_id.clone()),
                        Some(&attempt),
                    )?;
                    reconciled_running.insert(chat_id.clone());
                }
                // This exact original already has durable recovery ownership.
                continue;
            }

            let Some(message_id) = message_id else {
                continue;
            };
            let frozen = frozen_run_for_message(&handle, &message_id)?;
            let receipt = self
                .inner
                .doc_host()
                .map(|host| host.task_command_receipt_for_correlation(&chat_id, &message_id))
                .transpose()?
                .flatten();
            let acceptance = if accepted_session.is_some() {
                RecoveryAcceptance::Accepted
            } else if receipt
                .as_ref()
                .is_some_and(|receipt| receipt.status == CommandReceiptStatus::Rejected)
            {
                RecoveryAcceptance::RejectedBeforeAcceptance
            } else {
                RecoveryAcceptance::Unknown
            };
            let Some(frozen) = frozen else {
                tracing::warn!(chat = %chat_id, "recovery has no exact frozen Run command; refusing reconstruction");
                continue;
            };
            if frozen.stopped {
                continue;
            }
            let request = frozen.request;
            let Some(host) = self.inner.doc_host() else {
                continue;
            };
            let harness = accepted_session
                .as_ref()
                .map(|(harness, _)| *harness)
                .unwrap_or_else(|| host.harness_for_request(&chat_id, &request));
            let provider_session_id = accepted_session
                .as_ref()
                .map(|(_, session_id)| session_id.clone())
                .or_else(|| {
                    self.inner
                        .journal_harness_session(&chat_id)
                        .map(|(id, _)| id)
                });
            self.record_recovery_failure(
                &chat_id,
                harness,
                request,
                message_id,
                acceptance,
                RecoveryFailure::Interrupted,
                note.into(),
                provider_session_id,
                None,
            )?;
        }
        // A provider can durably append terminal Done and the process can die
        // before the recovery row is written. Such a journal is not "stale",
        // so inspect the latest local terminal segment and create recovery only
        // when no row owns that exact original message. An older terminal row
        // for the same task must never mask a newer failed Run command.
        if let Some(workspace) = self.inner.workspace() {
            for chat in workspace.read_chats()? {
                if self
                    .inner
                    .side_chat_lookup
                    .get()
                    .is_some_and(|lookup| lookup(&chat.id))
                {
                    continue;
                }
                if chat.device_id != self.inner.device_id
                    || lock(&self.inner.runs).contains_key(&chat.id)
                {
                    continue;
                }
                let replay = self.inner.journal.replay(&chat.id, 0)?;
                let Some(done_index) = replay
                    .iter()
                    .rposition(|(_, event)| matches!(event, AgentEvent::Done { .. }))
                else {
                    continue;
                };
                let AgentEvent::Done { status, error, .. } = &replay[done_index].1 else {
                    continue;
                };
                // A terminal Interrupted event is an authoritative engine/user
                // interruption (Stop, shutdown, or equivalent), not evidence
                // of a crash. Actual process loss is the non-terminal journal
                // path above, which appends its own synthetic Done only after
                // constructing fail-closed recovery.
                if matches!(status, DoneStatus::Completed | DoneStatus::Interrupted) {
                    continue;
                }
                let handle = self.doc_handle(&chat.id)?;
                let Some(message_id) = latest_user_message_id(&handle)? else {
                    continue;
                };
                if self
                    .inner
                    .recovery
                    .latest(&chat.id)?
                    .is_some_and(|record| record.view.original_message_id == message_id)
                {
                    continue;
                }
                let Some(frozen) = frozen_run_for_message(&handle, &message_id)? else {
                    tracing::warn!(chat = %chat.id, message = %message_id,
                        "terminal recovery has no exact frozen Run command; refusing reconstruction");
                    continue;
                };
                if frozen.stopped {
                    continue;
                }
                let FrozenRun {
                    command_id,
                    request,
                    stopped: _,
                } = frozen;
                let previous_done = replay[..done_index]
                    .iter()
                    .rposition(|(_, event)| matches!(event, AgentEvent::Done { .. }));
                let accepted_session = replay
                    .iter()
                    .take(done_index)
                    .skip(previous_done.map_or(0, |index| index + 1))
                    .filter_map(|(_, event)| match event {
                        AgentEvent::SessionStarted {
                            harness,
                            session_id,
                            ..
                        } => Some((*harness, session_id.clone())),
                        _ => None,
                    })
                    .next_back();
                let Some(host) = self.inner.doc_host() else {
                    continue;
                };
                let receipt = host
                    .task_command_receipt_for_correlation(&chat.id, &message_id)?
                    .filter(|receipt| receipt.command_id == command_id);
                // `inner.publish(SessionStarted)` appends+flushes the journal
                // before `confirm_task_command_correlation` can mark this exact
                // Run receipt Applied. Without both the command id match and
                // that Applied fence, the last terminal segment may belong to
                // an older execution and its session identity is unusable.
                let acceptance = match receipt.as_ref().map(|receipt| receipt.status) {
                    Some(CommandReceiptStatus::Applied) => RecoveryAcceptance::Accepted,
                    Some(CommandReceiptStatus::Rejected) => {
                        RecoveryAcceptance::RejectedBeforeAcceptance
                    }
                    _ => RecoveryAcceptance::Unknown,
                };
                let failure_detail = error
                    .clone()
                    .unwrap_or_else(|| "provider turn ended before recovery persisted".into());
                let failure = classify_failure(&failure_detail);
                let harness = if acceptance == RecoveryAcceptance::Accepted {
                    accepted_session
                        .as_ref()
                        .map(|(harness, _)| *harness)
                        .unwrap_or_else(|| host.harness_for_request(&chat.id, &request))
                } else {
                    host.harness_for_request(&chat.id, &request)
                };
                self.record_recovery_failure(
                    &chat.id,
                    harness,
                    request,
                    message_id,
                    acceptance,
                    failure,
                    failure_detail,
                    (acceptance == RecoveryAcceptance::Accepted)
                        .then(|| accepted_session.map(|(_, session_id)| session_id))
                        .flatten(),
                    None,
                )?;
            }
        }
        // A recovery claim is durable before provider dispatch. If the process
        // dies in that earliest window, the journal is already terminal from
        // the prior attempt and therefore is not returned by stale_sessions.
        // Reconcile every remaining Running row as delivery-unknown instead of
        // replaying it or minting a replacement chain/deadline.
        for record in self.inner.recovery.latest_records()? {
            if self
                .inner
                .side_chat_lookup
                .get()
                .is_some_and(|lookup| lookup(&record.view.task_id))
            {
                continue;
            }
            if record.view.status != RecoveryStatus::Running
                || reconciled_running.contains(&record.view.task_id)
            {
                continue;
            }
            let replay = self.inner.journal.replay(&record.view.task_id, 0)?;
            let last_done = replay
                .iter()
                .rposition(|(_, event)| matches!(event, AgentEvent::Done { .. }));
            let accepted_session = replay
                .iter()
                .skip(last_done.map_or(0, |index| index + 1))
                .filter_map(|(_, event)| match event {
                    AgentEvent::SessionStarted { session_id, .. } => Some(session_id.clone()),
                    _ => None,
                })
                .next_back();
            let attempt = RecoveryAttemptContext {
                chain_id: record.view.chain_id.clone(),
                attempt_count: record.view.attempt_count,
                automatic: record.attempt_automatic,
            };
            self.record_recovery_failure(
                &record.view.task_id,
                record.harness,
                record.request.clone(),
                record.view.original_message_id.clone(),
                if accepted_session.is_some() {
                    RecoveryAcceptance::Accepted
                } else {
                    RecoveryAcceptance::Unknown
                },
                RecoveryFailure::Interrupted,
                "Recovery attempt interrupted before delivery could be reconciled.".into(),
                accepted_session.or(record.provider_session_id.clone()),
                Some(&attempt),
            )?;
        }
        for record in self.inner.recovery.latest_records()? {
            if self
                .inner
                .side_chat_lookup
                .get()
                .is_some_and(|lookup| lookup(&record.view.task_id))
            {
                continue;
            }
            self.inner
                .publish_recovery(&record.view.task_id, Some(record.view.clone()));
            if record.view.status == RecoveryStatus::Scheduled {
                if !self.automatic_retries_enabled() {
                    self.hold_recovery(
                        &record.view.task_id,
                        "Automatic retries are disabled in Settings. Review and use Send now if recovery is still needed.",
                    )?;
                } else if now_ms() > record.view.automatic_deadline_ms {
                    if let Some(expired) = self.inner.recovery.update(
                        &record.view.task_id,
                        &record.view.chain_id,
                        |record| {
                            record.view.status = RecoveryStatus::RecoveryNeeded;
                            record.view.automatic = false;
                            record.view.retry_at_ms = None;
                            record.view.detail = "The initial six-hour automatic recovery window expired. Partial output and completed effects remain preserved.".into();
                            record.view.updated_at_ms = now_ms();
                        },
                    )? {
                        let task_id = expired.view.task_id.clone();
                        self.inner.publish_recovery(&task_id, Some(expired.view));
                    }
                } else {
                    self.arm_recovery(record);
                }
            }
        }
        Ok(recovered)
    }

    /// Graceful shutdown: interrupt every live run so streaming entries settle.
    pub async fn shutdown(&self) -> bool {
        let mut clean = true;
        for (_, timer) in lock(&self.inner.recovery_tasks).drain() {
            timer.abort();
        }
        // A dispatch admitted before shutdown can register its run after the
        // first sweep. Interrupt each run once, then wait (bounded) for
        // in-flight dispatches and sweep again until no new run appears.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut interrupted: HashSet<String> = HashSet::new();
        loop {
            let runs: Vec<(String, String)> = lock(&self.inner.runs)
                .iter()
                .map(|(chat, handle)| (chat.clone(), handle.run_id.clone()))
                .collect();
            let mut swept = false;
            for (chat_id, run_id) in runs {
                if !interrupted.insert(run_id) {
                    continue;
                }
                swept = true;
                if let Err(err) = self.interrupt(&chat_id).await {
                    tracing::warn!(chat = %chat_id, error = %err, "shutdown interrupt failed");
                    clean = false;
                }
            }
            let idle = self.inner.dispatches_in_flight.load(Ordering::Acquire) == 0;
            if idle && !swept {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!("shutdown left a run dispatch unsettled");
                clean = false;
                break;
            }
            if !idle {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        clean
    }

    fn is_live(&self, chat_id: &str, run_id: &str) -> bool {
        lock(&self.inner.runs)
            .get(chat_id)
            .is_some_and(|h| h.run_id == run_id)
    }

    fn set_status(&self, chat_id: &str, status: SessionStatus, fresh_start: bool) {
        self.inner.set_status(chat_id, status, fresh_start);
    }
}

fn approval_answer(
    question: &UserInputQuestion,
    decision: luna_proto::ApprovalDecision,
) -> Option<UserInputAnswer> {
    let approval = question.approval.as_ref()?;
    if approval.is_luna_cross_task() {
        return None;
    }
    let decision = match decision {
        _ if approval.expires_at_ms <= now_ms() => luna_proto::ApprovalDecision::Deny,
        luna_proto::ApprovalDecision::AllowTask if !approval.supports_task_allowance => {
            luna_proto::ApprovalDecision::Deny
        }
        decision => decision,
    };
    let label = if let Some(details) = &approval.details {
        let index = match decision {
            luna_proto::ApprovalDecision::ProviderChoice { index } => {
                usize::try_from(index).ok()?
            }
            // Old clients may still send these fixed decisions. They can only
            // select a stored exact once/session/deny/cancel choice.
            luna_proto::ApprovalDecision::AllowOnce => details
                .choices
                .iter()
                .position(|choice| choice.label == "Allow once")?,
            luna_proto::ApprovalDecision::AllowTask => details
                .choices
                .iter()
                .position(|choice| choice.label == "Allow for session")?,
            luna_proto::ApprovalDecision::Deny => details
                .choices
                .iter()
                .position(|choice| choice.label == "Deny")?,
            luna_proto::ApprovalDecision::Cancel => details
                .choices
                .iter()
                .position(|choice| choice.label == "Cancel turn")
                .or_else(|| {
                    details
                        .choices
                        .iter()
                        .position(|choice| choice.label == "Deny")
                })?,
        };
        let choice = details.choices.get(index)?;
        if question.options.get(index) != Some(&choice.label) {
            return None;
        }
        choice.label.as_str()
    } else {
        match decision {
            luna_proto::ApprovalDecision::AllowOnce => "Allow once",
            luna_proto::ApprovalDecision::AllowTask => "Allow for task",
            luna_proto::ApprovalDecision::Deny => "Deny",
            luna_proto::ApprovalDecision::Cancel => "Cancel",
            luna_proto::ApprovalDecision::ProviderChoice { .. } => return None,
        }
    };
    Some(UserInputAnswer {
        question_id: question.id.clone(),
        labels: vec![label.into()],
    })
}

impl Inner {
    /// Journal + broadcast one event (the two unconditional legs of the pipeline).
    /// Sanitize before ANY journal/publish path, including nested subagent events.
    async fn prepare_generated_image(
        &self,
        chat_id: &str,
        event: AgentEvent,
        seen: &mut std::collections::HashSet<String>,
    ) -> Vec<AgentEvent> {
        let mut parents = Vec::new();
        let mut leaf = event;
        while let AgentEvent::Subagent {
            parent_tool_use_id,
            event,
        } = leaf
        {
            parents.push(parent_tool_use_id);
            leaf = *event;
        }
        let events = if let AgentEvent::GeneratedImage { id, path, .. } = leaf {
            let key = std::iter::once(chat_id)
                .chain(parents.iter().map(String::as_str))
                .chain(std::iter::once(id.as_str()))
                .collect::<Vec<_>>()
                .join("\0");
            if seen.contains(&key) {
                return vec![];
            }
            let viewed = id.ends_with(":view");
            let result = if let Some((uploads, allowed_root)) = self.generated_images.get() {
                let uploads = uploads.clone();
                // Codex reports the exact local raster it inspected. Copy it
                // through the same race-checked intake, then persist only the
                // profile-owned copy so later edits cannot rewrite history.
                let root = if viewed {
                    std::path::Path::new(&path)
                        .parent()
                        .unwrap_or(allowed_root)
                        .to_path_buf()
                } else {
                    allowed_root.clone()
                };
                let stable_key = key.clone();
                tokio::task::spawn_blocking(move || {
                    uploads.import_generated_image(std::path::Path::new(&path), &root, &stable_key)
                })
                .await
                .unwrap_or_else(|err| Err(EngineError::Other(err.to_string())))
            } else {
                Err(EngineError::Other(
                    "Generated image intake is not configured".into(),
                ))
            };
            match result {
                Ok(image) => {
                    seen.insert(key);
                    vec![AgentEvent::GeneratedImage {
                        id,
                        path: image.path,
                        name: image.name,
                        mime_type: image.mime_type,
                    }]
                }
                Err(err) => {
                    tracing::warn!(chat = %chat_id, error = %err, "generated image import failed");
                    vec![
                        AgentEvent::ToolResult {
                            id: id
                                .strip_suffix(":image")
                                .or_else(|| id.strip_suffix(":view"))
                                .unwrap_or(&id)
                                .into(),
                            is_error: true,
                            output: None,
                            diff: None,
                        },
                        AgentEvent::Error {
                            message: if viewed {
                                "Viewed image unavailable"
                            } else {
                                "Generated image unavailable"
                            }
                            .into(),
                        },
                    ]
                }
            }
        } else {
            vec![leaf]
        };
        events
            .into_iter()
            .map(|mut event| {
                for parent in parents.iter().rev() {
                    event = AgentEvent::Subagent {
                        parent_tool_use_id: parent.clone(),
                        event: Box::new(event),
                    };
                }
                event
            })
            .collect()
    }

    fn publish(
        &self,
        chat_id: &str,
        event: &AgentEvent,
        permit: Option<&DeviceWritePermit>,
    ) -> Result<u64, EngineError> {
        let seq = self.journal.append_with_permit(chat_id, event, permit)?;
        if let Some(hub) = lock(&self.hubs).get(chat_id) {
            let _ = hub.send(JournaledEvent {
                seq,
                event: tool_event_preview(event),
            });
        }
        Ok(seq)
    }

    /// Bump the session's freshness on stream activity WITHOUT a status
    /// transition. Long silent-LOOKING stretches (thinking heartbeats, a big
    /// tool input being generated) still carry events — the UI's 45s
    /// staleness gate must not flip "Working" off mid-run. Throttled: a
    /// workspace-doc mirror per delta would be far too chatty.
    fn touch_session(&self, chat_id: &str) {
        const TOUCH_THROTTLE_MS: i64 = 10_000;
        let now = Utc::now();
        let session = {
            let mut statuses = lock(&self.statuses);
            let Some(entry) = statuses.get_mut(chat_id) else {
                return;
            };
            let age = now
                .signed_duration_since(entry.updated_at)
                .num_milliseconds();
            if age < TOUCH_THROTTLE_MS {
                return;
            }
            entry.updated_at = now;
            let session = entry.clone();
            let mut list: Vec<Session> = statuses.values().cloned().collect();
            list.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
            self.sessions_tx.send_replace(list);
            session
        };
        if let Some(ws) = self.workspace() {
            ws.record_session(&session);
        }
    }

    fn set_status(&self, chat_id: &str, status: SessionStatus, fresh_start: bool) {
        self.set_status_with_completion(chat_id, status, fresh_start, None);
    }

    fn has_pending_steers(&self, chat_id: &str, run_id: &str) -> bool {
        lock(&self.runs)
            .get(chat_id)
            .filter(|h| h.run_id == run_id)
            .is_some_and(|h| !lock(&h.routed_steers).is_empty())
    }

    /// Retire explicit native steers owned by the turn that just reached a
    /// terminal provider boundary. Ordinary warm routes stay in FIFO order
    /// for their legacy follow-up-turn fallback.
    fn take_terminal_explicit_steers(&self, chat_id: &str, run_id: &str) -> Vec<RoutedSteer> {
        let runs = lock(&self.runs);
        let Some(handle) = runs.get(chat_id).filter(|handle| handle.run_id == run_id) else {
            return Vec::new();
        };
        let mut ledger = lock(&handle.routed_steers);
        let entries = std::mem::take(&mut *ledger);
        let (ordinary, explicit): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|steer| steer.delivery == luna_harness::SteerDelivery::AllowFollowUpTurn);
        *ledger = ordinary.into();
        explicit
    }

    /// Publish the completion marker atomically with the settled status. Keep
    /// it on subsequent Working/heartbeat rows for coalesced and remote watches.
    fn set_status_with_completion(
        &self,
        chat_id: &str,
        status: SessionStatus,
        fresh_start: bool,
        completed_turn: Option<String>,
    ) {
        let now = Utc::now();
        let session = {
            let mut statuses = lock(&self.statuses);
            let entry = statuses
                .entry(chat_id.to_string())
                .or_insert_with(|| Session {
                    last_completed_turn: None,
                    chat_id: chat_id.to_string(),
                    device_id: self.device_id.clone(),
                    status,
                    started_at: None,
                    updated_at: now,
                    recovery: None,
                });
            // `started_at` is the elapsed-timer base and must only ever mean
            // "this turn". Entering Working from a settled state always
            // restamps (a steer into a parked session is a NEW turn — reusing
            // the old base showed the previous turn's 30min on send), and a
            // settled session drops its base entirely so no later reader can
            // resurrect a stale elapsed.
            let was_active = matches!(
                entry.status,
                SessionStatus::Working
                    | SessionStatus::AwaitingApproval
                    | SessionStatus::AwaitingInput
            );
            if let Some(turn) = completed_turn {
                entry.last_completed_turn = Some(turn);
            }
            entry.status = status;
            entry.updated_at = now;
            match status {
                SessionStatus::Working if fresh_start || !was_active => {
                    entry.started_at = Some(now);
                }
                SessionStatus::Working
                | SessionStatus::AwaitingApproval
                | SessionStatus::AwaitingInput => {}
                SessionStatus::Idle | SessionStatus::Errored => {
                    entry.started_at = None;
                }
            }
            let session = entry.clone();
            let mut list: Vec<Session> = statuses.values().cloned().collect();
            list.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
            // send_replace: keep the current value fresh even with no receivers,
            // so late WatchSessions subscribers see the last transition.
            self.sessions_tx.send_replace(list);
            session
        };
        // Mirror the transition into the workspace doc's session-status row so
        // remote devices' sidebars show this run (staleness-checked client-side).
        if let Some(ws) = self.workspace() {
            ws.record_session(&session);
        }
    }

    fn publish_recovery(&self, chat_id: &str, recovery: Option<TaskRecovery>) {
        let now = Utc::now();
        let session = {
            let mut statuses = lock(&self.statuses);
            let entry = statuses
                .entry(chat_id.to_string())
                .or_insert_with(|| Session {
                    last_completed_turn: None,
                    chat_id: chat_id.to_string(),
                    device_id: self.device_id.clone(),
                    status: SessionStatus::Idle,
                    started_at: None,
                    updated_at: now,
                    recovery: None,
                });
            entry.recovery = recovery;
            entry.updated_at = now;
            let session = entry.clone();
            let mut list: Vec<Session> = statuses.values().cloned().collect();
            list.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
            self.sessions_tx.send_replace(list);
            session
        };
        if let Some(workspace) = self.workspace() {
            workspace.record_session(&session);
        }
    }

    /// The doc host, once wired. `None` before assembly or after retirement.
    fn doc_host(&self) -> Option<DocHost> {
        lock(&self.doc_host).clone()
    }

    fn workspace(&self) -> Option<crate::workspace_host::WorkspaceHost> {
        self.doc_host().and_then(|host| host.workspace().cloned())
    }

    /// Sidebar freshness: push a message-persist preview into the chat's workspace row.
    fn note_message(&self, chat_id: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(ws) = self.workspace() {
            ws.note_message(chat_id, text);
        }
    }

    /// Record the chat's harness-native session id (and its cwd): live-process
    /// cache plus the durable workspace chat row — the row is what survives an
    /// engine restart (luna sessions.ts:1039).
    fn remember_harness_session(&self, chat_id: &str, session_id: &str, cwd: &str) {
        if session_id.is_empty() {
            return;
        }
        lock(&self.harness_sessions).insert(
            chat_id.to_string(),
            HarnessSessionRef {
                session_id: session_id.to_string(),
                cwd: cwd.to_string(),
            },
        );
        if let Some(ws) = self.workspace() {
            ws.set_chat_harness_session(chat_id, session_id, cwd);
        }
    }

    // NB: there is deliberately no `forget_harness_session` anymore. The old
    // tombstone fired on "run died before SessionStarted", which — since the
    // ACP conversion made stale ids a harness-internal fallback — only ever
    // meant a child STARTUP failure, and permanently severed good
    // conversations (user incident 2026-08-13). A truly stale id simply
    // yields a fresh session whose SessionStarted overwrites the row.

    /// The session id to resume for a run in `chat_id` launching from `cwd`
    /// (luna sessions.ts:736, looked up on every dispatch):
    /// live-process cache → workspace chat row → journal scan (the crash path
    /// where the debounced row write never landed — SessionStarted/Done events
    /// are journaled per event, flushed immediately). Cwd-gated throughout:
    /// harness session stores are keyed by cwd, so a session created elsewhere
    /// never rides `--resume`. An empty stored id is the explicit tombstone —
    /// no resume, no falling through to staler sources.
    fn resume_for(&self, chat_id: &str, cwd: &str) -> Option<String> {
        let cwd_ok = |session_cwd: &str| session_cwd.is_empty() || session_cwd == cwd;
        if let Some(known) = lock(&self.harness_sessions).get(chat_id).cloned() {
            return (!known.session_id.is_empty() && cwd_ok(&known.cwd))
                .then_some(known.session_id);
        }
        if let Some(ws) = self.workspace()
            && let Some((session_id, session_cwd)) = ws.chat_harness_session(chat_id)
        {
            return (!session_id.is_empty() && cwd_ok(session_cwd.as_deref().unwrap_or("")))
                .then_some(session_id);
        }
        let (session_id, session_cwd) = self.journal_harness_session(chat_id)?;
        // Cache the journal hit (memory + row) so later dispatches skip the scan.
        self.remember_harness_session(chat_id, &session_id, &session_cwd);
        cwd_ok(&session_cwd).then_some(session_id)
    }

    /// The last harness session id named anywhere in the chat's journal, with
    /// the cwd of the `SessionStarted` that governs it. `Done.session_id`
    /// inherits the cwd of the most recent `SessionStarted` (same run).
    fn journal_harness_session(&self, chat_id: &str) -> Option<(String, String)> {
        let events = match self.journal.replay(chat_id, 0) {
            Ok(events) => events,
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "journal scan for harness session failed");
                return None;
            }
        };
        let mut current_cwd = String::new();
        let mut found: Option<(String, String)> = None;
        for (_, event) in events {
            match event {
                AgentEvent::SessionStarted {
                    session_id, cwd, ..
                } => {
                    current_cwd = cwd;
                    if !session_id.is_empty() {
                        found = Some((session_id, current_cwd.clone()));
                    }
                }
                AgentEvent::Done {
                    session_id: Some(session_id),
                    ..
                } if !session_id.is_empty() => {
                    found = Some((session_id, current_cwd.clone()));
                }
                _ => {}
            }
        }
        found
    }

    fn remove_run(&self, chat_id: &str, run_id: &str) {
        let mut runs = lock(&self.runs);
        if runs.get(chat_id).is_some_and(|h| h.run_id == run_id) {
            runs.remove(chat_id);
        }
    }

    fn current_codex_binding_key(&self, chat_id: &str) -> Option<ProviderBindingKey> {
        let view = self.task_identity.get()?.view(chat_id)?;
        let binding = view.current_binding?;
        if view.task.tombstoned
            || view.workspace.environment_id != self.device_id
            || binding.task_id != chat_id
            || binding.environment_id != self.device_id
            || binding.provider_id != HarnessId::Codex
            || binding.binding_id.is_empty()
            || binding.provider_task_id.is_empty()
        {
            return None;
        }
        Some(ProviderBindingKey {
            task_id: chat_id.to_owned(),
            binding_id: binding.binding_id,
            generation: binding.generation,
            provider_thread_id: binding.provider_task_id,
        })
    }

    fn observe_codex_checkpoint(
        &self,
        chat_id: &str,
        run_id: &str,
        provider_thread_id: Option<&str>,
        provider_turn_id: &str,
    ) -> bool {
        let binding = self.current_codex_binding_key(chat_id);
        lock(&self.runs).get_mut(chat_id).is_some_and(|run| {
            run.observe_codex_checkpoint(
                chat_id,
                run_id,
                provider_thread_id,
                provider_turn_id,
                binding.as_ref(),
            )
        })
    }

    fn observe_codex_binding(&self, chat_id: &str, run_id: &str, provider_thread_id: &str) -> bool {
        let binding = self.current_codex_binding_key(chat_id);
        lock(&self.runs).get_mut(chat_id).is_some_and(|run| {
            run.observe_codex_binding(chat_id, run_id, provider_thread_id, binding.as_ref())
        })
    }

    fn reset_codex_checkpoint(&self, chat_id: &str, run_id: &str) {
        if let Some(run) = lock(&self.runs)
            .get_mut(chat_id)
            .filter(|run| run.run_id == run_id)
        {
            run.provider_turn = None;
            run.run_provider_binding = None;
            run.set_provider_turn_readiness(ProviderTurnReadiness::AwaitingBinding);
        }
    }

    fn rearm_codex_checkpoint_after_done(
        &self,
        chat_id: &str,
        run_id: &str,
        provider_thread_id: Option<&str>,
    ) {
        let binding = self.current_codex_binding_key(chat_id);
        if let Some(run) = lock(&self.runs)
            .get_mut(chat_id)
            .filter(|run| run.run_id == run_id)
            && *run.provider_turn_readiness.borrow() == ProviderTurnReadiness::Ended
        {
            run.rearm_codex_checkpoint(chat_id, run_id, provider_thread_id, binding.as_ref());
        }
    }

    fn clear_codex_checkpoint(&self, chat_id: &str, run_id: &str) {
        if let Some(run) = lock(&self.runs)
            .get_mut(chat_id)
            .filter(|run| run.run_id == run_id)
        {
            run.provider_turn = None;
            run.set_provider_turn_readiness(ProviderTurnReadiness::Ended);
        }
    }

    fn fail_codex_checkpoint(&self, chat_id: &str, run_id: &str) {
        if let Some(run) = lock(&self.runs)
            .get_mut(chat_id)
            .filter(|run| run.run_id == run_id)
        {
            run.provider_turn = None;
            run.run_provider_binding = None;
            run.set_provider_turn_readiness(ProviderTurnReadiness::Unavailable);
        }
    }
}

// ── run task ────────────────────────────────────────────────────────────────

/// A turn is in flight: streaming, or parked on a question it is still owed an
/// answer to. Notably NOT a persistent session that has parked between turns —
/// that holds a warm child with nothing outstanding.
fn is_active(session: &Session) -> bool {
    matches!(
        session.status,
        SessionStatus::Working | SessionStatus::AwaitingApproval | SessionStatus::AwaitingInput
    )
}

// ── subagent docs ───────────────────────────────────────────────────────────

/// The per-subagent doc id: `{chatId}--sub--{suffix}`. Constrained by the
/// edge's `ID_RE` (`^[A-Za-z0-9_-]{1,128}$` — the same id names the ChatRoom
/// `chat2/{id}/ws` and the frozen blob `blob/{chatId}/{id}`): a clean, short
/// tool-use id rides verbatim; anything unclean or over budget hashes.
pub(crate) fn subagent_doc_id(chat_id: &str, tool_use_id: &str) -> String {
    let clean = tool_use_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let budget = 128usize.saturating_sub(chat_id.len() + "--sub--".len());
    if clean && !tool_use_id.is_empty() && tool_use_id.len() <= budget {
        return format!("{chat_id}--sub--{tool_use_id}");
    }
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(tool_use_id.as_bytes());
    let mut hex = String::with_capacity(16);
    for b in &digest[..8] {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    format!("{chat_id}--sub--{hex}")
}

/// A live subagent transcript sink: its own doc (opened by id — the room
/// `chat2/{docId}/ws` dials automatically, so viewers sync it like a chat),
/// one streaming assistant entry folded from the tagged events. The held
/// doc Arc pins the doc warm for the LRU while the subagent runs.
struct SubagentSink {
    doc_id: String,
    doc: Arc<SessionDoc>,
    entry_id: String,
    started_at: i64,
    entry_index: Option<usize>,
    written: Vec<MessagePart>,
    folded: Vec<MessagePart>,
    dirty: bool,
}

impl SubagentSink {
    fn flush(&mut self, device_id: &str) {
        if !self.dirty || self.folded.is_empty() {
            return;
        }
        let rendered = render_parts(&self.folded);
        let result = match self.entry_index {
            Some(ix) => {
                let mut w = SegmentWriter::resume(&self.doc, ix, std::mem::take(&mut self.written));
                let r = w.sync(&rendered);
                let (ix, written) = w.into_state();
                self.entry_index = Some(ix);
                self.written = written;
                r
            }
            None => {
                match SegmentWriter::begin(&self.doc, &self.entry_id, device_id, self.started_at) {
                    Ok(mut w) => {
                        let r = w.sync(&rendered);
                        let (ix, written) = w.into_state();
                        self.entry_index = Some(ix);
                        self.written = written;
                        r
                    }
                    Err(e) => Err(e),
                }
            }
        };
        if let Err(err) = result {
            // Fail soft: a broken subagent doc degrades to chip-only, never
            // errors the chat.
            tracing::warn!(doc = %self.doc_id, error = %err, "subagent sink flush failed");
        }
        self.dirty = false;
    }

    /// A parent→subagent steer ([`AgentEvent::UserMessage`], tagged): close
    /// the open assistant segment (it reads Complete above the steer), write
    /// the steer as its own USER entry, and reset so the next tagged delta
    /// opens a fresh assistant entry below it — the subagent transcript then
    /// reads like any steered chat.
    fn push_user(&mut self, device_id: &str, text: &str) {
        let rendered = render_parts(&self.folded);
        let closed = match self.entry_index.take() {
            Some(ix) => SegmentWriter::resume(&self.doc, ix, std::mem::take(&mut self.written))
                .finish(&rendered, MessageStatus::Complete),
            None if !self.folded.is_empty() => {
                match SegmentWriter::begin(&self.doc, &self.entry_id, device_id, self.started_at) {
                    Ok(w) => w.finish(&rendered, MessageStatus::Complete),
                    Err(e) => Err(e),
                }
            }
            None => Ok(()),
        };
        if let Err(err) = closed {
            tracing::warn!(doc = %self.doc_id, error = %err, "subagent segment close failed");
        }
        let entry = SessionMessageEntry {
            id: new_id(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "t0".into(),
                text: text.to_owned(),
            }],
            created_at: now_ms(),
            device_id: device_id.to_owned(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        };
        if let Err(err) = self.doc.push_message(&entry) {
            tracing::warn!(doc = %self.doc_id, error = %err, "subagent steer write failed");
        }
        self.entry_id = new_id();
        self.started_at = now_ms();
        self.written = Vec::new();
        self.folded = Vec::new();
        self.dirty = false;
    }

    /// Final flush + entry finalize; returns the transcript JSON for the
    /// frozen blob (entries as the client renders them).
    fn finish(mut self, device_id: &str, status: MessageStatus) -> Option<String> {
        let rendered = render_parts(&self.folded);
        let finished = match self.entry_index {
            Some(ix) => SegmentWriter::resume(&self.doc, ix, std::mem::take(&mut self.written))
                .finish(&rendered, status),
            None if !self.folded.is_empty() => {
                match SegmentWriter::begin(&self.doc, &self.entry_id, device_id, self.started_at) {
                    Ok(w) => w.finish(&rendered, status),
                    Err(e) => Err(e),
                }
            }
            None => Ok(()),
        };
        if let Err(err) = finished {
            tracing::warn!(doc = %self.doc_id, error = %err, "subagent sink finish failed");
        }
        let entries = luna_doc::join_continuation_entries(self.doc.read_entries().ok()?);
        serde_json::to_string(&entries).ok()
    }
}

/// The parent-chip refresh a tagged event implies, for the in-place (parked)
/// path — LIFECYCLE ONLY, mirroring the fold (live tails were rejected:
/// per-delta chip rewrites grew the parent doc for the whole run).
fn subagent_chip_update(event: &AgentEvent) -> Option<&'static str> {
    match event {
        AgentEvent::Done { status, .. } => Some(match status {
            DoneStatus::Errored => "failed",
            DoneStatus::Interrupted => "interrupted",
            DoneStatus::Completed => "done",
        }),
        AgentEvent::InputRequested { .. } => Some("waiting"),
        AgentEvent::ChildStatus { status } => Some(match status {
            luna_proto::ChildRuntimeStatus::Active => "running",
            luna_proto::ChildRuntimeStatus::Idle => "idle",
        }),
        AgentEvent::InputResolved { .. }
        | AgentEvent::UserMessage { .. }
        | AgentEvent::Steered { .. } => Some("running"),
        _ => None,
    }
}

/// Apply the render-parts privacy policy: strip heavy/sensitive tool inputs before doc
/// entry. Full inputs live only in the local run journal.
fn render_parts(parts: &[MessagePart]) -> Vec<MessagePart> {
    parts
        .iter()
        .map(|part| match part {
            MessagePart::Tool {
                id,
                call,
                is_error,
                resolved,
                output,
                diff,
                output_ref,
                output_bytes,
                diff_ref,
                diff_stats,
                subagent_ref,
                subagent_status,
                subagent_tail,
                subagent_activity,
            } => MessagePart::Tool {
                id: id.clone(),
                call: sanitize_tool_call(call),
                is_error: *is_error,
                resolved: *resolved,
                // Output summaries, diff stats, and sidecar refs are
                // deliberately kept: unlike raw tool inputs they are the
                // transcript's record of what happened, and the strip already
                // bounded them (docs/chat2-sync.md A1).
                output: output.clone(),
                diff: diff.clone(),
                output_ref: output_ref.clone(),
                output_bytes: *output_bytes,
                diff_ref: diff_ref.clone(),
                diff_stats: diff_stats.clone(),
                subagent_ref: subagent_ref.clone(),
                subagent_status: *subagent_status,
                subagent_tail: subagent_tail.clone(),
                subagent_activity: subagent_activity.clone(),
            },
            other => other.clone(),
        })
        .collect()
}

/// The persisted assistant text of a folded segment (workspace preview source).
fn folded_text(parts: &[MessagePart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn sync_segment<'a>(
    doc: &'a SessionDoc,
    writer: &mut Option<SegmentWriter<'a>>,
    entry_id: &str,
    device_id: &str,
    started_at: i64,
    folded: &[MessagePart],
) -> Result<(), DocError> {
    if folded.is_empty() {
        return Ok(());
    }
    let rendered = render_parts(folded);
    if writer.is_none() {
        *writer = Some(SegmentWriter::begin(doc, entry_id, device_id, started_at)?);
    }
    if let Some(w) = writer.as_mut() {
        w.sync(&rendered)?;
    }
    Ok(())
}

fn finish_segment<'a>(
    doc: &'a SessionDoc,
    writer: Option<SegmentWriter<'a>>,
    entry_id: &str,
    device_id: &str,
    started_at: i64,
    folded: &[MessagePart],
    status: MessageStatus,
) -> Result<(), DocError> {
    let rendered = render_parts(folded);
    match writer {
        Some(w) => w.finish(&rendered, status),
        None if !folded.is_empty() => {
            SegmentWriter::begin(doc, entry_id, device_id, started_at)?.finish(&rendered, status)
        }
        None => Ok(()),
    }
}

/// `~` / `~/…` → this host's home directory. Anything else passes through.
fn expand_home(cwd: &str) -> String {
    match cwd.strip_prefix("~") {
        Some("") => crate::repos::home_dir().to_string_lossy().into_owned(),
        Some(rest) if rest.starts_with('/') => crate::repos::home_dir()
            .join(&rest[1..])
            .to_string_lossy()
            .into_owned(),
        _ => cwd.to_string(),
    }
}

/// Resume bookkeeping for one run task: which user entry the run answers (so
/// the startup-crash retry re-dispatches idempotently against the same doc
/// entry), whether `dispatch` injected the resume id itself (only
/// engine-injected resumes retry — a caller-specified resume fails loudly),
/// and whether this run already IS the retry (one attempt only).
struct RunResumeState {
    user_message_id: String,
    recovery: Option<RecoveryAttemptContext>,
}

#[derive(Debug, Clone)]
struct RecoveryAttemptContext {
    chain_id: String,
    attempt_count: u8,
    automatic: bool,
}

fn latest_user_message_id(handle: &ChatDocHandle) -> Result<Option<String>, EngineError> {
    Ok(handle
        .doc()
        .read_entries()?
        .into_iter()
        .rev()
        .find(|entry| entry.role == MessageRole::User)
        .map(|entry| entry.id))
}

fn reduce_provider_context_compaction(
    current: &ContextControlState,
    bound_thread_id: &str,
    binding_generation: u32,
    operation_id: &str,
    status: ContextOperationStatus,
    event_thread_id: &str,
) -> Option<(ContextControlState, bool)> {
    if bound_thread_id != event_thread_id || operation_id.is_empty() {
        return None;
    }
    let now = now_ms();
    let mut next = current.clone();
    match status {
        ContextOperationStatus::Running => {
            if current.compact.blocks_new_turn() {
                return None;
            }
            next.binding_generation = Some(binding_generation);
            next.compact = CompactOperation {
                operation_id: Some(operation_id.to_string()),
                status,
                mechanism: Some(CompactMechanism::NativeOperation),
                binding_generation: Some(binding_generation),
                started_at_ms: Some(now),
                finished_at_ms: None,
                detail: Some(
                    "Codex reported provider-owned context compaction in progress. The provider may compact traditionally or start a new context window without exposing a summary."
                        .into(),
                ),
            };
            next.notice = None;
            Some((next, false))
        }
        ContextOperationStatus::Completed | ContextOperationStatus::Unknown => {
            if current.compact.status != ContextOperationStatus::Running
                || current.compact.operation_id.as_deref() != Some(operation_id)
                || current.compact.binding_generation != Some(binding_generation)
            {
                return None;
            }
            next.compact.status = status;
            next.compact.finished_at_ms = Some(now);
            next.compact.detail = Some(if status == ContextOperationStatus::Completed {
                "Codex reported that provider-owned context compaction completed. No readable summary was exposed to LUNA."
                    .into()
            } else {
                "Codex context compaction ended without a correlated completion. LUNA will not replay it."
                    .into()
            });
            Some((next, status == ContextOperationStatus::Completed))
        }
        ContextOperationStatus::Idle | ContextOperationStatus::Failed => None,
    }
}

fn reduce_provider_context_terminal(
    current: &ContextControlState,
    bound_thread_id: &str,
    binding_generation: u32,
) -> Option<ContextControlState> {
    let operation_id = current.compact.operation_id.as_deref()?;
    if !operation_id.starts_with(&format!("codex-auto:{bound_thread_id}:")) {
        return None;
    }
    reduce_provider_context_compaction(
        current,
        bound_thread_id,
        binding_generation,
        operation_id,
        ContextOperationStatus::Unknown,
        bound_thread_id,
    )
    .map(|(next, _)| next)
}

fn frozen_run_for_message(
    handle: &ChatDocHandle,
    message_id: &str,
) -> Result<Option<FrozenRun>, EngineError> {
    let commands = handle.doc().read_commands()?;
    let Some((run_index, command_id, request)) =
        commands
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, command)| match &command.payload {
                SessionCommandPayload::Run {
                    request,
                    message_id: candidate,
                } if candidate == message_id => Some((index, command.id.clone(), request.clone())),
                _ => None,
            })
    else {
        return Ok(None);
    };
    let stopped = commands.iter().skip(run_index + 1).any(|command| {
        command.status == SessionCommandStatus::Applied
            && matches!(command.payload, SessionCommandPayload::Interrupt {})
    });
    Ok(Some(FrozenRun {
        command_id,
        request,
        stopped,
    }))
}

struct FrozenRun {
    command_id: String,
    request: RunRequest,
    stopped: bool,
}

#[allow(clippy::too_many_arguments)]
fn stop_after_journal_failure(
    inner: &Arc<Inner>,
    chat_id: &str,
    run_id: &str,
    harness_id: HarnessId,
    request: &RunRequest,
    resume_state: &RunResumeState,
    provider_session_id: Option<String>,
    error: &EngineError,
) {
    tracing::error!(chat = %chat_id, run = %run_id, %error, "run journal write outcome uncertain; stopping provider");
    let target = {
        let mut runs = lock(&inner.runs);
        runs.get_mut(chat_id)
            .filter(|handle| handle.run_id == run_id)
            .map(|handle| {
                handle.provider_turn = None;
                handle.set_provider_turn_readiness(ProviderTurnReadiness::Ended);
                let steers = lock(&handle.routed_steers).drain(..).collect::<Vec<_>>();
                (
                    handle.interrupt_token.clone(),
                    handle.cancel.clone(),
                    handle.pending_inputs.clone(),
                    steers,
                )
            })
    };
    if let Some((token, cancel, pending, steers)) = target {
        token.cancel();
        let _ = cancel.send(true);
        for (_, pending) in lock(&pending).drain() {
            let _ = pending.resolver.send(Vec::new());
        }
        if let Some(host) = inner.doc_host() {
            for steer in steers {
                if let Err(error) = host.mark_steer_correlation_unknown(chat_id, &steer.message_id)
                {
                    tracing::warn!(chat = %chat_id, %error, "journal failure steer hold could not persist");
                }
            }
        }
    }

    let detail = "Run stopped because the local journal write outcome is uncertain. The last provider event was not confirmed in the journal. Review the partial output and provider state; do not resend automatically.";
    if let Some(host) = inner.doc_host() {
        match host.open(chat_id) {
            Ok(handle) => {
                if let Err(error) = handle.mark_abandoned_streams(detail) {
                    tracing::warn!(chat = %chat_id, %error, "journal failure could not abort the streaming transcript");
                }
            }
            Err(error) => {
                tracing::warn!(chat = %chat_id, %error, "journal failure transcript readback unavailable")
            }
        }
    }
    let engine = SessionsEngine {
        inner: inner.clone(),
    };
    let recovery = engine.record_recovery_failure(
        chat_id,
        harness_id,
        request.clone(),
        resume_state.user_message_id.clone(),
        RecoveryAcceptance::Unknown,
        RecoveryFailure::Other,
        detail.into(),
        provider_session_id,
        resume_state.recovery.as_ref(),
    );
    if !matches!(&recovery, Ok(Some(view)) if view.detail.contains("local journal")) {
        if let Err(error) = recovery {
            tracing::error!(chat = %chat_id, %error, "journal failure recovery detail could not persist");
        }
        // Disk failure can also prevent a durable recovery row. Keep the
        // uncertainty visible in this process without claiming persistence.
        let now = now_ms();
        inner.publish_recovery(
            chat_id,
            Some(TaskRecovery {
                chain_id: new_id(),
                task_id: chat_id.into(),
                original_message_id: resume_state.user_message_id.clone(),
                original_command_id: None,
                original_acceptance: RecoveryAcceptance::Unknown,
                acceptance: RecoveryAcceptance::Unknown,
                failure: RecoveryFailure::Other,
                action: luna_proto::RecoveryAction::ObserveOnly,
                status: RecoveryStatus::CheckingDelivery,
                attempt_count: 0,
                max_attempts: MAX_AUTO_RETRIES,
                automatic: false,
                retry_at_ms: None,
                automatic_deadline_ms: now,
                environment_id: inner.device_id.clone(),
                binding_id: None,
                binding_generation: None,
                detail: detail.into(),
                created_at_ms: now,
                updated_at_ms: now,
            }),
        );
    }
    inner.set_status(chat_id, SessionStatus::Errored, false);
}

/// Recovery acceptance for a startup the adapter proved was rejected before
/// any user turn. A durable receipt that already proves acceptance or
/// ambiguous delivery keeps that stronger claim, so this rejection never makes
/// the whole input replayable. Read-only; see [`reject_known_startup`].
fn startup_rejection_acceptance(
    inner: &Arc<Inner>,
    chat_id: &str,
    correlation_id: &str,
) -> RecoveryAcceptance {
    let Some(host) = inner.doc_host() else {
        return RecoveryAcceptance::RejectedBeforeAcceptance;
    };
    match host.task_command_receipt_for_correlation(chat_id, correlation_id) {
        Ok(Some(receipt)) if receipt.status == CommandReceiptStatus::Applied => {
            RecoveryAcceptance::Accepted
        }
        Ok(Some(receipt))
            if receipt.status == CommandReceiptStatus::Pending
                && receipt.committed_at_ms.is_some() =>
        {
            RecoveryAcceptance::Unknown
        }
        Ok(_) => RecoveryAcceptance::RejectedBeforeAcceptance,
        Err(error) => {
            tracing::error!(chat = %chat_id, %error, "startup rejection receipt readback failed");
            RecoveryAcceptance::Unknown
        }
    }
}

/// Terminalize the exact pending Run receipt of a known startup rejection that
/// no automatic recovery will resend (a scheduled resend confirms the same
/// correlation on its own `SessionStarted`). The store fences the receipt and
/// marks the same-device frozen submission recoverable atomically; accepted,
/// delivery-unknown, terminal and fenced-out receipts are left unchanged.
fn reject_known_startup(inner: &Arc<Inner>, chat_id: &str, correlation_id: &str, message: &str) {
    let Some(host) = inner.doc_host() else {
        return;
    };
    let expected_w = match inner
        .recovery
        .execution_workspace_repair_revision(chat_id, correlation_id)
    {
        Ok(revision) => revision,
        Err(error) => {
            tracing::error!(chat = %chat_id, %error, "startup rejection workspace fence unavailable");
            return;
        }
    };
    match host.reject_run_before_acceptance(chat_id, correlation_id, expected_w, message) {
        Ok(rejected) if rejected.is_empty() => {
            tracing::info!(chat = %chat_id, correlation = %correlation_id,
                "startup rejection left the command receipt unchanged (terminal, unknown, accepted or fenced)");
        }
        Ok(_) => {}
        Err(error) => {
            tracing::error!(chat = %chat_id, %error, "startup rejection receipt could not be finalized");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive_run(
    inner: Arc<Inner>,
    chat_id: String,
    run_id: String,
    harness: Arc<dyn Harness>,
    request: RunRequest,
    original_request: RunRequest,
    doc: Arc<SessionDoc>,
    controls: RunControls,
    mut engine_rx: mpsc::UnboundedReceiver<AgentEvent>,
    mut cancel_rx: watch::Receiver<bool>,
    hold_recovery_on_interrupt: Arc<AtomicBool>,
    resume_state: RunResumeState,
    run_permit: Option<DeviceWritePermit>,
) {
    let device_id = inner.device_id.clone();
    // Captured for post-run auto-titling (the request moves into the harness).
    let harness_id = harness.id();
    let user_prompt = request.prompt.clone();
    let run_cwd = request.cwd.clone();
    let permission_policy = request.permission_policy.clone();
    let runtime_contract = request
        .model_options
        .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
        .and_then(|value| {
            serde_json::from_value::<luna_proto::LunaAgentRuntimeContract>(value.clone()).ok()
        });
    if request.resume.is_none() {
        let _ = doc.clear_context_usage();
    }
    let recovery_request = original_request;
    let mut stream = match harness.run(request, controls).await {
        Ok(stream) => stream,
        Err(err) => {
            let acceptance = if matches!(err, HarnessError::RejectedBeforeAcceptance(_)) {
                RecoveryAcceptance::RejectedBeforeAcceptance
            } else {
                RecoveryAcceptance::Unknown
            };
            let message = err.to_string();
            let journal_result = inner
                .publish(
                    &chat_id,
                    &AgentEvent::Error {
                        message: message.clone(),
                    },
                    run_permit.as_ref(),
                )
                .and_then(|_| {
                    inner.publish(
                        &chat_id,
                        &AgentEvent::Done {
                            status: DoneStatus::Errored,
                            result: None,
                            error: Some(message.clone()),
                            session_id: None,
                        },
                        run_permit.as_ref(),
                    )
                });
            if let Err(error) = journal_result {
                stop_after_journal_failure(
                    &inner,
                    &chat_id,
                    &run_id,
                    harness_id,
                    &recovery_request,
                    &resume_state,
                    None,
                    &error,
                );
                inner.remove_run(&chat_id, &run_id);
                return;
            }
            inner.remove_run(&chat_id, &run_id);
            // Recovery attempts keep their chain's own acceptance rules.
            let fresh_rejection = acceptance == RecoveryAcceptance::RejectedBeforeAcceptance
                && resume_state.recovery.is_none();
            let acceptance = if fresh_rejection {
                startup_rejection_acceptance(&inner, &chat_id, &resume_state.user_message_id)
            } else {
                acceptance
            };
            inner.set_status(&chat_id, SessionStatus::Errored, false);
            let engine = SessionsEngine {
                inner: inner.clone(),
            };
            let correlation_id = resume_state.user_message_id.clone();
            let recorded = engine.record_recovery_failure(
                &chat_id,
                harness_id,
                recovery_request,
                resume_state.user_message_id,
                acceptance,
                classify_failure(&message),
                message.clone(),
                None,
                resume_state.recovery.as_ref(),
            );
            let automatic_resend = matches!(&recorded, Ok(Some(view))
                if view.automatic || view.status == RecoveryStatus::Scheduled);
            if let Err(error) = recorded {
                tracing::error!(chat = %chat_id, %error, "failed to record pre-accept recovery");
            }
            if fresh_rejection
                && acceptance == RecoveryAcceptance::RejectedBeforeAcceptance
                && !automatic_resend
            {
                reject_known_startup(&inner, &chat_id, &correlation_id, &message);
            }
            return;
        }
    };

    let doc_ref: &SessionDoc = &doc;
    let mut folded: Vec<MessagePart> = Vec::new();
    // Every tool id this run has folded, across segment resets. Adapters
    // re-emit shape-bearing `tool_call_update`s (title/rawInput refreshes,
    // long-running completions) as full ToolCall events; once the fold has
    // reset at a steer/park boundary those ids are gone from `folded`, and
    // folding the echo would mint an orphan chip mid-text in the NEXT
    // segment — the mid-word transcript splits.
    let mut seen_tools: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tool_entry_by_id: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut seen_images = std::collections::HashSet::new();
    for entry in doc_ref.read_entries().unwrap_or_default() {
        for part in entry.parts {
            if let MessagePart::Image { id, .. } = part {
                seen_images.insert(format!("{chat_id}\0{id}"));
                if let Some(tool_id) = id.strip_suffix(":image") {
                    seen_tools.insert(tool_id.to_owned());
                }
            }
        }
    }
    let mut prepared_events = std::collections::VecDeque::new();
    let mut entry_id = new_id();
    let mut segment_started = now_ms();
    let mut writer: Option<SegmentWriter<'_>> = None;
    let mut provider_checkpoint_id: Option<String> = None;
    let mut dirty = false;
    let mut flush_at = tokio::time::Instant::now();
    // Set when the engine interrupts the run: the harness gets this long to end its own
    // stream (its token was cancelled); past it, a terminal Done is synthesized.
    let mut interrupt_deadline: Option<tokio::time::Instant> = None;
    let mut interrupted = false;
    let mut saw_session_started = false;
    let mut recovery_session_id = recovery_request.resume.clone();
    let mut last_error_message: Option<String> = None;
    // Liveness heartbeat: this loop RUNNING is proof the harness stream is
    // open, so freshness must not depend on events arriving. Silent stretches
    // are normal and UNBOUNDED — a long tool call, redacted thinking, an
    // agent waiting on an external process, a question parked for an hour —
    // and each starved the UI's 45s staleness gate in turn (working strip /
    // AwaitingInput dot vanishing mid-run, both user-reported). No stall
    // timeout here by design (a first port was rejected — agents may
    // legitimately be quiet for >10min): a live child means Working, dying
    // paths each carry their own error, and engine death stops these ticks
    // so the gate still catches real crashes. touch_session throttles at 10s.
    let mut live_heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
    live_heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // PERSISTENT SESSION (luna runsBySession): a completed turn on a
    // steerable harness parks here instead of ending the run — the child and
    // its steering mailbox stay warm, and the next user message (dispatch
    // routes into a live run) starts the next turn with zero respawn/resume
    // latency. `Some(when)` = idle since then; the 30-min reaper below ends
    // a session nobody comes back to (luna SESSION_IDLE_MS).
    const SESSION_IDLE: std::time::Duration = std::time::Duration::from_secs(30 * 60);
    let mut idle_since: Option<tokio::time::Instant> = None;
    // The kind of the transient provider notice currently on the doc, if any.
    // A notice left by a run that died mid-retry is stale for this run.
    let mut provider_notice: Option<luna_proto::ProviderNoticeKind> = None;
    if let Err(err) = doc_ref.set_provider_notice(None) {
        tracing::warn!(%chat_id, error = %err, "stale provider notice clear failed");
    }
    let steerable = harness.supports_steering();
    // TURN-QUIESCE WATCHDOG (2026-08-12 stuck-Working incident): a harness
    // that loses a turn's Done — the adapter never settles `session/prompt`
    // even though the agent finished — strands Working forever: the live
    // heartbeat above keeps the row fresh, and there is no per-turn timeout
    // by design. This is NOT that stall timeout: it never ends the run or
    // errors anything. When the stream has been silent past the window AND
    // the fold shows completed output with nothing in flight (no unresolved
    // tool, no open question), the turn parks exactly like a Done would —
    // segment finalized Complete, status Idle, child and mailbox warm. A
    // false trip (the agent was quietly waiting on something invisible)
    // costs a status dip: the parked-resume path below re-arms Working the
    // moment output flows again, and nothing is lost. `LUNA_TURN_QUIESCE_MS`
    // overrides the window; 0 disables.
    // RETIRED for native drivers: a harness whose every turn shape ends with
    // a deterministic wire Done (claude/codex/cursor native) needs no
    // quiesce backstop — arming one only risks false parks on long silent
    // work. The env knob can configure a diagnostic window, but a prompt
    // with an authoritative completion signal must still await that signal.
    // ACP retains the watchdog only for unowned self-continued activity.
    let deterministic_turn_end = harness.deterministic_turn_end();
    let authoritative_prompt_end = harness.authoritative_prompt_end();
    let quiesce_after: Option<std::time::Duration> = match std::env::var("LUNA_TURN_QUIESCE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        Some(0) => None,
        Some(ms) => Some(std::time::Duration::from_millis(ms)),
        None if deterministic_turn_end => None,
        None => Some(std::time::Duration::from_secs(120)),
    };
    let mut last_stream_activity = tokio::time::Instant::now();
    // SELF-CONTINUED turns get a much SHORTER quiesce window. A turn the
    // agent starts on its own (background-task wake) can never receive a
    // harness Done: the adapter has no `session/prompt` outstanding to
    // settle — verified in claude-agent-acp's autonomous-result lane, which
    // consumes the SDK's turn-end without emitting anything; codex shows
    // the same shape. The watchdog is that turn shape's ONLY settle path,
    // so the default 120s window read as 2min of stuck-Working after every
    // background notification (user report 2026-08-13). The in-flight
    // fold gate below still protects running tools; reasoning heartbeats
    // push the window during real thinking. `LUNA_SELF_TURN_QUIESCE_MS`
    // overrides; 0 falls back to the normal window. An explicit
    // `LUNA_TURN_QUIESCE_MS=0` still disables the watchdog entirely.
    let self_quiesce_after: Option<std::time::Duration> =
        match std::env::var("LUNA_SELF_TURN_QUIESCE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            Some(0) => None,
            Some(ms) => Some(std::time::Duration::from_millis(ms)),
            None => Some(std::time::Duration::from_secs(20)),
        };
    let mut self_continued_turn = false;
    // Spawn chips that have SETTLED (tagged Done seen). Content events for a
    // settled chip with no live sink are dropped — a straggler frame after
    // the freeze must not mint a new doc entry or wedge the transcript back
    // into Streaming. Only a steer (UserMessage) legitimately REOPENS a
    // settled subagent: it announces more work is coming.
    let mut settled_subagents: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Live subagent sinks, parent tool-use id → transcript doc state.
    let mut subagents: std::collections::HashMap<String, SubagentSink> =
        std::collections::HashMap::new();
    let mut child_activity: std::collections::HashMap<String, luna_doc::SubagentActivity> =
        std::collections::HashMap::new();
    // Nested spawn chips live in their owning child's transcript.
    let mut nested_spawns = native_subagents::NestedSpawns::default();

    let mut final_completed_turn = None;
    let mut journal_failure: Option<EngineError> = None;
    let final_status = loop {
        let event: AgentEvent = if let Some(event) = prepared_events.pop_front() {
            event
        } else {
            let raw_event = tokio::select! {
                biased;
                changed = cancel_rx.changed(), if !interrupted => {
                    let _ = changed;
                    interrupted = true;
                    inner.clear_codex_checkpoint(&chat_id, &run_id);
                    interrupt_deadline = Some(
                        tokio::time::Instant::now() + std::time::Duration::from_secs(3),
                    );
                    continue;
                }
                _ = tokio::time::sleep_until(
                    interrupt_deadline.unwrap_or_else(tokio::time::Instant::now)
                ), if interrupt_deadline.is_some() => AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: None,
                },
                _ = live_heartbeat.tick() => {
                    inner.touch_session(&chat_id);
                    continue;
                }
                // Idle reaper (luna SESSION_IDLE_MS): a parked persistent session
                // nobody returned to in 30 minutes releases its child. The turn
                // was finalized at Done, so this end is clean — no aborted stamp.
                _ = tokio::time::sleep_until(
                    idle_since.map(|at| at + SESSION_IDLE).unwrap_or_else(tokio::time::Instant::now)
                ), if idle_since.is_some() => {
                    tracing::info!(chat = %chat_id, "reaping idle persistent session");
                    if let Some(token) = lock(&inner.runs)
                        .get(&chat_id)
                        .filter(|h| h.run_id == run_id)
                        .map(|h| h.interrupt_token.clone())
                    {
                        token.cancel();
                    }
                    break SessionStatus::Idle;
                }
                Some(event) = engine_rx.recv() => event,
                next = stream.next() => match next {
                    Some(Ok(event)) => event,
                    // A stream error while PARKED is a post-turn child death —
                    // the turn was already finalized, so the run ends clean
                    // instead of stamping a completed session Errored.
                    Some(Err(err)) if idle_since.is_some() => {
                        tracing::warn!(chat = %chat_id, error = %err, "parked session child died; ending clean");
                        break SessionStatus::Idle;
                    }
                    Some(Err(err)) => AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(err.to_string()),
                        session_id: None,
                    },
                    None if interrupted => AgentEvent::Done {
                        status: DoneStatus::Interrupted,
                        result: None,
                        error: None,
                        session_id: None,
                    },
                    // Stream end while PARKED idle: a per-turn adapter closing
                    // after its final Done — a clean end, not a crash (the turn
                    // was already finalized). Persistent adapters keep the
                    // stream open and never hit this.
                    None if idle_since.is_some() => {
                        if let Some(run) = lock(&inner.runs)
                            .get(&chat_id)
                            .filter(|run| run.run_id == run_id)
                        {
                            run.idle_stream_closed.store(true, Ordering::Release);
                        }
                        break SessionStatus::Idle;
                    }
                    None => AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some("harness stream ended without Done".into()),
                        session_id: None,
                    },
                },
                _ = tokio::time::sleep_until(flush_at), if dirty || subagents.values().any(|s| s.dirty) => {
                    // Coalesced STREAM_COMMIT_MS tick: one doc commit per window
                    // (parent + any dirty subagent docs).
                    if dirty {
                        if let Err(err) = sync_segment(
                            doc_ref, &mut writer, &entry_id, &device_id, segment_started, &folded,
                        ) {
                            tracing::warn!(chat = %chat_id, error = %err, "segment sync failed");
                        }
                        dirty = false;
                    }
                    for sink in subagents.values_mut() {
                        sink.flush(&device_id);
                    }
                    continue;
                }
                // Turn-quiesce watchdog (see the knob above). Armed only when the
                // fold says nothing is in flight: an unresolved tool part is a
                // command still running (legitimately silent for minutes — the
                // rejected stall-timeout case), an unresolved input part is a
                // question awaiting the user. The live-plan chip is exempt from
                // the tool check: it is a singleton that never resolves. An EMPTY
                // fold still arms — a Steered boundary that no output ever
                // follows is one of the wedge shapes — it just parks without
                // writing a segment (an empty finalize would leave a stub entry).
                _ = tokio::time::sleep_until({
                    let mut window = quiesce_after.unwrap_or_default();
                    if self_continued_turn && let Some(short) = self_quiesce_after {
                        window = window.min(short);
                    }
                    last_stream_activity + window
                }), if quiesce_after.is_some()
                    && (self_continued_turn || !authoritative_prompt_end)
                    && idle_since.is_none()
                    && !interrupted
                    && steerable
                    && !folded.iter().any(|p| match p {
                        MessagePart::Tool { id, resolved: false, .. } => {
                            id != luna_proto::LIVE_PLAN_TOOL_ID
                        }
                        MessagePart::Input { resolved: false, .. } => true,
                        _ => false,
                    }) =>
                {
                    tracing::warn!(
                        chat = %chat_id,
                        quiet_ms = quiesce_after.unwrap_or_default().as_millis() as u64,
                        "turn quiesced: stream silent after completed output with no \
                         turn-end; parking (suspected missing harness Done)"
                    );
                    // Some adapters close a completed response through this
                    // engine watchdog instead of a native Done. Preserve that
                    // completion notice, but never notify for an empty boundary
                    // or while an accepted steer still awaits delivery.
                    let completed_turn = ((!folded.is_empty() || writer.is_some())
                        && !inner.has_pending_steers(&chat_id, &run_id))
                        .then(|| entry_id.clone());
                    if !folded.is_empty() || writer.is_some() {
                        if let Err(err) = finish_segment(
                            doc_ref,
                            writer.take(),
                            &entry_id,
                            &device_id,
                            segment_started,
                            &folded,
                            MessageStatus::Complete,
                        ) {
                            tracing::warn!(chat = %chat_id, error = %err, "quiesce segment finish failed");
                        }
                        inner.note_message(&chat_id, &folded_text(&folded));
                    }
                    folded.clear();
                    dirty = false;
                    entry_id = new_id();
                    segment_started = now_ms();
                    idle_since = Some(tokio::time::Instant::now());
                    self_continued_turn = false;
                    inner.set_status_with_completion(
                        &chat_id, SessionStatus::Idle, false, completed_turn,
                    );
                    continue;
                }
            };

            prepared_events.extend(
                inner
                    .prepare_generated_image(&chat_id, raw_event, &mut seen_images)
                    .await,
            );
            let Some(event) = prepared_events.pop_front() else {
                continue;
            };
            event
        };

        // Native provider evidence is host-journal retention only: never a
        // transcript part, turn boundary, resume or liveness signal, so it
        // bypasses the subagent router and the parked gate entirely.
        if matches!(&event, AgentEvent::ProviderEvidence { .. }) {
            if let Err(error) = inner.publish(&chat_id, &event, run_permit.as_ref()) {
                tracing::warn!(chat = %chat_id, %error, "provider evidence journal append failed");
            }
            continue;
        }

        // CC-06: live accounting is control-plane data for the Usage page; it
        // never reaches the journal, the transcript or turn state.
        if let AgentEvent::ProviderAccounting { report } = &event {
            inner.live_usage.record(&chat_id, report);
            continue;
        }

        // ── transient provider status ─────────────────────────────────
        // Retry/warning notices (root or sub-agent) are task metadata for the
        // working indicator: never a transcript part, never a turn failure.
        // A retry notice ends when output resumes; every notice ends with the
        // root turn.
        let notice = match &event {
            AgentEvent::ProviderNotice { notice } => Some(notice),
            AgentEvent::Subagent { event, .. } => match event.as_ref() {
                AgentEvent::ProviderNotice { notice } => Some(notice),
                _ => None,
            },
            _ => None,
        };
        if let Some(notice) = notice {
            if let Err(err) = doc_ref.set_provider_notice(Some(notice)) {
                tracing::warn!(%chat_id, error = %err, "provider notice write failed");
            }
            provider_notice = Some(notice.kind);
            continue;
        }
        if let Some(kind) = provider_notice {
            let output_resumed = matches!(
                &event,
                AgentEvent::TextDelta { text } | AgentEvent::ReasoningDelta { text }
                    if !text.is_empty()
            ) || matches!(&event, AgentEvent::ToolCall { .. });
            if matches!(&event, AgentEvent::Done { .. })
                || (kind == luna_proto::ProviderNoticeKind::Retrying && output_resumed)
            {
                if let Err(err) = doc_ref.set_provider_notice(None) {
                    tracing::warn!(%chat_id, error = %err, "provider notice clear failed");
                }
                provider_notice = None;
            }
        }

        // ── subagent routing ───────────────────────────────────────────
        // Tagged events NEVER fold into the parent transcript: they stream
        // into the subagent's own doc, and the parent keeps only the spawn
        // chip (ref + status + one-line tail) — refreshed through the live
        // fold while its segment still streams, else stamped in place (the
        // eager-done norm: the chip's entry finished before the background
        // subagent spoke). Handled BEFORE the parked gate so a parked
        // session's background traffic still reaches the subagent doc
        // without un-parking the chat.
        if let AgentEvent::Subagent {
            parent_tool_use_id,
            event: sub_event,
        } = &event
        {
            if let Err(error) = inner.publish(&chat_id, &event, run_permit.as_ref()) {
                journal_failure = Some(error);
                break SessionStatus::Errored;
            }
            let is_steer = matches!(
                sub_event.as_ref(),
                AgentEvent::UserMessage { .. } | AgentEvent::Steered { .. }
            );
            if is_steer {
                settled_subagents.remove(parent_tool_use_id);
            } else if settled_subagents.contains(parent_tool_use_id)
                && !subagents.contains_key(parent_tool_use_id)
            {
                // Straggler after the freeze: chip-only silence (the old
                // pre-viz behavior), never a reopened doc.
                continue;
            }
            nested_spawns.observe(parent_tool_use_id, sub_event);
            let sub_id = subagent_doc_id(&chat_id, parent_tool_use_id);
            let observation = child_activity
                .entry(parent_tool_use_id.clone())
                .or_default();
            observation.apply(sub_event);
            let chip_streaming = folded
                .iter()
                .any(|p| matches!(p, MessagePart::Tool { id, .. } if id == parent_tool_use_id));
            let sink_known = subagents.contains_key(parent_tool_use_id);
            let metadata_only = matches!(
                sub_event.as_ref(),
                AgentEvent::Usage { .. }
                    | AgentEvent::ContextUsage { .. }
                    | AgentEvent::ChildStatus { .. }
                    | AgentEvent::ChildProgress { .. }
            );
            // A Done with NO sink (a subagent that never streamed — codex
            // turn ends can beat registration) is chip-only: minting a doc
            // just to freeze it empty helps no one, and stamping the ref
            // would link the chip to that never-created doc (an empty tab
            // on click).
            let done_only = !sink_known && matches!(sub_event.as_ref(), AgentEvent::Done { .. });
            // A nested spawn's chip is in its owning child's transcript; any
            // chip refresh below targets that doc instead of the root's.
            let nested_chip_doc = (!chip_streaming
                && (!sink_known
                    || subagent_chip_update(sub_event).is_some()
                    || matches!(
                        sub_event.as_ref(),
                        AgentEvent::Usage { .. } | AgentEvent::ChildProgress { .. }
                    )))
            .then(|| {
                nested_spawns.chip_doc(
                    parent_tool_use_id,
                    &inner,
                    &chat_id,
                    &mut subagents,
                    &device_id,
                )
            })
            .flatten();
            let chip_doc: &SessionDoc = nested_chip_doc.as_deref().unwrap_or(doc_ref);
            if chip_streaming {
                if !sink_known && !done_only {
                    for p in folded.iter_mut() {
                        if let MessagePart::Tool {
                            id,
                            call,
                            subagent_ref,
                            ..
                        } = p
                            && id == parent_tool_use_id
                            // Genus gate: a subagent ref may only ever bind
                            // to a SPAWN call — mis-keyed tagged traffic
                            // must not turn an ordinary chip into a spawn
                            // link (empty tab on click).
                            && call.is_subagent_spawn()
                        {
                            *subagent_ref = Some(sub_id.clone());
                        }
                    }
                }
                luna_doc::fold_event_into_parts(&mut folded, &event);
                if !dirty {
                    dirty = true;
                    flush_at = tokio::time::Instant::now()
                        + std::time::Duration::from_millis(STREAM_COMMIT_MS);
                }
            }
            // Open the sink lazily; an open failure degrades to chip-only.
            if !sink_known && !done_only && !metadata_only {
                let opened = inner.doc_host().and_then(|host| match host.open(&sub_id) {
                    Ok(handle) => Some(handle.doc_arc()),
                    Err(err) => {
                        tracing::warn!(doc = %sub_id, error = %err, "subagent doc open failed (chip-only)");
                        None
                    }
                });
                if let Some(sub_doc) = opened {
                    subagents.insert(
                        parent_tool_use_id.clone(),
                        SubagentSink {
                            doc_id: sub_id.clone(),
                            doc: sub_doc,
                            entry_id: new_id(),
                            started_at: now_ms(),
                            entry_index: None,
                            written: Vec::new(),
                            folded: Vec::new(),
                            dirty: false,
                        },
                    );
                    if !chip_streaming {
                        let _ = chip_doc.update_subagent_chip(
                            parent_tool_use_id,
                            Some(&sub_id),
                            Some("running"),
                            None,
                            None,
                        );
                    }
                }
            }
            let done = matches!(sub_event.as_ref(), AgentEvent::Done { .. });
            if done {
                settled_subagents.insert(parent_tool_use_id.clone());
            }
            if !sink_known
                && (done_only
                    || matches!(
                        sub_event.as_ref(),
                        AgentEvent::Usage { .. }
                            | AgentEvent::ChildStatus { .. }
                            | AgentEvent::ChildProgress { .. }
                    ))
                && !chip_streaming
            {
                // Completion or usage can arrive before child content. Update
                // the chip without manufacturing an empty transcript doc.
                let _ = chip_doc.update_subagent_chip(
                    parent_tool_use_id,
                    None,
                    subagent_chip_update(sub_event),
                    None,
                    Some(observation.has_observation().then_some(&*observation)),
                );
            }
            if let Some(sink) = subagents.get_mut(parent_tool_use_id) {
                if let AgentEvent::UserMessage { text } = sub_event.as_ref() {
                    // A steer splits ENTRIES, not parts — handled at the
                    // sink level (the fold ignores UserMessage).
                    sink.push_user(&device_id, text);
                    if !chip_streaming {
                        let _ = chip_doc.update_subagent_chip(
                            parent_tool_use_id,
                            None,
                            Some("running"),
                            None,
                            Some(None),
                        );
                    }
                    nested_spawns.refreeze(&inner, &chat_id);
                    continue;
                }
                if !metadata_only {
                    luna_doc::fold_event_into_parts(&mut sink.folded, sub_event);
                    luna_doc::apply_journal_output_refs(&sub_id, &mut sink.folded);
                    sink.dirty = true;
                }
                if !chip_streaming
                    && (subagent_chip_update(sub_event).is_some()
                        || matches!(
                            sub_event.as_ref(),
                            AgentEvent::Usage { .. } | AgentEvent::ChildProgress { .. }
                        ))
                {
                    // In-place refresh only on observed status/usage/result.
                    // Text deltas never rewrite the parent doc.
                    let _ = chip_doc.update_subagent_chip(
                        parent_tool_use_id,
                        None,
                        subagent_chip_update(sub_event),
                        None,
                        Some(observation.has_observation().then_some(&*observation)),
                    );
                }
                if done {
                    let status = match sub_event.as_ref() {
                        AgentEvent::Done {
                            status: DoneStatus::Errored,
                            ..
                        } => MessageStatus::Complete,
                        AgentEvent::Done {
                            status: DoneStatus::Interrupted,
                            ..
                        } => MessageStatus::Aborted,
                        _ => MessageStatus::Complete,
                    };
                    let sink = subagents.remove(parent_tool_use_id).expect("checked");
                    let doc_id = sink.doc_id.clone();
                    // FREEZE: the finished transcript uploads as a static R2
                    // blob (`blob/{chatId}/{subDocId}`) so viewers of
                    // finished subagents never wake the doc's room; dropping
                    // the sink unpins the doc for the LRU and the room
                    // idles. The live doc remains the fallback.
                    if let Some(json) = sink.finish(&device_id, status)
                        && let Some(host) = inner.doc_host()
                    {
                        host.upload_tool_sidecar(
                            &chat_id,
                            luna_doc::SidecarPayload {
                                part_id: doc_id,
                                output: Some(json),
                                diff: None,
                            },
                        );
                    }
                }
            }
            nested_spawns.refreeze(&inner, &chat_id);
            continue;
        }

        // Any stream activity proves the run is alive — keep the session's
        // freshness inside the UI's 45s staleness window (throttled), and
        // push the quiesce watchdog's window out.
        inner.touch_session(&chat_id);
        last_stream_activity = tokio::time::Instant::now();
        // The engine's input bridge is the sole authority on input requests:
        // it mints the id and parks the resolver BEFORE emitting the event,
        // so a legitimate id is always pending here. A harness emitting its
        // own copy (a different id no resolver knows) would fold an
        // unanswerable twin chip into the doc — and answering the twin would
        // never resume the run. Dropped BEFORE the parked gate below: letting
        // it un-park the session and then dropping it would disarm the idle
        // reaper with no turn running (a leaked child no reap ever ends).
        if let AgentEvent::InputRequested { request_id, .. } = &event {
            let pending = lock(&inner.runs)
                .get(&chat_id)
                .map(|h| h.pending_inputs.clone());
            let known = pending.is_some_and(|p| lock(&p).contains_key(request_id));
            if !known {
                tracing::warn!(
                    chat = %chat_id,
                    request = %request_id,
                    "dropping harness-emitted InputRequested (unknown id; \
                     the engine input bridge owns this lifecycle)"
                );
                continue;
            }
        }
        // Capacity/occupancy can settle after Done; updating it must not reopen a turn.
        if let AgentEvent::ContextUsage { tokens, window } = &event {
            if let Err(err) = doc_ref.update_context_usage(*tokens, *window) {
                tracing::warn!(%chat_id, error = %err, "context usage write failed");
            }
            continue;
        }
        // Goal status is provider control state, including updates while warm
        // and parked. It never synthesizes a user message or a terminal turn.
        if let AgentEvent::GoalChanged {
            thread_id,
            goal,
            turn_id,
        } = &event
        {
            if harness_id == HarnessId::Codex
                && let (Some(binding), Some(host)) =
                    (inner.current_codex_binding_key(&chat_id), inner.doc_host())
                && binding.provider_thread_id == *thread_id
            {
                // "Goal achieved" belongs to the provider turn whose own
                // update completed the goal (the model's `update_goal`), never
                // to a client control or a resume snapshot (turnId null).
                let achieved = goal.as_ref().filter(|goal| {
                    goal.status == luna_proto::GoalStatus::Complete
                        && turn_id.is_some()
                        && provider_checkpoint_id.as_deref() == turn_id.as_deref()
                });
                let previously_complete = host.context_control(&chat_id).ok().and_then(|state| {
                    state
                        .goal
                        .and_then(|readback| readback.goal)
                        .filter(|previous| {
                            achieved.is_some_and(|goal| {
                                previous.created_at == goal.created_at
                                    && previous.status == luna_proto::GoalStatus::Complete
                            })
                        })
                });
                let readback = luna_proto::GoalReadback {
                    thread_id: thread_id.clone(),
                    binding_generation: binding.generation,
                    goal: goal.clone(),
                    checked_at_ms: now_ms(),
                    continuation_ready: true,
                };
                if let Err(error) = host.persist_goal_readback(&chat_id, &new_id(), &readback) {
                    tracing::warn!(chat = %chat_id, %error, "goal snapshot persistence failed");
                }
                if let Some(goal) = achieved
                    && previously_complete.is_none()
                    && let Err(error) = host.persist_goal_mark(&chat_id, &new_id(), |marks| {
                        marks.mark_achieved(&entry_id, goal.time_used_seconds)
                    })
                {
                    tracing::warn!(chat = %chat_id, %error, "goal achieved mark persistence failed");
                }
            } else if harness_id == HarnessId::ClaudeCode
                && let Some(host) = inner.doc_host()
                && let Some(binding) = crate::claude_ongoing::claude_binding(
                    inner.task_identity.get().and_then(|t| t.view(&chat_id)),
                    &chat_id,
                    &inner.device_id,
                    Some(thread_id.as_str()),
                )
            {
                crate::claude_ongoing::persist_run_goal(
                    host,
                    &chat_id,
                    &entry_id,
                    &binding,
                    goal.as_ref(),
                    turn_id.is_some(),
                );
            }
            continue;
        }
        // CC-14: Claude's own schedules (observation only; never published).
        if let AgentEvent::NativeSchedules {
            session_id,
            schedules,
        } = &event
        {
            if harness_id == HarnessId::ClaudeCode
                && let Some(host) = inner.doc_host()
                && crate::claude_ongoing::claude_binding(
                    inner.task_identity.get().and_then(|t| t.view(&chat_id)),
                    &chat_id,
                    &inner.device_id,
                    Some(session_id.as_str()),
                )
                .is_some()
            {
                crate::claude_ongoing::persist_schedules(host, &chat_id, schedules);
            }
            continue;
        }
        // Native peer (cross-session) observations become attributed System
        // entries, never user turns, and are recorded even while parked. A
        // mid-turn arrival closes the streaming segment first so the reply
        // that follows lands below it.
        if matches!(event, AgentEvent::PeerMessage { .. }) {
            if idle_since.is_none() && !folded.is_empty() {
                if let Err(err) = finish_segment(
                    doc_ref,
                    writer.take(),
                    &entry_id,
                    &device_id,
                    segment_started,
                    &folded,
                    MessageStatus::Complete,
                ) {
                    tracing::warn!(chat = %chat_id, error = %err, "segment finish failed");
                }
                inner.note_message(&chat_id, &folded_text(&folded));
                folded.clear();
                dirty = false;
                entry_id = new_id();
                segment_started = now_ms();
            }
            if let Err(error) =
                crate::claude_peers::record_peer_message(doc_ref, &device_id, &event, now_ms())
            {
                tracing::warn!(chat = %chat_id, %error, "peer message record failed");
            }
            continue;
        }
        // Native autonomous turns can begin immediately after Done. The
        // one-second parked-output gate is for unowned ACP noise, not an
        // authoritative Codex turn boundary.
        if let AgentEvent::ProviderTurnStarted { thread_id, turn_id } = &event {
            if harness_id != HarnessId::Codex
                || inner
                    .current_codex_binding_key(&chat_id)
                    .is_none_or(|binding| binding.provider_thread_id != *thread_id)
            {
                continue;
            }
            inner.rearm_codex_checkpoint_after_done(&chat_id, &run_id, Some(thread_id));
            if !inner.observe_codex_checkpoint(&chat_id, &run_id, Some(thread_id), turn_id) {
                continue;
            }
            provider_checkpoint_id = Some(turn_id.clone());
            if idle_since.take().is_some() {
                self_continued_turn = true;
                entry_id = new_id();
                segment_started = now_ms();
                last_stream_activity = tokio::time::Instant::now();
                inner.set_status(&chat_id, SessionStatus::Working, true);
                let turn_cwd = lock(&inner.last_requests)
                    .get(&chat_id)
                    .map(|request| request.cwd.clone());
                if let Some(cwd) = turn_cwd.as_deref() {
                    if let Some(checkpoints) = inner.git_checkpoints.get() {
                        checkpoints.start(&chat_id, &run_id, cwd).await;
                    }
                    SessionsEngine {
                        inner: inner.clone(),
                    }
                    .note_turn_start(&chat_id, cwd);
                }
            }
            continue;
        }
        if let AgentEvent::ContextCompaction {
            operation_id,
            status,
            thread_id,
            turn_id,
        } = &event
        {
            if harness_id != HarnessId::Codex {
                tracing::warn!(chat = %chat_id, harness = ?harness_id, "ignoring context compaction from unsupported harness");
                continue;
            }
            let binding = inner
                .task_identity
                .get()
                .and_then(|identity| identity.view(&chat_id))
                .and_then(|view| view.current_binding);
            let Some(binding) = binding.filter(|binding| {
                binding.provider_id == HarnessId::Codex
                    && !binding.provider_task_id.is_empty()
                    && binding.provider_task_id == *thread_id
            }) else {
                tracing::warn!(chat = %chat_id, %thread_id, %turn_id, "ignoring stale Codex context compaction binding");
                continue;
            };
            let Some(host) = inner.doc_host() else {
                continue;
            };
            let current = match host.context_control(&chat_id) {
                Ok(current) => current,
                Err(error) => {
                    tracing::warn!(chat = %chat_id, %error, "context compaction state read failed");
                    continue;
                }
            };
            let Some((next, clear_usage)) = reduce_provider_context_compaction(
                &current,
                &binding.provider_task_id,
                binding.generation,
                operation_id,
                *status,
                thread_id,
            ) else {
                tracing::debug!(chat = %chat_id, %operation_id, %turn_id, ?status, "ignoring duplicate or stale Codex context compaction event");
                continue;
            };
            if let Err(error) = host.persist_provider_context_control(
                &chat_id,
                &format!("provider-context:{operation_id}:{status:?}"),
                &next,
                clear_usage,
            ) {
                tracing::warn!(chat = %chat_id, %operation_id, %error, "context compaction state write failed");
            }
            continue;
        }
        if harness_id == HarnessId::Codex && matches!(&event, AgentEvent::Done { .. }) {
            let binding = inner
                .task_identity
                .get()
                .and_then(|identity| identity.view(&chat_id))
                .and_then(|view| view.current_binding)
                .filter(|binding| binding.provider_id == HarnessId::Codex);
            if let (Some(binding), Some(host)) = (binding, inner.doc_host())
                && let Ok(current) = host.context_control(&chat_id)
                && let Some(next) = reduce_provider_context_terminal(
                    &current,
                    &binding.provider_task_id,
                    binding.generation,
                )
            {
                let operation_id = next
                    .compact
                    .operation_id
                    .as_deref()
                    .unwrap_or("unknown-operation");
                if let Err(error) = host.persist_provider_context_control(
                    &chat_id,
                    &format!("provider-context:{operation_id}:terminal:{run_id}"),
                    &next,
                    false,
                ) {
                    tracing::warn!(chat = %chat_id, %operation_id, %error, "terminal Codex context compaction state write failed");
                }
            }
        }
        // PARKED: a steer boundary, a terminal Done, or SELF-CONTINUED OUTPUT
        // re-opens the session; everything else stays gated. The ACP child
        // keeps forwarding `session/update` frames after a turn completes,
        // and they split two ways:
        //
        // - Post-turn NOISE — late tool_call_updates for commands folded in a
        //   prior segment, command refreshes, reasoning heartbeats. Treating
        //   those as "the next turn" re-armed Working with no Done ever
        //   coming (the eternally-running session bug) and folded orphan
        //   parts into a phantom segment. Still dropped.
        // - SELF-CONTINUED WORK — Claude Code re-invokes itself when a
        //   background task finishes (turns no prompt started) and streams
        //   real output for them. Dropping those LOST transcript content
        //   (2026-08-12: "Build finished successfully…" streamed by the
        //   agent, absent from the doc). Fresh text or a genuinely new tool
        //   call resumes the session: new segment, Working, and the turn
        //   settles again via Done — or via the quiesce watchdog, which is
        //   what makes this resume safe where the naive version was not.
        //
        // The RESUME_GATE separates the two by arrival time: a finished
        // turn's tail flush lands within milliseconds of its Done, while a
        // self-continued turn starts a whole new agent round trip (seconds
        // at minimum, minutes in the incident). Inside the gate everything
        // non-boundary stays inert, exactly as before.
        let historical_steer = harness_id == HarnessId::Codex
            && matches!(
                &event,
                AgentEvent::Steered {
                    user_message_id: Some(_),
                    next_assistant_message_id: None,
                    ..
                }
            );
        let turn_was_active = idle_since.is_none();
        const RESUME_GATE: std::time::Duration = std::time::Duration::from_secs(1);
        if idle_since.is_some() && !historical_steer {
            let self_continued = idle_since
                .is_some_and(|parked_at| parked_at.elapsed() >= RESUME_GATE)
                && (matches!(
                    &event,
                    AgentEvent::TextDelta { text } if !text.is_empty()
                ) || matches!(
                    &event,
                    AgentEvent::ToolCall { id, .. }
                        if id == luna_proto::LIVE_PLAN_TOOL_ID || !seen_tools.contains(id)
                ));
            if self_continued {
                tracing::info!(
                    chat = %chat_id,
                    "parked session resumed by self-continued agent output"
                );
                idle_since = None;
                self_continued_turn = true;
                // The park cleared the fold; rotate to a fresh entry and
                // fall through — this event is the new segment's first part.
                entry_id = new_id();
                segment_started = now_ms();
                inner.set_status(&chat_id, SessionStatus::Working, true);
            } else {
                match &event {
                    AgentEvent::Steered { .. } if inner.has_pending_steers(&chat_id, &run_id) => {
                        idle_since = None;
                        inner.set_status(&chat_id, SessionStatus::Working, true);
                    }
                    AgentEvent::Steered { .. } => {
                        tracing::debug!(chat = %chat_id, run = %run_id,
                            "ignoring stale steer boundary after terminal turn");
                        continue;
                    }
                    AgentEvent::Done { .. } => {
                        idle_since = None;
                    }
                    // A question with NO turn behind it (post-turn permission
                    // noise): answer it empty right here. Un-parking into
                    // AwaitingInput would disarm the idle reaper with no Done
                    // ever coming — stranded status, leaked warm child.
                    AgentEvent::InputRequested { request_id, .. } => {
                        let resolver = lock(&inner.runs)
                            .get(&chat_id)
                            .and_then(|h| lock(&h.pending_inputs).remove(request_id));
                        if let Some(pending) = resolver {
                            let _ = pending.resolver.send(Vec::new());
                        }
                        tracing::debug!(chat = %chat_id, "parked session: post-turn input request auto-declined");
                        continue;
                    }
                    // A stale answer settling after its turn already closed:
                    // nothing is running — stay parked.
                    AgentEvent::InputResolved { .. } => continue,
                    _ => continue,
                }
            }
        }
        // Empty reasoning deltas are PURE heartbeats: redacted thinking and
        // tool-input-generation windows stream them with no text. They fold
        // to nothing, so journaling/publishing them is only noise (hundreds
        // per long turn observed) — the touch above already did their job.
        if matches!(&event, AgentEvent::ReasoningDelta { text } if text.is_empty()) {
            continue;
        }

        // Stale tool echoes: a ToolCall/ToolResult naming an id folded in a
        // PRIOR segment (the fold reset at a steer or park since) belongs to
        // a chip that already rendered in its own entry. Folding the echo
        // would splice a phantom chip into the middle of the current
        // segment's streaming text (the mid-word transcript splits). Refresh
        // its original entry; same-segment updates still fold in place.
        let in_segment = |folded: &[MessagePart], id: &str| {
            folded
                .iter()
                .any(|p| matches!(p, MessagePart::Tool { id: pid, .. } if pid == id))
        };
        match &event {
            // The live plan chip is a deliberate singleton (every update
            // reuses `LIVE_PLAN_TOOL_ID` so the fold refreshes in place):
            // treating its reappearance after a park/steer reset as a stale
            // echo dropped the todo list for the rest of the run — from the
            // first boundary on, plans never rendered again.
            AgentEvent::ToolCall { id, .. } if id == luna_proto::LIVE_PLAN_TOOL_ID => {}
            AgentEvent::ToolResult { id, .. } if id == luna_proto::LIVE_PLAN_TOOL_ID => {}
            AgentEvent::ToolCall { id, .. } => {
                let owner = tool_entry_by_id.get(id).cloned().or_else(|| {
                    // Per-file chips discovered by patchUpdated inherit the
                    // original patch's entry, including after a steer split.
                    id.rsplit_once('#')
                        .filter(|(_, suffix)| {
                            !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                        })
                        .and_then(|(root, _)| tool_entry_by_id.get(root))
                        .cloned()
                });
                if !in_segment(&folded, id)
                    && owner
                        .as_deref()
                        .is_some_and(|owner| owner != entry_id.as_str())
                {
                    let owner = owner.expect("checked above");
                    seen_tools.insert(id.clone());
                    tool_entry_by_id
                        .entry(id.clone())
                        .or_insert_with(|| owner.clone());
                    if let Err(error) = inner.publish(&chat_id, &event, run_permit.as_ref()) {
                        journal_failure = Some(error);
                        break SessionStatus::Errored;
                    }
                    if let Err(error) = doc_ref.apply_tool_event_in_entry(&owner, id, &event) {
                        tracing::warn!(chat = %chat_id, tool = %id, %error, "prior tool call could not be refreshed");
                    }
                    continue;
                }
                seen_tools.insert(id.clone());
                tool_entry_by_id
                    .entry(id.clone())
                    .or_insert_with(|| entry_id.clone());
            }
            AgentEvent::ToolActivity { id, .. }
                if !in_segment(&folded, id) && seen_tools.contains(id) =>
            {
                if let Some(original_entry) = tool_entry_by_id.get(id) {
                    if let Err(error) = inner.publish(&chat_id, &event, run_permit.as_ref()) {
                        journal_failure = Some(error);
                        break SessionStatus::Errored;
                    }
                    if let Err(error) =
                        doc_ref.apply_tool_event_in_entry(original_entry, id, &event)
                    {
                        tracing::warn!(chat = %chat_id, tool = %id, %error, "prior tool activity could not be folded");
                    }
                }
                continue;
            }
            AgentEvent::ToolResult {
                id,
                is_error,
                output,
                diff,
            } if !in_segment(&folded, id) && seen_tools.contains(id) => {
                if let Some(original_entry) = tool_entry_by_id.get(id) {
                    // The provider completed a call that predates the steer.
                    // Keep its chip at the original call site instead of
                    // manufacturing a second chip after the user message.
                    if let Err(error) = inner.publish(&chat_id, &event, run_permit.as_ref()) {
                        journal_failure = Some(error);
                        break SessionStatus::Errored;
                    }
                    match doc_ref.resolve_tool_in_entry(
                        original_entry,
                        id,
                        *is_error,
                        output.as_deref(),
                        diff.as_ref(),
                    ) {
                        Ok(true) => {}
                        Ok(false) => tracing::warn!(chat = %chat_id, tool = %id,
                            "prior tool chip missing after steer"),
                        Err(error) => tracing::warn!(chat = %chat_id, tool = %id, %error,
                            "prior tool chip could not be resolved"),
                    }
                }
                continue;
            }
            _ => {}
        }

        // A steer boundary splits the assistant entry exactly where the fold resets.
        if let AgentEvent::Steered {
            user_message_id,
            next_assistant_message_id,
            ..
        } = &event
        {
            // Identity-bearing boundaries match the exact ledger entry. A late
            // native record can recover a durable unknown claim after Done/restart.
            let confirmed_steer = lock(&inner.runs)
                .get(&chat_id)
                .filter(|h| h.run_id == run_id)
                .and_then(|h| {
                    let mut pending = lock(&h.routed_steers);
                    if let Some(id) = user_message_id {
                        let position = pending.iter().position(|steer| &steer.message_id == id)?;
                        if harness_id == HarnessId::Codex {
                            pending.get(position).cloned()
                        } else {
                            pending.remove(position)
                        }
                    } else if harness_id != HarnessId::Codex {
                        pending.pop_front()
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    let id = user_message_id.as_deref()?;
                    let row = inner.doc_host()?.queued_message(&chat_id, id).ok()??;
                    if !matches!(
                        row.status,
                        luna_doc::QueueMessageStatus::Claimed { .. }
                            | luna_doc::QueueMessageStatus::SteeringAccepted { .. }
                            | luna_doc::QueueMessageStatus::DeliveryUnknown { .. }
                    ) {
                        return None;
                    }
                    // Restore typed durable content from the same exact row.
                    Some(RoutedSteer {
                        prompt: row.text,
                        attachments: row.attachments,
                        input_items: row.input_items,
                        message_id: row.id,
                        delivery: luna_harness::SteerDelivery::RequireActiveTurn,
                    })
                });
            if harness_id == HarnessId::Codex && confirmed_steer.is_none() {
                continue;
            }
            if harness_id == HarnessId::Codex && !historical_steer {
                inner.rearm_codex_checkpoint_after_done(
                    &chat_id,
                    &run_id,
                    recovery_session_id.as_deref(),
                );
            }
            if let Err(error) = inner.publish(&chat_id, &event, run_permit.as_ref()) {
                journal_failure = Some(error);
                break SessionStatus::Errored;
            }
            if !historical_steer {
                // A steer boundary means a real prompt owns the turn again — its
                // Done will come; the short self-continued window stands down.
                self_continued_turn = false;
                if let Err(err) = finish_segment(
                    doc_ref,
                    writer.take(),
                    &entry_id,
                    &device_id,
                    segment_started,
                    &folded,
                    MessageStatus::Complete,
                ) {
                    tracing::warn!(chat = %chat_id, error = %err, "segment finish failed");
                }
                inner.note_message(&chat_id, &folded_text(&folded));
                folded.clear();
                dirty = false;
                entry_id = next_assistant_message_id.clone().unwrap_or_else(new_id);
                segment_started = now_ms();
                // The elapsed timer is per user message, not per child process: a
                // steer boundary restarts it (matches the parked-resume path and
                // the composer's optimistic overlay, which already reads 0:00).
                inner.set_status(&chat_id, SessionStatus::Working, true);
            }
            // Native recording confirms this exact input in history, not that
            // a model request has sampled it.
            if let Some(steer) = confirmed_steer
                && let Some(host) = inner.doc_host()
            {
                let turn_cwd = lock(&inner.last_requests)
                    .get(&chat_id)
                    .map(|request| request.cwd.clone());
                if !historical_steer
                    && let Some(cwd) = turn_cwd.as_deref()
                    && let Some(checkpoints) = inner.git_checkpoints.get()
                {
                    checkpoints.end(&chat_id, &run_id, "steered").await;
                    checkpoints.start(&chat_id, &run_id, cwd).await;
                }
                if steer.delivery == luna_harness::SteerDelivery::RequireActiveTurn {
                    let transcript_written = match host.open(&chat_id) {
                        Ok(handle) => handle
                            .write_user_message(&steer.message_id, &steer.prompt, now_ms())
                            .map_err(|error| {
                                tracing::error!(chat = %chat_id, run = %run_id, %error,
                                    "confirmed steer transcript write failed");
                            })
                            .is_ok(),
                        Err(error) => {
                            tracing::error!(chat = %chat_id, run = %run_id, %error,
                                "confirmed steer transcript unavailable");
                            false
                        }
                    };
                    if !transcript_written {
                        if let Err(error) =
                            host.mark_steer_correlation_unknown(&chat_id, &steer.message_id)
                        {
                            tracing::error!(chat = %chat_id, run = %run_id, %error,
                                "confirmed steer unknown-state persistence failed");
                        }
                        continue;
                    }
                    if !historical_steer && let Some(cwd) = turn_cwd.as_deref() {
                        SessionsEngine {
                            inner: inner.clone(),
                        }
                        .note_turn_start(&chat_id, cwd);
                    }
                    inner.note_message(&chat_id, &steer.prompt);
                }
                let binding = inner
                    .task_identity
                    .get()
                    .and_then(|identity| identity.view(&chat_id))
                    .and_then(|view| view.current_binding);
                if let Err(error) = host.confirm_task_command_correlation(
                    &chat_id,
                    &steer.message_id,
                    &run_id,
                    binding.as_ref().map(|binding| binding.binding_id.as_str()),
                    binding.as_ref().map(|binding| binding.generation),
                ) {
                    tracing::error!(chat = %chat_id, run = %run_id, %error,
                        "steer recording receipt finalization failed");
                } else if harness_id == HarnessId::Codex
                    && host
                        .queued_message(&chat_id, &steer.message_id)
                        .is_ok_and(|row| row.is_none())
                    && let Some(handle) = lock(&inner.runs)
                        .get(&chat_id)
                        .filter(|h| h.run_id == run_id)
                {
                    lock(&handle.routed_steers)
                        .retain(|pending| pending.message_id != steer.message_id);
                }
            }
            continue;
        }

        if let AgentEvent::ProviderTaskChanged { change } = &event {
            if let Some(task_identity) = inner.task_identity.get()
                && task_identity.view(&chat_id).is_some()
            {
                let task_identity = task_identity.clone();
                let task = chat_id.clone();
                let change = change.clone();
                tokio::spawn(async move {
                    if let Err(error) = task_identity.reconcile_task(&task, true).await {
                        tracing::warn!(task = %task, %change, %error, "provider notification reconciliation failed");
                    }
                });
            }
            // Control-plane signal only: no transcript/journal entry.
            continue;
        }

        if let AgentEvent::ProviderCheckpoint { checkpoint_id } = &event {
            provider_checkpoint_id = Some(checkpoint_id.clone());
            if harness_id == HarnessId::Codex {
                inner.observe_codex_checkpoint(
                    &chat_id,
                    &run_id,
                    if saw_session_started {
                        recovery_session_id.as_deref()
                    } else {
                        None
                    },
                    checkpoint_id,
                );
            }
            // Provider checkpoint ids are LUNA control-plane metadata. They
            // are persisted on the completed assistant entry below, never in
            // the visible event stream or transcript parts.
            continue;
        }
        if matches!(&event, AgentEvent::Done { .. }) {
            inner.clear_codex_checkpoint(&chat_id, &run_id);
        }
        // A provider-side interruption of the running turn (native approval
        // Cancel reports turn/completed status=interrupted) holds follow-up
        // work exactly like a local Stop: freeze the durable queue through
        // the Stop fence before the Idle transition can drain it, and treat
        // the run as interrupted so unconfirmed mailbox steers are not
        // re-dispatched as fresh turns at run exit. Local Stop already set
        // `interrupted` through the cancel channel and keeps its own path.
        if turn_was_active
            && !interrupted
            && matches!(
                &event,
                AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    ..
                }
            )
        {
            interrupted = true;
            if let Some(host) = inner.doc_host()
                && let Err(error) = host.fence_queue_for_stop(&chat_id).await
            {
                tracing::warn!(chat = %chat_id, run = %run_id, %error,
                    "provider interruption could not hold the queue");
            }
        }

        match &event {
            AgentEvent::SessionStarted {
                session_id, cwd, ..
            } => {
                inner.reset_codex_checkpoint(&chat_id, &run_id);
                saw_session_started = true;
                recovery_session_id = Some(session_id.clone());
                let engine = SessionsEngine {
                    inner: inner.clone(),
                };
                if let Err(error) = engine.mark_recovery_accepted(
                    &chat_id,
                    resume_state.recovery.as_ref(),
                    session_id,
                ) {
                    tracing::warn!(chat = %chat_id, %error, "recovery acceptance persistence failed");
                }
                if let Some(policy) = permission_policy.as_ref()
                    && let Some(workspace) = inner.workspace()
                    && let Ok(Some(chat)) = workspace.chat(&chat_id)
                    && let Some(mut config) = chat.config
                    && config.harness == harness_id
                {
                    // SessionStarted is the first authoritative proof that the
                    // replacement provider accepted this runtime policy. Keep
                    // any newer requested mode, but publish what is active.
                    apply_permission_policy(
                        &mut config,
                        policy,
                        format!("engine:{}", inner.device_id),
                    );
                    if let Err(error) = workspace.set_chat_config(&chat_id, &config) {
                        tracing::warn!(chat = %chat_id, %error, "applied permission policy persistence failed");
                    }
                }
                // The event's own cwd (where the harness actually created the
                // session) scopes the stored id, not the request's.
                if let Some(task_identity) = inner.task_identity.get()
                    && let Err(error) =
                        task_identity.record_session_binding(&chat_id, harness_id, session_id, cwd)
                {
                    inner.fail_codex_checkpoint(&chat_id, &run_id);
                    // Binding conflicts are terminal. Publishing SessionStarted
                    // without its durable mapping would make an unowned native
                    // thread look resumable, so fail before that event escapes.
                    let journal_result = inner
                        .publish(
                            &chat_id,
                            &AgentEvent::Error {
                                message: error.to_string(),
                            },
                            run_permit.as_ref(),
                        )
                        .and_then(|_| {
                            inner.publish(
                                &chat_id,
                                &AgentEvent::Done {
                                    status: DoneStatus::Errored,
                                    result: None,
                                    error: Some(error.to_string()),
                                    session_id: Some(session_id.clone()),
                                },
                                run_permit.as_ref(),
                            )
                        });
                    if let Err(journal_error) = journal_result {
                        stop_after_journal_failure(
                            &inner,
                            &chat_id,
                            &run_id,
                            harness_id,
                            &recovery_request,
                            &resume_state,
                            Some(session_id.clone()),
                            &journal_error,
                        );
                        inner.remove_run(&chat_id, &run_id);
                        return;
                    }
                    inner.remove_run(&chat_id, &run_id);
                    inner.set_status(&chat_id, SessionStatus::Errored, false);
                    let engine = SessionsEngine {
                        inner: inner.clone(),
                    };
                    if let Err(recovery_error) = engine.record_recovery_failure(
                        &chat_id,
                        harness_id,
                        recovery_request.clone(),
                        resume_state.user_message_id.clone(),
                        RecoveryAcceptance::Accepted,
                        RecoveryFailure::InvalidConfiguration,
                        error.to_string(),
                        Some(session_id.clone()),
                        resume_state.recovery.as_ref(),
                    ) {
                        tracing::warn!(chat = %chat_id, %recovery_error, "binding-failure recovery persistence failed");
                    }
                    return;
                }
                if harness_id == HarnessId::Codex {
                    inner.observe_codex_binding(&chat_id, &run_id, session_id);
                }
                if let Some(contract) = runtime_contract.as_ref() {
                    let mut receipt = contract.diagnostics(session_id);
                    receipt.run_id = Some(run_id.clone());
                    receipt.accepted_at_ms = Some(now_ms());
                    if let Err(error) = inner.journal.record_runtime_contract(&receipt) {
                        tracing::error!(chat = %chat_id, %error, "provider-acknowledged LUNA contract receipt could not be persisted");
                    }
                }
                inner.remember_harness_session(&chat_id, session_id, cwd);
                if let Some(host) = inner.doc_host() {
                    let binding = inner
                        .task_identity
                        .get()
                        .and_then(|identity| identity.view(&chat_id))
                        .and_then(|view| view.current_binding);
                    if let Ok(mut context) = host.context_control(&chat_id)
                        && context.pending_handoff.is_some()
                    {
                        context.pending_handoff = None;
                        context.binding_generation =
                            binding.as_ref().map(|binding| binding.generation);
                        context.notice = Some(
                            "The user-selected handoff was accepted by the fresh provider binding."
                                .into(),
                        );
                        if let Err(error) = host.persist_context_control(
                            &chat_id,
                            &format!("handoff-accepted:{session_id}"),
                            &context,
                        ) {
                            tracing::warn!(chat = %chat_id, %error, "handoff acceptance persistence failed");
                        }
                    }
                    if let Err(error) = host.confirm_task_command_correlation(
                        &chat_id,
                        &resume_state.user_message_id,
                        &run_id,
                        binding.as_ref().map(|binding| binding.binding_id.as_str()),
                        binding.as_ref().map(|binding| binding.generation),
                    ) {
                        tracing::error!(chat = %chat_id, run = %run_id, %error,
                            "provider acceptance receipt finalization failed");
                    }
                }
                if harness_id == HarnessId::Codex
                    && let Some(task_identity) = inner.task_identity.get()
                    && let Some(title) = inner
                        .workspace()
                        .and_then(|workspace| workspace.chat(&chat_id).ok().flatten())
                        .and_then(|chat| chat.title)
                        .filter(|title| !title.trim().is_empty())
                {
                    let task_identity = task_identity.clone();
                    let task = chat_id.clone();
                    tokio::spawn(async move {
                        if let Err(error) = task_identity.set_provider_name(&task, &title).await {
                            tracing::warn!(task = %task, %error, "provider title synchronization failed");
                            return;
                        }
                        let _ = task_identity.reconcile_task(&task, true).await;
                    });
                }
            }
            AgentEvent::Done {
                session_id: Some(session_id),
                ..
            } => {
                recovery_session_id = Some(session_id.clone());
                inner.remember_harness_session(&chat_id, session_id, &run_cwd);
            }
            AgentEvent::Error { message } => {
                last_error_message = Some(message.clone());
            }
            AgentEvent::InputRequested { questions, .. } => {
                // Known-id guaranteed: the unknown-id twin was dropped above,
                // before the parked gate.
                inner.set_status(
                    &chat_id,
                    if questions.iter().any(|question| question.approval.is_some()) {
                        SessionStatus::AwaitingApproval
                    } else {
                        SessionStatus::AwaitingInput
                    },
                    false,
                );
            }
            AgentEvent::InputResolved { .. } => {
                // One completed form must not hide another concurrent prompt.
                let remaining = lock(&inner.runs)
                    .get(&chat_id)
                    .map(|handle| handle.pending_inputs.clone());
                let status = remaining
                    .as_ref()
                    .and_then(|pending| {
                        let pending = lock(pending);
                        if pending
                            .values()
                            .any(|input| input.questions.iter().any(|q| q.approval.is_some()))
                        {
                            Some(SessionStatus::AwaitingApproval)
                        } else if !pending.is_empty() {
                            Some(SessionStatus::AwaitingInput)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(SessionStatus::Working);
                inner.set_status(&chat_id, status, false);
            }
            _ => {}
        }

        if let Err(error) = inner.publish(&chat_id, &event, run_permit.as_ref()) {
            journal_failure = Some(error);
            break SessionStatus::Errored;
        }

        if matches!(&event, AgentEvent::Done { .. })
            && let Some(task_identity) = inner.task_identity.get()
            && task_identity.view(&chat_id).is_some()
        {
            let task_identity = task_identity.clone();
            let task = chat_id.clone();
            tokio::spawn(async move {
                let _ = task_identity.reconcile_task(&task, false).await;
            });
        }

        // Defensive rule from luna: a mid-run SessionStarted re-emission (Claude SDK
        // background re-invocations) must not wipe the segment being written.
        let skip_fold = matches!(&event, AgentEvent::SessionStarted { .. }) && !folded.is_empty();
        if !skip_fold {
            fold_event_into_parts(&mut folded, &event);
            // The small output preview lives in the doc; an output ref points
            // back to the host journal for lazy full-text reads. No R2 upload.
            luna_doc::apply_journal_output_refs(&chat_id, &mut folded);
        }

        if let AgentEvent::Done { status, error, .. } = &event {
            if let Some(checkpoints) = inner.git_checkpoints.get() {
                let outcome = match status {
                    DoneStatus::Completed => "completed",
                    DoneStatus::Interrupted => "interrupted",
                    DoneStatus::Errored => "errored",
                };
                checkpoints.end(&chat_id, &run_id, outcome).await;
            }
            // A question still pending at turn end can never be legitimately
            // answered (its turn is over): drain the resolvers NOW, or a late
            // `respond_input` finds one, emits InputResolved, and un-parks
            // the session into Working with no turn behind it — stranded
            // Working, timer forever, reaper disarmed. Empty answers unblock
            // the harness-side bridge like an interrupt does.
            let pending = lock(&inner.runs)
                .get(&chat_id)
                .filter(|h| h.run_id == run_id)
                .map(|h| h.pending_inputs.clone());
            if let Some(pending) = pending {
                for (_, pending) in lock(&pending).drain() {
                    let _ = pending.resolver.send(Vec::new());
                }
            }
            let message_status = match status {
                DoneStatus::Interrupted => MessageStatus::Aborted,
                DoneStatus::Completed | DoneStatus::Errored => MessageStatus::Complete,
            };
            // No dangling chips: a run that ends for ANY reason (completed,
            // errored, interrupted) terminally resolves its input parts — an
            // unresolved question must not outlive the run that asked it
            // (its resolver died with the run; an answer could never land).
            for part in folded.iter_mut() {
                if let MessagePart::Input { resolved, .. } = part {
                    *resolved = true;
                }
            }
            // A Done landing on a PARKED session with nothing streamed (the
            // idle reaper's or an interrupt's own teardown) has no entry to
            // finalize — writing one would leave an empty aborted stub.
            let nothing_streamed = writer.is_none() && folded.is_empty();
            if !nothing_streamed {
                if let Err(err) = finish_segment(
                    doc_ref,
                    writer.take(),
                    &entry_id,
                    &device_id,
                    segment_started,
                    &folded,
                    message_status,
                ) {
                    tracing::warn!(chat = %chat_id, error = %err, "final segment finish failed");
                }
                if *status == DoneStatus::Completed
                    && let Some(checkpoint_id) = provider_checkpoint_id.take()
                    && let Err(error) = doc_ref.set_provider_checkpoint(&entry_id, &checkpoint_id)
                {
                    tracing::warn!(chat = %chat_id, %error, "provider checkpoint persistence failed");
                }
                inner.note_message(&chat_id, &folded_text(&folded));
            }
            if *status == DoneStatus::Completed {
                // A cleanly completed turn resets the auto-resume revival
                // budget: only consecutive crash-revive-crash cycles spend it.
                inner.journal.clear_resume_attempts(&chat_id);
            }
            let engine = SessionsEngine {
                inner: inner.clone(),
            };
            match status {
                DoneStatus::Completed => {
                    if let Err(error) =
                        engine.complete_recovery(&chat_id, resume_state.recovery.as_ref())
                    {
                        tracing::warn!(chat = %chat_id, %error, "recovery completion persistence failed");
                    }
                }
                DoneStatus::Interrupted
                    if resume_state.recovery.is_some()
                        && hold_recovery_on_interrupt.load(Ordering::Acquire) =>
                {
                    if let Err(error) = engine.hold_recovery(
                        &chat_id,
                        "Recovery held after interruption. Queued work and partial output were preserved.",
                    ) {
                        tracing::warn!(chat = %chat_id, %error, "recovery hold persistence failed");
                    }
                }
                DoneStatus::Errored => {
                    let message = error
                        .clone()
                        .or_else(|| last_error_message.clone())
                        .unwrap_or_else(|| "provider turn failed".into());
                    let acceptance = if saw_session_started {
                        RecoveryAcceptance::Accepted
                    } else {
                        RecoveryAcceptance::Unknown
                    };
                    if let Err(recovery_error) = engine.record_recovery_failure(
                        &chat_id,
                        harness_id,
                        recovery_request.clone(),
                        resume_state.user_message_id.clone(),
                        acceptance,
                        classify_failure(&message),
                        message,
                        recovery_session_id.clone(),
                        resume_state.recovery.as_ref(),
                    ) {
                        tracing::warn!(chat = %chat_id, %recovery_error, "turn recovery persistence failed");
                    }
                }
                DoneStatus::Interrupted => {}
            }
            // Exchange completed on an untitled chat → name it (fire-and-forget;
            // interrupted/errored turns never trigger naming).
            if *status == DoneStatus::Completed
                && let Some(titles) = inner.titles.get()
            {
                titles.maybe_generate(&chat_id, harness_id, &user_prompt, &run_cwd);
            }
            // Explicit native acceptance belongs to THIS provider turn. A
            // persistent warm stream may stay open after Done, so waiting for
            // EOF would leave its ledger entry blocking every later user turn.
            // Retire it now as unknown/no-replay. Ordinary warm routes retain
            // their legacy continuation/fallback semantics.
            let mut terminal_explicit = inner.take_terminal_explicit_steers(&chat_id, &run_id);
            if harness_id == HarnessId::Codex
                && let Some(handle) = lock(&inner.runs)
                    .get(&chat_id)
                    .filter(|h| h.run_id == run_id)
            {
                // Warm native mailbox delivery may also be unresolved when the
                // old turn ends. Preserve its fallback ownership, but show the
                // queue claim as unknown until exact evidence arrives.
                terminal_explicit.extend(lock(&handle.routed_steers).iter().cloned());
            }
            if let Some(host) = inner.doc_host() {
                for steer in terminal_explicit {
                    if let Err(error) =
                        host.mark_steer_correlation_unknown(&chat_id, &steer.message_id)
                    {
                        tracing::error!(chat = %chat_id, run = %run_id, %error,
                            "terminal explicit steer unknown-state persistence failed");
                    }
                }
            }
            // An ordinary warm steer awaiting its boundary owns the
            // continuation; explicit entries were finalized above.
            let pending_steer = inner.has_pending_steers(&chat_id, &run_id);
            let completed_turn = (*status == DoneStatus::Completed
                && !interrupted
                && turn_was_active
                && !pending_steer)
                .then(|| entry_id.clone());
            // PERSISTENT SESSION: a cleanly completed turn on a steerable
            // harness PARKS instead of ending — child + mailbox stay warm for
            // the next routed dispatch; per-turn state resets for it.
            if *status == DoneStatus::Completed && steerable && !interrupted {
                folded.clear();
                dirty = false;
                entry_id = new_id();
                segment_started = now_ms();
                // Resume-retry is strictly a first-turn concern.
                saw_session_started = true;
                idle_since = Some(tokio::time::Instant::now());
                self_continued_turn = false;
                inner.set_status_with_completion(
                    &chat_id,
                    SessionStatus::Idle,
                    false,
                    completed_turn,
                );
                continue;
            }
            final_completed_turn = completed_turn;
            break match status {
                DoneStatus::Errored => SessionStatus::Errored,
                _ => SessionStatus::Idle,
            };
        }

        if !folded.is_empty() && !dirty {
            dirty = true;
            flush_at =
                tokio::time::Instant::now() + std::time::Duration::from_millis(STREAM_COMMIT_MS);
        }
    };

    if let Some(error) = journal_failure.as_ref() {
        stop_after_journal_failure(
            &inner,
            &chat_id,
            &run_id,
            harness_id,
            &recovery_request,
            &resume_state,
            recovery_session_id.clone(),
            error,
        );
        interrupted = true; // no accepted steer may be replayed after uncertain journaling
    }

    // Parent stream closure says nothing about uncompleted provider children.
    // This also covers metadata-only children that never opened a transcript.
    for parent_id in child_activity.keys() {
        if !settled_subagents.contains(parent_id) {
            let nested =
                nested_spawns.chip_doc(parent_id, &inner, &chat_id, &mut subagents, &device_id);
            let _ = nested.as_deref().unwrap_or(doc_ref).update_subagent_chip(
                parent_id,
                None,
                Some("unobserved"),
                None,
                None,
            );
        }
    }
    nested_spawns.refreeze(&inner, &chat_id);
    // Any child transcript still streaming freezes as-is: the parent event
    // stream is gone, so this sink cannot receive more content in this run.
    for (_, sink) in subagents.drain() {
        let doc_id = sink.doc_id.clone();
        if let Some(json) = sink.finish(&device_id, MessageStatus::Aborted)
            && let Some(host) = inner.doc_host()
        {
            host.upload_tool_sidecar(
                &chat_id,
                luna_doc::SidecarPayload {
                    part_id: doc_id,
                    output: Some(json),
                    diff: None,
                },
            );
        }
    }

    // Claim any accepted-but-unconfirmed steers BEFORE the handle goes away:
    // a routed send that raced this exit either finds its entry gone (we own
    // it — re-dispatched below) or reclaims it and starts a fresh run itself.
    let orphans: Vec<RoutedSteer> = lock(&inner.runs)
        .get(&chat_id)
        .filter(|h| h.run_id == run_id)
        .map(|h| std::mem::take(&mut *lock(&h.routed_steers)).into())
        .unwrap_or_default();
    if harness_id == HarnessId::Codex
        && let Some(host) = inner.doc_host()
        && let Ok(state) = host.context_control(&chat_id)
        && let Some(mut goal) = state.goal
    {
        goal.continuation_ready = false;
        if let Err(error) = host.persist_goal_readback(&chat_id, &new_id(), &goal) {
            tracing::warn!(chat = %chat_id, %error, "goal subscription retirement persistence failed");
        }
    }
    inner.remove_run(&chat_id, &run_id);
    inner.set_status_with_completion(&chat_id, final_status, false, final_completed_turn);
    let (redispatch, explicit): (Vec<_>, Vec<_>) = orphans.into_iter().partition(|steer| {
        harness_id != HarnessId::Codex
            && steer.delivery == luna_harness::SteerDelivery::AllowFollowUpTurn
    });
    if let Some(host) = inner.doc_host() {
        for steer in explicit {
            if let Err(error) = host.mark_steer_correlation_unknown(&chat_id, &steer.message_id) {
                tracing::error!(chat = %chat_id, %error,
                    "explicit steer unknown-state persistence failed");
            }
        }
    }
    if !interrupted && !redispatch.is_empty() {
        // The dying run accepted these into its mailbox but never confirmed a
        // Steered boundary (idle-reaper race, a mid-turn error discarding
        // queued boundary steers, a parked child death). Their user entries
        // are already in the transcript — a message that shows as sent must
        // never silently not run. Re-dispatch each as a fresh turn
        // (write_user_message dedupes by id; resume is engine-injected).
        let engine = SessionsEngine {
            inner: inner.clone(),
        };
        let chat = chat_id.clone();
        tokio::spawn(async move {
            for steer in redispatch {
                let Some(mut request) = engine.last_request(&chat) else {
                    tracing::warn!(chat = %chat, "orphaned steer lost: no run config to re-dispatch");
                    break;
                };
                request.review = None;
                request.goal = None;
                request.prompt = steer.prompt.clone();
                request.resume = None;
                request.attachments = steer.attachments.clone();
                request.input_items = steer.input_items.clone();
                tracing::info!(chat = %chat, "re-dispatching steer orphaned by a dying run");
                let Some(host) = engine.inner.doc_host() else {
                    tracing::warn!(chat = %chat, "orphaned steer lost: doc host unavailable");
                    break;
                };
                let original_w = match engine
                    .inner
                    .recovery
                    .execution_workspace_repair_revision(&chat, &steer.message_id)
                {
                    Ok(revision) => revision,
                    Err(error) => {
                        tracing::warn!(chat = %chat, %error, "orphaned steer lost its original workspace authority");
                        break;
                    }
                };
                if let Err(err) = host
                    .dispatch_with_source_context(
                        &engine,
                        &chat,
                        harness_id,
                        request,
                        Some(steer.message_id.clone()),
                        original_w,
                    )
                    .await
                {
                    tracing::warn!(chat = %chat, error = %err, "orphaned steer re-dispatch failed");
                    engine
                        .inner
                        .set_status(&chat, SessionStatus::Errored, false);
                    break;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LunaApprovalOutcome, LunaSyntheticArchiveGrant, LunaSyntheticArchiveProposal, PendingInput,
        PendingInputKind, PendingInputs, ProviderBindingKey, ProviderTurnReadiness, RunHandle,
        RunResumeState, RuntimeConfig, SessionsEngine, approval_answer, expire_pending_approval,
        lock, reduce_provider_context_compaction, reduce_provider_context_terminal,
        stop_after_journal_failure, subagent_doc_id, take_pending_ordinary_input,
    };
    use futures::StreamExt as _;
    use luna_doc::{MessageRole, MessageStatus};
    use luna_proto::{
        ContextControlState, ContextOperationStatus, HarnessId, Model, ReasoningLevel,
        RecoveryAcceptance, RecoveryAction, RecoveryFailure, RecoveryStatus, RunRequest,
        SandboxLevel, Session, SessionStatus, SteeringMode, TaskRecovery,
    };
    use luna_sync::DocsStore;
    use std::collections::{HashSet, VecDeque};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct HoldingJournalHarness {
        release_first_turn: Arc<tokio::sync::Notify>,
        steer_boundary: Option<Arc<tokio::sync::Notify>>,
        release_steered_turn: Option<Arc<tokio::sync::Notify>>,
    }

    async fn scripted_steer(
        tx: &tokio::sync::mpsc::Sender<Result<luna_proto::AgentEvent, luna_harness::HarnessError>>,
        mut message: luna_harness::SteerMessage,
        token: &luna_harness::CancellationToken,
        boundary: Option<&Arc<tokio::sync::Notify>>,
        release: Option<&Arc<tokio::sync::Notify>>,
    ) -> bool {
        if let Some(acceptance) = message.acceptance.take() {
            let _ = acceptance.send(luna_harness::SteerAcceptance::Accepted);
        }
        if let Some(boundary) = boundary {
            tokio::select! {
                _ = boundary.notified() => {}
                _ = token.cancelled() => return false,
            }
        }
        if tx
            .send(Ok(luna_proto::AgentEvent::Steered {
                user_message_id: None,
                assistant_message_id: None,
                next_assistant_message_id: None,
            }))
            .await
            .is_err()
        {
            return false;
        }
        if tx
            .send(Ok(luna_proto::AgentEvent::TextDelta {
                text: "steered".into(),
            }))
            .await
            .is_err()
        {
            return false;
        }
        if let Some(release) = release {
            tokio::select! {
                _ = release.notified() => {}
                _ = token.cancelled() => return false,
            }
        }
        tx.send(Ok(luna_proto::AgentEvent::Done {
            status: luna_proto::DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("synthetic-session".into()),
        }))
        .await
        .is_ok()
    }

    #[async_trait::async_trait]
    impl luna_harness::Harness for HoldingJournalHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Synthetic journal holder"
        }
        fn supports_steering(&self) -> bool {
            true
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::StepBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[ReasoningLevel::Medium]
        }
        async fn models(&self) -> Result<Vec<Model>, luna_harness::HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            _request: RunRequest,
            controls: luna_harness::RunControls,
        ) -> Result<
            futures::stream::BoxStream<
                'static,
                Result<luna_proto::AgentEvent, luna_harness::HarnessError>,
            >,
            luna_harness::HarnessError,
        > {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            let release = self.release_first_turn.clone();
            let steer_boundary = self.steer_boundary.clone();
            let release_steered = self.release_steered_turn.clone();
            let token = controls.interrupt.clone();
            let mut steering = controls.steering;
            tokio::spawn(async move {
                let started = luna_proto::AgentEvent::SessionStarted {
                    harness: HarnessId::Mock,
                    model: "synthetic".into(),
                    tools: vec![],
                    cwd: "synthetic".into(),
                    session_id: "synthetic-session".into(),
                    assistant_message_id: "assistant-1".into(),
                };
                if tx.send(Ok(started)).await.is_err() {
                    return;
                }
                if tx
                    .send(Ok(luna_proto::AgentEvent::TextDelta {
                        text: "first".into(),
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::select! {
                    _ = release.notified() => {
                        if tx.send(Ok(luna_proto::AgentEvent::Done { status: luna_proto::DoneStatus::Completed, result: None, error: None, session_id: Some("synthetic-session".into()) })).await.is_err() { return; }
                    }
                    Some(message) = steering.recv() => {
                        if !scripted_steer(&tx, message, &token, steer_boundary.as_ref(), release_steered.as_ref()).await { return; }
                    }
                    _ = token.cancelled() => {
                        let _ = tx.send(Ok(luna_proto::AgentEvent::Done { status: luna_proto::DoneStatus::Interrupted, result: None, error: None, session_id: None })).await;
                        return;
                    }
                }
                loop {
                    tokio::select! {
                        _ = token.cancelled() => {
                            let _ = tx.send(Ok(luna_proto::AgentEvent::Done { status: luna_proto::DoneStatus::Interrupted, result: None, error: None, session_id: None })).await;
                            break;
                        }
                        Some(message) = steering.recv() => {
                            if !scripted_steer(&tx, message, &token, steer_boundary.as_ref(), release_steered.as_ref()).await { break; }
                        }
                    }
                }
            });
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }

    fn hermetic_journal_engine(
        root: &std::path::Path,
        release: Arc<tokio::sync::Notify>,
    ) -> crate::EngineCore {
        hermetic_scripted_engine(root, release, None, None)
    }

    fn hermetic_scripted_engine(
        root: &std::path::Path,
        release: Arc<tokio::sync::Notify>,
        steer_boundary: Option<Arc<tokio::sync::Notify>>,
        release_steered_turn: Option<Arc<tokio::sync::Notify>>,
    ) -> crate::EngineCore {
        let registry = Arc::new(crate::registry::HarnessRegistry::new());
        registry.register(Arc::new(HoldingJournalHarness {
            release_first_turn: release,
            steer_boundary,
            release_steered_turn,
        }));
        let data = root.join("synthetic-engine");
        let accounts = crate::agent_accounts::AgentAccountsConfig {
            data_dir: root.join("synthetic-provider-state"),
            claude_config_dir: root.join("synthetic-claude"),
            claude_config_file: root.join("synthetic-claude.json"),
            codex_home: root.join("synthetic-codex"),
        };
        crate::EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            crate::EngineProfile::development(&data, "synthetic-org", "synthetic-user"),
            registry,
            HarnessId::Mock,
            None,
            accounts,
        )
        .unwrap()
    }

    fn synthetic_journal_request(root: &std::path::Path, prompt: &str) -> RunRequest {
        let mut request = request();
        request.prompt = prompt.into();
        let workspace = root.join("synthetic-workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        request.cwd = workspace.to_string_lossy().into_owned();
        request
    }

    async fn journal_event(
        receiver: &mut tokio::sync::broadcast::Receiver<super::JournaledEvent>,
        predicate: impl Fn(&luna_proto::AgentEvent) -> bool,
    ) -> u64 {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let event = receiver.recv().await.unwrap();
                assert!(event.seq > 0, "live fanout must follow a confirmed append");
                if predicate(&event.event) {
                    return event.seq;
                }
            }
        })
        .await
        .unwrap()
    }

    fn fixture_git(root: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn checkpoint_repo(root: &std::path::Path) -> String {
        let path = root.join("synthetic-workspace");
        std::fs::create_dir_all(&path).unwrap();
        fixture_git(&path, &["init", "-q", "-b", "main"]);
        fixture_git(&path, &["config", "user.name", "LUNA Fixture"]);
        fixture_git(&path, &["config", "user.email", "fixture@example.invalid"]);
        std::fs::write(path.join("base.txt"), "base\n").unwrap();
        fixture_git(&path, &["add", "base.txt"]);
        fixture_git(&path, &["commit", "-q", "-m", "base"]);
        path.to_string_lossy().into_owned()
    }

    async fn checkpoint_rows(
        checkpoints: &crate::git_checkpoints::GitCheckpoints,
        cwd: &str,
        count: usize,
        finished: usize,
    ) -> Vec<luna_proto::GitCheckpoint> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let rows = checkpoints
                    .list("journal-chat", cwd, false)
                    .await
                    .unwrap()
                    .checkpoints;
                if rows.len() == count
                    && rows.iter().filter(|row| row.after_available).count() == finished
                {
                    return rows;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn git_checkpoint_fresh_and_warm_turns_follow_confirmed_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let cwd = checkpoint_repo(root.path());
        let release_first = Arc::new(tokio::sync::Notify::new());
        let boundary = Arc::new(tokio::sync::Notify::new());
        let release_next = Arc::new(tokio::sync::Notify::new());
        let core = hermetic_scripted_engine(
            root.path(),
            release_first.clone(),
            Some(boundary.clone()),
            Some(release_next.clone()),
        );
        let checkpoints = core.sessions.git_checkpoints().unwrap().clone();
        let (_, mut live) = core.sessions.subscribe("journal-chat", 0).unwrap();
        let first = core
            .sessions
            .dispatch(
                "journal-chat",
                HarnessId::Mock,
                synthetic_journal_request(root.path(), "first"),
                None,
            )
            .await
            .unwrap();
        journal_event(
            &mut live,
            |event| matches!(event, luna_proto::AgentEvent::TextDelta { text } if text == "first"),
        )
        .await;
        std::fs::write(std::path::Path::new(&cwd).join("first.txt"), "first\n").unwrap();
        release_first.notify_one();
        journal_event(&mut live, |event| {
            matches!(
                event,
                luna_proto::AgentEvent::Done {
                    status: luna_proto::DoneStatus::Completed,
                    ..
                }
            )
        })
        .await;
        let first_rows = checkpoint_rows(&checkpoints, &cwd, 1, 1).await;
        assert_eq!(first_rows[0].outcome.as_deref(), Some("completed"));
        let warm = core
            .sessions
            .dispatch(
                "journal-chat",
                HarnessId::Mock,
                synthetic_journal_request(root.path(), "warm"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(warm, first);
        assert_eq!(
            checkpoint_rows(&checkpoints, &cwd, 1, 1).await.len(),
            1,
            "mailbox acceptance must not create a pre-boundary checkpoint"
        );
        boundary.notify_one();
        journal_event(&mut live, |event| {
            matches!(event, luna_proto::AgentEvent::Steered { .. })
        })
        .await;
        let rows = checkpoint_rows(&checkpoints, &cwd, 2, 1).await;
        assert!(
            rows.iter()
                .any(|row| row.outcome.as_deref() == Some("in progress"))
        );
        std::fs::write(std::path::Path::new(&cwd).join("warm.txt"), "warm\n").unwrap();
        release_next.notify_one();
        journal_event(&mut live, |event| {
            matches!(
                event,
                luna_proto::AgentEvent::Done {
                    status: luna_proto::DoneStatus::Completed,
                    ..
                }
            )
        })
        .await;
        let rows = checkpoint_rows(&checkpoints, &cwd, 2, 2).await;
        let first_diff = checkpoints
            .diff("journal-chat", &cwd, &first_rows[0].turn_id)
            .await
            .unwrap();
        let warm_row = rows
            .iter()
            .find(|row| row.turn_id != first_rows[0].turn_id)
            .unwrap();
        let warm_diff = checkpoints
            .diff("journal-chat", &cwd, &warm_row.turn_id)
            .await
            .unwrap();
        assert_eq!(first_diff.paths, ["first.txt"]);
        assert_eq!(warm_diff.paths, ["warm.txt"]);
        assert!(
            rows.iter()
                .all(|row| row.outcome.as_deref() == Some("completed") && row.error.is_none())
        );
        core.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn git_checkpoint_replacement_done_cannot_close_new_run() {
        let root = tempfile::tempdir().unwrap();
        let cwd = checkpoint_repo(root.path());
        let release = Arc::new(tokio::sync::Notify::new());
        let core = hermetic_journal_engine(root.path(), release.clone());
        let checkpoints = core.sessions.git_checkpoints().unwrap().clone();
        let (_, mut live) = core.sessions.subscribe("journal-chat", 0).unwrap();
        let old_run = core
            .sessions
            .dispatch(
                "journal-chat",
                HarnessId::Mock,
                synthetic_journal_request(root.path(), "old"),
                None,
            )
            .await
            .unwrap();
        journal_event(
            &mut live,
            |event| matches!(event, luna_proto::AgentEvent::TextDelta { text } if text == "first"),
        )
        .await;
        std::fs::write(std::path::Path::new(&cwd).join("old.txt"), "old\n").unwrap();
        let mut replacement = synthetic_journal_request(root.path(), "replacement");
        replacement.model = Some("changed-model".into());
        let new_run = core
            .sessions
            .dispatch("journal-chat", HarnessId::Mock, replacement, None)
            .await
            .unwrap();
        assert_ne!(old_run, new_run);
        let rows = checkpoint_rows(&checkpoints, &cwd, 2, 1).await;
        let old = rows
            .iter()
            .find(|row| row.outcome.as_deref() == Some("interrupted"))
            .unwrap();
        assert!(old.before_available && old.after_available);
        let new = rows
            .iter()
            .find(|row| row.outcome.as_deref() == Some("in progress"))
            .unwrap();
        assert!(new.before_available && !new.after_available);
        std::fs::write(std::path::Path::new(&cwd).join("new.txt"), "new\n").unwrap();
        release.notify_one();
        journal_event(&mut live, |event| {
            matches!(
                event,
                luna_proto::AgentEvent::Done {
                    status: luna_proto::DoneStatus::Completed,
                    ..
                }
            )
        })
        .await;
        let rows = checkpoint_rows(&checkpoints, &cwd, 2, 2).await;
        let old_diff = checkpoints
            .diff("journal-chat", &cwd, &old.turn_id)
            .await
            .unwrap();
        let new_diff = checkpoints
            .diff("journal-chat", &cwd, &new.turn_id)
            .await
            .unwrap();
        assert_eq!(old_diff.paths, ["old.txt"]);
        assert_eq!(new_diff.paths, ["new.txt"]);
        assert!(
            rows.iter().any(
                |row| row.turn_id == new.turn_id && row.outcome.as_deref() == Some("completed")
            )
        );
        core.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn git_checkpoint_explicit_steer_and_rejected_dispatch_keep_turns_distinct() {
        let root = tempfile::tempdir().unwrap();
        let cwd = checkpoint_repo(root.path());
        let release_first = Arc::new(tokio::sync::Notify::new());
        let boundary = Arc::new(tokio::sync::Notify::new());
        let release_next = Arc::new(tokio::sync::Notify::new());
        let core = hermetic_scripted_engine(
            root.path(),
            release_first,
            Some(boundary.clone()),
            Some(release_next.clone()),
        );
        let checkpoints = core.sessions.git_checkpoints().unwrap().clone();
        let (_, mut live) = core.sessions.subscribe("journal-chat", 0).unwrap();
        core.sessions
            .dispatch(
                "journal-chat",
                HarnessId::Mock,
                synthetic_journal_request(root.path(), "first"),
                None,
            )
            .await
            .unwrap();
        journal_event(
            &mut live,
            |event| matches!(event, luna_proto::AgentEvent::TextDelta { text } if text == "first"),
        )
        .await;
        std::fs::write(std::path::Path::new(&cwd).join("pre.txt"), "pre\n").unwrap();
        assert_eq!(
            core.sessions
                .steer("journal-chat", "explicit", None)
                .await
                .unwrap(),
            super::SteerOutcome::Accepted
        );
        let rejected = core
            .sessions
            .dispatch(
                "journal-chat",
                HarnessId::Mock,
                synthetic_journal_request(root.path(), "rejected while steer pending"),
                None,
            )
            .await;
        assert!(rejected.is_err());
        assert_eq!(checkpoint_rows(&checkpoints, &cwd, 1, 0).await.len(), 1);
        boundary.notify_one();
        journal_event(&mut live, |event| {
            matches!(event, luna_proto::AgentEvent::Steered { .. })
        })
        .await;
        let rows = checkpoint_rows(&checkpoints, &cwd, 2, 1).await;
        let prior = rows
            .iter()
            .find(|row| row.outcome.as_deref() == Some("steered"))
            .unwrap();
        let current = rows
            .iter()
            .find(|row| row.outcome.as_deref() == Some("in progress"))
            .unwrap();
        std::fs::write(
            std::path::Path::new(&cwd).join("explicit.txt"),
            "explicit\n",
        )
        .unwrap();
        release_next.notify_one();
        journal_event(&mut live, |event| {
            matches!(
                event,
                luna_proto::AgentEvent::Done {
                    status: luna_proto::DoneStatus::Completed,
                    ..
                }
            )
        })
        .await;
        let rows = checkpoint_rows(&checkpoints, &cwd, 2, 2).await;
        assert_eq!(
            checkpoints
                .diff("journal-chat", &cwd, &prior.turn_id)
                .await
                .unwrap()
                .paths,
            ["pre.txt"]
        );
        assert_eq!(
            checkpoints
                .diff("journal-chat", &cwd, &current.turn_id)
                .await
                .unwrap()
                .paths,
            ["explicit.txt"]
        );
        assert!(rows.iter().any(
            |row| row.turn_id == current.turn_id && row.outcome.as_deref() == Some("completed")
        ));
        core.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn freeze_before_dispatch_waits_without_accepting_a_run() {
        let root = tempfile::tempdir().unwrap();
        let release = Arc::new(tokio::sync::Notify::new());
        let core = hermetic_journal_engine(root.path(), release);
        let epoch = core.device_write.epoch_for_tests();
        let snapshot = epoch
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        let sessions = core.sessions.clone();
        let request = synthetic_journal_request(root.path(), "first");
        let mut dispatch = tokio::spawn(async move {
            sessions
                .dispatch("journal-chat", HarnessId::Mock, request, None)
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut dispatch)
                .await
                .is_err()
        );
        assert!(!core.sessions.has_live_run("journal-chat"));
        assert!(
            core.sessions
                .subscribe("journal-chat", 0)
                .unwrap()
                .0
                .is_empty()
        );
        drop(snapshot);
        let run_id = dispatch.await.unwrap().unwrap();
        assert!(!run_id.is_empty());
        assert!(core.sessions.interrupt("journal-chat").await.unwrap());
        core.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn active_and_parked_run_timeout_freeze_then_interrupt_releases_ticket() {
        let root = tempfile::tempdir().unwrap();
        let release = Arc::new(tokio::sync::Notify::new());
        let core = hermetic_journal_engine(root.path(), release.clone());
        let epoch = core.device_write.epoch_for_tests();
        let (_, mut live) = core.sessions.subscribe("journal-chat", 0).unwrap();
        let first_id = core
            .sessions
            .dispatch(
                "journal-chat",
                HarnessId::Mock,
                synthetic_journal_request(root.path(), "first"),
                None,
            )
            .await
            .unwrap();
        journal_event(
            &mut live,
            |event| matches!(event, luna_proto::AgentEvent::TextDelta { text } if text == "first"),
        )
        .await;
        assert!(matches!(
            epoch.freeze(Duration::from_millis(40)).await,
            Err(luna_sync::write_epoch::EpochError::TimedOut)
        ));
        assert_eq!(epoch.phase(), luna_sync::write_epoch::Phase::Open);
        release.notify_one();
        journal_event(&mut live, |event| {
            matches!(
                event,
                luna_proto::AgentEvent::Done {
                    status: luna_proto::DoneStatus::Completed,
                    ..
                }
            )
        })
        .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if core
                    .sessions
                    .watch_sessions()
                    .borrow()
                    .iter()
                    .any(|session| {
                        session.chat_id == "journal-chat" && session.status == SessionStatus::Idle
                    })
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            core.sessions.has_live_run("journal-chat"),
            "persistent session is parked"
        );
        assert!(matches!(
            epoch.freeze(Duration::from_millis(40)).await,
            Err(luna_sync::write_epoch::EpochError::TimedOut)
        ));

        let warm_id = core
            .sessions
            .dispatch(
                "journal-chat",
                HarnessId::Mock,
                synthetic_journal_request(root.path(), "warm steer"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(warm_id, first_id);
        journal_event(&mut live, |event| {
            matches!(event, luna_proto::AgentEvent::Steered { .. })
        })
        .await;
        journal_event(&mut live, |event| {
            matches!(
                event,
                luna_proto::AgentEvent::Done {
                    status: luna_proto::DoneStatus::Completed,
                    ..
                }
            )
        })
        .await;
        assert!(core.sessions.interrupt("journal-chat").await.unwrap());
        let snapshot = epoch
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        let replay = core.sessions.subscribe("journal-chat", 0).unwrap().0;
        assert!(replay.iter().all(|event| event.seq > 0));
        drop(snapshot);
        core.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_run_append_stops_without_fanout_or_assistant_fold() {
        let root = tempfile::tempdir().unwrap();
        let release = Arc::new(tokio::sync::Notify::new());
        let core = hermetic_journal_engine(root.path(), release);
        let epoch = core.device_write.epoch_for_tests();
        let (_, mut live) = core.sessions.subscribe("journal-chat", 0).unwrap();
        core.sessions.inner.journal.fail_after_appends_for_tests(1);
        core.sessions
            .dispatch(
                "journal-chat",
                HarnessId::Mock,
                synthetic_journal_request(root.path(), "first"),
                None,
            )
            .await
            .unwrap();
        // SessionStarted is durable; the following TextDelta fails before it
        // can enter live fanout or the assistant transcript fold.
        journal_event(&mut live, |event| {
            matches!(event, luna_proto::AgentEvent::SessionStarted { .. })
        })
        .await;
        let status = tokio::time::timeout(Duration::from_secs(3), async {
            let mut watch = core.sessions.watch_sessions();
            loop {
                if let Some(session) = watch.borrow().iter().find(|session| {
                    session.chat_id == "journal-chat"
                        && session.status == SessionStatus::Errored
                        && session.recovery.is_some()
                }) {
                    return session.clone();
                }
                watch.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            status.recovery.as_ref().unwrap().status,
            RecoveryStatus::CheckingDelivery
        );
        assert!(
            status
                .recovery
                .as_ref()
                .unwrap()
                .detail
                .contains("local journal write outcome is uncertain")
        );
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let replay = core.sessions.subscribe("journal-chat", 0).unwrap().0;
        assert!(replay.iter().all(|event| event.seq > 0));
        let entries = core
            .doc_host
            .open("journal-chat")
            .unwrap()
            .doc()
            .read_entries()
            .unwrap();
        assert!(
            entries
                .iter()
                .all(|entry| entry.role != MessageRole::Assistant)
        );
        let snapshot = epoch
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        drop(snapshot);
        core.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_terminal_append_aborts_prior_stream_and_shows_uncertainty() {
        let root = tempfile::tempdir().unwrap();
        let release = Arc::new(tokio::sync::Notify::new());
        let core = hermetic_journal_engine(root.path(), release.clone());
        let epoch = core.device_write.epoch_for_tests();
        let (_, mut live) = core.sessions.subscribe("journal-chat", 0).unwrap();
        core.sessions.inner.journal.fail_after_appends_for_tests(2);
        core.sessions
            .dispatch(
                "journal-chat",
                HarnessId::Mock,
                synthetic_journal_request(root.path(), "first"),
                None,
            )
            .await
            .unwrap();
        journal_event(
            &mut live,
            |event| matches!(event, luna_proto::AgentEvent::TextDelta { text } if text == "first"),
        )
        .await;
        // The event is broadcast before the coalesced transcript commit.
        // Let that commit land so this exercises aborting an existing stream.
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let entries = core
                    .doc_host
                    .open("journal-chat")
                    .unwrap()
                    .doc()
                    .read_entries()
                    .unwrap();
                if entries.iter().any(|entry| {
                    entry.role == MessageRole::Assistant
                        && entry.status == Some(MessageStatus::Streaming)
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        release.notify_one();
        let status = tokio::time::timeout(Duration::from_secs(3), async {
            let mut watch = core.sessions.watch_sessions();
            loop {
                if let Some(session) = watch.borrow().iter().find(|session| {
                    session.chat_id == "journal-chat"
                        && session.status == SessionStatus::Errored
                        && session.recovery.is_some()
                }) {
                    return session.clone();
                }
                watch.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            status.recovery.unwrap().status,
            RecoveryStatus::CheckingDelivery
        );
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let replay = core.sessions.subscribe("journal-chat", 0).unwrap().0;
        assert_eq!(replay.len(), 2);
        assert!(replay.iter().all(|event| event.seq > 0));
        let entries = core
            .doc_host
            .open("journal-chat")
            .unwrap()
            .doc()
            .read_entries()
            .unwrap();
        assert!(
            entries
                .iter()
                .any(|entry| entry.role == MessageRole::Assistant
                    && entry.status == Some(MessageStatus::Aborted))
        );
        let snapshot = epoch
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        drop(snapshot);
        core.shutdown().await;
    }

    #[test]
    fn typed_approval_answers_fail_closed_for_unenforceable_task_allowance() {
        let question = luna_proto::UserInputQuestion {
            id: "approval-1".into(),
            header: "Permission".into(),
            question: "Allow?".into(),
            options: vec!["Deny".into(), "Allow once".into()],
            multi_select: false,
            mcp: None,
            approval: Some(luna_proto::ApprovalRequest {
                provider_request_id: "provider-1".into(),
                provider: HarnessId::ClaudeCode,
                turn_id: Some("turn-1".into()),
                machine_name: "test-machine".into(),
                action_kind: luna_proto::ApprovalActionKind::Command,
                summary: "echo ok".into(),
                requested_scope: luna_proto::PermissionScope::Workspace,
                expires_at_ms: i64::MAX,
                supports_task_allowance: false,
                details: None,
            }),
            input: None,
        };
        let answer = approval_answer(&question, luna_proto::ApprovalDecision::AllowTask).unwrap();
        assert_eq!(answer.labels, vec!["Deny"]);
        assert!(
            approval_answer(
                &luna_proto::UserInputQuestion {
                    mcp: None,
                    approval: None,
                    ..question
                },
                luna_proto::ApprovalDecision::AllowOnce
            )
            .is_none()
        );
    }

    #[test]
    fn provider_context_compaction_is_bound_to_generation_and_operation() {
        let operation = "codex-auto:thread-1:turn-1:item-1";
        let current = ContextControlState::default();
        assert!(
            reduce_provider_context_compaction(
                &current,
                "thread-1",
                4,
                operation,
                ContextOperationStatus::Running,
                "stale-thread",
            )
            .is_none()
        );
        let (running, clear_usage) = reduce_provider_context_compaction(
            &current,
            "thread-1",
            4,
            operation,
            ContextOperationStatus::Running,
            "thread-1",
        )
        .unwrap();
        assert!(!clear_usage);
        assert_eq!(running.compact.status, ContextOperationStatus::Running);
        assert_eq!(running.compact.binding_generation, Some(4));
        assert!(
            reduce_provider_context_compaction(
                &running,
                "thread-1",
                4,
                "stale-operation",
                ContextOperationStatus::Completed,
                "thread-1",
            )
            .is_none()
        );
        assert!(
            reduce_provider_context_compaction(
                &running,
                "thread-1",
                5,
                operation,
                ContextOperationStatus::Completed,
                "thread-1",
            )
            .is_none(),
            "a completion from an earlier binding generation must not settle the new binding"
        );
        let (completed, clear_usage) = reduce_provider_context_compaction(
            &running,
            "thread-1",
            4,
            operation,
            ContextOperationStatus::Completed,
            "thread-1",
        )
        .unwrap();
        assert!(clear_usage);
        assert_eq!(completed.compact.status, ContextOperationStatus::Completed);
        assert!(completed.provider_summary.is_none());
        assert!(reduce_provider_context_terminal(&completed, "thread-1", 4).is_none());

        let terminal = reduce_provider_context_terminal(&running, "thread-1", 4).unwrap();
        assert_eq!(terminal.compact.status, ContextOperationStatus::Unknown);
        assert!(terminal.compact.detail.unwrap().contains("will not replay"));
    }

    #[tokio::test]
    async fn expired_approval_resolves_once_with_a_fail_closed_answer() {
        let pending = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let (resolver, answer) = tokio::sync::oneshot::channel();
        pending.lock().unwrap().insert(
            "approval-1".into(),
            PendingInput {
                kind: PendingInputKind::Provider,
                questions: Vec::new(),
                resolver,
            },
        );
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        assert!(expire_pending_approval(&pending, &event_tx, "approval-1"));
        assert!(answer.await.unwrap().is_empty());
        assert!(matches!(
            event_rx.recv().await,
            Some(luna_proto::AgentEvent::InputResolved { request_id })
                if request_id == "approval-1"
        ));
        assert!(!expire_pending_approval(&pending, &event_tx, "approval-1"));
    }

    #[test]
    fn ordinary_input_route_cannot_consume_a_typed_approval() {
        let pending = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let (resolver, _answer) = tokio::sync::oneshot::channel();
        pending.lock().unwrap().insert(
            "approval-1".into(),
            PendingInput {
                kind: PendingInputKind::Provider,
                questions: vec![luna_proto::UserInputQuestion {
                    id: "question-1".into(),
                    header: "Approval".into(),
                    question: "Allow?".into(),
                    options: vec!["Deny".into(), "Allow once".into()],
                    multi_select: false,
                    mcp: None,
                    approval: Some(luna_proto::ApprovalRequest {
                        provider_request_id: "provider-1".into(),
                        provider: HarnessId::Codex,
                        turn_id: None,
                        machine_name: "test-machine".into(),
                        action_kind: luna_proto::ApprovalActionKind::Command,
                        summary: "echo safe".into(),
                        requested_scope: luna_proto::PermissionScope::Workspace,
                        expires_at_ms: i64::MAX,
                        supports_task_allowance: true,
                        details: None,
                    }),
                    input: None,
                }],
                resolver,
            },
        );

        assert!(take_pending_ordinary_input(&pending, "approval-1").is_none());
        assert!(pending.lock().unwrap().contains_key("approval-1"));
    }

    #[tokio::test]
    async fn generic_answers_cannot_consume_server_owned_luna_approval() {
        let root = tempfile::tempdir().unwrap();
        let sessions = SessionsEngine::new(
            "device-test".into(),
            Arc::new(crate::run_journal::RunJournal::open(root.path()).unwrap()),
            Arc::new(crate::registry::HarnessRegistry::new()),
            Arc::new(DocsStore::open(root.path().join("store")).unwrap()),
        );
        let question = |id: &str, action_kind| luna_proto::UserInputQuestion {
            id: id.into(),
            header: "Approval".into(),
            question: "Allow?".into(),
            options: vec!["Deny".into(), "Allow once".into()],
            multi_select: false,
            mcp: None,
            approval: Some(luna_proto::ApprovalRequest {
                provider_request_id: "claimed-local-ui".into(),
                provider: HarnessId::Codex,
                turn_id: Some("turn-a".into()),
                machine_name: "synthetic".into(),
                action_kind,
                summary: "synthetic".into(),
                requested_scope: luna_proto::PermissionScope::Workspace,
                expires_at_ms: i64::MAX,
                supports_task_allowance: false,
                details: None,
            }),
            input: None,
        };
        let pending = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let (luna_tx, mut luna_rx) = tokio::sync::oneshot::channel();
        let (luna_grant_tx, _luna_grant_rx) = tokio::sync::oneshot::channel();
        pending.lock().unwrap().insert(
            "claimed-ui-approval".into(),
            PendingInput {
                kind: PendingInputKind::LunaCrossTask {
                    run_id: "run-a".into(),
                    proposal: Box::new(build_fake_archive_proposal(
                        "claimed-ui-approval",
                        "chat-a",
                        "device-test",
                        synthetic_provider_binding(),
                        "claimed-local-ui".into(),
                    )),
                    grant_tx: luna_grant_tx,
                },
                // Even a forged provider-looking serialized hint cannot
                // override the server-owned pending kind.
                questions: vec![question(
                    "luna-question",
                    luna_proto::ApprovalActionKind::Command,
                )],
                resolver: luna_tx,
            },
        );
        let (provider_tx, provider_rx) = tokio::sync::oneshot::channel();
        pending.lock().unwrap().insert(
            "provider-approval".into(),
            PendingInput {
                kind: PendingInputKind::Provider,
                questions: vec![question(
                    "provider-question",
                    luna_proto::ApprovalActionKind::Tool,
                )],
                resolver: provider_tx,
            },
        );
        let (forged_source_tx, mut forged_source_rx) = tokio::sync::oneshot::channel();
        pending.lock().unwrap().insert(
            "forged-source".into(),
            PendingInput {
                kind: PendingInputKind::Provider,
                questions: vec![question(
                    "forged-source-question",
                    luna_proto::ApprovalActionKind::LunaCrossTask,
                )],
                resolver: forged_source_tx,
            },
        );
        let (ordinary_tx, ordinary_rx) = tokio::sync::oneshot::channel();
        pending.lock().unwrap().insert(
            "ordinary-question".into(),
            PendingInput {
                kind: PendingInputKind::Provider,
                questions: vec![luna_proto::UserInputQuestion {
                    id: "ordinary".into(),
                    header: "Question".into(),
                    question: "Which?".into(),
                    options: vec!["A".into()],
                    multi_select: false,
                    mcp: None,
                    approval: None,
                    input: None,
                }],
                resolver: ordinary_tx,
            },
        );
        let (steer_tx, _steer_rx) = tokio::sync::mpsc::channel(1);
        let (cancel, _cancel_rx) = tokio::sync::watch::channel(false);
        let (engine_tx, _engine_rx) = tokio::sync::mpsc::unbounded_channel();
        lock(&sessions.inner.runs).insert(
            "chat-a".into(),
            RunHandle {
                workspace_repair_revision: 0,
                idle_stream_closed: Arc::new(AtomicBool::new(false)),
                run_id: "run-a".into(),
                provider_turn: None,
                run_provider_binding: None,
                provider_turn_readiness: tokio::sync::watch::channel(
                    ProviderTurnReadiness::AwaitingBinding,
                )
                .0,
                steerable: false,
                runtime_config: RuntimeConfig::from_request(HarnessId::Codex, &request()),
                steer_tx,
                interrupt_token: luna_harness::CancellationToken::new(),
                cancel,
                hold_recovery_on_interrupt: Arc::new(AtomicBool::new(true)),
                engine_tx,
                pending_inputs: pending.clone(),
                routed_steers: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            },
        );

        assert!(sessions.is_luna_pending_approval("chat-a", "claimed-ui-approval"));
        assert!(
            !sessions
                .respond_approval(
                    "chat-a",
                    "claimed-ui-approval",
                    luna_proto::ApprovalDecision::AllowOnce,
                )
                .unwrap()
        );
        assert!(
            !sessions
                .respond_input(
                    "chat-a",
                    "claimed-ui-approval",
                    vec![luna_proto::UserInputAnswer {
                        question_id: "luna-question".into(),
                        labels: vec!["Allow once".into()],
                    }],
                )
                .unwrap()
        );
        assert!(pending.lock().unwrap().contains_key("claimed-ui-approval"));
        assert!(matches!(
            luna_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(
            !sessions
                .respond_approval(
                    "chat-a",
                    "forged-source",
                    luna_proto::ApprovalDecision::AllowOnce,
                )
                .unwrap()
        );
        assert!(pending.lock().unwrap().contains_key("forged-source"));
        assert!(matches!(
            forged_source_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        assert!(
            sessions
                .respond_approval(
                    "chat-a",
                    "provider-approval",
                    luna_proto::ApprovalDecision::AllowOnce,
                )
                .unwrap()
        );
        assert_eq!(provider_rx.await.unwrap()[0].labels, vec!["Allow once"]);
        assert!(
            sessions
                .respond_input(
                    "chat-a",
                    "ordinary-question",
                    vec![luna_proto::UserInputAnswer {
                        question_id: "ordinary".into(),
                        labels: vec!["A".into()],
                    }],
                )
                .unwrap()
        );
        assert_eq!(ordinary_rx.await.unwrap()[0].labels, vec!["A"]);
    }

    fn local_luna_test_run() -> (
        tempfile::TempDir,
        SessionsEngine,
        PendingInputs,
        tokio::sync::mpsc::UnboundedReceiver<luna_proto::AgentEvent>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let sessions = SessionsEngine::new(
            "device-test".into(),
            Arc::new(crate::run_journal::RunJournal::open(root.path()).unwrap()),
            Arc::new(crate::registry::HarnessRegistry::new()),
            Arc::new(DocsStore::open(root.path().join("store")).unwrap()),
        );
        let pending = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let (steer_tx, _steer_rx) = tokio::sync::mpsc::channel(1);
        let (cancel, _cancel_rx) = tokio::sync::watch::channel(false);
        let (engine_tx, engine_rx) = tokio::sync::mpsc::unbounded_channel();
        lock(&sessions.inner.runs).insert(
            "chat-a".into(),
            RunHandle {
                workspace_repair_revision: 0,
                idle_stream_closed: Arc::new(AtomicBool::new(false)),
                run_id: "run-a".into(),
                provider_turn: None,
                run_provider_binding: None,
                provider_turn_readiness: tokio::sync::watch::channel(
                    ProviderTurnReadiness::AwaitingBinding,
                )
                .0,
                steerable: false,
                runtime_config: RuntimeConfig::from_request(HarnessId::Codex, &request()),
                steer_tx,
                interrupt_token: luna_harness::CancellationToken::new(),
                cancel,
                hold_recovery_on_interrupt: Arc::new(AtomicBool::new(true)),
                engine_tx,
                pending_inputs: pending.clone(),
                routed_steers: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            },
        );
        (root, sessions, pending, engine_rx)
    }

    fn synthetic_provider_binding() -> ProviderBindingKey {
        ProviderBindingKey {
            task_id: "chat-a".into(),
            binding_id: "binding-a".into(),
            generation: 1,
            provider_thread_id: "provider-thread-a".into(),
        }
    }

    fn managed_codex_wait_fixture() -> (tempfile::TempDir, crate::EngineCore, String) {
        let root = tempfile::tempdir().unwrap();
        let accounts = crate::agent_accounts::AgentAccountsConfig {
            data_dir: root.path().join("synthetic-provider-state"),
            claude_config_dir: root.path().join("synthetic-claude"),
            claude_config_file: root.path().join("synthetic-claude.json"),
            codex_home: root.path().join("synthetic-codex"),
        };
        accounts.disable_claude_keychain_fallback().unwrap();
        let core = crate::EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            crate::EngineProfile::development(
                &root.path().join("synthetic-engine"),
                "synthetic-org",
                "synthetic-user",
            ),
            Arc::new(crate::registry::HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            accounts,
        )
        .unwrap();
        let project_path = root.path().join("project-a");
        std::fs::create_dir_all(&project_path).unwrap();
        core.workspace
            .create_space(
                "project-a",
                &core.device_id,
                &project_path.display().to_string(),
                None,
                false,
            )
            .unwrap();
        let task_id = uuid::Uuid::new_v4().to_string();
        core.workspace
            .create_chat(&task_id, Some("project-a"), None, None, None)
            .unwrap();
        core.task_identity
            .register_new_task(
                &task_id,
                Some("project-a"),
                None,
                project_path.to_str(),
                luna_proto::TaskLineage::default(),
            )
            .unwrap();
        let (steer_tx, _steer_rx) = tokio::sync::mpsc::channel(1);
        let (cancel, _cancel_rx) = tokio::sync::watch::channel(false);
        let (engine_tx, _engine_rx) = tokio::sync::mpsc::unbounded_channel();
        lock(&core.sessions.inner.runs).insert(
            task_id.clone(),
            RunHandle {
                workspace_repair_revision: 0,
                idle_stream_closed: Arc::new(AtomicBool::new(false)),
                run_id: "run-a".into(),
                provider_turn: None,
                run_provider_binding: None,
                provider_turn_readiness: tokio::sync::watch::channel(
                    ProviderTurnReadiness::AwaitingBinding,
                )
                .0,
                steerable: false,
                runtime_config: RuntimeConfig::from_request(HarnessId::Codex, &request()),
                steer_tx,
                interrupt_token: luna_harness::CancellationToken::new(),
                cancel,
                hold_recovery_on_interrupt: Arc::new(AtomicBool::new(true)),
                engine_tx,
                pending_inputs: Arc::new(Mutex::new(std::collections::HashMap::new())),
                routed_steers: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            },
        );
        (root, core, task_id)
    }

    fn publish_managed_codex_binding(
        root: &tempfile::TempDir,
        core: &crate::EngineCore,
        task_id: &str,
    ) {
        let cwd = root.path().join("project-a");
        let binding = core
            .task_identity
            .record_session_binding(
                task_id,
                HarnessId::Codex,
                "provider-thread-a",
                cwd.to_str().unwrap(),
            )
            .unwrap()
            .expect("durable managed binding");
        assert_eq!(binding.generation, 1);
        assert!(
            core.sessions
                .inner
                .observe_codex_binding(task_id, "run-a", "provider-thread-a")
        );
    }

    #[derive(Clone, Copy)]
    enum FakeInterruptMode {
        Receipt(luna_proto::CommandReceiptStatus),
        CommitThenError,
        NoReceipt,
        ReadbackError,
        ReadbackNever,
    }

    struct FakeControlEvidenceBackend {
        source_task_id: String,
        source_environment_id: String,
        target: Mutex<luna_proto::CrossTaskSummary>,
        snapshots: Mutex<VecDeque<luna_proto::TaskCommandSnapshot>>,
        snapshot_reads: AtomicUsize,
        gate_on_read: usize,
        entered_snapshot: Option<Arc<tokio::sync::Notify>>,
        release_snapshot: Option<Arc<tokio::sync::Notify>>,
        control_calls: AtomicUsize,
        receipt_reads: AtomicUsize,
        interrupt_mode: Mutex<Option<FakeInterruptMode>>,
        stored_receipt: Mutex<Option<luna_proto::CommandReceipt>>,
        submitted_request: Mutex<Option<luna_proto::CrossTaskControlRequest>>,
        readback_command_id: Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl crate::cross_task_tools::CrossTaskBackend for FakeControlEvidenceBackend {
        fn capabilities(&self) -> crate::cross_task_tools::CrossTaskCapabilities {
            crate::cross_task_tools::CrossTaskCapabilities {
                read: true,
                ..Default::default()
            }
        }

        async fn task(
            &self,
            task_id: &str,
        ) -> Result<Option<luna_proto::CrossTaskSummary>, crate::cross_task_tools::CrossTaskError>
        {
            let target = lock(&self.target);
            Ok(if task_id == self.source_task_id {
                Some(luna_proto::CrossTaskSummary {
                    task_id: self.source_task_id.clone(),
                    project_id: Some("project-a".into()),
                    environment_id: self.source_environment_id.clone(),
                    ..target.clone()
                })
            } else if task_id == target.task_id {
                Some(target.clone())
            } else {
                None
            })
        }

        async fn list(
            &self,
            _: luna_proto::CrossTaskListRequest,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<luna_proto::CrossTaskListPage, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn read(
            &self,
            _: luna_proto::CrossTaskReadRequest,
        ) -> Result<luna_proto::CrossTaskReadPage, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn enqueue_queue(
            &self,
            _: luna_proto::CrossTaskSendRequest,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<luna_proto::CrossTaskSendReceipt, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn wait_event(
            &self,
            _: luna_proto::CrossTaskWaitRequest,
        ) -> Result<Option<luna_proto::CrossTaskWaitEvent>, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn control_target_snapshot(
            &self,
            _: &str,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<luna_proto::TaskCommandSnapshot, crate::cross_task_tools::CrossTaskError>
        {
            let read = self.snapshot_reads.fetch_add(1, Ordering::SeqCst) + 1;
            if read == self.gate_on_read
                && let (Some(entered), Some(release)) =
                    (&self.entered_snapshot, &self.release_snapshot)
            {
                entered.notify_one();
                release.notified().await;
            }
            let mut snapshots = lock(&self.snapshots);
            let snapshot = snapshots.front().cloned().unwrap();
            if snapshots.len() > 1 {
                snapshots.pop_front();
            }
            Ok(snapshot)
        }

        async fn control_command_receipt(
            &self,
            _: &str,
            command_id: &str,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<Option<luna_proto::CommandReceipt>, crate::cross_task_tools::CrossTaskError>
        {
            self.receipt_reads.fetch_add(1, Ordering::SeqCst);
            *lock(&self.readback_command_id) = Some(command_id.into());
            let mode = *lock(&self.interrupt_mode);
            if matches!(mode, Some(FakeInterruptMode::ReadbackNever)) {
                std::future::pending().await
            }
            if matches!(mode, Some(FakeInterruptMode::ReadbackError)) {
                return Err(crate::cross_task_tools::CrossTaskError::Backend(
                    "synthetic readback failure".into(),
                ));
            }
            Ok(lock(&self.stored_receipt)
                .as_ref()
                .filter(|receipt| receipt.command_id == command_id)
                .cloned())
        }

        async fn control(
            &self,
            request: luna_proto::CrossTaskControlRequest,
            authority: &crate::cross_task_tools::CrossTaskAuthority,
            grant: &crate::cross_task_tools::CrossTaskControlGrant,
        ) -> Result<luna_proto::CrossTaskControlReceipt, crate::cross_task_tools::CrossTaskError>
        {
            self.control_calls.fetch_add(1, Ordering::SeqCst);
            let Some(mode) = *lock(&self.interrupt_mode) else {
                return Err(crate::cross_task_tools::CrossTaskError::Unsupported);
            };
            if request.action != luna_proto::CrossTaskControlAction::Interrupt
                || grant.request != request
                || grant.caller_task_id != authority.caller_task_id
                || !authority.explicit_user_request
                || !authority.granted_scopes.contains("threads.manage")
            {
                return Err(crate::cross_task_tools::CrossTaskError::UserRequest);
            }
            *lock(&self.submitted_request) = Some(request.clone());
            let status = match mode {
                FakeInterruptMode::Receipt(status) => status,
                FakeInterruptMode::CommitThenError => luna_proto::CommandReceiptStatus::Applied,
                FakeInterruptMode::NoReceipt
                | FakeInterruptMode::ReadbackError
                | FakeInterruptMode::ReadbackNever => luna_proto::CommandReceiptStatus::Pending,
            };
            let receipt = luna_proto::CommandReceipt {
                command_id: request.command_id.clone(),
                task_id: request.task_id.clone(),
                intent: "stop".into(),
                payload_hash: "synthetic-interrupt".into(),
                status,
                snapshot: lock(&self.snapshots).front().unwrap().clone(),
                event_sequence: (status == luna_proto::CommandReceiptStatus::Applied).then_some(1),
                actor_id: format!("luna-tool:{}", authority.caller_task_id),
                client_id: "luna-cross-task".into(),
                client_session_id: authority.caller_task_id.clone(),
                execution_environment: Some(authority.local_environment_id.clone()),
                correlation_id: None,
                reason: None,
                recovery_action: None,
                accepted_at_ms: 1,
                committed_at_ms: (status == luna_proto::CommandReceiptStatus::Applied).then_some(2),
                observed_at_ms: 2,
            };
            if !matches!(mode, FakeInterruptMode::NoReceipt) {
                *lock(&self.stored_receipt) = Some(receipt.clone());
            }
            if matches!(mode, FakeInterruptMode::CommitThenError) {
                return Err(crate::cross_task_tools::CrossTaskError::Backend(
                    "synthetic committed but response lost".into(),
                ));
            }
            Ok(luna_proto::CrossTaskControlReceipt {
                task_id: request.task_id,
                command: receipt,
                observed_status: "working".into(),
                settled: false,
            })
        }
    }

    fn control_evidence_authority(
        core: &crate::EngineCore,
        source_task_id: &str,
    ) -> crate::cross_task_tools::CrossTaskAuthority {
        crate::cross_task_tools::CrossTaskAuthority {
            caller_task_id: source_task_id.into(),
            local_environment_id: core.device_id.clone(),
            visible_project_ids: HashSet::from(["project-a".into()]),
            allow_projectless: false,
            granted_scopes: HashSet::from(["threads.read".into()]),
            explicit_user_request: false,
            delivery: luna_proto::RuntimeDelivery::CodexDynamicTools,
        }
    }

    fn control_evidence_backend(
        core: &crate::EngineCore,
        source_task_id: &str,
    ) -> Arc<FakeControlEvidenceBackend> {
        Arc::new(FakeControlEvidenceBackend {
            source_task_id: source_task_id.into(),
            source_environment_id: core.device_id.clone(),
            target: Mutex::new(luna_proto::CrossTaskSummary {
                task_id: "target-a".into(),
                project_id: Some("project-a".into()),
                environment_id: core.device_id.clone(),
                title: "Synthetic target".into(),
                status: "idle".into(),
                archived: false,
                updated_at_ms: 1,
            }),
            snapshots: Mutex::new(VecDeque::from([luna_proto::TaskCommandSnapshot {
                task_id: "target-a".into(),
                task_revision: 7,
                task_sequence: 12,
                binding_id: Some("binding-target".into()),
                binding_generation: Some(2),
                active_turn_id: Some("target-run".into()),
            }])),
            snapshot_reads: AtomicUsize::new(0),
            gate_on_read: 1,
            entered_snapshot: None,
            release_snapshot: None,
            control_calls: AtomicUsize::new(0),
            receipt_reads: AtomicUsize::new(0),
            interrupt_mode: Mutex::new(None),
            stored_receipt: Mutex::new(None),
            submitted_request: Mutex::new(None),
            readback_command_id: Mutex::new(None),
        })
    }

    fn register_control_evidence_backend(
        core: &crate::EngineCore,
        backend: &Arc<FakeControlEvidenceBackend>,
    ) {
        let erased: Arc<dyn crate::cross_task_tools::CrossTaskBackend> = backend.clone();
        core.sessions
            .set_cross_task_backend(Arc::downgrade(&erased));
    }

    fn ready_control_evidence_claim(
        root: &tempfile::TempDir,
        core: &crate::EngineCore,
        source_task_id: &str,
    ) -> super::ClaimedProviderCall {
        publish_managed_codex_binding(root, core, source_task_id);
        assert!(core.sessions.inner.observe_codex_checkpoint(
            source_task_id,
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
        ));
        core.sessions
            .claim_luna_provider_call_evidence(
                source_task_id,
                "run-a",
                "provider-thread-a",
                "turn-a",
                "call-a",
            )
            .expect("exact synthetic call claim")
    }

    #[tokio::test]
    async fn approved_send_commit_fence_rejects_wrong_call_target_revision_and_replaced_turn() {
        let (root, core, source) = managed_codex_wait_fixture();
        let claim = ready_control_evidence_claim(&root, &core, &source);
        let target = uuid::Uuid::new_v4().to_string();
        core.workspace
            .create_chat(&target, Some("project-a"), None, None, None)
            .unwrap();
        core.task_identity
            .register_new_task(
                &target,
                Some("project-a"),
                None,
                root.path().join("project-a").to_str(),
                luna_proto::TaskLineage::default(),
            )
            .unwrap();
        let fence = crate::cross_task_tools::ApprovedSendFence {
            source_task_id: source.clone(),
            source_run_id: claim.source_run_id,
            source_binding_id: claim.source_binding.binding_id,
            source_binding_generation: claim.source_binding.generation,
            source_provider_thread_id: claim.source_binding.provider_thread_id,
            source_provider_turn_id: claim.provider_turn_id,
            source_provider_call_id: claim.provider_call_id,
            target_snapshot: core
                .doc_host
                .task_command_snapshot(&target)
                .unwrap()
                .snapshot,
            request: luna_proto::CrossTaskSendRequest {
                task_id: target,
                message: "synthetic".into(),
                mode: luna_proto::CrossTaskSendMode::Queue,
                idempotency_key: "source-fence".into(),
            },
            expires_at_ms: crate::now_ms() + 30_000,
        };
        core.sessions
            .with_live_approved_send(&fence, || Ok(()))
            .unwrap();
        let mut wrong_call = fence.clone();
        wrong_call.source_provider_call_id = "unclaimed-call".into();
        assert!(
            core.sessions
                .with_live_approved_send(&wrong_call, || -> Result<(), crate::EngineError> {
                    panic!("wrong call reached commit")
                })
                .is_err()
        );
        let mut stale_target = fence.clone();
        stale_target.target_snapshot.task_revision += 1;
        assert!(
            core.sessions
                .with_live_approved_send(&stale_target, || -> Result<(), crate::EngineError> {
                    panic!("stale target reached commit")
                })
                .is_err()
        );
        assert!(core.sessions.inner.observe_codex_checkpoint(
            &source,
            "run-a",
            Some("provider-thread-a"),
            "replacement-turn",
        ));
        assert!(
            core.sessions
                .with_live_approved_send(&fence, || -> Result<(), crate::EngineError> {
                    panic!("replaced turn reached commit")
                })
                .is_err()
        );
        core.shutdown().await;
    }

    #[tokio::test]
    async fn approved_send_postcommit_install_failure_keeps_applied_receipt_through_rpc() {
        use crate::cross_task_tools::CrossTaskBackend;
        let (root, core, source) = managed_codex_wait_fixture();
        let claim = ready_control_evidence_claim(&root, &core, &source);
        let target = uuid::Uuid::new_v4().to_string();
        core.workspace
            .create_chat(&target, Some("project-a"), None, None, None)
            .unwrap();
        core.task_identity
            .register_new_task(
                &target,
                Some("project-a"),
                None,
                root.path().join("project-a").to_str(),
                luna_proto::TaskLineage::default(),
            )
            .unwrap();
        let request = luna_proto::CrossTaskSendRequest {
            task_id: target.clone(),
            message: "synthetic postcommit follow-up".into(),
            mode: luna_proto::CrossTaskSendMode::Queue,
            idempotency_key: "approved-postcommit".into(),
        };
        let fence = crate::cross_task_tools::ApprovedSendFence {
            source_task_id: source.clone(),
            source_run_id: claim.source_run_id,
            source_binding_id: claim.source_binding.binding_id,
            source_binding_generation: claim.source_binding.generation,
            source_provider_thread_id: claim.source_binding.provider_thread_id,
            source_provider_turn_id: claim.provider_turn_id,
            source_provider_call_id: claim.provider_call_id,
            target_snapshot: core
                .doc_host
                .task_command_snapshot(&target)
                .unwrap()
                .snapshot,
            request: request.clone(),
            expires_at_ms: crate::now_ms() + 30_000,
        };
        core.doc_host.fail_next_approved_queue_install_for_test();
        let rpc = core.rpc_service();
        let result = CrossTaskBackend::enqueue_approved_queue(
            rpc.as_ref(),
            request,
            &control_evidence_authority(&core, &source),
            &fence,
        )
        .await
        .unwrap();
        assert_eq!(result.delivery_state, "queued");
        assert_eq!(
            result.command.status,
            luna_proto::CommandReceiptStatus::Applied
        );
        assert_eq!(
            core.doc_host
                .task_command_receipt("approved-postcommit")
                .unwrap()
                .unwrap()
                .status,
            luna_proto::CommandReceiptStatus::Applied
        );
        let stored = core.store.load_snapshot(&target).unwrap().unwrap();
        let doc = luna_doc::SessionDoc::from_snapshot(&stored).unwrap();
        assert_eq!(doc.read_queue().unwrap().len(), 1);
        assert!(core.doc_host.open(&target).is_err());
        assert!(core.doc_host.flush_all_checked().is_err());
        core.shutdown().await;
    }

    async fn prospective_interrupt_fixture() -> (
        tempfile::TempDir,
        crate::EngineCore,
        String,
        Arc<FakeControlEvidenceBackend>,
        super::PreparedControlEvidence,
        tokio::sync::mpsc::UnboundedReceiver<luna_proto::AgentEvent>,
    ) {
        let (root, core, source_task_id) = managed_codex_wait_fixture();
        let claim = ready_control_evidence_claim(&root, &core, &source_task_id);
        let backend = control_evidence_backend(&core, &source_task_id);
        register_control_evidence_backend(&core, &backend);
        let evidence = core
            .sessions
            .prepare_luna_control_evidence(
                claim,
                &control_evidence_authority(&core, &source_task_id),
                "target-a",
            )
            .await
            .unwrap();
        let (engine_tx, events) = tokio::sync::mpsc::unbounded_channel();
        lock(&core.sessions.inner.runs)
            .get_mut(&source_task_id)
            .unwrap()
            .engine_tx = engine_tx;
        (root, core, source_task_id, backend, evidence, events)
    }

    async fn approved_interrupt_fixture() -> (
        tempfile::TempDir,
        crate::EngineCore,
        String,
        Arc<FakeControlEvidenceBackend>,
        super::LunaProspectiveInterruptGrant,
        tokio::sync::mpsc::UnboundedReceiver<luna_proto::AgentEvent>,
    ) {
        let (root, core, source_task_id, backend, evidence, mut events) =
            prospective_interrupt_fixture().await;
        let (request_id, answer, grant) = core
            .sessions
            .register_luna_prospective_interrupt(evidence)
            .await
            .unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(luna_proto::AgentEvent::InputRequested { .. })
        ));
        assert_eq!(
            core.sessions
                .respond_luna_prospective_interrupt_local_ui(
                    &source_task_id,
                    &request_id,
                    luna_proto::ApprovalDecision::AllowOnce,
                )
                .await,
            LunaApprovalOutcome::AllowedOnce
        );
        assert_eq!(answer.await.unwrap()[0].labels, vec!["Allow once"]);
        assert!(matches!(
            events.try_recv(),
            Ok(luna_proto::AgentEvent::InputResolved { request_id: resolved })
                if resolved == request_id
        ));
        let grant = grant.await.unwrap();
        assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
        (root, core, source_task_id, backend, grant, events)
    }

    async fn approved_gated_interrupt_fixture() -> (
        tempfile::TempDir,
        crate::EngineCore,
        String,
        Arc<FakeControlEvidenceBackend>,
        super::LunaProspectiveInterruptGrant,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
    ) {
        let (root, core, source_task_id) = managed_codex_wait_fixture();
        let claim = ready_control_evidence_claim(&root, &core, &source_task_id);
        let mut backend = control_evidence_backend(&core, &source_task_id);
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let fake = Arc::get_mut(&mut backend).unwrap();
        fake.gate_on_read = 5;
        fake.entered_snapshot = Some(entered.clone());
        fake.release_snapshot = Some(release.clone());
        register_control_evidence_backend(&core, &backend);
        let evidence = core
            .sessions
            .prepare_luna_control_evidence(
                claim,
                &control_evidence_authority(&core, &source_task_id),
                "target-a",
            )
            .await
            .unwrap();
        let (engine_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        lock(&core.sessions.inner.runs)
            .get_mut(&source_task_id)
            .unwrap()
            .engine_tx = engine_tx;
        let (request_id, answer, grant) = core
            .sessions
            .register_luna_prospective_interrupt(evidence)
            .await
            .unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(luna_proto::AgentEvent::InputRequested { .. })
        ));
        assert_eq!(
            core.sessions
                .respond_luna_prospective_interrupt_local_ui(
                    &source_task_id,
                    &request_id,
                    luna_proto::ApprovalDecision::AllowOnce,
                )
                .await,
            LunaApprovalOutcome::AllowedOnce
        );
        assert_eq!(answer.await.unwrap()[0].labels, vec!["Allow once"]);
        assert!(matches!(
            events.try_recv(),
            Ok(luna_proto::AgentEvent::InputResolved { .. })
        ));
        (
            root,
            core,
            source_task_id,
            backend,
            grant.await.unwrap(),
            entered,
            release,
        )
    }

    #[tokio::test]
    async fn private_interrupt_dispatch_classifies_exact_durable_receipts() {
        use super::LunaInterruptDispatchStatus as Outcome;
        use luna_proto::CommandReceiptStatus as Receipt;

        for (receipt_status, expected) in [
            (Receipt::Applied, Outcome::Applied),
            (Receipt::Rejected, Outcome::Rejected),
            (Receipt::Conflict, Outcome::Conflict),
            (Receipt::Pending, Outcome::Pending),
            (Receipt::Superseded, Outcome::Rejected),
            (Receipt::Failed, Outcome::Rejected),
        ] {
            let (_root, core, source_task_id, backend, grant, _events) =
                approved_interrupt_fixture().await;
            *lock(&backend.interrupt_mode) = Some(FakeInterruptMode::Receipt(receipt_status));
            let expected_snapshot = grant.proposal.evidence.target_snapshot.clone();
            let result = core
                .sessions
                .dispatch_luna_interrupt_grant_once(grant)
                .await
                .unwrap();
            assert_eq!(result.status, expected);
            assert_eq!(result.receipt.as_ref().unwrap().status, receipt_status);
            assert_eq!(result.receipt.as_ref().unwrap().snapshot, expected_snapshot);
            assert!(!result.settled, "Applied is not target settlement");
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 1);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 1);
            let request = lock(&backend.submitted_request).clone().unwrap();
            assert_eq!(request.command_id, result.command_id);
            assert_eq!(
                request.action,
                luna_proto::CrossTaskControlAction::Interrupt
            );
            assert_eq!(request.task_id, "target-a");
            assert_eq!(request.project_id, "project-a");
            assert_eq!(request.environment_id, core.device_id);
            assert_eq!(
                request.expected_task_revision,
                Some(expected_snapshot.task_revision)
            );
            assert_eq!(
                request.expected_binding_generation,
                expected_snapshot.binding_generation
            );
            assert_eq!(request.expected_turn_id, expected_snapshot.active_turn_id);
            assert_eq!(
                lock(&backend.readback_command_id).as_deref(),
                Some(result.command_id.as_str())
            );
            assert_eq!(
                source_task_id,
                result.receipt.as_ref().unwrap().client_session_id
            );
        }
    }

    #[tokio::test]
    async fn private_interrupt_dispatch_uses_same_id_readback_without_replay() {
        use super::LunaInterruptDispatchStatus as Outcome;

        for (mode, expected) in [
            (FakeInterruptMode::CommitThenError, Outcome::Applied),
            (FakeInterruptMode::NoReceipt, Outcome::Unknown),
            (FakeInterruptMode::ReadbackError, Outcome::Unknown),
        ] {
            let (_root, core, _source_task_id, backend, grant, _events) =
                approved_interrupt_fixture().await;
            *lock(&backend.interrupt_mode) = Some(mode);
            let result = core
                .sessions
                .dispatch_luna_interrupt_grant_once(grant)
                .await
                .unwrap();
            assert_eq!(result.status, expected);
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 1);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 1);
            assert_eq!(
                lock(&backend.submitted_request)
                    .as_ref()
                    .unwrap()
                    .command_id,
                result.command_id
            );
            assert_eq!(
                lock(&backend.readback_command_id).as_deref(),
                Some(result.command_id.as_str())
            );
            if expected == Outcome::Unknown {
                assert!(result.receipt.is_none());
                assert!(!result.settled);
            } else {
                assert_eq!(
                    result.receipt.as_ref().unwrap().status,
                    luna_proto::CommandReceiptStatus::Applied
                );
            }
        }
    }

    #[tokio::test]
    async fn private_interrupt_dispatch_bounds_stuck_same_id_readback() {
        use super::LunaInterruptDispatchStatus as Outcome;

        let (_root, core, _source_task_id, backend, grant, _events) =
            approved_interrupt_fixture().await;
        *lock(&backend.interrupt_mode) = Some(FakeInterruptMode::ReadbackNever);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            core.sessions.dispatch_luna_interrupt_grant_once(grant),
        )
        .await
        .expect("receipt readback must be bounded")
        .unwrap();
        assert_eq!(result.status, Outcome::Unknown);
        assert!(result.receipt.is_none());
        assert!(!result.settled);
        assert_eq!(backend.control_calls.load(Ordering::SeqCst), 1);
        assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 1);
        assert_eq!(
            lock(&backend.submitted_request)
                .as_ref()
                .unwrap()
                .command_id,
            result.command_id
        );
        assert_eq!(
            lock(&backend.readback_command_id).as_deref(),
            Some(result.command_id.as_str())
        );
    }

    #[tokio::test]
    async fn private_interrupt_dispatch_rejects_expired_or_stale_source_before_submit() {
        use crate::cross_task_tools::CrossTaskError;

        for change in ["expiry", "rebind", "turn", "cancel", "run-drop"] {
            let (_root, core, source_task_id, backend, mut grant, _events) =
                approved_interrupt_fixture().await;
            *lock(&backend.interrupt_mode) = Some(FakeInterruptMode::Receipt(
                luna_proto::CommandReceiptStatus::Applied,
            ));
            match change {
                "expiry" => grant.proposal.expires_at_ms = crate::now_ms() - 1,
                "rebind" => {
                    core.task_identity
                        .advance_binding(
                            &source_task_id,
                            HarnessId::Codex,
                            "provider-thread-b",
                            luna_proto::BindingOrigin::ContextReset,
                            "synthetic rebind before dispatch",
                        )
                        .unwrap();
                }
                "turn" => assert!(core.sessions.inner.observe_codex_checkpoint(
                    &source_task_id,
                    "run-a",
                    Some("provider-thread-a"),
                    "turn-b",
                )),
                "cancel" => {
                    lock(&core.sessions.inner.runs)
                        .get(&source_task_id)
                        .unwrap()
                        .cancel
                        .send_replace(true);
                }
                "run-drop" => core.sessions.inner.remove_run(&source_task_id, "run-a"),
                _ => unreachable!(),
            }
            let result = core
                .sessions
                .dispatch_luna_interrupt_grant_once(grant)
                .await;
            assert_eq!(
                result.unwrap_err(),
                if change == "expiry" {
                    CrossTaskError::UserRequest
                } else {
                    CrossTaskError::Unavailable
                },
                "{change}"
            );
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn private_interrupt_dispatch_rejects_changed_target_before_submit() {
        use crate::cross_task_tools::CrossTaskError;

        for change in [
            "revision",
            "sequence",
            "binding-id",
            "binding-generation",
            "active-turn",
            "project",
            "environment",
        ] {
            let (_root, core, _source_task_id, backend, grant, _events) =
                approved_interrupt_fixture().await;
            *lock(&backend.interrupt_mode) = Some(FakeInterruptMode::Receipt(
                luna_proto::CommandReceiptStatus::Applied,
            ));
            match change {
                "project" => lock(&backend.target).project_id = Some("project-b".into()),
                "environment" => lock(&backend.target).environment_id = "remote".into(),
                _ => {
                    let mut snapshot = lock(&backend.snapshots).front().unwrap().clone();
                    match change {
                        "revision" => snapshot.task_revision += 1,
                        "sequence" => snapshot.task_sequence += 1,
                        "binding-id" => snapshot.binding_id = Some("new-binding".into()),
                        "binding-generation" => snapshot.binding_generation = Some(3),
                        "active-turn" => snapshot.active_turn_id = Some("new-target-turn".into()),
                        _ => unreachable!(),
                    }
                    *lock(&backend.snapshots) = VecDeque::from([snapshot]);
                }
            }
            let result = core
                .sessions
                .dispatch_luna_interrupt_grant_once(grant)
                .await;
            assert_eq!(
                result.unwrap_err(),
                if matches!(change, "project" | "environment") {
                    CrossTaskError::TargetScope
                } else {
                    CrossTaskError::StaleSnapshot
                },
                "{change}"
            );
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn private_interrupt_dispatch_rejects_source_or_target_change_during_read() {
        use crate::cross_task_tools::CrossTaskError;

        for change in ["rebind", "target-turn", "cancel", "run-drop"] {
            let (_root, core, source_task_id, backend, grant, entered, release) =
                approved_gated_interrupt_fixture().await;
            *lock(&backend.interrupt_mode) = Some(FakeInterruptMode::Receipt(
                luna_proto::CommandReceiptStatus::Applied,
            ));
            let sessions = core.sessions.clone();
            let dispatching =
                tokio::spawn(
                    async move { sessions.dispatch_luna_interrupt_grant_once(grant).await },
                );
            tokio::time::timeout(Duration::from_secs(2), entered.notified())
                .await
                .expect("pre-dispatch target read entered");
            match change {
                "rebind" => {
                    core.task_identity
                        .advance_binding(
                            &source_task_id,
                            HarnessId::Codex,
                            "provider-thread-b",
                            luna_proto::BindingOrigin::ContextReset,
                            "synthetic rebind before submit",
                        )
                        .unwrap();
                }
                "target-turn" => {
                    let mut snapshot = lock(&backend.snapshots).front().unwrap().clone();
                    snapshot.active_turn_id = Some("new-target-turn".into());
                    *lock(&backend.snapshots) = VecDeque::from([snapshot]);
                }
                "cancel" => {
                    lock(&core.sessions.inner.runs)
                        .get(&source_task_id)
                        .unwrap()
                        .cancel
                        .send_replace(true);
                }
                "run-drop" => core.sessions.inner.remove_run(&source_task_id, "run-a"),
                _ => unreachable!(),
            }
            release.notify_one();
            let result = tokio::time::timeout(Duration::from_secs(2), dispatching)
                .await
                .expect("dispatch settled")
                .unwrap();
            assert_eq!(
                result.unwrap_err(),
                match change {
                    "target-turn" => CrossTaskError::StaleSnapshot,
                    "cancel" | "run-drop" => CrossTaskError::Cancelled,
                    _ => CrossTaskError::Unavailable,
                },
                "{change}"
            );
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn private_interrupt_grant_has_one_consumer_under_concurrency() {
        let (_root, core, _source_task_id, backend, grant, _events) =
            approved_interrupt_fixture().await;
        *lock(&backend.interrupt_mode) = Some(FakeInterruptMode::Receipt(
            luna_proto::CommandReceiptStatus::Pending,
        ));
        let grant = Arc::new(Mutex::new(Some(grant)));
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let consumers = (0..2)
            .map(|_| {
                let sessions = core.sessions.clone();
                let grant = grant.clone();
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    barrier.wait().await;
                    let one = lock(&grant).take();
                    match one {
                        Some(grant) => {
                            Some(sessions.dispatch_luna_interrupt_grant_once(grant).await)
                        }
                        None => None,
                    }
                })
            })
            .collect::<Vec<_>>();
        barrier.wait().await;
        let outcomes = futures::future::join_all(consumers).await;
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.as_ref().unwrap().is_some())
                .count(),
            1
        );
        assert!(
            outcomes
                .into_iter()
                .map(Result::unwrap)
                .flatten()
                .next()
                .unwrap()
                .is_ok()
        );
        assert_eq!(backend.control_calls.load(Ordering::SeqCst), 1);
        assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn prospective_interrupt_registers_one_exact_inert_pending_event() {
        let (_root, core, source_task_id, backend, mut evidence, mut events) =
            prospective_interrupt_fixture().await;
        evidence.target_snapshot.binding_id = Some("private-binding-sentinel".into());
        evidence.target_snapshot.active_turn_id = Some("private-turn-sentinel".into());
        *lock(&backend.snapshots) = VecDeque::from([evidence.target_snapshot.clone()]);
        let expected_snapshot = evidence.target_snapshot.clone();
        let (request_id, answer, grant) = core
            .sessions
            .register_luna_prospective_interrupt(evidence)
            .await
            .unwrap();
        assert!(!request_id.is_empty());
        assert!(
            core.sessions
                .is_luna_pending_approval(&source_task_id, &request_id)
        );
        let event = events.try_recv().expect("one engine-owned request");
        let luna_proto::AgentEvent::InputRequested {
            request_id: published_id,
            questions,
        } = event
        else {
            panic!("expected InputRequested");
        };
        assert_eq!(published_id, request_id);
        assert_eq!(questions.len(), 1);
        let approval = questions[0].approval.as_ref().unwrap();
        assert_eq!(approval.provider_request_id, "call-a");
        assert_eq!(approval.turn_id.as_deref(), Some("turn-a"));
        assert_eq!(
            approval.action_kind,
            luna_proto::ApprovalActionKind::LunaCrossTask
        );
        assert!(!approval.supports_task_allowance);
        assert!(approval.expires_at_ms > crate::now_ms());
        assert!(approval.expires_at_ms <= crate::now_ms() + 30_000);
        assert!(
            questions[0]
                .question
                .contains("active turn in task target-a")
        );
        assert!(questions[0].question.contains("task target-a"));
        assert!(questions[0].question.contains("revision 7, sequence 12"));
        assert!(
            questions[0]
                .question
                .contains("cannot execute the interruption yet")
        );
        let serialized_question = serde_json::to_string(&questions[0]).unwrap();
        assert!(!serialized_question.contains("private-binding-sentinel"));
        assert!(!serialized_question.contains("private-turn-sentinel"));
        assert!(!approval.summary.contains("private-binding-sentinel"));
        assert!(!approval.summary.contains("private-turn-sentinel"));
        let pending = lock(&core.sessions.inner.runs)
            .get(&source_task_id)
            .unwrap()
            .pending_inputs
            .clone();
        {
            let entries = lock(&pending);
            let stored = entries.get(&request_id).unwrap();
            let super::PendingInputKind::LunaProspectiveInterrupt {
                run_id, proposal, ..
            } = &stored.kind
            else {
                panic!("expected server-owned prospective kind");
            };
            assert_eq!(run_id, "run-a");
            assert_eq!(
                proposal.action,
                luna_proto::CrossTaskControlAction::Interrupt
            );
            assert_eq!(proposal.evidence.target_snapshot, expected_snapshot);
            assert!(proposal.matches_pending(
                &source_task_id,
                run_id,
                &request_id,
                &stored.questions[0]
            ));
        }
        assert_eq!(backend.snapshot_reads.load(Ordering::SeqCst), 3);
        assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert!(
            !core
                .sessions
                .respond_input(&source_task_id, &request_id, Vec::new())
                .unwrap()
        );
        assert!(
            !core
                .sessions
                .respond_approval(
                    &source_task_id,
                    &request_id,
                    luna_proto::ApprovalDecision::AllowOnce,
                )
                .unwrap()
        );
        assert_eq!(
            core.sessions.respond_luna_local_ui_approval(
                &source_task_id,
                &request_id,
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
        assert!(
            core.sessions
                .is_luna_pending_approval(&source_task_id, &request_id)
        );
        assert!(expire_pending_approval(
            &pending,
            &lock(&core.sessions.inner.runs)
                .get(&source_task_id)
                .unwrap()
                .engine_tx,
            &request_id,
        ));
        assert!(answer.await.unwrap().is_empty());
        assert!(grant.await.is_err());
        assert!(matches!(
            events.try_recv(),
            Ok(luna_proto::AgentEvent::InputResolved { request_id: resolved })
                if resolved == request_id
        ));
        assert!(!expire_pending_approval(
            &pending,
            &lock(&core.sessions.inner.runs)
                .get(&source_task_id)
                .unwrap()
                .engine_tx,
            &request_id,
        ));
    }

    #[tokio::test]
    async fn prospective_interrupt_local_allow_once_mints_only_one_private_grant() {
        let (_root, core, source_task_id, backend, evidence, mut events) =
            prospective_interrupt_fixture().await;
        let expected_snapshot = evidence.target_snapshot.clone();
        let (request_id, answer, grant) = core
            .sessions
            .register_luna_prospective_interrupt(evidence)
            .await
            .unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(luna_proto::AgentEvent::InputRequested { .. })
        ));
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let responders = (0..2)
            .map(|_| {
                let sessions = core.sessions.clone();
                let task = source_task_id.clone();
                let request = request_id.clone();
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    barrier.wait().await;
                    sessions
                        .respond_luna_prospective_interrupt_local_ui(
                            &task,
                            &request,
                            luna_proto::ApprovalDecision::AllowOnce,
                        )
                        .await
                })
            })
            .collect::<Vec<_>>();
        barrier.wait().await;
        let outcomes = futures::future::join_all(responders)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == LunaApprovalOutcome::AllowedOnce)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == LunaApprovalOutcome::Stale)
                .count(),
            1
        );
        assert_eq!(answer.await.unwrap()[0].labels, vec!["Allow once"]);
        let grant = grant.await.unwrap();
        assert_eq!(grant.proposal.request_id, request_id);
        assert_eq!(grant.proposal.evidence.target_snapshot, expected_snapshot);
        assert_eq!(
            grant.proposal.action,
            luna_proto::CrossTaskControlAction::Interrupt
        );
        assert!(matches!(
            events.try_recv(),
            Ok(luna_proto::AgentEvent::InputResolved { request_id: resolved })
                if resolved == request_id
        ));
        assert!(
            !core
                .sessions
                .is_luna_pending_approval(&source_task_id, &request_id)
        );
        assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn prospective_interrupt_local_denial_and_lost_receivers_mint_no_grant() {
        for decision in [
            luna_proto::ApprovalDecision::Deny,
            luna_proto::ApprovalDecision::Cancel,
            luna_proto::ApprovalDecision::AllowTask,
        ] {
            let (_root, core, source_task_id, backend, evidence, mut events) =
                prospective_interrupt_fixture().await;
            let (request_id, answer, grant) = core
                .sessions
                .register_luna_prospective_interrupt(evidence)
                .await
                .unwrap();
            assert!(matches!(
                events.try_recv(),
                Ok(luna_proto::AgentEvent::InputRequested { .. })
            ));
            assert_eq!(
                core.sessions
                    .respond_luna_prospective_interrupt_local_ui(
                        &source_task_id,
                        &request_id,
                        decision,
                    )
                    .await,
                LunaApprovalOutcome::Denied
            );
            let label = if decision == luna_proto::ApprovalDecision::Cancel {
                "Cancel"
            } else {
                "Deny"
            };
            assert_eq!(answer.await.unwrap()[0].labels, vec![label]);
            assert!(grant.await.is_err());
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        }

        let (_root, core, source_task_id, backend, evidence, _events) =
            prospective_interrupt_fixture().await;
        let (request_id, answer, grant) = core
            .sessions
            .register_luna_prospective_interrupt(evidence)
            .await
            .unwrap();
        drop(answer);
        assert_eq!(
            core.sessions
                .respond_luna_prospective_interrupt_local_ui(
                    &source_task_id,
                    &request_id,
                    luna_proto::ApprovalDecision::AllowOnce,
                )
                .await,
            LunaApprovalOutcome::RunEnded
        );
        let pending = lock(&core.sessions.inner.runs)
            .get(&source_task_id)
            .unwrap()
            .pending_inputs
            .clone();
        let engine_tx = lock(&core.sessions.inner.runs)
            .get(&source_task_id)
            .unwrap()
            .engine_tx
            .clone();
        assert!(expire_pending_approval(&pending, &engine_tx, &request_id));
        assert!(grant.await.is_err());
        assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);

        let (_root, core, source_task_id, backend, evidence, _events) =
            prospective_interrupt_fixture().await;
        let (request_id, answer, grant) = core
            .sessions
            .register_luna_prospective_interrupt(evidence)
            .await
            .unwrap();
        drop(grant);
        assert_eq!(
            core.sessions
                .respond_luna_prospective_interrupt_local_ui(
                    &source_task_id,
                    &request_id,
                    luna_proto::ApprovalDecision::AllowOnce,
                )
                .await,
            LunaApprovalOutcome::RunEnded
        );
        let pending = lock(&core.sessions.inner.runs)
            .get(&source_task_id)
            .unwrap()
            .pending_inputs
            .clone();
        let engine_tx = lock(&core.sessions.inner.runs)
            .get(&source_task_id)
            .unwrap()
            .engine_tx
            .clone();
        assert!(expire_pending_approval(&pending, &engine_tx, &request_id));
        assert!(answer.await.unwrap().is_empty());
        assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn prospective_interrupt_local_allow_once_rejects_every_changed_target_field() {
        for field in [
            "revision",
            "sequence",
            "binding-id",
            "binding-generation",
            "active-turn",
            "project",
            "environment",
        ] {
            let (_root, core, source_task_id, backend, evidence, mut events) =
                prospective_interrupt_fixture().await;
            let (request_id, answer, grant) = core
                .sessions
                .register_luna_prospective_interrupt(evidence)
                .await
                .unwrap();
            assert!(matches!(
                events.try_recv(),
                Ok(luna_proto::AgentEvent::InputRequested { .. })
            ));
            match field {
                "project" => lock(&backend.target).project_id = Some("project-b".into()),
                "environment" => lock(&backend.target).environment_id = "remote".into(),
                _ => {
                    let mut changed = lock(&backend.snapshots).front().unwrap().clone();
                    match field {
                        "revision" => changed.task_revision += 1,
                        "sequence" => changed.task_sequence += 1,
                        "binding-id" => changed.binding_id = Some("new-binding".into()),
                        "binding-generation" => changed.binding_generation = Some(3),
                        "active-turn" => changed.active_turn_id = Some("new-target-turn".into()),
                        _ => unreachable!(),
                    }
                    *lock(&backend.snapshots) = VecDeque::from([changed]);
                }
            }
            assert_eq!(
                core.sessions
                    .respond_luna_prospective_interrupt_local_ui(
                        &source_task_id,
                        &request_id,
                        luna_proto::ApprovalDecision::AllowOnce,
                    )
                    .await,
                LunaApprovalOutcome::Stale,
                "{field}"
            );
            assert!(answer.await.unwrap().is_empty(), "{field}");
            assert!(grant.await.is_err(), "{field}");
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn prospective_interrupt_local_allow_once_rejects_source_or_question_drift() {
        for drift in ["rebind", "turn", "question", "expired"] {
            let (_root, core, source_task_id, backend, evidence, _events) =
                prospective_interrupt_fixture().await;
            let (request_id, answer, grant) = core
                .sessions
                .register_luna_prospective_interrupt(evidence)
                .await
                .unwrap();
            match drift {
                "rebind" => {
                    core.task_identity
                        .advance_binding(
                            &source_task_id,
                            HarnessId::Codex,
                            "provider-thread-b",
                            luna_proto::BindingOrigin::ContextReset,
                            "synthetic rebind after question",
                        )
                        .unwrap();
                }
                "turn" => assert!(core.sessions.inner.observe_codex_checkpoint(
                    &source_task_id,
                    "run-a",
                    Some("provider-thread-a"),
                    "turn-b",
                )),
                "question" | "expired" => {
                    let pending = lock(&core.sessions.inner.runs)
                        .get(&source_task_id)
                        .unwrap()
                        .pending_inputs
                        .clone();
                    let mut entries = lock(&pending);
                    let entry = entries.get_mut(&request_id).unwrap();
                    if drift == "question" {
                        entry.questions[0].question = "forged target".into();
                    } else if let super::PendingInputKind::LunaProspectiveInterrupt {
                        proposal,
                        ..
                    } = &mut entry.kind
                    {
                        proposal.expires_at_ms = crate::now_ms() - 1;
                    }
                }
                _ => unreachable!(),
            }
            let outcome = core
                .sessions
                .respond_luna_prospective_interrupt_local_ui(
                    &source_task_id,
                    &request_id,
                    luna_proto::ApprovalDecision::AllowOnce,
                )
                .await;
            assert_eq!(
                outcome,
                if drift == "expired" {
                    LunaApprovalOutcome::Expired
                } else {
                    LunaApprovalOutcome::Stale
                },
                "{drift}"
            );
            let pending = lock(&core.sessions.inner.runs)
                .get(&source_task_id)
                .unwrap()
                .pending_inputs
                .clone();
            let engine_tx = lock(&core.sessions.inner.runs)
                .get(&source_task_id)
                .unwrap()
                .engine_tx
                .clone();
            expire_pending_approval(&pending, &engine_tx, &request_id);
            assert!(answer.await.unwrap().is_empty(), "{drift}");
            assert!(grant.await.is_err(), "{drift}");
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn prospective_interrupt_local_allow_once_rejects_changes_during_target_read() {
        for change in ["source-rebind", "target-turn", "cancel", "run-drop"] {
            let (root, core, source_task_id) = managed_codex_wait_fixture();
            let claim = ready_control_evidence_claim(&root, &core, &source_task_id);
            let mut backend = control_evidence_backend(&core, &source_task_id);
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let fake = Arc::get_mut(&mut backend).unwrap();
            fake.gate_on_read = 4;
            fake.entered_snapshot = Some(entered.clone());
            fake.release_snapshot = Some(release.clone());
            register_control_evidence_backend(&core, &backend);
            let evidence = core
                .sessions
                .prepare_luna_control_evidence(
                    claim,
                    &control_evidence_authority(&core, &source_task_id),
                    "target-a",
                )
                .await
                .unwrap();
            let (engine_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
            lock(&core.sessions.inner.runs)
                .get_mut(&source_task_id)
                .unwrap()
                .engine_tx = engine_tx;
            let (request_id, answer, grant) = core
                .sessions
                .register_luna_prospective_interrupt(evidence)
                .await
                .unwrap();
            assert!(matches!(
                events.try_recv(),
                Ok(luna_proto::AgentEvent::InputRequested { .. })
            ));
            let sessions = core.sessions.clone();
            let task = source_task_id.clone();
            let request = request_id.clone();
            let responding = tokio::spawn(async move {
                sessions
                    .respond_luna_prospective_interrupt_local_ui(
                        &task,
                        &request,
                        luna_proto::ApprovalDecision::AllowOnce,
                    )
                    .await
            });
            tokio::time::timeout(Duration::from_secs(2), entered.notified())
                .await
                .expect("approval reached target read");
            match change {
                "source-rebind" => {
                    core.task_identity
                        .advance_binding(
                            &source_task_id,
                            HarnessId::Codex,
                            "provider-thread-b",
                            luna_proto::BindingOrigin::ContextReset,
                            "synthetic rebind during approval",
                        )
                        .unwrap();
                }
                "target-turn" => {
                    let mut snapshot = lock(&backend.snapshots).front().unwrap().clone();
                    snapshot.active_turn_id = Some("new-target-turn".into());
                    *lock(&backend.snapshots) = VecDeque::from([snapshot]);
                }
                "cancel" => {
                    lock(&core.sessions.inner.runs)
                        .get(&source_task_id)
                        .unwrap()
                        .cancel
                        .send_replace(true);
                }
                "run-drop" => core.sessions.inner.remove_run(&source_task_id, "run-a"),
                _ => unreachable!(),
            }
            release.notify_one();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), responding)
                    .await
                    .expect("approval settled")
                    .unwrap(),
                if matches!(change, "cancel" | "run-drop") {
                    LunaApprovalOutcome::RunEnded
                } else {
                    LunaApprovalOutcome::Stale
                },
                "{change}"
            );
            assert!(answer.await.unwrap().is_empty(), "{change}");
            assert!(grant.await.is_err(), "{change}");
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn prospective_interrupt_local_allow_once_rejects_missing_backend_and_replay() {
        let (_root, core, source_task_id, backend, evidence, _events) =
            prospective_interrupt_fixture().await;
        let (request_id, answer, grant) = core
            .sessions
            .register_luna_prospective_interrupt(evidence)
            .await
            .unwrap();
        assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
        drop(backend);
        assert_eq!(
            core.sessions
                .respond_luna_prospective_interrupt_local_ui(
                    &source_task_id,
                    &request_id,
                    luna_proto::ApprovalDecision::AllowOnce,
                )
                .await,
            LunaApprovalOutcome::Stale
        );
        assert!(answer.await.unwrap().is_empty());
        assert!(grant.await.is_err());
        assert_eq!(
            core.sessions
                .respond_luna_prospective_interrupt_local_ui(
                    &source_task_id,
                    &request_id,
                    luna_proto::ApprovalDecision::AllowOnce,
                )
                .await,
            LunaApprovalOutcome::Stale
        );
    }

    #[tokio::test]
    async fn prospective_interrupt_rejects_changed_or_unscoped_evidence_without_event() {
        use crate::cross_task_tools::CrossTaskError;

        for failure in [
            "idle-target",
            "changed-snapshot",
            "foreign-project",
            "remote-environment",
            "missing-backend",
            "closed-event-channel",
        ] {
            let (_root, core, source_task_id, backend, mut evidence, mut events) =
                prospective_interrupt_fixture().await;
            match failure {
                "idle-target" => evidence.target_snapshot.active_turn_id = None,
                "changed-snapshot" => {
                    let mut changed = evidence.target_snapshot.clone();
                    changed.active_turn_id = Some("new-target-turn".into());
                    *lock(&backend.snapshots) = VecDeque::from([changed]);
                }
                "foreign-project" => {
                    lock(&backend.target).project_id = Some("project-b".into());
                }
                "remote-environment" => {
                    lock(&backend.target).environment_id = "remote".into();
                }
                "missing-backend" => {}
                "closed-event-channel" => events.close(),
                _ => unreachable!(),
            }
            if failure == "missing-backend" {
                drop(backend);
                // The backend is held only through a Weak edge in sessions.
                let result = core
                    .sessions
                    .register_luna_prospective_interrupt(evidence)
                    .await;
                assert!(matches!(result, Err(CrossTaskError::Unavailable)));
            } else {
                let result = core
                    .sessions
                    .register_luna_prospective_interrupt(evidence)
                    .await;
                let expected = match failure {
                    "changed-snapshot" => CrossTaskError::StaleSnapshot,
                    "closed-event-channel" => CrossTaskError::Unavailable,
                    _ => CrossTaskError::TargetScope,
                };
                assert_eq!(result.unwrap_err(), expected, "{failure}");
                assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
                assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
            }
            let pending = lock(&core.sessions.inner.runs)
                .get(&source_task_id)
                .unwrap()
                .pending_inputs
                .clone();
            assert!(lock(&pending).is_empty(), "{failure}");
            assert!(!matches!(
                events.try_recv(),
                Ok(luna_proto::AgentEvent::InputRequested { .. })
            ));
        }
    }

    #[tokio::test]
    async fn prospective_interrupt_rejects_stale_source_and_rebind_during_read() {
        use crate::cross_task_tools::CrossTaskError;

        for change in ["rebind", "turn", "cancel", "run-drop"] {
            let (_root, core, source_task_id, backend, evidence, mut events) =
                prospective_interrupt_fixture().await;
            match change {
                "rebind" => {
                    core.task_identity
                        .advance_binding(
                            &source_task_id,
                            HarnessId::Codex,
                            "provider-thread-b",
                            luna_proto::BindingOrigin::ContextReset,
                            "synthetic rebind",
                        )
                        .unwrap();
                }
                "turn" => assert!(core.sessions.inner.observe_codex_checkpoint(
                    &source_task_id,
                    "run-a",
                    Some("provider-thread-a"),
                    "turn-b",
                )),
                "cancel" => {
                    lock(&core.sessions.inner.runs)
                        .get(&source_task_id)
                        .unwrap()
                        .cancel
                        .send_replace(true);
                }
                "run-drop" => core.sessions.inner.remove_run(&source_task_id, "run-a"),
                _ => unreachable!(),
            }
            let result = core
                .sessions
                .register_luna_prospective_interrupt(evidence)
                .await;
            assert!(
                matches!(result, Err(CrossTaskError::Unavailable)),
                "{change}"
            );
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
            assert!(!matches!(
                events.try_recv(),
                Ok(luna_proto::AgentEvent::InputRequested { .. })
            ));
        }

        let (root, core, source_task_id) = managed_codex_wait_fixture();
        let claim = ready_control_evidence_claim(&root, &core, &source_task_id);
        let mut backend = control_evidence_backend(&core, &source_task_id);
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let fake = Arc::get_mut(&mut backend).unwrap();
        fake.gate_on_read = 3;
        fake.entered_snapshot = Some(entered.clone());
        fake.release_snapshot = Some(release.clone());
        register_control_evidence_backend(&core, &backend);
        let evidence = core
            .sessions
            .prepare_luna_control_evidence(
                claim,
                &control_evidence_authority(&core, &source_task_id),
                "target-a",
            )
            .await
            .unwrap();
        let (engine_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        lock(&core.sessions.inner.runs)
            .get_mut(&source_task_id)
            .unwrap()
            .engine_tx = engine_tx;
        let sessions = core.sessions.clone();
        let registering =
            tokio::spawn(
                async move { sessions.register_luna_prospective_interrupt(evidence).await },
            );
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .expect("registration reached target read");
        core.task_identity
            .advance_binding(
                &source_task_id,
                HarnessId::Codex,
                "provider-thread-b",
                luna_proto::BindingOrigin::ContextReset,
                "synthetic rebind during read",
            )
            .unwrap();
        release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(2), registering)
            .await
            .expect("registration settled")
            .unwrap();
        assert!(matches!(result, Err(CrossTaskError::Unavailable)));
        assert!(
            lock(&core.sessions.inner.runs)
                .get(&source_task_id)
                .unwrap()
                .pending_inputs
                .lock()
                .unwrap()
                .is_empty()
        );
        assert!(!matches!(
            events.try_recv(),
            Ok(luna_proto::AgentEvent::InputRequested { .. })
        ));
    }

    #[tokio::test]
    async fn prospective_interrupt_cleanup_is_bounded_and_restart_cannot_revive_it() {
        for end in ["deadline", "cancel", "readiness"] {
            let (_root, core, source_task_id, backend, evidence, mut events) =
                prospective_interrupt_fixture().await;
            let (request_id, answer, grant) = core
                .sessions
                .register_luna_prospective_interrupt(evidence)
                .await
                .unwrap();
            assert!(matches!(
                events.try_recv(),
                Ok(luna_proto::AgentEvent::InputRequested { .. })
            ));
            match end {
                "deadline" => {
                    let (pending, engine_tx, interrupt, cancel, readiness) = {
                        let runs = lock(&core.sessions.inner.runs);
                        let run = runs.get(&source_task_id).unwrap();
                        (
                            run.pending_inputs.clone(),
                            run.engine_tx.clone(),
                            run.interrupt_token.clone(),
                            run.cancel.subscribe(),
                            run.provider_turn_readiness.subscribe(),
                        )
                    };
                    super::schedule_luna_prospective_cleanup(
                        &pending,
                        engine_tx,
                        request_id.clone(),
                        crate::now_ms() + 10,
                        interrupt,
                        cancel,
                        readiness,
                    );
                }
                "cancel" => {
                    lock(&core.sessions.inner.runs)
                        .get(&source_task_id)
                        .unwrap()
                        .cancel
                        .send_replace(true);
                }
                "readiness" => core
                    .sessions
                    .inner
                    .clear_codex_checkpoint(&source_task_id, "run-a"),
                _ => unreachable!(),
            }
            assert!(
                tokio::time::timeout(Duration::from_secs(2), answer)
                    .await
                    .expect("cleanup settled")
                    .unwrap()
                    .is_empty()
            );
            assert!(grant.await.is_err());
            assert!(matches!(
                events.try_recv(),
                Ok(luna_proto::AgentEvent::InputResolved { request_id: resolved })
                    if resolved == request_id
            ));
            assert!(
                !core
                    .sessions
                    .is_luna_pending_approval(&source_task_id, &request_id)
            );
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        }

        let (root, core, source_task_id, backend, evidence, mut events) =
            prospective_interrupt_fixture().await;
        let (request_id, answer, grant) = core
            .sessions
            .register_luna_prospective_interrupt(evidence)
            .await
            .unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(luna_proto::AgentEvent::InputRequested { .. })
        ));
        core.sessions.inner.remove_run(&source_task_id, "run-a");
        assert!(
            tokio::time::timeout(Duration::from_secs(2), answer)
                .await
                .expect("run drop settled")
                .is_err()
        );
        assert!(grant.await.is_err());
        assert!(
            !core
                .sessions
                .is_luna_pending_approval(&source_task_id, &request_id)
        );
        assert_eq!(
            core.sessions.respond_luna_local_ui_approval(
                &source_task_id,
                &request_id,
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
        assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        drop(core);
        drop(backend);
        drop(events);

        let accounts = crate::agent_accounts::AgentAccountsConfig {
            data_dir: root.path().join("synthetic-provider-state"),
            claude_config_dir: root.path().join("synthetic-claude"),
            claude_config_file: root.path().join("synthetic-claude.json"),
            codex_home: root.path().join("synthetic-codex"),
        };
        accounts.disable_claude_keychain_fallback().unwrap();
        let restarted = crate::EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            crate::EngineProfile::development(
                &root.path().join("synthetic-engine"),
                "synthetic-org",
                "synthetic-user",
            ),
            Arc::new(crate::registry::HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            accounts,
        )
        .unwrap();
        assert!(
            !restarted
                .sessions
                .is_luna_pending_approval(&source_task_id, &request_id)
        );
        assert_eq!(
            restarted.sessions.respond_luna_local_ui_approval(
                &source_task_id,
                &request_id,
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
    }

    #[tokio::test]
    async fn provider_wait_returns_exact_claimed_call_evidence_once() {
        let (root, core, task_id) = managed_codex_wait_fixture();
        let sessions = core.sessions.clone();
        let waiting_task = task_id.clone();
        let waiter = tokio::spawn(async move {
            sessions
                .wait_and_claim_luna_provider_call_evidence(
                    &waiting_task,
                    "run-a",
                    "provider-thread-a",
                    "turn-a",
                    "call-a",
                    tokio::time::Instant::now() + Duration::from_secs(5),
                )
                .await
        });
        tokio::task::yield_now().await;
        publish_managed_codex_binding(&root, &core, &task_id);
        assert!(!waiter.is_finished());
        assert!(core.sessions.inner.observe_codex_checkpoint(
            &task_id,
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
        ));
        let claim = waiter.await.unwrap().expect("ready provider call claim");
        assert_eq!(claim.source_task_id, task_id);
        assert_eq!(claim.source_run_id, "run-a");
        assert_eq!(claim.source_binding.task_id, task_id);
        assert!(!claim.source_binding.binding_id.is_empty());
        assert_eq!(claim.source_binding.generation, 1);
        assert_eq!(claim.source_binding.provider_thread_id, "provider-thread-a");
        assert_eq!(claim.provider_turn_id, "turn-a");
        assert_eq!(claim.provider_call_id, "call-a");
        assert!(core.sessions.claimed_call_is_live(&claim));
        assert!(
            core.sessions
                .claim_luna_provider_call_evidence(
                    &task_id,
                    "run-a",
                    "provider-thread-a",
                    "turn-a",
                    "call-a",
                )
                .is_none()
        );
    }

    #[tokio::test]
    async fn control_preparation_keeps_only_exact_read_only_evidence() {
        let (root, core, source_task_id) = managed_codex_wait_fixture();
        let claim = ready_control_evidence_claim(&root, &core, &source_task_id);
        let backend = control_evidence_backend(&core, &source_task_id);
        register_control_evidence_backend(&core, &backend);
        let expected_snapshot = lock(&backend.snapshots).front().unwrap().clone();
        let evidence = core
            .sessions
            .prepare_luna_control_evidence(
                claim,
                &control_evidence_authority(&core, &source_task_id),
                "target-a",
            )
            .await
            .unwrap();
        assert_eq!(evidence.claim.source_task_id, source_task_id);
        assert_eq!(evidence.claim.source_run_id, "run-a");
        assert_eq!(evidence.claim.provider_turn_id, "turn-a");
        assert_eq!(evidence.claim.provider_call_id, "call-a");
        assert_eq!(evidence.source_project_id, "project-a");
        assert_eq!(evidence.source_environment_id, core.device_id);
        assert_eq!(evidence.target_task_id, "target-a");
        assert_eq!(evidence.target_snapshot, expected_snapshot);
        assert_eq!(backend.snapshot_reads.load(Ordering::SeqCst), 2);
        assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        assert!(
            lock(&core.sessions.inner.runs)
                .get(&source_task_id)
                .unwrap()
                .pending_inputs
                .lock()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn control_preparation_rejects_changed_snapshot_and_unscoped_targets() {
        use crate::cross_task_tools::CrossTaskError;

        let (root, core, source_task_id) = managed_codex_wait_fixture();
        let claim = ready_control_evidence_claim(&root, &core, &source_task_id);
        let backend = control_evidence_backend(&core, &source_task_id);
        let mut changed = lock(&backend.snapshots).front().unwrap().clone();
        changed.task_sequence += 1;
        lock(&backend.snapshots).push_back(changed);
        register_control_evidence_backend(&core, &backend);
        assert_eq!(
            core.sessions
                .prepare_luna_control_evidence(
                    claim,
                    &control_evidence_authority(&core, &source_task_id),
                    "target-a",
                )
                .await
                .unwrap_err(),
            CrossTaskError::StaleSnapshot
        );
        assert_eq!(backend.snapshot_reads.load(Ordering::SeqCst), 2);

        for scope in [
            "foreign-project",
            "remote-environment",
            "manage-scope",
            "user-request",
        ] {
            let (root, core, source_task_id) = managed_codex_wait_fixture();
            let claim = ready_control_evidence_claim(&root, &core, &source_task_id);
            let backend = control_evidence_backend(&core, &source_task_id);
            let mut authority = control_evidence_authority(&core, &source_task_id);
            match scope {
                "foreign-project" => lock(&backend.target).project_id = Some("project-b".into()),
                "remote-environment" => lock(&backend.target).environment_id = "remote".into(),
                "manage-scope" => {
                    authority.granted_scopes.insert("threads.manage".into());
                }
                "user-request" => authority.explicit_user_request = true,
                _ => unreachable!(),
            }
            register_control_evidence_backend(&core, &backend);
            assert_eq!(
                core.sessions
                    .prepare_luna_control_evidence(claim, &authority, "target-a")
                    .await
                    .unwrap_err(),
                CrossTaskError::TargetScope,
                "{scope}"
            );
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn control_preparation_closes_when_source_changes_during_backend_await() {
        use crate::cross_task_tools::CrossTaskError;

        for change in ["rebind", "turn", "run-drop", "cancel"] {
            let (root, core, source_task_id) = managed_codex_wait_fixture();
            let claim = ready_control_evidence_claim(&root, &core, &source_task_id);
            let mut backend = control_evidence_backend(&core, &source_task_id);
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let fake = Arc::get_mut(&mut backend).unwrap();
            fake.entered_snapshot = Some(entered.clone());
            fake.release_snapshot = Some(release.clone());
            register_control_evidence_backend(&core, &backend);
            let sessions = core.sessions.clone();
            let authority = control_evidence_authority(&core, &source_task_id);
            let preparing = tokio::spawn(async move {
                sessions
                    .prepare_luna_control_evidence(claim, &authority, "target-a")
                    .await
            });
            tokio::time::timeout(Duration::from_secs(2), entered.notified())
                .await
                .expect("backend read entered");
            match change {
                "rebind" => {
                    core.task_identity
                        .advance_binding(
                            &source_task_id,
                            HarnessId::Codex,
                            "provider-thread-b",
                            luna_proto::BindingOrigin::ContextReset,
                            "synthetic rebind",
                        )
                        .unwrap();
                }
                "turn" => assert!(core.sessions.inner.observe_codex_checkpoint(
                    &source_task_id,
                    "run-a",
                    Some("provider-thread-a"),
                    "turn-b",
                )),
                "run-drop" => {
                    lock(&core.sessions.inner.runs).remove(&source_task_id);
                }
                "cancel" => {
                    lock(&core.sessions.inner.runs)
                        .get(&source_task_id)
                        .unwrap()
                        .cancel
                        .send_replace(true);
                }
                _ => unreachable!(),
            }
            release.notify_one();
            let result = tokio::time::timeout(Duration::from_secs(2), preparing)
                .await
                .expect("preparation settled")
                .unwrap();
            assert!(
                matches!(
                    &result,
                    Err(CrossTaskError::Unavailable | CrossTaskError::Cancelled)
                ),
                "{change}: {result:?}"
            );
            assert_eq!(backend.control_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.receipt_reads.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn provider_turn_fence_requires_exact_checkpoint_and_claims_each_call_once() {
        let (_root, sessions, _pending, _events) = local_luna_test_run();
        let binding = synthetic_provider_binding();
        assert!(!sessions.inner.observe_codex_checkpoint(
            "chat-a",
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
        )); // No authoritative task binding is installed in this fixture.
        let mut runs = lock(&sessions.inner.runs);
        let run = runs.get_mut("chat-a").unwrap();
        assert!(!run.claim_codex_invocation(
            "chat-a",
            "run-a",
            Some(&binding),
            "provider-thread-a",
            "turn-a",
            "call-a",
        ));
        assert!(!run.observe_codex_checkpoint(
            "chat-a",
            "old-run",
            Some("provider-thread-a"),
            "turn-a",
            Some(&binding),
        ));
        assert!(!run.observe_codex_checkpoint("chat-a", "run-a", None, "turn-a", Some(&binding),));
        assert!(run.observe_codex_binding("chat-a", "run-a", "provider-thread-a", Some(&binding),));
        assert!(!run.observe_codex_checkpoint(
            "chat-a",
            "run-a",
            Some("foreign-thread"),
            "turn-a",
            Some(&binding),
        ));
        assert!(run.observe_codex_binding("chat-a", "run-a", "provider-thread-a", Some(&binding),));
        assert!(!run.observe_codex_checkpoint(
            "chat-a",
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
            None,
        ));
        assert!(run.observe_codex_binding("chat-a", "run-a", "provider-thread-a", Some(&binding),));
        assert!(run.observe_codex_checkpoint(
            "chat-a",
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
            Some(&binding),
        ));
        for (task, thread, turn, call) in [
            (
                "foreign-task",
                "provider-thread-a",
                "turn-a",
                "foreign-task",
            ),
            ("chat-a", "foreign-thread", "turn-a", "foreign-thread"),
            ("chat-a", "provider-thread-a", "old-turn", "old-turn"),
            ("chat-a", "provider-thread-a", "turn-a", ""),
        ] {
            assert!(
                !run.claim_codex_invocation(task, "run-a", Some(&binding), thread, turn, call,)
            );
        }
        assert!(run.claim_codex_invocation(
            "chat-a",
            "run-a",
            Some(&binding),
            "provider-thread-a",
            "turn-a",
            "call-a",
        ));
        assert!(!run.claim_codex_invocation(
            "chat-a",
            "run-a",
            Some(&binding),
            "provider-thread-a",
            "turn-a",
            "call-a",
        ));
        // A duplicate checkpoint cannot reopen an already claimed call ID.
        assert!(run.observe_codex_checkpoint(
            "chat-a",
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
            Some(&binding),
        ));
        assert!(!run.claim_codex_invocation(
            "chat-a",
            "run-a",
            Some(&binding),
            "provider-thread-a",
            "turn-a",
            "call-a",
        ));
        assert!(run.claim_codex_invocation(
            "chat-a",
            "run-a",
            Some(&binding),
            "provider-thread-a",
            "turn-a",
            "call-b",
        ));
        assert!(run.observe_codex_checkpoint(
            "chat-a",
            "run-a",
            Some("provider-thread-a"),
            "turn-b",
            Some(&binding),
        ));
        assert!(!run.claim_codex_invocation(
            "chat-a",
            "run-a",
            Some(&binding),
            "provider-thread-a",
            "turn-a",
            "call-c",
        ));
        assert!(run.claim_codex_invocation(
            "chat-a",
            "run-a",
            Some(&binding),
            "provider-thread-a",
            "turn-b",
            "call-a",
        )); // A call ID belongs to its provider turn, not the whole warm run.
        drop(runs);
        assert_eq!(sessions.active_turn_id("chat-a").as_deref(), Some("run-a"));
    }

    #[test]
    fn provider_turn_fence_rejects_rebinding_replacement_cancel_and_completion() {
        let (_root, sessions, _pending, _events) = local_luna_test_run();
        let binding = synthetic_provider_binding();
        let mut runs = lock(&sessions.inner.runs);
        let run = runs.get_mut("chat-a").unwrap();
        assert!(run.observe_codex_binding("chat-a", "run-a", "provider-thread-a", Some(&binding),));
        assert!(run.observe_codex_checkpoint(
            "chat-a",
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
            Some(&binding),
        ));
        let mut replaced_binding = binding.clone();
        replaced_binding.generation += 1;
        assert!(!run.claim_codex_invocation(
            "chat-a",
            "run-a",
            Some(&replaced_binding),
            "provider-thread-a",
            "turn-a",
            "call-a",
        ));
        assert!(!run.claim_codex_invocation(
            "chat-a",
            "old-run",
            Some(&binding),
            "provider-thread-a",
            "turn-a",
            "call-a",
        ));
        run.run_id = "run-b".into();
        assert!(!run.claim_codex_invocation(
            "chat-a",
            "run-b",
            Some(&binding),
            "provider-thread-a",
            "turn-a",
            "call-a",
        ));
        assert!(run.observe_codex_binding(
            "chat-a",
            "run-b",
            "provider-thread-a",
            Some(&replaced_binding),
        ));
        assert!(run.observe_codex_checkpoint(
            "chat-a",
            "run-b",
            Some("provider-thread-a"),
            "turn-b",
            Some(&replaced_binding),
        ));
        drop(runs);
        sessions.inner.clear_codex_checkpoint("chat-a", "run-a");
        let mut runs = lock(&sessions.inner.runs);
        let run = runs.get_mut("chat-a").unwrap();
        assert!(run.claim_codex_invocation(
            "chat-a",
            "run-b",
            Some(&replaced_binding),
            "provider-thread-a",
            "turn-b",
            "call-b",
        ));
        run.cancel.send_replace(true);
        assert!(!run.claim_codex_invocation(
            "chat-a",
            "run-b",
            Some(&replaced_binding),
            "provider-thread-a",
            "turn-b",
            "call-c",
        ));
        run.cancel.send_replace(false);
        run.interrupt_token.cancel();
        assert!(!run.claim_codex_invocation(
            "chat-a",
            "run-b",
            Some(&replaced_binding),
            "provider-thread-a",
            "turn-b",
            "call-d",
        ));
        drop(runs);
        // Done clears the same fence through this run-scoped helper.
        sessions.inner.clear_codex_checkpoint("chat-a", "run-b");
        assert!(
            lock(&sessions.inner.runs)
                .get("chat-a")
                .unwrap()
                .provider_turn
                .is_none()
        );
    }

    #[tokio::test]
    async fn provider_turn_waits_for_durable_binding_and_checkpoint_then_claims_once() {
        let (root, core, task_id) = managed_codex_wait_fixture();
        let sessions = core.sessions.clone();
        let waiting_task = task_id.clone();
        let waiter = tokio::spawn(async move {
            sessions
                .wait_and_claim_luna_provider_call(
                    &waiting_task,
                    "run-a",
                    "provider-thread-a",
                    "turn-a",
                    "call-a",
                    tokio::time::Instant::now() + Duration::from_secs(5),
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        publish_managed_codex_binding(&root, &core, &task_id);
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished(), "binding alone is not turn readiness");
        assert!(core.sessions.inner.observe_codex_checkpoint(
            &task_id,
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
        ));
        assert!(waiter.await.unwrap());
        assert!(
            !core
                .sessions
                .wait_and_claim_luna_provider_call(
                    &task_id,
                    "run-a",
                    "provider-thread-a",
                    "turn-a",
                    "call-a",
                    tokio::time::Instant::now() + Duration::from_millis(100),
                )
                .await
        );
        assert_eq!(
            core.sessions.active_turn_id(&task_id).as_deref(),
            Some("run-a")
        );
    }

    #[tokio::test]
    async fn provider_turn_wait_rejects_missing_binding_rebind_and_stale_context() {
        let (_root, core, task_id) = managed_codex_wait_fixture();
        let sessions = core.sessions.clone();
        let waiting_task = task_id.clone();
        let waiter = tokio::spawn(async move {
            sessions
                .wait_and_claim_luna_provider_call(
                    &waiting_task,
                    "run-a",
                    "provider-thread-a",
                    "turn-a",
                    "call-a",
                    tokio::time::Instant::now() + Duration::from_secs(5),
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !core
                .sessions
                .inner
                .observe_codex_binding(&task_id, "run-a", "provider-thread-a")
        );
        assert!(!waiter.await.unwrap());

        let (root, core, task_id) = managed_codex_wait_fixture();
        publish_managed_codex_binding(&root, &core, &task_id);
        core.task_identity
            .advance_binding(
                &task_id,
                HarnessId::Codex,
                "provider-thread-b",
                luna_proto::BindingOrigin::ContextReset,
                "synthetic rebind",
            )
            .unwrap();
        assert!(!core.sessions.inner.observe_codex_checkpoint(
            &task_id,
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
        ));
        assert!(
            !core
                .sessions
                .wait_and_claim_luna_provider_call(
                    &task_id,
                    "run-a",
                    "provider-thread-a",
                    "turn-a",
                    "call-b",
                    tokio::time::Instant::now() + Duration::from_millis(100),
                )
                .await
        );

        let (root, core, task_id) = managed_codex_wait_fixture();
        publish_managed_codex_binding(&root, &core, &task_id);
        assert!(core.sessions.inner.observe_codex_checkpoint(
            &task_id,
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
        ));
        assert!(
            !core
                .sessions
                .wait_and_claim_luna_provider_call(
                    &task_id,
                    "run-a",
                    "provider-thread-a",
                    "stale-turn",
                    "call-a",
                    tokio::time::Instant::now() + Duration::from_millis(100),
                )
                .await
        );
    }

    #[tokio::test]
    async fn provider_turn_wait_wakes_closed_on_cancel_done_and_run_drop() {
        for end in ["interrupt", "engine-cancel", "done", "replacement"] {
            let (_root, core, task_id) = managed_codex_wait_fixture();
            let sessions = core.sessions.clone();
            let waiting_task = task_id.clone();
            let waiter = tokio::spawn(async move {
                sessions
                    .wait_and_claim_luna_provider_call(
                        &waiting_task,
                        "run-a",
                        "provider-thread-a",
                        "turn-a",
                        "call-a",
                        tokio::time::Instant::now() + Duration::from_secs(5),
                    )
                    .await
            });
            tokio::task::yield_now().await;
            match end {
                "interrupt" => lock(&core.sessions.inner.runs)
                    .get(&task_id)
                    .unwrap()
                    .interrupt_token
                    .cancel(),
                "engine-cancel" => {
                    lock(&core.sessions.inner.runs)
                        .get(&task_id)
                        .unwrap()
                        .cancel
                        .send_replace(true);
                }
                "done" => core
                    .sessions
                    .inner
                    .clear_codex_checkpoint(&task_id, "run-a"),
                "replacement" => {
                    lock(&core.sessions.inner.runs).remove(&task_id);
                }
                _ => unreachable!(),
            }
            assert!(!waiter.await.unwrap(), "{end} must not claim the call");
        }
    }

    #[tokio::test]
    async fn provider_turn_wait_has_a_strict_cap_and_failed_binding_is_terminal() {
        let (_root, core, task_id) = managed_codex_wait_fixture();
        let started = tokio::time::Instant::now();
        let claimed = core
            .sessions
            .wait_and_claim_luna_provider_call(
                &task_id,
                "run-a",
                "provider-thread-a",
                "turn-a",
                "call-a",
                started + Duration::from_secs(60),
            )
            .await;
        assert!(!claimed);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            !core
                .sessions
                .wait_and_claim_luna_provider_call(
                    &task_id,
                    "run-a",
                    "provider-thread-a",
                    "turn-a",
                    "call-b",
                    tokio::time::Instant::now(),
                )
                .await
        );

        let (root, core, task_id) = managed_codex_wait_fixture();
        publish_managed_codex_binding(&root, &core, &task_id);
        let sessions = core.sessions.clone();
        let waiting_task = task_id.clone();
        let waiter = tokio::spawn(async move {
            sessions
                .wait_and_claim_luna_provider_call(
                    &waiting_task,
                    "run-a",
                    "provider-thread-a",
                    "turn-a",
                    "call-a",
                    tokio::time::Instant::now() + Duration::from_secs(5),
                )
                .await
        });
        tokio::task::yield_now().await;
        let cwd = root.path().join("project-a");
        assert!(
            core.task_identity
                .record_session_binding(
                    &task_id,
                    HarnessId::Codex,
                    "conflicting-thread",
                    cwd.to_str().unwrap()
                )
                .is_err()
        );
        core.sessions.inner.fail_codex_checkpoint(&task_id, "run-a");
        assert!(!waiter.await.unwrap());
        assert!(!core.sessions.inner.observe_codex_checkpoint(
            &task_id,
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
        ));
    }

    #[tokio::test]
    async fn provider_turn_wait_rearms_only_the_same_warm_run_binding() {
        let (root, core, task_id) = managed_codex_wait_fixture();
        publish_managed_codex_binding(&root, &core, &task_id);
        assert!(core.sessions.inner.observe_codex_checkpoint(
            &task_id,
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
        ));
        core.sessions
            .inner
            .clear_codex_checkpoint(&task_id, "run-a");
        core.sessions.inner.rearm_codex_checkpoint_after_done(
            &task_id,
            "run-a",
            Some("provider-thread-a"),
        );
        assert!(core.sessions.inner.observe_codex_checkpoint(
            &task_id,
            "run-a",
            Some("provider-thread-a"),
            "turn-b",
        ));
        assert!(
            core.sessions
                .wait_and_claim_luna_provider_call(
                    &task_id,
                    "run-a",
                    "provider-thread-a",
                    "turn-b",
                    "call-a",
                    tokio::time::Instant::now() + Duration::from_millis(100),
                )
                .await
        );
        core.sessions
            .inner
            .clear_codex_checkpoint(&task_id, "run-a");
        core.task_identity
            .advance_binding(
                &task_id,
                HarnessId::Codex,
                "provider-thread-b",
                luna_proto::BindingOrigin::ContextReset,
                "synthetic warm rebind",
            )
            .unwrap();
        core.sessions.inner.rearm_codex_checkpoint_after_done(
            &task_id,
            "run-a",
            Some("provider-thread-a"),
        );
        assert!(!core.sessions.inner.observe_codex_checkpoint(
            &task_id,
            "run-a",
            Some("provider-thread-a"),
            "turn-c",
        ));
        assert!(
            !core
                .sessions
                .wait_and_claim_luna_provider_call(
                    &task_id,
                    "run-a",
                    "provider-thread-a",
                    "turn-c",
                    "call-b",
                    tokio::time::Instant::now() + Duration::from_millis(100),
                )
                .await
        );
    }

    #[test]
    fn failed_journal_append_has_no_seq_zero_fanout_and_visible_uncertainty() {
        let (_root, sessions, pending, _engine_rx) = local_luna_test_run();
        let (_, mut live) = sessions.subscribe("chat-a", 0).unwrap();
        let token = lock(&sessions.inner.runs)
            .get("chat-a")
            .unwrap()
            .interrupt_token
            .clone();
        let (resolver, mut answer) = tokio::sync::oneshot::channel();
        lock(&pending).insert(
            "question-a".into(),
            PendingInput {
                kind: PendingInputKind::Provider,
                questions: Vec::new(),
                resolver,
            },
        );

        sessions.inner.journal.fail_next_append_for_tests();
        let event = luna_proto::AgentEvent::TextDelta {
            text: "not persisted".into(),
        };
        let error = sessions.inner.publish("chat-a", &event, None).unwrap_err();
        assert!(
            sessions
                .inner
                .journal
                .replay("chat-a", 0)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        stop_after_journal_failure(
            &sessions.inner,
            "chat-a",
            "run-a",
            HarnessId::Codex,
            &request(),
            &RunResumeState {
                user_message_id: "user-a".into(),
                recovery: None,
            },
            None,
            &error,
        );
        assert!(token.is_cancelled());
        assert!(answer.try_recv().unwrap().is_empty());
        let status = sessions
            .watch_sessions()
            .borrow()
            .iter()
            .find(|session| session.chat_id == "chat-a")
            .unwrap()
            .clone();
        assert_eq!(status.status, SessionStatus::Errored);
        let recovery = status.recovery.unwrap();
        assert_eq!(recovery.action, RecoveryAction::ObserveOnly);
        assert_eq!(recovery.status, RecoveryStatus::CheckingDelivery);
        assert!(!recovery.automatic);
        assert!(
            recovery
                .detail
                .contains("local journal write outcome is uncertain")
        );
    }

    /// Synthetic engine-owned registration; no production path creates LUNA
    /// approvals until the later cross-task tool work is authorized.
    fn inject_luna_pending_for_test(
        pending: &PendingInputs,
        request_id: &str,
        bound_run_id: &str,
        expires_at_ms: i64,
    ) -> tokio::sync::oneshot::Receiver<Vec<luna_proto::UserInputAnswer>> {
        let mut proposal = build_fake_archive_proposal(
            request_id,
            "chat-a",
            "local",
            synthetic_provider_binding(),
            "synthetic-luna".into(),
        );
        proposal.source_run_id = bound_run_id.into();
        proposal.expires_at_ms = expires_at_ms;
        let (answer, _grant) = inject_fake_archive_for_test(pending, proposal);
        answer
    }

    #[tokio::test]
    async fn local_luna_approval_is_one_shot_even_for_concurrent_clicks() {
        let (_root, core, task_id, mut events) = managed_archive_fixture();
        let sessions = core.sessions.clone();
        let pending = lock(&sessions.inner.runs)
            .get(&task_id)
            .unwrap()
            .pending_inputs
            .clone();
        let proposal = fake_archive_proposal(&core, &task_id, "approval-a");
        let (answer, grant) = inject_fake_archive_for_test(&pending, proposal);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let calls = (0..2)
            .map(|_| {
                let sessions = sessions.clone();
                let task_id = task_id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    sessions.respond_luna_local_ui_approval(
                        &task_id,
                        "approval-a",
                        luna_proto::ApprovalDecision::AllowOnce,
                    )
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let outcomes = calls
            .into_iter()
            .map(|call| call.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == LunaApprovalOutcome::AllowedOnce)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == LunaApprovalOutcome::Stale)
                .count(),
            1
        );
        assert_eq!(answer.await.unwrap()[0].labels, vec!["Allow once"]);
        assert_eq!(grant.await.unwrap().proposal.source_task_id, task_id);
        assert!(matches!(
            events.try_recv(),
            Ok(luna_proto::AgentEvent::InputResolved { request_id })
                if request_id == "approval-a"
        ));
        assert!(events.try_recv().is_err());
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "approval-a",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
    }

    #[tokio::test]
    async fn local_luna_approval_fails_closed_on_expiry_run_change_and_lost_resolver() {
        let (_root, sessions, pending, _events) = local_luna_test_run();
        let denied = inject_luna_pending_for_test(
            &pending,
            "unsupported",
            "run-a",
            crate::now_ms() + 30_000,
        );
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                "chat-a",
                "unsupported",
                luna_proto::ApprovalDecision::AllowTask,
            ),
            LunaApprovalOutcome::Denied
        );
        assert_eq!(denied.await.unwrap()[0].labels, vec!["Deny"]);

        let denied_by_user =
            inject_luna_pending_for_test(&pending, "user-deny", "run-a", crate::now_ms() + 30_000);
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                "chat-a",
                "user-deny",
                luna_proto::ApprovalDecision::Deny,
            ),
            LunaApprovalOutcome::Denied
        );
        assert_eq!(denied_by_user.await.unwrap()[0].labels, vec!["Deny"]);

        let cancelled_by_user = inject_luna_pending_for_test(
            &pending,
            "user-cancel",
            "run-a",
            crate::now_ms() + 30_000,
        );
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                "chat-a",
                "user-cancel",
                luna_proto::ApprovalDecision::Cancel,
            ),
            LunaApprovalOutcome::Denied
        );
        assert_eq!(cancelled_by_user.await.unwrap()[0].labels, vec!["Cancel"]);

        let expired =
            inject_luna_pending_for_test(&pending, "expired", "run-a", crate::now_ms() - 1);
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                "chat-a",
                "expired",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Expired
        );
        assert!(expired.await.unwrap().is_empty());

        let stale = inject_luna_pending_for_test(
            &pending,
            "stale-run",
            "old-run",
            crate::now_ms() + 30_000,
        );
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                "chat-a",
                "stale-run",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
        assert!(lock(&pending).contains_key("stale-run"));
        drop(stale);

        let replaced = inject_luna_pending_for_test(
            &pending,
            "replaced-run",
            "run-a",
            crate::now_ms() + 30_000,
        );
        lock(&sessions.inner.runs).get_mut("chat-a").unwrap().run_id = "run-b".into();
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                "chat-a",
                "replaced-run",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
        assert!(lock(&pending).contains_key("replaced-run"));
        drop(replaced);
        lock(&sessions.inner.runs).get_mut("chat-a").unwrap().run_id = "run-a".into();

        let cancelled =
            inject_luna_pending_for_test(&pending, "cancelled", "run-a", crate::now_ms() + 30_000);
        lock(&sessions.inner.runs)
            .get("chat-a")
            .unwrap()
            .interrupt_token
            .cancel();
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                "chat-a",
                "cancelled",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::RunEnded
        );
        assert!(cancelled.await.unwrap().is_empty());

        let (_root, core, task_id, _events) = managed_archive_fixture();
        let sessions = core.sessions.clone();
        let pending = lock(&sessions.inner.runs)
            .get(&task_id)
            .unwrap()
            .pending_inputs
            .clone();
        let proposal = fake_archive_proposal(&core, &task_id, "lost-resolver");
        let (lost, grant) = inject_fake_archive_for_test(&pending, proposal);
        drop(lost);
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "lost-resolver",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::RunEnded
        );
        assert!(grant.await.is_err());
        assert!(!lock(&pending).contains_key("lost-resolver"));

        let (forged_tx, _forged_rx) = tokio::sync::oneshot::channel();
        lock(&pending).insert(
            "forged-marker".into(),
            PendingInput {
                kind: PendingInputKind::Provider,
                questions: vec![luna_proto::UserInputQuestion {
                    id: "forged-question".into(),
                    header: "Forged".into(),
                    question: "Allow?".into(),
                    options: vec!["Allow once".into()],
                    multi_select: false,
                    mcp: None,
                    approval: Some(luna_proto::ApprovalRequest {
                        provider_request_id: "claimed-local-ui".into(),
                        provider: HarnessId::Codex,
                        turn_id: None,
                        machine_name: "synthetic".into(),
                        action_kind: luna_proto::ApprovalActionKind::LunaCrossTask,
                        summary: "forged source".into(),
                        requested_scope: luna_proto::PermissionScope::Workspace,
                        expires_at_ms: crate::now_ms() + 30_000,
                        supports_task_allowance: false,
                        details: None,
                    }),
                    input: None,
                }],
                resolver: forged_tx,
            },
        );
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "forged-marker",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
        assert!(lock(&pending).contains_key("forged-marker"));
    }

    struct FakeArchiveBackend {
        target: luna_proto::CrossTaskSummary,
        snapshot: luna_proto::TaskCommandSnapshot,
        live: Mutex<FakeLiveIdentity>,
        calls: AtomicUsize,
        suppress_readback: AtomicBool,
        receipt: Mutex<Option<luna_proto::CrossTaskControlReceipt>>,
    }

    impl FakeArchiveBackend {
        fn readback(&self, command_id: &str) -> Option<luna_proto::CrossTaskControlReceipt> {
            if self.suppress_readback.load(Ordering::SeqCst) {
                return None;
            }
            lock(&self.receipt)
                .as_ref()
                .filter(|receipt| receipt.command.command_id == command_id)
                .cloned()
        }
    }

    #[async_trait::async_trait]
    impl crate::cross_task_tools::CrossTaskBackend for FakeArchiveBackend {
        fn capabilities(&self) -> crate::cross_task_tools::CrossTaskCapabilities {
            crate::cross_task_tools::CrossTaskCapabilities {
                list: false,
                read: false,
                durable_queue: false,
                approved_queue: false,
                event_wait: false,
                child_creation: false,
            }
        }

        async fn task(
            &self,
            task_id: &str,
        ) -> Result<Option<luna_proto::CrossTaskSummary>, crate::cross_task_tools::CrossTaskError>
        {
            Ok((task_id == self.target.task_id).then(|| self.target.clone()))
        }

        async fn list(
            &self,
            _: luna_proto::CrossTaskListRequest,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<luna_proto::CrossTaskListPage, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn read(
            &self,
            _: luna_proto::CrossTaskReadRequest,
        ) -> Result<luna_proto::CrossTaskReadPage, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn enqueue_queue(
            &self,
            _: luna_proto::CrossTaskSendRequest,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<luna_proto::CrossTaskSendReceipt, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn wait_event(
            &self,
            _: luna_proto::CrossTaskWaitRequest,
        ) -> Result<Option<luna_proto::CrossTaskWaitEvent>, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn control(
            &self,
            request: luna_proto::CrossTaskControlRequest,
            _: &crate::cross_task_tools::CrossTaskAuthority,
            _: &crate::cross_task_tools::CrossTaskControlGrant,
        ) -> Result<luna_proto::CrossTaskControlReceipt, crate::cross_task_tools::CrossTaskError>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let receipt = luna_proto::CrossTaskControlReceipt {
                task_id: request.task_id.clone(),
                command: luna_proto::CommandReceipt {
                    command_id: request.command_id,
                    task_id: request.task_id,
                    intent: "setArchived".into(),
                    payload_hash: "synthetic-archive".into(),
                    status: luna_proto::CommandReceiptStatus::Applied,
                    snapshot: self.snapshot.clone(),
                    event_sequence: Some(1),
                    actor_id: "synthetic-luna".into(),
                    client_id: "synthetic-local-ui".into(),
                    client_session_id: "run-a".into(),
                    execution_environment: Some("local".into()),
                    correlation_id: None,
                    reason: None,
                    recovery_action: None,
                    accepted_at_ms: 1,
                    committed_at_ms: Some(1),
                    observed_at_ms: 1,
                },
                observed_status: "idle".into(),
                settled: true,
            };
            *lock(&self.receipt) = Some(receipt.clone());
            Ok(receipt)
        }
    }

    #[derive(Clone)]
    struct FakeLiveIdentity {
        source_task_id: String,
        source_project_id: String,
        source_environment_id: String,
        source_binding: ProviderBindingKey,
        source_run_id: String,
        source_turn_id: String,
        source_call_id: String,
        target_task_id: String,
        target_project_id: String,
        target_environment_id: String,
        target_snapshot: luna_proto::TaskCommandSnapshot,
    }

    impl From<&LunaSyntheticArchiveProposal> for FakeLiveIdentity {
        fn from(proposal: &LunaSyntheticArchiveProposal) -> Self {
            Self {
                source_task_id: proposal.source_task_id.clone(),
                source_project_id: proposal.source_project_id.clone(),
                source_environment_id: proposal.source_environment_id.clone(),
                source_binding: proposal.source_binding.clone(),
                source_run_id: proposal.source_run_id.clone(),
                source_turn_id: proposal.source_turn_id.clone(),
                source_call_id: proposal.source_call_id.clone(),
                target_task_id: proposal.target_task_id.clone(),
                target_project_id: proposal.target_project_id.clone(),
                target_environment_id: proposal.target_environment_id.clone(),
                target_snapshot: proposal.target_snapshot.clone(),
            }
        }
    }

    fn managed_archive_fixture() -> (
        tempfile::TempDir,
        crate::EngineCore,
        String,
        tokio::sync::mpsc::UnboundedReceiver<luna_proto::AgentEvent>,
    ) {
        let (root, core, task_id) = managed_codex_wait_fixture();
        let (engine_tx, events) = tokio::sync::mpsc::unbounded_channel();
        lock(&core.sessions.inner.runs)
            .get_mut(&task_id)
            .unwrap()
            .engine_tx = engine_tx;
        publish_managed_codex_binding(&root, &core, &task_id);
        assert!(core.sessions.inner.observe_codex_checkpoint(
            &task_id,
            "run-a",
            Some("provider-thread-a"),
            "turn-a",
        ));
        (root, core, task_id, events)
    }

    fn fake_archive_proposal(
        core: &crate::EngineCore,
        source_task_id: &str,
        request_id: &str,
    ) -> LunaSyntheticArchiveProposal {
        let source_call_id = format!("call-{request_id}");
        assert!(core.sessions.claim_luna_provider_call(
            source_task_id,
            "run-a",
            "provider-thread-a",
            "turn-a",
            &source_call_id,
        ));
        let source_binding = core
            .sessions
            .inner
            .current_codex_binding_key(source_task_id)
            .unwrap();
        build_fake_archive_proposal(
            request_id,
            source_task_id,
            &core.device_id,
            source_binding,
            source_call_id,
        )
    }

    fn build_fake_archive_proposal(
        request_id: &str,
        source_task_id: &str,
        source_environment_id: &str,
        source_binding: ProviderBindingKey,
        source_call_id: String,
    ) -> LunaSyntheticArchiveProposal {
        let snapshot = luna_proto::TaskCommandSnapshot {
            task_id: "task-a".into(),
            task_revision: 7,
            task_sequence: 12,
            binding_id: Some("binding-a".into()),
            binding_generation: Some(2),
            active_turn_id: None,
        };
        let request = luna_proto::CrossTaskControlRequest {
            action: luna_proto::CrossTaskControlAction::Archive,
            task_id: "task-a".into(),
            project_id: "project-a".into(),
            environment_id: source_environment_id.into(),
            command_id: format!("synthetic-{request_id}"),
            destination_project_id: None,
            expected_task_revision: Some(snapshot.task_revision),
            expected_binding_generation: snapshot.binding_generation,
            expected_turn_id: None,
        };
        LunaSyntheticArchiveProposal {
            request_id: request_id.into(),
            source_task_id: source_task_id.into(),
            source_project_id: "project-a".into(),
            source_environment_id: source_environment_id.into(),
            source_binding,
            source_run_id: "run-a".into(),
            source_turn_id: "turn-a".into(),
            source_call_id,
            target_task_id: "task-a".into(),
            target_project_id: "project-a".into(),
            target_environment_id: source_environment_id.into(),
            target_snapshot: snapshot,
            request_digest: LunaSyntheticArchiveProposal::request_digest(&request).unwrap(),
            request,
            expires_at_ms: crate::now_ms() + 30_000,
        }
    }

    fn inject_fake_archive_for_test(
        pending: &PendingInputs,
        proposal: LunaSyntheticArchiveProposal,
    ) -> (
        tokio::sync::oneshot::Receiver<Vec<luna_proto::UserInputAnswer>>,
        tokio::sync::oneshot::Receiver<LunaSyntheticArchiveGrant>,
    ) {
        let (resolver, answer) = tokio::sync::oneshot::channel();
        let (grant_tx, grant) = tokio::sync::oneshot::channel();
        let question = proposal.question();
        lock(pending).insert(
            proposal.request_id.clone(),
            PendingInput {
                kind: PendingInputKind::LunaCrossTask {
                    run_id: proposal.source_run_id.clone(),
                    proposal: Box::new(proposal),
                    grant_tx,
                },
                questions: vec![question],
                resolver,
            },
        );
        (answer, grant)
    }

    fn fake_archive_backend(proposal: &LunaSyntheticArchiveProposal) -> Arc<FakeArchiveBackend> {
        Arc::new(FakeArchiveBackend {
            target: luna_proto::CrossTaskSummary {
                task_id: "task-a".into(),
                project_id: Some("project-a".into()),
                environment_id: proposal.target_environment_id.clone(),
                title: "Synthetic target".into(),
                status: "idle".into(),
                archived: false,
                updated_at_ms: 1,
            },
            snapshot: proposal.target_snapshot.clone(),
            live: Mutex::new(FakeLiveIdentity::from(proposal)),
            calls: AtomicUsize::new(0),
            suppress_readback: AtomicBool::new(false),
            receipt: Mutex::new(None),
        })
    }

    async fn dispatch_fake_archive_once(
        sessions: &SessionsEngine,
        grant: LunaSyntheticArchiveGrant,
        attempted: luna_proto::CrossTaskControlRequest,
        backend: Arc<FakeArchiveBackend>,
        now_ms: i64,
    ) -> Result<luna_proto::CrossTaskControlReceipt, crate::cross_task_tools::CrossTaskError> {
        use crate::cross_task_tools::{
            CrossTaskAuthority, CrossTaskControlGrant, CrossTaskError, CrossTaskTools,
        };
        let proposal = grant.proposal;
        let live = lock(&backend.live).clone();
        let current_binding = sessions
            .inner
            .current_codex_binding_key(&proposal.source_task_id);
        let live_claim = lock(&sessions.inner.runs)
            .get(&proposal.source_task_id)
            .is_some_and(|run| proposal.matches_live_claim(run, current_binding.as_ref()));
        if proposal.expires_at_ms <= now_ms
            || !live_claim
            || attempted != proposal.request
            || LunaSyntheticArchiveProposal::request_digest(&attempted)
                != Some(proposal.request_digest)
            || live.source_task_id != proposal.source_task_id
            || live.source_project_id != proposal.source_project_id
            || live.source_environment_id != proposal.source_environment_id
            || live.source_binding != proposal.source_binding
            || live.source_run_id != proposal.source_run_id
            || live.source_turn_id != proposal.source_turn_id
            || live.source_call_id != proposal.source_call_id
            || live.target_task_id != proposal.target_task_id
            || live.target_project_id != proposal.target_project_id
            || live.target_environment_id != proposal.target_environment_id
            || live.target_snapshot != proposal.target_snapshot
        {
            return Err(CrossTaskError::TargetScope);
        }
        let authority = CrossTaskAuthority {
            caller_task_id: proposal.source_task_id.clone(),
            local_environment_id: proposal.source_environment_id.clone(),
            visible_project_ids: std::collections::HashSet::from([proposal
                .source_project_id
                .clone()]),
            allow_projectless: false,
            granted_scopes: std::collections::HashSet::from(["threads.manage".into()]),
            explicit_user_request: true,
            delivery: luna_proto::RuntimeDelivery::CodexDynamicTools,
        };
        let exact_grant = CrossTaskControlGrant {
            caller_task_id: proposal.source_task_id,
            request: proposal.request.clone(),
            expires_at_ms: proposal.expires_at_ms,
        };
        let receipt = CrossTaskTools::new(backend.clone())
            .control(
                &authority,
                &exact_grant,
                attempted,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await?;
        if receipt.command.snapshot != proposal.target_snapshot
            || backend.readback(&proposal.request.command_id) != Some(receipt.clone())
        {
            return Err(CrossTaskError::InvalidBackendResult);
        }
        Ok(receipt)
    }

    #[tokio::test]
    async fn synthetic_archive_allow_once_dispatches_exactly_once_with_receipt_readback() {
        let (_root, core, task_id, _events) = managed_archive_fixture();
        let sessions = core.sessions.clone();
        let pending = lock(&sessions.inner.runs)
            .get(&task_id)
            .unwrap()
            .pending_inputs
            .clone();
        let proposal = fake_archive_proposal(&core, &task_id, "approved");
        let attempted = proposal.request.clone();
        let backend = fake_archive_backend(&proposal);
        let (answer, grant) = inject_fake_archive_for_test(&pending, proposal);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let calls = (0..2)
            .map(|_| {
                let sessions = sessions.clone();
                let task_id = task_id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    sessions.respond_luna_local_ui_approval(
                        &task_id,
                        "approved",
                        luna_proto::ApprovalDecision::AllowOnce,
                    )
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let outcomes = calls
            .into_iter()
            .map(|call| call.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == LunaApprovalOutcome::AllowedOnce)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == LunaApprovalOutcome::Stale)
                .count(),
            1
        );
        assert_eq!(answer.await.unwrap()[0].labels, vec!["Allow once"]);
        let receipt = dispatch_fake_archive_once(
            &sessions,
            grant.await.unwrap(),
            attempted,
            backend.clone(),
            crate::now_ms(),
        )
        .await
        .unwrap();
        assert_eq!(
            receipt.command.status,
            luna_proto::CommandReceiptStatus::Applied
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(backend.readback(&receipt.command.command_id), Some(receipt));
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "approved",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
    }

    #[tokio::test]
    async fn synthetic_archive_uncertain_receipt_is_never_replayed() {
        let (_root, core, task_id, _events) = managed_archive_fixture();
        let sessions = core.sessions.clone();
        let pending = lock(&sessions.inner.runs)
            .get(&task_id)
            .unwrap()
            .pending_inputs
            .clone();
        let proposal = fake_archive_proposal(&core, &task_id, "uncertain");
        let attempted = proposal.request.clone();
        let backend = fake_archive_backend(&proposal);
        backend.suppress_readback.store(true, Ordering::SeqCst);
        let (_answer, grant) = inject_fake_archive_for_test(&pending, proposal);
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "uncertain",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::AllowedOnce
        );
        assert!(matches!(
            dispatch_fake_archive_once(
                &sessions,
                grant.await.unwrap(),
                attempted,
                backend.clone(),
                crate::now_ms()
            )
            .await,
            Err(crate::cross_task_tools::CrossTaskError::InvalidBackendResult)
        ));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "uncertain",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn synthetic_archive_never_dispatches_changed_stale_or_remote_grants() {
        let (_root, core, task_id, _events) = managed_archive_fixture();
        let sessions = core.sessions.clone();
        let pending = lock(&sessions.inner.runs)
            .get(&task_id)
            .unwrap()
            .pending_inputs
            .clone();
        for case in 0..20 {
            let request_id = format!("changed-{case}");
            let proposal = fake_archive_proposal(&core, &task_id, &request_id);
            let mut attempted = proposal.request.clone();
            let backend = fake_archive_backend(&proposal);
            let expires_at_ms = proposal.expires_at_ms;
            let (_answer, grant) = inject_fake_archive_for_test(&pending, proposal);
            assert_eq!(
                sessions.respond_luna_local_ui_approval(
                    &task_id,
                    &request_id,
                    luna_proto::ApprovalDecision::AllowOnce,
                ),
                LunaApprovalOutcome::AllowedOnce
            );
            let mut fake_now = crate::now_ms();
            match case {
                0 => attempted.action = luna_proto::CrossTaskControlAction::Unarchive,
                1 => attempted.command_id.push_str("-changed"),
                2 => attempted.project_id = "project-b".into(),
                3 => lock(&backend.live).source_task_id = "another-caller".into(),
                4 => lock(&backend.live).source_binding.generation = 9,
                5 => lock(&backend.live).source_run_id = "another-run".into(),
                6 => lock(&backend.live).source_turn_id = "another-turn".into(),
                7 => lock(&backend.live).source_call_id = "another-call".into(),
                8 => lock(&backend.live).target_task_id = "other-task".into(),
                9 => lock(&backend.live).target_project_id = "project-b".into(),
                10 => lock(&backend.live).target_environment_id = "remote".into(),
                11 => lock(&backend.live).target_snapshot.task_revision += 1,
                12 => lock(&backend.live).target_snapshot.binding_generation = Some(9),
                13 => fake_now = expires_at_ms + 1,
                14 => attempted.expected_task_revision = Some(8),
                15 => attempted.expected_binding_generation = Some(9),
                16 => attempted.environment_id = "remote".into(),
                17 => attempted.task_id = "other-task".into(),
                18 => lock(&backend.live).source_binding.binding_id = "other-binding".into(),
                19 => lock(&backend.live).source_binding.provider_thread_id = "other-thread".into(),
                _ => unreachable!(),
            }
            assert!(
                dispatch_fake_archive_once(
                    &sessions,
                    grant.await.unwrap(),
                    attempted,
                    backend.clone(),
                    fake_now
                )
                .await
                .is_err(),
                "case {case} must reject before the fake backend"
            );
            assert_eq!(backend.calls.load(Ordering::SeqCst), 0, "case {case}");
            assert!(
                backend
                    .readback(&format!("synthetic-{request_id}"))
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn synthetic_archive_denial_expiry_replacement_and_lost_resolver_mint_no_grant() {
        let (_root, core, task_id, _events) = managed_archive_fixture();
        let sessions = core.sessions.clone();
        let pending = lock(&sessions.inner.runs)
            .get(&task_id)
            .unwrap()
            .pending_inputs
            .clone();
        for (case, decision) in [
            ("deny", luna_proto::ApprovalDecision::Deny),
            ("cancel", luna_proto::ApprovalDecision::Cancel),
            ("allow-task", luna_proto::ApprovalDecision::AllowTask),
        ] {
            let proposal = fake_archive_proposal(&core, &task_id, case);
            let backend = fake_archive_backend(&proposal);
            let (_answer, grant) = inject_fake_archive_for_test(&pending, proposal);
            assert_eq!(
                sessions.respond_luna_local_ui_approval(&task_id, case, decision),
                LunaApprovalOutcome::Denied
            );
            assert!(grant.await.is_err());
            assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        }

        let mut expired = fake_archive_proposal(&core, &task_id, "expired-fake");
        expired.expires_at_ms = crate::now_ms() - 1;
        let backend = fake_archive_backend(&expired);
        let (_answer, grant) = inject_fake_archive_for_test(&pending, expired);
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "expired-fake",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Expired
        );
        assert!(grant.await.is_err());
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);

        let proposal = fake_archive_proposal(&core, &task_id, "replaced-fake");
        let backend = fake_archive_backend(&proposal);
        let (_answer, mut grant) = inject_fake_archive_for_test(&pending, proposal);
        lock(&sessions.inner.runs).get_mut(&task_id).unwrap().run_id = "replacement-run".into();
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "replaced-fake",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
        assert!(matches!(
            grant.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);

        let (_root, core, task_id, _events) = managed_archive_fixture();
        let sessions = core.sessions.clone();
        let pending = lock(&sessions.inner.runs)
            .get(&task_id)
            .unwrap()
            .pending_inputs
            .clone();
        let proposal = fake_archive_proposal(&core, &task_id, "lost-fake");
        let backend = fake_archive_backend(&proposal);
        let (answer, grant) = inject_fake_archive_for_test(&pending, proposal);
        drop(answer);
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "lost-fake",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::RunEnded
        );
        assert!(grant.await.is_err());
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);

        for (project, environment) in [
            ("project-b", core.device_id.as_str()),
            ("project-a", "remote"),
        ] {
            let request_id = format!("scope-{project}-{environment}");
            let mut proposal = fake_archive_proposal(&core, &task_id, &request_id);
            proposal.target_project_id = project.into();
            proposal.target_environment_id = environment.into();
            let backend = fake_archive_backend(&proposal);
            let (_answer, mut grant) = inject_fake_archive_for_test(&pending, proposal);
            assert_eq!(
                sessions.respond_luna_local_ui_approval(
                    &task_id,
                    &request_id,
                    luna_proto::ApprovalDecision::AllowOnce,
                ),
                LunaApprovalOutcome::Stale
            );
            assert!(matches!(
                grant.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn synthetic_archive_live_claim_change_before_ui_response_mints_no_grant() {
        for case in [
            "same-thread-rebind",
            "new-thread-rebind",
            "turn-advance",
            "done",
        ] {
            let (_root, core, task_id, _events) = managed_archive_fixture();
            let sessions = core.sessions.clone();
            let pending = lock(&sessions.inner.runs)
                .get(&task_id)
                .unwrap()
                .pending_inputs
                .clone();
            let proposal = fake_archive_proposal(&core, &task_id, case);
            let backend = fake_archive_backend(&proposal);
            let (_answer, mut grant) = inject_fake_archive_for_test(&pending, proposal);
            match case {
                "same-thread-rebind" | "new-thread-rebind" => {
                    let thread = if case == "same-thread-rebind" {
                        "provider-thread-a"
                    } else {
                        "provider-thread-b"
                    };
                    core.task_identity
                        .advance_binding(
                            &task_id,
                            HarnessId::Codex,
                            thread,
                            luna_proto::BindingOrigin::ContextReset,
                            "synthetic pre-approval rebind",
                        )
                        .unwrap();
                }
                "turn-advance" => {
                    assert!(sessions.inner.observe_codex_checkpoint(
                        &task_id,
                        "run-a",
                        Some("provider-thread-a"),
                        "turn-b",
                    ));
                }
                "done" => sessions.inner.clear_codex_checkpoint(&task_id, "run-a"),
                _ => unreachable!(),
            }
            assert_eq!(
                sessions.respond_luna_local_ui_approval(
                    &task_id,
                    case,
                    luna_proto::ApprovalDecision::AllowOnce,
                ),
                LunaApprovalOutcome::Stale,
                "{case}"
            );
            assert!(matches!(
                grant.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn synthetic_archive_grant_is_unusable_after_binding_turn_or_run_changes() {
        for case in ["rebind", "turn-advance", "run-removal"] {
            let (_root, core, task_id, _events) = managed_archive_fixture();
            let sessions = core.sessions.clone();
            let pending = lock(&sessions.inner.runs)
                .get(&task_id)
                .unwrap()
                .pending_inputs
                .clone();
            let proposal = fake_archive_proposal(&core, &task_id, case);
            let request = proposal.request.clone();
            let backend = fake_archive_backend(&proposal);
            let (_answer, grant) = inject_fake_archive_for_test(&pending, proposal);
            assert_eq!(
                sessions.respond_luna_local_ui_approval(
                    &task_id,
                    case,
                    luna_proto::ApprovalDecision::AllowOnce,
                ),
                LunaApprovalOutcome::AllowedOnce
            );
            let grant = grant.await.unwrap();
            match case {
                "rebind" => {
                    core.task_identity
                        .advance_binding(
                            &task_id,
                            HarnessId::Codex,
                            "provider-thread-a",
                            luna_proto::BindingOrigin::ContextReset,
                            "synthetic post-approval rebind",
                        )
                        .unwrap();
                }
                "turn-advance" => {
                    assert!(sessions.inner.observe_codex_checkpoint(
                        &task_id,
                        "run-a",
                        Some("provider-thread-a"),
                        "turn-b",
                    ));
                }
                "run-removal" => {
                    lock(&sessions.inner.runs).remove(&task_id);
                }
                _ => unreachable!(),
            }
            assert!(
                dispatch_fake_archive_once(
                    &sessions,
                    grant,
                    request,
                    backend.clone(),
                    crate::now_ms()
                )
                .await
                .is_err(),
                "{case}"
            );
            assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn synthetic_archive_rejects_unmatched_question_expiry_and_channel_failure() {
        let (_root, core, task_id, events) = managed_archive_fixture();
        let sessions = core.sessions.clone();
        let pending = lock(&sessions.inner.runs)
            .get(&task_id)
            .unwrap()
            .pending_inputs
            .clone();
        let proposal = fake_archive_proposal(&core, &task_id, "closed-event-channel");
        let (answer, grant) = inject_fake_archive_for_test(&pending, proposal);
        drop(events);
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "closed-event-channel",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::RunEnded
        );
        assert!(answer.await.unwrap().is_empty());
        assert!(grant.await.is_err());

        let (_root, core, task_id, _events) = managed_archive_fixture();
        let sessions = core.sessions.clone();
        let pending = lock(&sessions.inner.runs)
            .get(&task_id)
            .unwrap()
            .pending_inputs
            .clone();
        let proposal = fake_archive_proposal(&core, &task_id, "closed-grant-channel");
        let (answer, grant) = inject_fake_archive_for_test(&pending, proposal);
        drop(grant);
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "closed-grant-channel",
                luna_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::RunEnded
        );
        assert!(answer.await.unwrap().is_empty());

        let (_root, core, task_id, _events) = managed_archive_fixture();
        let sessions = core.sessions.clone();
        let pending = lock(&sessions.inner.runs)
            .get(&task_id)
            .unwrap()
            .pending_inputs
            .clone();
        for case in [
            "call-marker",
            "turn-marker",
            "question-body",
            "multi-select",
            "future-expiry",
            "request-digest",
            "target-snapshot",
        ] {
            let mut proposal = fake_archive_proposal(&core, &task_id, case);
            match case {
                "future-expiry" => proposal.expires_at_ms = crate::now_ms() + 61_000,
                "request-digest" => proposal.request.command_id.push_str("-changed"),
                "target-snapshot" => proposal.target_snapshot.task_revision += 1,
                _ => {}
            }
            let (_answer, mut grant) = inject_fake_archive_for_test(&pending, proposal);
            if matches!(
                case,
                "call-marker" | "turn-marker" | "question-body" | "multi-select"
            ) {
                let mut pending = lock(&pending);
                let question = &mut pending.get_mut(case).unwrap().questions[0];
                match case {
                    "call-marker" => {
                        question.approval.as_mut().unwrap().provider_request_id =
                            "forged-call".into();
                    }
                    "turn-marker" => {
                        question.approval.as_mut().unwrap().turn_id = Some("forged-turn".into());
                    }
                    "question-body" => question.question = "Misleading action".into(),
                    "multi-select" => question.multi_select = true,
                    _ => unreachable!(),
                }
            }
            assert_eq!(
                sessions.respond_luna_local_ui_approval(
                    &task_id,
                    case,
                    luna_proto::ApprovalDecision::AllowOnce,
                ),
                LunaApprovalOutcome::Stale,
                "{case}"
            );
            assert!(matches!(
                grant.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
        }
    }

    #[tokio::test]
    async fn generated_image_failure_is_sanitized_even_inside_subagents() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let sessions = SessionsEngine::new(
            "host".into(),
            Arc::new(RunJournal::open(dir.path().join("journals")).unwrap()),
            Arc::new(HarnessRegistry::new()),
            Arc::new(DocsStore::open(dir.path().join("store")).unwrap()),
        );
        let source = "/outside/private/source.png";
        let event = AgentEvent::Subagent {
            parent_tool_use_id: "child".into(),
            event: Box::new(AgentEvent::GeneratedImage {
                id: "i:image".into(),
                path: source.into(),
                name: "secret".into(),
                mime_type: "bad".into(),
            }),
        };
        let events = sessions
            .inner
            .prepare_generated_image("chat", event, &mut std::collections::HashSet::new())
            .await;
        assert_eq!(events.len(), 2);
        let mut parts = vec![];
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "i".into(),
                call: luna_proto::ToolCall::Unknown {
                    name: "Generate image".into(),
                    input: None,
                },
            },
        );
        for event in &events {
            sessions.inner.publish("chat", event, None).unwrap();
            let AgentEvent::Subagent {
                parent_tool_use_id,
                event,
            } = event
            else {
                panic!("lost owner")
            };
            assert_eq!(parent_tool_use_id, "child");
            fold_event_into_parts(&mut parts, event);
        }
        assert!(matches!(
            parts[0],
            MessagePart::Tool {
                is_error: true,
                resolved: true,
                ..
            }
        ));
        assert!(matches!(parts[1], MessagePart::Error { .. }));
        let journal =
            serde_json::to_string(&sessions.inner.journal.replay("chat", 0).unwrap()).unwrap();
        assert!(!journal.contains(source));
        assert!(!journal.contains("secret"));
    }

    fn request() -> RunRequest {
        RunRequest {
            review: None,
            goal: None,
            input_items: Vec::new(),
            prompt: "first".into(),
            harness: None,
            provider_account_id: None,
            model: Some("grok-4.6".into()),
            reasoning: Some(luna_proto::ReasoningLevel::High),
            model_options: serde_json::Map::new(),
            cwd: "/tmp".into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            permission_mode: luna_proto::PermissionMode::Ask,
            parent_permission_ceiling: None,
            environment_permission_ceiling: None,
            permission_policy: None,
            resume: None,
            attachments: Vec::new(),
            worktree: None,
        }
    }

    #[test]
    fn stale_attempt_results_cannot_mutate_a_newer_chain_or_attempt() {
        use super::{RecoveryAttemptContext, RecoveryRecord};

        let root = tempfile::tempdir().unwrap();
        let sessions = SessionsEngine::new(
            "device-test".into(),
            Arc::new(crate::run_journal::RunJournal::open(root.path()).unwrap()),
            Arc::new(crate::registry::HarnessRegistry::new()),
            Arc::new(DocsStore::open(root.path().join("store")).unwrap()),
        );
        let mut frozen = request();
        frozen.prompt = "newer frozen request".into();
        frozen.attachments = vec!["/tmp/newer.png".into()];
        let current = RecoveryRecord {
            workspace_repair_revision: 0,
            view: TaskRecovery {
                chain_id: "new-chain".into(),
                task_id: "chat".into(),
                original_message_id: "new-message".into(),
                original_command_id: Some("new-command".into()),
                original_acceptance: RecoveryAcceptance::RejectedBeforeAcceptance,
                acceptance: RecoveryAcceptance::RejectedBeforeAcceptance,
                failure: RecoveryFailure::Overloaded,
                action: RecoveryAction::ResendOriginal,
                status: RecoveryStatus::Running,
                attempt_count: 2,
                max_attempts: 4,
                automatic: false,
                retry_at_ms: None,
                automatic_deadline_ms: i64::MAX,
                environment_id: "device-test".into(),
                binding_id: None,
                binding_generation: None,
                detail: "current".into(),
                created_at_ms: 1,
                updated_at_ms: 2,
            },
            harness: HarnessId::Mock,
            request: frozen.clone(),
            original_acceptance: RecoveryAcceptance::RejectedBeforeAcceptance,
            attempt_acceptance: None,
            attempt_started_at_ms: Some(2),
            attempt_automatic: false,
            provider_session_id: None,
        };
        sessions.inner.recovery.put(current).unwrap();

        for stale in [
            RecoveryAttemptContext {
                chain_id: "old-chain".into(),
                attempt_count: 1,
                automatic: false,
            },
            RecoveryAttemptContext {
                chain_id: "new-chain".into(),
                attempt_count: 1,
                automatic: false,
            },
        ] {
            sessions
                .record_recovery_failure(
                    "chat",
                    HarnessId::Mock,
                    request(),
                    "old-message".into(),
                    RecoveryAcceptance::Accepted,
                    RecoveryFailure::Interrupted,
                    "stale result".into(),
                    Some("stale-session".into()),
                    Some(&stale),
                )
                .unwrap();
            sessions
                .mark_recovery_accepted("chat", Some(&stale), "stale-session")
                .unwrap();
            sessions.complete_recovery("chat", Some(&stale)).unwrap();
        }

        let stored = sessions.inner.recovery.latest("chat").unwrap().unwrap();
        assert_eq!(stored.view.chain_id, "new-chain");
        assert_eq!(stored.view.attempt_count, 2);
        assert_eq!(stored.view.status, RecoveryStatus::Running);
        assert_eq!(
            stored.view.acceptance,
            RecoveryAcceptance::RejectedBeforeAcceptance
        );
        assert_eq!(stored.request.prompt, "newer frozen request");
        assert_eq!(stored.request.attachments, vec!["/tmp/newer.png"]);
        assert!(stored.attempt_acceptance.is_none());
        assert!(stored.provider_session_id.is_none());
    }

    #[test]
    fn live_routing_requires_the_same_runtime_configuration() {
        let initial = request();
        let config = RuntimeConfig::from_request(HarnessId::Grok, &initial);

        let mut follow_up = initial.clone();
        follow_up.prompt = "second".into();
        follow_up.resume = Some("session-1".into());
        assert!(config.can_route(HarnessId::Grok, &follow_up));

        follow_up.model = Some("grok-4.5".into());
        assert!(!config.can_route(HarnessId::Grok, &follow_up));
        follow_up.model = initial.model.clone();

        follow_up.reasoning = Some(luna_proto::ReasoningLevel::Medium);
        assert!(!config.can_route(HarnessId::Grok, &follow_up));
        follow_up.reasoning = initial.reasoning;

        follow_up.provider_account_id = Some("codex-account-b".into());
        assert!(
            !config.can_route(HarnessId::Grok, &follow_up),
            "an auth-instance change must retire the warm runtime"
        );
        follow_up.provider_account_id = initial.provider_account_id.clone();

        follow_up.model_options.insert(
            luna_harness::LUNA_PROVIDER_AUTH_INSTANCE_OPTION.into(),
            "rotated-native-identity".into(),
        );
        assert!(
            !config.can_route(HarnessId::Grok, &follow_up),
            "an externally replaced native login must invalidate the warm runtime"
        );
        follow_up.model_options = initial.model_options.clone();

        follow_up.attachments.push("/tmp/image.png".into());
        assert!(!config.can_route(HarnessId::Grok, &follow_up));
    }

    #[tokio::test]
    async fn provider_account_change_rejects_an_active_turn_without_interrupting_it() {
        let root = tempfile::tempdir().unwrap();
        let sessions = SessionsEngine::new(
            "device-test".into(),
            Arc::new(crate::run_journal::RunJournal::open(root.path()).unwrap()),
            Arc::new(crate::registry::HarnessRegistry::new()),
            Arc::new(DocsStore::open(root.path().join("store")).unwrap()),
        );
        let mut request = request();
        request.provider_account_id = Some("codex-native".into());
        let (steer_tx, _steer_rx) = tokio::sync::mpsc::channel(1);
        let (cancel, _cancel_rx) = tokio::sync::watch::channel(false);
        let (engine_tx, _engine_rx) = tokio::sync::mpsc::unbounded_channel();
        lock(&sessions.inner.runs).insert(
            "chat-active".into(),
            RunHandle {
                workspace_repair_revision: 0,
                idle_stream_closed: Arc::new(AtomicBool::new(false)),
                run_id: "run-active".into(),
                provider_turn: None,
                run_provider_binding: None,
                provider_turn_readiness: tokio::sync::watch::channel(
                    ProviderTurnReadiness::AwaitingBinding,
                )
                .0,
                steerable: true,
                runtime_config: RuntimeConfig::from_request(HarnessId::Codex, &request),
                steer_tx,
                interrupt_token: luna_harness::CancellationToken::new(),
                cancel,
                hold_recovery_on_interrupt: Arc::new(AtomicBool::new(true)),
                engine_tx,
                pending_inputs: Arc::new(Mutex::new(std::collections::HashMap::new())),
                routed_steers: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            },
        );
        let now = chrono::Utc::now();
        lock(&sessions.inner.statuses).insert(
            "chat-active".into(),
            Session {
                last_completed_turn: None,
                chat_id: "chat-active".into(),
                device_id: "device-test".into(),
                status: SessionStatus::Working,
                started_at: Some(now),
                updated_at: now,
                recovery: None,
            },
        );

        let error = sessions
            .prepare_provider_account_change(HarnessId::Codex)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Stop it"));
        assert!(lock(&sessions.inner.runs).contains_key("chat-active"));
    }

    #[test]
    fn clean_tool_ids_ride_verbatim_and_fit_id_re() {
        let id = subagent_doc_id("chat-abc123", "toolu_01EjFLnNhCiMR2PBNKcVvkT");
        assert_eq!(id, "chat-abc123--sub--toolu_01EjFLnNhCiMR2PBNKcVvkT");
        assert!(id.len() <= 128);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
    }

    #[test]
    fn unclean_or_oversized_ids_hash_deterministically() {
        let dirty = subagent_doc_id("chat", "call/with:odd chars");
        assert!(
            dirty
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "{dirty}"
        );
        assert_eq!(dirty, subagent_doc_id("chat", "call/with:odd chars"));
        let long_chat = "c".repeat(100);
        let long = subagent_doc_id(&long_chat, &"t".repeat(64));
        assert!(long.len() <= 128, "{}", long.len());
        // Distinct inputs stay distinct.
        assert_ne!(
            subagent_doc_id("chat", "a:b"),
            subagent_doc_id("chat", "a:c")
        );
    }
}
