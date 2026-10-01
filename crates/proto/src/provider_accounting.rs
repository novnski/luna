//! Live provider accounting (CC-06): what a provider's own stream reported about
//! usage, cost, plan rate limits, API retries and the runtime it actually ran.
//!
//! Control-plane data only. It is never a transcript part, never added to the
//! transcript-history buckets of [`crate::UsageSummary`] (those already scan the
//! same native transcripts), and every unknown value stays `None`.
//!
//! Claude reports `total_cost_usd` and `modelUsage` as running totals for one
//! native session ("read the latest result rather than summing"), so consumers
//! keep the latest report per native session and never add reports together.

use serde::{Deserialize, Serialize};

use crate::{UsageProvider, UsageTokens};

/// One observation from a provider stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProviderAccountingReport {
    /// A terminal turn result with its usage and running totals.
    Result(ResultAccounting),
    /// A plan rate-limit observation (allowed, warning or rejected).
    RateLimit(RateLimitObservation),
    /// The provider is retrying a failed API request.
    ApiRetry(ApiRetryObservation),
    /// The provider's own readback of the model/effort a run actually uses.
    Runtime(RuntimeReadback),
}

impl ProviderAccountingReport {
    pub fn provider(&self) -> UsageProvider {
        match self {
            Self::Result(report) => report.provider,
            Self::RateLimit(report) => report.provider,
            Self::ApiRetry(report) => report.provider,
            Self::Runtime(report) => report.provider,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultAccounting {
    pub provider: UsageProvider,
    #[serde(default)]
    pub native_session_id: Option<String>,
    pub observed_at_ms: i64,
    /// Provider delivery sequence of this result within its process, if numbered.
    #[serde(default)]
    pub result_index: Option<u64>,
    /// Native origin kind (`human`, `task-notification`, `peer`, `channel`), if reported.
    #[serde(default)]
    pub origin: Option<String>,
    /// The turn's own main-loop usage. Excludes subagents and helper calls.
    #[serde(default)]
    pub turn_tokens: UsageTokens,
    /// Running per-session estimate, including subagents and background work.
    #[serde(default)]
    pub cumulative_cost_usd: Option<f64>,
    /// Running per-model totals for the same native session.
    #[serde(default)]
    pub models: Vec<ModelAccounting>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub duration_api_ms: Option<u64>,
    #[serde(default)]
    pub num_turns: Option<u64>,
}

impl ResultAccounting {
    /// Running token totals across every model of this native session.
    pub fn cumulative_tokens(&self) -> UsageTokens {
        let mut total = UsageTokens::default();
        for model in &self.models {
            total.add(model.tokens);
        }
        total
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAccounting {
    pub model: String,
    #[serde(default)]
    pub canonical_model: Option<String>,
    #[serde(default)]
    pub tokens: UsageTokens,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub web_search_requests: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitObservation {
    pub provider: UsageProvider,
    #[serde(default)]
    pub native_session_id: Option<String>,
    pub observed_at_ms: i64,
    /// `allowed`, `allowed_warning` or `rejected`, as reported.
    pub status: String,
    /// The window currently limiting the account (`five_hour`, `seven_day`, …).
    #[serde(default)]
    pub limiting_window: Option<String>,
    /// Fraction of the limiting window used (provider scale, usually 0–1).
    #[serde(default)]
    pub utilization: Option<f64>,
    #[serde(default)]
    pub resets_at_ms: Option<i64>,
    /// Every window the provider tracked on this observation.
    #[serde(default)]
    pub windows: Vec<RateLimitWindowObservation>,
    #[serde(default)]
    pub overage_status: Option<String>,
    #[serde(default)]
    pub using_overage: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitWindowObservation {
    pub id: String,
    /// Fraction used (provider scale, usually 0–1; may exceed 1).
    pub utilization: f64,
    #[serde(default)]
    pub resets_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiRetryObservation {
    pub provider: UsageProvider,
    #[serde(default)]
    pub native_session_id: Option<String>,
    pub observed_at_ms: i64,
    pub attempt: u32,
    pub max_retries: u32,
    pub retry_delay_ms: u64,
    /// HTTP status; `None` for connection errors without a response.
    #[serde(default)]
    pub error_status: Option<u16>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeReadbackSource {
    /// Read back after the provider process launched.
    Launch,
    /// Read back after a live model/effort change on a warm session.
    LiveSwitch,
}

/// Requested versus provider-applied runtime. `matches` is false when any
/// requested value was not applied; a value LUNA did not request is not compared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeReadback {
    pub provider: UsageProvider,
    #[serde(default)]
    pub native_session_id: Option<String>,
    pub observed_at_ms: i64,
    pub source: RuntimeReadbackSource,
    #[serde(default)]
    pub requested_model: Option<String>,
    #[serde(default)]
    pub effective_model: Option<String>,
    #[serde(default)]
    pub requested_effort: Option<String>,
    #[serde(default)]
    pub effective_effort: Option<String>,
    pub matches: bool,
    /// Why the readback is incomplete or a requested change was not applied.
    #[serde(default)]
    pub detail: Option<String>,
}

/// One live provider session as shown on the shared Usage page. Values come
/// from the provider's own stream; they are already part of the transcript
/// history once scanned and must never be added to history buckets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSessionUsage {
    pub provider: UsageProvider,
    /// LUNA task/chat that owns the run.
    pub task_id: String,
    #[serde(default)]
    pub native_session_id: Option<String>,
    pub updated_at_ms: i64,
    /// Results observed by this app process for the session.
    #[serde(default)]
    pub results: u64,
    /// Running totals from the latest result (never summed across results).
    #[serde(default)]
    pub tokens: UsageTokens,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// The latest result's own main-loop usage.
    #[serde(default)]
    pub last_turn_tokens: UsageTokens,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub runtime: Option<RuntimeReadback>,
    #[serde(default)]
    pub rate_limit: Option<RateLimitObservation>,
    #[serde(default)]
    pub api_retry: Option<ApiRetryObservation>,
}
