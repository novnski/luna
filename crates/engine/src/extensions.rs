//! CORE-06 extension manager: local package installation, per-profile
//! enablement and explicit capability grants.
//!
//! Scopes:
//! - **Installation is device-scoped** (`{device root}/extensions/`), outside
//!   the app bundle. Packages are immutable and content-addressed by their
//!   package hash.
//! - **Enablement and grants are profile-scoped** (`{store root}/
//!   extension-grants.json`), bound to the profile id and never synced.
//!
//! Installing never executes package code and never enables: a new package is
//! a disabled candidate. Enabling names the exact package hash and the exact
//! capabilities granted. Nothing here activates a package; the supervisor
//! (CORE-07, `runtime`) starts workers only for enabled, re-verified packages.
//!
//! CORE-19 lifecycle, on the same owners:
//! - **Version switch** (`update`): an installed (staged, verified) candidate
//!   of the same id replaces the enabled package only on explicit request,
//!   with its own exact grant set. The old version's instances are drained
//!   or deliberately stopped first; a newer data schema is produced by the
//!   new version's `luna.migrate` on a copy. The grants file's rename is the
//!   single commit point for enablement and data, so a crash leaves either
//!   the old version with its data or the new one with its data.
//! - **Removal** deletes the package from the device (disabling it here
//!   first) and releases its quarantine; **erasing data** is separate.
//! - **Safe mode** refuses every start; management keeps working.

mod data;
mod grants;
mod manifest;
/// CORE-08: enabled provider contributions in the harness registry.
pub(crate) mod providers;
mod runtime;
mod store;
/// CORE-12: enabled packages' UI contributions for the native shell.
mod ui;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use luna_proto::contract::{ExtensionId, NamespacedId};
use luna_proto::{
    CallExtensionInstanceRequest, EXTENSION_API, EXTENSION_MANIFEST_FILE,
    EXTENSION_MIGRATE_OPERATION, EXTENSION_SAFE_MODE_ENV, EXTENSION_WASM_PROFILE,
    EXTENSION_WORKER_PROTOCOL, EraseExtensionDataRequest, ExtensionActiveRunPolicy,
    ExtensionCallOutcome, ExtensionCallStatus, ExtensionCapabilityView, ExtensionDataCopyView,
    ExtensionDataPlan, ExtensionDataView, ExtensionEraseOutcome, ExtensionHostInfo,
    ExtensionInstancePurpose, ExtensionInstanceView, ExtensionInstancesSnapshot,
    ExtensionIntegrity, ExtensionIntegrityStatus, ExtensionOrigin, ExtensionOriginKind,
    ExtensionPackageView, ExtensionRemoveOutcome, ExtensionRuntimeKind, ExtensionSafeModeSource,
    ExtensionSafeModeView, ExtensionState, ExtensionSwitchKind, ExtensionSwitchView,
    ExtensionUpdateOutcome, ExtensionsSnapshot, InstallExtensionRequest, NATIVE_CODE_CAPABILITY,
    ReleaseExtensionQuarantineRequest, RemoveExtensionRequest, SetExtensionEnabledRequest,
    SetExtensionSafeModeRequest, StartExtensionInstanceRequest, UpdateExtensionRequest,
    extension_capability,
};

use crate::WorkspaceHost;
use crate::device_write::DeviceWriteHandle;
use grants::{DataGeneration, DataRecord, GrantRecord, GrantsFile, ProfileGrants};
use manifest::ValidatedManifest;
use store::{InstalledRecord, PackageStore};

/// The default and longest drain of a version switch, and of its migration.
const DEFAULT_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub(crate) struct ExtensionManager(Arc<Inner>);

struct Inner {
    /// Set when the store's roots could not be resolved; every call refuses.
    unavailable: Option<String>,
    packages: PackageStore,
    grants: ProfileGrants,
    writes: DeviceWriteHandle,
    /// Serializes install, enablement and recovery within this engine. The
    /// device InstanceLock already excludes other engines.
    gate: Mutex<()>,
    platform: String,
    /// CORE-07 supervised instances and their broker.
    runtime: runtime::ExtensionRuntime,
    /// CORE-19 plugin data in this profile.
    data: data::DataStore,
    /// Serializes version switches, removals, data erasure and safe-mode
    /// changes; each may wait for workers, so it is an async lock.
    lifecycle: tokio::sync::Mutex<()>,
    /// Extension ids being switched, removed or erased: their starts,
    /// restarts and enablement changes are refused until that finishes.
    transitions: Mutex<HashSet<String>>,
    /// CORE-08 provider contributions registered in the harness registry.
    providers: providers::ProviderHost,
    /// CORE-12 change notifications for the shell's UI contributions.
    ui: ui::UiHost,
}

/// An extension id claimed for one lifecycle change; released on drop.
struct Claim {
    manager: ExtensionManager,
    id: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.manager
            .0
            .transitions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

/// What a version switch does with this profile's data.
#[derive(Debug, Clone)]
enum DataStep {
    /// The new version declares no data.
    None,
    Keep(u32),
    Fresh(u32),
    Migrate {
        from: DataGeneration,
        to_schema: u32,
    },
    Restore(DataGeneration),
}

impl DataStep {
    fn view(&self) -> ExtensionDataPlan {
        match self {
            Self::None => ExtensionDataPlan::None,
            Self::Keep(schema) => ExtensionDataPlan::Keep {
                schema_version: *schema,
            },
            Self::Fresh(schema) => ExtensionDataPlan::Fresh {
                schema_version: *schema,
            },
            Self::Migrate { from, to_schema } => ExtensionDataPlan::Migrate {
                from_schema_version: from.schema_version,
                to_schema_version: *to_schema,
            },
            Self::Restore(kept) => ExtensionDataPlan::Restore {
                schema_version: kept.schema_version,
                written_by_version: kept.version.clone(),
                kept_at_ms: kept.created_at_ms,
            },
        }
    }

    /// Whether the step runs the new version's code (refused in safe mode).
    fn runs_code(&self) -> bool {
        matches!(self, Self::Migrate { .. })
    }
}

/// A checked switch from the enabled package to another installed one.
#[derive(Debug, Clone)]
struct SwitchPlan {
    kind: ExtensionSwitchKind,
    from: GrantRecord,
    data: DataStep,
    new_capabilities: Vec<String>,
    dropped_grants: Vec<String>,
}

/// Everything a version switch verified before it stopped anything.
#[derive(Clone)]
struct PreparedUpdate {
    to: InstalledRecord,
    validated: ValidatedManifest,
    grants: Vec<String>,
    plan: SwitchPlan,
}

impl ExtensionManager {
    /// Open the store and clear what an interrupted install or grant write
    /// left. Never fails engine assembly: a broken store reports itself on
    /// every call while the core keeps running without extensions. Nothing
    /// is started: instances run only when the owner starts them.
    pub(crate) fn open(
        device_root: &Path,
        store_root: &Path,
        profile_id: String,
        writes: DeviceWriteHandle,
        workspace: WorkspaceHost,
    ) -> Self {
        let roots = device_root
            .canonicalize()
            .and_then(|device| store_root.canonicalize().map(|store| (device, store)));
        let (unavailable, device, store) = match roots {
            Ok((device, store)) => (None, device, store),
            Err(error) => (
                Some(format!(
                    "Extensions are unavailable on this engine: {error}"
                )),
                device_root.to_path_buf(),
                store_root.to_path_buf(),
            ),
        };
        let launch_safe_mode =
            std::env::var(EXTENSION_SAFE_MODE_ENV).is_ok_and(|value| value.trim() == "1");
        if launch_safe_mode {
            tracing::warn!(
                "extensions start in safe mode for this run ({EXTENSION_SAFE_MODE_ENV})"
            );
        }
        let manager = Self(Arc::new(Inner {
            unavailable,
            packages: PackageStore::new(&device),
            grants: ProfileGrants::new(&store, profile_id.clone()),
            runtime: runtime::ExtensionRuntime::open(
                &store,
                profile_id,
                writes.clone(),
                Some(workspace),
                launch_safe_mode,
            ),
            data: data::DataStore::new(&store),
            writes,
            gate: Mutex::new(()),
            platform: manifest::host_platform(),
            lifecycle: tokio::sync::Mutex::new(()),
            transitions: Mutex::new(HashSet::new()),
            providers: providers::ProviderHost::default(),
            ui: ui::UiHost::default(),
        }));
        if manager.0.unavailable.is_none() {
            manager.recover();
        }
        manager
    }

    fn recover(&self) {
        let Ok(_permit) = self.0.writes.try_admit() else {
            tracing::warn!("extension recovery skipped: the device writer is unavailable");
            return;
        };
        let _gate = self.gate();
        match self.0.packages.recover() {
            Ok(recovered) if recovered.staging > 0 || recovered.orphans > 0 => tracing::warn!(
                staging = recovered.staging,
                orphans = recovered.orphans,
                "removed interrupted extension installs"
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "extension store recovery failed"),
        }
        match self.0.grants.recover() {
            Ok(0) => {}
            Ok(removed) => tracing::warn!(removed, "removed interrupted extension grant writes"),
            Err(error) => tracing::warn!(%error, "extension grant recovery failed"),
        }
        // Data copies the grants file never committed, and erases that were
        // interrupted. An unreadable grants file leaves every folder alone.
        match self.0.grants.read() {
            Ok(file) => match self.0.data.recover(file.data.iter()) {
                Ok(0) => {}
                Ok(removed) => {
                    tracing::warn!(removed, "removed uncommitted extension data copies")
                }
                Err(error) => tracing::warn!(%error, "extension data recovery failed"),
            },
            Err(error) => tracing::warn!(%error, "extension data recovery skipped"),
        }
        // A removal interrupted before it released its quarantine record.
        if let Ok(index) = self.0.packages.read_index() {
            let installed: Vec<String> = index
                .into_iter()
                .map(|record| record.package_hash)
                .collect();
            self.0.runtime.prune_quarantine(&installed);
        }
    }

    fn claim(&self, id: &ExtensionId) -> Result<Claim, String> {
        let mut transitions = self
            .0
            .transitions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !transitions.insert(id.as_str().to_owned()) {
            return Err(format!(
                "{} is already being changed; retry when that finishes",
                id.0
            ));
        }
        Ok(Claim {
            manager: self.clone(),
            id: id.as_str().to_owned(),
        })
    }

    /// `Err` while `id` is being switched, removed or erased.
    fn check_settled(&self, id: &str) -> Result<(), String> {
        let busy = self
            .0
            .transitions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(id);
        if busy {
            Err(format!(
                "{id} is being updated, removed or erased; retry when that finishes"
            ))
        } else {
            Ok(())
        }
    }

    fn gate(&self) -> MutexGuard<'_, ()> {
        self.0.gate.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn available(&self) -> Result<(), String> {
        match &self.0.unavailable {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }

    fn host(&self) -> ExtensionHostInfo {
        ExtensionHostInfo {
            api: EXTENSION_API,
            worker_protocol: EXTENSION_WORKER_PROTOCOL,
            wasm_profile: EXTENSION_WASM_PROFILE,
            platform: self.0.platform.clone(),
        }
    }

    /// Every installed package with this profile's enablement. Integrity is
    /// the cheap check (manifest hash and file sizes).
    pub(crate) fn snapshot(&self) -> Result<ExtensionsSnapshot, String> {
        self.available()?;
        let _gate = self.gate();
        let index = self.0.packages.read_index()?;
        let (grants, grants_error) = match self.0.grants.read() {
            Ok(file) => (Some(file), None),
            Err(error) => (None, Some(error)),
        };
        let packages = index
            .iter()
            .map(|record| {
                let grant = grants
                    .as_ref()
                    .and_then(|file| file.extensions.get(record.id.as_str()));
                match self.0.packages.read_manifest(&record.package_hash) {
                    Ok(validated) => {
                        let integrity = self.0.packages.integrity(&validated, false);
                        let mut package = view(record, Some(&validated), grant, integrity);
                        self.decorate(&mut package, &index, grants.as_ref(), record, &validated);
                        package
                    }
                    Err(detail) => {
                        let mut package = view(record, None, grant, failed(detail));
                        package.quarantine_reason =
                            self.0.runtime.quarantine_reason(&record.package_hash);
                        package
                    }
                }
            })
            .collect();
        Ok(ExtensionsSnapshot {
            host: self.host(),
            packages,
            grants_error,
            safe_mode: self.safe_mode_view(),
            data: grants
                .as_ref()
                .map(|file| self.data_views(file, &index))
                .unwrap_or_default(),
        })
    }

    /// The CORE-19 parts of a package view: its quarantine and, when another
    /// version of it is enabled here, what switching to it involves.
    fn decorate(
        &self,
        package: &mut ExtensionPackageView,
        index: &[InstalledRecord],
        file: Option<&GrantsFile>,
        record: &InstalledRecord,
        validated: &ValidatedManifest,
    ) {
        package.quarantine_reason = self.0.runtime.quarantine_reason(&record.package_hash);
        let Some(file) = file else {
            return;
        };
        let Some(enabled) = file.extensions.get(record.id.as_str()) else {
            return;
        };
        if enabled.package_hash == record.package_hash {
            return;
        }
        let running_instances = self.0.runtime.live_count(&record.id) as u32;
        let ids = |ids: &[String]| -> Vec<NamespacedId> {
            ids.iter()
                .filter_map(|id| NamespacedId::parse(id).ok())
                .collect()
        };
        package.switch = Some(match self.plan_switch(index, file, record, validated) {
            Ok(plan) => {
                let blocked = (plan.data.runs_code() && !matches!(self.0.runtime.safe_mode(), Ok(None)))
                    .then(|| {
                        "safe mode is on, and this switch migrates data with the new version's code; turn safe mode off first"
                            .to_string()
                    });
                ExtensionSwitchView {
                    kind: plan.kind,
                    from_version: plan.from.version.clone(),
                    from_package_hash: plan.from.package_hash.clone(),
                    new_capabilities: ids(&plan.new_capabilities),
                    dropped_grants: ids(&plan.dropped_grants),
                    data: plan.data.view(),
                    running_instances,
                    blocked,
                }
            }
            Err(reason) => ExtensionSwitchView {
                kind: switch_kind(&enabled.version, &record.version),
                from_version: enabled.version.clone(),
                from_package_hash: enabled.package_hash.clone(),
                new_capabilities: Vec::new(),
                dropped_grants: Vec::new(),
                data: ExtensionDataPlan::None,
                running_instances,
                blocked: Some(reason),
            },
        });
    }

    fn safe_mode_view(&self) -> Option<ExtensionSafeModeView> {
        let (launch, since_ms) = self.0.runtime.safe_mode().ok()??;
        Some(ExtensionSafeModeView {
            source: if launch {
                ExtensionSafeModeSource::Launch
            } else {
                ExtensionSafeModeSource::Profile
            },
            since_ms,
            profile_enabled: self.0.runtime.profile_safe_mode(),
        })
    }

    fn data_views(&self, file: &GrantsFile, index: &[InstalledRecord]) -> Vec<ExtensionDataView> {
        file.data
            .iter()
            .filter_map(|(id, record)| {
                let extension_id = ExtensionId::parse(id).ok()?;
                let copy = |generation: &DataGeneration| ExtensionDataCopyView {
                    schema_version: generation.schema_version,
                    version: generation.version.clone(),
                    package_hash: generation.package_hash.clone(),
                    created_at_ms: generation.created_at_ms,
                    size_bytes: self.0.data.size(&extension_id, &generation.id),
                };
                let current = copy(&record.current);
                let kept = record.kept.iter().map(copy).collect();
                let installed = index.iter().any(|installed| installed.id == extension_id);
                Some(ExtensionDataView {
                    extension_id,
                    current,
                    kept,
                    installed,
                })
            })
            .collect()
    }

    /// One installed package with every file re-hashed.
    pub(crate) fn inspect(&self, package_hash: &str) -> Result<ExtensionPackageView, String> {
        self.available()?;
        check_hash(package_hash)?;
        let _gate = self.gate();
        let index = self.0.packages.read_index()?;
        let record = installed(&index, package_hash)?;
        let grants = self.0.grants.read().ok();
        let grant = grants
            .as_ref()
            .and_then(|file| file.extensions.get(record.id.as_str()));
        Ok(match self.0.packages.read_manifest(package_hash) {
            Ok(validated) => {
                let integrity = self.0.packages.integrity(&validated, true);
                let mut package = view(record, Some(&validated), grant, integrity);
                self.decorate(&mut package, &index, grants.as_ref(), record, &validated);
                package
            }
            Err(detail) => view(record, None, grant, failed(detail)),
        })
    }

    /// Stage, verify and commit one local package as a disabled candidate.
    /// Reinstalling identical bytes returns the existing package.
    pub(crate) fn install(
        &self,
        request: &InstallExtensionRequest,
    ) -> Result<ExtensionPackageView, String> {
        self.available()?;
        if let Some(expected) = &request.expected_package_hash {
            check_hash(expected)?;
        }
        let _permit = self.0.writes.try_admit().map_err(|error| {
            format!("Extensions are temporarily unavailable ({error:?}); retry")
        })?;
        let _gate = self.gate();
        let staged = self.0.packages.stage(Path::new(&request.path))?;
        let manifest_bytes = store::read_regular_file(
            &staged.tree().join(EXTENSION_MANIFEST_FILE),
            manifest::MAX_MANIFEST_BYTES as u64,
        )
        .map_err(|error| {
            format!("the package has no readable {EXTENSION_MANIFEST_FILE}: {error}")
        })?;
        let validated = manifest::validate(&manifest_bytes)?;
        if let Some(expected) = &request.expected_package_hash
            && *expected != validated.package_hash
        {
            return Err(format!(
                "the package is not the one reviewed: expected {expected}, found {}",
                validated.package_hash
            ));
        }
        store::verify_tree(staged.tree(), &validated)?;

        let index = self.0.packages.read_index()?;
        let grants = self.0.grants.read().ok();
        let manifest = &validated.manifest;
        if let Some(existing) = index
            .iter()
            .find(|record| record.package_hash == validated.package_hash)
        {
            let grant = grants
                .as_ref()
                .and_then(|file| file.extensions.get(existing.id.as_str()));
            let integrity = self.0.packages.integrity(&validated, false);
            return Ok(view(existing, Some(&validated), grant, integrity));
        }
        if index
            .iter()
            .any(|record| record.id == manifest.id && record.version == manifest.version)
        {
            return Err(format!(
                "{} {} is already installed with different contents; a package version never changes",
                manifest.id.0, manifest.version
            ));
        }
        for dependency in &manifest.dependencies {
            let range =
                manifest::range_label(&dependency.min_version, dependency.max_version.as_deref());
            let versions: Vec<&str> = index
                .iter()
                .filter(|record| record.id == dependency.id)
                .map(|record| record.version.as_str())
                .collect();
            if versions.is_empty() {
                return Err(format!(
                    "requires {} {range}, which is not installed",
                    dependency.id.0
                ));
            }
            if !versions.iter().any(|version| {
                manifest::version_in_range(
                    version,
                    &dependency.min_version,
                    dependency.max_version.as_deref(),
                )
            }) {
                return Err(format!(
                    "requires {} {range}; installed: {}",
                    dependency.id.0,
                    versions.join(", ")
                ));
            }
        }

        let record = InstalledRecord {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            package_hash: validated.package_hash.clone(),
            display_name: manifest.display_name.clone(),
            publisher: manifest.publisher.clone(),
            runtime: manifest.runtime.kind,
            origin: ExtensionOrigin {
                kind: ExtensionOriginKind::Local,
                source: request.path.clone(),
            },
            installed_at_ms: now_ms(),
            size_bytes: validated.total_size(),
            file_count: manifest.files.len() as u32,
        };
        let record = self.0.packages.commit(staged, &validated, record)?;
        // A package hash is enabled only by an explicit change after install.
        let integrity = ExtensionIntegrity {
            status: ExtensionIntegrityStatus::Verified,
            detail: None,
        };
        Ok(view(&record, Some(&validated), None, integrity))
    }

    /// Enable one exact installed package with exactly `grants`, or disable it.
    pub(crate) fn set_enabled(
        &self,
        request: &SetExtensionEnabledRequest,
    ) -> Result<ExtensionPackageView, String> {
        self.available()?;
        check_hash(&request.package_hash)?;
        let _permit = self.0.writes.try_admit().map_err(|error| {
            format!("Extensions are temporarily unavailable ({error:?}); retry")
        })?;
        let _gate = self.gate();
        let index = self.0.packages.read_index()?;
        let record = installed(&index, &request.package_hash)?;
        if record.id != request.extension_id {
            return Err(format!(
                "package {} is {}, not {}",
                request.package_hash, record.id.0, request.extension_id.0
            ));
        }
        let id = record.id.as_str();
        self.check_settled(id)?;
        let mut file = self.0.grants.read()?;

        if !request.enabled {
            match file.extensions.get(id) {
                None => {
                    let validated = self.0.packages.read_manifest(&record.package_hash).ok();
                    let integrity = validated.as_ref().map_or_else(
                        || failed("the installed manifest could not be read".into()),
                        |validated| self.0.packages.integrity(validated, false),
                    );
                    return Ok(view(record, validated.as_ref(), None, integrity));
                }
                Some(existing) if existing.package_hash != record.package_hash => {
                    return Err(format!(
                        "{id} {} is the enabled version; disable that package instead",
                        existing.version
                    ));
                }
                Some(_) => {}
            }
            for (other, enabled) in &file.extensions {
                if other == id {
                    continue;
                }
                // An unreadable dependent cannot run either; it does not block.
                let depends = self
                    .0
                    .packages
                    .read_manifest(&enabled.package_hash)
                    .is_ok_and(|other| {
                        other
                            .manifest
                            .dependencies
                            .iter()
                            .any(|dependency| dependency.id == record.id)
                    });
                if depends {
                    return Err(format!("{other} depends on {id}; disable it first"));
                }
            }
            file.extensions.remove(id);
            self.0.grants.write(file)?;
            // Running instances lose their authority with the grant.
            self.0.runtime.revoke(&record.id, None);
            self.providers_changed();
            self.ui_changed();
            let validated = self.0.packages.read_manifest(&record.package_hash).ok();
            let integrity = validated.as_ref().map_or_else(
                || failed("the installed manifest could not be read".into()),
                |validated| self.0.packages.integrity(validated, false),
            );
            return Ok(view(record, validated.as_ref(), None, integrity));
        }

        let validated = self.0.packages.read_manifest(&record.package_hash)?;
        let granted = validate_grants(&validated, &request.grants, id)?;
        if let Some(existing) = file.extensions.get(id) {
            if existing.package_hash != record.package_hash {
                return Err(format!(
                    "{id} {} is enabled; disable it before enabling {}, or update to it",
                    existing.version, record.version
                ));
            }
            if existing.grants == granted {
                let integrity = self.0.packages.integrity(&validated, false);
                return Ok(view(record, Some(&validated), Some(existing), integrity));
            }
        }
        let integrity = self.0.packages.integrity(&validated, true);
        if integrity.status == ExtensionIntegrityStatus::Failed {
            return Err(format!(
                "the installed package failed verification ({}); reinstall it",
                integrity.detail.as_deref().unwrap_or("unknown change")
            ));
        }
        check_dependencies(&validated, &file, &index)?;
        // Enabling never migrates: a package reads exactly the schema of the
        // data this profile holds, or starts the extension's data afresh.
        let mut fresh = None;
        if let Some(schema) = validated
            .manifest
            .data
            .as_ref()
            .map(|data| data.schema_version)
        {
            let existing = file
                .data
                .get(id)
                .map(|data| (data.current.schema_version, data.current.version.clone()));
            match existing {
                Some((current, written_by)) if current != schema => {
                    return Err(format!(
                        "this profile's data for {id} is schema {current}, written by {written_by}; {} reads schema {schema}. Enable a version that reads it and update from there, or erase the data first",
                        record.version
                    ));
                }
                Some(_) => {}
                None => fresh = Some(self.fresh_data(&mut file, record, schema)?),
            }
        }
        file.extensions.insert(
            id.to_owned(),
            GrantRecord {
                package_hash: record.package_hash.clone(),
                version: record.version.clone(),
                grants: granted,
                enabled_at_ms: now_ms(),
            },
        );
        let file = match self.0.grants.write(file) {
            Ok(file) => file,
            Err(error) => {
                if let Some(generation) = fresh {
                    self.0.data.remove_generation(&record.id, &generation);
                }
                return Err(error);
            }
        };
        if let Some(enabled) = file.extensions.get(id) {
            // An instance bound to the previous grant set stops.
            self.0.runtime.revoke(
                &record.id,
                Some((enabled.package_hash.as_str(), enabled.grants.as_slice())),
            );
        }
        self.providers_changed();
        self.ui_changed();
        Ok(view(
            record,
            Some(&validated),
            file.extensions.get(id),
            integrity,
        ))
    }

    /// Every instance this engine started, and this profile's quarantine.
    pub(crate) fn instances(&self) -> Result<ExtensionInstancesSnapshot, String> {
        self.available()?;
        Ok(self.0.runtime.snapshot())
    }

    /// Verify the exact enabled package and its grants, then start a worker.
    pub(crate) async fn start_instance(
        &self,
        request: StartExtensionInstanceRequest,
    ) -> Result<ExtensionInstanceView, String> {
        self.available()?;
        check_hash(&request.package_hash)?;
        self.0.runtime.check_startable(&request.package_hash)?;
        let weak = Arc::downgrade(&self.0);
        let prepare = request.clone();
        let relaunch: runtime::Relaunch = Arc::new(move || {
            let inner = weak
                .upgrade()
                .ok_or_else(|| "the engine is shutting down".to_string())?;
            ExtensionManager(inner).prepare_launch(&prepare)
        });
        let first = relaunch.clone();
        let plan = tokio::task::spawn_blocking(move || first())
            .await
            .map_err(|error| format!("the extension could not be prepared: {error}"))??;
        self.0.runtime.start(plan, relaunch).await
    }

    pub(crate) async fn call_instance(
        &self,
        request: CallExtensionInstanceRequest,
    ) -> Result<ExtensionCallOutcome, String> {
        self.available()?;
        self.0.runtime.call(request).await
    }

    pub(crate) async fn stop_instance(
        &self,
        instance_id: &str,
    ) -> Result<ExtensionInstanceView, String> {
        self.available()?;
        self.0.runtime.stop(instance_id).await
    }

    pub(crate) async fn release_quarantine(
        &self,
        request: ReleaseExtensionQuarantineRequest,
    ) -> Result<ExtensionInstancesSnapshot, String> {
        self.available()?;
        check_hash(&request.package_hash)?;
        self.0
            .runtime
            .release_quarantine(&request.extension_id, &request.package_hash)
            .await?;
        Ok(self.0.runtime.snapshot())
    }

    /// Stop every worker; called once while the engine shuts down.
    pub(crate) async fn shutdown(&self) {
        self.0.runtime.shutdown().await;
    }

    /// Everything one start needs, verified now: the exact installed hash is
    /// enabled in this profile, the stored grant still fits the manifest,
    /// every file re-hashes to the manifest, the package is not quarantined,
    /// safe mode is off, the extension is not being switched, and the target
    /// task belongs to this device. A package with data gets the profile's
    /// current data generation, created on first use. Blocking I/O.
    fn prepare_launch(
        &self,
        request: &StartExtensionInstanceRequest,
    ) -> Result<runtime::LaunchPlan, String> {
        self.available()?;
        check_hash(&request.package_hash)?;
        if let Some(operation) = request
            .require
            .iter()
            .find(|operation| operation.is_empty() || operation.len() > 128)
        {
            return Err(format!("{operation:?} is not an operation name"));
        }
        let _gate = self.gate();
        let index = self.0.packages.read_index()?;
        let record = installed(&index, &request.package_hash)?;
        if record.id != request.extension_id {
            return Err(format!(
                "package {} is {}, not {}",
                request.package_hash, record.id.0, request.extension_id.0
            ));
        }
        let id = record.id.as_str();
        self.check_settled(id)?;
        let mut file = self.0.grants.read()?;
        let grant = file
            .extensions
            .get(id)
            .filter(|grant| grant.package_hash == record.package_hash)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "{id} {} is not enabled in this profile; enable that exact package first",
                    record.version
                )
            })?;
        let validated = self.0.packages.read_manifest(&record.package_hash)?;
        let integrity = self.0.packages.integrity(&validated, true);
        if integrity.status == ExtensionIntegrityStatus::Failed {
            return Err(format!(
                "the installed package failed verification ({}); it was not started",
                integrity.detail.as_deref().unwrap_or("unknown change")
            ));
        }
        for granted in &grant.grants {
            if !validated
                .capabilities
                .iter()
                .any(|capability| capability.id.as_str() == granted)
            {
                return Err(format!(
                    "the stored grant names {granted}, which the package does not request"
                ));
            }
        }
        if let Some(missing) = validated.capabilities.iter().find(|capability| {
            capability.required && !grant.grants.iter().any(|g| g == capability.id.as_str())
        }) {
            return Err(format!("{} is required but not granted", missing.id));
        }
        self.0.runtime.check_startable(&record.package_hash)?;
        let (spec, limits) =
            self.launch_spec(record, &validated, &grant.grants, &request.require)?;
        let target = match &request.task_id {
            None => runtime::BoundTarget::None,
            Some(task_id) => self.0.runtime.bind_task(task_id)?,
        };
        let (data_root, data_schema_version) = self.bind_data(&mut file, record, &validated)?;
        Ok(runtime::LaunchPlan {
            extension_id: record.id.clone(),
            version: record.version.clone(),
            package_hash: record.package_hash.clone(),
            runtime: validated.manifest.runtime.kind,
            grants: grant.grants.clone(),
            target,
            spec,
            limits,
            data_root,
            data_schema_version,
            purpose: ExtensionInstancePurpose::Run,
        })
    }

    /// How to run `record` with `grants` (no profile checks).
    fn launch_spec(
        &self,
        record: &InstalledRecord,
        validated: &ValidatedManifest,
        grants: &[String],
        require: &[String],
    ) -> Result<(runtime::LaunchSpec, runtime::Limits), String> {
        let id = record.id.as_str();
        let package_dir = self.0.packages.package_dir(&record.package_hash);
        let runtime_spec = &validated.manifest.runtime;
        match runtime_spec.kind {
            ExtensionRuntimeKind::Data => {
                Err(format!("{id} is a data package; it has nothing to run"))
            }
            ExtensionRuntimeKind::Native => {
                if !grants.iter().any(|g| g == NATIVE_CODE_CAPABILITY) {
                    return Err(format!(
                        "{NATIVE_CODE_CAPABILITY} is not granted, so native code cannot run"
                    ));
                }
                let entry = runtime_spec.entries.get(&self.0.platform).ok_or_else(|| {
                    format!(
                        "{id} has no native entry point for this device ({})",
                        self.0.platform
                    )
                })?;
                let (mut spec, limits) =
                    runtime::native_launch(&package_dir, entry, runtime_spec.limits, require);
                // CORE-11: LUNA's own worker hosts an adapter that ran inside
                // LUNA with its whole environment (provider keys, config and
                // data directories, QA wrappers); it keeps it. Its instance
                // reports the allow-list as not enforced.
                if providers::first_party::is_first_party(&record.id) {
                    spec.environment = std::env::vars_os().collect();
                }
                Ok((spec, limits))
            }
            ExtensionRuntimeKind::Wasm => {
                let module = runtime_spec
                    .module
                    .as_deref()
                    .ok_or_else(|| format!("{id} names no Wasm module"))?;
                let pinned = validated
                    .manifest
                    .files
                    .iter()
                    .find(|file| file.path == module)
                    .map(|file| file.sha256.clone())
                    .ok_or_else(|| format!("{id}'s module {module} is not a listed file"))?;
                runtime::portable_launch(
                    &package_dir,
                    module,
                    &pinned,
                    runtime_spec.limits,
                    require,
                )
            }
        }
    }

    /// Create an empty data generation for `record` and record it in `file`
    /// (the caller writes the file). Returns the generation id.
    fn fresh_data(
        &self,
        file: &mut GrantsFile,
        record: &InstalledRecord,
        schema_version: u32,
    ) -> Result<String, String> {
        let generation = self.0.data.create_empty(&record.id)?;
        file.data.insert(
            record.id.as_str().to_owned(),
            DataRecord {
                current: DataGeneration {
                    id: generation.clone(),
                    schema_version,
                    version: record.version.clone(),
                    package_hash: record.package_hash.clone(),
                    created_at_ms: now_ms(),
                },
                kept: Vec::new(),
            },
        );
        Ok(generation)
    }

    /// The data folder an instance of `record` binds: the profile's current
    /// generation, which must have the schema the package reads. One is
    /// created and saved on first use.
    fn bind_data(
        &self,
        file: &mut GrantsFile,
        record: &InstalledRecord,
        validated: &ValidatedManifest,
    ) -> Result<(Option<PathBuf>, Option<u32>), String> {
        let Some(schema) = validated
            .manifest
            .data
            .as_ref()
            .map(|data| data.schema_version)
        else {
            return Ok((None, None));
        };
        let key = record.id.as_str();
        let _permit = self.0.writes.try_admit().map_err(|error| {
            format!("Extensions are temporarily unavailable ({error:?}); retry")
        })?;
        if let Some(existing) = file.data.get(key) {
            if existing.current.schema_version != schema {
                return Err(format!(
                    "this profile's data for {key} is schema {}, but {} reads schema {schema}; switch versions with an update, which migrates or restores the data",
                    existing.current.schema_version, record.version
                ));
            }
            let dir = self.0.data.ensure(&record.id, &existing.current.id)?;
            return Ok((Some(dir), Some(schema)));
        }
        let generation = self.fresh_data(file, record, schema)?;
        match self.0.grants.write(file.clone()) {
            Ok(written) => *file = written,
            Err(error) => {
                self.0.data.remove_generation(&record.id, &generation);
                return Err(error);
            }
        }
        Ok((
            Some(self.0.data.generation_dir(&record.id, &generation)),
            Some(schema),
        ))
    }

    /// Check a switch from the enabled version of `to`'s extension to `to`:
    /// quarantine, the new version's dependencies, every enabled dependent's
    /// range, and what happens to the data. No I/O beyond manifests.
    fn plan_switch(
        &self,
        index: &[InstalledRecord],
        file: &GrantsFile,
        to: &InstalledRecord,
        validated: &ValidatedManifest,
    ) -> Result<SwitchPlan, String> {
        let id = to.id.as_str();
        let from = file.extensions.get(id).cloned().ok_or_else(|| {
            format!(
                "no version of {id} is enabled in this profile; enable {} instead",
                to.version
            )
        })?;
        if from.package_hash == to.package_hash {
            return Err(format!(
                "{id} {} is already the enabled version",
                to.version
            ));
        }
        if let Some(reason) = self.0.runtime.quarantine_reason(&to.package_hash) {
            return Err(format!(
                "{id} {} is quarantined in this profile ({reason}); release it first",
                to.version
            ));
        }
        check_dependencies(validated, file, index)?;
        for (other, enabled) in &file.extensions {
            if other == id {
                continue;
            }
            // An unreadable dependent cannot run either; it does not block.
            let Ok(dependent) = self.0.packages.read_manifest(&enabled.package_hash) else {
                continue;
            };
            for dependency in dependent
                .manifest
                .dependencies
                .iter()
                .filter(|dependency| dependency.id == to.id)
            {
                if !manifest::version_in_range(
                    &to.version,
                    &dependency.min_version,
                    dependency.max_version.as_deref(),
                ) {
                    return Err(format!(
                        "{other} {} requires {id} {}; {} would break it",
                        enabled.version,
                        manifest::range_label(
                            &dependency.min_version,
                            dependency.max_version.as_deref()
                        ),
                        to.version
                    ));
                }
            }
        }
        let data = data_step(file.data.get(id), validated)?;
        let new_capabilities = validated
            .capabilities
            .iter()
            .map(|capability| capability.id.to_string())
            .filter(|capability| !from.grants.contains(capability))
            .collect();
        let dropped_grants = from
            .grants
            .iter()
            .filter(|granted| {
                !validated
                    .capabilities
                    .iter()
                    .any(|capability| capability.id.as_str() == granted.as_str())
            })
            .cloned()
            .collect();
        Ok(SwitchPlan {
            kind: switch_kind(&from.version, &to.version),
            from,
            data,
            new_capabilities,
            dropped_grants,
        })
    }

    /// CORE-19: switch this profile from the enabled version of an extension
    /// to another installed one (update or rollback). The switch runs to
    /// completion in its own task even when the caller goes away.
    pub(crate) async fn update(
        &self,
        request: UpdateExtensionRequest,
    ) -> Result<ExtensionUpdateOutcome, String> {
        self.available()?;
        check_hash(&request.from_package_hash)?;
        check_hash(&request.to_package_hash)?;
        if request.from_package_hash == request.to_package_hash {
            return Err("the switch names the same package twice".into());
        }
        let drain = match request.active {
            ExtensionActiveRunPolicy::Drain => Some(lifecycle_timeout(
                "drainTimeoutMs",
                request.drain_timeout_ms,
            )?),
            ExtensionActiveRunPolicy::Stop => None,
        };
        let migration_timeout =
            lifecycle_timeout("migrationTimeoutMs", request.migration_timeout_ms)?;
        let manager = self.clone();
        tokio::spawn(async move { manager.run_update(request, drain, migration_timeout).await })
            .await
            .map_err(|error| format!("the switch stopped: {error}"))?
    }

    async fn run_update(
        &self,
        request: UpdateExtensionRequest,
        drain: Option<Duration>,
        migration_timeout: Duration,
    ) -> Result<ExtensionUpdateOutcome, String> {
        let _lifecycle = self.0.lifecycle.lock().await;
        let _claim = self.claim(&request.extension_id)?;
        let planner = self.clone();
        let planned = request.clone();
        let prepared = blocking(move || planner.prepare_update(&planned)).await?;
        // The enabled version's instances are pinned to its hash: drain or
        // stop them before anything changes. A drain that times out leaves
        // them running and changes nothing.
        let stopped = self.0.runtime.quiesce(&request.extension_id, drain).await?;
        let restartable = |error: String| -> String {
            if stopped.is_empty() {
                error
            } else {
                format!(
                    "{error}. {} instance(s) of {} were stopped for the switch; start them again",
                    stopped.len(),
                    prepared.plan.from.version
                )
            }
        };
        let migrated = match &prepared.plan.data {
            DataStep::Migrate { from, to_schema } => Some(
                self.migrate(&prepared, from, *to_schema, migration_timeout)
                    .await
                    .map_err(restartable)?,
            ),
            _ => None,
        };
        let committer = self.clone();
        let committed = prepared.clone();
        let (package, previous) = blocking(move || committer.commit_update(&committed, migrated))
            .await
            .map_err(restartable)?;
        let expanded_grants = prepared
            .plan
            .new_capabilities
            .iter()
            .filter(|capability| prepared.grants.contains(capability))
            .filter_map(|capability| NamespacedId::parse(capability).ok())
            .collect();
        Ok(ExtensionUpdateOutcome {
            kind: prepared.plan.kind,
            package,
            from_version: previous.version,
            from_package_hash: previous.package_hash,
            stopped_instances: stopped,
            data: prepared.plan.data.view(),
            expanded_grants,
        })
    }

    /// Everything a switch checks before it stops anything (blocking I/O).
    fn prepare_update(&self, request: &UpdateExtensionRequest) -> Result<PreparedUpdate, String> {
        let _gate = self.gate();
        let index = self.0.packages.read_index()?;
        let to = installed(&index, &request.to_package_hash)?.clone();
        if to.id != request.extension_id {
            return Err(format!(
                "package {} is {}, not {}",
                request.to_package_hash, to.id.0, request.extension_id.0
            ));
        }
        let id = to.id.as_str();
        let file = self.0.grants.read()?;
        match file.extensions.get(id) {
            Some(enabled) if enabled.package_hash == request.from_package_hash => {}
            Some(enabled) => {
                return Err(format!(
                    "{id} {} ({}) is the enabled version, not {}; nothing was switched",
                    enabled.version, enabled.package_hash, request.from_package_hash
                ));
            }
            None => {
                return Err(format!(
                    "no version of {id} is enabled in this profile; enable {} instead",
                    to.version
                ));
            }
        }
        let validated = self.0.packages.read_manifest(&to.package_hash)?;
        let grants = validate_grants(&validated, &request.grants, id)?;
        let integrity = self.0.packages.integrity(&validated, true);
        if integrity.status == ExtensionIntegrityStatus::Failed {
            return Err(format!(
                "the installed package failed verification ({}); reinstall it",
                integrity.detail.as_deref().unwrap_or("unknown change")
            ));
        }
        let plan = self.plan_switch(&index, &file, &to, &validated)?;
        if plan.data.runs_code() && self.0.runtime.safe_mode()?.is_some() {
            return Err(
                "safe mode is on, and this switch migrates data with the new version's code; turn safe mode off first"
                    .into(),
            );
        }
        Ok(PreparedUpdate {
            to,
            validated,
            grants,
            plan,
        })
    }

    /// Copy the current data and let the new version migrate the copy with
    /// `luna.migrate`. The copy is discarded unless the migration answers
    /// `ok`; the current data is never touched. Returns the copy's id.
    async fn migrate(
        &self,
        prepared: &PreparedUpdate,
        from: &DataGeneration,
        to_schema: u32,
        timeout: Duration,
    ) -> Result<String, String> {
        let id = prepared.to.id.clone();
        let copier = self.clone();
        let (copy_id, source) = (id.clone(), from.id.clone());
        let generation = blocking(move || {
            let _permit = copier.0.writes.try_admit().map_err(|error| {
                format!("Extensions are temporarily unavailable ({error:?}); nothing was switched")
            })?;
            copier.0.data.copy(&copy_id, &source)
        })
        .await?;
        let root = self.0.data.generation_dir(&id, &generation);
        let result = self
            .run_migration(prepared, root, from, to_schema, timeout)
            .await;
        if let Err(error) = result {
            let remover = self.clone();
            let (remove_id, copy) = (id.clone(), generation.clone());
            let _ = tokio::task::spawn_blocking(move || {
                remover.0.data.remove_generation(&remove_id, &copy)
            })
            .await;
            return Err(error);
        }
        Ok(generation)
    }

    async fn run_migration(
        &self,
        prepared: &PreparedUpdate,
        root: PathBuf,
        from: &DataGeneration,
        to_schema: u32,
        timeout: Duration,
    ) -> Result<(), String> {
        let to = &prepared.to;
        let keep = format!(
            "{} {} is still the enabled version and its data is unchanged",
            to.id.0, prepared.plan.from.version
        );
        let (spec, limits) = self.launch_spec(
            to,
            &prepared.validated,
            &prepared.grants,
            &[EXTENSION_MIGRATE_OPERATION.to_owned()],
        )?;
        let plan = runtime::LaunchPlan {
            extension_id: to.id.clone(),
            version: to.version.clone(),
            package_hash: to.package_hash.clone(),
            runtime: prepared.validated.manifest.runtime.kind,
            grants: prepared.grants.clone(),
            target: runtime::BoundTarget::None,
            spec,
            limits,
            data_root: Some(root),
            data_schema_version: Some(to_schema),
            purpose: ExtensionInstancePurpose::Migration,
        };
        let relaunch: runtime::Relaunch = Arc::new(|| -> Result<runtime::LaunchPlan, String> {
            Err("a migration worker is never restarted".to_string())
        });
        let started = self
            .0
            .runtime
            .start(plan, relaunch)
            .await
            .map_err(|error| {
                format!(
                    "{} {} could not migrate the data: {error}; {keep}",
                    to.id.0, to.version
                )
            })?;
        let outcome = self
            .0
            .runtime
            .call(CallExtensionInstanceRequest {
                instance_id: started.instance_id.clone(),
                method: EXTENSION_MIGRATE_OPERATION.to_owned(),
                params: serde_json::json!({
                    "fromSchemaVersion": from.schema_version,
                    "toSchemaVersion": to_schema,
                    "fromVersion": from.version,
                    "toVersion": to.version,
                }),
                timeout_ms: Some(timeout.as_millis() as u64),
                request_id: None,
            })
            .await;
        let _ = self.0.runtime.stop(&started.instance_id).await;
        match outcome {
            Ok(outcome) if outcome.status == ExtensionCallStatus::Ok => Ok(()),
            Ok(outcome) => Err(format!(
                "{} {}'s migration from data schema {} to {to_schema} failed ({}); {keep}",
                to.id.0,
                to.version,
                from.schema_version,
                outcome
                    .error
                    .map(|error| format!("{}: {}", error.code, error.message))
                    .unwrap_or_else(|| "no answer".into())
            )),
            Err(error) => Err(format!(
                "{} {}'s migration could not run ({error}); {keep}",
                to.id.0, to.version
            )),
        }
    }

    /// The commit point of a switch: one grants-file write that names the
    /// new package, its grants and its data generation. Discards the
    /// migrated copy when it fails. Returns the new view and the previous
    /// grant record.
    fn commit_update(
        &self,
        prepared: &PreparedUpdate,
        migrated: Option<String>,
    ) -> Result<(ExtensionPackageView, GrantRecord), String> {
        let result = self.commit_switch(prepared, migrated.as_deref());
        if result.is_err()
            && let Some(generation) = &migrated
        {
            self.0.data.remove_generation(&prepared.to.id, generation);
        }
        result
    }

    fn commit_switch(
        &self,
        prepared: &PreparedUpdate,
        migrated: Option<&str>,
    ) -> Result<(ExtensionPackageView, GrantRecord), String> {
        let _permit = self.0.writes.try_admit().map_err(|error| {
            format!("Extensions are temporarily unavailable ({error:?}); nothing was switched")
        })?;
        let _gate = self.gate();
        let to = &prepared.to;
        let key = to.id.as_str();
        let index = self.0.packages.read_index()?;
        installed(&index, &to.package_hash)?;
        let mut file = self.0.grants.read()?;
        let previous = match file.extensions.get(key) {
            Some(enabled) if enabled.package_hash == prepared.plan.from.package_hash => {
                enabled.clone()
            }
            _ => {
                return Err(
                    "the enabled version changed during the switch; nothing was switched".into(),
                );
            }
        };
        let now = now_ms();
        let generation = |id: String, schema_version: u32| DataGeneration {
            id,
            schema_version,
            version: to.version.clone(),
            package_hash: to.package_hash.clone(),
            created_at_ms: now,
        };
        let mut aged = Vec::new();
        let mut fresh = None;
        match &prepared.plan.data {
            DataStep::None | DataStep::Keep(_) => {}
            DataStep::Fresh(schema) => {
                if file.data.contains_key(key) {
                    return Err(
                        "this profile's data changed during the switch; nothing was switched"
                            .into(),
                    );
                }
                fresh = Some(self.fresh_data(&mut file, to, *schema)?);
            }
            DataStep::Migrate { from, to_schema } => {
                let copy = migrated.ok_or("the migrated data copy is missing")?;
                let record = file
                    .data
                    .get_mut(key)
                    .filter(|record| record.current.id == from.id)
                    .ok_or("this profile's data changed during the switch; nothing was switched")?;
                aged = data::promote(record, generation(copy.to_owned(), *to_schema));
            }
            DataStep::Restore(kept) => {
                let record = file
                    .data
                    .get_mut(key)
                    .filter(|record| record.kept.iter().any(|copy| copy.id == kept.id))
                    .ok_or("the kept data copy changed during the switch; nothing was switched")?;
                aged = data::promote(record, kept.clone());
            }
        }
        file.extensions.insert(
            key.to_owned(),
            GrantRecord {
                package_hash: to.package_hash.clone(),
                version: to.version.clone(),
                grants: prepared.grants.clone(),
                enabled_at_ms: now,
            },
        );
        // The commit point.
        let file = match self.0.grants.write(file) {
            Ok(file) => file,
            Err(error) => {
                if let Some(generation) = fresh {
                    self.0.data.remove_generation(&to.id, &generation);
                }
                return Err(error);
            }
        };
        for old in &aged {
            self.0.data.remove_generation(&to.id, &old.id);
        }
        if let Some(enabled) = file.extensions.get(key) {
            // Nothing of the extension keeps a principal of the old version.
            self.0.runtime.revoke(
                &to.id,
                Some((enabled.package_hash.as_str(), enabled.grants.as_slice())),
            );
        }
        // CORE-08: provider contributions follow the enabled version.
        self.providers_changed();
        // CORE-12: so do UI contributions.
        self.ui_changed();
        let integrity = self.0.packages.integrity(&prepared.validated, false);
        let mut package = view(
            to,
            Some(&prepared.validated),
            file.extensions.get(key),
            integrity,
        );
        self.decorate(&mut package, &index, Some(&file), to, &prepared.validated);
        Ok((package, previous))
    }

    /// CORE-19: remove one installed package from this device. Enabled here,
    /// it is disabled first (its dependents must allow it); its instances
    /// are stopped and revoked; its quarantine record is released. This
    /// profile's data for the extension is kept.
    pub(crate) async fn remove(
        &self,
        request: RemoveExtensionRequest,
    ) -> Result<ExtensionRemoveOutcome, String> {
        self.available()?;
        check_hash(&request.package_hash)?;
        let manager = self.clone();
        tokio::spawn(async move {
            let _lifecycle = manager.0.lifecycle.lock().await;
            let _claim = manager.claim(&request.extension_id)?;
            let checker = manager.clone();
            let checked = request.clone();
            blocking(move || checker.check_remove(&checked)).await?;
            let stopped = manager
                .0
                .runtime
                .retire_package(&request.package_hash)
                .await;
            let committer = manager.clone();
            let mut outcome = blocking(move || committer.commit_remove(&request)).await?;
            outcome.stopped_instances = stopped;
            Ok(outcome)
        })
        .await
        .map_err(|error| format!("the removal stopped: {error}"))?
    }

    fn check_remove(&self, request: &RemoveExtensionRequest) -> Result<(), String> {
        let _gate = self.gate();
        let index = self.0.packages.read_index()?;
        let record = installed(&index, &request.package_hash)?;
        if record.id != request.extension_id {
            return Err(format!(
                "package {} is {}, not {}",
                request.package_hash, record.id.0, request.extension_id.0
            ));
        }
        let file = self.0.grants.read()?;
        self.check_no_dependents(&file, record)
    }

    /// When `record` is the enabled package here, no other enabled package
    /// may depend on its extension.
    fn check_no_dependents(
        &self,
        file: &GrantsFile,
        record: &InstalledRecord,
    ) -> Result<(), String> {
        let id = record.id.as_str();
        let enabled_here = file
            .extensions
            .get(id)
            .is_some_and(|grant| grant.package_hash == record.package_hash);
        if !enabled_here {
            return Ok(());
        }
        for (other, enabled) in &file.extensions {
            if other == id {
                continue;
            }
            let depends = self
                .0
                .packages
                .read_manifest(&enabled.package_hash)
                .is_ok_and(|other| {
                    other
                        .manifest
                        .dependencies
                        .iter()
                        .any(|dependency| dependency.id == record.id)
                });
            if depends {
                return Err(format!("{other} depends on {id}; disable it first"));
            }
        }
        Ok(())
    }

    /// Grants first, then the index (the package is gone once it is not
    /// listed), then the quarantine record, then the tree. A crash between
    /// steps is finished by recovery: an unlisted tree is removed and the
    /// quarantine of a package that is not installed is released.
    fn commit_remove(
        &self,
        request: &RemoveExtensionRequest,
    ) -> Result<ExtensionRemoveOutcome, String> {
        let _permit = self.0.writes.try_admit().map_err(|error| {
            format!("Extensions are temporarily unavailable ({error:?}); retry")
        })?;
        let _gate = self.gate();
        let index = self.0.packages.read_index()?;
        let record = installed(&index, &request.package_hash)?.clone();
        let id = record.id.as_str();
        let mut file = self.0.grants.read()?;
        self.check_no_dependents(&file, &record)?;
        let was_enabled = file
            .extensions
            .get(id)
            .is_some_and(|grant| grant.package_hash == record.package_hash);
        if was_enabled {
            file.extensions.remove(id);
            file = self.0.grants.write(file)?;
            self.0.runtime.revoke(&record.id, None);
            self.providers_changed();
            self.ui_changed();
        }
        self.0.packages.remove(&record.package_hash)?;
        let quarantine_released = match self.0.runtime.forget_package(&record.package_hash) {
            Ok(released) => released,
            Err(error) => {
                tracing::warn!(%error, "the removed package's quarantine is released on the next open");
                false
            }
        };
        Ok(ExtensionRemoveOutcome {
            extension_id: record.id.clone(),
            version: record.version.clone(),
            package_hash: record.package_hash.clone(),
            was_enabled,
            stopped_instances: Vec::new(),
            quarantine_released,
            data_kept: file.data.contains_key(id) || self.0.data.exists(&record.id),
        })
    }

    /// CORE-19: erase this profile's data for one extension, every kept copy
    /// included. Refused while an instance of it runs. Package files and
    /// grants are untouched.
    pub(crate) async fn erase_data(
        &self,
        request: EraseExtensionDataRequest,
    ) -> Result<ExtensionEraseOutcome, String> {
        self.available()?;
        let manager = self.clone();
        tokio::spawn(async move {
            let _lifecycle = manager.0.lifecycle.lock().await;
            let _claim = manager.claim(&request.extension_id)?;
            if manager.0.runtime.live_count(&request.extension_id) > 0 {
                return Err(format!(
                    "{} is running; stop its instances before erasing its data",
                    request.extension_id.0
                ));
            }
            let eraser = manager.clone();
            blocking(move || eraser.commit_erase(&request.extension_id)).await
        })
        .await
        .map_err(|error| format!("the erase stopped: {error}"))?
    }

    /// The folders go first (renamed away, then deleted), then the record:
    /// a crash in between leaves a record whose folder is gone, which the
    /// next start replaces with an empty one.
    fn commit_erase(&self, id: &ExtensionId) -> Result<ExtensionEraseOutcome, String> {
        let _permit = self.0.writes.try_admit().map_err(|error| {
            format!("Extensions are temporarily unavailable ({error:?}); retry")
        })?;
        let _gate = self.gate();
        let mut file = self.0.grants.read()?;
        let record = file.data.get(id.as_str()).cloned();
        if record.is_none() && !self.0.data.exists(id) {
            return Err(format!("this profile holds no data for {}", id.0));
        }
        let (erased_copies, erased_bytes) = record.as_ref().map_or((0, 0), |record| {
            std::iter::once(&record.current)
                .chain(record.kept.iter())
                .fold((0_u32, 0_u64), |(copies, bytes), generation| {
                    (copies + 1, bytes + self.0.data.size(id, &generation.id))
                })
        });
        self.0.data.erase(id)?;
        if record.is_some() {
            file.data.remove(id.as_str());
            self.0.grants.write(file)?;
        }
        Ok(ExtensionEraseOutcome {
            extension_id: id.clone(),
            erased_copies,
            erased_bytes,
        })
    }

    /// CORE-19: turn this profile's safe mode on (every worker stops and no
    /// extension starts) or off. Grants, packages and data are untouched.
    pub(crate) async fn set_safe_mode(
        &self,
        request: SetExtensionSafeModeRequest,
    ) -> Result<ExtensionsSnapshot, String> {
        self.available()?;
        let manager = self.clone();
        tokio::spawn(async move {
            let _lifecycle = manager.0.lifecycle.lock().await;
            manager.0.runtime.set_safe_mode(request.enabled).await?;
            let reader = manager.clone();
            blocking(move || {
                // CORE-08: the provider registry re-reads what may run.
                reader.providers_changed();
                reader.ui_changed();
                reader.snapshot()
            })
            .await
        })
        .await
        .map_err(|error| format!("the safe-mode change stopped: {error}"))?
    }
}

/// Run blocking package, grant or data I/O off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| format!("the extension operation stopped: {error}"))?
}

fn lifecycle_timeout(label: &str, value: Option<u64>) -> Result<Duration, String> {
    match value {
        None => Ok(DEFAULT_LIFECYCLE_TIMEOUT),
        Some(0) => Err(format!("{label} must be at least 1")),
        Some(ms) if Duration::from_millis(ms) > MAX_LIFECYCLE_TIMEOUT => Err(format!(
            "{label} is at most {}",
            MAX_LIFECYCLE_TIMEOUT.as_millis()
        )),
        Some(ms) => Ok(Duration::from_millis(ms)),
    }
}

/// `requested` as the exact grant set for `validated`: only capabilities it
/// requests, none twice, every required one. Sorted.
fn validate_grants(
    validated: &ValidatedManifest,
    requested: &[NamespacedId],
    id: &str,
) -> Result<Vec<String>, String> {
    let mut granted: Vec<String> = Vec::with_capacity(requested.len());
    for grant in requested {
        let grant = grant.as_str();
        if !validated
            .capabilities
            .iter()
            .any(|capability| capability.id.as_str() == grant)
        {
            return Err(format!("{grant} was not requested by this package"));
        }
        if granted.iter().any(|existing| existing == grant) {
            return Err(format!("{grant} is granted more than once"));
        }
        granted.push(grant.to_owned());
    }
    if let Some(missing) = validated.capabilities.iter().find(|capability| {
        capability.required && !granted.iter().any(|g| g == capability.id.as_str())
    }) {
        return Err(format!(
            "{} is required; grant it to enable {id}",
            missing.id
        ));
    }
    granted.sort();
    Ok(granted)
}

/// Every dependency of `validated` is enabled in this profile, installed,
/// and in range.
fn check_dependencies(
    validated: &ValidatedManifest,
    file: &GrantsFile,
    index: &[InstalledRecord],
) -> Result<(), String> {
    for dependency in &validated.manifest.dependencies {
        let range =
            manifest::range_label(&dependency.min_version, dependency.max_version.as_deref());
        let Some(enabled) = file.extensions.get(dependency.id.as_str()) else {
            return Err(format!(
                "requires {} {range}; enable it first",
                dependency.id.0
            ));
        };
        if !index
            .iter()
            .any(|installed| installed.package_hash == enabled.package_hash)
        {
            return Err(format!(
                "requires {}; its enabled package is no longer installed",
                dependency.id.0
            ));
        }
        if !manifest::version_in_range(
            &enabled.version,
            &dependency.min_version,
            dependency.max_version.as_deref(),
        ) {
            return Err(format!(
                "requires {} {range}; the enabled version is {}",
                dependency.id.0, enabled.version
            ));
        }
    }
    Ok(())
}

/// What the switch does with this profile's data. A newer schema needs the
/// new version's migration (or an exact kept copy); an older schema needs an
/// exact kept copy, so older code never reads a schema it does not know.
fn data_step(
    record: Option<&DataRecord>,
    validated: &ValidatedManifest,
) -> Result<DataStep, String> {
    let manifest = &validated.manifest;
    let Some(schema) = manifest.data.as_ref() else {
        return Ok(DataStep::None);
    };
    let target = schema.schema_version;
    let Some(record) = record else {
        return Ok(DataStep::Fresh(target));
    };
    let current = record.current.schema_version;
    if current == target {
        return Ok(DataStep::Keep(target));
    }
    let kept = record
        .kept
        .iter()
        .find(|kept| kept.schema_version == target)
        .cloned();
    if current < target {
        let migrates = schema.migrates_from.is_some_and(|from| from <= current)
            && manifest.runtime.kind != ExtensionRuntimeKind::Data;
        if migrates {
            return Ok(DataStep::Migrate {
                from: record.current.clone(),
                to_schema: target,
            });
        }
        if let Some(kept) = kept {
            return Ok(DataStep::Restore(kept));
        }
        let reason = match schema.migrates_from {
            Some(from) => format!("it migrates from schema {from}"),
            None => "it declares no migration".to_owned(),
        };
        return Err(format!(
            "{} {} reads data schema {target} and cannot migrate this profile's schema {current} ({reason})",
            manifest.id.0, manifest.version
        ));
    }
    kept.map(DataStep::Restore).ok_or_else(|| {
        format!(
            "this profile's data for {} is schema {current}, written by {}; {} reads schema {target} and no schema-{target} copy is kept, so it would read a newer schema it does not know. Stay on {}, or erase the data first",
            manifest.id.0, record.current.version, manifest.version, record.current.version
        )
    })
}

fn switch_kind(from: &str, to: &str) -> ExtensionSwitchKind {
    if manifest::parse_version(to) > manifest::parse_version(from) {
        ExtensionSwitchKind::Update
    } else {
        ExtensionSwitchKind::Rollback
    }
}

fn check_hash(package_hash: &str) -> Result<(), String> {
    if manifest::is_sha256_hex(package_hash) {
        Ok(())
    } else {
        Err("a package hash is 64 lowercase hex characters".into())
    }
}

fn installed<'a>(
    index: &'a [InstalledRecord],
    package_hash: &str,
) -> Result<&'a InstalledRecord, String> {
    index
        .iter()
        .find(|record| record.package_hash == package_hash)
        .ok_or_else(|| "that package is not installed on this device".to_string())
}

fn failed(detail: String) -> ExtensionIntegrity {
    ExtensionIntegrity {
        status: ExtensionIntegrityStatus::Failed,
        detail: Some(detail),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The public view. A grant counts only for the exact package hash it names.
fn view(
    record: &InstalledRecord,
    validated: Option<&ValidatedManifest>,
    grant: Option<&GrantRecord>,
    integrity: ExtensionIntegrity,
) -> ExtensionPackageView {
    let grant = grant.filter(|grant| grant.package_hash == record.package_hash);
    let manifest = validated.map(|validated| &validated.manifest);
    let capabilities = validated
        .map(|validated| {
            validated
                .capabilities
                .iter()
                .map(|capability| ExtensionCapabilityView {
                    id: capability.id.clone(),
                    label: extension_capability(capability.id.as_str())
                        .map_or_else(|| capability.id.to_string(), |spec| spec.label.to_owned()),
                    reason: capability.reason.clone(),
                    required: capability.required,
                    granted: grant.is_some_and(|grant| {
                        grant
                            .grants
                            .iter()
                            .any(|granted| granted == capability.id.as_str())
                    }),
                })
                .collect()
        })
        .unwrap_or_default();
    ExtensionPackageView {
        id: record.id.clone(),
        version: record.version.clone(),
        package_hash: record.package_hash.clone(),
        display_name: record.display_name.clone(),
        publisher: record.publisher.clone(),
        description: manifest.and_then(|manifest| manifest.description.clone()),
        license: manifest.and_then(|manifest| manifest.license.clone()),
        runtime: record.runtime,
        trust: record.runtime.trust(),
        origin: record.origin.clone(),
        installed_at_ms: record.installed_at_ms,
        size_bytes: record.size_bytes,
        file_count: record.file_count,
        state: if grant.is_some() {
            ExtensionState::Enabled
        } else {
            ExtensionState::Disabled
        },
        enabled_at_ms: grant.map(|grant| grant.enabled_at_ms),
        integrity,
        capabilities,
        contributions: manifest
            .map(|manifest| manifest.contributions.clone())
            .unwrap_or_default(),
        dependencies: manifest
            .map(|manifest| manifest.dependencies.clone())
            .unwrap_or_default(),
        settings_schema_version: manifest
            .and_then(|manifest| manifest.settings.as_ref())
            .map(|settings| settings.schema_version),
        data_schema_version: manifest
            .and_then(|manifest| manifest.data.as_ref())
            .map(|data| data.schema_version),
        data_migrates_from: manifest
            .and_then(|manifest| manifest.data.as_ref())
            .and_then(|data| data.migrates_from),
        quarantine_reason: None,
        switch: None,
    }
}
