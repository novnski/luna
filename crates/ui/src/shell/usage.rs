//! L16 native Usage page and compact sidebar disclosure. Both read Shell's
//! single normalized engine snapshot; no UI-side provider or cost calculation
//! is used as a second source of truth. T3 Code's MIT Usage information
//! hierarchy and BB's documented compact disclosure behavior informed this.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset, Local, TimeZone, Utc};
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use luna_proto::{
    ChatIndicator, CostQuality, HarnessId, LimitAvailability, USAGE_CONTRACT_VERSION,
    USAGE_LIMIT_FRESHNESS_MS, UsageBucket, UsageLimitSnapshot, UsagePriceRate, UsageProvider,
    UsageResetState, UsageSnapshot, UsageSourceStatus,
};
use luna_rpc::methods;

use super::{SettingsSection, Shell};
use crate::icons::{self, icon};
use crate::theme::Theme;

fn provider_mark(provider: UsageProvider) -> &'static str {
    match provider {
        UsageProvider::Codex => icons::OPENAI_MARK,
        UsageProvider::OpenCodeGo => icons::OPENCODE_MARK,
        UsageProvider::Claude => icons::CLAUDE_MARK,
    }
}

fn provider_tone(provider: UsageProvider, theme: &Theme) -> gpui::Hsla {
    match provider {
        UsageProvider::Codex => theme.accent,
        UsageProvider::OpenCodeGo => theme.success,
        UsageProvider::Claude => theme.text_muted,
    }
}

fn metric_cell(theme: &Theme, label: &'static str, value: String) -> AnyElement {
    div()
        .min_w(px(112.0))
        .flex_1()
        .rounded(px(Theme::CONTROL_RADIUS))
        .border_1()
        .border_color(theme.border)
        .bg(theme.glass_hover().opacity(0.18))
        .p(px(9.0))
        .flex()
        .flex_col()
        .gap(px(3.0))
        .child(
            div()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_muted)
                .child(label),
        )
        .child(
            div()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text)
                .child(value),
        )
        .into_any_element()
}

fn count(value: u64) -> String {
    let text = value.to_string();
    let mut grouped = String::new();
    for (index, ch) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    grouped.chars().rev().collect()
}

fn money(value: f64) -> String {
    if value < 0.01 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

fn cost_of(buckets: &[&UsageBucket]) -> Option<f64> {
    buckets.iter().try_fold(0.0, |sum, bucket| {
        bucket.api_equivalent_cost_usd.map(|cost| sum + cost)
    })
}

fn cache_savings_label(buckets: &[&UsageBucket]) -> String {
    let cached = buckets.iter().fold(0_u64, |total, bucket| {
        total.saturating_add(bucket.tokens.cached_input)
    });
    if cached == 0 {
        return "No reuse".into();
    }
    let known: f64 = buckets
        .iter()
        .filter_map(|bucket| bucket.cache_savings_usd)
        .sum();
    if buckets
        .iter()
        .any(|bucket| bucket.cache_savings_usd.is_none())
    {
        if known > 0.0 {
            format!("{} known · partial", money(known))
        } else {
            "Unknown".into()
        }
    } else {
        money(known)
    }
}

fn period_label(start_ms: i64, hourly: bool) -> String {
    let Some(at) = Utc.timestamp_millis_opt(start_ms).single() else {
        return "Unknown time".into();
    };
    let local = at.with_timezone(&Local).fixed_offset();
    if hourly {
        format_local_hour(local)
    } else {
        format!(
            "{} UTC · starts {} local",
            at.format("%b %-d"),
            format_local_hour(local)
        )
    }
}

fn format_local_hour(at: DateTime<FixedOffset>) -> String {
    at.format("%b %-d, %-I %p %:z").to_string()
}

fn period_detail(start_ms: i64, hourly: bool, tokens: u64, cost: Option<f64>) -> String {
    let Some(start) = Utc.timestamp_millis_opt(start_ms).single() else {
        return "Unknown bucket time".into();
    };
    let end = start + chrono::Duration::hours(if hourly { 1 } else { 24 });
    format!(
        "UTC bucket {} to {} · local {} to {} · {} tokens · {}",
        start.format("%Y-%m-%d %H:%M"),
        end.format("%Y-%m-%d %H:%M"),
        format_local_hour(start.with_timezone(&Local).fixed_offset()),
        format_local_hour(end.with_timezone(&Local).fixed_offset()),
        count(tokens),
        cost.map_or("cost unknown".into(), |cost| format!(
            "{} API-equivalent",
            money(cost)
        ))
    )
}

fn quota_pace(
    limit: &UsageLimitSnapshot,
    window: &luna_proto::UsageLimitWindow,
    now_ms: i64,
) -> Option<String> {
    if limit.availability != LimitAvailability::Ready {
        return None;
    }
    let checked = limit.checked_at_ms?;
    let duration_ms = i64::try_from(window.duration_minutes?)
        .ok()?
        .checked_mul(60_000)?;
    let reset = window.resets_at_ms?;
    let start = reset.checked_sub(duration_ms)?;
    if duration_ms <= 0
        || now_ms < checked
        || now_ms.checked_sub(checked)? >= USAGE_LIMIT_FRESHNESS_MS
        || checked < start
        || checked >= reset
        || now_ms >= reset
        || !window.used_percent.is_finite()
        || !(0.0..=100.0).contains(&window.used_percent)
    {
        return None;
    }
    let elapsed = checked.checked_sub(start)? as f64 / duration_ms as f64 * 100.0;
    let delta = window.used_percent as f64 - elapsed;
    Some(format!(
        "Even pace: {:.0}% of window elapsed at check · used {:.0} points {}",
        elapsed,
        delta.abs(),
        if delta > 0.0 { "above" } else { "below" }
    ))
}

fn parse_price_override(fields: [&str; 5]) -> Result<(String, UsagePriceRate), String> {
    let model = fields[0].trim();
    if model.is_empty() || model.len() > 200 {
        return Err("Enter an exact model name (up to 200 characters)".into());
    }
    let mut values = [0.0; 4];
    for (index, value) in values.iter_mut().enumerate() {
        *value = fields[index + 1]
            .trim()
            .parse::<f64>()
            .map_err(|_| "Enter all four prices in USD per million tokens".to_string())?;
    }
    let rate = UsagePriceRate {
        input_per_million: values[0],
        cached_per_million: values[1],
        creation_per_million: values[2],
        output_per_million: values[3],
    };
    if !rate.valid() || values.iter().all(|value| *value == 0.0) {
        return Err("Prices must be finite, nonnegative, at most $1M per million, and include a positive rate".into());
    }
    Ok((model.into(), rate))
}

fn reset_label(state: UsageResetState, now_ms: i64) -> String {
    match state {
        UsageResetState::Unknown => "Reset time unknown".into(),
        UsageResetState::ReportedPassed { .. } => "Reset time passed · refresh to verify".into(),
        UsageResetState::LastReported { at_ms } => Utc
            .timestamp_millis_opt(at_ms)
            .single()
            .map_or("Last reported reset time unavailable".into(), |at| {
                format!("Last reported reset {} UTC", at.format("%b %-d, %H:%M"))
            }),
        UsageResetState::ReportedFuture { at_ms } => {
            let remaining = at_ms.saturating_sub(now_ms) / 60_000;
            if remaining < 60 {
                format!("Resets in {remaining}m")
            } else if remaining < 24 * 60 {
                format!("Resets in {}h {}m", remaining / 60, remaining % 60)
            } else {
                format!(
                    "Resets in {}d {}h",
                    remaining / (24 * 60),
                    (remaining % (24 * 60)) / 60
                )
            }
        }
    }
}

fn limits_status(limit: &UsageLimitSnapshot, now_ms: i64) -> String {
    let state = match limit.availability {
        LimitAvailability::Ready => "",
        LimitAvailability::Unsupported => "Unsupported",
        LimitAvailability::Unverified => "Unverified",
        LimitAvailability::ProbeFailed => "Refresh failed · last good values",
    };
    if !state.is_empty() {
        return state.into();
    }
    if limit.checked_at_ms.is_none() {
        return "Check time unknown".into();
    }
    let Some(age) = limit.checked_age_ms(now_ms) else {
        return "Check time unverified".into();
    };
    let age = age / 60_000;
    if !limit.is_fresh(now_ms) {
        format!("Stale · checked {age}m ago")
    } else {
        format!("Checked {age}m ago")
    }
}

fn limit_window_row(
    limit: &UsageLimitSnapshot,
    window: &luna_proto::UsageLimitWindow,
    theme: &Theme,
    now_ms: i64,
    compact: bool,
) -> AnyElement {
    let remaining = (100.0 - window.used_percent).clamp(0.0, 100.0);
    let tone = if remaining <= 5.0 {
        theme.danger
    } else if remaining <= 20.0 {
        theme.warning
    } else {
        theme.accent
    };
    let fresh = limit.is_fresh(now_ms);
    div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_between()
                .gap(px(6.0))
                .child(
                    div()
                        .min_w(px(100.0))
                        .flex_1()
                        .text_size(crate::typography::ui_rems(if compact {
                            11.0
                        } else {
                            13.0
                        }))
                        .text_color(theme.text)
                        .child(window.label.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(if compact {
                            11.0
                        } else {
                            13.0
                        }))
                        .text_color(tone)
                        .child(format!("{remaining:.0}% remaining")),
                ),
        )
        .child(
            div()
                .h(px(4.0))
                .w_full()
                .rounded(px(2.0))
                .bg(theme.border)
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(remaining / 100.0))
                        .rounded(px(2.0))
                        .bg(tone),
                ),
        )
        .child(
            div()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_muted)
                .child(format!(
                    "{:.0}% used · {}{}",
                    window.used_percent,
                    reset_label(limit.reset_state(window, now_ms), now_ms),
                    if fresh { "" } else { " · unverified now" }
                )),
        )
        .when(!compact, |row| {
            row.child(
                div()
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(theme.text_muted)
                    .child(quota_pace(limit, window, now_ms).unwrap_or_else(|| {
                        "Pace unavailable · quota window is unverified or incomplete".into()
                    })),
            )
        })
        .into_any_element()
}

fn limit_card(limit: &UsageLimitSnapshot, theme: &Theme, now_ms: i64, compact: bool) -> AnyElement {
    let title = match (&limit.account_label, &limit.plan) {
        (Some(account), Some(plan)) => format!("{} · {} · {plan}", limit.provider.label(), account),
        (Some(account), None) => format!("{} · {account}", limit.provider.label()),
        _ => limit.provider.label().to_owned(),
    };
    div()
        .w_full()
        .rounded(px(Theme::CONTROL_RADIUS))
        .border_1()
        .border_color(theme.border)
        .bg(theme.glass_hover().opacity(0.35))
        .p(px(if compact { 10.0 } else { 14.0 }))
        .flex()
        .flex_col()
        .gap(px(9.0))
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_between()
                .gap(px(6.0))
                .items_center()
                .child(
                    div()
                        .min_w(px(110.0))
                        .flex_1()
                        .text_size(crate::typography::ui_rems(if compact {
                            12.0
                        } else {
                            15.0
                        }))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(title),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(10.0))
                        .text_color(if limit.availability == LimitAvailability::ProbeFailed {
                            theme.warning
                        } else {
                            theme.text_muted
                        })
                        .child(limits_status(limit, now_ms)),
                ),
        )
        .when_some(limit.detail.clone(), |card, detail| {
            card.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(detail),
            )
        })
        .children(
            limit
                .windows
                .iter()
                .map(|window| limit_window_row(limit, window, theme, now_ms, compact)),
        )
        .when_some(
            if limit.provider == UsageProvider::Codex {
                limit.reset_credits
            } else {
                None
            },
            |card, credits| {
                card.child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(format!(
                            "Reset credits: {credits} · redemption unavailable in this build"
                        )),
                )
            },
        )
        .into_any_element()
}

fn compact_limit_status(limit: &UsageLimitSnapshot, now_ms: i64) -> Option<&'static str> {
    match limit.availability {
        LimitAvailability::Ready if limit.checked_at_ms.is_none() => Some("Check time unknown"),
        LimitAvailability::Ready if limit.checked_age_ms(now_ms).is_none() => {
            Some("Check time unverified")
        }
        LimitAvailability::Ready if !limit.is_fresh(now_ms) => Some("Stale · refresh to verify"),
        LimitAvailability::Ready => None,
        LimitAvailability::ProbeFailed if limit.windows.is_empty() => {
            Some("Refresh failed · quota unavailable")
        }
        LimitAvailability::ProbeFailed => Some("Refresh failed · last good values"),
        LimitAvailability::Unsupported => Some("Quota unsupported"),
        LimitAvailability::Unverified => Some("Quota unverified"),
    }
}

fn compact_reset_time(window: &luna_proto::UsageLimitWindow, now_ms: i64) -> String {
    let Some(reset_ms) = window.resets_at_ms else {
        return "—".into();
    };
    let minutes = reset_ms.saturating_sub(now_ms).div_euclid(60_000);
    if minutes <= 0 {
        "0m".into()
    } else if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 24 * 60 {
        if minutes % 60 == 0 {
            format!("{}h", minutes / 60)
        } else {
            format!("{}h {}m", minutes / 60, minutes % 60)
        }
    } else {
        let days = minutes / (24 * 60);
        let hours = (minutes % (24 * 60)) / 60;
        if hours == 0 {
            format!("{days}d")
        } else {
            format!("{days}d {hours}h")
        }
    }
}

fn compact_window_label(
    limit: &UsageLimitSnapshot,
    window: &luna_proto::UsageLimitWindow,
) -> String {
    if limit.provider == UsageProvider::Codex {
        match window.duration_minutes {
            Some(300) => return "5h".into(),
            Some(10_080) => return "7d".into(),
            _ => {}
        }
    }
    window.label.clone()
}

fn compact_plan_label(limit: &UsageLimitSnapshot, plan: &str) -> String {
    if limit.provider == UsageProvider::Codex && plan.eq_ignore_ascii_case("pro") {
        "Pro".into()
    } else {
        plan.into()
    }
}

fn compact_limit_row(limit: &UsageLimitSnapshot, now_ms: i64) -> AnyElement {
    let mut row = div()
        .w_full()
        .min_w_0()
        .px(px(10.0))
        .py(px(7.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(5.0))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(12.0))
                        .line_height(px(15.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(gpui::rgb(0xd1d1d3))
                        .child(match limit.provider {
                            UsageProvider::OpenCodeGo => "opencode",
                            provider => provider.label(),
                        }),
                )
                .when_some(limit.plan.clone(), |el, plan| {
                    el.child(
                        div()
                            .flex_none()
                            .px(px(5.0))
                            .py(px(1.0))
                            .rounded(px(5.0))
                            .bg(gpui::rgb(0x303030))
                            .text_size(crate::typography::ui_rems(10.0))
                            .line_height(px(12.0))
                            .text_color(gpui::rgb(0xa8a8ab))
                            .child(compact_plan_label(limit, &plan)),
                    )
                }),
        );
    if let Some(account) = limit.account_label.as_deref().filter(|s| !s.is_empty()) {
        row = row.child(
            div()
                .min_w_0()
                .truncate()
                .text_size(crate::typography::ui_rems(10.0))
                .line_height(px(14.0))
                .text_color(gpui::rgb(0xa7a7aa))
                .child(account.to_owned()),
        );
    }
    if limit.windows.is_empty() {
        row = row.child(
            div()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(gpui::rgb(0xa7a7aa))
                .child("Quota unavailable"),
        );
    }
    for window in &limit.windows {
        let used = window.used_percent.clamp(0.0, 100.0);
        let (label_width, reset_width) = if limit.provider == UsageProvider::Codex {
            (24.0, 45.0)
        } else {
            (50.0, 43.0)
        };
        let fill = if limit.provider == UsageProvider::Codex {
            gpui::rgb(0xd34351)
        } else {
            gpui::rgb(0xc9c9ca)
        };
        row = row.child(
            div()
                .mt(px(3.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(10.0))
                .line_height(px(14.0))
                .text_color(gpui::rgb(0xb5b5b8))
                .child(
                    div()
                        .w(px(label_width))
                        .flex_none()
                        .truncate()
                        .child(compact_window_label(limit, window)),
                )
                .child(
                    div()
                        .h(px(4.0))
                        .flex_1()
                        .min_w_0()
                        .rounded(px(2.0))
                        .bg(gpui::rgb(0x393939))
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative((used / 100.0).max(0.012)))
                                .rounded(px(2.0))
                                .bg(fill),
                        ),
                )
                .child(
                    div()
                        .w(px(26.0))
                        .flex_none()
                        .text_align(gpui::TextAlign::Right)
                        .child(format!("{used:.0}%")),
                )
                .child(
                    div()
                        .w(px(reset_width))
                        .flex_none()
                        .text_align(gpui::TextAlign::Right)
                        .child(if limit.is_fresh(now_ms) {
                            compact_reset_time(window, now_ms)
                        } else {
                            "—".into()
                        }),
                ),
        );
    }
    if let Some(status) = compact_limit_status(limit, now_ms) {
        row = row.child(
            div()
                .truncate()
                .text_size(crate::typography::ui_rems(9.0))
                .text_color(gpui::rgb(0xffce46))
                .child(status),
        );
    }
    row.into_any_element()
}

impl Shell {
    fn save_usage_price_override(&mut self, cx: &mut Context<Self>) {
        let fields: Vec<String> = self
            .usage_price_inputs
            .iter()
            .map(|input| input.read(cx).text().to_string())
            .collect();
        match parse_price_override([&fields[0], &fields[1], &fields[2], &fields[3], &fields[4]]) {
            Ok((model, rate)) => {
                self.settings.usage_price_overrides.insert(model, rate);
                self.usage_price_error = None;
                self.schedule_save(cx);
                self.refresh_usage(cx);
            }
            Err(error) => {
                self.usage_price_error = Some(error);
                cx.notify();
            }
        }
    }

    fn remove_usage_price_override(&mut self, model: &str, cx: &mut Context<Self>) {
        self.settings.usage_price_overrides.remove(model);
        self.usage_price_error = None;
        self.schedule_save(cx);
        self.refresh_usage(cx);
    }

    fn render_usage_price_editor(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut saved: Vec<_> = self.settings.usage_price_overrides.iter().collect();
        saved.sort_by(|left, right| left.0.cmp(right.0));
        let [
            model_input,
            input_rate,
            cached_rate,
            creation_rate,
            output_rate,
        ] = self.usage_price_inputs.clone();
        let mut card = div()
            .w_full()
            .rounded(px(Theme::CONTROL_RADIUS))
            .border_1()
            .border_color(theme.border)
            .bg(theme.glass_hover().opacity(0.35))
            .p(px(14.0))
            .flex()
            .flex_col()
            .gap(px(9.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(14.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child("Exact-model price overrides"),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child("Your USD per million token rates estimate API-equivalent cost for an exact model name. Provider-reported costs take precedence; unknown models stay unpriced."),
            )
            .child(
                div()
                    .w_full()
                    .child(crate::popover::dialog_field(
                        model_input.into_any_element(),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(7.0))
                    .children(
                        [input_rate, cached_rate, creation_rate, output_rate]
                            .into_iter()
                            .map(|input| {
                                div()
                                    .w(px(160.0))
                                    .child(crate::popover::dialog_field(input.into_any_element()))
                            }),
                    ),
            )
            .child(
                div()
                    .id("usage-price-save")
                    .cursor_pointer()
                    .text_color(theme.accent)
                    .text_size(crate::typography::ui_rems(11.0))
                    .on_click(cx.listener(|this, _, _, cx| this.save_usage_price_override(cx)))
                    .child("Save exact-model rate"),
            );
        if let Some(error) = &self.usage_price_error {
            card = card.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.warning)
                    .child(error.clone()),
            );
        }
        for (index, (model, rate)) in saved.into_iter().enumerate() {
            let model = model.clone();
            card = card.child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_between()
                    .gap(px(8.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(format!(
                        "{model} · input ${} · cached ${} · creation ${} · output ${} / 1M",
                        rate.input_per_million,
                        rate.cached_per_million,
                        rate.creation_per_million,
                        rate.output_per_million
                    ))
                    .child(
                        div()
                            .id(SharedString::from(format!("usage-price-remove-{index}")))
                            .cursor_pointer()
                            .text_color(theme.warning)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_usage_price_override(&model, cx)
                            }))
                            .child("Remove"),
                    ),
            );
        }
        card.into_any_element()
    }

    pub(super) fn toggle_usage_card(&mut self, cx: &mut Context<Self>) {
        self.usage_card_open = !self.usage_card_open;
        if self.usage_card_open {
            if self.usage_environment_filter.is_none() {
                self.usage_environment_filter = self
                    .state
                    .read(cx)
                    .selected_chat
                    .as_ref()
                    .and_then(|id| self.state.read(cx).chats.iter().find(|chat| &chat.id == id))
                    .map(|chat| chat.device_id.clone());
            }
            self.refresh_usage(cx);
        }
        cx.notify();
    }

    fn render_usage_provider_tabs(
        &mut self,
        theme: &Theme,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let visible = self.selected_usage_snapshot();
        div()
            .flex()
            .flex_wrap()
            .gap(px(5.0))
            .children(
                [
                    (None, "All providers"),
                    (Some(UsageProvider::Codex), "Codex"),
                    (Some(UsageProvider::OpenCodeGo), "OpenCode Go"),
                    (Some(UsageProvider::Claude), "Claude"),
                ]
                .into_iter()
                .filter(|(provider, _)| {
                    provider.is_none_or(|provider| {
                        visible.as_ref().is_some_and(|snapshot| {
                            snapshot
                                .summary
                                .sources
                                .iter()
                                .any(|source| source.provider == provider)
                                || snapshot
                                    .limits
                                    .iter()
                                    .any(|limit| limit.provider == provider)
                        })
                    })
                })
                .map(|(provider, label)| {
                    let label = if compact && provider.is_none() {
                        "All"
                    } else if compact && provider == Some(UsageProvider::OpenCodeGo) {
                        "Go"
                    } else {
                        label
                    };
                    div()
                        .id(SharedString::from(format!("usage-provider-{label}")))
                        .px(px(if compact { 6.0 } else { 8.0 }))
                        .py(px(if compact { 4.0 } else { 5.0 }))
                        .rounded(px(Theme::CONTROL_RADIUS))
                        .bg(if self.usage_provider_filter == provider {
                            theme.accent_wash
                        } else {
                            theme.glass_hover().opacity(0.1)
                        })
                        .text_color(if self.usage_provider_filter == provider {
                            theme.text
                        } else {
                            theme.text_muted
                        })
                        .text_size(crate::typography::ui_rems(if compact {
                            10.0
                        } else {
                            11.0
                        }))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.usage_provider_filter = provider;
                            cx.notify();
                        }))
                        .child(label)
                }),
            )
            .into_any_element()
    }

    fn selected_usage_snapshot(&self) -> Option<UsageSnapshot> {
        let Some(filter) = &self.usage_environment_filter else {
            return self.usage_snapshot.clone();
        };
        self.usage_environment_snapshots
            .iter()
            .find(|(device, _)| device == filter)
            .map(|(_, snapshot)| snapshot.clone())
    }

    fn cycle_usage_environment(&mut self, cx: &mut Context<Self>) {
        let environments: Vec<_> = self
            .usage_environment_snapshots
            .iter()
            .map(|(device, _)| device.clone())
            .collect();
        let next = match &self.usage_environment_filter {
            None => environments.first().cloned(),
            Some(current) => environments
                .iter()
                .position(|id| id == current)
                .and_then(|index| environments.get(index + 1).cloned()),
        };
        self.usage_environment_filter = next;
        cx.notify();
    }

    fn can_show_usage_alert(&self) -> bool {
        !self.settings.usage_sidebar_hidden
            && !self.settings.sidebar_collapsed
            && matches!(&self.route, super::Route::Chat)
    }

    fn working_usage_targets(&self, cx: &Context<Self>) -> Vec<(String, UsageProvider)> {
        let state = self.state.read(cx);
        let now = Utc::now();
        state
            .visible_chats()
            .filter(|chat| state.display_status_for(chat, now) == ChatIndicator::Working)
            .filter_map(|chat| {
                let provider = match chat.config.as_ref()?.harness {
                    HarnessId::Codex => UsageProvider::Codex,
                    HarnessId::Opencode => UsageProvider::OpenCodeGo,
                    HarnessId::ClaudeCode => UsageProvider::Claude,
                    _ => return None,
                };
                Some((chat.device_id.clone(), provider))
            })
            .collect()
    }

    fn maybe_open_low_usage_card(&mut self, cx: &mut Context<Self>) {
        let active = self.working_usage_targets(cx);
        let can_show = self.can_show_usage_alert();
        let now_ms = Utc::now().timestamp_millis();
        let mut alert = None;
        for (device, snapshot) in &self.usage_environment_snapshots {
            for limit in snapshot
                .limits
                .iter()
                .filter(|limit| limit.is_fresh(now_ms))
            {
                let key = (device.clone(), limit.provider);
                let low = limit.windows.iter().any(|window| {
                    window.used_percent.is_finite()
                        && window.used_percent > 95.0
                        && window.used_percent <= 100.0
                        && window.resets_at_ms.is_none_or(|reset| reset > now_ms)
                });
                if !low {
                    self.usage_alerted.retain(|alerted| alerted != &key);
                } else if can_show && active.contains(&key) && !self.usage_alerted.contains(&key) {
                    self.usage_alerted.push(key.clone());
                    if alert.is_none() {
                        alert = Some(key);
                    }
                }
            }
        }
        if let Some((device, provider)) = alert
            && !self.usage_card_open
        {
            self.usage_card_open = true;
            self.usage_environment_filter = Some(device);
            self.usage_provider_filter = Some(provider);
            cx.notify();
        }
    }

    pub(super) fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.usage_error = Some("Engine not connected".into());
            self.usage_refreshing = false;
            cx.notify();
            return;
        };
        self.usage_last_refresh_attempt_ms = Some(Utc::now().timestamp_millis());
        self.usage_request_epoch += 1;
        let epoch = self.usage_request_epoch;
        let engine_generation = self.state.read(cx).engine_generation();
        let local = engine.engine_info().device_id.clone();
        let mut targets = vec![local];
        for device in &self.state.read(cx).devices {
            if !targets.contains(&device.id) && targets.len() < 8 {
                targets.push(device.id.clone());
            }
        }
        let days = self.settings.usage_days;
        let price_overrides = self.settings.usage_price_overrides.clone();
        self.usage_refreshing = true;
        self.usage_error = None;
        // RpcClient and tokio::time both require the Tokio reactor. A GPUI
        // task may await this bridge, but must not poll the RPC fan-out itself.
        // Dropping usage_task drops `fetch`, which aborts the Tokio task.
        let fetch = gpui_tokio::Tokio::spawn(cx, async move {
            futures::future::join_all(targets.into_iter().map(|device| {
                let engine = engine.clone();
                let price_overrides = price_overrides.clone();
                async move {
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(8),
                        engine.client().call(
                            methods::GET_USAGE_SNAPSHOT,
                            serde_json::json!({"days": days, "targetDeviceId": device, "priceOverrides": price_overrides}),
                        ),
                    )
                    .await;
                    (device, result)
                }
            }))
            .await
        });
        self.usage_task = Some(cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |shell, cx| {
                if shell.usage_request_epoch != epoch
                    || shell.state.read(cx).engine_generation() != engine_generation
                {
                    return;
                }
                shell.usage_refreshing = false;
                let Ok(results) = result else {
                    shell.usage_error =
                        Some("Usage refresh stopped; showing last good data".into());
                    cx.notify();
                    return;
                };
                let mut snapshots = Vec::new();
                let mut failures = 0;
                for (device, result) in results {
                    match result {
                        Ok(Ok(value)) => match serde_json::from_value::<UsageSnapshot>(value) {
                            Ok(snapshot)
                                if snapshot.summary.contract_version == USAGE_CONTRACT_VERSION =>
                            {
                                snapshots.push((device, snapshot))
                            }
                            _ => failures += 1,
                        },
                        _ => failures += 1,
                    }
                }
                if let Some(mut merged) = luna_proto::merge_usage_snapshots(
                    snapshots
                        .iter()
                        .map(|(_, snapshot)| snapshot.clone())
                        .collect(),
                ) {
                    if failures > 0 {
                        merged.summary.partial = true;
                        merged
                            .summary
                            .warnings
                            .push(format!("{failures} environment usage requests failed"));
                    }
                    shell.usage_environment_snapshots = snapshots;
                    shell.usage_snapshot = Some(merged);
                    if shell.usage_provider_filter.is_some_and(|provider| {
                        shell.selected_usage_snapshot().is_none_or(|snapshot| {
                            !snapshot
                                .summary
                                .sources
                                .iter()
                                .any(|source| source.provider == provider)
                                && !snapshot
                                    .limits
                                    .iter()
                                    .any(|limit| limit.provider == provider)
                        })
                    }) {
                        shell.usage_provider_filter = None;
                    }
                    if failures > 0 {
                        shell.usage_error = Some(format!(
                            "{failures} environments unavailable; showing partial usage"
                        ));
                    }
                    shell.maybe_open_low_usage_card(cx);
                } else {
                    shell.usage_error = Some("Usage refresh failed; showing last good data".into());
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn maybe_auto_refresh_usage(&mut self, cx: &mut Context<Self>) {
        self.maybe_open_low_usage_card(cx);
        let visible = (!self.settings.usage_sidebar_hidden && self.usage_card_open)
            || matches!(&self.route, super::Route::Settings(SettingsSection::Usage));
        let watching = self.can_show_usage_alert() && !self.working_usage_targets(cx).is_empty();
        if (!visible && !watching)
            || self.usage_refreshing
            || self.state.read(cx).engine().is_none()
        {
            return;
        }
        if self.usage_card_open
            && self.usage_environment_filter.is_none()
            && self.usage_last_refresh_attempt_ms.is_none()
        {
            let state = self.state.read(cx);
            self.usage_environment_filter = state
                .selected_chat
                .as_ref()
                .and_then(|id| state.chats.iter().find(|chat| &chat.id == id))
                .map(|chat| chat.device_id.clone())
                .or_else(|| {
                    state
                        .engine()
                        .map(|engine| engine.engine_info().device_id.clone())
                });
        }
        let now_ms = Utc::now().timestamp_millis();
        if self
            .usage_last_refresh_attempt_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= USAGE_LIMIT_FRESHNESS_MS)
        {
            self.refresh_usage(cx);
        }
    }

    fn set_usage_metric(&mut self, metric: &'static str, cx: &mut Context<Self>) {
        self.settings.usage_metric = metric.into();
        self.schedule_save(cx);
        if metric == "limits" {
            self.refresh_usage(cx);
        }
        cx.notify();
    }

    fn set_usage_days(&mut self, days: u32, cx: &mut Context<Self>) {
        self.settings.usage_days = days;
        self.schedule_save(cx);
        self.refresh_usage(cx);
    }

    fn compact_usage_environment_label(&self, cx: &Context<Self>) -> String {
        let Some(id) = self.usage_environment_filter.as_deref() else {
            return "All machines".into();
        };
        let state = self.state.read(cx);
        if let Some(name) = state
            .devices
            .iter()
            .find(|device| device.id == id)
            .map(|device| device.name.trim())
            .filter(|name| !name.is_empty() && uuid::Uuid::parse_str(name).is_err())
        {
            return name.to_owned();
        }
        if state
            .engine()
            .is_some_and(|engine| engine.engine_info().device_id == id)
        {
            "This machine".into()
        } else {
            "Selected machine".into()
        }
    }

    pub(super) fn render_usage_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let environment_label = self.compact_usage_environment_label(cx);
        let now_ms = Utc::now().timestamp_millis();
        let metric = self.settings.usage_metric.clone();
        let limits_view = metric == "limits";
        let provider_tabs = self.render_usage_provider_tabs(&theme, false, cx);
        let snapshot = self.selected_usage_snapshot();
        let stale = snapshot
            .as_ref()
            .is_some_and(|snapshot| now_ms - snapshot.summary.read_at_ms >= 5 * 60_000);
        let headline = snapshot
            .as_ref()
            .map(|snapshot| {
                let ready = snapshot.summary.sources.iter().any(|source| {
                    matches!(
                        source.status,
                        UsageSourceStatus::Ready | UsageSourceStatus::Partial
                    ) && self
                        .usage_provider_filter
                        .is_none_or(|provider| provider == source.provider)
                });
                let selected: Vec<_> = snapshot
                    .summary
                    .buckets
                    .iter()
                    .filter(|bucket| {
                        self.usage_provider_filter
                            .is_none_or(|provider| bucket.provider == provider)
                    })
                    .collect();
                let tokens = selected
                    .iter()
                    .map(|bucket| bucket.tokens.total())
                    .sum::<u64>();
                if self.usage_provider_filter == Some(UsageProvider::OpenCodeGo) {
                    "OpenCode Go history unavailable".into()
                } else if !ready {
                    "Historical usage unavailable".into()
                } else if metric == "tokens" {
                    format!("{} tokens", count(tokens))
                } else if selected.is_empty() {
                    "No usage in this window".into()
                } else if let Some(cost) = cost_of(&selected) {
                    format!("{} API-equivalent estimate", money(cost))
                } else {
                    let known: f64 = selected
                        .iter()
                        .filter_map(|bucket| bucket.api_equivalent_cost_usd)
                        .sum();
                    let unknown = selected
                        .iter()
                        .filter(|bucket| bucket.api_equivalent_cost_usd.is_none())
                        .map(|bucket| bucket.records)
                        .sum::<u64>();
                    format!(
                        "{} known API-equivalent · {} unpriced records",
                        money(known),
                        count(unknown)
                    )
                }
            })
            .unwrap_or_else(|| "Usage unavailable".into());
        let toolbar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .children(
                [("cost", "Cost"), ("tokens", "Tokens"), ("limits", "Limits")]
                    .into_iter()
                    .map(|(key, label)| {
                        div()
                            .id(SharedString::from(format!("usage-metric-{key}")))
                            .rounded(px(Theme::CONTROL_RADIUS))
                            .px(px(12.0))
                            .py(px(7.0))
                            .bg(if metric == key {
                                theme.accent_wash
                            } else {
                                theme.glass_hover().opacity(0.2)
                            })
                            .text_color(if metric == key {
                                theme.text
                            } else {
                                theme.text_muted
                            })
                            .text_size(crate::typography::ui_rems(12.0))
                            .cursor_pointer()
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_usage_metric(key, cx)),
                            )
                            .child(label)
                    }),
            );
        let periods = div().flex().flex_wrap().gap(px(5.0)).children(
            [
                (1, "Past 24h"),
                (7, "7 days"),
                (30, "30 days"),
                (90, "90 days"),
            ]
            .into_iter()
            .map(|(days, label)| {
                div()
                    .id(SharedString::from(format!("usage-days-{days}")))
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .px(px(9.0))
                    .py(px(6.0))
                    .bg(if self.settings.usage_days == days {
                        theme.glass_hover()
                    } else {
                        theme.glass_hover().opacity(0.1)
                    })
                    .text_color(if limits_view {
                        theme.text_faint
                    } else {
                        theme.text_muted
                    })
                    .text_size(crate::typography::ui_rems(11.0))
                    .when(!limits_view, |el| {
                        el.cursor_pointer().on_click(
                            cx.listener(move |this, _, _, cx| this.set_usage_days(days, cx)),
                        )
                    })
                    .child(label)
            }),
        );
        let mut content = div().w_full().flex().flex_col().gap(px(14.0));
        if let Some(snapshot) = snapshot {
            if limits_view {
                content = content
                    .child(div().text_size(crate::typography::ui_rems(17.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child("Subscription limits"))
                    .child(div().text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child("Account allowance and reset windows are separate from historical tokens and API-equivalent cost. Local OpenCode Go reports subscription windows; external servers do not expose their credentials here."));
                for provider in [
                    UsageProvider::Codex,
                    UsageProvider::OpenCodeGo,
                    UsageProvider::Claude,
                ] {
                    if self
                        .usage_provider_filter
                        .is_some_and(|filter| filter != provider)
                    {
                        continue;
                    }
                    for limit in snapshot
                        .limits
                        .iter()
                        .filter(|limit| limit.provider == provider)
                    {
                        content = content.child(limit_card(limit, &theme, now_ms, false));
                    }
                }
            } else {
                let selected: Vec<_> = snapshot
                    .summary
                    .buckets
                    .iter()
                    .filter(|bucket| {
                        self.usage_provider_filter
                            .is_none_or(|provider| bucket.provider == provider)
                    })
                    .collect();
                let sessions: u64 = snapshot
                    .summary
                    .sources
                    .iter()
                    .filter(|source| {
                        matches!(
                            source.status,
                            UsageSourceStatus::Ready | UsageSourceStatus::Partial
                        ) && self
                            .usage_provider_filter
                            .is_none_or(|provider| source.provider == provider)
                    })
                    .map(|source| source.distinct_sessions)
                    .sum();
                content = content.child(
                    div()
                        .text_size(crate::typography::ui_rems(23.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(headline),
                );
                content = content.child(
                    div()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child(format!(
                            "{} sessions · {} records · UTC {} buckets",
                            count(sessions),
                            count(selected.iter().map(|bucket| bucket.records).sum()),
                            snapshot.summary.resolution
                        )),
                );
                if snapshot.summary.partial {
                    content = content.child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.warning)
                            .child("Partial coverage · totals may be incomplete"),
                    );
                }
                if selected.is_empty() {
                    content = content.child(div()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child(if self.usage_provider_filter == Some(UsageProvider::OpenCodeGo) {
                            "OpenCode Go historical tokens and Zen spending are not in the T3/BB history adapters. Its local subscription windows appear under Limits when the selected machine reports them."
                        } else {
                            "No historical buckets are available for this provider and window."
                        }));
                } else {
                    let mut mix = luna_proto::UsageTokens::default();
                    for bucket in &selected {
                        mix.add(bucket.tokens);
                    }
                    let all_tokens = selected
                        .iter()
                        .map(|bucket| bucket.tokens.total())
                        .sum::<u64>();
                    let all_cost = cost_of(&selected);
                    for provider in [
                        UsageProvider::Codex,
                        UsageProvider::OpenCodeGo,
                        UsageProvider::Claude,
                    ] {
                        let buckets: Vec<_> = selected
                            .iter()
                            .copied()
                            .filter(|bucket| bucket.provider == provider)
                            .collect();
                        if buckets.is_empty() {
                            continue;
                        }
                        let tokens = buckets
                            .iter()
                            .map(|bucket| bucket.tokens.total())
                            .sum::<u64>();
                        let value = if metric == "tokens" {
                            format!("{} tokens", count(tokens))
                        } else {
                            cost_of(&buckets)
                                .map_or("Cost unknown · unpriced records".into(), money)
                        };
                        let share = if metric == "tokens" && all_tokens > 0 {
                            Some(tokens as f64 / all_tokens as f64)
                        } else if metric == "cost" {
                            all_cost
                                .filter(|total| *total > 0.0)
                                .and_then(|total| cost_of(&buckets).map(|cost| cost / total))
                        } else {
                            None
                        };
                        let provider_sessions: u64 = snapshot
                            .summary
                            .sources
                            .iter()
                            .filter(|source| {
                                source.provider == provider
                                    && matches!(
                                        source.status,
                                        UsageSourceStatus::Ready | UsageSourceStatus::Partial
                                    )
                            })
                            .map(|source| source.distinct_sessions)
                            .sum();
                        content = content.child(
                            div()
                                .w_full()
                                .rounded(px(Theme::CONTROL_RADIUS))
                                .border_1()
                                .border_color(theme.border)
                                .p(px(12.0))
                                .flex()
                                .flex_wrap()
                                .justify_between()
                                .gap(px(7.0))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .child(
                                            icon(provider_mark(provider))
                                                .size(px(15.0))
                                                .text_color(provider_tone(provider, &theme)),
                                        )
                                        .child(div().text_color(theme.text).child(format!(
                                            "{} · {} sessions · {}",
                                            provider.label(),
                                            count(provider_sessions),
                                            share.map_or("share unknown".into(), |share| format!(
                                                "{:.0}% share",
                                                share * 100.0
                                            ))
                                        ))),
                                )
                                .child(div().text_color(theme.text_muted).child(value)),
                        );
                    }
                    let mut periods = BTreeMap::<i64, Vec<&UsageBucket>>::new();
                    for bucket in &selected {
                        periods.entry(bucket.start_ms).or_default().push(bucket);
                    }
                    let period_value = |items: &Vec<&UsageBucket>| -> f64 {
                        if metric == "tokens" {
                            items
                                .iter()
                                .map(|bucket| bucket.tokens.total() as f64)
                                .sum()
                        } else {
                            cost_of(items).unwrap_or(0.0)
                        }
                    };
                    let max_value = periods
                        .values()
                        .map(period_value)
                        .fold(0.0_f64, f64::max)
                        .max(0.000_001);
                    content = content.child(
                        div()
                            .text_size(crate::typography::ui_rems(14.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("By time"),
                    );
                    for (start, buckets) in periods.into_iter().rev() {
                        let tokens = buckets
                            .iter()
                            .map(|bucket| bucket.tokens.total())
                            .sum::<u64>();
                        let label = if metric == "tokens" {
                            format!("{} tokens", count(tokens))
                        } else {
                            cost_of(&buckets).map_or("Unknown cost".into(), money)
                        };
                        let cost_known = metric == "tokens" || cost_of(&buckets).is_some();
                        let detail = period_detail(
                            start,
                            snapshot.summary.days == 1,
                            tokens,
                            cost_of(&buckets),
                        );
                        let segments: Vec<AnyElement> = [
                            UsageProvider::Codex,
                            UsageProvider::OpenCodeGo,
                            UsageProvider::Claude,
                        ]
                        .into_iter()
                        .filter_map(|provider| {
                            let amount: f64 = buckets
                                .iter()
                                .filter(|bucket| bucket.provider == provider)
                                .map(|bucket| {
                                    if metric == "tokens" {
                                        bucket.tokens.total() as f64
                                    } else {
                                        bucket.api_equivalent_cost_usd.unwrap_or(0.0)
                                    }
                                })
                                .sum();
                            (cost_known && amount > 0.0).then(|| {
                                div()
                                    .h_full()
                                    .w(gpui::relative((amount / max_value) as f32))
                                    .bg(provider_tone(provider, &theme))
                                    .into_any_element()
                            })
                        })
                        .collect();
                        content = content.child(
                            div()
                                .id(SharedString::from(format!("usage-period-{start}")))
                                .tooltip(move |_, cx| {
                                    cx.new(|_| super::SurfaceTabTooltip {
                                        text: detail.clone().into(),
                                    })
                                    .into()
                                })
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .justify_between()
                                        .gap(px(5.0))
                                        .text_size(crate::typography::ui_rems(11.0))
                                        .text_color(theme.text_muted)
                                        .child(period_label(start, snapshot.summary.days == 1))
                                        .child(label),
                                )
                                .child(
                                    div()
                                        .h(px(5.0))
                                        .w_full()
                                        .rounded(px(3.0))
                                        .bg(theme.border)
                                        .flex()
                                        .overflow_hidden()
                                        .children(segments),
                                ),
                        );
                    }
                    let processed = mix
                        .uncached_input
                        .saturating_add(mix.cached_input)
                        .saturating_add(mix.cache_creation);
                    content = content.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(6.0))
                            .child(metric_cell(&theme, "Input processed", count(processed)))
                            .child(metric_cell(&theme, "Cache reused", count(mix.cached_input)))
                            .child(metric_cell(
                                &theme,
                                "Cache created",
                                count(mix.cache_creation),
                            ))
                            .child(metric_cell(&theme, "Output", count(mix.output)))
                            .child(metric_cell(
                                &theme,
                                "Cache savings",
                                cache_savings_label(&selected),
                            )),
                    );
                    content = content.child(div().text_size(crate::typography::ui_rems(10.0))
                        .text_color(theme.text_muted)
                        .child("Cache savings model the uncached-versus-cached input rate when known; subscription billing is separate."));
                    if mix.reasoning > 0 {
                        content = content.child(
                            div()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted)
                                .child(format!(
                                    "{} reasoning tokens are included in output",
                                    count(mix.reasoning)
                                )),
                        );
                    }
                    let mut models = BTreeMap::<String, Vec<&UsageBucket>>::new();
                    for bucket in &selected {
                        models.entry(bucket.model.clone()).or_default().push(bucket);
                    }
                    content = content.child(
                        div()
                            .text_size(crate::typography::ui_rems(14.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("By model"),
                    );
                    for (model, buckets) in models {
                        let quality = buckets.first().map(|bucket| bucket.cost_quality);
                        let quality = if quality.is_some_and(|quality| {
                            buckets.iter().all(|bucket| bucket.cost_quality == quality)
                        }) {
                            quality.unwrap()
                        } else {
                            CostQuality::Mixed
                        };
                        let provenance = match quality {
                            CostQuality::ProviderReported => "provider reported",
                            CostQuality::ModelPriced => "LiteLLM model price",
                            CostQuality::UserOverride => "your exact-model rate",
                            CostQuality::Unpriced => "unpriced",
                            CostQuality::Mixed => "mixed provenance",
                        };
                        content = content.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_between()
                                .gap(px(12.0))
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted)
                                .child(model)
                                .child(format!(
                                    "{} tokens · {} · {provenance}",
                                    count(buckets.iter().map(|bucket| bucket.tokens.total()).sum()),
                                    cost_of(&buckets).map_or("Unpriced".into(), money)
                                )),
                        );
                    }
                }
            }
            if !limits_view {
                content = content.child(
                    div()
                        .text_size(crate::typography::ui_rems(14.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child("History coverage"),
                );
                for source in &snapshot.summary.sources {
                    let status = match source.status {
                        UsageSourceStatus::Ready => "Ready",
                        UsageSourceStatus::Missing => "Missing",
                        UsageSourceStatus::Partial => "Partial",
                        UsageSourceStatus::Unsupported => "Unsupported",
                        UsageSourceStatus::Duplicate => "Duplicate · excluded",
                    };
                    content = content.child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child(format!(
                                "{} history: {status} · {} scanned · {} cached{}",
                                source.provider.label(),
                                source.files_scanned,
                                source.files_cached,
                                source
                                    .detail
                                    .as_ref()
                                    .map_or(String::new(), |detail| format!(" · {detail}"))
                            )),
                    );
                }
            }
            for warning in &snapshot.summary.warnings {
                content = content.child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.warning)
                        .child(warning.clone()),
                );
            }
        } else {
            content = content.child(div().text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child("No readable Codex or Claude transcript source is available for this environment. Provider limits may still be reported separately."));
        }
        div()
            .id("usage-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .max_w(px(820.0))
                    .mx_auto()
                    .px(px(28.0))
                    .py(px(28.0))
                    .flex()
                    .flex_col()
                    .gap(px(18.0))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .justify_between()
                            .gap(px(8.0))
                            .items_center()
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(19.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child("Usage"),
                            )
                            .child(
                                div()
                                    .id("usage-sidebar-visibility")
                                    .cursor_pointer()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.text_muted)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings.usage_sidebar_hidden =
                                            !this.settings.usage_sidebar_hidden;
                                        this.usage_card_open = false;
                                        this.schedule_save(cx);
                                        cx.notify();
                                    }))
                                    .child(if self.settings.usage_sidebar_hidden {
                                        "Show sidebar action"
                                    } else {
                                        "Hide sidebar action"
                                    }),
                            ),
                    )
                    .child(toolbar)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(10.0))
                            .child(periods)
                            .child(
                                div()
                                    .id("usage-environment")
                                    .cursor_pointer()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.text_muted)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cycle_usage_environment(cx)
                                    }))
                                    .child(format!("Environment: {environment_label} · change")),
                            )
                            .child(
                                div()
                                    .id("usage-refresh")
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.text_muted)
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh_usage(cx)))
                                    .child(icon(icons::REFRESH).size(px(14.0)))
                                    .child(if self.usage_refreshing {
                                        "Refreshing…"
                                    } else {
                                        "Refresh"
                                    }),
                            ),
                    )
                    .child(provider_tabs)
                    .when(stale, |el| {
                        el.child(div().text_color(theme.warning).child("Snapshot is stale"))
                    })
                    .when_some(self.usage_error.clone(), |el, error| {
                        el.child(div().text_color(theme.warning).child(error))
                    })
                    .child(content)
                    .when(metric == "cost", |page| {
                        page.child(self.render_usage_price_editor(&theme, cx))
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_usage_card(
        &mut self,
        _theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.usage_card_open {
            return div().into_any_element();
        }
        let environment_label = self.compact_usage_environment_label(cx);
        let snapshot = self.selected_usage_snapshot();
        let mut providers: Vec<_> = [UsageProvider::Codex, UsageProvider::OpenCodeGo]
            .into_iter()
            .filter(|provider| {
                snapshot.as_ref().is_none_or(|snapshot| {
                    snapshot
                        .limits
                        .iter()
                        .any(|limit| limit.provider == *provider)
                        || snapshot
                            .summary
                            .sources
                            .iter()
                            .any(|source| source.provider == *provider)
                })
            })
            .collect();
        if providers.is_empty()
            && snapshot.as_ref().is_some_and(|snapshot| {
                snapshot
                    .limits
                    .iter()
                    .any(|limit| limit.provider == UsageProvider::Claude)
                    || snapshot
                        .summary
                        .sources
                        .iter()
                        .any(|source| source.provider == UsageProvider::Claude)
            })
        {
            providers.push(UsageProvider::Claude);
        }
        let active = self
            .usage_provider_filter
            .filter(|provider| providers.contains(provider))
            .or_else(|| providers.first().copied());
        let tabs = div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .children(providers.into_iter().map(|provider| {
                let selected = active == Some(provider);
                let update_available = self.provider_updates.iter().any(|status| {
                    status.update_available
                        && status.harness
                            == match provider {
                                UsageProvider::Codex => luna_proto::HarnessId::Codex,
                                UsageProvider::OpenCodeGo => luna_proto::HarnessId::Opencode,
                                UsageProvider::Claude => luna_proto::HarnessId::ClaudeCode,
                            }
                }) || snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.limits.iter().any(|limit| {
                        limit.provider == provider
                            && limit.is_fresh(Utc::now().timestamp_millis())
                            && limit
                                .windows
                                .iter()
                                .any(|window| window.used_percent >= 95.0)
                    })
                });
                div()
                    .id(SharedString::from(format!(
                        "usage-compact-provider-{provider:?}"
                    )))
                    .role(gpui::Role::Button)
                    .aria_label(format!("Show {} usage", provider.label()))
                    .aria_selected(selected)
                    .tab_index(0)
                    .cursor_pointer()
                    .w(px(32.0))
                    .h(px(36.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_b_2()
                    .border_color(if selected {
                        gpui::rgb(0xcacacc)
                    } else {
                        gpui::rgb(0x242424)
                    })
                    .hover(|el| el.bg(gpui::rgb(0x303030)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.usage_provider_filter = Some(provider);
                        cx.notify();
                    }))
                    .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                        if matches!(
                            event.keystroke.key.to_ascii_lowercase().as_str(),
                            "enter" | "space"
                        ) {
                            cx.stop_propagation();
                            this.usage_provider_filter = Some(provider);
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .relative()
                            .size(px(18.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                icon(provider_mark(provider))
                                    .size(px(if provider == UsageProvider::OpenCodeGo {
                                        14.0
                                    } else {
                                        18.0
                                    }))
                                    .text_color(if provider == UsageProvider::OpenCodeGo {
                                        gpui::rgb(0x3976f6)
                                    } else if selected {
                                        gpui::rgb(0xd1d1d3)
                                    } else {
                                        gpui::rgb(0xa5a5a8)
                                    }),
                            )
                            .when(update_available, |el| {
                                el.child(
                                    div()
                                        .absolute()
                                        .top(px(-2.0))
                                        .right(px(-3.0))
                                        .size(px(6.0))
                                        .rounded(px(3.0))
                                        .bg(gpui::rgb(0xd34351)),
                                )
                            }),
                    )
            }));
        let mut content = div()
            .id("usage-card-summaries")
            .min_w_0()
            .pb(px(4.0))
            .max_h(px((self.viewport_height * 0.25).clamp(130.0, 230.0)))
            .overflow_y_scroll()
            .flex()
            .flex_col();
        if let Some(snapshot) = snapshot {
            let now = Utc::now().timestamp_millis();
            let limits: Vec<_> = snapshot
                .limits
                .iter()
                .filter(|limit| Some(limit.provider) == active)
                .collect();
            if limits.is_empty() {
                content = content.child(
                    div()
                        .p(px(10.0))
                        .text_size(crate::typography::ui_rems(10.0))
                        .text_color(gpui::rgb(0xa7a7aa))
                        .child("No verified account limits available"),
                );
            }
            for limit in limits {
                content = content.child(compact_limit_row(limit, now));
            }
        } else {
            content = content.child(
                div()
                    .p(px(10.0))
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(gpui::rgb(0xa7a7aa))
                    .child(if self.usage_refreshing {
                        "Refreshing usage…"
                    } else {
                        "No verified account limits available"
                    }),
            );
        }
        div()
            .id("usage-sidebar-card")
            .mx(px(Theme::SPACE_SM))
            .mb(px(0.0))
            .min_w_0()
            .rounded(px(9.0))
            .border_1()
            .border_color(gpui::rgb(0x3a3a3a))
            .bg(gpui::rgb(0x242424))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(40.0))
                    .px(px(4.0))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(tabs)
                    .child(
                        div()
                            .id("usage-card-environment")
                            .role(gpui::Role::Button)
                            .aria_label("Change usage machine")
                            .tab_index(0)
                            .min_w_0()
                            .flex_1()
                            .ml(px(6.0))
                            .truncate()
                            .px(px(5.0))
                            .py(px(4.0))
                            .rounded(px(5.0))
                            .bg(gpui::rgb(0x292929))
                            .cursor_pointer()
                            .text_size(crate::typography::ui_rems(12.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(gpui::rgb(0xc7c7ca))
                            .on_click(
                                cx.listener(|this, _, _, cx| this.cycle_usage_environment(cx)),
                            )
                            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                                if matches!(
                                    event.keystroke.key.to_ascii_lowercase().as_str(),
                                    "enter" | "space"
                                ) {
                                    cx.stop_propagation();
                                    this.cycle_usage_environment(cx);
                                }
                            }))
                            .child(environment_label),
                    )
                    .child(
                        div()
                            .id("usage-card-refresh")
                            .role(gpui::Role::Button)
                            .aria_label(if self.usage_refreshing {
                                "Refreshing usage"
                            } else {
                                "Refresh usage"
                            })
                            .tab_index(0)
                            .size(px(26.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|el| el.bg(gpui::rgb(0x303030)))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_usage(cx)))
                            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                                if matches!(
                                    event.keystroke.key.to_ascii_lowercase().as_str(),
                                    "enter" | "space"
                                ) {
                                    cx.stop_propagation();
                                    this.refresh_usage(cx);
                                }
                            }))
                            .child(icon(icons::REFRESH).size(px(15.0)).text_color(
                                if self.usage_refreshing {
                                    gpui::rgb(0xe0e0e2)
                                } else {
                                    gpui::rgb(0xbdbdc0)
                                },
                            )),
                    )
                    .child(
                        div()
                            .id("usage-card-collapse")
                            .role(gpui::Role::Button)
                            .aria_label("Collapse usage")
                            .tab_index(0)
                            .size(px(24.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_usage_card(cx)))
                            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                                if matches!(
                                    event.keystroke.key.to_ascii_lowercase().as_str(),
                                    "enter" | "space"
                                ) {
                                    cx.stop_propagation();
                                    this.toggle_usage_card(cx);
                                }
                            }))
                            .child(
                                icon(icons::ALT_ARROW_DOWN)
                                    .size(px(13.0))
                                    .text_color(gpui::rgb(0xbdbdc0)),
                            ),
                    ),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(gpui::rgb(0x363636))
                    .child(content),
            )
            .when_some(self.usage_error.clone(), |el, error| {
                el.child(
                    div()
                        .px(px(10.0))
                        .pb(px(7.0))
                        .text_size(crate::typography::ui_rems(9.0))
                        .text_color(gpui::rgb(0xffce46))
                        .child(error),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_usage_action(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.usage_card_open;
        let action = div()
            .id("usage-sidebar-action")
            .role(gpui::Role::Button)
            .aria_label("Toggle usage")
            .aria_expanded(open)
            .size(px(32.0))
            .track_focus(&self.usage_action_focus)
            .cursor_pointer()
            .rounded(px(Theme::CONTROL_RADIUS))
            .bg(theme.glass_hover().opacity(if open { 0.65 } else { 0.0 }))
            .hover(|el| el.bg(theme.glass_hover().opacity(if open { 0.8 } else { 0.0 })))
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.text)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_usage_card(cx)))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if matches!(
                    event.keystroke.key.to_ascii_lowercase().as_str(),
                    "enter" | "space"
                ) {
                    cx.stop_propagation();
                    this.toggle_usage_card(cx);
                }
            }))
            .child(icon(icons::CHART).size(px(18.0)).text_color(theme.text));
        div()
            .size(px(36.0))
            .flex()
            .items_center()
            .justify_center()
            .child(action)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cache_savings_label, compact_limit_status, format_local_hour, limits_status,
        parse_price_override, quota_pace, reset_label,
    };
    use chrono::{FixedOffset, TimeZone};
    use luna_proto::{
        CostQuality, LimitAvailability, UsageBucket, UsageLimitSnapshot, UsageLimitWindow,
        UsageProvider, UsageResetState, UsageTokens,
    };

    #[test]
    fn cache_savings_keeps_partial_price_coverage_visible() {
        let bucket = |savings| UsageBucket {
            start_ms: 0,
            provider: UsageProvider::Codex,
            model: "synthetic".into(),
            tokens: UsageTokens {
                cached_input: 1_000,
                ..UsageTokens::default()
            },
            records: 1,
            sessions: 1,
            api_equivalent_cost_usd: None,
            cache_savings_usd: savings,
            cost_quality: CostQuality::Unpriced,
        };
        let priced = bucket(Some(0.02));
        let unknown = bucket(None);
        assert_eq!(cache_savings_label(&[&priced]), "$0.02");
        assert_eq!(cache_savings_label(&[&unknown]), "Unknown");
        assert!(cache_savings_label(&[&priced, &unknown]).contains("known · partial"));
    }

    #[test]
    fn reset_countdown_does_not_claim_a_past_window_is_current() {
        assert_eq!(
            reset_label(UsageResetState::Unknown, 0),
            "Reset time unknown"
        );
        assert_eq!(
            reset_label(UsageResetState::ReportedFuture { at_ms: 65 * 60_000 }, 0),
            "Resets in 1h 5m"
        );
        assert_eq!(
            reset_label(UsageResetState::ReportedPassed { at_ms: 1 }, 1),
            "Reset time passed · refresh to verify"
        );
        let stale = reset_label(UsageResetState::LastReported { at_ms: 65 * 60_000 }, 0);
        assert!(stale.starts_with("Last reported reset"));
        assert!(!stale.contains("Resets in"));
    }

    #[test]
    fn compact_quota_status_preserves_stale_and_last_good_truth() {
        let mut limit = UsageLimitSnapshot {
            provider: UsageProvider::Codex,
            verified_account_id: Some("synthetic".into()),
            account_label: None,
            plan: None,
            checked_at_ms: Some(0),
            availability: LimitAvailability::Ready,
            windows: vec![UsageLimitWindow {
                id: "window".into(),
                label: "Five hours".into(),
                used_percent: 40.0,
                resets_at_ms: None,
                duration_minutes: Some(300),
            }],
            reset_credits: None,
            detail: None,
        };
        assert_eq!(compact_limit_status(&limit, 60_000), None);
        limit.checked_at_ms = Some(2 * 60_000);
        assert_eq!(
            compact_limit_status(&limit, 60_000),
            Some("Check time unverified")
        );
        assert_eq!(limits_status(&limit, 60_000), "Check time unverified");
        limit.checked_at_ms = Some(0);
        assert_eq!(
            compact_limit_status(&limit, 6 * 60_000),
            Some("Stale · refresh to verify")
        );
        limit.availability = LimitAvailability::ProbeFailed;
        assert_eq!(
            compact_limit_status(&limit, 6 * 60_000),
            Some("Refresh failed · last good values")
        );
        limit.windows[0].resets_at_ms = Some(10 * 60_000);
        let last_reported =
            reset_label(limit.reset_state(&limit.windows[0], 6 * 60_000), 6 * 60_000);
        assert!(last_reported.starts_with("Last reported reset"));
        assert!(!last_reported.contains("Resets in"));
        limit.windows.clear();
        assert_eq!(
            compact_limit_status(&limit, 6 * 60_000),
            Some("Refresh failed · quota unavailable")
        );
        limit.availability = LimitAvailability::Unsupported;
        assert_eq!(
            compact_limit_status(&limit, 6 * 60_000),
            Some("Quota unsupported")
        );
    }

    #[test]
    fn pace_requires_a_fresh_verified_window() {
        let mut limit = UsageLimitSnapshot {
            provider: UsageProvider::Codex,
            verified_account_id: Some("synthetic".into()),
            account_label: None,
            plan: None,
            checked_at_ms: Some(150 * 60_000),
            availability: LimitAvailability::Ready,
            windows: vec![],
            reset_credits: None,
            detail: None,
        };
        let mut window = UsageLimitWindow {
            id: "five-hour".into(),
            label: "Five hours".into(),
            used_percent: 40.0,
            resets_at_ms: Some(300 * 60_000),
            duration_minutes: Some(300),
        };
        assert!(
            quota_pace(&limit, &window, 151 * 60_000)
                .unwrap()
                .contains("used 10 points below")
        );
        assert!(quota_pace(&limit, &window, 156 * 60_000).is_none());
        limit.availability = LimitAvailability::ProbeFailed;
        assert!(quota_pace(&limit, &window, 151 * 60_000).is_none());
        limit.availability = LimitAvailability::Ready;
        window.duration_minutes = None;
        assert!(quota_pace(&limit, &window, 151 * 60_000).is_none());
        window.duration_minutes = Some(300);
        assert!(quota_pace(&limit, &window, 301 * 60_000).is_none());
    }

    #[test]
    fn exact_model_rate_validation_rejects_unknown_as_zero() {
        let (_, rate) = parse_price_override([" gpt-test ", "1", "0.5", "0", "2"]).unwrap();
        assert_eq!(rate.input_per_million, 1.0);
        assert!(parse_price_override(["gpt-test", "0", "0", "0", "0"]).is_err());
        assert!(parse_price_override(["gpt-test", "NaN", "0", "0", "2"]).is_err());
    }

    #[test]
    fn repeated_local_hour_displays_distinct_dst_offsets() {
        let early = FixedOffset::east_opt(2 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 25, 3, 0, 0)
            .unwrap();
        let late = FixedOffset::east_opt(3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 25, 3, 0, 0)
            .unwrap();
        assert!(format_local_hour(early).contains("+02:00"));
        assert!(format_local_hour(late).contains("+01:00"));
        assert_ne!(format_local_hour(early), format_local_hour(late));
    }
}
