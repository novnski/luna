//! L16 historical usage and account limits. Historical cost is an API-equivalent
//! estimate, never a subscription charge. Based on T3 Code's MIT usage contract.

use serde::{Deserialize, Serialize};

pub const USAGE_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageProvider {
    Codex,
    Claude,
}

impl UsageProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
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
    pub cost_quality: CostQuality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CostQuality {
    ProviderReported,
    ModelPriced,
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
    fn physical_sources_and_verified_accounts_dedupe() {
        let snapshot = |environment: &str, fingerprint: &str, read_at| UsageSnapshot {
            summary: UsageSummary {
                contract_version: USAGE_CONTRACT_VERSION,
                read_at_ms: read_at,
                time_zone: "UTC".into(),
                days: 7,
                resolution: "day".into(),
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
}
