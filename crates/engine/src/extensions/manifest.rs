//! Version-1 manifest validation. Pure: it reads only the manifest bytes and
//! decides whether this host can accept the package. File contents are
//! checked separately against the staged copy (`store::verify_tree`).

use std::collections::{BTreeSet, HashSet};

use luna_proto::{
    EXTENSION_API, EXTENSION_CONTRIBUTION_KINDS, EXTENSION_MANIFEST_FILE,
    EXTENSION_MANIFEST_VERSION, EXTENSION_WASM_PROFILE, EXTENSION_WORKER_PROTOCOL,
    ExtensionCapabilityRequest, ExtensionManifest, ExtensionRuntimeKind, NATIVE_CODE_CAPABILITY,
    RESERVED_EXTENSION_NAMESPACE, extension_capability,
};
use sha2::{Digest as _, Sha256};

/// Largest accepted manifest.
pub(super) const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// Most files one package may hold (the manifest excluded).
pub(super) const MAX_FILES: usize = 4096;
/// Largest total of all file sizes in one package.
pub(super) const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
/// Longest relative path, in bytes, and deepest nesting.
const MAX_PATH_BYTES: usize = 240;
const MAX_PATH_DEPTH: usize = 16;
const MAX_SETTINGS_SCHEMA_BYTES: u64 = 64 * 1024;
const MAX_LABEL_CHARS: usize = 120;
const MAX_TEXT_CHARS: usize = 1000;

/// A manifest this host accepts, with its exact bytes and package hash.
#[derive(Debug, Clone)]
pub(super) struct ValidatedManifest {
    pub manifest: ExtensionManifest,
    pub package_hash: String,
    /// Requested capabilities in manifest order; a native package's implicit
    /// [`NATIVE_CODE_CAPABILITY`] comes first and is always required.
    pub capabilities: Vec<ExtensionCapabilityRequest>,
}

impl ValidatedManifest {
    /// Executable entry points (any platform), which install read-only and
    /// executable. Nothing in the installer runs them.
    pub fn executable_paths(&self) -> BTreeSet<&str> {
        match self.manifest.runtime.kind {
            ExtensionRuntimeKind::Native => self
                .manifest
                .runtime
                .entries
                .values()
                .map(String::as_str)
                .collect(),
            ExtensionRuntimeKind::Data | ExtensionRuntimeKind::Wasm => BTreeSet::new(),
        }
    }

    pub fn total_size(&self) -> u64 {
        self.manifest.files.iter().map(|file| file.size).sum()
    }
}

/// This host's platform key: `os-arch`, e.g. `macos-aarch64`.
pub(super) fn host_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    crate::repos::hex(&Sha256::digest(bytes))
}

pub(super) fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `MAJOR.MINOR.PATCH`, decimal, no sign, suffix or leading zeros.
pub(super) fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.split('.');
    let mut next = || -> Option<u64> {
        let part = parts.next()?;
        let canonical = !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'));
        canonical.then(|| part.parse().ok()).flatten()
    };
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

/// Whether `version` falls in `[min, max)`.
pub(super) fn version_in_range(version: &str, min: &str, max: Option<&str>) -> bool {
    let (Some(version), Some(min)) = (parse_version(version), parse_version(min)) else {
        return false;
    };
    version >= min
        && max
            .map(|max| parse_version(max).is_some_and(|max| version < max))
            .unwrap_or(true)
}

pub(super) fn range_label(min: &str, max: Option<&str>) -> String {
    match max {
        Some(max) => format!(">= {min}, < {max}"),
        None => format!(">= {min}"),
    }
}

/// One package-relative path, `/`-separated. Refuses anything that could
/// name a location outside the package root or collide on a
/// case-insensitive filesystem only by spelling (checked by callers).
pub(super) fn check_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return Err(format!(
            "path {path:?} is empty or longer than {MAX_PATH_BYTES} bytes"
        ));
    }
    if path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return Err(format!("path {path:?} must be relative and use `/`"));
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > MAX_PATH_DEPTH {
        return Err(format!(
            "path {path:?} is nested deeper than {MAX_PATH_DEPTH} levels"
        ));
    }
    for segment in segments {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!("path {path:?} escapes or is not canonical"));
        }
        if segment.chars().any(char::is_control) {
            return Err(format!("path {path:?} contains a control character"));
        }
    }
    Ok(())
}

fn bounded(label: &str, value: &str, max_chars: usize, required: bool) -> Result<(), String> {
    let trimmed = value.trim();
    if required && trimmed.is_empty() {
        return Err(format!("{label} is empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!("{label} is longer than {max_chars} characters"));
    }
    Ok(())
}

/// Parse and validate a manifest for this host. Refusal messages name the
/// field and the reason; they never include file contents.
pub(super) fn validate(bytes: &[u8]) -> Result<ValidatedManifest, String> {
    validate_with(bytes, false)
}

/// [`validate`] for a package already in the store, or LUNA's own package
/// being installed (CORE-11): the reserved `luna` namespace is accepted,
/// since only LUNA installs into it.
pub(super) fn validate_installed(bytes: &[u8]) -> Result<ValidatedManifest, String> {
    validate_with(bytes, true)
}

fn validate_with(bytes: &[u8], allow_reserved: bool) -> Result<ValidatedManifest, String> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "{EXTENSION_MANIFEST_FILE} is larger than {} KiB",
            MAX_MANIFEST_BYTES / 1024
        ));
    }
    let manifest: ExtensionManifest = serde_json::from_slice(bytes)
        .map_err(|error| format!("{EXTENSION_MANIFEST_FILE} is invalid: {error}"))?;
    if manifest.manifest_version != EXTENSION_MANIFEST_VERSION {
        return Err(format!(
            "manifest version {} is not supported; this LUNA reads version {EXTENSION_MANIFEST_VERSION}",
            manifest.manifest_version
        ));
    }

    let id = &manifest.id.0;
    let namespace = id.namespace();
    if !allow_reserved
        && (namespace == RESERVED_EXTENSION_NAMESPACE
            || namespace.starts_with(&format!("{RESERVED_EXTENSION_NAMESPACE}.")))
    {
        return Err(format!(
            "the `{RESERVED_EXTENSION_NAMESPACE}` namespace is reserved for LUNA's own packages ({id})"
        ));
    }
    if parse_version(&manifest.version).is_none() {
        return Err(format!(
            "version {:?} is not MAJOR.MINOR.PATCH",
            manifest.version
        ));
    }
    bounded("displayName", &manifest.display_name, MAX_LABEL_CHARS, true)?;
    bounded("publisher", &manifest.publisher, MAX_LABEL_CHARS, true)?;
    if let Some(description) = &manifest.description {
        bounded("description", description, MAX_TEXT_CHARS, false)?;
    }
    if let Some(license) = &manifest.license {
        bounded("license", license, MAX_LABEL_CHARS, false)?;
    }
    if !manifest.api.accepts(EXTENSION_API) {
        return Err(format!(
            "requires extension API {}.{} or newer within major {}; this LUNA provides {}.{}",
            manifest.api.major,
            manifest.api.min_minor,
            manifest.api.major,
            EXTENSION_API.major,
            EXTENSION_API.minor
        ));
    }

    // Files first: entry points and schemas must name listed files.
    if manifest.files.len() > MAX_FILES {
        return Err(format!("the package lists more than {MAX_FILES} files"));
    }
    let mut listed = HashSet::new();
    let mut folded = HashSet::new();
    let mut total: u64 = 0;
    for file in &manifest.files {
        check_relative_path(&file.path)?;
        if file.path == EXTENSION_MANIFEST_FILE {
            return Err(format!(
                "{EXTENSION_MANIFEST_FILE} is hashed as the package identity and must not list itself"
            ));
        }
        if !is_sha256_hex(&file.sha256) {
            return Err(format!(
                "{}: sha256 must be 64 lowercase hex characters",
                file.path
            ));
        }
        // Case-insensitive filesystems would merge two spellings into one file.
        if !listed.insert(file.path.as_str()) || !folded.insert(file.path.to_lowercase()) {
            return Err(format!("{} is listed more than once", file.path));
        }
        total = total
            .checked_add(file.size)
            .filter(|total| *total <= MAX_PACKAGE_BYTES)
            .ok_or_else(|| {
                format!(
                    "the package is larger than {} MiB",
                    MAX_PACKAGE_BYTES / (1024 * 1024)
                )
            })?;
    }
    // A listed file cannot also be a directory prefix of another listed file.
    for path in &listed {
        for (end, _) in path.match_indices('/') {
            let prefix = &path[..end];
            if listed.contains(prefix) {
                return Err(format!(
                    "{prefix} is listed as a file and used as a directory"
                ));
            }
        }
    }
    let require_listed = |label: &str, path: &str| -> Result<(), String> {
        check_relative_path(path)?;
        if listed.contains(path) {
            Ok(())
        } else {
            Err(format!("{label} {path} is not a listed file"))
        }
    };
    let entry_label = |key: &str| format!("runtime.entries[{key}]");

    let platform = host_platform();
    let runtime = &manifest.runtime;
    match runtime.kind {
        ExtensionRuntimeKind::Data => {
            if runtime.protocol.is_some() || runtime.module.is_some() || !runtime.entries.is_empty()
            {
                return Err("a data package has no protocol, module or entries".into());
            }
        }
        ExtensionRuntimeKind::Wasm => {
            let protocol = runtime
                .protocol
                .ok_or("a wasm package must declare its portable profile in runtime.protocol")?;
            if !protocol.accepts(EXTENSION_WASM_PROFILE) {
                return Err(format!(
                    "requires portable profile {}.{}; this LUNA provides {}.{}",
                    protocol.major,
                    protocol.min_minor,
                    EXTENSION_WASM_PROFILE.major,
                    EXTENSION_WASM_PROFILE.minor
                ));
            }
            if !runtime.entries.is_empty() {
                return Err("a wasm package names one runtime.module, not entries".into());
            }
            let module = runtime
                .module
                .as_deref()
                .ok_or("a wasm package must name runtime.module")?;
            require_listed("runtime.module", module)?;
        }
        ExtensionRuntimeKind::Native => {
            let protocol = runtime
                .protocol
                .ok_or("a native package must declare its worker protocol in runtime.protocol")?;
            if !protocol.accepts(EXTENSION_WORKER_PROTOCOL) {
                return Err(format!(
                    "requires worker protocol {}.{}; this LUNA provides {}.{}",
                    protocol.major,
                    protocol.min_minor,
                    EXTENSION_WORKER_PROTOCOL.major,
                    EXTENSION_WORKER_PROTOCOL.minor
                ));
            }
            if runtime.module.is_some() {
                return Err("a native package names runtime.entries, not a module".into());
            }
            for (key, path) in &runtime.entries {
                check_platform_key(key)?;
                require_listed(entry_label(key.as_str()).as_str(), path.as_str())?;
            }
            if !runtime.entries.contains_key(&platform) {
                return Err(format!(
                    "the package has no native entry point for this device ({platform})"
                ));
            }
        }
    }
    if let Some(limits) = &runtime.limits {
        if runtime.kind == ExtensionRuntimeKind::Data {
            return Err("a data package runs nothing, so it has no runtime.limits".into());
        }
        if manifest.api.min_minor < 1 {
            return Err("runtime.limits needs extension API 1.1; set api.minMinor to 1".into());
        }
        let in_range = |label: &str, value: Option<u32>, (min, max): (u32, u32)| match value {
            Some(value) if value < min || value > max => Err(format!(
                "runtime.limits.{label} must be between {min} and {max}"
            )),
            _ => Ok(()),
        };
        in_range(
            "memoryMiB",
            limits.memory_mib,
            luna_proto::EXTENSION_MEMORY_MIB_RANGE,
        )?;
        in_range(
            "cpuSeconds",
            limits.cpu_seconds,
            luna_proto::EXTENSION_CPU_SECONDS_RANGE,
        )?;
    }
    for key in &manifest.platforms {
        check_platform_key(key)?;
    }
    if !manifest.platforms.is_empty() && !manifest.platforms.contains(&platform) {
        return Err(format!(
            "the package does not support this device ({platform})"
        ));
    }

    let mut dependencies = HashSet::new();
    for dependency in &manifest.dependencies {
        if dependency.id == manifest.id {
            return Err("a package cannot depend on itself".into());
        }
        if !dependencies.insert(&dependency.id) {
            return Err(format!(
                "dependency {} is listed more than once",
                dependency.id.0
            ));
        }
        let min = parse_version(&dependency.min_version).ok_or_else(|| {
            format!(
                "dependency {}: minVersion {:?} is not MAJOR.MINOR.PATCH",
                dependency.id.0, dependency.min_version
            )
        })?;
        if let Some(max) = &dependency.max_version {
            let max = parse_version(max).ok_or_else(|| {
                format!(
                    "dependency {}: maxVersion {max:?} is not MAJOR.MINOR.PATCH",
                    dependency.id.0
                )
            })?;
            if max <= min {
                return Err(format!(
                    "dependency {}: maxVersion must be greater than minVersion",
                    dependency.id.0
                ));
            }
        }
    }

    let mut contributions = HashSet::new();
    for contribution in &manifest.contributions {
        let contribution_id = &contribution.id.0;
        let inside = contribution_id.namespace() == namespace
            && (contribution_id.name() == id.name()
                || contribution_id
                    .name()
                    .strip_prefix(id.name())
                    .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1));
        if !inside {
            return Err(format!(
                "contribution {contribution_id} is outside the package's namespace; use {id} or {id}.<name>"
            ));
        }
        if !contributions.insert(contribution_id) {
            return Err(format!(
                "contribution {contribution_id} is declared more than once"
            ));
        }
        if !EXTENSION_CONTRIBUTION_KINDS.contains(&contribution.kind.as_str()) {
            return Err(format!(
                "contribution {contribution_id}: kind {:?} is not supported by this LUNA",
                contribution.kind
            ));
        }
        if contribution.kind == "provider" && runtime.kind == ExtensionRuntimeKind::Data {
            return Err(format!(
                "contribution {contribution_id}: a provider needs a wasm or native runtime"
            ));
        }
        if let Some(title) = &contribution.title {
            bounded("contribution title", title, MAX_LABEL_CHARS, false)?;
        }
    }

    let mut capabilities = Vec::with_capacity(manifest.capabilities.len() + 1);
    if runtime.kind == ExtensionRuntimeKind::Native {
        capabilities.push(ExtensionCapabilityRequest {
            id: luna_proto::contract::NamespacedId::parse(NATIVE_CODE_CAPABILITY)
                .expect("the native-code capability id is well formed"),
            required: true,
            reason: None,
        });
    }
    let mut requested = HashSet::new();
    for capability in &manifest.capabilities {
        let capability_id = capability.id.as_str();
        if extension_capability(capability_id).is_none() {
            return Err(format!(
                "capability {capability_id} is not one this LUNA can grant"
            ));
        }
        if !requested.insert(capability_id) {
            return Err(format!(
                "capability {capability_id} is requested more than once"
            ));
        }
        if let Some(reason) = &capability.reason {
            bounded("capability reason", reason, MAX_TEXT_CHARS, false)?;
        }
        if capability_id == NATIVE_CODE_CAPABILITY {
            if runtime.kind != ExtensionRuntimeKind::Native {
                return Err(format!(
                    "{NATIVE_CODE_CAPABILITY} only applies to a native package"
                ));
            }
            // Keep the package's reason, but it stays required.
            capabilities[0].reason = capability.reason.clone();
            continue;
        }
        capabilities.push(capability.clone());
    }

    if let Some(settings) = &manifest.settings {
        if settings.schema_version == 0 {
            return Err("settings.schemaVersion starts at 1".into());
        }
        require_listed("settings.schema", settings.schema.as_str())?;
        let size = manifest
            .files
            .iter()
            .find(|file| file.path == settings.schema)
            .map_or(0, |file| file.size);
        if size > MAX_SETTINGS_SCHEMA_BYTES {
            return Err("settings.schema is larger than 64 KiB".into());
        }
    }
    if manifest
        .data
        .as_ref()
        .is_some_and(|data| data.schema_version == 0)
    {
        return Err("data.schemaVersion starts at 1".into());
    }
    if let Some(from) = manifest.data.as_ref().and_then(|data| data.migrates_from) {
        if manifest.api.min_minor < 2 {
            return Err("data.migratesFrom needs extension API 1.2; set api.minMinor to 2".into());
        }
        if runtime.kind == ExtensionRuntimeKind::Data {
            return Err("a data package runs nothing, so it cannot migrate data".into());
        }
        let schema = manifest.data.as_ref().map_or(0, |data| data.schema_version);
        if from == 0 || from >= schema {
            return Err(
                "data.migratesFrom is an older schema: at least 1 and below data.schemaVersion"
                    .into(),
            );
        }
    }

    Ok(ValidatedManifest {
        package_hash: sha256_hex(bytes),
        manifest,
        capabilities,
    })
}

fn check_platform_key(key: &str) -> Result<(), String> {
    let valid = key.split_once('-').is_some_and(|(os, arch)| {
        let part = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        };
        part(os) && part(arch)
    });
    if valid {
        Ok(())
    } else {
        Err(format!("platform {key:?} is not an `os-arch` key"))
    }
}
