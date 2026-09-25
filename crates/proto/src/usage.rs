//! L16 historical usage and account limits. Historical cost is an API-equivalent
//! estimate, never a subscription charge. Based on T3 Code's MIT usage contract.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// v2 adds a distinct OpenCode Go provider that older clients cannot decode.
pub const USAGE_CONTRACT_VERSION: u32 = 2;
/// A recent observation may show a countdown; older observations remain visible
/// only as last-reported data. This does not establish live account access.
pub const USAGE_LIMIT_FRESHNESS_MS: i64 = 5 * 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageProvider {
    Codex,
    Claude,
    /// OpenCode Go subscription quotas. No historical transcript adapter yet.
    OpenCodeGo,
}

impl UsageProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::OpenCodeGo => "OpenCode Go",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTokens {
    pub uncached_input: u64,
    pub cached_input: u64,
    pub cache_creation: u64,
    pub output: u64,
    /// A subset of output, excluded from total_tokens().
    pub reasoning: u64,
}

impl UsageTokens {
    pub fn total(self) -> u64 {
        self.uncached_input
            .saturating_add(self.cached_input)
            .saturating_add(self.cache_creation)
            .saturating_add(self.output)
    }

    pub fn add(&mut self, other: Self) {
        self.uncached_input = self.uncached_input.saturating_add(other.uncached_input);
        self.cached_input = self.cached_input.saturating_add(other.cached_input);
        self.cache_creation = self.cache_creation.saturating_add(other.cache_creation);
        self.output = self.output.saturating_add(other.output);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRequest {
    /// Supported: 1, 7, 30, 90. One day uses hourly buckets.
    pub days: u32,
    /// Viewer-calculated period starts. Daily starts use the viewer's calendar
    /// timezone, including DST; hourly starts anchor an exact rolling 24h.
    #[serde(default)]
    pub period_starts_ms: Vec<i64>,
    /// Exclusive end of the final period.
    #[serde(default)]
    pub until_ms: Option<i64>,
    /// Historical Cost/Tokens reads skip provider quota probes. Older clients
    /// keep the original combined snapshot behavior.
    #[serde(default = "default_include_limits")]
    pub include_limits: bool,
    /// User supplied USD per million tokens, keyed by an exact model name.
    #[serde(default)]
    pub price_overrides: HashMap<String, UsagePriceRate>,
}

fn default_include_limits() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePriceRate {
    pub input_per_million: f64,
    pub cached_per_million: f64,
    pub creation_per_million: f64,
    pub output_per_million: f64,
}

impl UsagePriceRate {
    pub fn valid(self) -> bool {
        [
            self.input_per_million,
            self.cached_per_million,
            self.creation_per_million,
            self.output_per_million,
        ]
        .into_iter()
        .all(|rate| rate.is_finite() && (0.0..=1_000_000.0).contains(&rate))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    pub start_ms: i64,
    pub provider: UsageProvider,
    pub model: String,
    pub tokens: UsageTokens,
    pub records: u64,
    pub sessions: u64,
    /// Sum only when every record in this bucket is priced.
    pub api_equivalent_cost_usd: Option<f64>,
    /// Priced portion of this bucket. Unlike the legacy all-or-nothing cost,
    /// this remains available when other records in the bucket are unpriced.
    #[serde(default)]
    pub known_cost_usd: Option<f64>,
    #[serde(default)]
    pub unpriced_records: Option<u64>,
    /// Modeled uncached-vs-cached input discount; unknown when any cached
    /// record lacks an exact model rate. Not a subscription billing amount.
    #[serde(default)]
    pub cache_savings_usd: Option<f64>,
    pub cost_quality: CostQuality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CostQuality {
    ProviderReported,
    ModelPriced,
    UserOverride,
    Unpriced,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageSourceStatus {
    Ready,
    Missing,
    Partial,
    Unsupported,
    Duplicate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSource {
    pub provider: UsageProvider,
    pub environment_id: String,
    /// SHA-256 of host identity, canonical home path, volume, and provider.
    pub fingerprint: String,
    pub status: UsageSourceStatus,
    pub distinct_sessions: u64,
    pub files_scanned: u64,
    pub files_cached: u64,
    pub failed_files: u64,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub contract_version: u32,
    pub read_at_ms: i64,
    pub time_zone: String,
    pub days: u32,
    pub resolution: String,
    #[serde(default)]
    pub period_starts_ms: Vec<i64>,
    #[serde(default)]
    pub until_ms: Option<i64>,
    pub buckets: Vec<UsageBucket>,
    pub sources: Vec<UsageSource>,
    pub scan_duration_ms: u64,
    pub partial: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LimitAvailability {
    Ready,
    Unsupported,
    ProbeFailed,
    Unverified,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitWindow {
    pub id: String,
    pub label: String,
    pub used_percent: f32,
    pub resets_at_ms: Option<i64>,
    pub duration_minutes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitSnapshot {
    pub provider: UsageProvider,
    /// Only a verified identity may be used for account de-duplication.
    pub verified_account_id: Option<String>,
    pub account_label: Option<String>,
    pub plan: Option<String>,
    pub checked_at_ms: Option<i64>,
    pub availability: LimitAvailability,
    pub windows: Vec<UsageLimitWindow>,
    pub reset_credits: Option<u64>,
    pub detail: Option<String>,
}

/// Interpretation of an explicitly reported reset time at display time.
/// Derived locally from the existing wire fields; never serialized or inferred
/// when the provider supplied no reset timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageResetState {
    Unknown,
    ReportedFuture { at_ms: i64 },
    ReportedPassed { at_ms: i64 },
    LastReported { at_ms: i64 },
}

impl UsageLimitSnapshot {
    /// None includes missing and future check times; neither proves freshness.
    pub fn checked_age_ms(&self, now_ms: i64) -> Option<i64> {
        now_ms
            .checked_sub(self.checked_at_ms?)
            .filter(|age| *age >= 0)
    }

    pub fn is_fresh(&self, now_ms: i64) -> bool {
        self.availability == LimitAvailability::Ready
            && self
                .checked_age_ms(now_ms)
                .is_some_and(|age| age < USAGE_LIMIT_FRESHNESS_MS)
    }

    pub fn reset_state(&self, window: &UsageLimitWindow, now_ms: i64) -> UsageResetState {
        let Some(at_ms) = window.resets_at_ms else {
            return UsageResetState::Unknown;
        };
        if !self.is_fresh(now_ms) {
            return UsageResetState::LastReported { at_ms };
        }
        if at_ms <= now_ms {
            UsageResetState::ReportedPassed { at_ms }
        } else {
            UsageResetState::ReportedFuture { at_ms }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub summary: UsageSummary,
    pub limits: Vec<UsageLimitSnapshot>,
}

/// Merge normalized environment reports without transferring transcripts.
/// A physical provider home contributes once even when two worktrees expose it.
pub fn merge_usage_snapshots(mut snapshots: Vec<UsageSnapshot>) -> Option<UsageSnapshot> {
    use std::collections::{HashMap, HashSet};
    snapshots.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.summary.read_at_ms));
    let mut merged = snapshots.first()?.clone();
    merged.summary.buckets.clear();
    merged.summary.sources.clear();
    merged.summary.warnings.clear();
    merged.limits.clear();
    let mut fingerprints = HashSet::new();
    let mut accounts: HashMap<(u8, String), usize> = HashMap::new();
    for snapshot in snapshots {
        if snapshot.summary.contract_version != USAGE_CONTRACT_VERSION
            || snapshot.summary.days != merged.summary.days
            || snapshot.summary.resolution != merged.summary.resolution
            || snapshot.summary.time_zone != merged.summary.time_zone
            || snapshot.summary.period_starts_ms != merged.summary.period_starts_ms
            || snapshot.summary.until_ms != merged.summary.until_ms
        {
            merged.summary.partial = true;
            merged
                .summary
                .warnings
                .push("Incompatible environment usage summary excluded".into());
            continue;
        }
        let mut accepted = HashSet::new();
        for mut source in snapshot.summary.sources {
            if matches!(
                source.status,
                UsageSourceStatus::Ready | UsageSourceStatus::Partial
            ) && !source.fingerprint.is_empty()
            {
                if fingerprints.insert(source.fingerprint.clone()) {
                    accepted.insert(source.provider as u8);
                } else {
                    source.status = UsageSourceStatus::Duplicate;
                    source.detail = Some("Same physical provider home already counted".into());
                    merged.summary.warnings.push(format!(
                        "{} duplicate physical source excluded",
                        source.provider.label()
                    ));
                }
            }
            merged.summary.sources.push(source);
        }
        merged.summary.buckets.extend(
            snapshot
                .summary
                .buckets
                .into_iter()
                .filter(|bucket| accepted.contains(&(bucket.provider as u8))),
        );
        for limit in snapshot.limits {
            if let Some(id) = limit
                .verified_account_id
                .as_ref()
                .filter(|id| !id.is_empty())
            {
                let key = (limit.provider as u8, id.clone());
                if let Some(&index) = accounts.get(&key) {
                    let prior = &merged.limits[index];
                    let better = (
                        limit.availability == LimitAvailability::Ready,
                        limit.checked_at_ms.unwrap_or(0),
                    ) > (
                        prior.availability == LimitAvailability::Ready,
                        prior.checked_at_ms.unwrap_or(0),
                    );
                    if better {
                        merged.limits[index] = limit;
                    }
                } else {
                    accounts.insert(key, merged.limits.len());
                    merged.limits.push(limit);
                }
            } else {
                merged.limits.push(limit);
            }
        }
    }
    merged.summary.partial |= merged
        .summary
        .sources
        .iter()
        .any(|source| source.status == UsageSourceStatus::Partial);
    Some(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reported_resets_require_a_recent_ready_check_for_countdown() {
        let now = 10 * 60_000;
        let future = now + 60_000;
        let cases = [
            (
                "codex future",
                UsageProvider::Codex,
                LimitAvailability::Ready,
                Some(now - 60_000),
                Some(future),
                UsageResetState::ReportedFuture { at_ms: future },
            ),
            (
                "go future",
                UsageProvider::OpenCodeGo,
                LimitAvailability::Ready,
                Some(now - 60_000),
                Some(future),
                UsageResetState::ReportedFuture { at_ms: future },
            ),
            (
                "passed",
                UsageProvider::Codex,
                LimitAvailability::Ready,
                Some(now - 60_000),
                Some(now),
                UsageResetState::ReportedPassed { at_ms: now },
            ),
            (
                "stale boundary",
                UsageProvider::OpenCodeGo,
                LimitAvailability::Ready,
                Some(now - USAGE_LIMIT_FRESHNESS_MS),
                Some(future),
                UsageResetState::LastReported { at_ms: future },
            ),
            (
                "failed last good",
                UsageProvider::Codex,
                LimitAvailability::ProbeFailed,
                Some(now - 60_000),
                Some(future),
                UsageResetState::LastReported { at_ms: future },
            ),
            (
                "future check",
                UsageProvider::OpenCodeGo,
                LimitAvailability::Ready,
                Some(now + 1),
                Some(future),
                UsageResetState::LastReported { at_ms: future },
            ),
            (
                "missing check",
                UsageProvider::Codex,
                LimitAvailability::Ready,
                None,
                Some(future),
                UsageResetState::LastReported { at_ms: future },
            ),
            (
                "missing reset",
                UsageProvider::OpenCodeGo,
                LimitAvailability::Ready,
                Some(now - 60_000),
                None,
                UsageResetState::Unknown,
            ),
        ];
        for (name, provider, availability, checked_at_ms, resets_at_ms, expected) in cases {
            let limit = UsageLimitSnapshot {
                provider,
                verified_account_id: Some("synthetic-account".into()),
                account_label: None,
                plan: None,
                checked_at_ms,
                availability,
                windows: vec![],
                reset_credits: None,
                detail: None,
            };
            let window = UsageLimitWindow {
                id: "synthetic".into(),
                label: "Synthetic window".into(),
                used_percent: 40.0,
                resets_at_ms,
                duration_minutes: Some(300),
            };
            assert_eq!(limit.reset_state(&window, now), expected, "{name}");
        }
        assert_eq!(USAGE_CONTRACT_VERSION, 2);
    }

    #[test]
    fn physical_sources_and_verified_accounts_dedupe() {
        let snapshot = |environment: &str, fingerprint: &str, read_at| UsageSnapshot {
            summary: UsageSummary {
                contract_version: USAGE_CONTRACT_VERSION,
                read_at_ms: read_at,
                time_zone: "UTC".into(),
                days: 7,
                resolution: "day".into(),
                period_starts_ms: vec![],
                until_ms: None,
                buckets: vec![UsageBucket {
                    start_ms: 0,
                    provider: UsageProvider::Codex,
                    model: "test".into(),
                    tokens: UsageTokens {
                        output: 10,
                        ..Default::default()
                    },
                    records: 1,
                    sessions: 1,
                    api_equivalent_cost_usd: None,
                    known_cost_usd: None,
                    unpriced_records: None,
                    cache_savings_usd: None,
                    cost_quality: CostQuality::Unpriced,
                }],
                sources: vec![UsageSource {
                    provider: UsageProvider::Codex,
                    environment_id: environment.into(),
                    fingerprint: fingerprint.into(),
                    status: UsageSourceStatus::Ready,
                    distinct_sessions: 1,
                    files_scanned: 1,
                    files_cached: 0,
                    failed_files: 0,
                    detail: None,
                }],
                scan_duration_ms: 1,
                partial: false,
                warnings: vec![],
            },
            limits: vec![UsageLimitSnapshot {
                provider: UsageProvider::Codex,
                verified_account_id: Some("account".into()),
                account_label: None,
                plan: None,
                checked_at_ms: Some(read_at),
                availability: LimitAvailability::Ready,
                windows: vec![],
                reset_credits: None,
                detail: None,
            }],
        };
        let merged =
            merge_usage_snapshots(vec![snapshot("one", "same", 1), snapshot("two", "same", 2)])
                .unwrap();
        assert_eq!(merged.summary.buckets.len(), 1);
        assert_eq!(merged.limits.len(), 1);
        assert!(
            merged
                .summary
                .sources
                .iter()
                .any(|source| source.status == UsageSourceStatus::Duplicate)
        );
        let distinct =
            merge_usage_snapshots(vec![snapshot("one", "one", 1), snapshot("two", "two", 2)])
                .unwrap();
        assert_eq!(distinct.summary.buckets.len(), 2);
    }

    #[test]
    fn opencode_go_limits_merge_only_verified_matching_accounts() {
        let snapshot = |id: Option<&str>, checked, availability| UsageSnapshot {
            summary: UsageSummary {
                contract_version: USAGE_CONTRACT_VERSION,
                read_at_ms: checked,
                time_zone: "UTC".into(),
                days: 7,
                resolution: "day".into(),
                period_starts_ms: vec![],
                until_ms: None,
                buckets: vec![],
                sources: vec![],
                scan_duration_ms: 0,
                partial: false,
                warnings: vec![],
            },
            limits: vec![UsageLimitSnapshot {
                provider: UsageProvider::OpenCodeGo,
                verified_account_id: id.map(str::to_owned),
                account_label: Some("synthetic Go".into()),
                plan: Some("Go".into()),
                checked_at_ms: Some(checked),
                availability,
                windows: vec![],
                reset_credits: None,
                detail: None,
            }],
        };
        let merged = merge_usage_snapshots(vec![
            snapshot(Some("same"), 20, LimitAvailability::ProbeFailed),
            snapshot(Some("same"), 10, LimitAvailability::Ready),
        ])
        .unwrap();
        assert_eq!(merged.limits.len(), 1);
        assert_eq!(merged.limits[0].availability, LimitAvailability::Ready);
        let unknown = merge_usage_snapshots(vec![
            snapshot(None, 10, LimitAvailability::Ready),
            snapshot(None, 20, LimitAvailability::Ready),
        ])
        .unwrap();
        assert_eq!(unknown.limits.len(), 2);
    }
}
