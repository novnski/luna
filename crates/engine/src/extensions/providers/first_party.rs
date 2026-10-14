//! CORE-11: LUNA's own provider packages.
//!
//! OpenCode is a first-party package, `luna.provider/opencode`: LUNA ships
//! it, but it is installed, enabled, listed and run through the same
//! package, worker and registry path a third-party provider uses, with no
//! compiled provider shortcut. LUNA bundles the package's worker executable
//! (`luna-opencode-worker`, the OpenCode 2 adapter in its own process) next
//! to its own executable; the manifest and the reviewed `luna-provider.json`
//! are compiled in. When the engine opens:
//!
//! 1. **Install.** The package is assembled from those bytes and installed
//!    like any local package (staged, verified, hash-pinned, read-only)
//!    unless that exact package hash is installed already. Only LUNA can
//!    install into the reserved `luna` namespace. A rebuilt worker with the
//!    same version replaces the old bytes; versions no longer used are
//!    removed. An unchanged bundle is recognized from its file metadata and
//!    not hashed again.
//! 2. **Enable by default, once.** The first time a profile sees the
//!    package it is enabled with exactly its declared grants (native code
//!    and `provider.register`), through `SetExtensionEnabled`. Afterwards
//!    LUNA never enables it again: a user who disabled it keeps it disabled
//!    across launches and updates. An enabled older first-party hash moves
//!    to the bundled one with the same grants (nothing runs yet at open).
//! 3. **One path.** While the package is installed, the compiled
//!    registration of the same provider (the legacy `opencode` slot) is
//!    removed from the registry, so the package is OpenCode's only path:
//!    disabling it removes OpenCode from the picker, enabling it restores
//!    it, and safe mode refuses its runs with the safe-mode reason. Chats
//!    keep their `opencode` binding, history and provider session.
//!
//! A build that bundles no worker (or `LUNA_OPENCODE_WORKER=off`) installs
//! nothing; then, without an installed package, the compiled adapter keeps
//! serving OpenCode. `ListExtensionProviders` reports which case holds.
//!
//! A first-party native worker inherits the engine's environment, like the
//! in-process adapter it replaces (provider keys, XDG and OpenCode config
//! paths, QA wrappers); its instance reports the environment allow-list as
//! not enforced. It is still not OS-sandboxed.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use luna_proto::contract::ExtensionId;
use luna_proto::{
    EXTENSION_MANIFEST_FILE, ExtensionOrigin, ExtensionOriginKind, HarnessId,
    NATIVE_CODE_CAPABILITY, RESERVED_EXTENSION_NAMESPACE, SetExtensionEnabledRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::super::store::{self, InstalledRecord};
use super::super::{ExtensionManager, manifest, now_ms};
use super::declaration::DECLARATION_FILE;
use super::{PROVIDER_REGISTER_CAPABILITY, legacy_spelling};
use crate::registry::HarnessRegistry;

/// Profile-scoped record of what LUNA did with its own packages.
const MARKER_FILE: &str = "first-party-extensions.json";
/// Shares the grants file's temp prefix, so an interrupted write is
/// removed by the grants recovery at the next open.
const MARKER_TEMP_PREFIX: &str = ".extension-grants-first-party-";
const MARKER_VERSION: u32 = 1;
const MAX_MARKER_BYTES: u64 = 64 * 1024;
/// Part of the bundle key: a change to the assembled manifest's shape
/// reassembles every bundle.
const ASSEMBLY_REVISION: u32 = 1;
/// The worker group's ceilings (`runtime.limits`, the host's maximum memory).
const WORKER_MEMORY_MIB: u32 = 4096;
const WORKER_CPU_SECONDS: u32 = 86_400;

/// One provider package LUNA ships.
pub(super) struct Spec {
    pub package_id: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    /// The worker executable's file name next to LUNA's own executable.
    pub worker: &'static str,
    /// An absolute path overrides where the worker is; `off` (or `0`)
    /// installs and updates nothing.
    pub worker_env: &'static str,
    /// The package's reviewed `luna-provider.json`.
    pub declaration: &'static str,
    pub native_code_reason: &'static str,
    pub register_reason: &'static str,
}

pub(super) const OPENCODE: Spec = Spec {
    package_id: "luna.provider/opencode",
    display_name: "OpenCode",
    description: "OpenCode 2 in LUNA's model picker: LUNA's own OpenCode adapter, run in a supervised worker per turn.",
    worker: "luna-opencode-worker",
    worker_env: "LUNA_OPENCODE_WORKER",
    declaration: include_str!("opencode/luna-provider.json"),
    native_code_reason: "The worker hosts LUNA's OpenCode 2 adapter and starts your installed `opencode` with your account's access.",
    register_reason: "Lists OpenCode and its models in the model picker.",
};

const SPECS: &[Spec] = &[OPENCODE];

/// Whether `id` is one of LUNA's own packages (only LUNA installs into the
/// reserved namespace).
pub(crate) fn is_first_party(id: &ExtensionId) -> bool {
    let namespace = id.0.namespace();
    namespace == RESERVED_EXTENSION_NAMESPACE
        || namespace.starts_with(&format!("{RESERVED_EXTENSION_NAMESPACE}."))
}

/// Where this LUNA's bundled worker stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum BundleState {
    /// Found and installed.
    Installed,
    /// This build ships no worker for the package.
    NotBundled,
    /// The override turned installation and updates off.
    Off,
    /// Found, but it could not be installed or enabled.
    Failed,
}

/// `ListExtensionProviders.firstParty[]`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FirstPartyStatus {
    pub package_id: String,
    /// The provider id the package registers under.
    pub provider_id: String,
    pub bundle: BundleState,
    /// The bundled worker this LUNA found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// A version of the package is installed on this device.
    pub installed: bool,
    /// The enabled package hash, else the bundled one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_hash: Option<String>,
    pub enabled: bool,
    /// This open enabled it, for the profile's first time.
    pub enabled_by_default: bool,
    /// The compiled registration of the provider was removed in its favour.
    pub replaces_builtin: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Marker {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    packages: BTreeMap<String, MarkerEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarkerEntry {
    /// When this profile enabled the package by default (once, ever).
    #[serde(default)]
    default_enabled_at_ms: Option<i64>,
    /// The bundle the package hash below was assembled from.
    #[serde(default)]
    bundle_key: Option<String>,
    #[serde(default)]
    package_hash: Option<String>,
}

enum Located {
    Found(PathBuf),
    Off,
    Missing(String),
}

/// The package assembled from the bundle, in a disposable folder.
struct Assembled {
    dir: tempfile::TempDir,
    package_hash: String,
}

fn locate(spec: &Spec) -> Located {
    if let Some(value) = std::env::var_os(spec.worker_env) {
        let text = value.to_string_lossy();
        if matches!(text.trim(), "" | "0" | "off") {
            return Located::Off;
        }
        let path = PathBuf::from(&value);
        return if path.is_absolute() && path.is_file() {
            Located::Found(path)
        } else {
            Located::Missing(format!(
                "{} names {}, which is not an absolute path to a file",
                spec.worker_env,
                path.display()
            ))
        };
    }
    let name = format!("{}{}", spec.worker, std::env::consts::EXE_SUFFIX);
    match std::env::current_exe() {
        Ok(exe) => {
            let path = exe.with_file_name(&name);
            if path.is_file() {
                Located::Found(path)
            } else {
                Located::Missing(format!(
                    "this build does not bundle {name} next to {}",
                    exe.display()
                ))
            }
        }
        Err(error) => Located::Missing(format!("LUNA's own executable is unknown: {error}")),
    }
}

/// Identifies a bundle by what it is built from, without reading it.
fn bundle_key(spec: &Spec, worker: &Path, version: &str) -> Result<String, String> {
    let metadata = fs::metadata(worker)
        .map_err(|error| format!("{} could not be read: {error}", worker.display()))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_millis());
    Ok(format!(
        "{ASSEMBLY_REVISION}|{}|{}|{modified}|{version}|{}",
        worker.display(),
        metadata.len(),
        manifest::sha256_hex(spec.declaration.as_bytes())
    ))
}

/// Write the package: the worker, the declaration and a manifest pinning
/// both. Blocking I/O; runs nothing.
fn assemble(spec: &Spec, worker: &Path, version: &str) -> Result<Assembled, String> {
    let dir = tempfile::Builder::new()
        .prefix("luna-first-party-")
        .tempdir()
        .map_err(|error| format!("a staging folder could not be created: {error}"))?;
    let entry = format!("bin/{}{}", spec.worker, std::env::consts::EXE_SUFFIX);
    fs::create_dir_all(dir.path().join("bin"))
        .map_err(|error| format!("the package folder could not be created: {error}"))?;
    let bin = dir.path().join(&entry);
    fs::copy(worker, &bin)
        .map_err(|error| format!("{} could not be copied: {error}", worker.display()))?;
    let (worker_size, worker_sha256) = store::hash_file(&bin)
        .map_err(|error| format!("{} could not be hashed: {error}", worker.display()))?;
    let declaration = spec.declaration.as_bytes();
    fs::write(dir.path().join(DECLARATION_FILE), declaration)
        .map_err(|error| format!("{DECLARATION_FILE} could not be written: {error}"))?;
    let mut entries = Map::new();
    entries.insert(manifest::host_platform(), Value::String(entry.clone()));
    let manifest_value = json!({
        "manifestVersion": 1,
        "id": spec.package_id,
        "version": version,
        "displayName": spec.display_name,
        "publisher": "LUNA",
        "description": spec.description,
        "license": "MIT",
        "api": {"major": 1, "minMinor": 1},
        "runtime": {
            "kind": "native",
            "protocol": {"major": 1, "minMinor": 0},
            "entries": entries,
            "limits": {"memoryMiB": WORKER_MEMORY_MIB, "cpuSeconds": WORKER_CPU_SECONDS},
        },
        "contributions": [
            {"id": spec.package_id, "kind": "provider", "title": spec.display_name},
        ],
        "capabilities": [
            {"id": NATIVE_CODE_CAPABILITY, "reason": spec.native_code_reason},
            {"id": PROVIDER_REGISTER_CAPABILITY, "reason": spec.register_reason},
        ],
        "files": [
            {"path": entry, "size": worker_size, "sha256": worker_sha256},
            {
                "path": DECLARATION_FILE,
                "size": declaration.len(),
                "sha256": manifest::sha256_hex(declaration),
            },
        ],
    });
    let bytes = serde_json::to_vec_pretty(&manifest_value)
        .map_err(|error| format!("the manifest could not be written: {error}"))?;
    fs::write(dir.path().join(EXTENSION_MANIFEST_FILE), &bytes)
        .map_err(|error| format!("{EXTENSION_MANIFEST_FILE} could not be written: {error}"))?;
    let validated = manifest::validate_installed(&bytes)?;
    Ok(Assembled {
        dir,
        package_hash: validated.package_hash,
    })
}

fn read_marker(root: &Path) -> Marker {
    match store::read_regular_file(&root.join(MARKER_FILE), MAX_MARKER_BYTES) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            tracing::warn!(%error, "LUNA's first-party package record is invalid; starting a new one");
            Marker::default()
        }),
        Err(_) => Marker::default(),
    }
}

fn write_marker(root: &Path, marker: &mut Marker) -> Result<(), String> {
    marker.version = MARKER_VERSION;
    let mut bytes = serde_json::to_vec_pretty(marker).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    store::publish_file(root, MARKER_TEMP_PREFIX, &root.join(MARKER_FILE), &bytes)
        .map_err(|error| format!("{MARKER_FILE} could not be saved: {error}"))
}

fn provider_id(spec: &Spec) -> String {
    HarnessId::parse(spec.package_id)
        .map(legacy_spelling)
        .map(|id| id.to_string())
        .unwrap_or_else(|_| spec.package_id.to_owned())
}

impl ExtensionManager {
    /// Install, update and default-enable LUNA's own packages. Blocking
    /// I/O at engine open, before the registry is attached; starts nothing.
    pub(super) fn ensure_first_party(&self) -> Vec<FirstPartyStatus> {
        SPECS.iter().map(|spec| self.ensure_one(spec)).collect()
    }

    fn ensure_one(&self, spec: &Spec) -> FirstPartyStatus {
        let mut status = FirstPartyStatus {
            package_id: spec.package_id.to_owned(),
            provider_id: provider_id(spec),
            bundle: BundleState::NotBundled,
            worker: None,
            installed: false,
            package_hash: None,
            enabled: false,
            enabled_by_default: false,
            replaces_builtin: false,
            detail: None,
        };
        if let Some(reason) = &self.0.unavailable {
            status.bundle = BundleState::Failed;
            status.detail = Some(reason.clone());
            return status;
        }
        let root = self.0.grants.root().to_path_buf();
        let mut marker = read_marker(&root);
        match locate(spec) {
            Located::Found(worker) => {
                status.worker = Some(worker.display().to_string());
                match self.install_bundled(spec, &worker, &mut marker) {
                    Ok(hash) => {
                        status.bundle = BundleState::Installed;
                        match self.default_enablement(spec, &hash, &mut marker) {
                            Ok(enabled_now) => status.enabled_by_default = enabled_now,
                            Err(error) => {
                                status.bundle = BundleState::Failed;
                                status.detail = Some(format!("it was not enabled: {error}"));
                            }
                        }
                        status.package_hash = Some(hash);
                        if let Err(error) = write_marker(&root, &mut marker) {
                            tracing::warn!(%error, "LUNA's first-party package record was not saved");
                        }
                    }
                    Err(error) => {
                        tracing::warn!(package = spec.package_id, %error, "LUNA's own provider package was not installed");
                        status.bundle = BundleState::Failed;
                        status.detail = Some(error);
                    }
                }
            }
            Located::Off => {
                status.bundle = BundleState::Off;
                status.detail = Some(format!(
                    "{} is off: the bundled package is neither installed nor updated",
                    spec.worker_env
                ));
            }
            Located::Missing(reason) => {
                status.bundle = BundleState::NotBundled;
                status.detail = Some(reason);
            }
        }
        // What this device has now, whatever this build bundles.
        let index = self.0.packages.read_index().unwrap_or_default();
        status.installed = index
            .iter()
            .any(|record| record.id.as_str() == spec.package_id);
        if let Ok(file) = self.0.grants.read()
            && let Some(grant) = file.extensions.get(spec.package_id)
            && index
                .iter()
                .any(|record| record.package_hash == grant.package_hash)
        {
            status.enabled = true;
            status.package_hash = Some(grant.package_hash.clone());
        }
        status
    }

    fn package_installed(&self, package_hash: &str) -> bool {
        self.0.packages.read_index().is_ok_and(|index| {
            index
                .iter()
                .any(|record| record.package_hash == package_hash)
        })
    }

    /// The installed package hash of the bundled bytes.
    fn install_bundled(
        &self,
        spec: &Spec,
        worker: &Path,
        marker: &mut Marker,
    ) -> Result<String, String> {
        let version = env!("CARGO_PKG_VERSION");
        let key = bundle_key(spec, worker, version)?;
        let entry = marker
            .packages
            .entry(spec.package_id.to_owned())
            .or_default();
        if entry.bundle_key.as_deref() == Some(key.as_str())
            && let Some(hash) = entry.package_hash.clone()
            && self.package_installed(&hash)
        {
            return Ok(hash);
        }
        let assembled = assemble(spec, worker, version)?;
        self.install_assembled(&assembled, worker)?;
        entry.bundle_key = Some(key);
        entry.package_hash = Some(assembled.package_hash.clone());
        Ok(assembled.package_hash)
    }

    /// `InstallExtension` for LUNA's own bytes: the same staging and
    /// verification, the reserved namespace allowed, and a same-version
    /// package with other bytes (a rebuilt worker) replaced.
    fn install_assembled(&self, assembled: &Assembled, worker: &Path) -> Result<(), String> {
        let _permit = self.0.writes.try_admit().map_err(|error| {
            format!("the device writer is unavailable ({error:?}); it is retried at the next open")
        })?;
        let _gate = self.gate();
        let index = self.0.packages.read_index()?;
        if index
            .iter()
            .any(|record| record.package_hash == assembled.package_hash)
        {
            return Ok(());
        }
        let staged = self.0.packages.stage(assembled.dir.path())?;
        let bytes = store::read_regular_file(
            &staged.tree().join(EXTENSION_MANIFEST_FILE),
            manifest::MAX_MANIFEST_BYTES as u64,
        )
        .map_err(|error| format!("the staged manifest could not be read: {error}"))?;
        let validated = manifest::validate_installed(&bytes)?;
        if validated.package_hash != assembled.package_hash {
            return Err("the package changed while it was staged".into());
        }
        store::verify_tree(staged.tree(), &validated)?;
        let manifest = &validated.manifest;
        for old in index
            .iter()
            .filter(|record| record.id == manifest.id && record.version == manifest.version)
        {
            self.0.packages.remove(&old.package_hash)?;
            if let Err(error) = self.0.runtime.forget_package(&old.package_hash) {
                tracing::warn!(%error, "a replaced first-party package's runtime record remains");
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
                source: worker.display().to_string(),
            },
            installed_at_ms: now_ms(),
            size_bytes: validated.total_size(),
            file_count: manifest.files.len() as u32,
        };
        self.0.packages.commit(staged, &validated, record)?;
        tracing::info!(
            package = %manifest.id.0,
            package_hash = %validated.package_hash,
            "installed LUNA's own provider package"
        );
        Ok(())
    }

    /// Enable once per profile; afterwards only follow LUNA's updates.
    /// `true` when this call enabled it.
    fn default_enablement(
        &self,
        spec: &Spec,
        package_hash: &str,
        marker: &mut Marker,
    ) -> Result<bool, String> {
        let file = self.0.grants.read()?;
        let current = file
            .extensions
            .get(spec.package_id)
            .map(|grant| grant.package_hash.clone());
        let entry = marker
            .packages
            .entry(spec.package_id.to_owned())
            .or_default();
        let enabled_now = match current {
            Some(enabled) if enabled == package_hash => {
                entry.default_enabled_at_ms.get_or_insert_with(now_ms);
                false
            }
            Some(_) => {
                self.move_enablement(spec, package_hash)?;
                entry.default_enabled_at_ms.get_or_insert_with(now_ms);
                false
            }
            // The user turned it off after LUNA enabled it: it stays off.
            None if entry.default_enabled_at_ms.is_some() => false,
            None => {
                let request: SetExtensionEnabledRequest = serde_json::from_value(json!({
                    "extensionId": spec.package_id,
                    "packageHash": package_hash,
                    "enabled": true,
                    "grants": [NATIVE_CODE_CAPABILITY, PROVIDER_REGISTER_CAPABILITY],
                }))
                .map_err(|error| format!("the enable request could not be formed: {error}"))?;
                self.set_enabled(&request)?;
                entry.default_enabled_at_ms = Some(now_ms());
                true
            }
        };
        self.remove_unused_versions(spec, package_hash);
        Ok(enabled_now)
    }

    /// An earlier bundled hash is enabled: these bytes take its place with
    /// the same grants. Nothing runs at open, so nothing is drained.
    fn move_enablement(&self, spec: &Spec, package_hash: &str) -> Result<(), String> {
        let _permit = self.0.writes.try_admit().map_err(|error| {
            format!("the device writer is unavailable ({error:?}); it is retried at the next open")
        })?;
        let _gate = self.gate();
        let mut file = self.0.grants.read()?;
        let Some(record) = file.extensions.get_mut(spec.package_id) else {
            return Ok(());
        };
        record.package_hash = package_hash.to_owned();
        record.version = env!("CARGO_PKG_VERSION").to_owned();
        record.enabled_at_ms = now_ms();
        self.0.grants.write(file)?;
        Ok(())
    }

    /// Versions of the package that are neither bundled nor enabled.
    fn remove_unused_versions(&self, spec: &Spec, keep: &str) {
        let Ok(_permit) = self.0.writes.try_admit() else {
            return;
        };
        let _gate = self.gate();
        let enabled = self.0.grants.read().ok().and_then(|file| {
            file.extensions
                .get(spec.package_id)
                .map(|grant| grant.package_hash.clone())
        });
        let Ok(index) = self.0.packages.read_index() else {
            return;
        };
        for record in index.iter().filter(|record| {
            record.id.as_str() == spec.package_id
                && record.package_hash != keep
                && Some(&record.package_hash) != enabled.as_ref()
        }) {
            match self.0.packages.remove(&record.package_hash) {
                Ok(_) => {
                    let _ = self.0.runtime.forget_package(&record.package_hash);
                }
                Err(error) => {
                    tracing::warn!(%error, "an old first-party package version remains")
                }
            }
        }
    }

    /// An installed first-party package replaces the compiled registration
    /// of its provider id. Nothing restores that registration while the
    /// package is installed.
    pub(super) fn displace_builtins(
        &self,
        registry: &HarnessRegistry,
        statuses: &mut [FirstPartyStatus],
    ) {
        for status in statuses.iter_mut().filter(|status| status.installed) {
            let Ok(id) = HarnessId::parse(&status.provider_id) else {
                continue;
            };
            status.replaces_builtin = registry.unregister(id);
            if status.replaces_builtin {
                tracing::info!(
                    provider = %id,
                    package = %status.package_id,
                    "LUNA's own provider package replaces the compiled registration"
                );
            }
        }
    }
}
