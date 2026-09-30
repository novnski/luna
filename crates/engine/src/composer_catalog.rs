//! Small bounded last-good cache. Keys are hashed execution identities; never credentials.
use luna_proto::{
    ComposerCatalog, ComposerCatalogKind, ComposerCatalogSlice, ComposerCatalogStatus,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub(crate) const TTL_MS: u64 = 300_000;
const MAX_STALE: Duration = Duration::from_secs(1800);
#[derive(Default)]
pub(crate) struct CatalogCache(HashMap<String, (Instant, ComposerCatalog)>);
impl CatalogCache {
    pub(crate) fn fresh(&self, scope: &str) -> Option<ComposerCatalog> {
        self.0
            .get(scope)
            .filter(|(at, value)| {
                !value.stale
                    && at.elapsed() < Duration::from_millis(TTL_MS)
                    && value
                        .slices
                        .iter()
                        .all(|slice| slice.status != ComposerCatalogStatus::Unavailable)
            })
            .map(|(_, value)| value.clone())
    }
    pub(crate) fn update(&mut self, scope: String, mut value: ComposerCatalog) -> ComposerCatalog {
        self.0.retain(|_, (at, _)| at.elapsed() < MAX_STALE);
        value.scope = scope.clone();
        value.ttl_ms = TTL_MS;
        value.fetched_at_ms = chrono::Utc::now().timestamp_millis();
        if let Some((_, previous)) = self.0.get(&scope) {
            for slice in &value.slices {
                if slice.status == ComposerCatalogStatus::Unavailable {
                    let retained: Vec<_> = previous
                        .entries
                        .iter()
                        .filter(|entry| entry.kind == slice.kind)
                        .cloned()
                        .collect();
                    if !retained.is_empty() {
                        value.stale = true;
                        value.entries.extend(retained);
                    }
                }
            }
        }
        // A failed retry must not renew the last-good timestamp indefinitely.
        if !value.stale {
            if self.0.len() >= 32
                && !self.0.contains_key(&scope)
                && let Some(oldest) = self
                    .0
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(key, _)| key.clone())
            {
                self.0.remove(&oldest);
            }
            self.0.insert(scope, (Instant::now(), value.clone()));
        }
        value
    }
}
pub(crate) fn unavailable() -> ComposerCatalog {
    ComposerCatalog {
        slices: [
            ComposerCatalogKind::Plugin,
            ComposerCatalogKind::App,
            ComposerCatalogKind::Skill,
        ]
        .into_iter()
        .map(|kind| ComposerCatalogSlice {
            kind,
            status: ComposerCatalogStatus::Unavailable,
            disabled_count: 0,
            detail: Some(
                "Native integration discovery is unavailable; retry when the host is connected"
                    .into(),
            ),
        })
        .collect(),
        ..Default::default()
    }
}
