//! CORE-06 extension packages: the version-1 manifest a local package carries
//! and the install, enablement and grant views the engine publishes.
//!
//! Installation and execution are separate states. Installing copies one
//! package into the device's package store, verifies every byte against its
//! manifest and leaves it disabled; it never runs package code. Enabling binds
//! one profile to one exact package hash and an explicit capability grant set.
//! Nothing here starts a worker: activation belongs to the extension
//! supervisor (CORE-07).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contract::{ContributionId, ExtensionId, NamespacedId};
use crate::extension_lifecycle::{ExtensionDataView, ExtensionSafeModeView, ExtensionSwitchView};

/// The manifest's file name at the package root. Its exact bytes are hashed
/// into the package hash, so it is never listed in its own `files`.
pub const EXTENSION_MANIFEST_FILE: &str = "luna-plugin.json";
pub const EXTENSION_MANIFEST_VERSION: u32 = 1;

/// Namespace reserved for LUNA's own packages; a local package cannot use it.
pub const RESERVED_EXTENSION_NAMESPACE: &str = "luna";
/// Namespace of the host's capability vocabulary.
pub const EXTENSION_CAPABILITY_NAMESPACE: &str = "luna.capability";
/// Implicitly requested, and always required, by every native package.
pub const NATIVE_CODE_CAPABILITY: &str = "luna.capability/native-code";

/// A host contract version: `major` must match, `minor` is additive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionContractVersion {
    pub major: u32,
    pub minor: u32,
}

/// The extension API this LUNA implements (manifest fields, contribution
/// kinds and the capability vocabulary). 1.1 (CORE-07) adds
/// `runtime.limits` and the broker operations in `extension_runtime`. 1.2
/// (CORE-19) adds `data.migratesFrom`, the `data.read`/`data.write` broker
/// operations and the `luna.migrate`/`luna.reconcile` worker operations.
pub const EXTENSION_API: ExtensionContractVersion = ExtensionContractVersion { major: 1, minor: 2 };
/// The native worker handshake (CORE-00: JSON lines, major/minor).
pub const EXTENSION_WORKER_PROTOCOL: ExtensionContractVersion =
    ExtensionContractVersion { major: 1, minor: 0 };
/// The portable profile (CORE-00: core Wasm modules, versioned LUNA imports, no WASI).
pub const EXTENSION_WASM_PROFILE: ExtensionContractVersion =
    ExtensionContractVersion { major: 1, minor: 0 };

/// What a package needs from the host: the same major, at least `minMinor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionContractRequirement {
    pub major: u32,
    #[serde(default)]
    pub min_minor: u32,
}

impl ExtensionContractRequirement {
    pub fn accepts(&self, host: ExtensionContractVersion) -> bool {
        self.major == host.major && self.min_minor <= host.minor
    }
}

/// One capability the host knows how to enforce and can explain to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionCapabilitySpec {
    pub id: &'static str,
    /// What granting it allows, in the user's terms.
    pub label: &'static str,
}

/// The version-1 capability vocabulary. A package may only request these;
/// the broker (CORE-07) enforces them. Adding one is a minor API change.
pub const EXTENSION_CAPABILITIES: &[ExtensionCapabilitySpec] = &[
    ExtensionCapabilitySpec {
        id: NATIVE_CODE_CAPABILITY,
        label: "Run native code on this device with your account's access. LUNA does not sandbox it.",
    },
    ExtensionCapabilitySpec {
        id: "luna.capability/provider.register",
        label: "Add agent providers and their models to the model picker",
    },
    ExtensionCapabilitySpec {
        id: "luna.capability/workspace.read",
        label: "Read files in a task's project folder",
    },
    ExtensionCapabilitySpec {
        id: "luna.capability/workspace.write",
        label: "Change files in a task's project folder",
    },
    ExtensionCapabilitySpec {
        id: "luna.capability/process.spawn",
        label: "Start programs on this device",
    },
    ExtensionCapabilitySpec {
        id: "luna.capability/network",
        label: "Connect to network services",
    },
    ExtensionCapabilitySpec {
        id: "luna.capability/tasks.read",
        label: "Read tasks and their conversations",
    },
];

pub fn extension_capability(id: &str) -> Option<&'static ExtensionCapabilitySpec> {
    EXTENSION_CAPABILITIES.iter().find(|spec| spec.id == id)
}

/// Contribution kinds the version-1 API accepts. Registries that place them
/// are CORE-08 (providers) and CORE-12 onwards (UI and renderers).
pub const EXTENSION_CONTRIBUTION_KINDS: &[&str] = &[
    "provider",
    "command",
    "theme",
    "settingsSection",
    "sidebarSection",
    "panel",
    "renderer",
];

/// `luna-plugin.json`, version 1. Unknown fields are refused, so an install
/// hook or any other field this host would not honour fails installation
/// instead of being silently ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionManifest {
    pub manifest_version: u32,
    pub id: ExtensionId,
    /// `MAJOR.MINOR.PATCH`. Together with `id` it names one immutable package.
    pub version: String,
    pub display_name: String,
    pub publisher: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    /// The extension API range (see [`EXTENSION_API`]).
    pub api: ExtensionContractRequirement,
    pub runtime: ExtensionRuntime,
    /// `os-arch` keys (`macos-aarch64`); empty means every platform.
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Packages that must be installed, and enabled in the same profile,
    /// in a matching version.
    #[serde(default)]
    pub dependencies: Vec<ExtensionDependency>,
    #[serde(default)]
    pub contributions: Vec<ExtensionContribution>,
    /// Capabilities the package asks the user to grant.
    #[serde(default)]
    pub capabilities: Vec<ExtensionCapabilityRequest>,
    /// JSON Schema for the package's per-profile settings.
    #[serde(default)]
    pub settings: Option<ExtensionSettingsSchema>,
    /// Version of the package-owned data layout, for namespaced migrations.
    #[serde(default)]
    pub data: Option<ExtensionDataSchema>,
    /// Every file in the package except the manifest, with its exact size
    /// and SHA-256. Installation refuses missing, extra or changed files.
    pub files: Vec<ExtensionFileEntry>,
}

/// Execution class. `data` packages never execute; `wasm` and `native` name
/// their entry points, which only the supervisor (CORE-07) may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionRuntimeKind {
    Data,
    Wasm,
    Native,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionRuntime {
    pub kind: ExtensionRuntimeKind,
    /// Worker protocol (`native`) or portable profile (`wasm`) requirement.
    #[serde(default)]
    pub protocol: Option<ExtensionContractRequirement>,
    /// The Wasm module, a listed file (`wasm` only).
    #[serde(default)]
    pub module: Option<String>,
    /// Platform key → listed executable file (`native` only).
    #[serde(default)]
    pub entries: BTreeMap<String, String>,
    /// API 1.1: resources the worker asks for, within the host's ceilings
    /// (`wasm` and `native` only). Absent fields take the host's defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<ExtensionRuntimeLimits>,
}

/// What one worker may use. The supervisor enforces these; the manifest can
/// only choose values inside the host's published range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionRuntimeLimits {
    /// Resident memory of the worker's whole process group, in MiB
    /// ([`EXTENSION_MEMORY_MIB_RANGE`]).
    #[serde(default, rename = "memoryMiB")]
    pub memory_mib: Option<u32>,
    /// CPU seconds each worker process may use (`RLIMIT_CPU`,
    /// [`EXTENSION_CPU_SECONDS_RANGE`]).
    #[serde(default)]
    pub cpu_seconds: Option<u32>,
}

/// Accepted `runtime.limits.memoryMiB` values, inclusive.
pub const EXTENSION_MEMORY_MIB_RANGE: (u32, u32) = (16, 4096);
/// Accepted `runtime.limits.cpuSeconds` values, inclusive.
pub const EXTENSION_CPU_SECONDS_RANGE: (u32, u32) = (1, 86_400);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionDependency {
    pub id: ExtensionId,
    /// Inclusive lower bound, `MAJOR.MINOR.PATCH`.
    pub min_version: String,
    /// Exclusive upper bound; absent means no upper bound.
    #[serde(default)]
    pub max_version: Option<String>,
}

/// A declared contribution. Its id must sit inside the package's own id:
/// `acme.tools/echo` may contribute `acme.tools/echo` or `acme.tools/echo.<x>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionContribution {
    pub id: ContributionId,
    pub kind: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionCapabilityRequest {
    pub id: NamespacedId,
    /// A required capability must be granted to enable the package.
    #[serde(default = "required_by_default")]
    pub required: bool,
    /// Why the package needs it, shown beside the host's own label.
    #[serde(default)]
    pub reason: Option<String>,
}

fn required_by_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionSettingsSchema {
    pub schema_version: u32,
    /// A listed JSON file holding the schema.
    pub schema: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionDataSchema {
    pub schema_version: u32,
    /// API 1.2: the oldest data schema this version migrates forward with
    /// its `luna.migrate` operation. Absent: it reads only `schemaVersion`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrates_from: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionFileEntry {
    /// Relative, `/`-separated; no `.`/`..`, absolute, empty or `\` segments.
    pub path: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the file's bytes.
    pub sha256: String,
}

/// How much authority executing the package implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionTrust {
    /// Declarations and assets only; nothing executes.
    Data,
    /// Wasm with only the broker imports it is granted.
    Portable,
    /// Native user-level code: explicitly trusted, not sandboxed.
    Native,
}

impl ExtensionRuntimeKind {
    pub fn trust(self) -> ExtensionTrust {
        match self {
            Self::Data => ExtensionTrust::Data,
            Self::Wasm => ExtensionTrust::Portable,
            Self::Native => ExtensionTrust::Native,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionOriginKind {
    /// Installed from a directory or archive on this device.
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionOrigin {
    pub kind: ExtensionOriginKind,
    /// The path it was installed from, as given. Provenance only: the
    /// installed copy never reads it again.
    pub source: String,
}

/// Per-profile state. Installing never enables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionState {
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionIntegrityStatus {
    /// Manifest hash and file sizes match (listings; no file is re-read).
    Checked,
    /// Every installed file was re-hashed and matches (inspect, enable).
    Verified,
    /// The installed copy changed or is missing; it cannot be enabled.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionIntegrity {
    pub status: ExtensionIntegrityStatus,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionCapabilityView {
    pub id: NamespacedId,
    /// The host's description, never the package's.
    pub label: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub required: bool,
    pub granted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPackageView {
    pub id: ExtensionId,
    pub version: String,
    /// SHA-256 of the exact manifest bytes; the manifest pins every file's
    /// hash, so this is the package's execution identity.
    pub package_hash: String,
    pub display_name: String,
    pub publisher: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    pub runtime: ExtensionRuntimeKind,
    pub trust: ExtensionTrust,
    pub origin: ExtensionOrigin,
    pub installed_at_ms: i64,
    pub size_bytes: u64,
    pub file_count: u32,
    pub state: ExtensionState,
    #[serde(default)]
    pub enabled_at_ms: Option<i64>,
    pub integrity: ExtensionIntegrity,
    /// Requested capabilities (native packages include
    /// [`NATIVE_CODE_CAPABILITY`]) and whether this profile granted each.
    pub capabilities: Vec<ExtensionCapabilityView>,
    pub contributions: Vec<ExtensionContribution>,
    pub dependencies: Vec<ExtensionDependency>,
    #[serde(default)]
    pub settings_schema_version: Option<u32>,
    #[serde(default)]
    pub data_schema_version: Option<u32>,
    /// The oldest data schema this package migrates forward (API 1.2).
    #[serde(default)]
    pub data_migrates_from: Option<u32>,
    /// Why this exact package hash is quarantined in this profile.
    #[serde(default)]
    pub quarantine_reason: Option<String>,
    /// Set when another version of the same extension is enabled in this
    /// profile: what switching to this package involves (CORE-19).
    #[serde(default)]
    pub switch: Option<ExtensionSwitchView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionHostInfo {
    pub api: ExtensionContractVersion,
    pub worker_protocol: ExtensionContractVersion,
    pub wasm_profile: ExtensionContractVersion,
    /// This host's platform key, e.g. `macos-aarch64`.
    pub platform: String,
}

/// `ListExtensions`: everything installed on this device, with this
/// profile's enablement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionsSnapshot {
    pub host: ExtensionHostInfo,
    pub packages: Vec<ExtensionPackageView>,
    /// Set when this profile's grants file could not be read. Every package
    /// then shows as disabled and enablement changes are refused.
    #[serde(default)]
    pub grants_error: Option<String>,
    /// Set while safe mode keeps every extension from running.
    #[serde(default)]
    pub safe_mode: Option<ExtensionSafeModeView>,
    /// This profile's extension data, including data whose package was
    /// removed (it stays until it is erased).
    #[serde(default)]
    pub data: Vec<ExtensionDataView>,
}

/// `InstallExtension`. Unknown fields are refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallExtensionRequest {
    /// Absolute path of a package directory, or of a `.tar`/`.tar.gz` archive.
    pub path: String,
    /// When set, the install is refused unless the package hash is this one
    /// (bind an install to the candidate a user previewed).
    #[serde(default)]
    pub expected_package_hash: Option<String>,
}

/// `InspectExtension`: one package, with every installed file re-hashed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InspectExtensionRequest {
    pub package_hash: String,
}

/// `SetExtensionEnabled`. Enabling names the exact package hash and the
/// exact capability set granted; there is no "grant everything" or
/// "confirmed" shortcut, and unknown fields are refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetExtensionEnabledRequest {
    pub extension_id: ExtensionId,
    pub package_hash: String,
    pub enabled: bool,
    #[serde(default)]
    pub grants: Vec<NamespacedId>,
}
