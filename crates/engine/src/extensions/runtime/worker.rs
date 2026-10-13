//! One worker process generation: launch and handshake, the frame pumps,
//! calls with deadlines and cancellation, host-call dispatch to the broker,
//! resource sampling, and reaping.
//!
//! Tasks per worker: a writer (bounded queue to stdin), a stderr drain into
//! the bounded log window, a frame reader, a resource sampler and a monitor.
//! The monitor observes the leader's exit without reaping it, kills whatever
//! is left of the group while the group id is still reserved, reaps, fails
//! the pending calls (never replaying them) and reports the exit once.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use luna_proto::{
    EXTENSION_API, EXTENSION_WASM_PROFILE, EXTENSION_WORKER_PROTOCOL, ExtensionCallError,
    ExtensionContractVersion, ExtensionExitReason, ExtensionInstanceLimits, ExtensionInstanceUsage,
    ExtensionLogLine, ExtensionLogSource, ExtensionPortableInfo, ExtensionTrust,
    ExtensionWorkerExit,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::mpsc::error::{SendTimeoutError, TrySendError};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use super::broker::{BoundTarget, Broker, BrokerError, Principal};
use super::process::{self, Signal};
use super::protocol::{
    self, CallFrame, CancelFrame, FRAME_LIMIT, FrameError, HelloFrame, HelloInstance, HelloLimits,
    HelloPortable, HelloTarget, HostResultFrame, Inbound, NAME_LIMIT, ShutdownFrame, bounded_text,
};

/// The error code a worker uses when it stops a call at its `deadlineMs`.
const DEADLINE_EXCEEDED: &str = "deadlineExceeded";
pub(super) const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const MAX_CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a cancelled call has to be acknowledged.
const CANCEL_GRACE: Duration = Duration::from_millis(1000);
/// Each step of a graceful stop: shutdown frame and end of input, then
/// `SIGTERM`, then `SIGKILL`.
const STOP_GRACE: Duration = Duration::from_millis(500);
/// How long a caller waits for an exit to be recorded.
const EXIT_RECORD_WAIT: Duration = Duration::from_secs(5);
/// How long the monitor waits for the rest of the group to disappear.
const GROUP_GONE_WAIT: Duration = Duration::from_secs(1);
/// How long the monitor waits for the rest of stderr after the group is gone.
const STDERR_TAIL_WAIT: Duration = Duration::from_millis(500);
pub(super) const MAX_CONCURRENT_CALLS: usize = 8;
const MAX_CONCURRENT_HOST_CALLS: usize = 16;
const HOST_CALL_TIMEOUT: Duration = Duration::from_secs(5);
const HOST_CALL_BURST: u32 = 100;
const HOST_CALLS_PER_SECOND: u32 = 50;
const FRAME_BURST: u32 = 4096;
const FRAMES_PER_SECOND: u32 = 2048;
const LOG_LINES: usize = 256;
const LOG_BYTES: usize = 64 * 1024;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
const OUTBOUND_QUEUE: usize = 64;
/// A worker that does not read its input for this long while the host has a
/// frame for it is stopped (`unresponsive`).
const WRITE_STALL: Duration = Duration::from_secs(2);
const MAX_OPERATIONS: usize = 256;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn add(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

/// The resource limits of one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Limits {
    pub memory_bytes: u64,
    pub cpu_seconds: u64,
    pub max_processes: u32,
    pub open_files: u64,
    pub handshake_timeout: Duration,
    pub guest_memory_bytes: Option<u64>,
    pub guest_fuel_per_call: Option<u64>,
}

impl Limits {
    pub fn view(&self) -> ExtensionInstanceLimits {
        ExtensionInstanceLimits {
            memory_bytes: self.memory_bytes,
            cpu_seconds: self.cpu_seconds,
            max_processes: self.max_processes,
            open_files: self.open_files,
            frame_bytes: FRAME_LIMIT as u32,
            handshake_timeout_ms: self.handshake_timeout.as_millis() as u64,
            default_call_timeout_ms: DEFAULT_CALL_TIMEOUT.as_millis() as u64,
            max_call_timeout_ms: MAX_CALL_TIMEOUT.as_millis() as u64,
            cancel_grace_ms: CANCEL_GRACE.as_millis() as u64,
            max_concurrent_calls: MAX_CONCURRENT_CALLS as u32,
            max_concurrent_host_calls: MAX_CONCURRENT_HOST_CALLS as u32,
            host_call_timeout_ms: HOST_CALL_TIMEOUT.as_millis() as u64,
            host_call_burst: HOST_CALL_BURST,
            host_calls_per_second: HOST_CALLS_PER_SECOND,
            frame_burst: FRAME_BURST,
            frames_per_second: FRAMES_PER_SECOND,
            log_lines: LOG_LINES as u32,
            log_bytes: LOG_BYTES as u32,
            guest_memory_bytes: self.guest_memory_bytes,
            guest_fuel_per_call: self.guest_fuel_per_call,
        }
    }
}

/// Counters for one instance, across its worker generations.
#[derive(Default)]
pub(super) struct Usage {
    pub calls: AtomicU64,
    pub calls_failed: AtomicU64,
    pub calls_deadline_exceeded: AtomicU64,
    pub calls_busy: AtomicU64,
    host_calls: AtomicU64,
    host_calls_denied: AtomicU64,
    host_calls_busy: AtomicU64,
    frames: AtomicU64,
    log_lines: AtomicU64,
    log_lines_evicted: AtomicU64,
    stderr_bytes: AtomicU64,
    sampled: AtomicBool,
    resident_bytes: AtomicU64,
    peak_resident_bytes: AtomicU64,
    processes: AtomicU32,
    pub restarts: AtomicU32,
    pub faults: AtomicU32,
}

impl Usage {
    pub fn count(counter: &AtomicU64) {
        add(counter);
    }

    fn record_sample(&self, usage: process::GroupUsage) {
        self.resident_bytes
            .store(usage.resident_bytes, Ordering::Relaxed);
        self.peak_resident_bytes
            .fetch_max(usage.resident_bytes, Ordering::Relaxed);
        self.processes.store(usage.processes, Ordering::Relaxed);
        self.sampled.store(true, Ordering::Relaxed);
    }

    pub fn view(&self, host_calls_in_flight: u32) -> ExtensionInstanceUsage {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let sampled = self.sampled.load(Ordering::Relaxed);
        ExtensionInstanceUsage {
            calls: load(&self.calls),
            calls_failed: load(&self.calls_failed),
            calls_deadline_exceeded: load(&self.calls_deadline_exceeded),
            calls_busy: load(&self.calls_busy),
            host_calls: load(&self.host_calls),
            host_calls_denied: load(&self.host_calls_denied),
            host_calls_busy: load(&self.host_calls_busy),
            host_calls_in_flight,
            frames: load(&self.frames),
            log_lines: load(&self.log_lines),
            log_lines_evicted: load(&self.log_lines_evicted),
            stderr_bytes: load(&self.stderr_bytes),
            resident_bytes: sampled.then(|| load(&self.resident_bytes)),
            peak_resident_bytes: sampled.then(|| load(&self.peak_resident_bytes)),
            processes: sampled.then(|| self.processes.load(Ordering::Relaxed)),
            restarts: self.restarts.load(Ordering::Relaxed),
            faults: self.faults.load(Ordering::Relaxed),
        }
    }
}

/// The retained log window: the most recent lines, bounded by count and
/// bytes. Older lines are evicted and counted.
#[derive(Default)]
pub(super) struct LogRing {
    lines: VecDeque<ExtensionLogLine>,
    bytes: usize,
}

impl LogRing {
    fn push(&mut self, line: ExtensionLogLine, usage: &Usage) {
        let size = line.message.len();
        while !self.lines.is_empty()
            && (self.lines.len() >= LOG_LINES || self.bytes + size > LOG_BYTES)
        {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.message.len();
                add(&usage.log_lines_evicted);
            }
        }
        self.bytes += size;
        self.lines.push_back(line);
        add(&usage.log_lines);
    }

    pub fn snapshot(&self) -> Vec<ExtensionLogLine> {
        self.lines.iter().cloned().collect()
    }

    fn last_stderr(&self, generation: u32) -> Option<String> {
        self.lines
            .iter()
            .rev()
            .find(|line| line.generation == generation && line.source == ExtensionLogSource::Stderr)
            .map(|line| bounded_text(&line.message, 500))
    }
}

/// What every worker generation of one instance shares.
pub(super) struct InstanceShared {
    pub principal: Arc<Principal>,
    pub limits: Limits,
    pub broker: Broker,
    pub usage: Usage,
    pub logs: Mutex<LogRing>,
}

impl InstanceShared {
    pub fn new(principal: Arc<Principal>, limits: Limits, broker: Broker) -> Self {
        Self {
            principal,
            limits,
            broker,
            usage: Usage::default(),
            logs: Mutex::new(LogRing::default()),
        }
    }

    fn log(
        &self,
        generation: u32,
        source: ExtensionLogSource,
        level: Option<String>,
        message: String,
    ) {
        lock(&self.logs).push(
            ExtensionLogLine {
                at_ms: now_ms(),
                generation,
                source,
                level,
                message,
            },
            &self.usage,
        );
    }

    fn stderr_line(&self, generation: u32, line: &[u8], truncated: bool) {
        let mut message = String::from_utf8_lossy(line).into_owned();
        if truncated {
            message.push('…');
        }
        self.log(generation, ExtensionLogSource::Stderr, None, message);
    }
}

struct TokenBucket {
    capacity: f64,
    per_second: f64,
    tokens: f64,
    refilled: Instant,
}

impl TokenBucket {
    fn new(capacity: u32, per_second: u32) -> Self {
        Self {
            capacity: f64::from(capacity),
            per_second: f64::from(per_second),
            tokens: f64::from(capacity),
            refilled: Instant::now(),
        }
    }

    fn take(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.refilled).as_secs_f64();
        self.refilled = now;
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// How to start one worker process.
pub(crate) struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
    /// Operations the caller needs the worker to advertise.
    pub required: Vec<String>,
    /// Portable only: the module SHA-256 the host must report.
    pub expected_module_sha256: Option<String>,
}

/// What the worker said in its hello.
#[derive(Debug, Clone)]
pub(super) struct Handshake {
    pub protocol: ExtensionContractVersion,
    pub operations: Vec<String>,
    pub portable: Option<ExtensionPortableInfo>,
}

/// A call still waiting for its answer when the worker exited: the worker
/// may or may not have acted on it (CORE-19).
#[derive(Debug, Clone)]
pub(super) struct InterruptedCall {
    pub call_id: String,
    pub request_id: Option<String>,
    pub method: String,
    pub started_at_ms: i64,
}

/// Called once per worker generation when its exit has been reaped and
/// classified, before callers waiting on the exit are released. `ready` says
/// whether the worker had completed its handshake; the calls are those it
/// never answered.
pub(super) type ExitHook = Arc<
    dyn Fn(u32, ExtensionWorkerExit, bool, Vec<InterruptedCall>) -> BoxFuture<'static, ()>
        + Send
        + Sync,
>;

pub(super) enum CallResult {
    Ok(Value),
    Failed(ExtensionCallError),
    DeadlineExceeded,
    WorkerExited(Option<ExtensionWorkerExit>),
    Busy(String),
}

type Reply = Result<Value, protocol::WorkerError>;

/// One call sent to the worker and not answered yet.
struct PendingCall {
    reply: oneshot::Sender<Reply>,
    request_id: Option<String>,
    method: String,
    started_at_ms: i64,
}

struct WorkerCore {
    generation: u32,
    pid: u32,
    shared: Arc<InstanceShared>,
    out: mpsc::Sender<Vec<u8>>,
    /// Ends the worker's input once queued frames are written.
    input_closed: CancellationToken,
    pending: Mutex<HashMap<String, PendingCall>>,
    /// Cancelled calls whose acknowledgement is still due.
    cancelled: Mutex<HashSet<String>>,
    host_tasks: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    host_rate: Mutex<TokenBucket>,
    frame_rate: Mutex<TokenBucket>,
    /// Why the supervisor ended the worker; the first reason wins.
    kill_reason: Mutex<Option<(ExtensionExitReason, String)>>,
    /// Set by the monitor once the leader is about to be reaped; after that
    /// the group id may be reused and is never signalled again.
    reaped: Mutex<bool>,
    ready: AtomicBool,
    handshake_done: CancellationToken,
    /// Cancelled when stderr reached its end, so an exit report includes
    /// the worker's last words.
    stderr_done: CancellationToken,
    /// Cancelled when the worker has exited; ends the reader and sampler.
    stop: CancellationToken,
    exit: watch::Receiver<Option<ExtensionWorkerExit>>,
}

impl WorkerCore {
    fn set_reason(&self, reason: ExtensionExitReason, detail: String) {
        let mut slot = lock(&self.kill_reason);
        if slot.is_none() {
            *slot = Some((reason, detail));
        }
    }

    fn signal(&self, signal: Signal) {
        let reaped = lock(&self.reaped);
        if !*reaped {
            process::signal_group(self.pid, signal);
        }
    }

    /// End the worker's whole process group now.
    fn kill(&self, reason: ExtensionExitReason, detail: impl Into<String>) {
        self.set_reason(reason, detail.into());
        self.signal(Signal::Kill);
    }

    async fn wait_exit(&self, timeout: Duration) -> Option<ExtensionWorkerExit> {
        let mut exit = self.exit.clone();
        let recorded = tokio::time::timeout(timeout, exit.wait_for(Option::is_some)).await;
        match recorded {
            Ok(Ok(value)) => value.clone(),
            _ => None,
        }
    }

    async fn send(&self, frame: Vec<u8>) -> bool {
        match self.out.send_timeout(frame, WRITE_STALL).await {
            Ok(()) => true,
            Err(SendTimeoutError::Timeout(_)) => {
                self.kill(
                    ExtensionExitReason::Unresponsive,
                    format!("stopped reading its input for {} s", WRITE_STALL.as_secs()),
                );
                false
            }
            Err(SendTimeoutError::Closed(_)) => false,
        }
    }

    async fn send_host_result(&self, id: &str, result: Result<Value, BrokerError>) -> bool {
        let frame = match &result {
            Ok(value) => protocol::encode(&HostResultFrame {
                kind: "hostResult",
                id,
                ok: true,
                result: Some(value),
                error: None,
            }),
            Err(error) => protocol::encode(&HostResultFrame {
                kind: "hostResult",
                id,
                ok: false,
                result: None,
                error: Some(error),
            }),
        };
        let frame = match frame {
            Ok(frame) => frame,
            Err(detail) => {
                let error = BrokerError {
                    code: "tooLarge",
                    message: detail,
                };
                match protocol::encode(&HostResultFrame {
                    kind: "hostResult",
                    id,
                    ok: false,
                    result: None,
                    error: Some(&error),
                }) {
                    Ok(frame) => frame,
                    Err(_) => return true,
                }
            }
        };
        self.send(frame).await
    }

    /// Withdraw a call the caller gave up on: tell the worker, and stop it
    /// if it does not acknowledge within the grace period.
    fn cancel_call(self: &Arc<Self>, id: &str) {
        if lock(&self.pending).remove(id).is_none() {
            return;
        }
        lock(&self.cancelled).insert(id.to_owned());
        if let Ok(frame) = protocol::encode(&CancelFrame { kind: "cancel", id }) {
            // A full queue is decided by the grace period below.
            let _ = self.out.try_send(frame);
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.kill(
                ExtensionExitReason::Hung,
                "a call was cancelled outside the runtime",
            );
            return;
        };
        let core = self.clone();
        let id = id.to_owned();
        runtime.spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(CANCEL_GRACE) => {}
                () = core.stop.cancelled() => return,
            }
            if lock(&core.cancelled).remove(&id) {
                core.kill(
                    ExtensionExitReason::Hung,
                    format!(
                        "did not acknowledge a cancelled call within {} ms",
                        CANCEL_GRACE.as_millis()
                    ),
                );
            }
        });
    }

    /// Handle one decoded frame; `false` stops reading.
    async fn dispatch(self: &Arc<Self>, inbound: Inbound) -> bool {
        match inbound {
            Inbound::Response { id, outcome } => {
                let waiting = lock(&self.pending).remove(&id);
                if let Some(call) = waiting {
                    let _ = call.reply.send(outcome);
                    return true;
                }
                if lock(&self.cancelled).remove(&id) {
                    return true;
                }
                self.kill(
                    ExtensionExitReason::ProtocolViolation,
                    format!(
                        "answered call {} that is not pending",
                        bounded_text(&id, NAME_LIMIT)
                    ),
                );
                false
            }
            Inbound::HostCall { id, op, params } => self.host_call(id, op, params).await,
            Inbound::RefusedHostCall { id, reason } => {
                let usage = &self.shared.usage;
                add(&usage.host_calls);
                add(&usage.host_calls_denied);
                self.send_host_result(&id, Err(BrokerError::bad_request(reason)))
                    .await
            }
            Inbound::HostCancel { id } => {
                let task = lock(&self.host_tasks).remove(&id);
                if let Some(task) = task {
                    task.abort();
                }
                true
            }
            Inbound::Log { level, message } => {
                self.shared
                    .log(self.generation, ExtensionLogSource::Worker, level, message);
                true
            }
        }
    }

    async fn host_call(self: &Arc<Self>, id: String, op: String, params: Value) -> bool {
        let usage = &self.shared.usage;
        add(&usage.host_calls);
        let refusal = {
            let tasks = lock(&self.host_tasks);
            if tasks.contains_key(&id) {
                Some(BrokerError::bad_request(format!(
                    "host call {} is already running",
                    bounded_text(&id, NAME_LIMIT)
                )))
            } else if tasks.len() >= MAX_CONCURRENT_HOST_CALLS {
                Some(BrokerError::busy(format!(
                    "at most {MAX_CONCURRENT_HOST_CALLS} host calls run at once"
                )))
            } else if !lock(&self.host_rate).take() {
                Some(BrokerError::busy(format!(
                    "more than {HOST_CALL_BURST} host calls in a burst; {HOST_CALLS_PER_SECOND} per second are accepted"
                )))
            } else {
                None
            }
        };
        if let Some(error) = refusal {
            add(if error.code == "busy" {
                &usage.host_calls_busy
            } else {
                &usage.host_calls_denied
            });
            return self.send_host_result(&id, Err(error)).await;
        }
        let core = self.clone();
        let task_id = id.clone();
        let mut tasks = lock(&self.host_tasks);
        let task = tokio::spawn(async move {
            let principal = core.shared.principal.clone();
            let result = match tokio::time::timeout(
                HOST_CALL_TIMEOUT,
                core.shared.broker.handle(principal, op, params),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(BrokerError::timeout(format!(
                    "the host call took longer than {} s",
                    HOST_CALL_TIMEOUT.as_secs()
                ))),
            };
            if result.as_ref().is_err_and(BrokerError::is_refusal) {
                add(&core.shared.usage.host_calls_denied);
            }
            // Removed here, or by a hostCancel first; only one of them answers.
            let wanted = lock(&core.host_tasks).remove(&task_id).is_some();
            if wanted {
                core.send_host_result(&task_id, result).await;
            }
        });
        tasks.insert(id, task.abort_handle());
        true
    }
}

/// One running worker process generation.
pub(super) struct Worker {
    core: Arc<WorkerCore>,
    handshake: Handshake,
}

/// Withdraws its call when dropped while still armed: the caller timed out
/// or went away.
struct CallGuard {
    core: Arc<WorkerCore>,
    id: String,
    armed: bool,
}

impl CallGuard {
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if self.armed {
            self.core.cancel_call(&self.id);
        }
    }
}

impl Worker {
    /// Spawn, supervise and handshake one worker generation. On a failed
    /// handshake the group is killed and reaped before this returns.
    pub async fn launch(
        spec: LaunchSpec,
        shared: Arc<InstanceShared>,
        generation: u32,
        hook: ExitHook,
    ) -> Result<Arc<Worker>, String> {
        let spawn_limits = process::SpawnLimits {
            cpu_seconds: shared.limits.cpu_seconds,
            open_files: shared.limits.open_files,
        };
        let process::Spawned { mut child, pid } = process::spawn(
            &spec.program,
            &spec.args,
            &spec.cwd,
            &spec.environment,
            &spawn_limits,
        )
        .map_err(|error| format!("the worker could not be started: {error}"))?;
        let leader = match process::watch_leader(pid) {
            Ok(leader) => leader,
            Err(error) => {
                process::signal_group(pid, Signal::Kill);
                let _ = child.wait().await;
                return Err(format!("the worker could not be supervised: {error}"));
            }
        };
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            process::signal_group(pid, Signal::Kill);
            let _ = child.wait().await;
            return Err("the worker's stdio could not be connected".into());
        };
        let (out, frames) = mpsc::channel(OUTBOUND_QUEUE);
        let (exit_sender, exit) = watch::channel(None);
        let core = Arc::new(WorkerCore {
            generation,
            pid,
            shared: shared.clone(),
            out,
            input_closed: CancellationToken::new(),
            pending: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(HashSet::new()),
            host_tasks: Mutex::new(HashMap::new()),
            host_rate: Mutex::new(TokenBucket::new(HOST_CALL_BURST, HOST_CALLS_PER_SECOND)),
            frame_rate: Mutex::new(TokenBucket::new(FRAME_BURST, FRAMES_PER_SECOND)),
            kill_reason: Mutex::new(None),
            reaped: Mutex::new(false),
            ready: AtomicBool::new(false),
            handshake_done: CancellationToken::new(),
            stderr_done: CancellationToken::new(),
            stop: CancellationToken::new(),
            exit,
        });
        tokio::spawn(write_frames(stdin, frames, core.input_closed.clone()));
        let stderr_task = tokio::spawn(drain_stderr(
            shared.clone(),
            generation,
            stderr,
            core.stderr_done.clone(),
        ));
        tokio::spawn(monitor(
            core.clone(),
            child,
            leader,
            exit_sender,
            hook,
            stderr_task.abort_handle(),
        ));

        let mut reader = BufReader::new(stdout);
        match handshake(&core, &spec, &mut reader).await {
            Ok(handshake) => {
                core.ready.store(true, Ordering::SeqCst);
                core.handshake_done.cancel();
                tokio::spawn(read_frames(core.clone(), reader));
                if process::GROUP_SAMPLING {
                    tokio::spawn(sample(core.clone()));
                }
                Ok(Arc::new(Worker { core, handshake }))
            }
            Err(detail) => {
                core.kill(ExtensionExitReason::HandshakeFailed, detail.clone());
                core.handshake_done.cancel();
                drop(reader);
                let exit = core.wait_exit(EXIT_RECORD_WAIT).await;
                let stderr = lock(&shared.logs).last_stderr(generation);
                let mut message = format!("the worker failed its handshake: {detail}");
                if let Some(stderr) = stderr {
                    message.push_str(&format!("; its last stderr line: {stderr}"));
                }
                if exit.is_some_and(|exit| !exit.group_cleared) {
                    message.push_str("; part of its process group was still running");
                }
                Err(message)
            }
        }
    }

    pub fn generation(&self) -> u32 {
        self.core.generation
    }

    pub fn pid(&self) -> u32 {
        self.core.pid
    }

    pub fn handshake(&self) -> &Handshake {
        &self.handshake
    }

    pub fn host_calls_in_flight(&self) -> u32 {
        lock(&self.core.host_tasks).len() as u32
    }

    pub fn has_exited(&self) -> bool {
        self.core.stop.is_cancelled()
    }

    /// Calls sent and not answered yet.
    pub fn calls_in_flight(&self) -> usize {
        lock(&self.core.pending).len()
    }

    /// Whether the worker advertised `operation` in its hello.
    pub fn advertises(&self, operation: &str) -> bool {
        self.handshake
            .operations
            .iter()
            .any(|advertised| advertised == operation)
    }

    /// Send one call (`id` is the frame id the worker sees) and wait for its
    /// answer, the deadline or the worker's exit. A deadline cancels the
    /// call; nothing is ever retried.
    pub async fn call(
        &self,
        id: &str,
        request_id: Option<&str>,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> CallResult {
        let core = &self.core;
        if core.stop.is_cancelled() {
            return CallResult::WorkerExited(core.wait_exit(EXIT_RECORD_WAIT).await);
        }
        let id = id.to_owned();
        let frame = match protocol::encode(&CallFrame {
            kind: "call",
            id: &id,
            method,
            params,
            deadline_ms: timeout.as_millis() as u64,
        }) {
            Ok(frame) => frame,
            Err(detail) => {
                return CallResult::Failed(ExtensionCallError {
                    code: "tooLarge".into(),
                    message: detail,
                });
            }
        };
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = lock(&core.pending);
            if pending.len() >= MAX_CONCURRENT_CALLS {
                return CallResult::Busy(format!(
                    "this instance already has {MAX_CONCURRENT_CALLS} calls in progress"
                ));
            }
            pending.insert(
                id.clone(),
                PendingCall {
                    reply: sender,
                    request_id: request_id.map(str::to_owned),
                    method: method.to_owned(),
                    started_at_ms: now_ms(),
                },
            );
        }
        let guard = CallGuard {
            core: core.clone(),
            id: id.clone(),
            armed: true,
        };
        match core.out.try_send(frame) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                lock(&core.pending).remove(&id);
                guard.disarm();
                return CallResult::Busy("the worker's input queue is full".into());
            }
            Err(TrySendError::Closed(_)) => {
                lock(&core.pending).remove(&id);
                guard.disarm();
                return CallResult::WorkerExited(core.wait_exit(EXIT_RECORD_WAIT).await);
            }
        }
        tokio::select! {
            reply = receiver => {
                guard.disarm();
                match reply {
                    Ok(Ok(value)) => CallResult::Ok(value),
                    // A worker that honoured the frame's `deadlineMs` itself
                    // reports the same outcome as the supervisor's timer, so
                    // native and portable calls share one status contract.
                    Ok(Err(error)) if error.code == DEADLINE_EXCEEDED => {
                        CallResult::DeadlineExceeded
                    }
                    Ok(Err(error)) => CallResult::Failed(ExtensionCallError {
                        code: error.code,
                        message: error.message,
                    }),
                    Err(_) => CallResult::WorkerExited(core.wait_exit(EXIT_RECORD_WAIT).await),
                }
            }
            () = tokio::time::sleep(timeout) => {
                // Still armed: dropping it cancels the call.
                drop(guard);
                CallResult::DeadlineExceeded
            }
        }
    }

    /// Stop gracefully: a shutdown frame and the end of its input, then
    /// `SIGTERM`, then `SIGKILL` for the whole group.
    pub async fn stop(&self, reason: ExtensionExitReason) -> Option<ExtensionWorkerExit> {
        let core = &self.core;
        core.set_reason(reason, describe(reason).into());
        if let Ok(frame) = protocol::encode(&ShutdownFrame { kind: "shutdown" }) {
            let _ = core.out.try_send(frame);
        }
        core.input_closed.cancel();
        if let Some(exit) = core.wait_exit(STOP_GRACE).await {
            return Some(exit);
        }
        core.signal(Signal::Terminate);
        if let Some(exit) = core.wait_exit(STOP_GRACE).await {
            return Some(exit);
        }
        core.signal(Signal::Kill);
        core.wait_exit(EXIT_RECORD_WAIT).await
    }

    /// Stop without waiting; kills immediately outside a runtime.
    pub fn stop_in_background(self: &Arc<Self>, reason: ExtensionExitReason) {
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                let worker = self.clone();
                runtime.spawn(async move {
                    worker.stop(reason).await;
                });
            }
            Err(_) => self.kill_now(reason),
        }
    }

    /// Kill the group immediately.
    pub fn kill_now(&self, reason: ExtensionExitReason) {
        self.core.kill(reason, describe(reason));
    }
}

fn describe(reason: ExtensionExitReason) -> &'static str {
    match reason {
        ExtensionExitReason::Stopped => "stopped by the owner",
        ExtensionExitReason::Shutdown => "stopped because the engine shut down",
        ExtensionExitReason::Revoked => {
            "stopped because the package was disabled or its grants changed"
        }
        ExtensionExitReason::Quarantined => "stopped because the package was quarantined",
        ExtensionExitReason::Updating => "stopped so the extension's enabled version could change",
        ExtensionExitReason::SafeMode => "stopped because safe mode was turned on",
        _ => "stopped by the supervisor",
    }
}

async fn handshake(
    core: &WorkerCore,
    spec: &LaunchSpec,
    reader: &mut BufReader<ChildStdout>,
) -> Result<Handshake, String> {
    let principal = &core.shared.principal;
    let limits = &core.shared.limits;
    let trust = principal.trust();
    let id = uuid::Uuid::new_v4().to_string();
    let root = match &principal.target {
        BoundTarget::Task { root, .. } if trust == ExtensionTrust::Native => {
            Some(root.to_string_lossy().into_owned())
        }
        _ => None,
    };
    let target = match &principal.target {
        BoundTarget::None => HelloTarget::None,
        BoundTarget::Task { task_id, .. } => HelloTarget::Task {
            task_id,
            root: root.as_deref(),
        },
    };
    let hello = HelloFrame {
        kind: "hello",
        id: &id,
        protocol: EXTENSION_WORKER_PROTOCOL,
        api: EXTENSION_API,
        instance: HelloInstance {
            id: &principal.instance_id,
            extension_id: principal.extension_id.as_str(),
            version: &principal.version,
            package_hash: &principal.package_hash,
            trust,
            grants: &principal.grants,
            target,
        },
        limits: HelloLimits {
            frame_bytes: FRAME_LIMIT as u32,
            max_concurrent_calls: MAX_CONCURRENT_CALLS as u32,
            max_concurrent_host_calls: MAX_CONCURRENT_HOST_CALLS as u32,
            host_call_timeout_ms: HOST_CALL_TIMEOUT.as_millis() as u64,
            memory_bytes: limits.memory_bytes,
            cpu_seconds: limits.cpu_seconds,
        },
        required: &spec.required,
        portable: (trust == ExtensionTrust::Portable).then(|| HelloPortable {
            guest_memory_bytes: limits.guest_memory_bytes.unwrap_or(0),
            fuel_per_call: limits.guest_fuel_per_call.unwrap_or(0),
        }),
    };
    let bytes = protocol::encode(&hello)?;
    if core.out.send(bytes).await.is_err() {
        return Err("the worker closed its input before the handshake".into());
    }
    let frame =
        match tokio::time::timeout(limits.handshake_timeout, protocol::read_frame(reader)).await {
            Err(_) => {
                return Err(format!(
                    "no hello within {} ms",
                    limits.handshake_timeout.as_millis()
                ));
            }
            Ok(Ok(Some(frame))) => frame,
            Ok(Ok(None)) => return Err("the worker exited before its hello".into()),
            Ok(Err(FrameError::TooLarge)) => {
                return Err(format!(
                    "the hello is larger than {} KiB",
                    FRAME_LIMIT / 1024
                ));
            }
            Ok(Err(FrameError::Io(error))) => {
                return Err(format!("the hello could not be read: {error}"));
            }
        };
    let reply = protocol::decode_hello(&frame)?;
    if reply.kind != "hello" {
        return Err(format!(
            "the first frame is {:?}, not a hello",
            bounded_text(&reply.kind, NAME_LIMIT)
        ));
    }
    if reply.id != id {
        return Err("the hello answers a different request id".into());
    }
    if reply.protocol.major != EXTENSION_WORKER_PROTOCOL.major {
        return Err(format!(
            "the worker speaks protocol {}.{}; this LUNA speaks {}.x",
            reply.protocol.major, reply.protocol.minor, EXTENSION_WORKER_PROTOCOL.major
        ));
    }
    if reply.extension_id != principal.extension_id.as_str()
        || reply.package_hash != principal.package_hash
    {
        return Err("the worker answered for a different package".into());
    }
    if reply.operations.len() > MAX_OPERATIONS
        || reply
            .operations
            .iter()
            .any(|operation| operation.is_empty() || operation.len() > NAME_LIMIT)
    {
        return Err(format!(
            "the worker advertised more than {MAX_OPERATIONS} operations or an invalid name"
        ));
    }
    if let Some(missing) = spec.required.iter().find(|required| {
        !reply
            .operations
            .iter()
            .any(|operation| operation == *required)
    }) {
        return Err(format!(
            "the worker does not implement the required operation {missing}"
        ));
    }
    let portable = if trust == ExtensionTrust::Portable {
        let Some(portable) = reply.portable else {
            return Err("the portable host did not describe the module it compiled".into());
        };
        if portable.profile.major != EXTENSION_WASM_PROFILE.major {
            return Err(format!(
                "the portable host runs profile {}.{}; this LUNA needs {}.x",
                portable.profile.major, portable.profile.minor, EXTENSION_WASM_PROFILE.major
            ));
        }
        let expected = spec.expected_module_sha256.as_deref().unwrap_or_default();
        if portable.module_sha256 != expected {
            return Err(format!(
                "the portable host compiled module {}, not the pinned {expected}",
                bounded_text(&portable.module_sha256, 64)
            ));
        }
        Some(ExtensionPortableInfo {
            profile: portable.profile,
            runtime: bounded_text(&portable.runtime, 64),
            module_sha256: portable.module_sha256,
            imports: portable
                .imports
                .iter()
                .take(64)
                .map(|import| bounded_text(import, NAME_LIMIT))
                .collect(),
        })
    } else {
        None
    };
    Ok(Handshake {
        protocol: ExtensionContractVersion {
            major: reply.protocol.major,
            minor: reply.protocol.minor.min(EXTENSION_WORKER_PROTOCOL.minor),
        },
        operations: reply.operations,
        portable,
    })
}

async fn write_frames(
    mut stdin: ChildStdin,
    mut frames: mpsc::Receiver<Vec<u8>>,
    closed: CancellationToken,
) {
    loop {
        let frame = tokio::select! {
            biased;
            frame = frames.recv() => match frame {
                Some(frame) => frame,
                None => break,
            },
            () = closed.cancelled() => break,
        };
        if stdin.write_all(&frame).await.is_err() || stdin.flush().await.is_err() {
            break;
        }
    }
    // Dropping stdin ends the worker's input: protocol 1.0 workers exit on EOF.
}

async fn drain_stderr(
    shared: Arc<InstanceShared>,
    generation: u32,
    mut stderr: ChildStderr,
    done: CancellationToken,
) {
    let mut chunk = vec![0_u8; 8 * 1024];
    let mut line = Vec::with_capacity(256);
    let mut truncated = false;
    loop {
        let read = match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        shared
            .usage
            .stderr_bytes
            .fetch_add(read as u64, Ordering::Relaxed);
        for &byte in &chunk[..read] {
            if byte == b'\n' {
                shared.stderr_line(generation, &line, truncated);
                line.clear();
                truncated = false;
            } else if line.len() < protocol::LOG_LINE_LIMIT {
                line.push(byte);
            } else {
                truncated = true;
            }
        }
    }
    if !line.is_empty() {
        shared.stderr_line(generation, &line, truncated);
    }
    done.cancel();
}

async fn read_frames(core: Arc<WorkerCore>, mut reader: BufReader<ChildStdout>) {
    loop {
        let frame = tokio::select! {
            () = core.stop.cancelled() => return,
            frame = protocol::read_frame(&mut reader) => frame,
        };
        let bytes = match frame {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(FrameError::Io(_)) => return,
            Err(FrameError::TooLarge) => {
                core.kill(
                    ExtensionExitReason::ProtocolViolation,
                    format!("sent a frame larger than {} KiB", FRAME_LIMIT / 1024),
                );
                return;
            }
        };
        add(&core.shared.usage.frames);
        if !lock(&core.frame_rate).take() {
            core.kill(
                ExtensionExitReason::Flood,
                format!(
                    "sent more than {FRAME_BURST} frames in a burst; {FRAMES_PER_SECOND} per second are accepted"
                ),
            );
            return;
        }
        let inbound = match protocol::decode(&bytes) {
            Ok(inbound) => inbound,
            Err(detail) => {
                core.kill(ExtensionExitReason::ProtocolViolation, detail);
                return;
            }
        };
        if !core.dispatch(inbound).await {
            return;
        }
    }
}

async fn sample(core: Arc<WorkerCore>) {
    let limits = core.shared.limits.clone();
    let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = core.stop.cancelled() => return,
            _ = ticker.tick() => {}
        }
        let Some(usage) = process::group_usage(core.pid) else {
            continue;
        };
        core.shared.usage.record_sample(usage);
        if usage.processes > limits.max_processes {
            core.kill(
                ExtensionExitReason::ProcessLimit,
                format!(
                    "its process group reached {} processes; the limit is {}",
                    usage.processes, limits.max_processes
                ),
            );
            return;
        }
        if usage.resident_bytes > limits.memory_bytes {
            core.kill(
                ExtensionExitReason::MemoryLimit,
                format!(
                    "its process group used {} MiB of resident memory; the limit is {} MiB",
                    usage.resident_bytes / (1024 * 1024),
                    limits.memory_bytes / (1024 * 1024)
                ),
            );
            return;
        }
    }
}

async fn monitor(
    core: Arc<WorkerCore>,
    mut child: Child,
    leader: oneshot::Receiver<()>,
    exit_sender: watch::Sender<Option<ExtensionWorkerExit>>,
    hook: ExitHook,
    stderr_task: tokio::task::AbortHandle,
) {
    // The leader has exited but is not reaped, so its group id is still
    // reserved: whatever is left of the group dies with it.
    let _ = leader.await;
    {
        let mut reaped = lock(&core.reaped);
        process::signal_group(core.pid, Signal::Kill);
        *reaped = true;
    }
    let status = child.wait().await.ok();
    core.stop.cancel();
    // Waiting callers see the exit; nothing is replayed. What they asked
    // for is reported, so its unknown outcome is recorded.
    let interrupted: Vec<InterruptedCall> = lock(&core.pending)
        .drain()
        .map(|(call_id, call)| InterruptedCall {
            call_id,
            request_id: call.request_id,
            method: call.method,
            started_at_ms: call.started_at_ms,
        })
        .collect();
    lock(&core.cancelled).clear();
    for (_, task) in lock(&core.host_tasks).drain() {
        task.abort();
    }
    let group_cleared = process::wait_group_gone(core.pid, GROUP_GONE_WAIT).await;
    // The group is gone, so stderr ends unless something outside it kept a
    // copy; do not wait long for that.
    let _ = tokio::time::timeout(STDERR_TAIL_WAIT, core.stderr_done.cancelled()).await;
    stderr_task.abort();
    // Let a handshake in progress record why it failed.
    let _ = tokio::time::timeout(
        core.shared.limits.handshake_timeout + Duration::from_secs(1),
        core.handshake_done.cancelled(),
    )
    .await;
    let ready = core.ready.load(Ordering::SeqCst);
    let (code, signal) = status
        .as_ref()
        .map(process::exit_parts)
        .unwrap_or((None, None));
    let recorded = lock(&core.kill_reason).clone();
    let (reason, detail) = match recorded {
        Some((reason, detail)) => (reason, detail),
        None if signal.is_some() && signal == process::cpu_limit_signal() => (
            ExtensionExitReason::CpuLimit,
            "a worker process reached its CPU-seconds limit".to_owned(),
        ),
        None if !ready => (
            ExtensionExitReason::HandshakeFailed,
            "the worker ended before completing its handshake".to_owned(),
        ),
        None => match (code, signal) {
            (Some(0), _) => (
                ExtensionExitReason::Exited,
                "the worker exited on its own".to_owned(),
            ),
            (Some(code), _) => (
                ExtensionExitReason::Crashed,
                format!("the worker exited with status {code}"),
            ),
            (None, Some(signal)) => (
                ExtensionExitReason::Crashed,
                format!("the worker was ended by signal {signal}, which LUNA did not send"),
            ),
            (None, None) => (
                ExtensionExitReason::Crashed,
                "the worker ended with an unknown status".to_owned(),
            ),
        },
    };
    let exit = ExtensionWorkerExit {
        generation: core.generation,
        pid: core.pid,
        reason,
        code,
        signal,
        detail: Some(detail),
        at_ms: now_ms(),
        group_cleared,
    };
    hook(core.generation, exit.clone(), ready, interrupted).await;
    let _ = exit_sender.send(Some(exit));
}
