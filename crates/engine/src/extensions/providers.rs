//! CORE-08 provider extensions: an enabled package's `provider`
//! contributions become ordinary entries of the engine's `HarnessRegistry`,
//! so the existing catalog watch, picker, model discovery, run, input and
//! Stop paths serve them with no compiled provider id.
//!
//! A contribution is registered when its exact package hash is enabled in
//! this profile with `luna.capability/provider.register` and its
//! `luna-provider.json` declaration validates. Disabling the package (or
//! dropping that grant) unregisters it; chats keep their provider id and
//! history. Registration starts nothing: a worker runs only for model
//! discovery and for a run (`bridge`), each in its own supervised instance.
//!
//! Static facts (identity, profile, optional profiles, permission modes) come
//! only from the reviewed, hash-pinned declaration. A native provider worker
//! is user-level code without an OS sandbox (CORE-07); every view says so.

mod bridge;
mod declaration;
mod declared;

pub(crate) use declared::{declared_mapping, declared_permissions};

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use luna_proto::{ExtensionRuntimeKind, ExtensionTrust, HarnessId, PermissionMode};
use serde::Serialize;

use super::{ExtensionManager, store};
use crate::registry::HarnessRegistry;
use declaration::{DECLARATION_FILE, MAX_DECLARATION_BYTES, ProviderDeclaration};

/// The grant that lets a package's providers join the model picker.
const PROVIDER_REGISTER_CAPABILITY: &str = "luna.capability/provider.register";
const PROVIDER_KIND: &str = "provider";

/// Provider registration state, owned by the extension manager.
#[derive(Default)]
pub(super) struct ProviderHost {
    registry: OnceLock<Arc<HarnessRegistry>>,
    state: Mutex<HostState>,
}

#[derive(Default)]
struct HostState {
    registered: BTreeMap<HarnessId, Registered>,
    /// Provider contributions of enabled packages that are not registered.
    refused: Vec<ProviderEntryView>,
}

struct Registered {
    package_hash: String,
    declaration: ProviderDeclaration,
    provider: Arc<bridge::ExtensionProvider>,
    /// Keeps the declared permission modes visible to the resolver.
    _declared: declared::DeclaredGuard,
}

/// One provider contribution an enabled package wants registered.
struct Wanted {
    id: HarnessId,
    extension_id: luna_proto::contract::ExtensionId,
    version: String,
    package_hash: String,
    runtime: ExtensionRuntimeKind,
    declaration: ProviderDeclaration,
}

/// `ListExtensionProviders`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExtensionProvidersSnapshot {
    pub providers: Vec<ProviderEntryView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ProviderEntryState {
    Registered,
    Refused,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderEntryView {
    pub provider_id: String,
    pub extension_id: String,
    pub version: String,
    pub package_hash: String,
    pub state: ProviderEntryState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaration: Option<DeclarationView>,
    pub trust: ExtensionTrust,
    /// Always false: no OS sandbox is applied to any extension worker.
    pub os_sandbox: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_describe: Option<bridge::DescribeRecord>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeclarationView {
    pub display_name: String,
    pub profile: declaration::ProfileVersion,
    pub optional_profiles: Vec<declaration::OptionalProfile>,
    pub permission_modes: Vec<PermissionMode>,
    pub mode_mappings: BTreeMap<PermissionMode, String>,
    pub automatic_reviewer: bool,
    pub reasoning_levels: Vec<luna_proto::ReasoningLevel>,
    pub omitted: Vec<declaration::Omission>,
    pub limitations: Vec<String>,
}

impl DeclarationView {
    fn of(declaration: &ProviderDeclaration) -> Self {
        Self {
            display_name: declaration.display_name.clone(),
            profile: declaration.profile,
            optional_profiles: declaration.optional.iter().copied().collect(),
            permission_modes: declaration.modes.keys().copied().collect(),
            mode_mappings: declaration.modes.clone(),
            automatic_reviewer: declaration.automatic_reviewer,
            reasoning_levels: declaration.reasoning_levels.clone(),
            omitted: declaration.omitted.clone(),
            limitations: declaration.limitations.clone(),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ExtensionManager {
    /// Connect the engine's registry and register this profile's enabled
    /// providers. Called once while the engine assembles; starts nothing.
    pub(crate) fn attach_providers(&self, registry: Arc<HarnessRegistry>) {
        if self.0.providers.registry.set(registry).is_err() {
            tracing::warn!("extension providers were already attached");
            return;
        }
        self.providers_changed();
    }

    /// Re-read enablement and declarations and update the registry. Called
    /// after every change of what is enabled (and by later lifecycle owners
    /// after an update, rollback, removal or safe-mode change). Blocking I/O;
    /// it never takes the manager's gate, so callers holding it may call it.
    pub(super) fn providers_changed(&self) {
        let Some(registry) = self.0.providers.registry.get().cloned() else {
            return;
        };
        let (wanted, mut refused) = self.wanted_providers();
        let mut state = lock(&self.0.providers.state);
        let stale: Vec<HarnessId> = state
            .registered
            .iter()
            .filter(|(id, registered)| {
                !wanted.iter().any(|want| {
                    want.id == **id
                        && want.package_hash == registered.package_hash
                        && want.declaration == registered.declaration
                })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(registered) = state.registered.remove(&id) {
                registry.unregister(id);
                tracing::info!(
                    provider = %id,
                    package_hash = %registered.package_hash,
                    "extension provider left the registry"
                );
            }
        }
        for want in wanted {
            if state.registered.contains_key(&want.id) {
                continue;
            }
            // Publish the declared modes before the catalog announces the
            // provider, so a picker reacting to the change resolves them.
            let guard = declared::publish(
                want.id,
                want.declaration.permissions(),
                want.declaration.modes.clone(),
            );
            let provider = Arc::new(bridge::ExtensionProvider::new(
                want.id,
                want.extension_id.clone(),
                want.version.clone(),
                want.package_hash.clone(),
                want.runtime,
                want.declaration.clone(),
                Arc::downgrade(&self.0),
            ));
            match registry.try_register(provider.clone()) {
                Ok(()) => {
                    tracing::info!(
                        provider = %want.id,
                        package_hash = %want.package_hash,
                        "extension provider joined the registry"
                    );
                    state.registered.insert(
                        want.id,
                        Registered {
                            package_hash: want.package_hash,
                            declaration: want.declaration,
                            provider,
                            _declared: guard,
                        },
                    );
                }
                Err(error) => {
                    drop(guard);
                    refused.push(entry_view(
                        &want.id.to_string(),
                        want.extension_id.as_str(),
                        &want.version,
                        &want.package_hash,
                        want.runtime,
                        Some(&want.declaration),
                        Some(format!("not registered: {error}")),
                    ));
                }
            }
        }
        state.refused = refused;
    }

    /// Every provider contribution of an enabled package: registered ones
    /// with their last `describe`, then refused ones with the reason.
    pub(crate) fn provider_snapshot(&self) -> ExtensionProvidersSnapshot {
        let state = lock(&self.0.providers.state);
        let mut providers: Vec<ProviderEntryView> = state
            .registered
            .iter()
            .map(|(id, registered)| {
                let mut view = entry_view(
                    &id.to_string(),
                    registered.provider.extension_id().as_str(),
                    registered.provider.version(),
                    &registered.package_hash,
                    registered.provider.runtime_kind(),
                    Some(&registered.declaration),
                    None,
                );
                view.state = ProviderEntryState::Registered;
                view.last_describe = registered.provider.last_describe();
                view
            })
            .collect();
        providers.extend(state.refused.iter().cloned());
        ExtensionProvidersSnapshot { providers }
    }

    /// Whether `package_hash` is still the enabled package of `extension_id`
    /// in this profile (a cheap file read; never starts anything).
    fn provider_package_enabled(&self, extension_id: &str, package_hash: &str) -> bool {
        self.0.unavailable.is_none()
            && self.0.grants.read().is_ok_and(|file| {
                file.extensions
                    .get(extension_id)
                    .is_some_and(|grant| grant.package_hash == package_hash)
            })
    }

    fn wanted_providers(&self) -> (Vec<Wanted>, Vec<ProviderEntryView>) {
        let mut wanted = Vec::new();
        let mut refused = Vec::new();
        if self.0.unavailable.is_some() {
            return (wanted, refused);
        }
        let (index, file) = match (self.0.packages.read_index(), self.0.grants.read()) {
            (Ok(index), Ok(file)) => (index, file),
            (Err(error), _) | (_, Err(error)) => {
                tracing::warn!(%error, "extension providers unregistered: enablement is unreadable");
                return (wanted, refused);
            }
        };
        for (extension_id, grant) in &file.extensions {
            let Some(record) = index
                .iter()
                .find(|record| record.package_hash == grant.package_hash)
            else {
                continue;
            };
            let validated = match self.0.packages.read_manifest(&record.package_hash) {
                Ok(validated) => validated,
                Err(error) => {
                    tracing::warn!(extension = %extension_id, %error, "enabled package unreadable");
                    continue;
                }
            };
            let manifest = &validated.manifest;
            let contributions: Vec<String> = manifest
                .contributions
                .iter()
                .filter(|contribution| contribution.kind == PROVIDER_KIND)
                .map(|contribution| contribution.id.as_str().to_owned())
                .collect();
            if contributions.is_empty() {
                continue;
            }
            let runtime = manifest.runtime.kind;
            let refuse = |refused: &mut Vec<ProviderEntryView>, reason: String| {
                for id in &contributions {
                    refused.push(entry_view(
                        id,
                        extension_id,
                        &record.version,
                        &record.package_hash,
                        runtime,
                        None,
                        Some(reason.clone()),
                    ));
                }
            };
            if !grant
                .grants
                .iter()
                .any(|granted| granted == PROVIDER_REGISTER_CAPABILITY)
            {
                refuse(
                    &mut refused,
                    format!(
                        "{PROVIDER_REGISTER_CAPABILITY} is not granted, so its providers stay out of the model picker"
                    ),
                );
                continue;
            }
            let declarations = match self.read_declaration(&validated, &contributions) {
                Ok(declarations) => declarations,
                Err(reason) => {
                    refuse(&mut refused, reason);
                    continue;
                }
            };
            for (id, declaration) in declarations {
                let parsed = HarnessId::parse(&id)
                    .map_err(|error| format!("{id} is not a provider id: {error}"));
                match (parsed, declaration) {
                    (Ok(provider_id), Ok(declaration)) => wanted.push(Wanted {
                        id: provider_id,
                        extension_id: record.id.clone(),
                        version: record.version.clone(),
                        package_hash: record.package_hash.clone(),
                        runtime,
                        declaration,
                    }),
                    (Err(reason), _) | (_, Err(reason)) => refused.push(entry_view(
                        &id,
                        extension_id,
                        &record.version,
                        &record.package_hash,
                        runtime,
                        None,
                        Some(reason),
                    )),
                }
            }
        }
        (wanted, refused)
    }

    /// The package's `luna-provider.json`: listed, read from the installed
    /// tree and checked against the manifest's pin before it is parsed.
    fn read_declaration(
        &self,
        validated: &super::manifest::ValidatedManifest,
        contributions: &[String],
    ) -> Result<BTreeMap<String, Result<ProviderDeclaration, String>>, String> {
        let pinned = validated
            .manifest
            .files
            .iter()
            .find(|file| file.path == DECLARATION_FILE)
            .ok_or_else(|| format!("the package lists no {DECLARATION_FILE}"))?;
        let path = self
            .0
            .packages
            .package_dir(&validated.package_hash)
            .join(DECLARATION_FILE);
        let bytes = store::read_regular_file(&path, MAX_DECLARATION_BYTES)
            .map_err(|error| format!("{DECLARATION_FILE} could not be read: {error}"))?;
        if super::manifest::sha256_hex(&bytes) != pinned.sha256 {
            return Err(format!(
                "{DECLARATION_FILE} changed after installation; reinstall the package"
            ));
        }
        declaration::parse(&bytes, contributions)
    }
}

fn entry_view(
    provider_id: &str,
    extension_id: &str,
    version: &str,
    package_hash: &str,
    runtime: ExtensionRuntimeKind,
    declaration: Option<&ProviderDeclaration>,
    reason: Option<String>,
) -> ProviderEntryView {
    ProviderEntryView {
        provider_id: provider_id.to_owned(),
        extension_id: extension_id.to_owned(),
        version: version.to_owned(),
        package_hash: package_hash.to_owned(),
        state: ProviderEntryState::Refused,
        reason,
        declaration: declaration.map(DeclarationView::of),
        trust: runtime.trust(),
        os_sandbox: false,
        last_describe: None,
    }
}
