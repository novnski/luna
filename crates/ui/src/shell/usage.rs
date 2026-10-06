//! L16 native Usage page and compact sidebar disclosure. Both read Shell's
//! single normalized engine snapshot; no UI-side provider or cost calculation
//! is used as a second source of truth. T3 Code's MIT Usage information
//! hierarchy and BB's documented compact disclosure behavior informed this.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset, Local, TimeZone, Utc};
use gpui::{AnyElement, Context, PathBuilder, SharedString, canvas, div, point, prelude::*, px};
use luna_proto::{
    ChatIndicator, CostQuality, HarnessId, LimitAvailability, USAGE_CONTRACT_VERSION,
    USAGE_LIMIT_FRESHNESS_MS, UsageBucket, UsageLimitSnapshot, UsagePriceRate, UsageProvider,
    UsageResetState, UsageSnapshot, UsageSourceStatus,
};
use luna_rpc::methods;

use super::{SettingsSection, Shell};
use crate::icons::{self, icon};
use crate::theme::Theme;

/// CC-06 live provider sessions section.
#[path = "usage_live.rs"]
mod live;

fn provider_mark(provider: UsageProvider) -> &'static str {
    match provider {
        UsageProvider::Codex => icons::OPENAI_MARK,
        UsageProvider::OpenCodeGo => icons::OPENCODE_MARK,
        UsageProvider::Claude => icons::CLAUDE_MARK,
    }
}

/// One usage meter for the card and the Usage page (audit SS-04): the fill
/// is what's USED, neutral until 75%, warning from 75%, danger from 90%; a
/// 6 pt capsule; nothing drawn below 0.5% (no false stub). Provider identity
/// lives in the mark, not the bar colour.
fn usage_meter(used_percent: f32, theme: &Theme) -> gpui::Div {
    let track = if theme.appearance.is_dark() {
        crate::theme::ink(0.08)
    } else {
        crate::theme::ink(0.10)
    };
    usage_meter_with(
        used_percent,
        [
            theme.text_muted,
            theme.readable_tone(theme.warning, 1.0),
            theme.readable_tone(theme.danger, 1.0),
        ],
        track,
    )
}

/// [`usage_meter`] with explicit `[neutral, warning, danger]` fills.
fn usage_meter_with(used_percent: f32, tones: [gpui::Hsla; 3], track: gpui::Hsla) -> gpui::Div {
    let used = used_percent.clamp(0.0, 100.0);
    let fill = if used >= 90.0 {
        tones[2]
    } else if used >= 75.0 {
        tones[1]
    } else {
        tones[0]
    };
    div()
        .h(px(6.0))
        .flex_1()
        .min_w_0()
        .rounded_full()
        .bg(track)
        .when(used >= 0.5, |bar| {
            bar.child(
                div()
                    .h_full()
                    .w(gpui::relative(used / 100.0))
                    .rounded_full()
                    .bg(fill),
            )
        })
}

fn provider_tone(provider: UsageProvider, theme: &Theme) -> gpui::Hsla {
    match provider {
        UsageProvider::Codex => theme.text,
        UsageProvider::OpenCodeGo => gpui::rgb(0x5b9bbd).into(),
        UsageProvider::Claude => gpui::rgb(0xd97757).into(),
    }
}

#[derive(Clone, Copy)]
struct CompactUsagePalette {
    card: gpui::Hsla,
    border: gpui::Hsla,
    divider: gpui::Hsla,
    selected_rule: gpui::Hsla,
    idle_rule: gpui::Hsla,
    hover: gpui::Hsla,
    heading: gpui::Hsla,
    secondary: gpui::Hsla,
    quiet: gpui::Hsla,
    selected_icon: gpui::Hsla,
    idle_icon: gpui::Hsla,
    control_icon: gpui::Hsla,
    refresh_active: gpui::Hsla,
    opencode_icon: gpui::Hsla,
    environment: gpui::Hsla,
    environment_text: gpui::Hsla,
    badge: gpui::Hsla,
    badge_text: gpui::Hsla,
    track: gpui::Hsla,
    other_fill: gpui::Hsla,
    status: gpui::Hsla,
    dot: gpui::Hsla,
}

impl CompactUsagePalette {
    fn for_theme(theme: &Theme) -> Self {
        Self {
            card: theme.card_glass_bg(),
            border: theme.border_strong,
            divider: theme.border,
            selected_rule: theme.accent,
            idle_rule: theme.surface.opacity(0.0),
            hover: theme.element_hover,
            heading: theme.text,
            secondary: theme.text_muted,
            quiet: theme.text_muted,
            selected_icon: theme.text,
            idle_icon: theme.text_muted,
            control_icon: theme.text_muted,
            refresh_active: theme.text,
            opencode_icon: theme.accent,
            environment: theme.surface_raised,
            environment_text: theme.text,
            badge: theme.surface_raised,
            badge_text: theme.text_muted,
            track: theme.surface_raised,
            other_fill: theme.text_muted,
            status: theme.warning,
            dot: theme.danger,
        }
    }
}

fn compact_tokens(value: u64) -> String {
    for (threshold, suffix) in [
        (1_000_000_000_000, "T"),
        (1_000_000_000, "B"),
        (1_000_000, "M"),
        (1_000, "K"),
    ] {
        if value >= threshold {
            let scaled = value as f64 / threshold as f64;
            let digits = if scaled >= 100.0 {
                0
            } else if scaled >= 10.0 {
                1
            } else {
                2
            };
            let formatted = format!("{scaled:.digits$}");
            let trimmed = if formatted.contains('.') {
                formatted.trim_end_matches('0').trim_end_matches('.')
            } else {
                &formatted
            };
            return format!("{trimmed}{suffix}");
        }
    }
    count(value)
}

fn usage_percent(value: f64) -> String {
    let percent = value * 100.0;
    if percent > 0.0 && percent < 0.1 {
        "<0.1%".into()
    } else {
        format!("{percent:.1}%")
    }
}

fn bucket_known_cost(bucket: &UsageBucket) -> f64 {
    bucket
        .known_cost_usd
        .unwrap_or(bucket.api_equivalent_cost_usd.unwrap_or(0.0))
}

fn bucket_unpriced_records(bucket: &UsageBucket) -> u64 {
    bucket.unpriced_records.unwrap_or_else(|| {
        if bucket.api_equivalent_cost_usd.is_none() {
            bucket.records
        } else {
            0
        }
    })
}

fn short_period_label(start_ms: i64, hourly: bool) -> String {
    let Some(at) = Utc.timestamp_millis_opt(start_ms).single() else {
        return "Unknown".into();
    };
    if hourly {
        at.with_timezone(&Local).format("%-I %p").to_string()
    } else {
        at.with_timezone(&Local).format("%b %-d").to_string()
    }
}

fn hover_period_label(start_ms: i64, hourly: bool) -> String {
    let Some(start) = Utc.timestamp_millis_opt(start_ms).single() else {
        return "Unknown period".into();
    };
    let local = start.with_timezone(&Local);
    if hourly {
        let end = (start + chrono::Duration::hours(1)).with_timezone(&Local);
        format!(
            "{}–{}",
            local.format("%b %-d, %-I:%M %p"),
            end.format("%-I:%M %p")
        )
    } else {
        local.format("%a, %b %-d").to_string()
    }
}

fn local_day_start(day: chrono::NaiveDate) -> Option<i64> {
    (0..=3).find_map(|hour| {
        let wall = day.and_hms_opt(hour, 0, 0)?;
        Local
            .from_local_datetime(&wall)
            .earliest()
            .map(|time| time.timestamp_millis())
    })
}

fn usage_period_window(days: u32, now_ms: i64) -> (Vec<i64>, i64) {
    if days == 1 {
        let until = now_ms.div_euclid(60_000) * 60_000;
        let first = until - 24 * 3_600_000;
        return (
            (0..24).map(|index| first + index * 3_600_000).collect(),
            until,
        );
    }
    let days = if [7, 30, 90].contains(&days) {
        days
    } else {
        30
    };
    let today = Utc
        .timestamp_millis_opt(now_ms)
        .single()
        .unwrap_or_else(Utc::now)
        .with_timezone(&Local)
        .date_naive();
    let starts: Vec<_> = (0..days)
        .rev()
        .filter_map(|back| {
            today
                .checked_sub_signed(chrono::Duration::days(i64::from(back)))
                .and_then(local_day_start)
        })
        .collect();
    let until = today
        .checked_add_signed(chrono::Duration::days(1))
        .and_then(local_day_start)
        .unwrap_or(now_ms + 86_400_000);
    (starts, until)
}

fn usage_table_row(
    theme: &Theme,
    columns: [String; 4],
    header: bool,
    unpriced: bool,
    provider: Option<UsageProvider>,
) -> AnyElement {
    let color = if header || unpriced {
        theme.text_muted
    } else {
        theme.text
    };
    div()
        .w_full()
        .flex()
        .items_center()
        .min_h(px(if header { 31.0 } else { 38.0 }))
        .border_b_1()
        .border_color(theme.border.opacity(if header { 1.0 } else { 0.55 }))
        .text_size(crate::typography::ui_rems(if header { 11.0 } else { 13.0 }))
        .child(
            div()
                .w(gpui::relative(0.40))
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(7.0))
                .when_some(provider, |row, provider| {
                    row.child(
                        icon(provider_mark(provider))
                            .size(px(14.0))
                            .text_color(theme.text),
                    )
                })
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(color)
                        .child(columns[0].clone()),
                ),
        )
        .child(
            div()
                .w(gpui::relative(0.20))
                .text_align(gpui::TextAlign::Right)
                .text_color(color)
                .child(columns[1].clone()),
        )
        .child(
            div()
                .w(gpui::relative(0.20))
                .text_align(gpui::TextAlign::Right)
                .text_color(theme.text_muted)
                .child(columns[2].clone()),
        )
        .child(
            div()
                .w(gpui::relative(0.20))
                .text_align(gpui::TextAlign::Right)
                .text_color(theme.text_muted)
                .child(columns[3].clone()),
        )
        .into_any_element()
}

fn monotone_usage_tangents(points: &[(f32, f32)]) -> Vec<f32> {
    if points.len() < 2 {
        return vec![0.0; points.len()];
    }
    let slopes: Vec<f32> = points
        .windows(2)
        .map(|pair| {
            let dx = pair[1].0 - pair[0].0;
            if dx == 0.0 {
                0.0
            } else {
                (pair[1].1 - pair[0].1) / dx
            }
        })
        .collect();
    let mut tangents = vec![0.0; points.len()];
    tangents[0] = slopes[0];
    *tangents.last_mut().unwrap() = *slopes.last().unwrap();
    for index in 1..points.len() - 1 {
        let previous = slopes[index - 1];
        let next = slopes[index];
        tangents[index] = if previous * next <= 0.0 {
            0.0
        } else {
            (previous + next) / 2.0
        };
    }
    for (index, slope) in slopes.iter().copied().enumerate() {
        if slope == 0.0 {
            tangents[index] = 0.0;
            tangents[index + 1] = 0.0;
            continue;
        }
        let a = tangents[index] / slope;
        let b = tangents[index + 1] / slope;
        let magnitude = a * a + b * b;
        if magnitude > 9.0 {
            let scale = 3.0 / magnitude.sqrt();
            tangents[index] = scale * a * slope;
            tangents[index + 1] = scale * b * slope;
        }
    }
    tangents
}

fn usage_chart(
    theme: &Theme,
    periods: Vec<(i64, [f64; 3])>,
    tokens: bool,
    hourly: bool,
    hovered_start: Option<i64>,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let peak = periods
        .iter()
        .flat_map(|(_, values)| values)
        .copied()
        .fold(0.0_f64, f64::max);
    let scale = if peak <= 0.0 {
        1.0
    } else {
        let rough = peak / 2.0;
        let magnitude = 10_f64.powf(rough.log10().floor());
        let unit = [1.0, 2.0, 5.0, 10.0]
            .into_iter()
            .find(|step| step * magnitude >= rough)
            .unwrap_or(10.0)
            * magnitude;
        (peak / unit).ceil() * unit
    };
    let labels = [scale, scale / 2.0, 0.0].map(|value| {
        if value == 0.0 {
            "0".into()
        } else if tokens {
            compact_tokens(value.round() as u64)
        } else {
            money(value)
        }
    });
    let labels = if peak <= 0.0 {
        [String::new(), String::new(), "0".into()]
    } else {
        labels
    };
    let colors = [
        provider_tone(UsageProvider::Codex, theme),
        provider_tone(UsageProvider::OpenCodeGo, theme),
        provider_tone(UsageProvider::Claude, theme),
    ];
    let grid = theme.border;
    let hover_color = theme.text_muted;
    let drawing_periods = periods.clone();
    let plot = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            if width < 2.0 || height < 2.0 {
                return;
            }
            let x0 = bounds.origin.x;
            let y0 = bounds.origin.y;
            for ratio in [0.0_f32, 0.5, 1.0] {
                let mut path = PathBuilder::stroke(px(1.0));
                let y = y0 + px(ratio * height);
                path.move_to(point(x0, y));
                path.line_to(point(x0 + px(width), y));
                if let Ok(path) = path.build() {
                    window.paint_path(path, grid);
                }
            }
            if drawing_periods.is_empty() {
                return;
            }
            // The source chart layers each provider from zero; the largest series
            // is painted first so smaller series and their lines remain visible.
            let mut order = [0_usize, 1, 2];
            order.sort_by(|a, b| {
                let sum = |index| {
                    drawing_periods
                        .iter()
                        .map(|(_, values)| values[index])
                        .sum::<f64>()
                };
                sum(*b).total_cmp(&sum(*a))
            });
            for fill in [true, false] {
                for index in order {
                    if drawing_periods
                        .iter()
                        .all(|(_, values)| values[index] <= 0.0)
                    {
                        continue;
                    }
                    let mut path = if fill {
                        PathBuilder::fill()
                    } else {
                        PathBuilder::stroke(px(2.0))
                    };
                    let length = drawing_periods.len();
                    let coordinates: Vec<(f32, f32)> = drawing_periods
                        .iter()
                        .enumerate()
                        .map(|(position, (_, values))| {
                            let x = if length == 1 {
                                width / 2.0
                            } else {
                                width * position as f32 / (length - 1) as f32
                            };
                            let y = height
                                - (values[index] / scale).clamp(0.0, 1.0) as f32 * (height - 8.0);
                            (x, y)
                        })
                        .collect();
                    if coordinates.is_empty() {
                        continue;
                    }
                    path.move_to(point(x0 + px(coordinates[0].0), y0 + px(coordinates[0].1)));
                    // The T3 chart's monotone cubic curve avoids drawing a
                    // false peak between two measured periods.
                    let tangents = monotone_usage_tangents(&coordinates);
                    for (index, pair) in coordinates.windows(2).enumerate() {
                        let (xa, ya) = pair[0];
                        let (xb, yb) = pair[1];
                        let dx = (xb - xa) / 3.0;
                        path.cubic_bezier_to(
                            point(x0 + px(xb), y0 + px(yb)),
                            point(x0 + px(xa + dx), y0 + px(ya + tangents[index] * dx)),
                            point(x0 + px(xb - dx), y0 + px(yb - tangents[index + 1] * dx)),
                        );
                    }
                    if fill {
                        path.line_to(point(
                            x0 + px(coordinates.last().unwrap().0),
                            y0 + px(height),
                        ));
                        path.line_to(point(x0 + px(coordinates[0].0), y0 + px(height)));
                        path.close();
                    }
                    if let Ok(path) = path.build() {
                        window.paint_path(
                            path,
                            if fill {
                                colors[index].opacity(0.12)
                            } else {
                                colors[index]
                            },
                        );
                    }
                }
            }
            if let Some(index) = drawing_periods
                .iter()
                .position(|(start, _)| Some(*start) == hovered_start)
            {
                let x = if drawing_periods.len() == 1 {
                    width / 2.0
                } else {
                    width * index as f32 / (drawing_periods.len() - 1) as f32
                };
                let mut guide = PathBuilder::stroke(px(1.0));
                guide.move_to(point(x0 + px(x), y0));
                guide.line_to(point(x0 + px(x), y0 + px(height)));
                if let Ok(guide) = guide.build() {
                    window.paint_path(guide, hover_color);
                }
            }
        },
    )
    .w_full()
    .h(px(224.0));
    let x_labels = if periods.is_empty() {
        [String::new(), String::new(), String::new()]
    } else {
        [0, periods.len() / 2, periods.len() - 1]
            .map(|index| short_period_label(periods[index].0, hourly).to_uppercase())
    };
    let hover_cells: Vec<_> = periods
        .iter()
        .map(|(start, values)| {
            let start = *start;
            let label = hover_period_label(start, hourly);
            let values = *values;
            let format = |value: f64| {
                if tokens {
                    compact_tokens(value.round() as u64)
                } else {
                    money(value)
                }
            };
            let detail = format!(
                "{label}\nCodex {}\nOpenCode Go {}\nClaude {}\nTotal {}",
                format(values[0]),
                format(values[1]),
                format(values[2]),
                format(values.iter().sum())
            );
            div()
                .id(SharedString::from(format!("usage-chart-period-{start}")))
                .flex_1()
                .h_full()
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        this.usage_hover_period_start = Some(start);
                    } else if this.usage_hover_period_start == Some(start) {
                        this.usage_hover_period_start = None;
                    }
                    cx.notify();
                }))
                .tooltip(move |_, cx| {
                    cx.new(|_| super::SurfaceTabTooltip {
                        text: detail.clone().into(),
                    })
                    .into()
                })
        })
        .collect();
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(
            div()
                .w_full()
                .flex()
                .gap(px(8.0))
                .child(
                    div()
                        .w(px(64.0))
                        .h(px(224.0))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .justify_between()
                        .text_align(gpui::TextAlign::Right)
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .children(labels),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .h(px(224.0))
                        .child(plot)
                        .child(div().absolute().inset_0().flex().children(hover_cells)),
                ),
        )
        .child(
            div()
                .w_full()
                .pl(px(72.0))
                .flex()
                .justify_between()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .children(x_labels),
        )
        .into_any_element()
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
                .text_size(crate::typography::ui_rems(11.0))
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
    let cents = (value.max(0.0) * 100.0).round() as u64;
    format!("${}.{:02}", count(cents / 100), cents % 100)
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

/// T3's Limits hierarchy: provider sections, one pool card per quota window,
/// and equal-width account segments. An unverified account keeps its own
/// segment; only verified identities were de-duplicated by the snapshot merge.
fn pooled_limit_sections(limits: &[UsageLimitSnapshot], theme: &Theme, now_ms: i64) -> AnyElement {
    let mut sections = div().w_full().flex().flex_col().gap(px(28.0));
    let mut has_section = false;
    for provider in [
        UsageProvider::Codex,
        UsageProvider::Claude,
        UsageProvider::OpenCodeGo,
    ] {
        let accounts: Vec<_> = limits
            .iter()
            .filter(|limit| limit.provider == provider)
            .collect();
        if accounts.is_empty() {
            continue;
        }
        has_section = true;
        let mut section = div().w_full().flex().flex_col().gap(px(12.0)).child(
            div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .text_color(theme.text)
                .text_size(crate::typography::ui_rems(14.0))
                .child(
                    icon(provider_mark(provider))
                        .size(px(17.0))
                        .text_color(theme.text),
                )
                .child(provider.label()),
        );
        let ready: Vec<_> = accounts
            .iter()
            .copied()
            .filter(|limit| {
                limit.availability == LimitAvailability::Ready && !limit.windows.is_empty()
            })
            .collect();
        let mut windows: Vec<(String, String, Option<u64>)> = ready
            .iter()
            .flat_map(|limit| {
                limit.windows.iter().map(|window| {
                    (
                        window.id.clone(),
                        window.label.clone(),
                        window.duration_minutes,
                    )
                })
            })
            .collect();
        windows.sort_by(|left, right| {
            left.2
                .unwrap_or(u64::MAX)
                .cmp(&right.2.unwrap_or(u64::MAX))
                .then_with(|| left.0.cmp(&right.0))
        });
        windows.dedup_by(|left, right| left.0 == right.0);
        for (window_id, label, _) in windows {
            let reported: Vec<_> = ready
                .iter()
                .filter_map(|limit| {
                    limit
                        .windows
                        .iter()
                        .find(|window| window.id == window_id)
                        .map(|window| (*limit, window))
                })
                .collect();
            if reported.is_empty() {
                continue;
            }
            let used = reported
                .iter()
                .map(|(_, window)| window.used_percent.clamp(0.0, 100.0))
                .sum::<f32>()
                / reported.len() as f32;
            let remaining = (100.0 - used).round();
            let next_reset = reported
                .iter()
                .filter_map(|(limit, window)| {
                    (window.used_percent > 0.0)
                        .then(|| window.resets_at_ms)
                        .flatten()
                        .filter(|at| *at > now_ms && limit.is_fresh(now_ms))
                })
                .min();
            let reset = next_reset
                .map(|at| {
                    let delta = (at - now_ms) / 60_000;
                    if delta < 60 {
                        format!("Next reset in {delta}m")
                    } else if delta < 24 * 60 {
                        format!("Next reset in {}h {}m", delta / 60, delta % 60)
                    } else {
                        format!(
                            "Next reset in {}d {}h",
                            delta / (24 * 60),
                            (delta % (24 * 60)) / 60
                        )
                    }
                })
                .unwrap_or_else(|| "Reset time unavailable".into());
            let mut segments = div().w_full().flex().gap(px(4.0));
            for limit in &ready {
                if let Some(window) = limit.windows.iter().find(|window| window.id == window_id) {
                    let left = (100.0 - window.used_percent).clamp(0.0, 100.0);
                    let account = limit
                        .account_label
                        .clone()
                        .unwrap_or_else(|| provider.label().into());
                    let detail = format!(
                        "{account} · {left:.0}% left · {} · {}",
                        limit.plan.as_deref().unwrap_or("Plan unknown"),
                        reset_label(limit.reset_state(window, now_ms), now_ms)
                    );
                    segments = segments.child(
                        usage_meter(window.used_percent, &theme)
                            .id(SharedString::from(format!(
                                "usage-limit-{}-{window_id}-{}",
                                provider.label(),
                                account
                            )))
                            .tooltip(crate::popover::text_tooltip(detail)),
                    );
                } else {
                    segments = segments.child(usage_meter(0.0, &theme));
                }
            }
            let status = if ready.iter().all(|limit| limit.is_fresh(now_ms)) {
                reset
            } else {
                format!("{reset} · some account values are last reported")
            };
            let status = if reported.len() == 1 {
                quota_pace(reported[0].0, reported[0].1, now_ms)
                    .map_or(status.clone(), |pace| format!("{status} · {pace}"))
            } else {
                status
            };
            section = section.child(
                div()
                    .w_full()
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(theme.border.opacity(0.6))
                    .p(px(15.0))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(22.0))
                    .child(
                        div()
                            .w(px(176.0))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_color(theme.text)
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .text_color(theme.text)
                                            .text_size(crate::typography::ui_rems(29.0))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(format!("{remaining:.0}%")),
                                    )
                                    .child(
                                        div()
                                            .text_color(theme.text_muted)
                                            .text_size(crate::typography::ui_rems(12.0))
                                            .child("left"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .min_w(px(220.0))
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(segments)
                            .child(
                                div()
                                    .text_color(theme.text_muted)
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .child(format!(
                                        "{} · {} {}",
                                        status,
                                        ready.len(),
                                        if ready.len() == 1 {
                                            "account"
                                        } else {
                                            "accounts"
                                        }
                                    )),
                            ),
                    ),
            );
        }
        let credits: u64 = ready.iter().filter_map(|limit| limit.reset_credits).sum();
        if credits > 0 {
            section = section.child(
                div()
                    .text_color(theme.text_muted)
                    .text_size(crate::typography::ui_rems(11.0))
                    .child(format!(
                        "{credits} reset {} banked · redemption unavailable in this build",
                        if credits == 1 { "credit" } else { "credits" }
                    )),
            );
        }
        for limit in accounts.into_iter().filter(|limit| {
            limit.availability != LimitAvailability::Ready || limit.windows.is_empty()
        }) {
            let detail = limit
                .detail
                .clone()
                .unwrap_or_else(|| limits_status(limit, now_ms));
            section = section.child(
                div()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border.opacity(0.6))
                    .p(px(12.0))
                    .text_color(theme.text_muted)
                    .text_size(crate::typography::ui_rems(11.0))
                    .child(format!("{}: {detail}", provider.label())),
            );
        }
        sections = sections.child(section);
    }
    if !has_section {
        sections = sections.child(
            div()
                .text_color(theme.text_muted)
                .child("No provider on the selected environments reports subscription limits."),
        );
    }
    sections.into_any_element()
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

fn compact_limit_row(
    limit: &UsageLimitSnapshot,
    now_ms: i64,
    palette: CompactUsagePalette,
) -> AnyElement {
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
                        .line_height(crate::typography::ui_rems(15.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(palette.heading)
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
                            .bg(palette.badge)
                            .text_size(crate::typography::ui_rems(11.0))
                            .line_height(crate::typography::ui_rems(12.0))
                            .text_color(palette.badge_text)
                            .child(compact_plan_label(limit, &plan)),
                    )
                }),
        );
    if let Some(account) = limit.account_label.as_deref().filter(|s| !s.is_empty()) {
        row = row.child(
            div()
                .min_w_0()
                .truncate()
                .text_size(crate::typography::ui_rems(11.0))
                .line_height(crate::typography::ui_rems(14.0))
                .text_color(palette.quiet)
                .child(account.to_owned()),
        );
    }
    if limit.windows.is_empty() {
        row = row.child(
            div()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(palette.quiet)
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
        row = row.child(
            div()
                .mt(px(3.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(11.0))
                .line_height(crate::typography::ui_rems(14.0))
                .text_color(palette.secondary)
                .child(
                    div()
                        .w(crate::typography::ui_rems(label_width))
                        .flex_none()
                        .truncate()
                        .child(compact_window_label(limit, window)),
                )
                .child(usage_meter_with(
                    used,
                    [palette.other_fill, palette.status, palette.dot],
                    palette.track,
                ))
                .child(
                    div()
                        .w(crate::typography::ui_rems(30.0))
                        .flex_none()
                        .whitespace_nowrap()
                        .text_align(gpui::TextAlign::Right)
                        .child(format!("{used:.0}%")),
                )
                .child(
                    div()
                        .w(crate::typography::ui_rems(reset_width))
                        .flex_none()
                        .whitespace_nowrap()
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
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(palette.status)
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
                    .text_color(theme.readable_tone(theme.accent, 1.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .on_click(cx.listener(|this, _, _, cx| this.save_usage_price_override(cx)))
                    .child("Save exact-model rate"),
            );
        if let Some(error) = &self.usage_price_error {
            card = card.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.readable_tone(theme.warning, 1.0))
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
                            .text_color(theme.readable_tone(theme.warning, 1.0))
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

    fn page_usage_snapshot(&self) -> Option<UsageSnapshot> {
        let Some(ids) = &self.usage_page_environment_ids else {
            return self.usage_snapshot.clone();
        };
        luna_proto::merge_usage_snapshots(
            self.usage_environment_snapshots
                .iter()
                .filter(|(device, _)| ids.contains(device))
                .map(|(_, snapshot)| snapshot.clone())
                .collect(),
        )
    }

    fn usage_environment_name(&self, id: &str, cx: &Context<Self>) -> String {
        let state = self.state.read(cx);
        if state
            .engine()
            .is_some_and(|engine| engine.engine_info().device_id == id)
        {
            return "This machine".into();
        }
        state
            .devices
            .iter()
            .find(|device| device.id == id)
            .map(|device| device.name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Selected environment".into())
    }

    fn toggle_page_usage_environment(&mut self, id: &str, cx: &mut Context<Self>) {
        let all: std::collections::HashSet<_> = self
            .usage_environment_snapshots
            .iter()
            .map(|(device, _)| device.clone())
            .collect();
        let mut selected = self
            .usage_page_environment_ids
            .clone()
            .unwrap_or_else(|| all.clone());
        if !selected.remove(id) {
            selected.insert(id.to_owned());
        }
        self.usage_page_environment_ids = (selected != all).then_some(selected);
        cx.notify();
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
        let (period_starts_ms, until_ms) = usage_period_window(days, Utc::now().timestamp_millis());
        let include_limits = !matches!(self.route, super::Route::Settings(SettingsSection::Usage))
            || self.settings.usage_metric == "limits";
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
                let period_starts_ms = period_starts_ms.clone();
                async move {
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        engine.client().call(
                            methods::GET_USAGE_SNAPSHOT,
                            serde_json::json!({"days": days, "periodStartsMs": period_starts_ms, "untilMs": until_ms, "includeLimits": include_limits, "targetDeviceId": device, "priceOverrides": price_overrides}),
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
        self.usage_hover_period_start = None;
        self.schedule_save(cx);
        if metric == "limits" {
            self.refresh_usage(cx);
        }
        cx.notify();
    }

    fn set_usage_days(&mut self, days: u32, cx: &mut Context<Self>) {
        self.settings.usage_days = days;
        self.usage_hover_period_start = None;
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
        let now_ms = Utc::now().timestamp_millis();
        let metric = self.settings.usage_metric.clone();
        let limits_view = metric == "limits";
        let tokens_view = metric == "tokens";
        let hourly = self.settings.usage_days == 1;
        let compact = self.viewport_width < 1000.0;
        let snapshot = self.page_usage_snapshot();
        let environment_label = match &self.usage_page_environment_ids {
            None => "All environments".to_string(),
            Some(ids) if ids.len() == 1 => {
                self.usage_environment_name(ids.iter().next().unwrap(), cx)
            }
            Some(ids) => format!("{} environments", ids.len()),
        };
        let mut environment_menu = div()
            .w(px(300.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.bg)
            .p(px(5.0))
            .flex()
            .flex_col()
            .gap(px(2.0));
        if self.usage_environment_menu_open {
            environment_menu = environment_menu.child(
                div()
                    .id("usage-environment-all")
                    .cursor_pointer()
                    .rounded(px(6.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .text_color(theme.text)
                    .text_size(crate::typography::ui_rems(12.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.usage_page_environment_ids = None;
                        cx.notify();
                    }))
                    .child(format!(
                        "{}  All environments",
                        if self.usage_page_environment_ids.is_none() {
                            "✓"
                        } else {
                            " "
                        }
                    )),
            );
            for (device, source) in &self.usage_environment_snapshots {
                let id = device.clone();
                let checked = self
                    .usage_page_environment_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(device));
                let label = self.usage_environment_name(device, cx);
                let status = if source.summary.partial {
                    "Partial"
                } else {
                    "Ready"
                };
                environment_menu = environment_menu.child(
                    div()
                        .id(SharedString::from(format!("usage-environment-{id}")))
                        .cursor_pointer()
                        .rounded(px(6.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .flex()
                        .justify_between()
                        .text_size(crate::typography::ui_rems(12.0))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_page_usage_environment(&id, cx)
                        }))
                        .child(
                            div()
                                .text_color(theme.text)
                                .child(format!("{}  {label}", if checked { "✓" } else { " " })),
                        )
                        .child(div().text_color(theme.text_muted).child(status)),
                );
            }
            if self.usage_environment_snapshots.is_empty() {
                environment_menu = environment_menu.child(
                    div()
                        .px(px(8.0))
                        .py(px(6.0))
                        .text_color(theme.text_muted)
                        .child("No environments connected."),
                );
            }
            environment_menu = environment_menu.child(
                div()
                    .id("usage-prices-menu")
                    .cursor_pointer()
                    .border_t_1()
                    .border_color(theme.border)
                    .px(px(8.0))
                    .py(px(7.0))
                    .text_color(theme.text_muted)
                    .text_size(crate::typography::ui_rems(12.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.usage_page_price_editor_open = true;
                        this.usage_environment_menu_open = false;
                        cx.notify();
                    }))
                    .child("Model prices"),
            );
        }

        let metric_control = div()
            .flex()
            .items_center()
            .rounded(px(10.0))
            .bg(theme.glass_hover().opacity(0.25))
            .p(px(2.0))
            .children(
                [("cost", "Cost"), ("tokens", "Tokens"), ("limits", "Limits")]
                    .into_iter()
                    .map(|(key, label)| {
                        div()
                            .id(SharedString::from(format!("usage-metric-{key}")))
                            .px(px(12.0))
                            .py(px(5.0))
                            .rounded(px(8.0))
                            .bg(if metric == key {
                                theme.glass_hover()
                            } else {
                                theme.glass_hover().opacity(0.0)
                            })
                            .text_color(if metric == key {
                                theme.text
                            } else {
                                theme.text_muted
                            })
                            .text_size(crate::typography::ui_rems(13.0))
                            .cursor_pointer()
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_usage_metric(key, cx)),
                            )
                            .child(label)
                    }),
            );
        let period_control = div()
            .flex()
            .items_center()
            .rounded(px(10.0))
            .bg(theme.glass_hover().opacity(0.25))
            .p(px(2.0))
            .children(
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
                        .px(px(if compact { 7.0 } else { 11.0 }))
                        .py(px(5.0))
                        .rounded(px(8.0))
                        .bg(if self.settings.usage_days == days {
                            theme.glass_hover()
                        } else {
                            theme.glass_hover().opacity(0.0)
                        })
                        .text_color(if limits_view {
                            theme.text_faint
                        } else if self.settings.usage_days == days {
                            theme.text
                        } else {
                            theme.text_muted
                        })
                        .text_size(crate::typography::ui_rems(13.0))
                        .when(!limits_view, |element| {
                            element.cursor_pointer().on_click(
                                cx.listener(move |this, _, _, cx| this.set_usage_days(days, cx)),
                            )
                        })
                        .child(label)
                }),
            );
        let header = div()
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(10.0))
            .child(
                div()
                    .id("usage-environment")
                    .size(px(29.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_color(theme.text_muted)
                    .tooltip(move |_, cx| {
                        cx.new(|_| super::SurfaceTabTooltip {
                            text: environment_label.clone().into(),
                        })
                        .into()
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.usage_environment_menu_open = !this.usage_environment_menu_open;
                        cx.notify();
                    }))
                    .child(
                        icon(icons::GLOBE)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .child(metric_control)
                    .child(period_control)
                    .child(
                        div()
                            .id("usage-refresh")
                            .size(px(29.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .text_color(theme.text_muted)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_usage(cx)))
                            .tooltip(|_, cx| {
                                cx.new(|_| super::SurfaceTabTooltip {
                                    text: "Refresh usage".into(),
                                })
                                .into()
                            })
                            .child(
                                icon(icons::REFRESH)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            ),
                    ),
            );

        let mut content = div().w_full().flex().flex_col().gap(px(26.0));
        if let Some(snapshot) = snapshot {
            let chats = &self.state.read(cx).chats;
            let title_of = |task: &str| {
                chats
                    .iter()
                    .find(|chat| chat.id == task)
                    .and_then(|chat| chat.title.clone())
            };
            // Provenance of any Claude windows taken from a session stream.
            let limit_note = snapshot
                .limits
                .iter()
                .find(|limit| {
                    limit.account_label.as_deref() == Some(luna_proto::STREAM_OBSERVED_LIMIT_LABEL)
                })
                .and_then(|limit| limit.detail.clone());
            if let Some(section) = live::live_sessions_section(
                &snapshot.live_sessions,
                title_of,
                limit_note,
                &theme,
                now_ms,
            ) {
                content = content.child(section);
            }
            if limits_view {
                content = content.child(pooled_limit_sections(&snapshot.limits, &theme, now_ms));
            } else {
                let buckets: Vec<&UsageBucket> = snapshot.summary.buckets.iter().collect();
                let mut mix = luna_proto::UsageTokens::default();
                let mut total_cost = 0.0;
                let mut records = 0_u64;
                let mut unpriced = 0_u64;
                let mut cache_savings = 0.0;
                let mut cache_savings_unknown = false;
                let mut provider_tokens = [0_u64; 3];
                let mut provider_cost = [0.0_f64; 3];
                let mut provider_unpriced = [0_u64; 3];
                let mut periods = BTreeMap::<i64, [f64; 3]>::new();
                let mut period_costs = BTreeMap::<i64, [f64; 3]>::new();
                let mut period_tokens = BTreeMap::<i64, u64>::new();
                let mut models =
                    BTreeMap::<(u8, String), (UsageProvider, f64, u64, u64, u64)>::new();
                for bucket in &buckets {
                    let index = match bucket.provider {
                        UsageProvider::Codex => 0,
                        UsageProvider::OpenCodeGo => 1,
                        UsageProvider::Claude => 2,
                    };
                    let cost = bucket_known_cost(bucket);
                    let tokens = bucket.tokens.total();
                    mix.add(bucket.tokens);
                    total_cost += cost;
                    records += bucket.records;
                    unpriced += bucket_unpriced_records(bucket);
                    provider_tokens[index] += tokens;
                    provider_cost[index] += cost;
                    provider_unpriced[index] += bucket_unpriced_records(bucket);
                    match bucket.cache_savings_usd {
                        Some(value) => cache_savings += value,
                        None if bucket.tokens.cached_input > 0 => cache_savings_unknown = true,
                        None => {}
                    }
                    periods.entry(bucket.start_ms).or_default()[index] +=
                        if tokens_view { tokens as f64 } else { cost };
                    period_costs.entry(bucket.start_ms).or_default()[index] += cost;
                    *period_tokens.entry(bucket.start_ms).or_default() += tokens;
                    let model = models
                        .entry((index as u8, bucket.model.clone()))
                        .or_insert((bucket.provider, 0.0, 0, 0, 0));
                    model.1 += cost;
                    model.2 += tokens;
                    model.3 += bucket.records;
                    model.4 += bucket_unpriced_records(bucket);
                }
                let total_tokens = mix.total();
                let sessions: u64 = snapshot
                    .summary
                    .sources
                    .iter()
                    .filter(|source| {
                        matches!(
                            source.status,
                            UsageSourceStatus::Ready | UsageSourceStatus::Partial
                        )
                    })
                    .map(|source| source.distinct_sessions)
                    .sum();
                let caption = if tokens_view {
                    format!("{} sessions", count(sessions))
                } else if records > 0 && unpriced > 0 {
                    format!(
                        "{} sessions · API estimate excludes {} unpriced records",
                        count(sessions),
                        usage_percent(unpriced as f64 / records as f64)
                    )
                } else {
                    format!("{} sessions · API estimate", count(sessions))
                };
                let providers = [
                    UsageProvider::Codex,
                    UsageProvider::OpenCodeGo,
                    UsageProvider::Claude,
                ];
                let mut left = div().min_w_0().flex().flex_col().gap(px(18.0));
                left = left.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(36.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(if tokens_view {
                                    compact_tokens(total_tokens)
                                } else if total_cost == 0.0 && unpriced > 0 {
                                    "Unpriced".into()
                                } else {
                                    money(total_cost)
                                }),
                        )
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted)
                                .child(caption),
                        ),
                );
                for (index, provider) in providers.into_iter().enumerate() {
                    if provider_tokens[index] == 0 && provider_cost[index] == 0.0 {
                        continue;
                    }
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
                    let amount = if tokens_view {
                        compact_tokens(provider_tokens[index])
                    } else if provider_cost[index] == 0.0 && provider_unpriced[index] > 0 {
                        "Unpriced".into()
                    } else {
                        money(provider_cost[index])
                    };
                    let share = if tokens_view {
                        if total_tokens == 0 {
                            0.0
                        } else {
                            provider_tokens[index] as f64 / total_tokens as f64
                        }
                    } else if total_cost == 0.0 {
                        0.0
                    } else {
                        provider_cost[index] / total_cost
                    };
                    left = left.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(9.0))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap(px(7.0))
                                            .child(
                                                div()
                                                    .size(px(8.0))
                                                    .rounded_full()
                                                    .bg(provider_tone(provider, &theme)),
                                            )
                                            .child(
                                                icon(provider_mark(provider))
                                                    .size(px(16.0))
                                                    .text_color(theme.text),
                                            )
                                            .child(
                                                div()
                                                    .text_color(theme.text)
                                                    .text_size(crate::typography::ui_rems(13.0))
                                                    .child(provider.label()),
                                            )
                                            .child(
                                                div()
                                                    .text_color(theme.text_muted)
                                                    .text_size(crate::typography::ui_rems(11.0))
                                                    .child(format!(
                                                        "{} sessions",
                                                        count(provider_sessions)
                                                    )),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_color(theme.text)
                                            .text_size(crate::typography::ui_rems(13.0))
                                            .child(amount),
                                    ),
                            )
                            .child(
                                div()
                                    .text_color(theme.text_muted)
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .child(if tokens_view {
                                        format!(
                                            "{} of tokens · {}",
                                            usage_percent(share),
                                            money(provider_cost[index])
                                        )
                                    } else if provider_cost[index] == 0.0
                                        && provider_unpriced[index] > 0
                                    {
                                        format!(
                                            "— of cost · {} tokens",
                                            compact_tokens(provider_tokens[index])
                                        )
                                    } else {
                                        format!(
                                            "{} of cost · {} tokens",
                                            usage_percent(share),
                                            compact_tokens(provider_tokens[index])
                                        )
                                    }),
                            ),
                    );
                }
                let starts = if snapshot.summary.period_starts_ms.is_empty() {
                    usage_period_window(self.settings.usage_days, now_ms).0
                } else {
                    snapshot.summary.period_starts_ms.clone()
                };
                let chart_periods: Vec<_> = starts
                    .into_iter()
                    .map(|start| (start, periods.get(&start).copied().unwrap_or_default()))
                    .collect();
                let hover_readout = self.usage_hover_period_start.and_then(|start| {
                    periods.get(&start).map(|values| {
                        format!(
                            "{} · Total {}",
                            short_period_label(start, hourly),
                            if tokens_view {
                                compact_tokens(values.iter().sum::<f64>().round() as u64)
                            } else {
                                money(values.iter().sum())
                            }
                        )
                    })
                });
                let hover_detail = self.usage_hover_period_start.and_then(|start| {
                    periods.get(&start).map(|values| {
                        let format = |value: f64| {
                            if tokens_view {
                                compact_tokens(value.round() as u64)
                            } else {
                                money(value)
                            }
                        };
                        let providers = [
                            ("Codex", values[0]),
                            ("OpenCode Go", values[1]),
                            ("Claude", values[2]),
                        ]
                        .into_iter()
                        .filter(|(_, value)| *value > 0.0)
                        .map(|(name, value)| format!("{name} {}", format(value)))
                        .collect::<Vec<_>>();
                        if providers.is_empty() {
                            format!("{} · No activity", hover_period_label(start, hourly))
                        } else {
                            format!(
                                "{} · {} · Total {}",
                                hover_period_label(start, hourly),
                                providers.join(" · "),
                                format(values.iter().sum())
                            )
                        }
                    })
                });
                let chart = div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .justify_between()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_color(theme.text)
                                    .text_size(crate::typography::ui_rems(14.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(format!(
                                        "{} {}",
                                        if hourly { "Hourly" } else { "Daily" },
                                        if tokens_view {
                                            "processed tokens"
                                        } else {
                                            "cost"
                                        }
                                    )),
                            )
                            .when_some(hover_readout, |row, readout| {
                                row.child(
                                    div()
                                        .text_color(theme.text_muted)
                                        .text_size(crate::typography::ui_rems(11.0))
                                        .child(readout),
                                )
                            }),
                    )
                    .child(usage_chart(
                        &theme,
                        chart_periods,
                        tokens_view,
                        hourly,
                        self.usage_hover_period_start,
                        cx,
                    ))
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .text_size(crate::typography::ui_rems(11.0))
                            .child(hover_detail.unwrap_or_else(|| {
                                if hourly {
                                    "Each point covers one hour. Hover for provider values.".into()
                                } else {
                                    "Each point covers one day. Hover for provider values.".into()
                                }
                            })),
                    );
                content = content.child(if compact {
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(22.0))
                        .child(left)
                        .child(chart)
                        .into_any_element()
                } else {
                    div()
                        .w_full()
                        .flex()
                        .gap(px(24.0))
                        .child(div().w(px(288.0)).flex_none().child(left))
                        .child(chart)
                        .into_any_element()
                });
                let metric = |label, value: String| {
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted)
                                .child(label),
                        )
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(15.0))
                                .text_color(theme.text)
                                .child(value),
                        )
                };
                content = content.child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(14.0))
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(14.0))
                                .text_color(theme.text)
                                .child("Totals"),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .flex_wrap()
                                .gap(px(14.0))
                                .child(metric("Processed tokens", compact_tokens(total_tokens)))
                                .child(metric("Cached input", compact_tokens(mix.cached_input)))
                                .child(metric("Uncached input", compact_tokens(mix.uncached_input)))
                                .child(metric("Output", compact_tokens(mix.output)))
                                .child(metric(
                                    "Cache savings",
                                    if cache_savings_unknown {
                                        format!("{} known", money(cache_savings))
                                    } else {
                                        money(cache_savings)
                                    },
                                )),
                        ),
                );
                let by_time = self.usage_breakdown_by_time;
                let breakdown_control = div()
                    .flex()
                    .rounded(px(10.0))
                    .bg(theme.glass_hover().opacity(0.25))
                    .p(px(2.0))
                    .children(
                        [
                            (false, "Model"),
                            (true, if hourly { "Hour" } else { "Day" }),
                        ]
                        .into_iter()
                        .map(|(time, label)| {
                            div()
                                .id(SharedString::from(format!(
                                    "usage-breakdown-{}",
                                    if time { "time" } else { "model" }
                                )))
                                .rounded(px(8.0))
                                .px(px(12.0))
                                .py(px(5.0))
                                .bg(if by_time == time {
                                    theme.glass_hover()
                                } else {
                                    theme.glass_hover().opacity(0.0)
                                })
                                .text_color(if by_time == time {
                                    theme.text
                                } else {
                                    theme.text_muted
                                })
                                .text_size(crate::typography::ui_rems(13.0))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.usage_breakdown_by_time = time;
                                    cx.notify();
                                }))
                                .child(label)
                        }),
                    );
                let mut breakdown = div().w_full().flex().flex_col().gap(px(12.0)).child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(14.0))
                                .text_color(theme.text)
                                .child("Breakdown"),
                        )
                        .child(breakdown_control),
                );
                if by_time {
                    let active: Vec<_> = [
                        UsageProvider::Codex,
                        UsageProvider::OpenCodeGo,
                        UsageProvider::Claude,
                    ]
                    .into_iter()
                    .enumerate()
                    .filter(|(index, _)| provider_tokens[*index] > 0)
                    .collect();
                    let column_width = 0.60 / (active.len() + 2) as f32;
                    let row = |label: String,
                               provider_values: Vec<String>,
                               total: String,
                               tokens: String,
                               header: bool| {
                        div()
                            .w_full()
                            .min_h(px(if header { 31.0 } else { 38.0 }))
                            .flex()
                            .items_center()
                            .border_b_1()
                            .border_color(theme.border.opacity(if header { 1.0 } else { 0.55 }))
                            .text_size(crate::typography::ui_rems(if header { 11.0 } else { 13.0 }))
                            .child(
                                div()
                                    .w(gpui::relative(0.40))
                                    .text_color(if header { theme.text_muted } else { theme.text })
                                    .child(label),
                            )
                            .children(provider_values.into_iter().map(|value| {
                                div()
                                    .w(gpui::relative(column_width))
                                    .text_align(gpui::TextAlign::Right)
                                    .text_color(theme.text_muted)
                                    .child(value)
                            }))
                            .child(
                                div()
                                    .w(gpui::relative(column_width))
                                    .text_align(gpui::TextAlign::Right)
                                    .text_color(if header { theme.text_muted } else { theme.text })
                                    .child(total),
                            )
                            .child(
                                div()
                                    .w(gpui::relative(column_width))
                                    .text_align(gpui::TextAlign::Right)
                                    .text_color(theme.text_muted)
                                    .child(tokens),
                            )
                    };
                    breakdown = breakdown.child(row(
                        if hourly { "Hour" } else { "Day" }.into(),
                        active
                            .iter()
                            .map(|(_, provider)| provider.label().to_string())
                            .collect(),
                        "Total".into(),
                        "Tokens".into(),
                        true,
                    ));
                    for start in periods.keys().rev() {
                        let token_count = period_tokens.get(start).copied().unwrap_or(0);
                        let costs = period_costs.get(start).copied().unwrap_or_default();
                        breakdown = breakdown.child(row(
                            short_period_label(*start, hourly),
                            active
                                .iter()
                                .map(|(index, _)| money(costs[*index]))
                                .collect(),
                            money(costs.iter().sum()),
                            compact_tokens(token_count),
                            false,
                        ));
                    }
                } else {
                    breakdown = breakdown.child(usage_table_row(
                        &theme,
                        [
                            "Model".into(),
                            "Cost".into(),
                            "Share".into(),
                            "Tokens".into(),
                        ],
                        true,
                        false,
                        None,
                    ));
                    let mut models: Vec<_> = models.into_iter().collect();
                    models.sort_by(|a, b| {
                        let left = a.1;
                        let right = b.1;
                        if tokens_view {
                            right
                                .2
                                .cmp(&left.2)
                                .then_with(|| right.1.total_cmp(&left.1))
                        } else {
                            right
                                .1
                                .total_cmp(&left.1)
                                .then_with(|| right.2.cmp(&left.2))
                        }
                    });
                    if models.is_empty() {
                        breakdown = breakdown.child(
                            div()
                                .w_full()
                                .py(px(20.0))
                                .text_align(gpui::TextAlign::Center)
                                .text_color(theme.text_muted)
                                .child("No activity in this window."),
                        );
                    }
                    for ((_, name), (provider, cost, tokens, records, missing)) in models {
                        let unknown = records > 0 && missing >= records;
                        breakdown = breakdown.child(div().id(SharedString::from(format!("usage-model-{}-{name}", provider.label())))
                            .tooltip(move |_, cx| cx.new(|_| super::SurfaceTabTooltip {
                                text: if missing > 0 { format!("{missing} unpriced records; priced cost shown where available").into() }
                                    else { "API-equivalent estimate".into() },
                            }).into())
                            .child(usage_table_row(&theme,
                                [name, if unknown { "Unpriced".into() } else { money(cost) },
                                 if unknown { "—".into() } else { usage_percent(if total_cost > 0.0 { cost / total_cost } else { 0.0 }) }, compact_tokens(tokens)], false, unknown, Some(provider))));
                    }
                }
                content = content.child(breakdown);
                if snapshot.summary.partial || !snapshot.summary.warnings.is_empty() {
                    content = content.child(div().w_full().flex().flex_col().gap(px(4.0))
                        .children(snapshot.summary.warnings.iter().cloned().chain(
                            snapshot.summary.partial.then(|| "Some environments reported partial history; totals may be incomplete.".to_string())
                        ).map(|warning| div().text_size(crate::typography::ui_rems(11.0)).text_color(theme.readable_tone(theme.warning, 1.0)).child(warning))));
                }
            }
        } else {
            content = content.child(div().text_color(theme.text_muted).child(
                if self.usage_refreshing {
                    "Loading usage…"
                } else {
                    "Connect an environment to see usage and limits."
                },
            ));
        }
        if let Some(error) = &self.usage_error {
            content = content.child(
                div()
                    .text_color(theme.readable_tone(theme.warning, 1.0))
                    .child(error.clone()),
            );
        }
        let price_open = self.usage_page_price_editor_open;
        let content = content.when(price_open, |page| {
            page.child(
                div()
                    .id("usage-model-prices-close")
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.usage_page_price_editor_open = false;
                        cx.notify();
                    }))
                    .child("Hide model prices"),
            )
            .child(self.render_usage_price_editor(&theme, cx))
        });
        div()
            .id("usage-page")
            .size_full()
            .relative()
            .overflow_y_scroll()
            .child(div().w_full().px(px(22.0)).py(px(12.0)).child(header))
            .child(
                div()
                    .w_full()
                    .max_w(px(1024.0))
                    .mx_auto()
                    .px(px(24.0))
                    .pt(px(22.0))
                    .pb(px(48.0))
                    .child(content),
            )
            .when(self.usage_environment_menu_open, |page| {
                page.child(
                    div()
                        .absolute()
                        .left(px(22.0))
                        .top(px(48.0))
                        .child(environment_menu),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_usage_card(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.usage_card_open {
            return div().into_any_element();
        }
        let palette = CompactUsagePalette::for_theme(theme);
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
                        palette.selected_rule
                    } else {
                        palette.idle_rule
                    })
                    .hover(|el| el.bg(palette.hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.usage_provider_filter = Some(provider);
                        cx.notify();
                    }))
                    .on_key_down(crate::swallow_activation_key)
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
                                        palette.opencode_icon
                                    } else if selected {
                                        palette.selected_icon
                                    } else {
                                        palette.idle_icon
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
                                        .bg(palette.dot),
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
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(palette.quiet)
                        .child("No verified account limits available"),
                );
            }
            for limit in limits {
                content = content.child(compact_limit_row(limit, now, palette));
            }
        } else {
            content = content.child(
                div()
                    .p(px(10.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(palette.quiet)
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
            .rounded(px(Theme::PANEL_RADIUS))
            .border_1()
            .border_color(palette.border)
            .bg(palette.card)
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
                            .bg(palette.environment)
                            .cursor_pointer()
                            .text_size(crate::typography::ui_rems(12.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(palette.environment_text)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.cycle_usage_environment(cx)),
                            )
                            .on_key_down(crate::swallow_activation_key)
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
                            .hover(|el| el.bg(palette.hover))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_usage(cx)))
                            .on_key_down(crate::swallow_activation_key)
                            .child(icon(icons::REFRESH).size(px(15.0)).text_color(
                                if self.usage_refreshing {
                                    palette.refresh_active
                                } else {
                                    palette.control_icon
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
                            .on_key_down(crate::swallow_activation_key)
                            .child(
                                icon(icons::ALT_ARROW_DOWN)
                                    .size(px(13.0))
                                    .text_color(palette.control_icon),
                            ),
                    ),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(palette.divider)
                    .child(content),
            )
            .when_some(self.usage_error.clone(), |el, error| {
                el.child(
                    div()
                        .px(px(10.0))
                        .pb(px(7.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(palette.status)
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
            .on_key_down(crate::swallow_activation_key)
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
            known_cost_usd: None,
            unpriced_records: None,
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
