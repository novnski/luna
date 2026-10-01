//! CC-06 "Live sessions" section of the Usage page: what running provider
//! sessions reported on their own stream (running cost/tokens per native
//! session, the provider's readback of the model/effort actually applied,
//! plan rate-limit observations and API retries). Values come from the engine
//! snapshot as-is; nothing is summed or estimated here, and the rows are shown
//! beside history, never added to it.

use gpui::{AnyElement, div, prelude::*, px};
use luna_proto::{LiveSessionUsage, RuntimeReadback, UsageTokens};

use super::{compact_tokens, money, usage_table_row};
use crate::theme::Theme;

fn live_money(value: Option<f64>) -> String {
    match value {
        None => "—".into(),
        Some(value) if value < 1.0 => format!("${value:.3}"),
        Some(value) => money(value),
    }
}

fn tokens_or_dash(tokens: UsageTokens) -> String {
    match tokens.total() {
        0 => "—".into(),
        total => compact_tokens(total),
    }
}

fn runtime_label(row: &LiveSessionUsage) -> Option<String> {
    let runtime = row.runtime.as_ref();
    let model = runtime
        .and_then(|r| r.effective_model.clone())
        .or_else(|| row.models.last().cloned())?;
    Some(match runtime.and_then(|r| r.effective_effort.as_deref()) {
        Some(effort) => format!("{model} · {effort}"),
        None => model,
    })
}

fn readback_note(runtime: &RuntimeReadback) -> Option<String> {
    (!runtime.matches).then(|| {
        format!(
            "Runtime readback: {}",
            runtime
                .detail
                .as_deref()
                .unwrap_or("requested settings not applied")
        )
    })
}

fn reset_in(at_ms: i64, now_ms: i64) -> String {
    let minutes = (at_ms - now_ms).max(0) / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("resets in {m}m"),
        (h, m) if h < 48 => format!("resets in {h}h {m}m"),
        (h, _) => format!("resets in {}d", h / 24),
    }
}

/// Status notes under a row, most important first, with a warning flag.
fn notes(row: &LiveSessionUsage, now_ms: i64) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    if let Some(note) = row.runtime.as_ref().and_then(readback_note) {
        out.push((note, true));
    }
    if let Some(rate) = &row.rate_limit
        && rate.status != "allowed"
    {
        let window = rate
            .limiting_window
            .as_deref()
            .map(|id| match id {
                "five_hour" => "5-hour",
                "seven_day" => "weekly",
                other => other,
            })
            .unwrap_or("plan");
        let mut text = match rate.status.as_str() {
            "rejected" => format!("{window} limit reached"),
            _ => format!("{window} limit warning"),
        };
        if let Some(utilization) = rate.utilization {
            text.push_str(&format!(" · {:.0}% used", utilization * 100.0));
        }
        if let Some(at) = rate.resets_at_ms {
            text.push_str(&format!(" · {}", reset_in(at, now_ms)));
        }
        out.push((text, true));
    }
    if let Some(retry) = &row.api_retry
        && now_ms - retry.observed_at_ms < 10 * 60_000
    {
        let status = retry
            .error_status
            .map(|code| format!(" · HTTP {code}"))
            .unwrap_or_default();
        out.push((
            format!("API retry {}/{}{status}", retry.attempt, retry.max_retries),
            false,
        ));
    }
    out
}

/// The section, or `None` when no live provider session reported anything.
pub(super) fn live_sessions_section(
    rows: &[LiveSessionUsage],
    title_of: impl Fn(&str) -> Option<String>,
    limit_note: Option<String>,
    theme: &Theme,
    now_ms: i64,
) -> Option<AnyElement> {
    if rows.is_empty() {
        return None;
    }
    let mut section = div()
        .id("usage-live-sessions")
        .w_full()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .pb(px(6.0))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(13.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child("Live sessions"),
                )
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(
                            "Reported by each running provider session · latest running \
                             totals, not added to the history below",
                        ),
                )
                .when_some(limit_note, |header, note| {
                    header.child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.warning)
                            .child(format!("Claude limits on this page: {note}")),
                    )
                }),
        )
        .child(usage_table_row(
            theme,
            [
                "Session".into(),
                "Cost".into(),
                "Tokens".into(),
                "Last turn".into(),
            ],
            true,
            false,
            None,
        ));
    for row in rows {
        let title = title_of(&row.task_id).unwrap_or_else(|| "Untitled chat".into());
        let name = match runtime_label(row) {
            Some(runtime) => format!("{title} · {runtime}"),
            None => title,
        };
        section = section.child(usage_table_row(
            theme,
            [
                name,
                live_money(row.cost_usd),
                tokens_or_dash(row.tokens),
                tokens_or_dash(row.last_turn_tokens),
            ],
            false,
            row.cost_usd.is_none(),
            Some(row.provider),
        ));
        for (note, warning) in notes(row, now_ms) {
            section = section.child(
                div()
                    .w_full()
                    .pl(px(21.0))
                    .py(px(3.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(if warning {
                        theme.warning
                    } else {
                        theme.text_muted
                    })
                    .child(note),
            );
        }
    }
    Some(section.into_any_element())
}

/// Headless visual QA: open the Usage page on a metric (production fetch).
#[cfg(feature = "appshots-fixture")]
impl super::Shell {
    pub fn fixture_open_usage_page(&mut self, metric: &str, cx: &mut gpui::Context<Self>) {
        self.settings.usage_metric = metric.to_owned();
        self.open_settings(super::SettingsSection::Usage, cx);
    }

    /// The boot splash only fades on a connection transition, which a
    /// pre-connected fixture never makes.
    pub fn fixture_dismiss_splash(&mut self, cx: &mut gpui::Context<Self>) {
        self.splash = super::super::SplashPhase::Gone;
        cx.notify();
    }

    pub fn fixture_usage_page_live_rows(&self) -> usize {
        self.usage_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.live_sessions.len())
    }

    pub fn fixture_usage_page_claude_limit(&self) -> Option<luna_proto::UsageLimitSnapshot> {
        self.usage_snapshot.as_ref().and_then(|snapshot| {
            snapshot
                .limits
                .iter()
                .find(|limit| limit.provider == luna_proto::UsageProvider::Claude)
                .cloned()
        })
    }
}
