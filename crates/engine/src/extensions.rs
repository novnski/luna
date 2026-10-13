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

mod grants;
mod manifest;
mod runtime;
mod store;

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use luna_proto::{
    CallExtensionInstanceRequest, EXTENSION_API, EXTENSION_MANIFEST_FILE, EXTENSION_WASM_PROFILE,
    EXTENSION_WORKER_PROTOCOL, ExtensionCallOutcome, ExtensionCapabilityView, ExtensionHostInfo,
    ExtensionInstanceView, ExtensionInstancesSnapshot, ExtensionIntegrity,
    ExtensionIntegrityStatus, ExtensionOrigin, ExtensionOriginKind, ExtensionPackageView,
    ExtensionRuntimeKind, ExtensionState, ExtensionsSnapshot, InstallExtensionRequest,
    NATIVE_CODE_CAPABILITY, ReleaseExtensionQuarantineRequest, SetExtensionEnabledRequest,
    StartExtensionInstanceRequest, extension_capability,
};

use crate::WorkspaceHost;
use crate::device_write::DeviceWriteHandle;
use grants::{GrantRecord, ProfileGrants};
use manifest::ValidatedManifest;
use store::{InstalledRecord, PackageStore};

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
        let manager = Self(Arc::new(Inner {
            unavailable,
            packages: PackageStore::new(&device),
            grants: ProfileGrants::new(&store, profile_id.clone()),
            runtime: runtime::ExtensionRuntime::open(
                &store,
                profile_id,
                writes.clone(),
                Some(workspace),
            ),
            writes,
            gate: Mutex::new(()),
            platform: manifest::host_platform(),
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
                        view(record, Some(&validated), grant, integrity)
                    }
                    Err(detail) => view(record, None, grant, failed(detail)),
                }
            })
            .collect();
        Ok(ExtensionsSnapshot {
            host: self.host(),
            packages,
            grants_error,
        })
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
                view(record, Some(&validated), grant, integrity)
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
            let validated = self.0.packages.read_manifest(&record.package_hash).ok();
            let integrity = validated.as_ref().map_or_else(
                || failed("the installed manifest could not be read".into()),
                |validated| self.0.packages.integrity(validated, false),
            );
            return Ok(view(record, validated.as_ref(), None, integrity));
        }

        let validated = self.0.packages.read_manifest(&record.package_hash)?;
        let mut granted: Vec<String> = Vec::with_capacity(request.grants.len());
        for grant in &request.grants {
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
        if let Some(existing) = file.extensions.get(id) {
            if existing.package_hash != record.package_hash {
                return Err(format!(
                    "{id} {} is enabled; disable it before enabling {}",
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
        file.extensions.insert(
            id.to_owned(),
            GrantRecord {
                package_hash: record.package_hash.clone(),
                version: record.version.clone(),
                grants: granted,
                enabled_at_ms: now_ms(),
            },
        );
        let file = self.0.grants.write(file)?;
        if let Some(enabled) = file.extensions.get(id) {
            // An instance bound to the previous grant set stops.
            self.0.runtime.revoke(
                &record.id,
                Some((enabled.package_hash.as_str(), enabled.grants.as_slice())),
            );
        }
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
    /// every file re-hashes to the manifest, the package is not quarantined
    /// and the target task belongs to this device. Blocking I/O.
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
        let file = self.0.grants.read()?;
        let grant = file
            .extensions
            .get(id)
            .filter(|grant| grant.package_hash == record.package_hash)
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
        let package_dir = self.0.packages.package_dir(&record.package_hash);
        let runtime_spec = &validated.manifest.runtime;
        let (spec, limits) = match runtime_spec.kind {
            ExtensionRuntimeKind::Data => {
                return Err(format!("{id} is a data package; it has nothing to run"));
            }
            ExtensionRuntimeKind::Native => {
                if !grant.grants.iter().any(|g| g == NATIVE_CODE_CAPABILITY) {
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
                runtime::native_launch(&package_dir, entry, runtime_spec.limits, &request.require)
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
                    &request.require,
                )?
            }
        };
        let target = match &request.task_id {
            None => runtime::BoundTarget::None,
            Some(task_id) => self.0.runtime.bind_task(task_id)?,
        };
        Ok(runtime::LaunchPlan {
            extension_id: record.id.clone(),
            version: record.version.clone(),
            package_hash: record.package_hash.clone(),
            runtime: runtime_spec.kind,
            grants: grant.grants.clone(),
            target,
            spec,
            limits,
        })
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
    }
}
