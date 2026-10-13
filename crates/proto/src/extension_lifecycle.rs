//! CORE-19 extension lifecycle: version switches (update and rollback),
//! removal, plugin data and safe mode.
//!
//! A switch replaces the one enabled package of an extension id in a profile
//! with another installed package of the same id. Installing stages and
//! verifies the candidate; nothing changes until the owner activates it with
//! [`UpdateExtensionRequest`], which names both exact package hashes and the
//! exact grant set for the new one. Running instances are pinned to the hash
//! they started with: the switch drains them (waits for their calls, taking
//! no new ones) or deliberately stops them first, and a call whose worker
//! stopped before answering is recorded with an unknown outcome instead of
//! being sent again.
//!
//! Plugin data is profile-scoped and versioned by the package's
//! `data.schemaVersion`. A newer schema is produced by the new version's own
//! `luna.migrate` operation on a copy; the previous copy is kept so a
//! rollback restores it instead of letting older code read a newer schema.
//! Removing a package never erases its data; erasing data is its own command.
//!
//! Safe mode keeps every installed extension from running (grants and data
//! are untouched) while the owner disables, rolls back, removes or erases
//! them. LUNA's built-in providers are not extension packages and keep
//! working, offline as before.

use serde::{Deserialize, Serialize};

use crate::contract::{ExtensionId, NamespacedId};
use crate::extension_packages::ExtensionPackageView;

/// The worker operation a package implements to migrate its data forward.
/// Params `{fromSchemaVersion, toSchemaVersion, fromVersion, toVersion}`;
/// the worker reads and writes the copy through the `data.*` broker
/// operations. Any `ok` answer commits the copy; anything else discards it.
pub const EXTENSION_MIGRATE_OPERATION: &str = "luna.migrate";

/// The worker operation a package implements to settle calls whose outcome
/// is unknown. Params `{calls: [{callId, requestId?, method, version,
/// packageHash, generation, interruptedAtMs}]}`; answer `{results: [{callId,
/// outcome: "applied" | "notApplied" | "unknown", detail?}]}`.
pub const EXTENSION_RECONCILE_OPERATION: &str = "luna.reconcile";

/// Launch-time safe mode: when this environment variable is `1`, the engine
/// opens with extensions in safe mode for that run, whatever the profile says.
pub const EXTENSION_SAFE_MODE_ENV: &str = "LUNA_EXTENSIONS_SAFE_MODE";

/// What the instances of the enabled version do while the version switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionActiveRunPolicy {
    /// Take no new calls, wait for the calls in progress, then stop. If they
    /// are still running at the drain deadline, nothing changes.
    #[default]
    Drain,
    /// Stop now. Calls in progress end `workerExited`; their outcome is
    /// recorded as unknown and is never replayed.
    Stop,
}

/// `UpdateExtension`: switch this profile from one installed version of an
/// extension to another (an update or a rollback). Unknown fields are
/// refused; `grants` has no default, so the grant decision is always explicit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateExtensionRequest {
    pub extension_id: ExtensionId,
    /// The package enabled now; the switch is refused if another one is.
    pub from_package_hash: String,
    /// The installed package to enable instead.
    pub to_package_hash: String,
    /// The exact grant set for the new package. Nothing carries over from
    /// the old one: a capability the new version adds is granted only when
    /// it is named here.
    pub grants: Vec<NamespacedId>,
    #[serde(default)]
    pub active: ExtensionActiveRunPolicy,
    /// How long `drain` waits (default 30 s, at most 120 s).
    #[serde(default)]
    pub drain_timeout_ms: Option<u64>,
    /// The deadline of the `luna.migrate` call (default 30 s, at most 120 s).
    #[serde(default)]
    pub migration_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionSwitchKind {
    /// To a newer version.
    Update,
    /// To an older version.
    Rollback,
}

/// What happens to this profile's data for the extension on a switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExtensionDataPlan {
    /// The new version declares no data; existing data is kept untouched.
    None,
    /// Same schema: the current data carries over as it is.
    #[serde(rename_all = "camelCase")]
    Keep { schema_version: u32 },
    /// The profile has no data yet; the new version starts with an empty folder.
    #[serde(rename_all = "camelCase")]
    Fresh { schema_version: u32 },
    /// The new version migrates a copy forward with `luna.migrate`; the
    /// current data is kept for a rollback.
    #[serde(rename_all = "camelCase")]
    Migrate {
        from_schema_version: u32,
        to_schema_version: u32,
    },
    /// A kept copy with exactly the new version's schema becomes current
    /// again; the current data is kept too. Changes made since that copy
    /// was kept are not in it.
    #[serde(rename_all = "camelCase")]
    Restore {
        schema_version: u32,
        /// The version that last wrote the restored copy.
        written_by_version: String,
        kept_at_ms: i64,
    },
}

/// On a disabled package whose extension has another version enabled in
/// this profile: what switching to it involves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionSwitchView {
    pub kind: ExtensionSwitchKind,
    pub from_version: String,
    pub from_package_hash: String,
    /// Capabilities this package requests that the enabled one was not
    /// granted. They need a fresh decision; nothing grants them implicitly.
    pub new_capabilities: Vec<NamespacedId>,
    /// Grants of the enabled package that this one no longer requests.
    pub dropped_grants: Vec<NamespacedId>,
    pub data: ExtensionDataPlan,
    /// Instances of the extension running now (drained or stopped first).
    pub running_instances: u32,
    /// Why the switch is refused (dependencies, API, data, quarantine or
    /// safe mode), when it is.
    #[serde(default)]
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionUpdateOutcome {
    pub kind: ExtensionSwitchKind,
    /// The package now enabled.
    pub package: ExtensionPackageView,
    pub from_version: String,
    pub from_package_hash: String,
    /// Instances of the previous version that were drained or stopped.
    pub stopped_instances: Vec<String>,
    pub data: ExtensionDataPlan,
    /// New capabilities granted by this request (the review's renewed consent).
    pub expanded_grants: Vec<NamespacedId>,
}

/// `RemoveExtension`: delete one installed package from this device. If it
/// is enabled in this profile it is disabled first. Its data is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveExtensionRequest {
    pub extension_id: ExtensionId,
    pub package_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionRemoveOutcome {
    pub extension_id: ExtensionId,
    pub version: String,
    pub package_hash: String,
    /// It was enabled in this profile; its grant record was deleted.
    pub was_enabled: bool,
    pub stopped_instances: Vec<String>,
    /// Its quarantine record in this profile was released.
    pub quarantine_released: bool,
    /// This profile still holds data for the extension.
    pub data_kept: bool,
}

/// `EraseExtensionData`: delete this profile's data for one extension, every
/// kept copy included. Refused while an instance of it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EraseExtensionDataRequest {
    pub extension_id: ExtensionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionEraseOutcome {
    pub extension_id: ExtensionId,
    pub erased_copies: u32,
    pub erased_bytes: u64,
}

/// `SetExtensionSafeMode`. Replies with a fresh `ListExtensions` snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetExtensionSafeModeRequest {
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionSafeModeSource {
    /// Turned on in this profile; it lasts until the owner turns it off.
    Profile,
    /// Requested for this run at launch ([`EXTENSION_SAFE_MODE_ENV`]).
    Launch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionSafeModeView {
    pub source: ExtensionSafeModeSource,
    #[serde(default)]
    pub since_ms: Option<i64>,
    /// The profile setting, which outlasts a launch-time request.
    pub profile_enabled: bool,
}

/// One kept copy of an extension's data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionDataCopyView {
    pub schema_version: u32,
    /// The version that created or last migrated it.
    pub version: String,
    pub package_hash: String,
    pub created_at_ms: i64,
    pub size_bytes: u64,
}

/// This profile's data for one extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionDataView {
    pub extension_id: ExtensionId,
    pub current: ExtensionDataCopyView,
    /// Older copies kept for rollback, newest first.
    pub kept: Vec<ExtensionDataCopyView>,
    /// Some version of the extension is still installed on this device.
    pub installed: bool,
}
