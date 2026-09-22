//! L16 native Usage page and compact sidebar disclosure. Both read Shell's
//! single normalized engine snapshot; no UI-side provider or cost calculation
//! is used as a second source of truth. T3 Code's MIT Usage information
//! hierarchy and BB's documented compact disclosure behavior informed this.

use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use zeron_proto::{
    CostQuality, LimitAvailability, USAGE_CONTRACT_VERSION, UsageBucket, UsageLimitSnapshot,
    UsageProvider, UsageSnapshot, UsageSourceStatus,
};
use zeron_rpc::methods;

use super::{SettingsSection, Shell};
use crate::icons::{self, icon};
use crate::theme::Theme;

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

fn period_label(start_ms: i64, hourly: bool) -> String {
    let Some(at) = Utc.timestamp_millis_opt(start_ms).single() else {
        return "Unknown time".into();
    };
    if hourly {
        at.format("%b %-d, %-I %p UTC").to_string()
    } else {
        at.format("%b %-d UTC").to_string()
    }
}

fn reset_label(at_ms: Option<i64>, now_ms: i64) -> String {
    let Some(at_ms) = at_ms else {
        return "Reset time unknown".into();
    };
    if at_ms <= now_ms {
        return "Reset time passed · refresh to verify".into();
    }
    let remaining = (at_ms - now_ms).max(0) / 60_000;
    if remaining < 60 {
        format!("Resets in {remaining}m")
    } else if remaining < 24 * 60 {
        format!("Resets in {}h {}m", remaining / 60, remaining % 60)
    } else {
        format!("Resets in {}d", remaining / (24 * 60))
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
    let Some(checked) = limit.checked_at_ms else {
        return "Check time unknown".into();
    };
    let age = (now_ms - checked).max(0) / 60_000;
    if age >= 5 {
        format!("Stale · checked {age}m ago")
    } else {
        format!("Checked {age}m ago")
    }
}

fn limit_window_row(
    limit: &UsageLimitSnapshot,
    window: &zeron_proto::UsageLimitWindow,
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
    let fresh = limit.availability == LimitAvailability::Ready
        && limit
            .checked_at_ms
            .is_some_and(|at| now_ms - at < 5 * 60_000);
    div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(
            div()
                .flex()
                .justify_between()
                .child(
                    div()
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
                    reset_label(window.resets_at_ms, now_ms),
                    if fresh { "" } else { " · unverified now" }
                )),
        )
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
                .justify_between()
                .items_center()
                .child(
                    div()
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
        .when_some(limit.reset_credits, |card, credits| {
            card.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(format!(
                        "Reset credits: {credits} · redemption unavailable in this build"
                    )),
            )
        })
        .into_any_element()
}

impl Shell {
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

    fn render_usage_provider_tabs(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .gap(px(5.0))
            .children(
                [
                    (None, "All providers"),
                    (Some(UsageProvider::Codex), "Codex"),
                    (Some(UsageProvider::Claude), "Claude"),
                ]
                .into_iter()
                .map(|(provider, label)| {
                    div()
                        .id(SharedString::from(format!("usage-provider-{label}")))
                        .px(px(8.0))
                        .py(px(5.0))
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
                        .text_size(crate::typography::ui_rems(11.0))
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
            .find(|snapshot| {
                snapshot
                    .summary
                    .sources
                    .iter()
                    .any(|source| &source.environment_id == filter)
            })
            .cloned()
    }

    fn cycle_usage_environment(&mut self, cx: &mut Context<Self>) {
        let environments: Vec<_> = self
            .usage_environment_snapshots
            .iter()
            .filter_map(|snapshot| {
                snapshot
                    .summary
                    .sources
                    .first()
                    .map(|source| source.environment_id.clone())
            })
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

    pub(super) fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.usage_error = Some("Engine not connected".into());
            self.usage_refreshing = false;
            cx.notify();
            return;
        };
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
        self.usage_refreshing = true;
        self.usage_error = None;
        // RpcClient and tokio::time both require the Tokio reactor. A GPUI
        // task may await this bridge, but must not poll the RPC fan-out itself.
        // Dropping usage_task drops `fetch`, which aborts the Tokio task.
        let fetch = gpui_tokio::Tokio::spawn(cx, async move {
            futures::future::join_all(targets.into_iter().map(|device| {
                let engine = engine.clone();
                async move {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(8),
                        engine.client().call(
                            methods::GET_USAGE_SNAPSHOT,
                            serde_json::json!({"days": days, "targetDeviceId": device}),
                        ),
                    )
                    .await
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
                for result in results {
                    match result {
                        Ok(Ok(value)) => match serde_json::from_value::<UsageSnapshot>(value) {
                            Ok(snapshot)
                                if snapshot.summary.contract_version == USAGE_CONTRACT_VERSION =>
                            {
                                snapshots.push(snapshot)
                            }
                            _ => failures += 1,
                        },
                        _ => failures += 1,
                    }
                }
                if let Some(mut merged) = zeron_proto::merge_usage_snapshots(snapshots.clone()) {
                    if failures > 0 {
                        merged.summary.partial = true;
                        merged
                            .summary
                            .warnings
                            .push(format!("{failures} environment usage requests failed"));
                    }
                    shell.usage_environment_snapshots = snapshots;
                    shell.usage_snapshot = Some(merged);
                    if failures > 0 {
                        shell.usage_error = Some(format!(
                            "{failures} environments unavailable; showing partial usage"
                        ));
                    }
                } else {
                    shell.usage_error = Some("Usage refresh failed; showing last good data".into());
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
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

    pub(super) fn render_usage_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let now_ms = Utc::now().timestamp_millis();
        let metric = self.settings.usage_metric.clone();
        let limits_view = metric == "limits";
        let provider_tabs = self.render_usage_provider_tabs(&theme, cx);
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
                    )
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
                if !ready {
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
                content = content.child(div().text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child("Subscription limits are separate from historical tokens and API-equivalent cost."));
                for limit in &snapshot.limits {
                    if self
                        .usage_provider_filter
                        .is_none_or(|provider| limit.provider == provider)
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
                if metric == "tokens" {
                    let mut mix = zeron_proto::UsageTokens::default();
                    for bucket in &selected {
                        mix.add(bucket.tokens);
                    }
                    content = content.child(div().text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child(format!("Input {} · Cached {} · Cache creation {} · Output {} · Reasoning {} (inside output)",
                            count(mix.uncached_input), count(mix.cached_input), count(mix.cache_creation), count(mix.output), count(mix.reasoning))));
                }
                let mut providers = BTreeMap::<&str, Vec<&UsageBucket>>::new();
                for bucket in &selected {
                    providers
                        .entry(bucket.provider.label())
                        .or_default()
                        .push(bucket);
                }
                let all_tokens = selected
                    .iter()
                    .map(|bucket| bucket.tokens.total())
                    .sum::<u64>();
                let all_cost = cost_of(&selected);
                for (provider, buckets) in providers {
                    let tokens = buckets
                        .iter()
                        .map(|bucket| bucket.tokens.total())
                        .sum::<u64>();
                    let value = if metric == "tokens" {
                        format!("{} tokens", count(tokens))
                    } else {
                        cost_of(&buckets).map_or("Cost unknown · unpriced records".into(), money)
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
                    let mark = if provider == "Codex" {
                        icons::OPENAI_MARK
                    } else {
                        icons::CLAUDE_MARK
                    };
                    content = content.child(
                        div()
                            .w_full()
                            .rounded(px(Theme::CONTROL_RADIUS))
                            .border_1()
                            .border_color(theme.border)
                            .p(px(12.0))
                            .flex()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(icon(mark).size(px(15.0)).text_color(
                                        if provider == "Codex" {
                                            theme.accent
                                        } else {
                                            theme.text_muted
                                        },
                                    ))
                                    .child(div().text_color(theme.text).child(format!(
                                        "{} · {}",
                                        provider,
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
                    let segments: Vec<AnyElement> = [UsageProvider::Codex, UsageProvider::Claude]
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
                                    .bg(if provider == UsageProvider::Codex {
                                        theme.accent
                                    } else {
                                        theme.text_muted
                                    })
                                    .into_any_element()
                            })
                        })
                        .collect();
                    content = content.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
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
                        CostQuality::ModelPriced => "exact model price",
                        CostQuality::Unpriced => "unpriced",
                        CostQuality::Mixed => "mixed provenance",
                    };
                    content = content.child(
                        div()
                            .flex()
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
                .child("No verified usage source is available. Configure an isolated synthetic fixture to validate this page."));
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
                            .justify_between()
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
                                    .flex()
                                    .items_center()
                                    .gap(px(12.0))
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
                                    )
                                    .child(
                                        div()
                                            .id("usage-refresh")
                                            .cursor_pointer()
                                            .flex()
                                            .items_center()
                                            .gap(px(5.0))
                                            .text_color(theme.text_muted)
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.refresh_usage(cx)
                                                }),
                                            )
                                            .child(icon(icons::REFRESH).size(px(14.0)))
                                            .child(if self.usage_refreshing {
                                                "Refreshing…"
                                            } else {
                                                "Refresh"
                                            }),
                                    ),
                            ),
                    )
                    .child(toolbar)
                    .child(periods)
                    .child(provider_tabs)
                    .child(
                        div()
                            .id("usage-environment")
                            .cursor_pointer()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.cycle_usage_environment(cx)),
                            )
                            .child(format!(
                                "Environment: {} · click to change",
                                self.usage_environment_filter
                                    .as_deref()
                                    .unwrap_or("All environments")
                            )),
                    )
                    .when(stale, |el| {
                        el.child(div().text_color(theme.warning).child("Snapshot is stale"))
                    })
                    .when_some(self.usage_error.clone(), |el, error| {
                        el.child(div().text_color(theme.warning).child(error))
                    })
                    .child(content),
            )
            .into_any_element()
    }

    pub(super) fn render_usage_footer(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.usage_card_open;
        let provider_tabs = self.render_usage_provider_tabs(theme, cx);
        let mut card = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .mx(px(Theme::SPACE_SM))
            .mb(px(4.0))
            .p(px(10.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .border_1()
            .border_color(theme.border)
            .bg(theme.glass_hover().opacity(0.5))
            .child(provider_tabs);
        if let Some(snapshot) = self.selected_usage_snapshot() {
            let now = Utc::now().timestamp_millis();
            for limit in &snapshot.limits {
                if self
                    .usage_provider_filter
                    .is_none_or(|provider| provider == limit.provider)
                {
                    card = card.child(limit_card(limit, theme, now, true));
                }
            }
        } else {
            card = card.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child("No verified account limits available"),
            );
        }
        let card = card
            .child(
                div()
                    .id("usage-card-environment")
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(theme.text_muted)
                    .on_click(cx.listener(|this, _, _, cx| this.cycle_usage_environment(cx)))
                    .child(format!(
                        "Machine: {} · change",
                        self.usage_environment_filter.as_deref().unwrap_or("All")
                    )),
            )
            .when_some(self.usage_error.clone(), |el, error| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(10.0))
                        .text_color(theme.warning)
                        .child(error),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("usage-card-refresh")
                            .cursor_pointer()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_usage(cx)))
                            .child(if self.usage_refreshing {
                                "Refreshing…"
                            } else {
                                "Refresh"
                            }),
                    )
                    .child(
                        div()
                            .id("usage-view-all")
                            .cursor_pointer()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.accent)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_settings(SettingsSection::Usage, cx)
                            }))
                            .child("View all usage"),
                    ),
            );
        div()
            .flex()
            .flex_col()
            .when(open, |el| el.child(card))
            .child(
                div()
                    .id("usage-sidebar-action")
                    .role(gpui::Role::Button)
                    .aria_label("Usage disclosure")
                    .aria_expanded(open)
                    .mx(px(Theme::SPACE_SM))
                    .track_focus(&self.usage_action_focus)
                    .px(px(Theme::SPACE_SM))
                    .py(px(5.0))
                    .cursor_pointer()
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .hover(|el| el.bg(theme.glass_hover()))
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .text_color(theme.text_muted)
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
                    .child(icon(icons::CHART).size(px(16.0)))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(12.0))
                            .child("Usage"),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::reset_label;

    #[test]
    fn reset_countdown_does_not_claim_a_past_window_is_current() {
        assert_eq!(reset_label(None, 0), "Reset time unknown");
        assert_eq!(reset_label(Some(65 * 60_000), 0), "Resets in 1h 5m");
        assert_eq!(
            reset_label(Some(1), 1),
            "Reset time passed · refresh to verify"
        );
    }
}
