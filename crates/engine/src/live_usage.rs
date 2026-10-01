//! CC-06 live provider accounting for the shared Usage page.
//!
//! Records `AgentEvent::ProviderAccounting` reports per task and native
//! session. Claude's `total_cost_usd`/`modelUsage` are running totals, so each
//! row holds the LATEST report (never a sum); a resumed native session keeps
//! its row because the first resumed result already carries the saved totals.
//! Rows are shown beside transcript history and are never added to its
//! buckets (the history scan reads the same native transcripts).
//!
//! Plan rate-limit observations from the stream fill the Claude limit windows
//! only when the account probe did not produce a Ready snapshot. They carry no
//! verified account id (so they are never de-duplicated with an account) and
//! say so in their label and detail, because the stream does not name its
//! account.

use std::sync::Mutex;

use luna_proto::{
    LimitAvailability, LiveSessionUsage, ProviderAccountingReport, RateLimitObservation,
    UsageLimitWindow, UsageProvider, UsageSnapshot,
};

/// Rows kept per engine process (newest updates win the slots).
const MAX_ROWS: usize = 32;

#[derive(Default)]
struct LedgerState {
    rows: Vec<LiveSessionUsage>,
    rate_limits: Vec<RateLimitObservation>,
}

#[derive(Default)]
pub struct LiveUsageLedger {
    state: Mutex<LedgerState>,
}

fn row_for<'a>(
    rows: &'a mut Vec<LiveSessionUsage>,
    provider: UsageProvider,
    task_id: &str,
    session: Option<&str>,
    now_ms: i64,
) -> &'a mut LiveSessionUsage {
    let exact = rows.iter().position(|row| {
        row.provider == provider
            && row.task_id == task_id
            && row.native_session_id.as_deref() == session
    });
    // A launch readback can precede the first result that names the session.
    let adopt = || {
        rows.iter().position(|row| {
            row.provider == provider && row.task_id == task_id && row.native_session_id.is_none()
        })
    };
    let index = match exact.or_else(adopt) {
        Some(index) => index,
        None => {
            rows.push(LiveSessionUsage {
                provider,
                task_id: task_id.to_owned(),
                native_session_id: session.map(str::to_owned),
                updated_at_ms: now_ms,
                results: 0,
                tokens: Default::default(),
                cost_usd: None,
                last_turn_tokens: Default::default(),
                models: Vec::new(),
                runtime: None,
                rate_limit: None,
                api_retry: None,
            });
            rows.len() - 1
        }
    };
    let row = &mut rows[index];
    if row.native_session_id.is_none() {
        row.native_session_id = session.map(str::to_owned);
    }
    row
}

/// The task's most recent row, for observations that may omit a session id.
fn latest_for_task<'a>(
    rows: &'a mut Vec<LiveSessionUsage>,
    provider: UsageProvider,
    task_id: &str,
    session: Option<&str>,
    now_ms: i64,
) -> &'a mut LiveSessionUsage {
    if session.is_none()
        && let Some(index) = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.provider == provider && row.task_id == task_id)
            .max_by_key(|(_, row)| row.updated_at_ms)
            .map(|(index, _)| index)
    {
        return &mut rows[index];
    }
    row_for(rows, provider, task_id, session, now_ms)
}

impl LiveUsageLedger {
    pub fn record(&self, task_id: &str, report: &ProviderAccountingReport) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let provider = report.provider();
        match report {
            ProviderAccountingReport::Result(result) => {
                let row = row_for(
                    &mut state.rows,
                    provider,
                    task_id,
                    result.native_session_id.as_deref(),
                    result.observed_at_ms,
                );
                // Latest wins: these are running totals for the session.
                row.results = row.results.saturating_add(1);
                row.updated_at_ms = result.observed_at_ms;
                row.cost_usd = result.cumulative_cost_usd;
                if !result.models.is_empty() {
                    row.tokens = result.cumulative_tokens();
                    row.models = result.models.iter().map(|m| m.model.clone()).collect();
                }
                row.last_turn_tokens = result.turn_tokens;
            }
            ProviderAccountingReport::RateLimit(observation) => {
                state.rate_limits.retain(|prior| prior.provider != provider);
                state.rate_limits.push(observation.clone());
                let row = latest_for_task(
                    &mut state.rows,
                    provider,
                    task_id,
                    observation.native_session_id.as_deref(),
                    observation.observed_at_ms,
                );
                row.rate_limit = Some(observation.clone());
                row.updated_at_ms = row.updated_at_ms.max(observation.observed_at_ms);
            }
            ProviderAccountingReport::ApiRetry(retry) => {
                let row = latest_for_task(
                    &mut state.rows,
                    provider,
                    task_id,
                    retry.native_session_id.as_deref(),
                    retry.observed_at_ms,
                );
                row.api_retry = Some(retry.clone());
                row.updated_at_ms = row.updated_at_ms.max(retry.observed_at_ms);
            }
            ProviderAccountingReport::Runtime(readback) => {
                let row = latest_for_task(
                    &mut state.rows,
                    provider,
                    task_id,
                    readback.native_session_id.as_deref(),
                    readback.observed_at_ms,
                );
                row.runtime = Some(readback.clone());
                row.updated_at_ms = row.updated_at_ms.max(readback.observed_at_ms);
            }
        }
        if state.rows.len() > MAX_ROWS {
            state
                .rows
                .sort_by_key(|row| std::cmp::Reverse(row.updated_at_ms));
            state.rows.truncate(MAX_ROWS);
        }
    }

    /// Rows newest first.
    pub fn sessions(&self) -> Vec<LiveSessionUsage> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        let mut rows = state.rows.clone();
        rows.sort_by_key(|row| std::cmp::Reverse(row.updated_at_ms));
        rows
    }

    pub fn latest_rate_limit(&self, provider: UsageProvider) -> Option<RateLimitObservation> {
        let state = self.state.lock().ok()?;
        state
            .rate_limits
            .iter()
            .find(|observation| observation.provider == provider)
            .cloned()
    }
}

fn window_shape(id: &str) -> (&'static str, Option<u64>) {
    match id {
        "five_hour" => ("5-hour", Some(5 * 60)),
        "seven_day" => ("Weekly", Some(7 * 24 * 60)),
        "seven_day_opus" => ("Weekly (Opus)", Some(7 * 24 * 60)),
        "seven_day_sonnet" => ("Weekly (Sonnet)", Some(7 * 24 * 60)),
        "seven_day_overage_included" => ("Weekly (overage included)", Some(7 * 24 * 60)),
        "overage" => ("Overage", None),
        _ => ("Usage window", None),
    }
}

fn observed_windows(observation: &RateLimitObservation) -> Vec<UsageLimitWindow> {
    let mut windows: Vec<UsageLimitWindow> = observation
        .windows
        .iter()
        .map(|window| {
            let (label, duration) = window_shape(&window.id);
            UsageLimitWindow {
                id: window.id.clone(),
                label: label.into(),
                used_percent: (window.utilization * 100.0) as f32,
                resets_at_ms: window.resets_at_ms,
                duration_minutes: duration,
            }
        })
        .collect();
    // Older CLIs report only the limiting window.
    if windows.is_empty()
        && let (Some(id), Some(utilization)) = (
            observation.limiting_window.as_deref(),
            observation.utilization,
        )
    {
        let (label, duration) = window_shape(id);
        windows.push(UsageLimitWindow {
            id: id.to_owned(),
            label: label.into(),
            used_percent: (utilization * 100.0) as f32,
            resets_at_ms: observation.resets_at_ms,
            duration_minutes: duration,
        });
    }
    windows
}

/// Attach live rows (enabled providers only) and, when the account probe is
/// not Ready, the latest stream-observed Claude windows as unverified values.
pub fn attach(
    snapshot: &mut UsageSnapshot,
    ledger: &LiveUsageLedger,
    enabled: &[UsageProvider],
    overlay_limits: bool,
) {
    snapshot.live_sessions = ledger
        .sessions()
        .into_iter()
        .filter(|row| enabled.contains(&row.provider))
        .collect();
    if !overlay_limits || !enabled.contains(&UsageProvider::Claude) {
        return;
    }
    let Some(observation) = ledger.latest_rate_limit(UsageProvider::Claude) else {
        return;
    };
    let windows = observed_windows(&observation);
    if windows.is_empty() {
        return;
    }
    let Some(limit) = snapshot
        .limits
        .iter_mut()
        .find(|limit| limit.provider == UsageProvider::Claude)
    else {
        return;
    };
    if limit.availability == LimitAvailability::Ready {
        return;
    }
    let probe = limit.detail.take();
    // Real provider-reported values with an explicit observation time; the
    // missing verified id keeps them out of account de-duplication.
    limit.availability = LimitAvailability::Ready;
    limit.verified_account_id = None;
    limit.account_label = Some(luna_proto::STREAM_OBSERVED_LIMIT_LABEL.into());
    limit.windows = windows;
    limit.checked_at_ms = Some(observation.observed_at_ms);
    limit.detail = Some(match probe {
        Some(probe) => format!(
            "Observed in a live Claude session stream ({}) · account not verified · account probe: {probe}",
            observation.status.replace('_', " ")
        ),
        None => format!(
            "Observed in a live Claude session stream ({}) · account not verified",
            observation.status.replace('_', " ")
        ),
    });
}
