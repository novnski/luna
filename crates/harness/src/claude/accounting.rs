//! CC-06 live accounting from Claude's own stream (2.1.286 SDK schema).
//!
//! - `result.usage` is the turn's MAIN-LOOP usage only (schema: "excludes Task
//!   subagent, sidechain, and auxiliary model calls … Prefer modelUsage").
//! - `result.total_cost_usd` / `result.modelUsage` are running totals for the
//!   native session, including subagents, compaction and Workflow agents;
//!   "read the latest result rather than summing". A resumed session's first
//!   result already carries the saved totals.
//! - `rate_limit_event.rate_limit_info` reports status, the limiting window,
//!   utilization (fraction), `resetsAt` (epoch seconds) and `unifiedWindows`.
//! - `system/api_retry` reports a retryable API failure and its backoff.
//!
//! Missing values stay `None`; nothing is estimated here.

use luna_proto::{
    ApiRetryObservation, ModelAccounting, ProviderAccountingReport, RateLimitObservation,
    RateLimitWindowObservation, ResultAccounting, UsageProvider, UsageTokens,
};
use serde_json::Value;

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn u64_at(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn str_at(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn epoch_seconds_ms(value: Option<&Value>) -> Option<i64> {
    let seconds = value?
        .as_i64()
        .or_else(|| value?.as_f64().map(|f| f as i64))?;
    (seconds > 0).then(|| seconds.saturating_mul(1000))
}

/// Accounting for one parsed stdout frame, if it carries any.
pub(crate) fn report_for(frame: &Value) -> Option<ProviderAccountingReport> {
    match frame.get("type").and_then(Value::as_str)? {
        "result" => Some(ProviderAccountingReport::Result(result_accounting(frame))),
        "rate_limit_event" => rate_limit(frame).map(ProviderAccountingReport::RateLimit),
        "system" if frame.get("subtype").and_then(Value::as_str) == Some("api_retry") => {
            Some(ProviderAccountingReport::ApiRetry(api_retry(frame)))
        }
        _ => None,
    }
}

fn result_accounting(frame: &Value) -> ResultAccounting {
    let usage = frame.get("usage").cloned().unwrap_or(Value::Null);
    let turn_tokens = UsageTokens {
        uncached_input: u64_at(&usage, "input_tokens").unwrap_or(0),
        cached_input: u64_at(&usage, "cache_read_input_tokens").unwrap_or(0),
        cache_creation: u64_at(&usage, "cache_creation_input_tokens").unwrap_or(0),
        output: u64_at(&usage, "output_tokens").unwrap_or(0),
        reasoning: 0,
    };
    let mut models: Vec<ModelAccounting> = frame
        .get("modelUsage")
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .map(|(model, entry)| ModelAccounting {
                    model: model.clone(),
                    canonical_model: str_at(entry, "canonicalModel"),
                    tokens: UsageTokens {
                        uncached_input: u64_at(entry, "inputTokens").unwrap_or(0),
                        cached_input: u64_at(entry, "cacheReadInputTokens").unwrap_or(0),
                        cache_creation: u64_at(entry, "cacheCreationInputTokens").unwrap_or(0),
                        output: u64_at(entry, "outputTokens").unwrap_or(0),
                        // Thinking is already inside outputTokens.
                        reasoning: u64_at(entry, "thinkingTokens").unwrap_or(0),
                    },
                    cost_usd: entry.get("costUSD").and_then(Value::as_f64),
                    context_window: u64_at(entry, "contextWindow").filter(|n| *n > 0),
                    max_output_tokens: u64_at(entry, "maxOutputTokens").filter(|n| *n > 0),
                    web_search_requests: u64_at(entry, "webSearchRequests"),
                })
                .collect()
        })
        .unwrap_or_default();
    models.sort_by(|left, right| left.model.cmp(&right.model));
    ResultAccounting {
        provider: UsageProvider::Claude,
        native_session_id: str_at(frame, "session_id"),
        observed_at_ms: now_ms(),
        result_index: u64_at(frame, "result_index"),
        origin: frame
            .get("origin")
            .and_then(|origin| origin.get("kind"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        turn_tokens,
        cumulative_cost_usd: frame
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .filter(|cost| cost.is_finite() && *cost >= 0.0),
        models,
        duration_ms: u64_at(frame, "duration_ms"),
        duration_api_ms: u64_at(frame, "duration_api_ms"),
        num_turns: u64_at(frame, "num_turns"),
    }
}

fn rate_limit(frame: &Value) -> Option<RateLimitObservation> {
    let info = frame.get("rate_limit_info")?;
    let status = str_at(info, "status")?;
    let windows = info
        .get("unifiedWindows")
        .and_then(Value::as_object)
        .map(|windows| {
            windows
                .iter()
                .filter_map(|(id, window)| {
                    Some(RateLimitWindowObservation {
                        id: id.clone(),
                        utilization: window.get("utilization")?.as_f64()?,
                        resets_at_ms: epoch_seconds_ms(window.get("resetsAt")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(RateLimitObservation {
        provider: UsageProvider::Claude,
        native_session_id: str_at(frame, "session_id"),
        observed_at_ms: now_ms(),
        status,
        limiting_window: str_at(info, "rateLimitType"),
        utilization: info.get("utilization").and_then(Value::as_f64),
        resets_at_ms: epoch_seconds_ms(info.get("resetsAt")),
        windows,
        overage_status: str_at(info, "overageStatus"),
        using_overage: info
            .get("isUsingOverage")
            .or_else(|| info.get("overageInUse"))
            .and_then(Value::as_bool),
    })
}

fn api_retry(frame: &Value) -> ApiRetryObservation {
    ApiRetryObservation {
        provider: UsageProvider::Claude,
        native_session_id: str_at(frame, "session_id"),
        observed_at_ms: now_ms(),
        attempt: u64_at(frame, "attempt").unwrap_or(0) as u32,
        max_retries: u64_at(frame, "max_retries").unwrap_or(0) as u32,
        retry_delay_ms: u64_at(frame, "retry_delay_ms").unwrap_or(0),
        error_status: u64_at(frame, "error_status").map(|status| status as u16),
        error: str_at(frame, "error"),
    }
}

/// Relative wording for a reported reset, for the rejection message. Empty
/// when the provider supplied no reset time (never inferred).
pub(crate) fn reset_suffix(resets_at_seconds: Option<i64>) -> String {
    let Some(at_ms) = epoch_seconds_ms(resets_at_seconds.map(Value::from).as_ref()) else {
        return String::new();
    };
    let minutes = (at_ms - now_ms()).max(0) / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, 0) => " It resets in under a minute.".into(),
        (0, m) => format!(" It resets in {m}m."),
        (h, m) => format!(" It resets in {h}h {m}m."),
    }
}
