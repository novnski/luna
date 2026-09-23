//! SessionsEngine — per-chat agent runs: dispatch, steering, interrupts, input bridging,
//! journal + broadcast fan-out, and 120ms coalesced doc streaming.
//!
//! Pragmatic port of zeron's `sessions.ts` (spec: feature-inventory §3.2):
//! - every `AgentEvent` is (a) appended to the on-disk run journal, (b) broadcast to
//!   in-process subscribers, (c) folded via `fold_event_into_parts` and diffed into the
//!   chat's `SessionDoc` through `SegmentWriter` on a coalesced `STREAM_COMMIT_MS` timer;
//! - the user message entry is pushed to the doc immediately on dispatch (id = the
//!   command's client-minted message id, so optimistic echoes never flicker);
//! - a `Steered` event splits the assistant entry at the exact boundary;
//! - recovery (interrupt or a stale journal at boot) stamps the streaming entry `aborted`.
//!
//! Scope notes: sessions are keyed by chat id (one live run per chat). Zeron's pulse
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

use zeron_doc::{
    DocError, MessagePart, MessageRole, MessageStatus, STREAM_COMMIT_MS, SegmentWriter,
    SessionCommandPayload, SessionCommandStatus, SessionDoc, SessionMessageEntry,
    fold_event_into_parts, sanitize_tool_call,
};
use zeron_harness::{
    CancellationToken, Harness, HarnessError, RunControls, SteerAcceptance, SteerMessage,
};
use zeron_proto::{
    AgentEvent, ChatConfig, CommandReceiptStatus, CompactMechanism, CompactOperation,
    ContextControlState, ContextOperationStatus, DoneStatus, HarnessId, PermissionAuditEntry,
    PermissionMode, PermissionPolicy, RecoveryAcceptance, RecoveryFailure, RecoveryStatus,
    RunRequest, Session, SessionStatus, TaskRecovery, UserInputAnswer, UserInputQuestion,
};

use crate::device_write::DeviceWritePermit;
use zeron_sync::DocsStore;

use crate::doc_host::{ChatDocHandle, DocHost};
use crate::recovery::{
    MAX_AUTO_RETRIES, MAX_AUTO_WAIT_MS, RecoveryChange, RecoveryRecord, RecoveryStore,
    classify_failure, recovery_decision,
};
use crate::registry::{HarnessDescriptor, HarnessRegistry};
use crate::run_journal::RunJournal;
use crate::{EngineError, new_id, now_ms};

/// One journaled event: the durable seq plus the event, as broadcast to subscribers.
#[derive(Debug, Clone)]
pub struct JournaledEvent {
    pub seq: u64,
    pub event: AgentEvent,
}

/// Outcome of a steer attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerOutcome {
    /// The provider accepted a native in-turn steering request.
    Accepted,
    /// No live steerable run.
    NotSteerable,
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
    target_snapshot: zeron_proto::TaskCommandSnapshot,
    request: zeron_proto::CrossTaskControlRequest,
    request_digest: [u8; 32],
    expires_at_ms: i64,
}

impl LunaSyntheticArchiveProposal {
    fn request_digest(request: &zeron_proto::CrossTaskControlRequest) -> Option<[u8; 32]> {
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
            approval: Some(zeron_proto::ApprovalRequest {
                provider_request_id: self.source_call_id.clone(),
                provider: HarnessId::Codex,
                turn_id: Some(self.source_turn_id.clone()),
                machine_name: "synthetic".into(),
                action_kind: zeron_proto::ApprovalActionKind::LunaCrossTask,
                summary: "Synthetic archive".into(),
                requested_scope: zeron_proto::PermissionScope::Workspace,
                expires_at_ms: self.expires_at_ms,
                supports_task_allowance: false,
            }),
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
            && self.request.action == zeron_proto::CrossTaskControlAction::Archive
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
pub enum LunaApprovalOutcome {
    AllowedOnce,
    Denied,
    Expired,
    Stale,
    RunEnded,
}

type PendingInputs = Arc<Mutex<HashMap<String, PendingInput>>>;

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
    tokio::spawn(async move {
        let delay_ms = expires_at_ms.saturating_sub(now_ms()).max(0) as u64;
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
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
        PendingInputKind::LunaCrossTask { .. } | PendingInputKind::LunaProspectiveInterrupt { .. }
    ) || pending
        .questions
        .iter()
        .any(|question| question.approval.is_some())
    {
        // Typed approvals cannot leave through ordinary input. Provider
        // approvals use `respond_approval`; LUNA approvals need the future
        // trusted local UI route.
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
/// directory — zeron sessions.ts:563 "harness session stores are keyed by
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
    reasoning: Option<zeron_proto::ReasoningLevel>,
    model_options: serde_json::Map<String, serde_json::Value>,
    cwd: String,
    sandbox: zeron_proto::SandboxLevel,
    auto_approve: bool,
    permission_policy: Option<zeron_proto::PermissionPolicy>,
    worktree: Option<zeron_proto::WorktreeSpec>,
}

impl RuntimeConfig {
    fn from_request(harness_id: HarnessId, request: &RunRequest) -> Self {
        Self {
            harness_id,
            provider_account_id: request.provider_account_id.clone(),
            model: request.model.clone(),
            reasoning: request.reasoning,
            model_options: request.model_options.clone(),
            cwd: request.cwd.clone(),
            sandbox: request.sandbox,
            auto_approve: request.auto_approve,
            permission_policy: request.permission_policy.clone(),
            worktree: request.worktree.clone(),
        }
    }

    fn can_route(&self, harness_id: HarnessId, request: &RunRequest) -> bool {
        request.attachments.is_empty() && self == &Self::from_request(harness_id, request)
    }
}

struct RunHandle {
    run_id: String,
    /// In-memory provider turn evidence for a future per-request LUNA grant.
    /// This is separate from `active_turn_id()`, which intentionally returns
    /// the engine run ID for the existing command fence.
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
    target_snapshot: zeron_proto::TaskCommandSnapshot,
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
            delivery: zeron_proto::RuntimeDelivery::CodexDynamicTools,
        }
    }
}

/// One exact, read-only proposal. Keeping the full snapshot and source claim
/// private prevents a serialized question from becoming control authority.
#[derive(Debug)]
#[allow(dead_code)]
struct LunaProspectiveInterruptProposal {
    request_id: String,
    action: zeron_proto::CrossTaskControlAction,
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
            approval: Some(zeron_proto::ApprovalRequest {
                provider_request_id: evidence.claim.provider_call_id.clone(),
                provider: HarnessId::Codex,
                turn_id: Some(evidence.claim.provider_turn_id.clone()),
                machine_name: evidence.source_environment_id.clone(),
                action_kind: zeron_proto::ApprovalActionKind::LunaCrossTask,
                summary: format!(
                    "Prospective Interrupt of the active turn in task {} (preview only)",
                    evidence.target_task_id
                ),
                requested_scope: zeron_proto::PermissionScope::Workspace,
                expires_at_ms: self.expires_at_ms,
                supports_task_allowance: false,
            }),
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
        self.action == zeron_proto::CrossTaskControlAction::Interrupt
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
/// re-dispatch it (ordinary warm route) or must preserve it as unknown
/// (explicit native steer).
#[derive(Debug, Clone)]
struct RoutedSteer {
    prompt: String,
    message_id: String,
    delivery: zeron_harness::SteerDelivery,
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
    /// Harness-native session ids per chat (resume continuity across turns) —
    /// the live-process cache over the durable copy on the workspace chat row
    /// (zeron kept the same pair on `chats.harness_session_id`). An empty
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
}

/// Turn-start hook: called with `(chat_id, cwd)`.
pub type TurnListener = Arc<dyn Fn(&str, &str) + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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
                recovery_settings_path: Mutex::new(None),
                last_requests: Mutex::new(HashMap::new()),
                harness_sessions: Mutex::new(HashMap::new()),
                titles: OnceLock::new(),
                task_identity: OnceLock::new(),
                cross_task_backends: Mutex::new(Vec::new()),
                agent_accounts: OnceLock::new(),
                integration_settings: OnceLock::new(),
                account_transition: Arc::new(AsyncMutex::new(())),
                generated_images: OnceLock::new(),
                turn_listener: OnceLock::new(),
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
        context: &zeron_proto::RuntimeContext,
        delivery: zeron_proto::RuntimeDelivery,
    ) -> (
        Vec<zeron_proto::RuntimeToolDefinition>,
        Vec<zeron_proto::OmittedRuntimeTool>,
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

    /// The execution-lane fence for controls such as stop and steer. This is
    /// intentionally the engine run id: a replacement provider runtime always
    /// receives a new value even when it resumes the same native thread.
    pub fn active_turn_id(&self, chat_id: &str) -> Option<String> {
        lock(&self.inner.runs)
            .get(chat_id)
            .map(|run| run.run_id.clone())
    }

    /// Await this run's engine-observed binding and checkpoint, then reserve
    /// one adapter-validated call for a future LUNA approval proposal. This
    /// context is evidence only, never a user grant or a tool dispatch.
    #[allow(dead_code)]
    async fn wait_and_claim_luna_provider_invocation(
        &self,
        chat_id: &str,
        run_id: &str,
        context: &zeron_harness::LunaToolInvocationContext,
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

    /// Dormant read-only preparation for a future, separately authorized
    /// control flow. This cannot register input, mint consent or grant, choose
    /// an action, generate a command ID, or submit a command.
    #[allow(dead_code)]
    async fn prepare_luna_control_evidence(
        &self,
        claim: ClaimedProviderCall,
        authority: &crate::cross_task_tools::CrossTaskAuthority,
        target_task_id: &str,
    ) -> Result<PreparedControlEvidence, crate::cross_task_tools::CrossTaskError> {
        use crate::cross_task_tools::{CrossTaskError, CrossTaskTools};
        if authority.caller_task_id != claim.source_task_id
            || authority.local_environment_id != self.inner.device_id
            || authority.delivery != zeron_proto::RuntimeDelivery::CodexDynamicTools
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
            action: zeron_proto::CrossTaskControlAction::Interrupt,
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
        decision: zeron_proto::ApprovalDecision,
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
                labels: vec![if decision == zeron_proto::ApprovalDecision::Cancel {
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
        decision: zeron_proto::ApprovalDecision,
    ) -> LunaApprovalOutcome {
        use crate::cross_task_tools::{CrossTaskError, CrossTaskTools};
        if decision != zeron_proto::ApprovalDecision::AllowOnce {
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
    ) -> Result<Option<zeron_proto::RuntimeContractDiagnostics>, EngineError> {
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
    ) -> Result<zeron_proto::RuntimeContext, EngineError> {
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
        Ok(zeron_proto::RuntimeContext {
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
            .map(|(seq, event)| JournaledEvent { seq, event })
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
    ///   (zeron's persistent-session routing); otherwise any live run is interrupted
    ///   first — never two runtimes driving one chat.
    pub async fn dispatch(
        &self,
        chat_id: &str,
        harness_id: HarnessId,
        request: RunRequest,
        message_id: Option<String>,
    ) -> Result<String, EngineError> {
        self.dispatch_with_context(chat_id, harness_id, request, message_id, None)
            .await
    }

    fn dispatch_with_context<'a>(
        &'a self,
        chat_id: &'a str,
        harness_id: HarnessId,
        request: RunRequest,
        message_id: Option<String>,
        recovery: Option<RecoveryAttemptContext>,
    ) -> futures::future::BoxFuture<'a, Result<String, EngineError>> {
        Box::pin(self.dispatch_inner(chat_id, harness_id, request, message_id, recovery))
    }

    async fn dispatch_inner(
        &self,
        chat_id: &str,
        harness_id: HarnessId,
        mut request: RunRequest,
        mut message_id: Option<String>,
        recovery: Option<RecoveryAttemptContext>,
    ) -> Result<String, EngineError> {
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
        // Runtime contracts are host authority, never client-provided model
        // options. Strip even a syntactically valid forged contract before any
        // request snapshot, recovery record, or provider dispatch.
        request
            .model_options
            .remove(zeron_proto::LUNA_RUNTIME_CONTRACT_OPTION);
        let context_control = self.doc_handle(chat_id)?.doc().context_control();
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
        let frozen_request = request.clone();
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
                zeron_harness::LUNA_PROVIDER_AUTH_INSTANCE_OPTION.into(),
                execution.auth_instance_id.into(),
            );
            if let Some(home) = execution.codex_home {
                let home = home.to_str().ok_or_else(|| {
                    EngineError::Other("Codex account home is not valid UTF-8".into())
                })?;
                request.model_options.insert(
                    zeron_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
                    home.into(),
                );
            }
            if execution.codex_file_auth {
                request.model_options.insert(
                    zeron_harness::codex::LUNA_CODEX_FILE_AUTH_OPTION.into(),
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
        let permission_policy = crate::permissions::resolve_run_policy(harness_id, &mut request);
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
                zeron_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
                serde_json::to_value(&compiled.contract)?,
            );
        }
        let routed = lock(&self.inner.runs).get(chat_id).map(|h| {
            (
                h.run_id.clone(),
                h.steerable,
                h.runtime_config.can_route(harness_id, &request),
                h.steer_tx.clone(),
                h.routed_steers.clone(),
            )
        });
        if routed.as_ref().is_some_and(|(_, _, _, _, ledger)| {
            lock(ledger)
                .iter()
                .any(|steer| steer.delivery == zeron_harness::SteerDelivery::RequireActiveTurn)
        }) {
            return Err(EngineError::Other(
                "native steer incorporation is still unconfirmed; wait for the current turn boundary"
                    .into(),
            ));
        }
        // Every accepted dispatched prompt is a turn — routed steer or fresh
        // run alike. A single-flight rejection above is not a turn.
        self.note_turn_start(chat_id, &request.cwd);
        if let Some((run_id, steerable, same_runtime, steer_tx, ledger)) = routed {
            let user_id = message_id.clone().unwrap_or_else(new_id);
            let accepted = if steerable && same_runtime {
                // Warm dispatch uses the same mailbox as explicit steering.
                // Register acceptance before a fast boundary can retire it.
                let mut pending = lock(&ledger);
                let message = SteerMessage {
                    prompt: request.prompt.clone(),
                    message_id: Some(user_id.clone()),
                    delivery: zeron_harness::SteerDelivery::AllowFollowUpTurn,
                    acceptance: None,
                };
                if steer_tx.try_send(message).is_ok() {
                    pending.push_back(RoutedSteer {
                        prompt: request.prompt.clone(),
                        message_id: user_id.clone(),
                        delivery: zeron_harness::SteerDelivery::AllowFollowUpTurn,
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

        // Engine-owned resume (zeron sessions.ts:736 — every dispatch read the
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
                    zeron_harness::codex::LUNA_STRICT_RESUME_OPTION.into(),
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
            .get(zeron_proto::LUNA_RUNTIME_CONTRACT_OPTION)
            .and_then(|value| {
                serde_json::from_value::<zeron_proto::LunaAgentRuntimeContract>(value.clone()).ok()
            })
            && contract.delivery == zeron_proto::RuntimeDelivery::CodexDynamicTools
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
                zeron_harness::codex::LUNA_NATIVE_THREAD_NAME_OPTION.into(),
                format!("[LUNA] {title}").into(),
            );
        }
        lock(&self.inner.last_requests).insert(chat_id.to_string(), request.clone());

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
                let _ = engine_tx.send(AgentEvent::InputRequested {
                    request_id: request_id.clone(),
                    questions: questions.clone(),
                });
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
        let controls = RunControls {
            request_input,
            steering: steer_rx,
            interrupt: interrupt_token.clone(),
            invoke_luna_tool: request
                .model_options
                .get(zeron_proto::LUNA_RUNTIME_CONTRACT_OPTION)
                .and_then(|value| {
                    serde_json::from_value::<zeron_proto::LunaAgentRuntimeContract>(value.clone())
                        .ok()
                })
                .filter(|contract| !contract.tools.is_empty())
                .map(|contract| {
                    let cancel = interrupt_token.clone();
                    let backend = lock(&self.inner.cross_task_backends)
                        .iter()
                        .rev()
                        .find(|backend| backend.strong_count() > 0)
                        .cloned();
                    std::sync::Arc::new(move |invocation: zeron_harness::LunaToolInvocation| {
                        let contract = contract.clone();
                        let cancel = cancel.clone();
                        let backend = backend.as_ref().and_then(Weak::upgrade);
                        Box::pin(async move {
                            // The adapter validated this provider invocation against
                            // its active thread and turn. The current registry only
                            // dispatches read tools; a future mutating path must
                            // bind these IDs to a separate one-use user grant.
                            let _provider_context = invocation.context;
                            crate::runtime_contract::invoke_with_backend(
                                &contract,
                                &invocation.name,
                                invocation.arguments,
                                &cancel,
                                backend,
                            )
                            .await
                            .map_err(|error| {
                                zeron_harness::HarnessError::Protocol(error.to_string())
                            })
                        })
                            as futures::future::BoxFuture<
                                'static,
                                Result<serde_json::Value, zeron_harness::HarnessError>,
                            >
                    }) as zeron_harness::LunaToolExecutor
                }),
        };

        lock(&self.inner.runs).insert(
            chat_id.to_string(),
            RunHandle {
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
            .compact_context(zeron_harness::ContextCompactRequest {
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
        let target = lock(&self.inner.runs)
            .get(chat_id)
            .filter(|h| h.steerable)
            .map(|h| {
                (
                    h.run_id.clone(),
                    h.steer_tx.clone(),
                    h.routed_steers.clone(),
                )
            });
        let Some((run_id, steer_tx, ledger)) = target else {
            return Ok(SteerOutcome::NotSteerable);
        };
        let user_id = message_id.unwrap_or_else(new_id);
        let (acceptance_tx, acceptance_rx) = tokio::sync::oneshot::channel();
        let message = SteerMessage {
            prompt: prompt.to_string(),
            message_id: Some(user_id.clone()),
            delivery: zeron_harness::SteerDelivery::RequireActiveTurn,
            acceptance: Some(acceptance_tx),
        };
        {
            // Serialize mailbox acceptance with confirmation and Done-time
            // inspection: a fast consumer must never outrun its ledger entry.
            // Explicit native steering is intentionally single-flight. The
            // native Codex/Claude wires expose no correlation id on echoed
            // user content, so allowing a second pending row would let an
            // identity-free boundary retire the wrong durable receipt.
            let mut pending = lock(&ledger);
            if !pending.is_empty() {
                return Ok(SteerOutcome::NotSteerable);
            }
            if steer_tx.try_send(message).is_err() {
                return Ok(SteerOutcome::NotSteerable);
            }
            pending.push_back(RoutedSteer {
                prompt: prompt.to_string(),
                message_id: user_id.clone(),
                delivery: zeron_harness::SteerDelivery::RequireActiveTurn,
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
        // Unpark any blocked question FIRST (mirrors zeron: harness teardown can await a
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

    /// Resolve a pending `request_input` question set. Returns `false` when no such
    /// request is pending (unknown id, or the run already settled).
    pub fn respond_input(
        &self,
        chat_id: &str,
        request_id: &str,
        answers: Vec<UserInputAnswer>,
    ) -> Result<bool, EngineError> {
        let target = lock(&self.inner.runs)
            .get(chat_id)
            .map(|h| (h.pending_inputs.clone(), h.engine_tx.clone()));
        let Some((pending, engine_tx)) = target else {
            return Ok(false);
        };
        let Some(pending) = take_pending_ordinary_input(&pending, request_id) else {
            return Ok(false);
        };
        let _ = pending.resolver.send(answers);
        let _ = engine_tx.send(AgentEvent::InputResolved {
            request_id: request_id.to_string(),
        });
        Ok(true)
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
                )
            })
        })
    }

    /// The only response path for a server-owned LUNA approval. The caller
    /// must be an in-process UI handle; this method is not exposed by RPC.
    /// No production path creates such a pending request yet.
    pub fn respond_luna_local_ui_approval(
        &self,
        chat_id: &str,
        request_id: &str,
        decision: zeron_proto::ApprovalDecision,
    ) -> LunaApprovalOutcome {
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
                || decision == zeron_proto::ApprovalDecision::AllowOnce
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
            zeron_proto::ApprovalDecision::AllowOnce => {
                ("Allow once", LunaApprovalOutcome::AllowedOnce)
            }
            zeron_proto::ApprovalDecision::Cancel => ("Cancel", LunaApprovalOutcome::Denied),
            zeron_proto::ApprovalDecision::Deny | zeron_proto::ApprovalDecision::AllowTask => {
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

    pub fn respond_approval(
        &self,
        chat_id: &str,
        request_id: &str,
        decision: zeron_proto::ApprovalDecision,
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
            if !matches!(&pending.kind, PendingInputKind::Provider) {
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
        let _ = pending.resolver.send(vec![answer]);
        let _ = engine_tx.send(AgentEvent::InputResolved {
            request_id: request_id.to_string(),
        });
        Ok(true)
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
        let decision = recovery_decision(
            effective_acceptance,
            &failure,
            now,
            created_at_ms,
            attempt_count,
            self.automatic_retries_enabled(),
            safe_continuation,
            jitter_seed,
        );
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
            zeron_proto::RecoveryAction::ObserveOnly
                | zeron_proto::RecoveryAction::FixConfiguration
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
            zeron_proto::RecoveryAction::ContinueSession
                | zeron_proto::RecoveryAction::SignInThenContinue
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
            request.prompt = "Continue from the preserved provider session. Do not repeat completed tool or external effects; first inspect the current state and resume only unfinished work.".into();
            request.attachments.clear();
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
        let stale = self.inner.journal.stale_sessions()?;
        let mut recovered = 0usize;
        let mut reconciled_running = std::collections::HashSet::new();
        // Stop is authoritative even when the process died after the doc
        // command landed but before receipt/recovery finalization. Restore the
        // hold before any stale/terminal sweep can make the chain runnable.
        for record in self.inner.recovery.latest_records()? {
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
    pub async fn shutdown(&self) {
        for (_, timer) in lock(&self.inner.recovery_tasks).drain() {
            timer.abort();
        }
        let chats: Vec<String> = lock(&self.inner.runs).keys().cloned().collect();
        for chat_id in chats {
            if let Err(err) = self.interrupt(&chat_id).await {
                tracing::warn!(chat = %chat_id, error = %err, "shutdown interrupt failed");
            }
        }
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
    decision: zeron_proto::ApprovalDecision,
) -> Option<UserInputAnswer> {
    let approval = question.approval.as_ref()?;
    if approval.is_luna_cross_task() {
        return None;
    }
    let decision = match decision {
        _ if approval.expires_at_ms <= now_ms() => zeron_proto::ApprovalDecision::Deny,
        zeron_proto::ApprovalDecision::AllowTask if !approval.supports_task_allowance => {
            zeron_proto::ApprovalDecision::Deny
        }
        decision => decision,
    };
    let label = match decision {
        zeron_proto::ApprovalDecision::AllowOnce => "Allow once",
        zeron_proto::ApprovalDecision::AllowTask => "Allow for task",
        zeron_proto::ApprovalDecision::Deny => "Deny",
        zeron_proto::ApprovalDecision::Cancel => "Cancel",
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
            let result = if let Some((uploads, allowed_root)) = self.generated_images.get() {
                let uploads = uploads.clone();
                let root = allowed_root.clone();
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
                            id: id.strip_suffix(":image").unwrap_or(&id).into(),
                            is_error: true,
                            output: None,
                            diff: None,
                        },
                        AgentEvent::Error {
                            message: "Generated image unavailable".into(),
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
                event: event.clone(),
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
            .partition(|steer| steer.delivery == zeron_harness::SteerDelivery::AllowFollowUpTurn);
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
    /// engine restart (zeron sessions.ts:1039).
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
    /// (zeron sessions.ts:736, looked up on every dispatch):
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
        let entries = zeron_doc::join_continuation_entries(self.doc.read_entries().ok()?);
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
            zeron_proto::ChildRuntimeStatus::Active => "running",
            zeron_proto::ChildRuntimeStatus::Idle => "idle",
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
                action: zeron_proto::RecoveryAction::ObserveOnly,
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
        .get(zeron_proto::LUNA_RUNTIME_CONTRACT_OPTION)
        .and_then(|value| {
            serde_json::from_value::<zeron_proto::LunaAgentRuntimeContract>(value.clone()).ok()
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
            inner.set_status(&chat_id, SessionStatus::Errored, false);
            let engine = SessionsEngine {
                inner: inner.clone(),
            };
            if let Err(error) = engine.record_recovery_failure(
                &chat_id,
                harness_id,
                recovery_request,
                resume_state.user_message_id,
                acceptance,
                classify_failure(&message),
                message,
                None,
                resume_state.recovery.as_ref(),
            ) {
                tracing::error!(chat = %chat_id, %error, "failed to record pre-accept recovery");
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
    // PERSISTENT SESSION (zeron runsBySession): a completed turn on a
    // steerable harness parks here instead of ending the run — the child and
    // its steering mailbox stay warm, and the next user message (dispatch
    // routes into a live run) starts the next turn with zero respawn/resume
    // latency. `Some(when)` = idle since then; the 30-min reaper below ends
    // a session nobody comes back to (zeron SESSION_IDLE_MS).
    const SESSION_IDLE: std::time::Duration = std::time::Duration::from_secs(30 * 60);
    let mut idle_since: Option<tokio::time::Instant> = None;
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
    // moment output flows again, and nothing is lost. `ZERON_TURN_QUIESCE_MS`
    // overrides the window; 0 disables.
    // RETIRED for native drivers: a harness whose every turn shape ends with
    // a deterministic wire Done (claude/codex/cursor native) needs no
    // quiesce backstop — arming one only risks false parks on long silent
    // work. The env knob can configure a diagnostic window, but a prompt
    // with an authoritative completion signal must still await that signal.
    // ACP retains the watchdog only for unowned self-continued activity.
    let deterministic_turn_end = harness.deterministic_turn_end();
    let authoritative_prompt_end = harness.authoritative_prompt_end();
    let quiesce_after: Option<std::time::Duration> = match std::env::var("ZERON_TURN_QUIESCE_MS")
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
    // push the window during real thinking. `ZERON_SELF_TURN_QUIESCE_MS`
    // overrides; 0 falls back to the normal window. An explicit
    // `ZERON_TURN_QUIESCE_MS=0` still disables the watchdog entirely.
    let self_quiesce_after: Option<std::time::Duration> =
        match std::env::var("ZERON_SELF_TURN_QUIESCE_MS")
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
    let mut child_activity: std::collections::HashMap<String, zeron_doc::SubagentActivity> =
        std::collections::HashMap::new();

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
                // Idle reaper (zeron SESSION_IDLE_MS): a parked persistent session
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
                    None if idle_since.is_some() => break SessionStatus::Idle,
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
                            id != zeron_proto::LIVE_PLAN_TOOL_ID
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
            );
            // A Done with NO sink (a subagent that never streamed — codex
            // turn ends can beat registration) is chip-only: minting a doc
            // just to freeze it empty helps no one, and stamping the ref
            // would link the chip to that never-created doc (an empty tab
            // on click).
            let done_only = !sink_known && matches!(sub_event.as_ref(), AgentEvent::Done { .. });
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
                zeron_doc::fold_event_into_parts(&mut folded, &event);
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
                        let _ = doc_ref.update_subagent_chip(
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
                        AgentEvent::Usage { .. } | AgentEvent::ChildStatus { .. }
                    ))
                && !chip_streaming
            {
                // Completion or usage can arrive before child content. Update
                // the chip without manufacturing an empty transcript doc.
                let _ = doc_ref.update_subagent_chip(
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
                        let _ = doc_ref.update_subagent_chip(
                            parent_tool_use_id,
                            None,
                            Some("running"),
                            None,
                            Some(None),
                        );
                    }
                    continue;
                }
                if !metadata_only {
                    zeron_doc::fold_event_into_parts(&mut sink.folded, sub_event);
                    sink.dirty = true;
                }
                if !chip_streaming
                    && (subagent_chip_update(sub_event).is_some()
                        || matches!(sub_event.as_ref(), AgentEvent::Usage { .. }))
                {
                    // In-place refresh only on observed status/usage/result.
                    // Text deltas never rewrite the parent doc.
                    let _ = doc_ref.update_subagent_chip(
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
                            zeron_doc::SidecarPayload {
                                part_id: doc_id,
                                output: Some(json),
                                diff: None,
                            },
                        );
                    }
                }
            }
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
        let turn_was_active = idle_since.is_none();
        const RESUME_GATE: std::time::Duration = std::time::Duration::from_secs(1);
        if idle_since.is_some() {
            let self_continued = idle_since
                .is_some_and(|parked_at| parked_at.elapsed() >= RESUME_GATE)
                && (matches!(
                    &event,
                    AgentEvent::TextDelta { text } if !text.is_empty()
                ) || matches!(
                    &event,
                    AgentEvent::ToolCall { id, .. }
                        if id == zeron_proto::LIVE_PLAN_TOOL_ID || !seen_tools.contains(id)
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
        // segment's streaming text (the mid-word transcript splits). Dropped;
        // same-segment refreshes still land in place.
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
            AgentEvent::ToolCall { id, .. } if id == zeron_proto::LIVE_PLAN_TOOL_ID => {}
            AgentEvent::ToolResult { id, .. } if id == zeron_proto::LIVE_PLAN_TOOL_ID => {}
            AgentEvent::ToolCall { id, .. } => {
                if !in_segment(&folded, id) && seen_tools.contains(id) {
                    continue;
                }
                seen_tools.insert(id.clone());
            }
            AgentEvent::ToolResult { id, .. }
                if !in_segment(&folded, id) && seen_tools.contains(id) =>
            {
                continue;
            }
            _ => {}
        }

        // A steer boundary splits the assistant entry exactly where the fold resets.
        if let AgentEvent::Steered {
            next_assistant_message_id,
            ..
        } = &event
        {
            if harness_id == HarnessId::Codex {
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
            // The boundary confirms delivery of the oldest accepted steer —
            // retire its at-least-once ledger entry.
            let confirmed_steer = if let Some(h) = lock(&inner.runs)
                .get(&chat_id)
                .filter(|h| h.run_id == run_id)
            {
                lock(&h.routed_steers).pop_front()
            } else {
                None
            };
            if let Some(steer) = confirmed_steer
                && let Some(host) = inner.doc_host()
            {
                if steer.delivery == zeron_harness::SteerDelivery::RequireActiveTurn {
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
                    if let Some(request) = lock(&inner.last_requests).get(&chat_id) {
                        SessionsEngine {
                            inner: inner.clone(),
                        }
                        .note_turn_start(&chat_id, &request.cwd);
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
                        "steer acceptance receipt finalization failed");
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
                inner.set_status(&chat_id, SessionStatus::Working, false);
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

        // Defensive rule from zeron: a mid-run SessionStarted re-emission (Claude SDK
        // background re-invocations) must not wipe the segment being written.
        let skip_fold = matches!(&event, AgentEvent::SessionStarted { .. }) && !folded.is_empty();
        if !skip_fold {
            fold_event_into_parts(&mut folded, &event);
            // R2 sidecar PARKED (2026-08-10, product call): the fold's
            // summary/stats ARE the doc's whole record — no refs stamped, no
            // uploads. Full outputs survive only in the host's local run
            // journal. To reintroduce: `zeron_doc::sidecar_payload(&event)`
            // → `apply_sidecar_refs` → `doc_host.upload_tool_sidecar`, all
            // still in place and tested.
        }

        if let AgentEvent::Done { status, error, .. } = &event {
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
            let terminal_explicit = inner.take_terminal_explicit_steers(&chat_id, &run_id);
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
            let _ = doc_ref.update_subagent_chip(parent_id, None, Some("unobserved"), None, None);
        }
    }
    // Any child transcript still streaming freezes as-is: the parent event
    // stream is gone, so this sink cannot receive more content in this run.
    for (_, sink) in subagents.drain() {
        let doc_id = sink.doc_id.clone();
        if let Some(json) = sink.finish(&device_id, MessageStatus::Aborted)
            && let Some(host) = inner.doc_host()
        {
            host.upload_tool_sidecar(
                &chat_id,
                zeron_doc::SidecarPayload {
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
    inner.remove_run(&chat_id, &run_id);
    inner.set_status_with_completion(&chat_id, final_status, false, final_completed_turn);
    let (redispatch, explicit): (Vec<_>, Vec<_>) = orphans
        .into_iter()
        .partition(|steer| steer.delivery == zeron_harness::SteerDelivery::AllowFollowUpTurn);
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
                request.prompt = steer.prompt.clone();
                request.resume = None;
                request.attachments = Vec::new();
                tracing::info!(chat = %chat, "re-dispatching steer orphaned by a dying run");
                let Some(host) = engine.inner.doc_host() else {
                    tracing::warn!(chat = %chat, "orphaned steer lost: doc host unavailable");
                    break;
                };
                if let Err(err) = host
                    .dispatch_with_source_context(
                        &engine,
                        &chat,
                        harness_id,
                        request,
                        Some(steer.message_id.clone()),
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
    use std::collections::{HashSet, VecDeque};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use zeron_doc::{MessageRole, MessageStatus};
    use zeron_proto::{
        ContextControlState, ContextOperationStatus, HarnessId, Model, ReasoningLevel,
        RecoveryAcceptance, RecoveryAction, RecoveryFailure, RecoveryStatus, RunRequest,
        SandboxLevel, Session, SessionStatus, SteeringMode, TaskRecovery,
    };
    use zeron_sync::DocsStore;

    struct HoldingJournalHarness {
        release_first_turn: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl zeron_harness::Harness for HoldingJournalHarness {
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
        async fn models(&self) -> Result<Vec<Model>, zeron_harness::HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            _request: RunRequest,
            controls: zeron_harness::RunControls,
        ) -> Result<
            futures::stream::BoxStream<
                'static,
                Result<zeron_proto::AgentEvent, zeron_harness::HarnessError>,
            >,
            zeron_harness::HarnessError,
        > {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            let release = self.release_first_turn.clone();
            let token = controls.interrupt.clone();
            let mut steering = controls.steering;
            tokio::spawn(async move {
                let started = zeron_proto::AgentEvent::SessionStarted {
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
                    .send(Ok(zeron_proto::AgentEvent::TextDelta {
                        text: "first".into(),
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::select! {
                    _ = release.notified() => {
                        if tx.send(Ok(zeron_proto::AgentEvent::Done { status: zeron_proto::DoneStatus::Completed, result: None, error: None, session_id: Some("synthetic-session".into()) })).await.is_err() { return; }
                    }
                    _ = token.cancelled() => {
                        let _ = tx.send(Ok(zeron_proto::AgentEvent::Done { status: zeron_proto::DoneStatus::Interrupted, result: None, error: None, session_id: None })).await;
                        return;
                    }
                }
                loop {
                    tokio::select! {
                        _ = token.cancelled() => {
                            let _ = tx.send(Ok(zeron_proto::AgentEvent::Done { status: zeron_proto::DoneStatus::Interrupted, result: None, error: None, session_id: None })).await;
                            break;
                        }
                        Some(_) = steering.recv() => {
                            if tx.send(Ok(zeron_proto::AgentEvent::Steered { assistant_message_id: None, next_assistant_message_id: None })).await.is_err() { break; }
                            if tx.send(Ok(zeron_proto::AgentEvent::TextDelta { text: "steered".into() })).await.is_err() { break; }
                            if tx.send(Ok(zeron_proto::AgentEvent::Done { status: zeron_proto::DoneStatus::Completed, result: None, error: None, session_id: Some("synthetic-session".into()) })).await.is_err() { break; }
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
        let registry = Arc::new(crate::registry::HarnessRegistry::new());
        registry.register(Arc::new(HoldingJournalHarness {
            release_first_turn: release,
        }));
        let data = root.join("synthetic-engine");
        let accounts = crate::agent_accounts::AgentAccountsConfig {
            data_dir: root.join("synthetic-provider-state"),
            claude_config_dir: root.join("synthetic-claude"),
            claude_config_file: root.join("synthetic-claude.json"),
            codex_home: root.join("synthetic-codex"),
            cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
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
        predicate: impl Fn(&zeron_proto::AgentEvent) -> bool,
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
            |event| matches!(event, zeron_proto::AgentEvent::TextDelta { text } if text == "first"),
        )
        .await;
        assert!(matches!(
            epoch.freeze(Duration::from_millis(40)).await,
            Err(zeron_sync::write_epoch::EpochError::TimedOut)
        ));
        assert_eq!(epoch.phase(), zeron_sync::write_epoch::Phase::Open);
        release.notify_one();
        journal_event(&mut live, |event| {
            matches!(
                event,
                zeron_proto::AgentEvent::Done {
                    status: zeron_proto::DoneStatus::Completed,
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
            Err(zeron_sync::write_epoch::EpochError::TimedOut)
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
            matches!(event, zeron_proto::AgentEvent::Steered { .. })
        })
        .await;
        journal_event(&mut live, |event| {
            matches!(
                event,
                zeron_proto::AgentEvent::Done {
                    status: zeron_proto::DoneStatus::Completed,
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
            matches!(event, zeron_proto::AgentEvent::SessionStarted { .. })
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
            |event| matches!(event, zeron_proto::AgentEvent::TextDelta { text } if text == "first"),
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
        let question = zeron_proto::UserInputQuestion {
            id: "approval-1".into(),
            header: "Permission".into(),
            question: "Allow?".into(),
            options: vec!["Deny".into(), "Allow once".into()],
            multi_select: false,
            approval: Some(zeron_proto::ApprovalRequest {
                provider_request_id: "provider-1".into(),
                provider: HarnessId::ClaudeCode,
                turn_id: Some("turn-1".into()),
                machine_name: "test-machine".into(),
                action_kind: zeron_proto::ApprovalActionKind::Command,
                summary: "echo ok".into(),
                requested_scope: zeron_proto::PermissionScope::Workspace,
                expires_at_ms: i64::MAX,
                supports_task_allowance: false,
            }),
        };
        let answer = approval_answer(&question, zeron_proto::ApprovalDecision::AllowTask).unwrap();
        assert_eq!(answer.labels, vec!["Deny"]);
        assert!(
            approval_answer(
                &zeron_proto::UserInputQuestion {
                    approval: None,
                    ..question
                },
                zeron_proto::ApprovalDecision::AllowOnce
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
            Some(zeron_proto::AgentEvent::InputResolved { request_id })
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
                questions: vec![zeron_proto::UserInputQuestion {
                    id: "question-1".into(),
                    header: "Approval".into(),
                    question: "Allow?".into(),
                    options: vec!["Deny".into(), "Allow once".into()],
                    multi_select: false,
                    approval: Some(zeron_proto::ApprovalRequest {
                        provider_request_id: "provider-1".into(),
                        provider: HarnessId::Codex,
                        turn_id: None,
                        machine_name: "test-machine".into(),
                        action_kind: zeron_proto::ApprovalActionKind::Command,
                        summary: "echo safe".into(),
                        requested_scope: zeron_proto::PermissionScope::Workspace,
                        expires_at_ms: i64::MAX,
                        supports_task_allowance: true,
                    }),
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
        let question = |id: &str, action_kind| zeron_proto::UserInputQuestion {
            id: id.into(),
            header: "Approval".into(),
            question: "Allow?".into(),
            options: vec!["Deny".into(), "Allow once".into()],
            multi_select: false,
            approval: Some(zeron_proto::ApprovalRequest {
                provider_request_id: "claimed-local-ui".into(),
                provider: HarnessId::Codex,
                turn_id: Some("turn-a".into()),
                machine_name: "synthetic".into(),
                action_kind,
                summary: "synthetic".into(),
                requested_scope: zeron_proto::PermissionScope::Workspace,
                expires_at_ms: i64::MAX,
                supports_task_allowance: false,
            }),
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
                    zeron_proto::ApprovalActionKind::Command,
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
                    zeron_proto::ApprovalActionKind::Tool,
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
                    zeron_proto::ApprovalActionKind::LunaCrossTask,
                )],
                resolver: forged_source_tx,
            },
        );
        let (ordinary_tx, ordinary_rx) = tokio::sync::oneshot::channel();
        pending.lock().unwrap().insert(
            "ordinary-question".into(),
            PendingInput {
                kind: PendingInputKind::Provider,
                questions: vec![zeron_proto::UserInputQuestion {
                    id: "ordinary".into(),
                    header: "Question".into(),
                    question: "Which?".into(),
                    options: vec!["A".into()],
                    multi_select: false,
                    approval: None,
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
                interrupt_token: zeron_harness::CancellationToken::new(),
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
                    zeron_proto::ApprovalDecision::AllowOnce,
                )
                .unwrap()
        );
        assert!(
            !sessions
                .respond_input(
                    "chat-a",
                    "claimed-ui-approval",
                    vec![zeron_proto::UserInputAnswer {
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
                    zeron_proto::ApprovalDecision::AllowOnce,
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
                    zeron_proto::ApprovalDecision::AllowOnce,
                )
                .unwrap()
        );
        assert_eq!(provider_rx.await.unwrap()[0].labels, vec!["Allow once"]);
        assert!(
            sessions
                .respond_input(
                    "chat-a",
                    "ordinary-question",
                    vec![zeron_proto::UserInputAnswer {
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
        tokio::sync::mpsc::UnboundedReceiver<zeron_proto::AgentEvent>,
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
                interrupt_token: zeron_harness::CancellationToken::new(),
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
            cursor_sdk_auth_file: root.path().join("synthetic-cursor-auth.json"),
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
                zeron_proto::TaskLineage::default(),
            )
            .unwrap();
        let (steer_tx, _steer_rx) = tokio::sync::mpsc::channel(1);
        let (cancel, _cancel_rx) = tokio::sync::watch::channel(false);
        let (engine_tx, _engine_rx) = tokio::sync::mpsc::unbounded_channel();
        lock(&core.sessions.inner.runs).insert(
            task_id.clone(),
            RunHandle {
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
                interrupt_token: zeron_harness::CancellationToken::new(),
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

    struct FakeControlEvidenceBackend {
        source_task_id: String,
        source_environment_id: String,
        target: Mutex<zeron_proto::CrossTaskSummary>,
        snapshots: Mutex<VecDeque<zeron_proto::TaskCommandSnapshot>>,
        snapshot_reads: AtomicUsize,
        gate_on_read: usize,
        entered_snapshot: Option<Arc<tokio::sync::Notify>>,
        release_snapshot: Option<Arc<tokio::sync::Notify>>,
        control_calls: AtomicUsize,
        receipt_reads: AtomicUsize,
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
        ) -> Result<Option<zeron_proto::CrossTaskSummary>, crate::cross_task_tools::CrossTaskError>
        {
            let target = lock(&self.target);
            Ok(if task_id == self.source_task_id {
                Some(zeron_proto::CrossTaskSummary {
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
            _: zeron_proto::CrossTaskListRequest,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<zeron_proto::CrossTaskListPage, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn read(
            &self,
            _: zeron_proto::CrossTaskReadRequest,
        ) -> Result<zeron_proto::CrossTaskReadPage, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn enqueue_queue(
            &self,
            _: zeron_proto::CrossTaskSendRequest,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<zeron_proto::CrossTaskSendReceipt, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn wait_event(
            &self,
            _: zeron_proto::CrossTaskWaitRequest,
        ) -> Result<Option<zeron_proto::CrossTaskWaitEvent>, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn control_target_snapshot(
            &self,
            _: &str,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<zeron_proto::TaskCommandSnapshot, crate::cross_task_tools::CrossTaskError>
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
            _: &str,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<Option<zeron_proto::CommandReceipt>, crate::cross_task_tools::CrossTaskError>
        {
            self.receipt_reads.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }

        async fn control(
            &self,
            _: zeron_proto::CrossTaskControlRequest,
            _: &crate::cross_task_tools::CrossTaskAuthority,
            _: &crate::cross_task_tools::CrossTaskControlGrant,
        ) -> Result<zeron_proto::CrossTaskControlReceipt, crate::cross_task_tools::CrossTaskError>
        {
            self.control_calls.fetch_add(1, Ordering::SeqCst);
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
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
            delivery: zeron_proto::RuntimeDelivery::CodexDynamicTools,
        }
    }

    fn control_evidence_backend(
        core: &crate::EngineCore,
        source_task_id: &str,
    ) -> Arc<FakeControlEvidenceBackend> {
        Arc::new(FakeControlEvidenceBackend {
            source_task_id: source_task_id.into(),
            source_environment_id: core.device_id.clone(),
            target: Mutex::new(zeron_proto::CrossTaskSummary {
                task_id: "target-a".into(),
                project_id: Some("project-a".into()),
                environment_id: core.device_id.clone(),
                title: "Synthetic target".into(),
                status: "idle".into(),
                archived: false,
                updated_at_ms: 1,
            }),
            snapshots: Mutex::new(VecDeque::from([zeron_proto::TaskCommandSnapshot {
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

    async fn prospective_interrupt_fixture() -> (
        tempfile::TempDir,
        crate::EngineCore,
        String,
        Arc<FakeControlEvidenceBackend>,
        super::PreparedControlEvidence,
        tokio::sync::mpsc::UnboundedReceiver<zeron_proto::AgentEvent>,
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
        let zeron_proto::AgentEvent::InputRequested {
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
            zeron_proto::ApprovalActionKind::LunaCrossTask
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
                zeron_proto::CrossTaskControlAction::Interrupt
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
                    zeron_proto::ApprovalDecision::AllowOnce,
                )
                .unwrap()
        );
        assert_eq!(
            core.sessions.respond_luna_local_ui_approval(
                &source_task_id,
                &request_id,
                zeron_proto::ApprovalDecision::AllowOnce,
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
            Ok(zeron_proto::AgentEvent::InputResolved { request_id: resolved })
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
            Ok(zeron_proto::AgentEvent::InputRequested { .. })
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
                            zeron_proto::ApprovalDecision::AllowOnce,
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
            zeron_proto::CrossTaskControlAction::Interrupt
        );
        assert!(matches!(
            events.try_recv(),
            Ok(zeron_proto::AgentEvent::InputResolved { request_id: resolved })
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
            zeron_proto::ApprovalDecision::Deny,
            zeron_proto::ApprovalDecision::Cancel,
            zeron_proto::ApprovalDecision::AllowTask,
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
                Ok(zeron_proto::AgentEvent::InputRequested { .. })
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
            let label = if decision == zeron_proto::ApprovalDecision::Cancel {
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
                    zeron_proto::ApprovalDecision::AllowOnce,
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
                    zeron_proto::ApprovalDecision::AllowOnce,
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
                Ok(zeron_proto::AgentEvent::InputRequested { .. })
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
                        zeron_proto::ApprovalDecision::AllowOnce,
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
                            zeron_proto::BindingOrigin::ContextReset,
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
                    zeron_proto::ApprovalDecision::AllowOnce,
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
                Ok(zeron_proto::AgentEvent::InputRequested { .. })
            ));
            let sessions = core.sessions.clone();
            let task = source_task_id.clone();
            let request = request_id.clone();
            let responding = tokio::spawn(async move {
                sessions
                    .respond_luna_prospective_interrupt_local_ui(
                        &task,
                        &request,
                        zeron_proto::ApprovalDecision::AllowOnce,
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
                            zeron_proto::BindingOrigin::ContextReset,
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
                    zeron_proto::ApprovalDecision::AllowOnce,
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
                    zeron_proto::ApprovalDecision::AllowOnce,
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
                Ok(zeron_proto::AgentEvent::InputRequested { .. })
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
                            zeron_proto::BindingOrigin::ContextReset,
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
                Ok(zeron_proto::AgentEvent::InputRequested { .. })
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
                zeron_proto::BindingOrigin::ContextReset,
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
            Ok(zeron_proto::AgentEvent::InputRequested { .. })
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
                Ok(zeron_proto::AgentEvent::InputRequested { .. })
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
                Ok(zeron_proto::AgentEvent::InputResolved { request_id: resolved })
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
            Ok(zeron_proto::AgentEvent::InputRequested { .. })
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
            cursor_sdk_auth_file: root.path().join("synthetic-cursor-auth.json"),
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                            zeron_proto::BindingOrigin::ContextReset,
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
                zeron_proto::BindingOrigin::ContextReset,
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
                zeron_proto::BindingOrigin::ContextReset,
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
        let event = zeron_proto::AgentEvent::TextDelta {
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
    ) -> tokio::sync::oneshot::Receiver<Vec<zeron_proto::UserInputAnswer>> {
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
                        zeron_proto::ApprovalDecision::AllowOnce,
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
            Ok(zeron_proto::AgentEvent::InputResolved { request_id })
                if request_id == "approval-a"
        ));
        assert!(events.try_recv().is_err());
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "approval-a",
                zeron_proto::ApprovalDecision::AllowOnce,
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
                zeron_proto::ApprovalDecision::AllowTask,
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
                zeron_proto::ApprovalDecision::Deny,
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
                zeron_proto::ApprovalDecision::Cancel,
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                questions: vec![zeron_proto::UserInputQuestion {
                    id: "forged-question".into(),
                    header: "Forged".into(),
                    question: "Allow?".into(),
                    options: vec!["Allow once".into()],
                    multi_select: false,
                    approval: Some(zeron_proto::ApprovalRequest {
                        provider_request_id: "claimed-local-ui".into(),
                        provider: HarnessId::Codex,
                        turn_id: None,
                        machine_name: "synthetic".into(),
                        action_kind: zeron_proto::ApprovalActionKind::LunaCrossTask,
                        summary: "forged source".into(),
                        requested_scope: zeron_proto::PermissionScope::Workspace,
                        expires_at_ms: crate::now_ms() + 30_000,
                        supports_task_allowance: false,
                    }),
                }],
                resolver: forged_tx,
            },
        );
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "forged-marker",
                zeron_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
        assert!(lock(&pending).contains_key("forged-marker"));
    }

    struct FakeArchiveBackend {
        target: zeron_proto::CrossTaskSummary,
        snapshot: zeron_proto::TaskCommandSnapshot,
        live: Mutex<FakeLiveIdentity>,
        calls: AtomicUsize,
        suppress_readback: AtomicBool,
        receipt: Mutex<Option<zeron_proto::CrossTaskControlReceipt>>,
    }

    impl FakeArchiveBackend {
        fn readback(&self, command_id: &str) -> Option<zeron_proto::CrossTaskControlReceipt> {
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
                event_wait: false,
            }
        }

        async fn task(
            &self,
            task_id: &str,
        ) -> Result<Option<zeron_proto::CrossTaskSummary>, crate::cross_task_tools::CrossTaskError>
        {
            Ok((task_id == self.target.task_id).then(|| self.target.clone()))
        }

        async fn list(
            &self,
            _: zeron_proto::CrossTaskListRequest,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<zeron_proto::CrossTaskListPage, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn read(
            &self,
            _: zeron_proto::CrossTaskReadRequest,
        ) -> Result<zeron_proto::CrossTaskReadPage, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn enqueue_queue(
            &self,
            _: zeron_proto::CrossTaskSendRequest,
            _: &crate::cross_task_tools::CrossTaskAuthority,
        ) -> Result<zeron_proto::CrossTaskSendReceipt, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn wait_event(
            &self,
            _: zeron_proto::CrossTaskWaitRequest,
        ) -> Result<Option<zeron_proto::CrossTaskWaitEvent>, crate::cross_task_tools::CrossTaskError>
        {
            Err(crate::cross_task_tools::CrossTaskError::Unsupported)
        }

        async fn control(
            &self,
            request: zeron_proto::CrossTaskControlRequest,
            _: &crate::cross_task_tools::CrossTaskAuthority,
            _: &crate::cross_task_tools::CrossTaskControlGrant,
        ) -> Result<zeron_proto::CrossTaskControlReceipt, crate::cross_task_tools::CrossTaskError>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let receipt = zeron_proto::CrossTaskControlReceipt {
                task_id: request.task_id.clone(),
                command: zeron_proto::CommandReceipt {
                    command_id: request.command_id,
                    task_id: request.task_id,
                    intent: "setArchived".into(),
                    payload_hash: "synthetic-archive".into(),
                    status: zeron_proto::CommandReceiptStatus::Applied,
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
        target_snapshot: zeron_proto::TaskCommandSnapshot,
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
        tokio::sync::mpsc::UnboundedReceiver<zeron_proto::AgentEvent>,
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
        let snapshot = zeron_proto::TaskCommandSnapshot {
            task_id: "task-a".into(),
            task_revision: 7,
            task_sequence: 12,
            binding_id: Some("binding-a".into()),
            binding_generation: Some(2),
            active_turn_id: None,
        };
        let request = zeron_proto::CrossTaskControlRequest {
            action: zeron_proto::CrossTaskControlAction::Archive,
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
        tokio::sync::oneshot::Receiver<Vec<zeron_proto::UserInputAnswer>>,
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
            target: zeron_proto::CrossTaskSummary {
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
        attempted: zeron_proto::CrossTaskControlRequest,
        backend: Arc<FakeArchiveBackend>,
        now_ms: i64,
    ) -> Result<zeron_proto::CrossTaskControlReceipt, crate::cross_task_tools::CrossTaskError> {
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
            delivery: zeron_proto::RuntimeDelivery::CodexDynamicTools,
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
                        zeron_proto::ApprovalDecision::AllowOnce,
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
            zeron_proto::CommandReceiptStatus::Applied
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(backend.readback(&receipt.command.command_id), Some(receipt));
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                &task_id,
                "approved",
                zeron_proto::ApprovalDecision::AllowOnce,
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                    zeron_proto::ApprovalDecision::AllowOnce,
                ),
                LunaApprovalOutcome::AllowedOnce
            );
            let mut fake_now = crate::now_ms();
            match case {
                0 => attempted.action = zeron_proto::CrossTaskControlAction::Unarchive,
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
            ("deny", zeron_proto::ApprovalDecision::Deny),
            ("cancel", zeron_proto::ApprovalDecision::Cancel),
            ("allow-task", zeron_proto::ApprovalDecision::AllowTask),
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                    zeron_proto::ApprovalDecision::AllowOnce,
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
                            zeron_proto::BindingOrigin::ContextReset,
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
                    zeron_proto::ApprovalDecision::AllowOnce,
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
                    zeron_proto::ApprovalDecision::AllowOnce,
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
                            zeron_proto::BindingOrigin::ContextReset,
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                zeron_proto::ApprovalDecision::AllowOnce,
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
                    zeron_proto::ApprovalDecision::AllowOnce,
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
                call: zeron_proto::ToolCall::Unknown {
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
            prompt: "first".into(),
            harness: None,
            provider_account_id: None,
            model: Some("grok-4.6".into()),
            reasoning: Some(zeron_proto::ReasoningLevel::High),
            model_options: serde_json::Map::new(),
            cwd: "/tmp".into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            permission_mode: zeron_proto::PermissionMode::Ask,
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

        follow_up.reasoning = Some(zeron_proto::ReasoningLevel::Medium);
        assert!(!config.can_route(HarnessId::Grok, &follow_up));
        follow_up.reasoning = initial.reasoning;

        follow_up.provider_account_id = Some("codex-account-b".into());
        assert!(
            !config.can_route(HarnessId::Grok, &follow_up),
            "an auth-instance change must retire the warm runtime"
        );
        follow_up.provider_account_id = initial.provider_account_id.clone();

        follow_up.model_options.insert(
            zeron_harness::LUNA_PROVIDER_AUTH_INSTANCE_OPTION.into(),
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
                interrupt_token: zeron_harness::CancellationToken::new(),
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
