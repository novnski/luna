//! CORE-11: the hosted provider bridge, provider profile 2.0
//! (`luna_harness::hosted`). A `Harness` that serves a package whose worker
//! hosts a whole adapter (the first-party OpenCode package hosts LUNA's own
//! OpenCode 2 adapter), through CORE-07 supervised instances.
//!
//! - **One instance per run.** The run's worker owns the provider's own
//!   server process (OpenCode: `opencode serve --stdio`, its port and its
//!   password), so Stop, a crash or a resource limit affects only that run,
//!   and the instance is stopped when the run ends. A native child Stop and
//!   a compaction of a chat with a live run go to that run's worker.
//! - **One service instance** for everything outside a run (models, titles,
//!   commands, extensions, MCP status, fork, history, rewind, compaction of
//!   an idle chat), kept while the provider is registered, like the single
//!   in-process adapter it replaces.
//! - The bridge invents nothing. Run events, questions, tool calls and
//!   results are the adapter's own values in the adapter's order (contiguous
//!   `seq`, duplicates dropped, a gap fails the run). Only a run the worker
//!   provably never started is "rejected before acceptance"; a start whose
//!   answer was lost is delivery-unknown and never resent. A run ends with
//!   the adapter's own `Done`; the bridge adds one (`Errored`/`Interrupted`,
//!   with the reason) only when the worker ended without it.
//! - Permission modes: the adapter settles its own approvals (OpenCode Full
//!   answers stray asks once; legacy Ask shows its cards). An approval card
//!   in a Full or Auto run is refused here, so it can never park a person.
//! - Admission stays core-owned: the engine resolves the mode, validates
//!   inputs and records receipts before `run` is called.
//!
//! A native worker is user-level code without an OS sandbox (CORE-07).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use luna_harness::hosted::{self, op};
use luna_harness::mcp_control::{McpControlReply, McpControlRequest, SessionMcpObservation};
use luna_harness::{
    CancellationToken, CommandScope, ContextCompactRequest, Harness, HarnessError,
    LunaToolExecutor, LunaToolInvocation, NativeHistoryScope, NativeHistoryTurns,
    NativeRewindOutcome, NativeRewindProgress, NativeRewindProgressReporter, NativeRewindRequest,
    NativeTaskStop, ProviderForkProgressReporter, RunControls, SteerAcceptance, SteerMessage,
};
use luna_proto::contract::ExtensionId;
use luna_proto::{
    AgentEvent, CallExtensionInstanceRequest, DoneStatus, ExtensionCallStatus,
    ExtensionInstanceState, ExtensionRuntimeKind, ExtensionWorkerExit, HarnessId, Model,
    NativeExtensionsView, PermissionMode, ProviderForkCapability, ProviderForkRequest,
    ProviderForkResult, ReasoningLevel, RunRequest, RuntimeDelivery, SlashCommand,
    StartExtensionInstanceRequest, SteeringMode, UserInputAnswer, UserInputQuestion,
};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};

use super::super::{ExtensionManager, Inner};
use super::bridge::{DescribeRecord, Executable};
use super::declaration::{HostedDeclaration, HostedOperation, ProfileVersion, ProviderDeclaration};

const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_CALL_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long one `provider.events` call may wait for items.
const POLL_WAIT: Duration = Duration::from_secs(10);
const POLL_SLACK: Duration = Duration::from_secs(5);
/// How long a run's own interrupt may take before LUNA ends the run's
/// worker group itself. OpenCode's adapter waits 2 s for its interrupt and
/// 3 s for its server before it kills it; this stays above both.
const STOP_CONFIRM: Duration = Duration::from_secs(8);
const BUSY_RETRY: Duration = Duration::from_millis(100);
const BUSY_ATTEMPTS: u32 = 50;
/// Consecutive unanswered event requests that end a worker.
const MAX_DEADLINE_STREAK: u32 = 2;
const JOB_BUFFER: usize = 512;
const STREAM_BUFFER: usize = 256;
const MAX_OBSERVATIONS: usize = 256;
const MAX_STATE_KEYS: usize = 4096;
const MAX_MESSAGE_BYTES: usize = 4 * 1024;
/// A run's report that LUNA tool delivery is withdrawn holds this long, the
/// adapter's own retry window for a failed attachment readback.
const DELIVERY_WITHDRAWAL: Duration = Duration::from_secs(10 * 60);
/// A description that found the CLI missing hides the provider this long;
/// then the next discovery describes again (a CLI installed meanwhile shows
/// up, as the in-process adapter's filesystem probe noticed it).
const UNAVAILABLE_RECHECK: Duration = Duration::from_secs(60);

type InputRequester =
    Box<dyn Fn(Vec<UserInputQuestion>) -> oneshot::Receiver<Vec<UserInputAnswer>> + Send + Sync>;
type SteerAcks = Arc<Mutex<HashMap<String, oneshot::Sender<SteerAcceptance>>>>;
type ProgressSink<'a> = dyn FnMut(&Value) -> Result<(), HarnessError> + Send + 'a;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// At most `max` bytes, cut on a character boundary.
fn clip(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

/// The adapter's own message, without the error kind's prefix.
fn error_text(error: &HarnessError) -> String {
    match error {
        HarnessError::NotInstalled(message)
        | HarnessError::Protocol(message)
        | HarnessError::Tool(message)
        | HarnessError::RejectedBeforeAcceptance(message)
        | HarnessError::Install(message) => message.clone(),
        HarnessError::Io(error) => error.to_string(),
    }
}

fn mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Full => "Full access",
        PermissionMode::Auto => "Approve for me",
        PermissionMode::Ask => "Ask",
    }
}

fn exit_detail(exit: Option<&ExtensionWorkerExit>) -> String {
    let Some(exit) = exit else {
        return String::new();
    };
    let reason = serde_json::to_value(exit.reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "exited".into());
    let mut detail = format!(" ({reason}");
    if let Some(code) = exit.code {
        detail.push_str(&format!(", exit code {code}"));
    }
    if let Some(signal) = exit.signal {
        detail.push_str(&format!(", signal {signal}"));
    }
    if let Some(text) = &exit.detail {
        detail.push_str(&format!(": {}", clip(text.clone(), 256)));
    }
    detail.push(')');
    detail
}

fn event_kind(event: &Value) -> String {
    event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn done_ok(event: &Value) -> bool {
    event.get("ok").and_then(Value::as_bool) == Some(true)
}

// ---------------------------------------------------------------------------
// One supervised instance
// ---------------------------------------------------------------------------

enum Reply {
    Ok(Value),
    Failed {
        code: String,
        message: String,
    },
    Deadline,
    /// The worker exited, or a replacement worker answered.
    Exited(Option<ExtensionWorkerExit>),
    Busy,
}

enum Item {
    Event(Value),
    /// The worker is gone; the reason, without the provider's name.
    Gone(String),
}

enum StartError {
    /// Nothing reached the worker, or it refused the job before running it.
    NotSent(String),
    /// The job may be running; it is never sent again.
    Unknown(String),
}

impl StartError {
    fn into_message(self) -> String {
        match self {
            Self::NotSent(message) | Self::Unknown(message) => message,
        }
    }
}

#[derive(Default)]
struct PumpState {
    /// The last `seq` applied.
    cursor: u64,
    /// The fragments of the item being reassembled.
    fragments: Vec<String>,
}

struct Worker {
    manager: ExtensionManager,
    instance_id: String,
    generation: u32,
    label: String,
    jobs: Mutex<HashMap<String, mpsc::Sender<Item>>>,
    /// A pump task is reading the event queue.
    pumping: Mutex<bool>,
    pump: Mutex<PumpState>,
    gone: Mutex<Option<String>>,
    closed: AtomicBool,
}

impl Drop for Worker {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let manager = self.manager.clone();
        let instance = self.instance_id.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = manager.stop_instance(&instance).await;
            });
        }
    }
}

struct Job {
    id: String,
    receiver: mpsc::Receiver<Item>,
    worker: Arc<Worker>,
}

impl Job {
    async fn next(&mut self) -> Option<Item> {
        self.receiver.recv().await
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.worker.unregister(&self.id);
    }
}

impl Worker {
    fn new(manager: ExtensionManager, instance_id: String, generation: u32, label: String) -> Self {
        Self {
            manager,
            instance_id,
            generation,
            label,
            jobs: Mutex::new(HashMap::new()),
            pumping: Mutex::new(false),
            pump: Mutex::new(PumpState::default()),
            gone: Mutex::new(None),
            closed: AtomicBool::new(false),
        }
    }

    /// `Err` means the engine refused the call: nothing reached the worker.
    async fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Reply, String> {
        let request: CallExtensionInstanceRequest = serde_json::from_value(json!({
            "instanceId": self.instance_id,
            "method": method,
            "params": params,
            "timeoutMs": timeout.as_millis() as u64,
        }))
        .map_err(|error| format!("the {method} call could not be formed: {error}"))?;
        let outcome = self.manager.call_instance(request).await?;
        if outcome.generation != self.generation {
            // The supervisor restarted a crashed worker for this call; the
            // worker this bridge talked to is gone and nothing is replayed.
            return Ok(Reply::Exited(outcome.exit));
        }
        Ok(match outcome.status {
            ExtensionCallStatus::Ok => Reply::Ok(outcome.result.unwrap_or(Value::Null)),
            ExtensionCallStatus::Failed => {
                let (code, message) = outcome
                    .error
                    .map(|error| (error.code, error.message))
                    .unwrap_or_else(|| ("failed".into(), String::new()));
                Reply::Failed { code, message }
            }
            ExtensionCallStatus::DeadlineExceeded => Reply::Deadline,
            ExtensionCallStatus::Busy => Reply::Busy,
            _ => Reply::Exited(outcome.exit),
        })
    }

    /// A busy instance took nothing, so the call is tried again briefly.
    async fn call_retrying(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Reply, String> {
        let mut attempts = 0;
        loop {
            match self.call(method, params.clone(), timeout).await {
                Ok(Reply::Busy) if attempts < BUSY_ATTEMPTS => {
                    attempts += 1;
                    tokio::time::sleep(BUSY_RETRY).await;
                }
                other => return other,
            }
        }
    }

    /// Parameters larger than a frame are uploaded first. A failed upload
    /// means the call itself was never made (`Err`).
    async fn call_large(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Reply, String> {
        let encoded = params.to_string();
        if encoded.len() <= hosted::INLINE_PARAMS_BYTES {
            return self.call_retrying(method, params, timeout).await;
        }
        if encoded.len() > hosted::MAX_UPLOAD_BYTES {
            return Err(format!(
                "the {method} parameters are larger than {} MiB",
                hosted::MAX_UPLOAD_BYTES / (1024 * 1024)
            ));
        }
        let upload = format!("upload-{}", uuid::Uuid::new_v4());
        let parts = hosted::split_text(&encoded, hosted::UPLOAD_CHUNK_BYTES);
        let count = parts.len();
        for (index, part) in parts.iter().enumerate() {
            let chunk = json!({"uploadId": upload, "index": index, "count": count, "data": part});
            let reply = self
                .call_retrying(hosted::UPLOAD, chunk, CALL_TIMEOUT)
                .await?;
            if !matches!(reply, Reply::Ok(_)) {
                return Err(format!(
                    "the {method} parameters did not reach the worker: {}",
                    self.failure(hosted::UPLOAD, reply)
                ));
            }
        }
        self.call_retrying(method, json!({"upload": upload}), timeout)
            .await
    }

    fn failure(&self, method: &str, reply: Reply) -> String {
        let label = &self.label;
        match reply {
            Reply::Ok(_) => format!("{label}'s worker answered {method} unexpectedly"),
            Reply::Failed { code, message } => format!(
                "{label}'s worker refused {method} ({code}): {}",
                clip(message, MAX_MESSAGE_BYTES)
            ),
            Reply::Deadline => format!("{label}'s worker did not answer {method} in time"),
            Reply::Exited(exit) => format!(
                "{label}'s worker exited during {method}{}",
                exit_detail(exit.as_ref())
            ),
            Reply::Busy => format!("{label}'s worker was busy and did not take {method}"),
        }
    }

    fn usable(&self) -> bool {
        !self.closed.load(Ordering::SeqCst) && lock(&self.gone).is_none()
    }

    /// Usable, and the supervisor still runs this exact worker generation.
    fn alive(&self) -> bool {
        self.usable()
            && self.manager.instances().is_ok_and(|snapshot| {
                snapshot.instances.iter().any(|view| {
                    view.instance_id == self.instance_id
                        && view.generation == self.generation
                        && view.state == ExtensionInstanceState::Running
                })
            })
    }

    /// Stop the instance: every process in its group ends with it.
    async fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Err(error) = self.manager.stop_instance(&self.instance_id).await {
            tracing::debug!(instance = %self.instance_id, %error, "hosted provider instance already gone");
        }
    }

    fn register(self: &Arc<Self>, job: &str) -> mpsc::Receiver<Item> {
        let (sender, receiver) = mpsc::channel(JOB_BUFFER);
        lock(&self.jobs).insert(job.to_owned(), sender);
        self.ensure_pump();
        receiver
    }

    fn unregister(&self, job: &str) {
        lock(&self.jobs).remove(job);
    }

    fn ensure_pump(self: &Arc<Self>) {
        let mut pumping = lock(&self.pumping);
        if *pumping || self.closed.load(Ordering::SeqCst) {
            return;
        }
        *pumping = true;
        let worker = self.clone();
        tokio::spawn(async move { worker.pump().await });
    }

    /// Read the worker's event queue while any job waits on it.
    async fn pump(self: Arc<Self>) {
        let mut deadline_streak = 0_u32;
        loop {
            {
                let mut pumping = lock(&self.pumping);
                if lock(&self.jobs).is_empty() || self.closed.load(Ordering::SeqCst) {
                    *pumping = false;
                    return;
                }
            }
            let after = lock(&self.pump).cursor;
            let params = json!({"after": after, "waitMs": POLL_WAIT.as_millis() as u64});
            match self
                .call(hosted::EVENTS, params, POLL_WAIT + POLL_SLACK)
                .await
            {
                Ok(Reply::Ok(page)) => {
                    deadline_streak = 0;
                    if let Err(reason) = self.apply_page(page).await {
                        return self.fail(reason).await;
                    }
                }
                Ok(Reply::Deadline) => {
                    deadline_streak += 1;
                    if deadline_streak >= MAX_DEADLINE_STREAK {
                        return self.fail("the worker stopped answering".into()).await;
                    }
                }
                Ok(Reply::Busy) => tokio::time::sleep(BUSY_RETRY).await,
                Ok(Reply::Exited(exit)) => {
                    let reason = format!("the worker exited{}", exit_detail(exit.as_ref()));
                    return self.fail(reason).await;
                }
                Ok(Reply::Failed { code, message }) => {
                    let reason = format!(
                        "the worker refused its event request ({code}): {}",
                        clip(message, MAX_MESSAGE_BYTES)
                    );
                    return self.fail(reason).await;
                }
                Err(error) => return self.fail(error).await,
            }
        }
    }

    async fn apply_page(&self, mut page: Value) -> Result<(), String> {
        let events = match page.get_mut("events").map(Value::take) {
            Some(Value::Array(events)) => events,
            _ => return Err("the worker sent an invalid events page".into()),
        };
        for raw in events {
            let seq = raw
                .get("seq")
                .and_then(Value::as_u64)
                .ok_or("the worker sent an event without a sequence number")?;
            let item = {
                let mut pump = lock(&self.pump);
                if seq <= pump.cursor {
                    // Already applied: a repeated page after a lost reply.
                    continue;
                }
                if seq != pump.cursor + 1 {
                    return Err(format!(
                        "the worker skipped events {} to {}",
                        pump.cursor + 1,
                        seq - 1
                    ));
                }
                pump.cursor = seq;
                if raw.get("type").and_then(Value::as_str) == Some("fragment") {
                    Self::fragment(&mut pump, &raw)?
                } else {
                    Some(raw)
                }
            };
            if let Some(item) = item {
                self.route(item).await;
            }
        }
        Ok(())
    }

    /// One fragment; the whole item once its last fragment arrived.
    fn fragment(pump: &mut PumpState, raw: &Value) -> Result<Option<Value>, String> {
        let number = |key: &str| {
            raw.get(key)
                .and_then(Value::as_u64)
                .map(|value| value as usize)
        };
        let (Some(index), Some(count), Some(data)) = (
            number("index"),
            number("count"),
            raw.get("data").and_then(Value::as_str),
        ) else {
            return Err("the worker sent an invalid fragment".into());
        };
        if count == 0
            || count > hosted::MAX_FRAGMENTS
            || index >= count
            || index != pump.fragments.len()
        {
            return Err("the worker sent fragments out of order".into());
        }
        pump.fragments.push(data.to_owned());
        if pump.fragments.len() < count {
            return Ok(None);
        }
        let text = pump.fragments.concat();
        pump.fragments.clear();
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| format!("the worker sent a fragmented item LUNA cannot read: {error}"))
    }

    async fn route(&self, item: Value) {
        let Some(job) = item.get("job").and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        let sender = lock(&self.jobs).get(&job).cloned();
        if let Some(sender) = sender
            && sender.send(Item::Event(item)).await.is_err()
        {
            self.unregister(&job);
        }
    }

    /// The worker is gone or broke the profile: every job hears why, and
    /// its group is stopped.
    async fn fail(&self, reason: String) {
        *lock(&self.gone) = Some(reason.clone());
        let senders: Vec<mpsc::Sender<Item>> =
            lock(&self.jobs).drain().map(|(_, sender)| sender).collect();
        *lock(&self.pumping) = false;
        for sender in senders {
            let _ = sender.send(Item::Gone(reason.clone())).await;
        }
        self.close().await;
    }

    async fn start_job(
        self: &Arc<Self>,
        operation: &str,
        params: Value,
    ) -> Result<Job, StartError> {
        if let Some(reason) = lock(&self.gone).clone() {
            return Err(StartError::NotSent(format!("{}: {reason}", self.label)));
        }
        let id = format!("job-{}", uuid::Uuid::new_v4());
        let receiver = self.register(&id);
        let job = Job {
            id: id.clone(),
            receiver,
            worker: self.clone(),
        };
        let start = json!({"job": id, "op": operation, "params": params});
        match self
            .call_large(hosted::JOB_START, start, CALL_TIMEOUT)
            .await
        {
            Ok(Reply::Ok(_)) => Ok(job),
            Ok(Reply::Failed { code, message }) if code == "rejected" || code == "badRequest" => {
                Err(StartError::NotSent(format!(
                    "{}'s worker refused {operation}: {}",
                    self.label,
                    clip(message, MAX_MESSAGE_BYTES)
                )))
            }
            Ok(Reply::Busy) => Err(StartError::NotSent(format!(
                "{}'s worker was busy; nothing was sent",
                self.label
            ))),
            Err(error) => Err(StartError::NotSent(format!("{}: {error}", self.label))),
            Ok(reply) => {
                let failure = self.failure(hosted::JOB_START, reply);
                let _ = self
                    .call(hosted::JOB_CANCEL, json!({"job": id}), STOP_CALL_TIMEOUT)
                    .await;
                Err(StartError::Unknown(failure))
            }
        }
    }

    async fn reply(&self, job: &str, kind: &str, id: &str, payload: Value) {
        let mut params = hosted::object_or_empty(&payload);
        params.insert("job".into(), json!(job));
        params.insert("kind".into(), json!(kind));
        params.insert("id".into(), json!(id));
        match self
            .call_large(hosted::JOB_REPLY, Value::Object(params), CALL_TIMEOUT)
            .await
        {
            Ok(Reply::Ok(_)) => {}
            Ok(reply) => {
                tracing::debug!(failure = %self.failure(hosted::JOB_REPLY, reply), "a hosted reply was not taken")
            }
            Err(error) => tracing::debug!(%error, "a hosted reply was not sent"),
        }
    }
}

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

/// Why a worker could not be opened.
enum OpenError {
    /// The provider reports it cannot run here (its CLI is missing).
    Unavailable(String),
    Failed(String),
}

impl OpenError {
    fn message(self) -> String {
        match self {
            Self::Unavailable(message) | Self::Failed(message) => message,
        }
    }

    fn into_harness(self) -> HarnessError {
        match self {
            Self::Unavailable(message) => HarnessError::NotInstalled(message),
            Self::Failed(message) => HarnessError::Protocol(message),
        }
    }
}

/// What the provider and its run tasks share.
struct Shared {
    id: HarnessId,
    label: String,
    /// Root provider session → the worker of its live run.
    live: Mutex<HashMap<String, Weak<Worker>>>,
    /// Adapter state carried from one run's worker to the next.
    state: Mutex<Map<String, Value>>,
    observations: Mutex<HashMap<String, SessionMcpObservation>>,
    /// The last `describe`'s availability, and when it was seen.
    available: Mutex<Option<(bool, Instant)>>,
    /// When a worker last reported LUNA tool delivery withdrawn.
    withdrawn: Mutex<Option<Instant>>,
}

impl Shared {
    fn note_delivery(&self, reported: RuntimeDelivery) {
        if reported == RuntimeDelivery::Unsupported {
            *lock(&self.withdrawn) = Some(Instant::now());
        }
    }

    fn delivery(&self, declared: RuntimeDelivery) -> RuntimeDelivery {
        let withdrawn = lock(&self.withdrawn).is_some_and(|at| at.elapsed() < DELIVERY_WITHDRAWAL);
        if withdrawn {
            RuntimeDelivery::Unsupported
        } else {
            declared
        }
    }

    /// The run imported `imported`; what it exported replaces them, and an
    /// imported key it no longer exports was dropped by the adapter.
    fn merge_state(&self, imported: &[String], exported: Value) {
        let Value::Object(exported) = exported else {
            return;
        };
        let mut state = lock(&self.state);
        for key in imported {
            if !exported.contains_key(key) {
                state.remove(key);
            }
        }
        for (key, value) in exported {
            if state.len() < MAX_STATE_KEYS || state.contains_key(&key) {
                state.insert(key, value);
            }
        }
    }

    fn observe(&self, observation: SessionMcpObservation) {
        let mut observations = lock(&self.observations);
        if observations.len() >= MAX_OBSERVATIONS
            && !observations.contains_key(&observation.session_id)
        {
            let oldest = observations
                .values()
                .min_by_key(|seen| seen.observed_at_ms)
                .map(|seen| seen.session_id.clone());
            if let Some(oldest) = oldest {
                observations.remove(&oldest);
            }
        }
        observations.insert(observation.session_id.clone(), observation);
    }

    fn live_worker(&self, session: &str) -> Option<Arc<Worker>> {
        lock(&self.live)
            .get(session)
            .and_then(Weak::upgrade)
            .filter(|worker| worker.usable())
    }
}

pub(super) struct HostedProvider {
    extension_id: ExtensionId,
    version: String,
    package_hash: String,
    runtime: ExtensionRuntimeKind,
    declaration: ProviderDeclaration,
    hosted: HostedDeclaration,
    manager: Weak<Inner>,
    /// The exact package this provider runs; part of catalog provenance.
    source: String,
    shared: Arc<Shared>,
    last_describe: Mutex<Option<DescribeRecord>>,
    service: tokio::sync::Mutex<Option<Arc<Worker>>>,
}

impl Drop for HostedProvider {
    fn drop(&mut self) {
        let Some(worker) = self.service.get_mut().take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { worker.close().await });
        }
    }
}

impl HostedProvider {
    /// `None` unless `declaration` is a provider profile 2.0 declaration.
    pub(super) fn new(
        id: HarnessId,
        extension_id: ExtensionId,
        version: String,
        package_hash: String,
        runtime: ExtensionRuntimeKind,
        declaration: ProviderDeclaration,
        manager: Weak<Inner>,
    ) -> Option<Self> {
        let hosted = declaration.hosted.clone()?;
        let source = format!(
            "extension:{}@{version}#{package_hash}",
            extension_id.as_str()
        );
        let shared = Arc::new(Shared {
            id,
            label: declaration.display_name.clone(),
            live: Mutex::new(HashMap::new()),
            state: Mutex::new(Map::new()),
            observations: Mutex::new(HashMap::new()),
            available: Mutex::new(None),
            withdrawn: Mutex::new(None),
        });
        Some(Self {
            extension_id,
            version,
            package_hash,
            runtime,
            declaration,
            hosted,
            manager,
            source,
            shared,
            last_describe: Mutex::new(None),
            service: tokio::sync::Mutex::new(None),
        })
    }

    pub(super) fn extension_id(&self) -> &ExtensionId {
        &self.extension_id
    }

    pub(super) fn version(&self) -> &str {
        &self.version
    }

    pub(super) fn runtime_kind(&self) -> ExtensionRuntimeKind {
        self.runtime
    }

    pub(super) fn last_describe(&self) -> Option<DescribeRecord> {
        lock(&self.last_describe).clone()
    }

    fn label(&self) -> &str {
        &self.shared.label
    }

    fn manager(&self) -> Option<ExtensionManager> {
        self.manager.upgrade().map(ExtensionManager)
    }

    /// Start a verified instance that advertises every profile operation,
    /// and check its description.
    async fn open_worker(&self) -> Result<Arc<Worker>, OpenError> {
        let label = self.label().to_owned();
        let manager = self
            .manager()
            .ok_or_else(|| OpenError::Failed(format!("{label}: the engine is shutting down")))?;
        let request: StartExtensionInstanceRequest = serde_json::from_value(json!({
            "extensionId": self.extension_id,
            "packageHash": self.package_hash,
            "require": hosted::OPERATIONS,
        }))
        .map_err(|error| {
            OpenError::Failed(format!("the provider start could not be formed: {error}"))
        })?;
        let view = manager.start_instance(request).await.map_err(|error| {
            OpenError::Failed(format!("{label} could not start its worker: {error}"))
        })?;
        let worker = Arc::new(Worker::new(
            manager,
            view.instance_id,
            view.generation,
            label,
        ));
        if let Err(error) = self.describe(&worker).await {
            worker.close().await;
            return Err(error);
        }
        Ok(worker)
    }

    async fn describe(&self, worker: &Worker) -> Result<(), OpenError> {
        let mut record = DescribeRecord {
            at_ms: chrono::Utc::now().timestamp_millis(),
            conforms: false,
            available: None,
            profile: None,
            executable: None,
            detail: None,
        };
        let result = match worker
            .call(hosted::DESCRIBE, json!({}), DESCRIBE_TIMEOUT)
            .await
        {
            Ok(Reply::Ok(value)) => self.check_description(&value, &mut record),
            Ok(reply) => Err(OpenError::Failed(worker.failure(hosted::DESCRIBE, reply))),
            Err(error) => Err(OpenError::Failed(error)),
        };
        match &result {
            Ok(()) => record.conforms = true,
            Err(OpenError::Unavailable(message)) => {
                record.conforms = true;
                record.detail = Some(message.clone());
            }
            Err(OpenError::Failed(message)) => record.detail = Some(message.clone()),
        }
        *lock(&self.last_describe) = Some(record);
        result
    }

    fn check_description(
        &self,
        value: &Value,
        record: &mut DescribeRecord,
    ) -> Result<(), OpenError> {
        let label = self.label();
        let part = |pointer: &str| {
            value
                .pointer(pointer)
                .and_then(Value::as_u64)
                .map_or(0, |part| part as u32)
        };
        let (major, minor) = (part("/profile/major"), part("/profile/minor"));
        record.profile = Some(ProfileVersion { major, minor });
        if major != hosted::PROFILE_MAJOR {
            return Err(OpenError::Failed(format!(
                "{label}'s worker speaks provider profile {major}.{minor}; its package declares profile {}.x",
                hosted::PROFILE_MAJOR
            )));
        }
        let described = value
            .get("providerId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if HarnessId::parse(described).ok() != Some(self.shared.id) {
            return Err(OpenError::Failed(format!(
                "{label}'s worker describes provider {}, not {}",
                clip(described.to_owned(), 256),
                self.shared.id
            )));
        }
        let available = value
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        record.available = Some(available);
        record.executable = value
            .get("executable")
            .cloned()
            .and_then(|executable| serde_json::from_value::<Executable>(executable).ok());
        *lock(&self.shared.available) = Some((available, Instant::now()));
        if let Some(delivery) = value
            .get("runtimeDelivery")
            .cloned()
            .and_then(|delivery| serde_json::from_value::<RuntimeDelivery>(delivery).ok())
        {
            self.shared.note_delivery(delivery);
        }
        if !available {
            let reason = value
                .get("unavailableReason")
                .and_then(Value::as_str)
                .map(|reason| clip(reason.to_owned(), MAX_MESSAGE_BYTES))
                .filter(|reason| !reason.trim().is_empty())
                .unwrap_or_else(|| "it gave no reason".into());
            return Err(OpenError::Unavailable(format!(
                "{label} is unavailable on this device: {reason}"
            )));
        }
        Ok(())
    }

    /// The service instance, started on first use and replaced when its
    /// worker is gone.
    async fn service_worker(&self) -> Result<Arc<Worker>, HarnessError> {
        let mut slot = self.service.lock().await;
        if let Some(worker) = slot.as_ref()
            && worker.alive()
        {
            return Ok(worker.clone());
        }
        if let Some(stale) = slot.take() {
            stale.close().await;
        }
        let worker = self.open_worker().await.map_err(OpenError::into_harness)?;
        *slot = Some(worker.clone());
        Ok(worker)
    }

    /// One job on `worker`, answering its progress reports, until its
    /// terminal `done`.
    async fn op_on(
        &self,
        worker: &Arc<Worker>,
        operation: &str,
        params: Value,
        on_progress: &mut ProgressSink<'_>,
    ) -> Result<Value, HarnessError> {
        let label = self.label().to_owned();
        let mut job = worker
            .start_job(operation, params)
            .await
            .map_err(|error| HarnessError::Protocol(error.into_message()))?;
        loop {
            match job.next().await {
                Some(Item::Event(mut event)) => match event_kind(&event).as_str() {
                    "done" => {
                        return if done_ok(&event) {
                            Ok(event
                                .get_mut("result")
                                .map(Value::take)
                                .unwrap_or(Value::Null))
                        } else {
                            Err(hosted::decode_error(
                                event.get("error").unwrap_or(&Value::Null),
                            ))
                        };
                    }
                    "progress" => {
                        let outcome = on_progress(event.get("progress").unwrap_or(&Value::Null));
                        if let Some(ack) = event.get("ack").and_then(Value::as_str) {
                            let payload = match &outcome {
                                Ok(()) => json!({"ok": true}),
                                Err(error) => {
                                    json!({"ok": false, "error": hosted::encode_error(error)})
                                }
                            };
                            worker.reply(&job.id, "progress", ack, payload).await;
                        }
                    }
                    _ => {}
                },
                Some(Item::Gone(reason)) => {
                    return Err(HarnessError::Protocol(format!(
                        "{label}: {reason} during {operation}; the outcome is unknown and LUNA does not repeat it"
                    )));
                }
                None => {
                    return Err(HarnessError::Protocol(format!(
                        "{label}'s worker stopped reporting during {operation}; the outcome is unknown and LUNA does not repeat it"
                    )));
                }
            }
        }
    }

    async fn service_op(&self, operation: &str, params: Value) -> Result<Value, HarnessError> {
        let worker = self.service_worker().await?;
        self.op_on(&worker, operation, params, &mut |_| Ok(()))
            .await
    }

    async fn discover(&self, live: bool) -> Result<Vec<Model>, HarnessError> {
        let mut result = self.service_op(op::MODELS, json!({"live": live})).await?;
        let models = result
            .get_mut("models")
            .map(Value::take)
            .unwrap_or(Value::Null);
        serde_json::from_value(models).map_err(|error| {
            HarnessError::Protocol(format!("{}'s model list is invalid: {error}", self.label()))
        })
    }

    fn unsupported(message: &str) -> HarnessError {
        HarnessError::Protocol(message.into())
    }

    async fn start_run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let label = self.label().to_owned();
        let mode = request
            .permission_policy
            .as_ref()
            .map_or(request.permission_mode, |policy| policy.effective_mode);
        if !self.declaration.supports_mode(mode) {
            return Err(HarnessError::RejectedBeforeAcceptance(format!(
                "{label} does not offer {}; switch this chat to Full access",
                mode_label(mode)
            )));
        }
        let encoded = serde_json::to_value(&request).map_err(|error| {
            HarnessError::RejectedBeforeAcceptance(format!(
                "{label}: the request could not be encoded: {error}"
            ))
        })?;
        let worker = self
            .open_worker()
            .await
            .map_err(|error| HarnessError::RejectedBeforeAcceptance(error.message()))?;
        let state = lock(&self.shared.state).clone();
        let imported: Vec<String> = state.keys().cloned().collect();
        let params = json!({
            "request": encoded,
            "lunaTools": controls.invoke_luna_tool.is_some(),
            "state": Value::Object(state),
        });
        let mut job = match worker.start_job(op::RUN, params).await {
            Ok(job) => job,
            Err(StartError::NotSent(reason)) => {
                worker.close().await;
                return Err(HarnessError::RejectedBeforeAcceptance(reason));
            }
            Err(StartError::Unknown(reason)) => {
                worker.close().await;
                return Err(HarnessError::Protocol(format!(
                    "{reason}; the prompt may have reached {label}, so LUNA does not send it again"
                )));
            }
        };
        // The adapter's own `run` decides acceptance: `started` once it
        // returned its stream, or its exact error.
        let mut early = Vec::new();
        loop {
            match job.next().await {
                Some(Item::Event(event)) => match event_kind(&event).as_str() {
                    "started" => break,
                    "done" => {
                        worker.close().await;
                        return Err(if done_ok(&event) {
                            HarnessError::Protocol(format!(
                                "{label} ended the run before starting it"
                            ))
                        } else {
                            hosted::decode_error(event.get("error").unwrap_or(&Value::Null))
                        });
                    }
                    _ => early.push(event),
                },
                Some(Item::Gone(reason)) => {
                    worker.close().await;
                    return Err(HarnessError::Protocol(format!(
                        "{label}: {reason} before confirming the run; the prompt may have reached {label}, so LUNA does not send it again"
                    )));
                }
                None => {
                    worker.close().await;
                    return Err(HarnessError::Protocol(format!(
                        "{label}'s worker stopped reporting before confirming the run; the prompt may have reached {label}, so LUNA does not send it again"
                    )));
                }
            }
        }
        let RunControls {
            request_input,
            steering,
            interrupt,
            invoke_luna_tool,
        } = controls;
        let acks: SteerAcks = Arc::default();
        let steer_task = tokio::spawn(forward_steering(
            worker.clone(),
            job.id.clone(),
            steering,
            acks.clone(),
            label,
        ));
        let (tx, rx) = mpsc::channel(STREAM_BUFFER);
        let pump = RunPump {
            shared: self.shared.clone(),
            job_id: job.id.clone(),
            worker,
            tx,
            request_input,
            invoke_luna_tool,
            interrupt,
            mode,
            imported,
            acks,
            session_id: None,
            done_seen: false,
            stop_deadline: None,
            tasks: Vec::new(),
        };
        tokio::spawn(async move {
            pump.run(job, early, steer_task).await;
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })
        .boxed())
    }
}

#[async_trait]
impl Harness for HostedProvider {
    fn id(&self) -> HarnessId {
        self.shared.id
    }
    /// The declared delivery, withdrawn for a while after a run's worker
    /// reported it could not verify it.
    fn luna_runtime_delivery(&self) -> RuntimeDelivery {
        self.shared.delivery(self.hosted.runtime_delivery)
    }
    fn display_name(&self) -> &str {
        &self.declaration.display_name
    }
    fn supports_steering(&self) -> bool {
        self.hosted.steering_mode.is_some()
    }
    fn steering_mode(&self) -> SteeringMode {
        self.hosted
            .steering_mode
            .unwrap_or(SteeringMode::TurnBoundary)
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &self.declaration.reasoning_levels
    }
    /// Its package is still the enabled one in this profile (a file read),
    /// and no description in the last minute reported the CLI missing.
    fn installed(&self) -> bool {
        let missing = lock(&self.shared.available)
            .is_some_and(|(available, at)| !available && at.elapsed() < UNAVAILABLE_RECHECK);
        !missing
            && self.manager().is_some_and(|manager| {
                manager.provider_package_enabled(self.extension_id.as_str(), &self.package_hash)
            })
    }
    fn model_catalog_source(&self) -> String {
        self.source.clone()
    }
    fn deterministic_turn_end(&self) -> bool {
        self.hosted.deterministic_turn_end
    }
    fn supports_safe_recovery_continuation(&self) -> bool {
        self.hosted.safe_recovery
    }
    fn fork_capability(&self) -> ProviderForkCapability {
        self.hosted.fork
    }
    async fn fork_session(
        &self,
        request: ProviderForkRequest,
    ) -> Result<ProviderForkResult, HarnessError> {
        self.fork_session_staged(request, &|_| Ok(())).await
    }
    /// Each progress report is confirmed to the worker before its adapter
    /// continues, exactly as the in-process reporter returns.
    async fn fork_session_staged(
        &self,
        request: ProviderForkRequest,
        progress: &ProviderForkProgressReporter,
    ) -> Result<ProviderForkResult, HarnessError> {
        if !self.hosted.offers(HostedOperation::Fork) {
            return Err(Self::unsupported(
                "provider-backed session forks are not supported by this harness",
            ));
        }
        let params = json!({"request": hosted::encode_fork_request(&request)});
        let mut report = |value: &Value| -> Result<(), HarnessError> {
            let reported = hosted::decode_fork_progress(value).map_err(HarnessError::Protocol)?;
            progress(reported)
        };
        let worker = self.service_worker().await?;
        let result = self.op_on(&worker, op::FORK, params, &mut report).await?;
        hosted::decode_fork_result(result.get("result").unwrap_or(&Value::Null))
            .map_err(HarnessError::Protocol)
    }
    async fn native_history_turns(
        &self,
        scope: NativeHistoryScope,
    ) -> Result<NativeHistoryTurns, HarnessError> {
        if !self.hosted.offers(HostedOperation::History) {
            return Err(Self::unsupported(
                "native history readback is not supported by this harness",
            ));
        }
        let result = self
            .service_op(
                op::HISTORY,
                json!({"scope": hosted::encode_history_scope(&scope)}),
            )
            .await?;
        hosted::decode_history_turns(result.get("turns").unwrap_or(&Value::Null))
            .map_err(HarnessError::Protocol)
    }
    async fn rewind_native_conversation(
        &self,
        request: NativeRewindRequest,
        progress: &NativeRewindProgressReporter,
    ) -> Result<NativeRewindOutcome, HarnessError> {
        if !self.hosted.offers(HostedOperation::Rewind) {
            return Err(Self::unsupported(
                "same-session native rewind is not supported by this harness",
            ));
        }
        let params = json!({"request": hosted::encode_rewind_request(&request)});
        let mut report = |_value: &Value| -> Result<(), HarnessError> {
            progress(NativeRewindProgress::RequestWritten);
            Ok(())
        };
        let worker = self.service_worker().await?;
        let result = self.op_on(&worker, op::REWIND, params, &mut report).await?;
        hosted::decode_rewind_outcome(result.get("outcome").unwrap_or(&Value::Null))
            .map_err(HarnessError::Protocol)
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.discover(false).await
    }
    async fn models_live(&self) -> Result<Vec<Model>, HarnessError> {
        self.discover(true).await
    }
    async fn commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        if !self.hosted.offers(HostedOperation::Commands) {
            return Ok(Vec::new());
        }
        let mut result = self
            .service_op(op::COMMANDS, json!({"scope": null}))
            .await?;
        serde_json::from_value(
            result
                .get_mut("commands")
                .map(Value::take)
                .unwrap_or(Value::Null),
        )
        .map_err(|error| HarnessError::Protocol(format!("the command list is invalid: {error}")))
    }
    async fn commands_in(&self, scope: &CommandScope) -> Result<Vec<SlashCommand>, HarnessError> {
        if !self.hosted.offers(HostedOperation::Commands) {
            return Ok(Vec::new());
        }
        let mut result = self
            .service_op(
                op::COMMANDS,
                json!({"scope": hosted::encode_command_scope(scope)}),
            )
            .await?;
        serde_json::from_value(
            result
                .get_mut("commands")
                .map(Value::take)
                .unwrap_or(Value::Null),
        )
        .map_err(|error| HarnessError::Protocol(format!("the command list is invalid: {error}")))
    }
    async fn mcp_control(
        &self,
        request: McpControlRequest,
    ) -> Result<McpControlReply, HarnessError> {
        if !self.hosted.offers(HostedOperation::Mcp) {
            return Err(Self::unsupported(
                "provider MCP control is not supported by this harness",
            ));
        }
        let result = self
            .service_op(
                op::MCP,
                json!({"request": hosted::encode_mcp_request(&request)}),
            )
            .await?;
        hosted::decode_mcp_reply(result.get("reply").unwrap_or(&Value::Null))
            .map_err(HarnessError::Protocol)
    }
    fn session_mcp_observation(&self, provider_session_id: &str) -> Option<SessionMcpObservation> {
        lock(&self.shared.observations)
            .get(provider_session_id)
            .cloned()
    }
    async fn native_extensions(
        &self,
        cwd: Option<&std::path::Path>,
        mode: PermissionMode,
        refresh: bool,
    ) -> Result<NativeExtensionsView, HarnessError> {
        if !self.hosted.offers(HostedOperation::Extensions) {
            return Err(Self::unsupported(
                "native extension inventory is unsupported by this harness",
            ));
        }
        let cwd = cwd.map(|cwd| cwd.to_string_lossy().into_owned());
        let mut result = self
            .service_op(
                op::EXTENSIONS,
                json!({"cwd": cwd, "mode": mode, "refresh": refresh}),
            )
            .await?;
        serde_json::from_value(
            result
                .get_mut("view")
                .map(Value::take)
                .unwrap_or(Value::Null),
        )
        .map_err(|error| {
            HarnessError::Protocol(format!("the extension inventory is invalid: {error}"))
        })
    }
    /// A chat with a live run compacts through that run's worker (the
    /// adapter steers the item into its own server); otherwise the service
    /// worker boots a server for the session, as the adapter does.
    async fn compact_context(&self, request: ContextCompactRequest) -> Result<(), HarnessError> {
        if !self.hosted.offers(HostedOperation::Compact) {
            return Err(Self::unsupported(
                "context compaction is not supported by this harness",
            ));
        }
        let worker = match self.shared.live_worker(&request.session_id) {
            Some(worker) => worker,
            None => self.service_worker().await?,
        };
        let params = json!({"request": hosted::encode_compact_request(&request)});
        self.op_on(&worker, op::COMPACT, params, &mut |_| Ok(()))
            .await
            .map(|_| ())
    }
    fn supports_native_child_stop(&self) -> bool {
        self.hosted.native_child_stop
    }
    /// The live run's own worker stops the child; there is no other server
    /// that knows it.
    async fn stop_native_child(
        &self,
        native_session_id: &str,
        spawn_tool_use_id: &str,
    ) -> Result<NativeTaskStop, HarnessError> {
        if !self.hosted.native_child_stop {
            return Err(Self::unsupported(
                "stopping a native child is not supported by this harness",
            ));
        }
        let label = self.label();
        let Some(worker) = self.shared.live_worker(native_session_id) else {
            return Ok(NativeTaskStop::Rejected(format!(
                "No live {label} run owns this conversation."
            )));
        };
        let params = json!({"sessionId": native_session_id, "childId": spawn_tool_use_id});
        match worker.call(hosted::CHILD_STOP, params, CALL_TIMEOUT).await {
            Ok(Reply::Ok(answer)) => {
                if done_ok(&answer) {
                    hosted::decode_task_stop(answer.get("stop").unwrap_or(&Value::Null))
                        .map_err(HarnessError::Protocol)
                } else {
                    Err(hosted::decode_error(
                        answer.get("error").unwrap_or(&Value::Null),
                    ))
                }
            }
            Ok(Reply::Busy) => Ok(NativeTaskStop::Rejected(format!(
                "{label}'s worker was busy; the stop was not sent"
            ))),
            Ok(reply) => Ok(NativeTaskStop::Unknown(
                worker.failure(hosted::CHILD_STOP, reply),
            )),
            Err(error) => Ok(NativeTaskStop::Rejected(format!("{label}: {error}"))),
        }
    }
    async fn run_title(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        if !self.hosted.offers(HostedOperation::Title) {
            return Err(Self::unsupported(
                "title generation is not supported by this harness",
            ));
        }
        let encoded = serde_json::to_value(&request).map_err(|error| {
            HarnessError::Protocol(format!("the title request could not be encoded: {error}"))
        })?;
        let worker = self.service_worker().await?;
        let mut job = worker
            .start_job(op::TITLE, json!({"request": encoded}))
            .await
            .map_err(|error| HarnessError::Protocol(error.into_message()))?;
        let job_id = job.id.clone();
        let interrupt = controls.interrupt.clone();
        let label = self.label().to_owned();
        let mut done = loop {
            let item = tokio::select! {
                item = job.next() => item,
                () = interrupt.cancelled() => {
                    let _ = worker
                        .call(hosted::JOB_CANCEL, json!({"job": job_id}), STOP_CALL_TIMEOUT)
                        .await;
                    return Err(HarnessError::Protocol("title generation cancelled".into()));
                }
            };
            match item {
                Some(Item::Event(event)) if event_kind(&event) == "done" => break event,
                Some(Item::Event(_)) => {}
                Some(Item::Gone(reason)) => {
                    return Err(HarnessError::Protocol(format!(
                        "{label}: {reason} during title generation"
                    )));
                }
                None => {
                    return Err(HarnessError::Protocol(format!(
                        "{label}'s worker stopped reporting during title generation"
                    )));
                }
            }
        };
        if !done_ok(&done) {
            return Err(hosted::decode_error(
                done.get("error").unwrap_or(&Value::Null),
            ));
        }
        let events = done
            .pointer_mut("/result/events")
            .map(Value::take)
            .unwrap_or(Value::Null);
        let events: Vec<AgentEvent> = serde_json::from_value(events).map_err(|error| {
            HarnessError::Protocol(format!("{label}'s title events are invalid: {error}"))
        })?;
        Ok(futures::stream::iter(events.into_iter().map(Ok)).boxed())
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.start_run(request, controls).await
    }
}

// ---------------------------------------------------------------------------
// One run
// ---------------------------------------------------------------------------

/// Forward the engine's steer messages, in order, one call at a time. The
/// adapter's own acceptance comes back as a `steerAck` event.
async fn forward_steering(
    worker: Arc<Worker>,
    job: String,
    mut steering: mpsc::Receiver<SteerMessage>,
    acks: SteerAcks,
    label: String,
) {
    while let Some(message) = steering.recv().await {
        let SteerMessage {
            prompt,
            attachments,
            input_items,
            message_id,
            delivery,
            acceptance,
            goal,
        } = message;
        let steer = format!("steer-{}", uuid::Uuid::new_v4());
        let wants = acceptance.is_some();
        if let Some(sender) = acceptance {
            lock(&acks).insert(steer.clone(), sender);
        }
        let params = json!({
            "job": job,
            "steer": steer,
            "prompt": prompt,
            "attachments": attachments,
            "inputItems": input_items,
            "messageId": message_id,
            "delivery": hosted::encode_steer_delivery(delivery),
            "goal": goal,
            "acceptance": wants,
        });
        let settled = match worker
            .call_large(hosted::RUN_STEER, params, CALL_TIMEOUT)
            .await
        {
            Ok(Reply::Ok(answer))
                if answer.get("delivered").and_then(Value::as_bool) == Some(true) =>
            {
                None
            }
            Ok(Reply::Ok(_)) => Some(SteerAcceptance::Rejected(format!(
                "{label} is no longer taking messages in this turn"
            ))),
            Ok(Reply::Failed { code, message }) if code == "unknownJob" || code == "badRequest" => {
                Some(SteerAcceptance::Rejected(format!(
                    "{label}: {}",
                    clip(message, MAX_MESSAGE_BYTES)
                )))
            }
            Err(error) => Some(SteerAcceptance::Rejected(format!("{label}: {error}"))),
            Ok(reply) => Some(SteerAcceptance::Unknown(
                worker.failure(hosted::RUN_STEER, reply),
            )),
        };
        if let Some(acceptance) = settled
            && let Some(sender) = lock(&acks).remove(&steer)
        {
            let _ = sender.send(acceptance);
        }
    }
}

/// Drives one accepted run until its stream ends.
struct RunPump {
    shared: Arc<Shared>,
    worker: Arc<Worker>,
    job_id: String,
    tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    request_input: InputRequester,
    invoke_luna_tool: Option<LunaToolExecutor>,
    interrupt: CancellationToken,
    mode: PermissionMode,
    /// The state keys this run started with.
    imported: Vec<String>,
    acks: SteerAcks,
    session_id: Option<String>,
    /// The adapter's own `Done` was forwarded.
    done_seen: bool,
    /// Set once LUNA asked the run to stop.
    stop_deadline: Option<tokio::time::Instant>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl RunPump {
    async fn run(
        mut self,
        mut job: Job,
        early: Vec<Value>,
        steer_task: tokio::task::JoinHandle<()>,
    ) {
        let mut finished = false;
        for event in early {
            if self.apply(event).await {
                finished = true;
                break;
            }
        }
        let interrupt = self.interrupt.clone();
        let closed = self.tx.clone();
        while !finished {
            let deadline = self.stop_deadline;
            let stopping = deadline.is_some();
            let never = tokio::time::Instant::now() + Duration::from_secs(24 * 3600);
            tokio::select! {
                item = job.next() => match item {
                    Some(Item::Event(event)) => finished = self.apply(event).await,
                    Some(Item::Gone(reason)) => {
                        self.worker_gone(reason).await;
                        finished = true;
                    }
                    None => {
                        self.worker_gone("the worker stopped reporting".into()).await;
                        finished = true;
                    }
                },
                () = interrupt.cancelled(), if !stopping => self.request_stop(),
                () = closed.closed(), if !stopping => self.request_stop(),
                () = tokio::time::sleep_until(deadline.unwrap_or(never)), if stopping => {
                    self.unconfirmed_stop().await;
                    finished = true;
                }
            }
        }
        steer_task.abort();
        for task in self.tasks.drain(..) {
            task.abort();
        }
        if let Some(session) = &self.session_id {
            let mut live = lock(&self.shared.live);
            if live
                .get(session)
                .is_some_and(|worker| worker.as_ptr() == Arc::as_ptr(&self.worker))
            {
                live.remove(session);
            }
        }
        self.worker.close().await;
        drop(job);
    }

    async fn emit(&self, item: Result<AgentEvent, HarnessError>) {
        // A dropped stream is noticed by the loop, which then stops the run.
        let _ = self.tx.send(item).await;
    }

    /// The bridge's own end of the run. Its reason is the turn's terminal
    /// error, shown once, on this `Done`: an `Error` event before it would
    /// be a second, identical error part.
    async fn done(&mut self, status: DoneStatus, error: String) {
        self.done_seen = true;
        let session_id = self.session_id.clone();
        self.emit(Ok(AgentEvent::Done {
            status,
            result: None,
            error: Some(error),
            session_id,
        }))
        .await;
    }

    /// `true` once the run's job ended.
    async fn apply(&mut self, mut event: Value) -> bool {
        match event_kind(&event).as_str() {
            "agent" => {
                let raw = event
                    .get_mut("event")
                    .map(Value::take)
                    .unwrap_or(Value::Null);
                match serde_json::from_value::<AgentEvent>(raw) {
                    Ok(agent) => {
                        if let AgentEvent::SessionStarted { session_id, .. } = &agent {
                            self.note_session(session_id);
                        }
                        if matches!(agent, AgentEvent::Done { .. }) {
                            self.done_seen = true;
                        }
                        self.emit(Ok(agent)).await;
                        false
                    }
                    Err(error) => {
                        let reason = format!(
                            "{} sent a run event this LUNA cannot read: {error}",
                            self.shared.label
                        );
                        self.conformance(reason).await;
                        true
                    }
                }
            }
            "streamError" => {
                let error = hosted::decode_error(event.get("error").unwrap_or(&Value::Null));
                self.emit(Err(error)).await;
                false
            }
            "input" => {
                self.on_input(event).await;
                false
            }
            "tool" => {
                self.on_tool(event);
                false
            }
            "steerAck" => {
                self.on_steer_ack(&event);
                false
            }
            "observation" => {
                if let Ok(observation) =
                    hosted::decode_session_mcp(event.get("mcp").unwrap_or(&Value::Null))
                {
                    self.shared.observe(observation);
                }
                false
            }
            "done" => {
                self.on_done(event).await;
                true
            }
            _ => false,
        }
    }

    fn note_session(&mut self, session_id: &str) {
        if self.session_id.is_some() {
            return;
        }
        self.session_id = Some(session_id.to_owned());
        lock(&self.shared.live).insert(session_id.to_owned(), Arc::downgrade(&self.worker));
    }

    fn spawn_reply(&mut self, kind: &'static str, id: String, payload: Value) {
        let worker = self.worker.clone();
        let job = self.job_id.clone();
        self.tasks.push(tokio::spawn(async move {
            worker.reply(&job, kind, &id, payload).await;
        }));
    }

    /// A question or approval card from the adapter. In Full and Auto an
    /// approval never reaches a person: it is declined, visibly.
    async fn on_input(&mut self, mut event: Value) {
        let Some(request) = event
            .get("request")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return;
        };
        let launch_urls: Vec<Option<String>> = event
            .get("launchUrls")
            .and_then(Value::as_array)
            .map(|urls| {
                urls.iter()
                    .map(|url| url.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let raw = event
            .get_mut("questions")
            .map(Value::take)
            .unwrap_or(Value::Null);
        let mut questions: Vec<UserInputQuestion> = match serde_json::from_value(raw) {
            Ok(questions) => questions,
            Err(error) => {
                let message = format!(
                    "{} sent a question this LUNA cannot read: {error}",
                    self.shared.label
                );
                self.emit(Ok(AgentEvent::Error { message })).await;
                self.spawn_reply("input", request, json!({"cancelled": true}));
                return;
            }
        };
        for (question, url) in questions.iter_mut().zip(launch_urls) {
            if let (Some(mcp), Some(url)) = (question.mcp.as_mut(), url) {
                mcp.launch_url = Some(url);
            }
        }
        if matches!(self.mode, PermissionMode::Full | PermissionMode::Auto)
            && questions.iter().any(|question| question.approval.is_some())
        {
            let message = format!(
                "{} asked a person to approve an action in a {} chat, which never asks; LUNA declined it.",
                self.shared.label,
                mode_label(self.mode)
            );
            self.emit(Ok(AgentEvent::Error { message })).await;
            self.spawn_reply("input", request, json!({"cancelled": true}));
            return;
        }
        let answered = (self.request_input)(questions);
        let worker = self.worker.clone();
        let job = self.job_id.clone();
        self.tasks.push(tokio::spawn(async move {
            let payload = match answered.await {
                Ok(answers) => json!({"answers": answers}),
                // Dismissed, or the run is ending: the adapter sees its
                // sender dropped, as it would in process.
                Err(_) => json!({"cancelled": true}),
            };
            worker.reply(&job, "input", &request, payload).await;
        }));
    }

    /// A LUNA tool call the adapter matched to its live thread and turn.
    fn on_tool(&mut self, mut event: Value) {
        let Some(call) = event.get("call").and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        let name = event
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let arguments = event
            .get_mut("arguments")
            .map(Value::take)
            .unwrap_or(Value::Null);
        let context = hosted::decode_tool_context(event.get("context").unwrap_or(&Value::Null));
        let executor = self.invoke_luna_tool.clone();
        let worker = self.worker.clone();
        let job = self.job_id.clone();
        let label = self.shared.label.clone();
        self.tasks.push(tokio::spawn(async move {
            let result = match (executor, context) {
                (Some(executor), Ok(context)) => {
                    executor(LunaToolInvocation {
                        name,
                        arguments,
                        context,
                    })
                    .await
                }
                (None, _) => Err(HarnessError::Tool(format!(
                    "LUNA tools are not attached to this {label} run"
                ))),
                (Some(_), Err(error)) => Err(HarnessError::Protocol(format!(
                    "{label} sent a tool call without its context: {error}"
                ))),
            };
            let payload = match result {
                Ok(value) => json!({"ok": true, "result": value}),
                Err(error) => json!({"ok": false, "error": hosted::encode_error(&error)}),
            };
            worker.reply(&job, "tool", &call, payload).await;
        }));
    }

    fn on_steer_ack(&mut self, event: &Value) {
        let Some(steer) = event.get("steer").and_then(Value::as_str) else {
            return;
        };
        let Some(sender) = lock(&self.acks).remove(steer) else {
            return;
        };
        match hosted::decode_steer_acceptance(event.get("acceptance").unwrap_or(&Value::Null)) {
            Ok(Some(acceptance)) => {
                let _ = sender.send(acceptance);
            }
            // The adapter dropped it unanswered: so does LUNA.
            Ok(None) => drop(sender),
            Err(error) => {
                let _ = sender.send(SteerAcceptance::Unknown(format!(
                    "{}: {error}",
                    self.shared.label
                )));
            }
        }
    }

    async fn on_done(&mut self, mut event: Value) {
        if done_ok(&event) {
            let mut result = event
                .get_mut("result")
                .map(Value::take)
                .unwrap_or(Value::Null);
            if let Some(state) = result.get_mut("state").map(Value::take) {
                self.shared.merge_state(&self.imported, state);
            }
            if let Some(delivery) = result
                .get("runtimeDelivery")
                .cloned()
                .and_then(|delivery| serde_json::from_value::<RuntimeDelivery>(delivery).ok())
            {
                self.shared.note_delivery(delivery);
            }
            return;
        }
        // The job ended without its stream's end (LUNA cancelled it).
        if self.done_seen {
            return;
        }
        let message = error_text(&hosted::decode_error(
            event.get("error").unwrap_or(&Value::Null),
        ));
        let status = if self.stop_deadline.is_some() {
            DoneStatus::Interrupted
        } else {
            DoneStatus::Errored
        };
        self.done(status, message).await;
    }

    fn request_stop(&mut self) {
        if self.stop_deadline.is_some() {
            return;
        }
        self.stop_deadline = Some(tokio::time::Instant::now() + STOP_CONFIRM);
        let worker = self.worker.clone();
        let job = self.job_id.clone();
        self.tasks.push(tokio::spawn(async move {
            let _ = worker
                .call(hosted::RUN_STOP, json!({"job": job}), STOP_CALL_TIMEOUT)
                .await;
        }));
    }

    /// No end of the run in time: end the worker's group ourselves.
    async fn unconfirmed_stop(&mut self) {
        self.worker.close().await;
        if !self.done_seen {
            let note = format!(
                "{} did not confirm the stop within {} s; LUNA stopped its worker and every process in its group",
                self.shared.label,
                STOP_CONFIRM.as_secs()
            );
            self.done(DoneStatus::Interrupted, note).await;
        }
    }

    /// The worker exited (or broke the profile) before the run's end.
    async fn worker_gone(&mut self, reason: String) {
        if self.done_seen {
            return;
        }
        let label = self.shared.label.clone();
        if self.stop_deadline.is_some() {
            self.done(
                DoneStatus::Interrupted,
                format!("{label}: {reason} while stopping"),
            )
            .await;
            return;
        }
        let message = format!("{label}: {reason} during the run; the turn was not replayed");
        self.done(DoneStatus::Errored, message).await;
    }

    async fn conformance(&mut self, reason: String) {
        let _ = self
            .worker
            .call(
                hosted::JOB_CANCEL,
                json!({"job": self.job_id}),
                STOP_CALL_TIMEOUT,
            )
            .await;
        self.worker.close().await;
        if !self.done_seen {
            self.done(DoneStatus::Errored, reason).await;
        }
    }
}
