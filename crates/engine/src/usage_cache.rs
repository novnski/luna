//! Host-owned usage snapshots. Quotas and history have independent single-flight
//! refreshes; a reader never owns or cancels shared work. Only bounded normalized
//! results are persisted, with provider/account and query fences.
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use luna_proto::{
    HarnessId, LimitAvailability, USAGE_CONTRACT_VERSION, UsageLimitSnapshot, UsageProvider,
    UsageReadRequest, UsageReadResponse, UsageRequest, UsageSnapshot, UsageSummary,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{agent_accounts::AgentAccounts, registry::HarnessRegistry};

const REFRESH_MS: i64 = 60_000;
const RETRY_MS: i64 = 15_000;
const MAX_HISTORY: usize = 8;
const MAX_FILE: u64 = 4 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
struct HistoryEntry {
    scope: String,
    query: UsageRequest,
    summary: UsageSummary,
}

#[derive(Clone, Serialize, Deserialize)]
struct LimitEntry {
    value: UsageLimitSnapshot,
    attempted_at_ms: i64,
}

#[derive(Default, Clone, Serialize, Deserialize)]
struct Saved {
    version: u32,
    history: HashMap<String, HistoryEntry>,
    limits: HashMap<String, LimitEntry>,
}

#[derive(Default)]
struct State {
    saved: Saved,
    running: HashSet<String>,
    errors: HashMap<String, String>,
}

pub(crate) struct UsageCache {
    root: PathBuf,
    accounts: AgentAccounts,
    registry: Arc<HarnessRegistry>,
    state: Mutex<State>,
    changed: tokio::sync::watch::Sender<u64>,
    cancel: CancellationToken,
    started: std::sync::atomic::AtomicBool,
    persist_gate: tokio::sync::Mutex<()>,
    native_probes: bool,
    accounts_attempt: std::sync::atomic::AtomicI64,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn provider_harness(provider: UsageProvider) -> HarnessId {
    match provider {
        UsageProvider::Codex => HarnessId::Codex,
        UsageProvider::Claude => HarnessId::ClaudeCode,
        UsageProvider::OpenCodeGo => HarnessId::Opencode,
    }
}
fn unavailable(provider: UsageProvider) -> UsageLimitSnapshot {
    UsageLimitSnapshot {
        provider,
        verified_account_id: None,
        account_label: None,
        plan: None,
        checked_at_ms: None,
        availability: LimitAvailability::Unverified,
        windows: vec![],
        reset_credits: None,
        detail: None,
    }
}

impl UsageCache {
    pub(crate) fn new(
        accounts: AgentAccounts,
        registry: Arc<HarnessRegistry>,
        native_probes: bool,
    ) -> Arc<Self> {
        let root = accounts.usage_history_paths().2;
        let path = root.join("usage-snapshots-v1.json");
        let saved = std::fs::symlink_metadata(&path)
            .ok()
            .filter(|m| m.is_file() && m.len() <= MAX_FILE)
            .and_then(|_| std::fs::read(path).ok())
            .and_then(|v| serde_json::from_slice::<Saved>(&v).ok())
            .filter(|v| v.version == 1 && v.history.len() <= MAX_HISTORY && v.limits.len() <= 32)
            .unwrap_or_default();
        let (changed, _) = tokio::sync::watch::channel(0);
        Arc::new(Self {
            root,
            accounts,
            registry,
            state: Mutex::new(State {
                saved,
                ..Default::default()
            }),
            changed,
            cancel: CancellationToken::new(),
            started: std::sync::atomic::AtomicBool::new(false),
            persist_gate: tokio::sync::Mutex::new(()),
            native_probes,
            accounts_attempt: std::sync::atomic::AtomicI64::new(0),
        })
    }

    pub(crate) fn shutdown(&self) {
        self.cancel.cancel();
    }

    fn providers(&self) -> Vec<UsageProvider> {
        let enabled = self.registry.enabled_set();
        [
            UsageProvider::Codex,
            UsageProvider::Claude,
            UsageProvider::OpenCodeGo,
        ]
        .into_iter()
        .filter(|p| enabled.contains(&provider_harness(*p)))
        .collect()
    }

    fn limit_key(&self, provider: UsageProvider) -> String {
        let scope = if crate::usage::synthetic_mode_requested()
            || (!self.native_probes && provider == UsageProvider::OpenCodeGo)
        {
            format!(
                "fixture:{:?}:{:?}",
                self.root,
                std::env::var_os("LUNA_USAGE_SYNTHETIC_LIMITS_JSON")
            )
        } else if provider == UsageProvider::OpenCodeGo {
            crate::usage_opencode::selection_signature().unwrap_or_else(|_| "indeterminate".into())
        } else {
            let identity = self
                .accounts
                .resolve_execution(provider_harness(provider), None)
                .map(|v| v.auth_instance_id)
                .unwrap_or_else(|_| "disconnected".into());
            let (codex, claude, _) = self.accounts.usage_history_selection();
            match provider {
                UsageProvider::Codex => format!("{identity}:{codex:?}"),
                _ => format!("{identity}:{claude:?}"),
            }
        };
        format!("limit:{provider:?}:{}", digest(&scope))
    }

    pub(crate) fn history_scope(&self) -> String {
        let homes = self.accounts.usage_history_selection();
        let ids: Vec<_> = self
            .providers()
            .into_iter()
            .map(|p| self.limit_key(p))
            .collect();
        digest(&format!(
            "{homes:?}:{ids:?}:{:?}:{:?}",
            std::env::var_os("LUNA_USAGE_SYNTHETIC_CODEX_HOME"),
            std::env::var_os("LUNA_USAGE_SYNTHETIC_CLAUDE_HOME")
        ))
    }

    fn history_key(scope: &str, query: &UsageRequest) -> String {
        let mut prices: Vec<_> = query.price_overrides.iter().collect();
        prices.sort_by(|a, b| a.0.cmp(b.0));
        format!(
            "history:{}",
            digest(&format!(
                "{scope}:{}:{:?}:{:?}:{prices:?}",
                query.days, query.period_starts_ms, query.until_ms
            ))
        )
    }

    fn start_job(&self, key: &str, force: bool, fresh: impl FnOnce(&Saved) -> bool) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.cancel.is_cancelled()
            || state.running.contains(key)
            || (!force && fresh(&state.saved))
        {
            return false;
        }
        state.running.insert(key.into());
        state.errors.remove(key);
        true
    }

    fn finish(&self, key: &str, error: Option<String>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.running.remove(key);
        if let Some(error) = error {
            state.errors.insert(key.into(), error);
        }
        drop(state);
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    async fn persist(&self) {
        let _gate = self.persist_gate.lock().await;
        let mut saved = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .saved
            .clone();
        saved.version = 1;
        let root = self.root.clone();
        let _ = tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let path = root.join("usage-snapshots-v1.json");
            if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
                return;
            }
            let Ok(bytes) = serde_json::to_vec(&saved) else {
                return;
            };
            if bytes.len() as u64 > MAX_FILE {
                return;
            }
            let Ok(mut temp) = tempfile::NamedTempFile::new_in(root) else {
                return;
            };
            if temp.write_all(&bytes).is_ok() {
                let _ = temp.persist(path);
            }
        })
        .await;
    }

    fn queue_limit(self: &Arc<Self>, provider: UsageProvider, key: String, force: bool) {
        if !self.start_job(&key, force, |saved| {
            saved.limits.get(&key).is_some_and(|v| {
                let interval = if v.value.availability == LimitAvailability::Ready {
                    REFRESH_MS
                } else {
                    RETRY_MS
                };
                (0..interval).contains(&now().saturating_sub(v.attempted_at_ms))
                    && !(v.value.availability == LimitAvailability::Ready
                        && v.value.windows.iter().any(|window| {
                            window.resets_at_ms.is_some_and(|reset| {
                                reset <= now()
                                    && v.value.checked_at_ms.is_some_and(|checked| checked < reset)
                            })
                        }))
            })
        }) {
            return;
        }
        let cache = self.clone();
        tokio::spawn(async move {
            let observation = tokio::select! {
                _ = cache.cancel.cancelled() => { cache.finish(&key, None); return; }
                value = cache.probe_limit(provider) => value,
            };
            if cache.limit_key(provider) == key {
                {
                    let mut state = cache
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let mut value = observation;
                    if value.availability != LimitAvailability::Ready
                        && let Some(previous) = state
                            .saved
                            .limits
                            .get(&key)
                            .filter(|v| !v.value.windows.is_empty())
                    {
                        let detail = value.detail;
                        value = previous.value.clone();
                        value.availability = LimitAvailability::ProbeFailed;
                        value.detail = detail;
                    }
                    state.saved.limits.insert(
                        key.clone(),
                        LimitEntry {
                            value,
                            attempted_at_ms: now(),
                        },
                    );
                    if state.saved.limits.len() > 32
                        && let Some(oldest) = state
                            .saved
                            .limits
                            .iter()
                            .min_by_key(|(_, v)| v.attempted_at_ms)
                            .map(|(k, _)| k.clone())
                    {
                        state.saved.limits.remove(&oldest);
                    }
                }
                cache.persist().await;
            }
            cache.finish(&key, None);
        });
    }

    async fn probe_limit(&self, provider: UsageProvider) -> UsageLimitSnapshot {
        if crate::usage::synthetic_mode_requested() {
            return tokio::task::spawn_blocking(crate::usage::fixture_limits)
                .await
                .ok()
                .and_then(|v| v.into_iter().find(|v| v.provider == provider))
                .unwrap_or_else(|| unavailable(provider));
        }
        if !self.native_probes {
            return unavailable(provider);
        }
        match provider {
            UsageProvider::Claude => self.accounts.read_claude_usage_limit().await,
            UsageProvider::OpenCodeGo => crate::usage_opencode::read_go_limits().await,
            UsageProvider::Codex => {
                let Some((home, file_auth)) = self.accounts.codex_usage_target().ok() else {
                    return unavailable(provider);
                };
                let client = luna_harness::codex::native::CodexNativeClient::new(&home)
                    .for_execution_home(&home, file_auth);
                match tokio::time::timeout(
                    Duration::from_secs(8),
                    client.read_account_rate_limits(),
                )
                .await
                {
                    Ok(Ok(luna_harness::codex::native::CodexQuotaObservation::Ready(read))) => {
                        crate::usage::live_codex_limit(read)
                    }
                    Ok(Ok(luna_harness::codex::native::CodexQuotaObservation::Unsupported)) => {
                        let mut value = unavailable(provider);
                        value.availability = LimitAvailability::Unsupported;
                        value
                    }
                    _ => {
                        let mut value = unavailable(provider);
                        value.availability = LimitAvailability::ProbeFailed;
                        value.detail = Some("Codex native usage read did not complete".into());
                        value
                    }
                }
            }
        }
    }

    pub(crate) fn queue_accounts(self: &Arc<Self>, force: bool) {
        use std::sync::atomic::Ordering;
        if !self.native_probes
            || crate::usage::synthetic_mode_requested()
            || !self.providers().contains(&UsageProvider::Codex)
        {
            return;
        }
        let key = "accounts:codex";
        if !self.start_job(key, force, |_| {
            now().saturating_sub(self.accounts_attempt.load(Ordering::Acquire)) < REFRESH_MS
        }) {
            return;
        }
        self.accounts_attempt.store(now(), Ordering::Release);
        let cache = self.clone();
        tokio::spawn(async move {
            // Named/inactive Codex accounts keep independent provider windows.
            // This metadata refresh must never discover another provider.
            tokio::select! {
                _ = cache.cancel.cancelled() => {},
                _ = cache.accounts.list_harness(HarnessId::Codex, true) => {},
            }
            cache.finish(key, None);
        });
    }

    fn queue_history(
        self: &Arc<Self>,
        key: String,
        scope: String,
        mut query: UsageRequest,
        force: bool,
    ) {
        if !self.start_job(&key, force, |saved| {
            saved.history.get(&key).is_some_and(|v| {
                (0..REFRESH_MS).contains(&now().saturating_sub(v.summary.read_at_ms))
            })
        }) {
            return;
        }
        query.include_limits = false;
        let cache = self.clone();
        let homes = cache.accounts.usage_history_selection();
        let enabled = cache.providers();
        tokio::spawn(async move {
            let request = query.clone();
            let fetch = tokio::task::spawn_blocking(move || {
                if crate::usage::synthetic_mode_requested() {
                    crate::usage::snapshot(request)
                } else {
                    crate::usage::snapshot_live(
                        request,
                        homes.0.map(|v| v.0),
                        homes.1,
                        homes.2,
                        enabled,
                    )
                }
            });
            let result = tokio::select! {
                _ = cache.cancel.cancelled() => { cache.finish(&key, None); return; }
                result = fetch => result,
            };
            let error = if cache.history_scope() != scope {
                Some("Provider account selection changed during usage refresh".into())
            } else if let Ok(value) = result {
                {
                    let mut state = cache
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.saved.history.insert(
                        key.clone(),
                        HistoryEntry {
                            scope,
                            query,
                            summary: value.summary,
                        },
                    );
                    if state.saved.history.len() > MAX_HISTORY
                        && let Some(oldest) = state
                            .saved
                            .history
                            .iter()
                            .min_by_key(|(_, v)| v.summary.read_at_ms)
                            .map(|(k, _)| k.clone())
                    {
                        state.saved.history.remove(&oldest);
                    }
                }
                cache.persist().await;
                None
            } else {
                Some("Usage history scan did not complete".into())
            };
            cache.finish(&key, error);
        });
    }

    pub(crate) async fn read(
        self: &Arc<Self>,
        request: UsageReadRequest,
    ) -> Result<UsageReadResponse, String> {
        let scope = self.history_scope();
        let history_key = Self::history_key(&scope, &request.query);
        let limit_keys: Vec<_> = if request.query.include_limits {
            self.providers()
                .into_iter()
                .map(|p| (p, self.limit_key(p)))
                .collect()
        } else {
            vec![]
        };
        let mut changes = self.changed.subscribe();
        if request.include_history {
            self.queue_history(
                history_key.clone(),
                scope.clone(),
                request.query.clone(),
                request.refresh,
            );
        }
        for (provider, key) in &limit_keys {
            self.queue_limit(*provider, key.clone(), request.refresh);
        }
        let running = |state: &State| {
            (request.include_history && state.running.contains(&history_key))
                || limit_keys.iter().any(|(_, k)| state.running.contains(k))
        };
        if !request.background {
            loop {
                if !running(
                    &self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                ) {
                    break;
                }
                tokio::select! {
                    _ = self.cancel.cancelled() => return Err("Usage engine stopped".into()),
                    _ = changes.changed() => {}
                }
            }
        }
        if self.history_scope() != scope {
            return Err("Provider account selection changed during usage refresh".into());
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !request.background
            && request.include_history
            && let Some(error) = state.errors.get(&history_key)
        {
            return Err(error.clone());
        }
        let summary = request
            .include_history
            .then(|| state.saved.history.get(&history_key))
            .flatten()
            .map(|v| v.summary.clone())
            .unwrap_or_else(|| UsageSummary {
                contract_version: USAGE_CONTRACT_VERSION,
                read_at_ms: 0,
                time_zone: "UTC".into(),
                days: request.query.days,
                resolution: if request.query.days == 1 {
                    "hourly"
                } else {
                    "daily"
                }
                .into(),
                period_starts_ms: request.query.period_starts_ms.clone(),
                until_ms: request.query.until_ms,
                buckets: vec![],
                sources: vec![],
                scan_duration_ms: 0,
                partial: false,
                warnings: vec![],
            });
        let limits = limit_keys
            .iter()
            .map(|(p, k)| {
                state
                    .saved
                    .limits
                    .get(k)
                    .map(|v| v.value.clone())
                    .unwrap_or_else(|| unavailable(*p))
            })
            .collect();
        Ok(UsageReadResponse {
            snapshot: UsageSnapshot {
                summary,
                limits,
                live_sessions: vec![],
            },
            refreshing: running(&state),
            scope,
        })
    }

    pub(crate) fn start(self: &Arc<Self>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.started.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(self);
        let cancel = self.cancel.clone();
        runtime.spawn(async move {
            tokio::select! { _ = cancel.cancelled() => return, _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
            let mut next_prices = 0;
            loop {
                let Some(cache) = weak.upgrade() else { break };
                cache.queue_accounts(false);
                if now() >= next_prices
                    && cache.native_probes
                    && !crate::usage::synthetic_mode_requested()
                {
                    next_prices = now() + 60 * 60_000;
                    let root = cache.root.clone();
                    let cancel = cancel.clone();
                    tokio::spawn(async move {
                        tokio::select! { _ = cancel.cancelled() => {}, _ = crate::usage::refresh_live_prices(&root) => {} }
                    });
                }
                for provider in cache.providers() {
                    cache.queue_limit(provider, cache.limit_key(provider), false);
                }
                let history: Vec<_> = cache
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .saved
                    .history
                    .iter()
                    .filter(|(_, v)| now().saturating_sub(v.summary.read_at_ms) < 10 * 60_000)
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                for (key, entry) in history {
                    if cache.history_scope() == entry.scope {
                        cache.queue_history(key, entry.scope, entry.query, false);
                    }
                }
                drop(cache);
                tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(15)) => {} }
            }
        });
    }
}
