//! CORE-07 extension runtime: supervised instances of enabled packages and
//! the capability broker they call.
//!
//! An instance is one activation of one exact package hash, started by the
//! owner. The engine binds its principal (instance, package, version, grants)
//! and its target (one task, or none) when it starts; a worker can never name
//! or change them. Workers reach LUNA only through broker operations over
//! their own stdio. Native workers are user-level code with the account's OS
//! access: LUNA bounds what it brokers and the resources it can meter, and
//! says so in [`ExtensionIsolation`]; it does not apply an OS sandbox.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contract::{ExtensionId, NamespacedId};
use crate::extension_packages::{ExtensionContractVersion, ExtensionRuntimeKind, ExtensionTrust};

/// One broker operation and the capability it needs (`None`: always allowed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionBrokerOperation {
    pub name: &'static str,
    pub capability: Option<&'static str>,
}

/// The broker operations of extension API 1.2. Anything else is refused as
/// `unknownOperation`; owner RPC methods are never broker operations. The
/// `data.*` operations (1.2) reach only the package's own data in this
/// profile, so they need no capability; an instance without data gets
/// `noData`.
pub const EXTENSION_BROKER_OPERATIONS: &[ExtensionBrokerOperation] = &[
    ExtensionBrokerOperation {
        name: "host.info",
        capability: None,
    },
    ExtensionBrokerOperation {
        name: "workspace.read",
        capability: Some("luna.capability/workspace.read"),
    },
    ExtensionBrokerOperation {
        name: "workspace.write",
        capability: Some("luna.capability/workspace.write"),
    },
    ExtensionBrokerOperation {
        name: "tasks.list",
        capability: Some("luna.capability/tasks.read"),
    },
    ExtensionBrokerOperation {
        name: "data.read",
        capability: None,
    },
    ExtensionBrokerOperation {
        name: "data.write",
        capability: None,
    },
];

pub fn extension_broker_operation(name: &str) -> Option<&'static ExtensionBrokerOperation> {
    EXTENSION_BROKER_OPERATIONS
        .iter()
        .find(|operation| operation.name == name)
}

/// What an instance acts on. Chosen by the owner at start and resolved by the
/// engine; a worker cannot select another one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExtensionTarget {
    /// No task: workspace operations are refused (`noTarget`).
    None,
    /// One local task; workspace operations are confined to its checkout.
    #[serde(rename_all = "camelCase")]
    Task { task_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionInstanceState {
    /// Launching or handshaking.
    Starting,
    Running,
    /// Its package version is being switched: it takes no new calls and
    /// stops once the calls in progress finish (CORE-19).
    Draining,
    /// The worker exited unexpectedly; the next call restarts it after the
    /// package and grants are verified again.
    Crashed,
    /// Stopped by the owner or by engine shutdown.
    Stopped,
    /// The worker never completed its handshake.
    Failed,
    /// Disabled, or its grants changed, while it existed. Start a new one.
    Revoked,
    /// The package hash is quarantined in this profile.
    Quarantined,
}

/// Why a worker process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionExitReason {
    /// Stopped by the owner.
    Stopped,
    /// Stopped because the engine shut down.
    Shutdown,
    /// Stopped because the package was disabled or its grants changed.
    Revoked,
    /// Stopped because its package was quarantined.
    Quarantined,
    /// Stopped so the extension's enabled version could change (CORE-19).
    Updating,
    /// Stopped because safe mode was turned on (CORE-19).
    SafeMode,
    /// Exited on its own with status 0.
    Exited,
    /// Exited with a failure status or a signal the supervisor did not send.
    Crashed,
    /// Refused or failed during the handshake.
    HandshakeFailed,
    /// Sent a frame the protocol forbids.
    ProtocolViolation,
    /// Did not acknowledge a cancelled call within the grace period.
    Hung,
    /// Stopped reading its input while the host had frames for it.
    Unresponsive,
    /// Sent frames faster than the host's frame budget.
    Flood,
    /// Its process group's resident memory exceeded the budget.
    MemoryLimit,
    /// A worker process reached its CPU-seconds limit.
    CpuLimit,
    /// Its process group exceeded the process-count cap.
    ProcessLimit,
}

impl ExtensionExitReason {
    /// Whether this exit counts towards quarantine.
    pub fn is_fault(self) -> bool {
        !matches!(
            self,
            Self::Stopped
                | Self::Shutdown
                | Self::Revoked
                | Self::Quarantined
                | Self::Updating
                | Self::SafeMode
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionWorkerExit {
    /// The worker process generation that ended (1 for the first start).
    pub generation: u32,
    pub pid: u32,
    pub reason: ExtensionExitReason,
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
    #[serde(default)]
    pub detail: Option<String>,
    pub at_ms: i64,
    /// Every process of the worker's group was gone when the supervisor
    /// checked after reaping it.
    pub group_cleared: bool,
}

/// The limits the supervisor applies to one instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInstanceLimits {
    /// Resident memory of the whole process group (sampled).
    pub memory_bytes: u64,
    /// CPU seconds per worker process (`RLIMIT_CPU`).
    pub cpu_seconds: u64,
    /// Processes in the worker's group (sampled).
    pub max_processes: u32,
    /// Open descriptors per process (`RLIMIT_NOFILE`).
    pub open_files: u64,
    /// Largest control frame in either direction, checked before decoding.
    pub frame_bytes: u32,
    pub handshake_timeout_ms: u64,
    pub default_call_timeout_ms: u64,
    pub max_call_timeout_ms: u64,
    /// How long a cancelled call has to be acknowledged before the worker is
    /// treated as hung and stopped.
    pub cancel_grace_ms: u64,
    pub max_concurrent_calls: u32,
    pub max_concurrent_host_calls: u32,
    pub host_call_timeout_ms: u64,
    /// Host calls accepted in a burst, then per second.
    pub host_call_burst: u32,
    pub host_calls_per_second: u32,
    /// Frames accepted in a burst, then per second; more stops the worker.
    pub frame_burst: u32,
    pub frames_per_second: u32,
    /// The retained log window (worker log frames and stderr lines).
    pub log_lines: u32,
    pub log_bytes: u32,
    /// Portable only: the guest's linear-memory ceiling.
    #[serde(default)]
    pub guest_memory_bytes: Option<u64>,
    /// Portable only: fuel for one call.
    #[serde(default)]
    pub guest_fuel_per_call: Option<u64>,
}

/// What LUNA enforces for this instance and what it does not. A native
/// worker is never described as sandboxed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionIsolation {
    /// Whether an OS sandbox confines the worker. Always `false` in this
    /// version: no platform confinement is applied.
    pub os_sandbox: bool,
    /// Mechanisms the supervisor actually enforces.
    pub enforced: Vec<String>,
    /// Authority the worker keeps because nothing confines it.
    pub not_enforced: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInstanceUsage {
    pub calls: u64,
    pub calls_failed: u64,
    pub calls_deadline_exceeded: u64,
    pub calls_busy: u64,
    pub host_calls: u64,
    pub host_calls_denied: u64,
    pub host_calls_busy: u64,
    pub host_calls_in_flight: u32,
    pub frames: u64,
    pub log_lines: u64,
    /// Lines pushed out of the retained window.
    pub log_lines_evicted: u64,
    pub stderr_bytes: u64,
    #[serde(default)]
    pub resident_bytes: Option<u64>,
    #[serde(default)]
    pub peak_resident_bytes: Option<u64>,
    #[serde(default)]
    pub processes: Option<u32>,
    pub restarts: u32,
    pub faults: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionLogSource {
    /// A `log` frame from the worker.
    Worker,
    /// A line the worker wrote to stderr.
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionLogLine {
    pub at_ms: i64,
    pub generation: u32,
    pub source: ExtensionLogSource,
    #[serde(default)]
    pub level: Option<String>,
    pub message: String,
}

/// What the portable host reported about the module it compiled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPortableInfo {
    pub profile: ExtensionContractVersion,
    pub runtime: String,
    /// SHA-256 of the exact bytes the host compiled; equals the manifest's pin.
    pub module_sha256: String,
    pub imports: Vec<String>,
}

/// Why an instance exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionInstancePurpose {
    /// Started by the owner.
    #[default]
    Run,
    /// Started by a version switch to run the new version's `luna.migrate`
    /// on a copy of the data. It is never restarted.
    Migration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInstanceView {
    pub instance_id: String,
    #[serde(default)]
    pub purpose: ExtensionInstancePurpose,
    pub extension_id: ExtensionId,
    pub version: String,
    pub package_hash: String,
    pub runtime: ExtensionRuntimeKind,
    pub trust: ExtensionTrust,
    pub target: ExtensionTarget,
    /// The grants bound at start, sorted.
    pub grants: Vec<NamespacedId>,
    pub state: ExtensionInstanceState,
    /// Worker process generation; a restart increments it.
    pub generation: u32,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub protocol: Option<ExtensionContractVersion>,
    /// Operations the worker advertised in its hello.
    pub operations: Vec<String>,
    #[serde(default)]
    pub portable: Option<ExtensionPortableInfo>,
    pub limits: ExtensionInstanceLimits,
    pub isolation: ExtensionIsolation,
    pub usage: ExtensionInstanceUsage,
    #[serde(default)]
    pub last_exit: Option<ExtensionWorkerExit>,
    pub started_at_ms: i64,
    /// The retained log window, oldest first.
    pub logs: Vec<ExtensionLogLine>,
    /// The data schema of the data folder bound at start, when the package
    /// declares data (CORE-19).
    #[serde(default)]
    pub data_schema_version: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionQuarantineView {
    pub extension_id: ExtensionId,
    pub version: String,
    pub package_hash: String,
    pub reason: String,
    pub faults: u32,
    pub quarantined_at_ms: i64,
}

/// `ListExtensionInstances`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInstancesSnapshot {
    pub instances: Vec<ExtensionInstanceView>,
    pub quarantined: Vec<ExtensionQuarantineView>,
    /// Set when this profile's runtime state could not be read; starts are
    /// then refused.
    #[serde(default)]
    pub state_error: Option<String>,
    /// Calls whose worker stopped before answering, oldest first, with how
    /// each was settled (CORE-19). They are never replayed.
    #[serde(default)]
    pub unresolved: Vec<ExtensionUnresolvedCallView>,
    /// Set while safe mode keeps every extension from running.
    #[serde(default)]
    pub safe_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionCallResolutionOutcome {
    /// The extension reports that the call took effect.
    Applied,
    /// The extension reports that it did not; sending it again is safe.
    NotApplied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionCallResolution {
    pub outcome: ExtensionCallResolutionOutcome,
    #[serde(default)]
    pub detail: Option<String>,
    pub resolved_at_ms: i64,
    /// The version whose `luna.reconcile` answered.
    pub resolved_by_version: String,
    pub resolved_by_package_hash: String,
}

/// A call that reached a worker which stopped before answering: the
/// extension may or may not have acted on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionUnresolvedCallView {
    /// The id the worker saw in its `call` frame.
    pub call_id: String,
    /// The caller's request id, when it gave one.
    #[serde(default)]
    pub request_id: Option<String>,
    pub extension_id: ExtensionId,
    pub version: String,
    pub package_hash: String,
    pub instance_id: String,
    pub generation: u32,
    pub method: String,
    pub started_at_ms: i64,
    pub interrupted_at_ms: i64,
    pub exit_reason: ExtensionExitReason,
    /// Absent while the outcome is unknown.
    #[serde(default)]
    pub resolution: Option<ExtensionCallResolution>,
}

/// `StartExtensionInstance`. Unknown fields are refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartExtensionInstanceRequest {
    pub extension_id: ExtensionId,
    /// The exact enabled package hash.
    pub package_hash: String,
    /// The local task the instance acts on; absent means no target.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Operations the caller needs; the worker must advertise each by name.
    #[serde(default)]
    pub require: Vec<String>,
}

/// `CallExtensionInstance`. Unknown fields are refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallExtensionInstanceRequest {
    pub instance_id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    /// Caller deadline, at most [`ExtensionInstanceLimits::max_call_timeout_ms`].
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// The caller's id for this request (1 to 128 printable bytes). A
    /// request id whose earlier delivery has an unknown outcome, or was
    /// reconciled as applied, is refused instead of being sent again.
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionCallStatus {
    Ok,
    /// The worker answered with an error.
    Failed,
    /// The caller's deadline passed; the call was cancelled, never retried.
    DeadlineExceeded,
    /// The worker exited before answering; the call is never replayed.
    WorkerExited,
    /// The instance's call concurrency is in use; nothing was sent.
    Busy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionCallError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionCallOutcome {
    pub instance_id: String,
    /// The id the worker saw in its `call` frame.
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    /// The worker generation that received the call.
    pub generation: u32,
    pub status: ExtensionCallStatus,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<ExtensionCallError>,
    /// For `workerExited`: how that worker ended.
    #[serde(default)]
    pub exit: Option<ExtensionWorkerExit>,
    pub elapsed_ms: u64,
}

/// `StopExtensionInstance`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StopExtensionInstanceRequest {
    pub instance_id: String,
}

/// `ReleaseExtensionQuarantine`: the owner allows this exact package to run
/// again in this profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseExtensionQuarantineRequest {
    pub extension_id: ExtensionId,
    pub package_hash: String,
}
