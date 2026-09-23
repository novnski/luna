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

use std::collections::HashMap;
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingInputKind {
    Provider,
    // Only engine-owned registration may select this kind. The run binding is
    // never derived from a serialized question or client-provided source hint.
    #[allow(dead_code)]
    LunaCrossTask {
        run_id: String,
    },
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
    if matches!(&pending.kind, PendingInputKind::LunaCrossTask { .. })
        || pending
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
                    std::sync::Arc::new(move |name: String, arguments: serde_json::Value| {
                        let contract = contract.clone();
                        let cancel = cancel.clone();
                        let backend = backend.as_ref().and_then(Weak::upgrade);
                        Box::pin(async move {
                            crate::runtime_contract::invoke_with_backend(
                                &contract, &name, arguments, &cancel, backend,
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
        let target = lock(&self.inner.runs).get(chat_id).map(|h| {
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
            lock(pending)
                .get(request_id)
                .is_some_and(|item| matches!(&item.kind, PendingInputKind::LunaCrossTask { .. }))
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
            if !matches!(&pending.kind, PendingInputKind::LunaCrossTask { run_id: bound } if bound == &handle.run_id)
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
            let now = now_ms();
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
        if handle
            .engine_tx
            .send(AgentEvent::InputResolved {
                request_id: request_id.to_string(),
            })
            .is_err()
        {
            let _ = pending.resolver.send(Vec::new());
            return LunaApprovalOutcome::RunEnded;
        }
        if pending
            .resolver
            .send(vec![UserInputAnswer {
                question_id: pending.questions[0].id.clone(),
                labels: vec![label.into()],
            }])
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
            self.inner.publish(&chat_id, &done);
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

    fn publish(&self, chat_id: &str, event: &AgentEvent) -> u64 {
        let seq = match self.journal.append(chat_id, event) {
            Ok(seq) => seq,
            Err(err) => {
                tracing::error!(chat = %chat_id, error = %err, "journal append failed");
                0
            }
        };
        if let Some(hub) = lock(&self.hubs).get(chat_id) {
            let _ = hub.send(JournaledEvent {
                seq,
                event: event.clone(),
            });
        }
        seq
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
            inner.publish(
                &chat_id,
                &AgentEvent::Error {
                    message: message.clone(),
                },
            );
            inner.publish(
                &chat_id,
                &AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(message.clone()),
                    session_id: None,
                },
            );
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
    let final_status = loop {
        let event: AgentEvent = if let Some(event) = prepared_events.pop_front() {
            event
        } else {
            let raw_event = tokio::select! {
                biased;
                changed = cancel_rx.changed(), if !interrupted => {
                    let _ = changed;
                    interrupted = true;
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
            inner.publish(&chat_id, &event);
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
            inner.publish(&chat_id, &event);
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
            // Provider checkpoint ids are LUNA control-plane metadata. They
            // are persisted on the completed assistant entry below, never in
            // the visible event stream or transcript parts.
            continue;
        }

        match &event {
            AgentEvent::SessionStarted {
                session_id, cwd, ..
            } => {
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
                    // Binding conflicts are terminal. Publishing SessionStarted
                    // without its durable mapping would make an unowned native
                    // thread look resumable, so fail before that event escapes.
                    inner.publish(
                        &chat_id,
                        &AgentEvent::Error {
                            message: error.to_string(),
                        },
                    );
                    inner.publish(
                        &chat_id,
                        &AgentEvent::Done {
                            status: DoneStatus::Errored,
                            result: None,
                            error: Some(error.to_string()),
                            session_id: Some(session_id.clone()),
                        },
                    );
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

        inner.publish(&chat_id, &event);

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
        LunaApprovalOutcome, PendingInput, PendingInputKind, PendingInputs, RunHandle,
        RuntimeConfig, SessionsEngine, approval_answer, expire_pending_approval, lock,
        reduce_provider_context_compaction, reduce_provider_context_terminal, subagent_doc_id,
        take_pending_ordinary_input,
    };
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use zeron_proto::{
        ContextControlState, ContextOperationStatus, HarnessId, RecoveryAcceptance, RecoveryAction,
        RecoveryFailure, RecoveryStatus, RunRequest, SandboxLevel, Session, SessionStatus,
        TaskRecovery,
    };
    use zeron_sync::DocsStore;

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
        pending.lock().unwrap().insert(
            "claimed-ui-approval".into(),
            PendingInput {
                kind: PendingInputKind::LunaCrossTask {
                    run_id: "run-a".into(),
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

    /// Synthetic engine-owned registration; no production path creates LUNA
    /// approvals until the later cross-task tool work is authorized.
    fn inject_luna_pending_for_test(
        pending: &PendingInputs,
        request_id: &str,
        bound_run_id: &str,
        expires_at_ms: i64,
    ) -> tokio::sync::oneshot::Receiver<Vec<zeron_proto::UserInputAnswer>> {
        let (resolver, answer) = tokio::sync::oneshot::channel();
        lock(pending).insert(
            request_id.into(),
            PendingInput {
                kind: PendingInputKind::LunaCrossTask {
                    run_id: bound_run_id.into(),
                },
                questions: vec![zeron_proto::UserInputQuestion {
                    id: format!("question-{request_id}"),
                    header: "LUNA approval".into(),
                    question: "Allow once?".into(),
                    options: vec!["Deny".into(), "Allow once".into()],
                    multi_select: false,
                    approval: Some(zeron_proto::ApprovalRequest {
                        provider_request_id: "synthetic-luna".into(),
                        provider: HarnessId::Codex,
                        turn_id: Some("turn-a".into()),
                        machine_name: "synthetic".into(),
                        action_kind: zeron_proto::ApprovalActionKind::LunaCrossTask,
                        summary: "synthetic local action".into(),
                        requested_scope: zeron_proto::PermissionScope::Workspace,
                        expires_at_ms,
                        supports_task_allowance: false,
                    }),
                }],
                resolver,
            },
        );
        answer
    }

    #[tokio::test]
    async fn local_luna_approval_is_one_shot_even_for_concurrent_clicks() {
        let (_root, sessions, pending, mut events) = local_luna_test_run();
        let answer =
            inject_luna_pending_for_test(&pending, "approval-a", "run-a", crate::now_ms() + 30_000);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let calls = (0..2)
            .map(|_| {
                let sessions = sessions.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    sessions.respond_luna_local_ui_approval(
                        "chat-a",
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
        assert!(matches!(
            events.try_recv(),
            Ok(zeron_proto::AgentEvent::InputResolved { request_id })
                if request_id == "approval-a"
        ));
        assert!(events.try_recv().is_err());
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                "chat-a",
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

        let (_root, sessions, pending, _events) = local_luna_test_run();
        let lost = inject_luna_pending_for_test(
            &pending,
            "lost-resolver",
            "run-a",
            crate::now_ms() + 30_000,
        );
        drop(lost);
        assert_eq!(
            sessions.respond_luna_local_ui_approval(
                "chat-a",
                "lost-resolver",
                zeron_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::RunEnded
        );
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
                "chat-a",
                "forged-marker",
                zeron_proto::ApprovalDecision::AllowOnce,
            ),
            LunaApprovalOutcome::Stale
        );
        assert!(lock(&pending).contains_key("forged-marker"));
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
            sessions.inner.publish("chat", event);
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
