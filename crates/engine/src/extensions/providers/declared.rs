//! Permission facts of registered extension providers, for the resolver.
//!
//! `permissions::provider_capabilities` is a free function used by the
//! engine's admission and by the in-process pickers, so the declared modes
//! of a registered extension provider are published here, keyed by its id,
//! for as long as its registration lives. A registered extension id is never
//! a built-in spelling: contribution ids are namespaced and `luna.*` is
//! reserved, so a built-in provider never reads this table.
//!
//! Process-wide: two engines in one process that register the same provider
//! id share the most recent declaration (the app runs one engine).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, PoisonError, RwLock};

use luna_proto::{HarnessId, PermissionMode, ProviderPermissionCapabilities};

struct Entry {
    token: u64,
    permissions: ProviderPermissionCapabilities,
    mappings: BTreeMap<PermissionMode, String>,
}

static DECLARED: LazyLock<RwLock<HashMap<HarnessId, Vec<Entry>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Removes its declaration when the registration ends.
pub(super) struct DeclaredGuard {
    id: HarnessId,
    token: u64,
}

impl Drop for DeclaredGuard {
    fn drop(&mut self) {
        let mut table = DECLARED.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(entries) = table.get_mut(&self.id) {
            entries.retain(|entry| entry.token != self.token);
            if entries.is_empty() {
                table.remove(&self.id);
            }
        }
    }
}

pub(super) fn publish(
    id: HarnessId,
    permissions: ProviderPermissionCapabilities,
    mappings: BTreeMap<PermissionMode, String>,
) -> DeclaredGuard {
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    DECLARED
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(id)
        .or_default()
        .push(Entry {
            token,
            permissions,
            mappings,
        });
    DeclaredGuard { id, token }
}

/// The declared permission capabilities of a registered extension provider.
pub(crate) fn declared_permissions(id: HarnessId) -> Option<ProviderPermissionCapabilities> {
    DECLARED
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&id)
        .and_then(|entries| entries.last())
        .map(|entry| entry.permissions.clone())
}

/// How a registered extension provider says it maps `mode` natively.
pub(crate) fn declared_mapping(id: HarnessId, mode: PermissionMode) -> Option<String> {
    let table = DECLARED.read().unwrap_or_else(PoisonError::into_inner);
    let entry = table.get(&id)?.last()?;
    Some(match entry.mappings.get(&mode) {
        Some(mapping) => format!("extension provider: {mapping}"),
        None => "unavailable: this extension provider does not declare this mode".into(),
    })
}
