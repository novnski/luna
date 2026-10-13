//! CORE-07 extension supervisor.
//!
//! An instance is one owner-started activation of one exact, enabled package
//! hash. Its principal and target are bound here; its worker reaches LUNA
//! only through the broker over its own stdio. Every start and restart is
//! prepared by the extension manager, which re-reads the profile grant for
//! that hash and re-hashes every file first.
//!
//! Faults (crashes, protocol violations, hangs, floods, limit kills, failed
//! handshakes) count per package hash; three within ten minutes quarantine
//! the hash durably in this profile until the owner releases it. A crashed
//! worker restarts only when the next call asks for it, and a call is never
//! replayed. Nothing starts when the engine opens.
//!
//! CORE-19: an instance stays pinned to the package hash it started with. A
//! version switch drains the extension's instances (no new calls, wait for
//! the ones in progress) or stops them deliberately before the switch
//! commits. A call whose worker stops before answering is recorded with an
//! unknown outcome; a later instance of the same extension that implements
//! `luna.reconcile` settles it, and a resend of the same request id is
//! refused until it is settled as not applied. Safe mode (profile setting or
//! launch request) refuses every start.

mod broker;
mod process;
mod protocol;
mod quarantine;
mod worker;

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use luna_proto::contract::{ExtensionId, NamespacedId};
use luna_proto::{
    CallExtensionInstanceRequest, EXTENSION_RECONCILE_OPERATION, ExtensionCallError,
    ExtensionCallOutcome, ExtensionCallResolution, ExtensionCallResolutionOutcome,
    ExtensionCallStatus, ExtensionExitReason, ExtensionInstancePurpose, ExtensionInstanceState,
    ExtensionInstanceView, ExtensionInstancesSnapshot, ExtensionIsolation, ExtensionRuntimeKind,
    ExtensionRuntimeLimits, ExtensionWorkerExit,
};
use serde_json::{Value, json};

use crate::WorkspaceHost;
use crate::device_write::DeviceWriteHandle;

pub(super) use broker::BoundTarget;
use broker::{Broker, Principal};
use quarantine::{QuarantineRecord, QuarantineStore, SafeModeRecord, UnresolvedRecord};
use worker::{CallResult, ExitHook, Handshake, InstanceShared, InterruptedCall, Usage, Worker};
pub(super) use worker::{LaunchSpec, Limits};

/// Instances running (or starting) at once in one engine.
const MAX_RUNNING_INSTANCES: usize = 16;
/// Instance records kept for readback, running or not.
const MAX_RETAINED_INSTANCES: usize = 32;
/// Faults within [`FAULT_WINDOW`] that quarantine a package hash.
const QUARANTINE_FAULTS: usize = 3;
const FAULT_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Unresolved calls offered to one `luna.reconcile`, and its deadline.
const RECONCILE_BATCH: usize = 64;
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a drain checks the calls in progress.
const DRAIN_POLL: Duration = Duration::from_millis(20);
const REQUEST_ID_LIMIT: usize = 128;

const NATIVE_HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(1000);
/// The portable host compiles the module before it answers.
const PORTABLE_HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(3000);
const NATIVE_MEMORY_MIB: u64 = 1024;
const PORTABLE_MEMORY_MIB: u64 = 256;
const DEFAULT_CPU_SECONDS: u64 = 3600;
const NATIVE_MAX_PROCESSES: u32 = 64;
const PORTABLE_MAX_PROCESSES: u32 = 4;
const NATIVE_OPEN_FILES: u64 = 1024;
const PORTABLE_OPEN_FILES: u64 = 64;
/// CORE-00: one 16 MiB guest linear memory.
const GUEST_MEMORY_BYTES: u64 = 16 * 1024 * 1024;
const GUEST_FUEL_PER_CALL: u64 = 2_000_000_000;

/// The first-party portable host executable, installed next to LUNA.
pub(super) const PORTABLE_HOST_NAME: &str = "luna-extension-host";
/// Developer override for the portable host's location (an absolute path),
/// for builds that do not bundle it next to the executable.
const PORTABLE_HOST_ENV: &str = "LUNA_EXTENSION_HOST";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Everything one start needs, prepared and verified by the manager.
pub(super) struct LaunchPlan {
    pub extension_id: ExtensionId,
    pub version: String,
    pub package_hash: String,
    pub runtime: ExtensionRuntimeKind,
    pub grants: Vec<String>,
    pub target: BoundTarget,
    pub spec: LaunchSpec,
    pub limits: Limits,
    /// The data generation folder bound to the instance (CORE-19).
    pub data_root: Option<PathBuf>,
    pub data_schema_version: Option<u32>,
    pub purpose: ExtensionInstancePurpose,
}

/// Prepares a fresh, verified plan for a restart (blocking I/O).
pub(super) type Relaunch = Arc<dyn Fn() -> Result<LaunchPlan, String> + Send + Sync>;

/// Limits for a worker class, with the manifest's requests applied (the
/// manifest validator has already checked their ranges).
fn limits_for(runtime: ExtensionRuntimeKind, requested: Option<ExtensionRuntimeLimits>) -> Limits {
    let portable = runtime == ExtensionRuntimeKind::Wasm;
    let memory_mib = requested.and_then(|limits| limits.memory_mib).map_or(
        if portable {
            PORTABLE_MEMORY_MIB
        } else {
            NATIVE_MEMORY_MIB
        },
        u64::from,
    );
    let cpu_seconds = requested
        .and_then(|limits| limits.cpu_seconds)
        .map_or(DEFAULT_CPU_SECONDS, u64::from);
    Limits {
        memory_bytes: memory_mib * 1024 * 1024,
        cpu_seconds,
        max_processes: if portable {
            PORTABLE_MAX_PROCESSES
        } else {
            NATIVE_MAX_PROCESSES
        },
        open_files: if portable {
            PORTABLE_OPEN_FILES
        } else {
            NATIVE_OPEN_FILES
        },
        handshake_timeout: if portable {
            PORTABLE_HANDSHAKE_TIMEOUT
        } else {
            NATIVE_HANDSHAKE_TIMEOUT
        },
        guest_memory_bytes: portable.then_some(GUEST_MEMORY_BYTES),
        guest_fuel_per_call: portable.then_some(GUEST_FUEL_PER_CALL),
    }
}

/// A native worker: the package's own entry point, run from its read-only
/// package directory with the allow-listed environment.
pub(super) fn native_launch(
    package_dir: &Path,
    entry: &str,
    requested: Option<ExtensionRuntimeLimits>,
    required: &[String],
) -> (LaunchSpec, Limits) {
    let spec = LaunchSpec {
        program: package_dir.join(entry),
        args: Vec::new(),
        cwd: package_dir.to_path_buf(),
        environment: process::worker_environment(),
        required: required.to_vec(),
        expected_module_sha256: None,
    };
    (spec, limits_for(ExtensionRuntimeKind::Native, requested))
}

/// A portable worker: LUNA's own host process compiles the pinned module.
/// It gets no environment at all.
pub(super) fn portable_launch(
    package_dir: &Path,
    module: &str,
    module_sha256: &str,
    requested: Option<ExtensionRuntimeLimits>,
    required: &[String],
) -> Result<(LaunchSpec, Limits), String> {
    let host = portable_host().ok_or_else(|| {
        format!(
            "this LUNA does not include its portable extension host ({PORTABLE_HOST_NAME}); Wasm packages cannot run here"
        )
    })?;
    let spec = LaunchSpec {
        program: host,
        args: vec![
            OsString::from("--module"),
            package_dir.join(module).into_os_string(),
        ],
        cwd: package_dir.to_path_buf(),
        environment: Vec::new(),
        required: required.to_vec(),
        expected_module_sha256: Some(module_sha256.to_owned()),
    };
    Ok((spec, limits_for(ExtensionRuntimeKind::Wasm, requested)))
}

fn portable_host() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(PORTABLE_HOST_ENV) {
        let path = PathBuf::from(path);
        return (path.is_absolute() && path.is_file()).then_some(path);
    }
    let name = format!("{PORTABLE_HOST_NAME}{}", std::env::consts::EXE_SUFFIX);
    let host = std::env::current_exe().ok()?.with_file_name(name);
    host.is_file().then_some(host)
}

/// What is and is not enforced for a worker class. Never claims an OS
/// sandbox: none is applied.
fn isolation(runtime: ExtensionRuntimeKind) -> ExtensionIsolation {
    let mut enforced: Vec<String> = [
        "brokerCapabilities",
        "serverBoundTarget",
        "frameSize",
        "frameRate",
        "callConcurrency",
        "callDeadline",
        "hostCallRate",
        "logWindow",
        "environmentAllowList",
        "noInheritedDescriptors",
        "ownSession",
        "cpuSeconds",
        "openFiles",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if process::GROUP_SAMPLING {
        enforced.push("residentMemory".into());
        enforced.push("processCount".into());
    }
    let not_enforced: Vec<String> = match runtime {
        ExtensionRuntimeKind::Wasm => {
            enforced.extend(
                [
                    "noAmbientImports",
                    "guestMemory",
                    "guestFuel",
                    "freshInstancePerCall",
                ]
                .map(String::from),
            );
            Vec::new()
        }
        _ => [
            "filesystem",
            "network",
            "processSpawn",
            "loopbackOwnerIpc",
            "otherUserProcesses",
            "diskUsage",
            "childSessions",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    };
    ExtensionIsolation {
        os_sandbox: false,
        enforced,
        not_enforced,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Starting,
    Running,
    /// A version switch waits for its calls; it takes no new ones.
    Draining,
    Crashed,
    Stopped,
    Failed,
    Revoked,
    Quarantined,
}

impl Phase {
    fn view(self) -> ExtensionInstanceState {
        match self {
            Self::Starting => ExtensionInstanceState::Starting,
            Self::Running => ExtensionInstanceState::Running,
            Self::Draining => ExtensionInstanceState::Draining,
            Self::Crashed => ExtensionInstanceState::Crashed,
            Self::Stopped => ExtensionInstanceState::Stopped,
            Self::Failed => ExtensionInstanceState::Failed,
            Self::Revoked => ExtensionInstanceState::Revoked,
            Self::Quarantined => ExtensionInstanceState::Quarantined,
        }
    }

    /// Set by the owner or the supervisor; a worker exit never undoes it.
    fn is_final(self) -> bool {
        matches!(self, Self::Stopped | Self::Revoked | Self::Quarantined)
    }

    fn is_live(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Draining)
    }
}

struct InstanceState {
    phase: Phase,
    generation: u32,
    worker: Option<Arc<Worker>>,
    /// The last successful handshake, kept for readback after an exit.
    handshake: Option<Handshake>,
    last_exit: Option<ExtensionWorkerExit>,
}

struct Instance {
    id: String,
    purpose: ExtensionInstancePurpose,
    data_schema_version: Option<u32>,
    shared: Arc<InstanceShared>,
    relaunch: Relaunch,
    started_at_ms: i64,
    state: Mutex<InstanceState>,
    /// Serializes launches of this instance.
    start: tokio::sync::Mutex<()>,
}

impl Instance {
    fn principal(&self) -> &Principal {
        &self.shared.principal
    }
}

struct RuntimeInner {
    instances: Mutex<VecDeque<Arc<Instance>>>,
    /// Package hash → recent fault times.
    faults: Mutex<HashMap<String, VecDeque<Instant>>>,
    quarantine: Arc<QuarantineStore>,
    broker: Broker,
    closed: AtomicBool,
    /// Safe mode was requested at launch for this run (CORE-19).
    launch_safe_mode: bool,
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        // Engine teardown without a graceful shutdown: no worker outlives it.
        for instance in lock(&self.instances).iter() {
            if let Some(worker) = lock(&instance.state).worker.clone() {
                worker.kill_now(ExtensionExitReason::Shutdown);
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct ExtensionRuntime {
    inner: Arc<RuntimeInner>,
}

impl ExtensionRuntime {
    pub fn open(
        store_root: &Path,
        profile_id: String,
        writes: DeviceWriteHandle,
        workspace: Option<WorkspaceHost>,
        launch_safe_mode: bool,
    ) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                instances: Mutex::new(VecDeque::new()),
                faults: Mutex::new(HashMap::new()),
                quarantine: Arc::new(QuarantineStore::open(store_root, profile_id, writes)),
                broker: Broker::new(workspace),
                closed: AtomicBool::new(false),
                launch_safe_mode,
            }),
        }
    }

    /// Resolve the owner's chosen task (blocking-safe; reads the registry).
    pub fn bind_task(&self, task_id: &str) -> Result<BoundTarget, String> {
        self.inner.broker.bind_task(task_id)
    }

    /// `Err` when `package_hash` must not start in this profile.
    pub fn check_startable(&self, package_hash: &str) -> Result<(), String> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err("the engine is shutting down".into());
        }
        if self.safe_mode()?.is_some() {
            return Err(
                "safe mode is on, so no extension runs; turn it off in Settings → Extensions to run it again"
                    .into(),
            );
        }
        match self.inner.quarantine.get(package_hash)? {
            Some(record) => Err(format!(
                "{} {} is quarantined in this profile ({}); release it to run it again",
                record.extension_id.as_str(),
                record.version,
                record.reason
            )),
            None => Ok(()),
        }
    }

    pub fn snapshot(&self) -> ExtensionInstancesSnapshot {
        let instances: Vec<Arc<Instance>> = lock(&self.inner.instances).iter().cloned().collect();
        ExtensionInstancesSnapshot {
            instances: instances.iter().map(|instance| view(instance)).collect(),
            quarantined: self.inner.quarantine.views(),
            state_error: self.inner.quarantine.error(),
            unresolved: self.inner.quarantine.unresolved_views(),
            safe_mode: matches!(self.safe_mode(), Ok(Some(_))),
        }
    }

    /// Why this package hash is quarantined in this profile, if it is.
    pub fn quarantine_reason(&self, package_hash: &str) -> Option<String> {
        self.inner
            .quarantine
            .get(package_hash)
            .ok()
            .flatten()
            .map(|record| record.reason)
    }

    /// Safe mode, and whether it came from this launch. `Err` when the
    /// runtime state is unreadable (starts are refused then anyway).
    pub fn safe_mode(&self) -> Result<Option<(bool, Option<i64>)>, String> {
        let profile = self.inner.quarantine.safe_mode()?;
        Ok(match (self.inner.launch_safe_mode, profile) {
            (true, profile) => Some((true, profile.map(|record| record.enabled_at_ms))),
            (false, Some(record)) => Some((false, Some(record.enabled_at_ms))),
            (false, None) => None,
        })
    }

    /// Whether the profile setting (not a launch request) is on.
    pub fn profile_safe_mode(&self) -> bool {
        matches!(self.inner.quarantine.safe_mode(), Ok(Some(_)))
    }

    /// Turn the profile's safe mode on or off. Turning it on stops every
    /// worker; nothing about grants or data changes.
    pub async fn set_safe_mode(&self, enabled: bool) -> Result<Vec<String>, String> {
        let store = self.inner.quarantine.clone();
        let record = enabled.then(|| SafeModeRecord {
            enabled_at_ms: now_ms(),
        });
        let already = store.safe_mode()?.is_some();
        if already != enabled {
            tokio::task::spawn_blocking(move || store.set_safe_mode(record))
                .await
                .map_err(|error| format!("safe mode was not saved: {error}"))??;
        }
        if !enabled {
            return Ok(Vec::new());
        }
        Ok(self
            .retire(|_| true, ExtensionExitReason::SafeMode, false)
            .await)
    }

    /// Instances of `extension_id` that are starting, running or draining.
    pub fn live_count(&self, extension_id: &ExtensionId) -> usize {
        lock(&self.inner.instances)
            .iter()
            .filter(|instance| {
                instance.principal().extension_id == *extension_id
                    && lock(&instance.state).phase.is_live()
            })
            .count()
    }

    /// Drop quarantine records of packages that are no longer installed.
    pub fn prune_quarantine(&self, installed: &[String]) {
        match self.inner.quarantine.prune(installed) {
            Ok(0) => {}
            Ok(pruned) => {
                tracing::warn!(pruned, "released quarantine of removed extension packages")
            }
            Err(error) => tracing::warn!(%error, "extension quarantine could not be pruned"),
        }
    }

    /// A package hash was removed from this device: release its quarantine
    /// record and forget its faults (blocking I/O). `true` when it was
    /// quarantined.
    pub fn forget_package(&self, package_hash: &str) -> Result<bool, String> {
        lock(&self.inner.faults).remove(package_hash);
        self.inner.quarantine.remove(package_hash)
    }

    fn find(&self, instance_id: &str) -> Result<Arc<Instance>, String> {
        lock(&self.inner.instances)
            .iter()
            .find(|instance| instance.id == instance_id)
            .cloned()
            .ok_or_else(|| format!("there is no extension instance {instance_id}"))
    }

    /// Create an instance for a verified plan and start its first worker.
    pub async fn start(
        &self,
        plan: LaunchPlan,
        relaunch: Relaunch,
    ) -> Result<ExtensionInstanceView, String> {
        self.check_startable(&plan.package_hash)?;
        let id = format!("ext-{}", uuid::Uuid::new_v4());
        let principal = Arc::new(Principal::new(
            id.clone(),
            plan.extension_id,
            plan.version,
            plan.package_hash,
            plan.runtime,
            plan.grants,
            plan.target,
            plan.data_root,
        ));
        let shared = Arc::new(InstanceShared::new(
            principal,
            plan.limits,
            self.inner.broker.clone(),
        ));
        let instance = Arc::new(Instance {
            id,
            purpose: plan.purpose,
            data_schema_version: plan.data_schema_version,
            shared,
            relaunch,
            started_at_ms: now_ms(),
            state: Mutex::new(InstanceState {
                phase: Phase::Starting,
                generation: 1,
                worker: None,
                handshake: None,
                last_exit: None,
            }),
            start: tokio::sync::Mutex::new(()),
        });
        {
            let mut instances = lock(&self.inner.instances);
            let live = instances
                .iter()
                .filter(|instance| lock(&instance.state).phase.is_live())
                .count();
            if live >= MAX_RUNNING_INSTANCES {
                return Err(format!(
                    "{MAX_RUNNING_INSTANCES} extension instances are already running; stop one first"
                ));
            }
            while instances.len() >= MAX_RETAINED_INSTANCES {
                let Some(position) = instances
                    .iter()
                    .position(|instance| !lock(&instance.state).phase.is_live())
                else {
                    break;
                };
                instances.remove(position);
            }
            instances.push_back(instance.clone());
        }
        let _start = instance.start.lock().await;
        self.launch(&instance, plan.spec, 1).await?;
        Ok(view(&instance))
    }

    /// Start worker `generation` for `instance` and publish it if it is still
    /// wanted. The instance's start lock is held by the caller.
    async fn launch(
        &self,
        instance: &Arc<Instance>,
        spec: LaunchSpec,
        generation: u32,
    ) -> Result<Arc<Worker>, String> {
        let hook = self.exit_hook(instance);
        let worker = Worker::launch(spec, instance.shared.clone(), generation, hook).await?;
        let published = {
            let mut state = lock(&instance.state);
            if state.generation == generation && state.phase == Phase::Starting {
                state.phase = Phase::Running;
                state.worker = Some(worker.clone());
                state.handshake = Some(worker.handshake().clone());
                true
            } else {
                if !worker.has_exited() {
                    // Revoked, quarantined or retired while it was starting.
                    worker.kill_now(match state.phase {
                        Phase::Revoked => ExtensionExitReason::Revoked,
                        Phase::Quarantined => ExtensionExitReason::Quarantined,
                        _ => ExtensionExitReason::Stopped,
                    });
                }
                false
            }
        };
        if published {
            self.reconcile(instance, &worker).await;
        }
        Ok(worker)
    }

    /// Offer this extension's unresolved calls to a worker that implements
    /// `luna.reconcile`, and record what it reports. Calls it reports as
    /// unknown (or omits) stay unresolved. Nothing is ever re-sent.
    async fn reconcile(&self, instance: &Arc<Instance>, worker: &Arc<Worker>) {
        if instance.purpose != ExtensionInstancePurpose::Run
            || !worker.advertises(EXTENSION_RECONCILE_OPERATION)
        {
            return;
        }
        let principal = instance.principal();
        let pending = self
            .inner
            .quarantine
            .pending_for(&principal.extension_id, RECONCILE_BATCH);
        if pending.is_empty() {
            return;
        }
        let calls: Vec<Value> = pending
            .iter()
            .map(|record| {
                json!({
                    "callId": record.call_id,
                    "requestId": record.request_id,
                    "method": record.method,
                    "version": record.version,
                    "packageHash": record.package_hash,
                    "generation": record.generation,
                    "interruptedAtMs": record.interrupted_at_ms,
                })
            })
            .collect();
        let id = uuid::Uuid::new_v4().to_string();
        let answer = worker
            .call(
                &id,
                None,
                EXTENSION_RECONCILE_OPERATION,
                &json!({ "calls": calls }),
                RECONCILE_TIMEOUT,
            )
            .await;
        let CallResult::Ok(answer) = answer else {
            tracing::warn!(
                extension = principal.extension_id.as_str(),
                "luna.reconcile did not answer; the calls stay unresolved"
            );
            return;
        };
        let at = now_ms();
        let resolutions: Vec<(String, ExtensionCallResolution)> = answer
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|result| {
                let call_id = result.get("callId")?.as_str()?;
                if !pending.iter().any(|record| record.call_id == call_id) {
                    return None;
                }
                let outcome = match result.get("outcome")?.as_str()? {
                    "applied" => ExtensionCallResolutionOutcome::Applied,
                    "notApplied" => ExtensionCallResolutionOutcome::NotApplied,
                    _ => return None,
                };
                Some((
                    call_id.to_owned(),
                    ExtensionCallResolution {
                        outcome,
                        detail: result
                            .get("detail")
                            .and_then(Value::as_str)
                            .map(|detail| protocol::bounded_text(detail, 500)),
                        resolved_at_ms: at,
                        resolved_by_version: principal.version.clone(),
                        resolved_by_package_hash: principal.package_hash.clone(),
                    },
                ))
            })
            .collect();
        if resolutions.is_empty() {
            return;
        }
        let store = self.inner.quarantine.clone();
        match tokio::task::spawn_blocking(move || store.resolve(resolutions)).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "extension call resolutions were not saved"),
            Err(error) => tracing::warn!(%error, "extension call resolutions stopped"),
        }
    }

    fn exit_hook(&self, instance: &Arc<Instance>) -> ExitHook {
        let runtime = Arc::downgrade(&self.inner);
        let instance = Arc::downgrade(instance);
        Arc::new(
            move |generation: u32,
                  exit: ExtensionWorkerExit,
                  ready: bool,
                  interrupted: Vec<InterruptedCall>|
                  -> BoxFuture<'static, ()> {
                let runtime: Weak<RuntimeInner> = runtime.clone();
                let instance: Weak<Instance> = instance.clone();
                Box::pin(async move {
                    let (Some(runtime), Some(instance)) = (runtime.upgrade(), instance.upgrade())
                    else {
                        return;
                    };
                    ExtensionRuntime { inner: runtime }
                        .worker_exited(&instance, generation, exit, ready, interrupted)
                        .await;
                })
            },
        )
    }

    async fn worker_exited(
        &self,
        instance: &Arc<Instance>,
        generation: u32,
        exit: ExtensionWorkerExit,
        ready: bool,
        interrupted: Vec<InterruptedCall>,
    ) {
        {
            let mut state = lock(&instance.state);
            if state.generation == generation {
                state.worker = None;
                if !state.phase.is_final() {
                    state.phase = match exit.reason {
                        ExtensionExitReason::Stopped
                        | ExtensionExitReason::Shutdown
                        | ExtensionExitReason::Updating
                        | ExtensionExitReason::SafeMode => Phase::Stopped,
                        ExtensionExitReason::Revoked => Phase::Revoked,
                        ExtensionExitReason::Quarantined => Phase::Quarantined,
                        _ if !ready => Phase::Failed,
                        _ => Phase::Crashed,
                    };
                }
                state.last_exit = Some(exit.clone());
            }
        }
        self.record_interrupted(instance, generation, &exit, interrupted)
            .await;
        if exit.reason.is_fault() {
            instance.shared.usage.faults.fetch_add(1, Ordering::Relaxed);
            self.record_fault(instance, &exit).await;
        }
    }

    /// Calls the worker never answered have an unknown outcome: record them
    /// (owner calls of a running instance only; `luna.*` host operations and
    /// migration instances are settled by their own caller).
    async fn record_interrupted(
        &self,
        instance: &Arc<Instance>,
        generation: u32,
        exit: &ExtensionWorkerExit,
        interrupted: Vec<InterruptedCall>,
    ) {
        if instance.purpose != ExtensionInstancePurpose::Run {
            return;
        }
        let principal = instance.principal();
        let records: Vec<UnresolvedRecord> = interrupted
            .into_iter()
            .filter(|call| !call.method.starts_with("luna."))
            .map(|call| UnresolvedRecord {
                call_id: call.call_id,
                request_id: call.request_id,
                extension_id: principal.extension_id.clone(),
                version: principal.version.clone(),
                package_hash: principal.package_hash.clone(),
                instance_id: instance.id.clone(),
                generation,
                method: call.method,
                started_at_ms: call.started_at_ms,
                interrupted_at_ms: exit.at_ms,
                exit_reason: exit.reason,
                resolution: None,
            })
            .collect();
        if records.is_empty() {
            return;
        }
        let store = self.inner.quarantine.clone();
        match tokio::task::spawn_blocking(move || store.record_unresolved(records)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(
                %error,
                "unresolved extension calls were not saved; they apply until restart"
            ),
            Err(error) => tracing::warn!(%error, "unresolved extension calls write stopped"),
        }
    }

    async fn record_fault(&self, instance: &Arc<Instance>, exit: &ExtensionWorkerExit) {
        let principal = instance.principal();
        let hash = principal.package_hash.clone();
        let count = {
            let mut faults = lock(&self.inner.faults);
            let recent = faults.entry(hash.clone()).or_default();
            recent.push_back(Instant::now());
            while recent.front().is_some_and(|at| at.elapsed() > FAULT_WINDOW) {
                recent.pop_front();
            }
            recent.len()
        };
        if count < QUARANTINE_FAULTS {
            return;
        }
        let record = QuarantineRecord {
            extension_id: principal.extension_id.clone(),
            version: principal.version.clone(),
            reason: format!(
                "{count} faults within {} minutes; the last: {}",
                FAULT_WINDOW.as_secs() / 60,
                exit.detail.as_deref().unwrap_or("unknown")
            ),
            faults: count as u32,
            quarantined_at_ms: now_ms(),
        };
        let store = self.inner.quarantine.clone();
        let key = hash.clone();
        match tokio::task::spawn_blocking(move || store.insert(&key, record)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, "extension quarantine was not saved; it applies until restart")
            }
            Err(error) => tracing::warn!(%error, "extension quarantine write stopped"),
        }
        tracing::warn!(
            extension = principal.extension_id.as_str(),
            package_hash = %hash,
            "extension package quarantined after repeated faults"
        );
        let instances: Vec<Arc<Instance>> = lock(&self.inner.instances).iter().cloned().collect();
        for other in instances
            .iter()
            .filter(|other| other.principal().package_hash == hash)
        {
            let worker = {
                let mut state = lock(&other.state);
                state.phase = Phase::Quarantined;
                state.worker.clone()
            };
            if let Some(worker) = worker {
                worker.kill_now(ExtensionExitReason::Quarantined);
            }
        }
    }

    /// The running worker, restarting a crashed one after re-verification.
    async fn ensure_running(&self, instance: &Arc<Instance>) -> Result<Arc<Worker>, String> {
        let _start = instance.start.lock().await;
        let (phase, generation) = {
            let state = lock(&instance.state);
            if let (Phase::Running, Some(worker)) = (state.phase, &state.worker)
                && !worker.has_exited()
            {
                return Ok(worker.clone());
            }
            (state.phase, state.generation)
        };
        match phase {
            Phase::Running | Phase::Crashed => {}
            Phase::Starting => return Err("the instance is still starting".into()),
            Phase::Draining => {
                return Err(
                    "the extension's version is being switched; this instance takes no new calls"
                        .into(),
                );
            }
            Phase::Stopped => {
                return Err("the instance was stopped; start a new one".into());
            }
            Phase::Failed => {
                return Err("the instance failed to start; see its last exit".into());
            }
            Phase::Revoked => {
                return Err(
                    "the package was disabled or its grants changed; start a new instance".into(),
                );
            }
            Phase::Quarantined => {
                return Err(
                    "the package is quarantined in this profile; release it to run it again".into(),
                );
            }
        }
        let principal = instance.principal();
        if let Err(error) = self.check_startable(&principal.package_hash) {
            if self
                .inner
                .quarantine
                .get(&principal.package_hash)
                .ok()
                .flatten()
                .is_some()
            {
                lock(&instance.state).phase = Phase::Quarantined;
            }
            return Err(error);
        }
        let relaunch = instance.relaunch.clone();
        let plan = tokio::task::spawn_blocking(move || relaunch())
            .await
            .map_err(|error| format!("the restart was interrupted: {error}"))?
            .map_err(|error| format!("the instance cannot restart: {error}"))?;
        if plan.package_hash != principal.package_hash
            || plan.grants != principal.grants
            || plan.target != principal.target
            || plan.data_root != principal.data_root
        {
            principal.revoke();
            lock(&instance.state).phase = Phase::Revoked;
            return Err(
                "the package's grants, target or data changed since this instance started; start a new instance"
                    .into(),
            );
        }
        let next = generation + 1;
        {
            let mut state = lock(&instance.state);
            state.phase = Phase::Starting;
            state.generation = next;
        }
        instance
            .shared
            .usage
            .restarts
            .fetch_add(1, Ordering::Relaxed);
        self.launch(instance, plan.spec, next).await
    }

    pub async fn call(
        &self,
        request: CallExtensionInstanceRequest,
    ) -> Result<ExtensionCallOutcome, String> {
        let method = request.method.trim();
        if method.is_empty()
            || method.len() > protocol::NAME_LIMIT
            || method.chars().any(char::is_control)
        {
            return Err(format!(
                "a method name is 1 to {} printable bytes",
                protocol::NAME_LIMIT
            ));
        }
        let timeout = match request.timeout_ms {
            None => worker::DEFAULT_CALL_TIMEOUT,
            Some(0) => return Err("timeoutMs must be at least 1".into()),
            Some(ms) if Duration::from_millis(ms) > worker::MAX_CALL_TIMEOUT => {
                return Err(format!(
                    "timeoutMs is at most {}",
                    worker::MAX_CALL_TIMEOUT.as_millis()
                ));
            }
            Some(ms) => Duration::from_millis(ms),
        };
        let request_id = match request.request_id.as_deref() {
            None => None,
            Some(id)
                if id.is_empty()
                    || id.len() > REQUEST_ID_LIMIT
                    || id.chars().any(char::is_control) =>
            {
                return Err(format!(
                    "a request id is 1 to {REQUEST_ID_LIMIT} printable bytes"
                ));
            }
            Some(id) => Some(id),
        };
        let instance = self.find(&request.instance_id)?;
        if let Some(request_id) = request_id {
            self.check_request(&instance.principal().extension_id, request_id)?;
        }
        let worker = self.ensure_running(&instance).await?;
        let usage = &instance.shared.usage;
        Usage::count(&usage.calls);
        let started = Instant::now();
        let call_id = uuid::Uuid::new_v4().to_string();
        let result = worker
            .call(&call_id, request_id, method, &request.params, timeout)
            .await;
        let mut outcome = ExtensionCallOutcome {
            instance_id: instance.id.clone(),
            call_id: Some(call_id),
            request_id: request_id.map(str::to_owned),
            generation: worker.generation(),
            status: ExtensionCallStatus::Ok,
            result: None,
            error: None,
            exit: None,
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        match result {
            CallResult::Ok(value) => outcome.result = Some(value),
            CallResult::Failed(error) => {
                Usage::count(&usage.calls_failed);
                outcome.status = ExtensionCallStatus::Failed;
                outcome.error = Some(error);
            }
            CallResult::DeadlineExceeded => {
                Usage::count(&usage.calls_deadline_exceeded);
                outcome.status = ExtensionCallStatus::DeadlineExceeded;
                outcome.error = Some(ExtensionCallError {
                    code: "deadlineExceeded".into(),
                    message: format!(
                        "no answer within {} ms; the call was cancelled and is not retried",
                        timeout.as_millis()
                    ),
                });
            }
            CallResult::WorkerExited(exit) => {
                Usage::count(&usage.calls_failed);
                outcome.status = ExtensionCallStatus::WorkerExited;
                outcome.error = Some(ExtensionCallError {
                    code: "workerExited".into(),
                    message: "the worker exited before answering; the call is not replayed".into(),
                });
                outcome.exit = exit;
            }
            CallResult::Busy(message) => {
                Usage::count(&usage.calls_busy);
                outcome.status = ExtensionCallStatus::Busy;
                outcome.error = Some(ExtensionCallError {
                    code: "busy".into(),
                    message,
                });
            }
        }
        Ok(outcome)
    }

    /// Refuse a request id whose earlier delivery to this extension has an
    /// unknown outcome or was reconciled as applied.
    fn check_request(&self, extension_id: &ExtensionId, request_id: &str) -> Result<(), String> {
        let Some(record) = self.inner.quarantine.request(extension_id, request_id)? else {
            return Ok(());
        };
        match record
            .resolution
            .as_ref()
            .map(|resolution| resolution.outcome)
        {
            None => Err(format!(
                "request {request_id} reached {} {} and its worker stopped before answering, so its outcome is unknown; it is not sent again until the extension reconciles it",
                record.extension_id.as_str(),
                record.version
            )),
            Some(ExtensionCallResolutionOutcome::Applied) => Err(format!(
                "request {request_id} was already applied (reconciled by {}); it is not sent again",
                record
                    .resolution
                    .as_ref()
                    .map_or("the extension", |resolution| resolution
                        .resolved_by_version
                        .as_str())
            )),
            Some(ExtensionCallResolutionOutcome::NotApplied) => Ok(()),
        }
    }

    /// Stop the instances `select` picks: each becomes final (`Stopped`, or
    /// `Revoked` with its authority withdrawn first when `revoke`) and its
    /// worker gets the graceful stop sequence. Returns the instances that
    /// had a worker or were about to get one.
    async fn retire(
        &self,
        select: impl Fn(&Instance) -> bool,
        reason: ExtensionExitReason,
        revoke: bool,
    ) -> Vec<String> {
        let instances: Vec<Arc<Instance>> = lock(&self.inner.instances)
            .iter()
            .filter(|instance| select(instance))
            .cloned()
            .collect();
        let mut retired = Vec::new();
        let mut workers = Vec::new();
        for instance in &instances {
            if revoke {
                instance.principal().revoke();
            }
            let mut state = lock(&instance.state);
            let active = state.phase.is_live() || state.phase == Phase::Crashed;
            if !state.phase.is_final() && state.phase != Phase::Failed {
                state.phase = if revoke {
                    Phase::Revoked
                } else {
                    Phase::Stopped
                };
            }
            if active {
                retired.push(instance.id.clone());
            }
            if let Some(worker) = state.worker.clone() {
                workers.push(worker);
            }
        }
        futures::future::join_all(workers.iter().map(|worker| worker.stop(reason))).await;
        retired
    }

    /// Stop every instance of `extension_id` before its enabled version
    /// changes. With `drain`, the instances first take no new calls and the
    /// calls in progress may finish; if any is still running when the drain
    /// deadline passes, the instances run on and nothing is stopped.
    /// Without, they stop now and their unanswered calls are recorded as
    /// unresolved.
    pub async fn quiesce(
        &self,
        extension_id: &ExtensionId,
        drain: Option<Duration>,
    ) -> Result<Vec<String>, String> {
        let of_extension = |instance: &Instance| instance.principal().extension_id == *extension_id;
        if let Some(timeout) = drain {
            let deadline = Instant::now() + timeout;
            let instances: Vec<Arc<Instance>> = lock(&self.inner.instances)
                .iter()
                .filter(|instance| of_extension(instance))
                .cloned()
                .collect();
            // Wait for launches in progress, then stop taking calls.
            for instance in &instances {
                let _start = instance.start.lock().await;
                let mut state = lock(&instance.state);
                if state.phase == Phase::Running {
                    state.phase = Phase::Draining;
                }
            }
            let in_flight = |instances: &[Arc<Instance>]| -> usize {
                instances
                    .iter()
                    .filter_map(|instance| {
                        let state = lock(&instance.state);
                        (state.phase == Phase::Draining).then(|| {
                            state
                                .worker
                                .as_ref()
                                .map_or(0, |worker| worker.calls_in_flight())
                        })
                    })
                    .sum()
            };
            let mut quiet = 0;
            loop {
                if in_flight(&instances) == 0 {
                    quiet += 1;
                    // Twice in a row, so a call admitted just before the
                    // drain began has registered.
                    if quiet >= 2 {
                        break;
                    }
                } else {
                    quiet = 0;
                }
                if Instant::now() >= deadline {
                    let busy = in_flight(&instances);
                    for instance in &instances {
                        let mut state = lock(&instance.state);
                        if state.phase == Phase::Draining {
                            state.phase = Phase::Running;
                        }
                    }
                    return Err(format!(
                        "{busy} call(s) were still in progress after {} ms, so nothing changed; retry, or stop the running work instead",
                        timeout.as_millis()
                    ));
                }
                tokio::time::sleep(DRAIN_POLL).await;
            }
        }
        Ok(self
            .retire(of_extension, ExtensionExitReason::Updating, false)
            .await)
    }

    /// Stop and revoke every instance of one package hash (it is being
    /// removed from this device).
    pub async fn retire_package(&self, package_hash: &str) -> Vec<String> {
        self.retire(
            |instance| instance.principal().package_hash == package_hash,
            ExtensionExitReason::Revoked,
            true,
        )
        .await
    }

    pub async fn stop(&self, instance_id: &str) -> Result<ExtensionInstanceView, String> {
        let instance = self.find(instance_id)?;
        let _start = instance.start.lock().await;
        let worker = {
            let mut state = lock(&instance.state);
            if !matches!(
                state.phase,
                Phase::Revoked | Phase::Quarantined | Phase::Failed
            ) {
                state.phase = Phase::Stopped;
            }
            state.worker.clone()
        };
        if let Some(worker) = worker {
            worker.stop(ExtensionExitReason::Stopped).await;
        }
        Ok(view(&instance))
    }

    /// The package was disabled (`keep` is `None`) or re-enabled with another
    /// grant set: withdraw every instance whose principal no longer matches.
    pub fn revoke(&self, extension_id: &ExtensionId, keep: Option<(&str, &[String])>) {
        let instances: Vec<Arc<Instance>> = lock(&self.inner.instances).iter().cloned().collect();
        for instance in instances
            .iter()
            .filter(|instance| instance.principal().extension_id == *extension_id)
        {
            let principal = instance.principal();
            let current = keep.is_some_and(|(hash, grants)| {
                principal.package_hash == hash && principal.grants.as_slice() == grants
            });
            if current {
                continue;
            }
            principal.revoke();
            let worker = {
                let mut state = lock(&instance.state);
                if !matches!(
                    state.phase,
                    Phase::Stopped | Phase::Failed | Phase::Quarantined
                ) {
                    state.phase = Phase::Revoked;
                }
                state.worker.clone()
            };
            if let Some(worker) = worker {
                worker.stop_in_background(ExtensionExitReason::Revoked);
            }
        }
    }

    /// The owner allows this exact package to run again in this profile.
    pub async fn release_quarantine(
        &self,
        extension_id: &ExtensionId,
        package_hash: &str,
    ) -> Result<(), String> {
        match self.inner.quarantine.get(package_hash)? {
            Some(record) if record.extension_id == *extension_id => {}
            Some(_) => {
                return Err(format!(
                    "package {package_hash} is not {}",
                    extension_id.as_str()
                ));
            }
            None => return Err(format!("package {package_hash} is not quarantined")),
        }
        let store = self.inner.quarantine.clone();
        let key = package_hash.to_owned();
        tokio::task::spawn_blocking(move || store.remove(&key))
            .await
            .map_err(|error| format!("the release was interrupted: {error}"))??;
        lock(&self.inner.faults).remove(package_hash);
        Ok(())
    }

    /// Stop every worker and refuse new starts.
    pub async fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let instances: Vec<Arc<Instance>> = lock(&self.inner.instances).iter().cloned().collect();
        let workers: Vec<Arc<Worker>> = instances
            .iter()
            .filter_map(|instance| {
                let mut state = lock(&instance.state);
                if state.phase.is_live() || state.phase == Phase::Crashed {
                    state.phase = Phase::Stopped;
                }
                state.worker.clone()
            })
            .collect();
        futures::future::join_all(
            workers
                .iter()
                .map(|worker| worker.stop(ExtensionExitReason::Shutdown)),
        )
        .await;
        drop(workers);
        // No instance record, worker task or RPC service clone keeps the
        // engine's registry (and through it the data root) past shutdown.
        self.inner.broker.release();
    }
}

fn view(instance: &Instance) -> ExtensionInstanceView {
    let principal = instance.principal();
    let state = lock(&instance.state);
    let worker = state.worker.as_ref();
    let handshake = state.handshake.as_ref();
    ExtensionInstanceView {
        instance_id: instance.id.clone(),
        purpose: instance.purpose,
        extension_id: principal.extension_id.clone(),
        version: principal.version.clone(),
        package_hash: principal.package_hash.clone(),
        runtime: principal.runtime,
        trust: principal.trust(),
        target: principal.target_view(),
        grants: principal
            .grants
            .iter()
            .filter_map(|grant| NamespacedId::parse(grant).ok())
            .collect(),
        state: state.phase.view(),
        generation: state.generation,
        pid: worker.map(|worker| worker.pid()),
        protocol: handshake.map(|handshake| handshake.protocol),
        operations: handshake
            .map(|handshake| handshake.operations.clone())
            .unwrap_or_default(),
        portable: handshake.and_then(|handshake| handshake.portable.clone()),
        limits: instance.shared.limits.view(),
        isolation: isolation(principal.runtime),
        usage: instance
            .shared
            .usage
            .view(worker.map_or(0, |worker| worker.host_calls_in_flight())),
        last_exit: state.last_exit.clone(),
        started_at_ms: instance.started_at_ms,
        logs: lock(&instance.shared.logs).snapshot(),
        data_schema_version: instance.data_schema_version,
    }
}
