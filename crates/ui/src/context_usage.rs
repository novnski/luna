//! Context occupancy is read from the replicated chat snapshot, never local CLI state.
use crate::theme::Theme;
use gpui::{
    Context, IntoElement, PathBuilder, Render, SharedString, Window, canvas, div, point,
    prelude::*, px,
};
use luna_proto::{ContextOperationStatus, ContextUsage};

/// The chip's state tone before flooring: compaction running (accent) or
/// unknown (warning), else by occupancy (danger from 90%, warning from 75%,
/// muted below), faint with nothing reported.
pub fn tone(
    compact_status: ContextOperationStatus,
    fraction: Option<f64>,
    theme: &Theme,
) -> gpui::Hsla {
    match (compact_status, fraction) {
        (ContextOperationStatus::Unknown, _) => theme.warning,
        (ContextOperationStatus::Running, _) => theme.accent,
        (_, Some(f)) if f >= 0.9 => theme.danger,
        (_, Some(f)) if f >= 0.75 => theme.warning,
        (_, Some(_)) => theme.text_muted,
        (_, None) => theme.text_faint,
    }
}

/// The chip's text: the occupied share, or the compaction state.
pub fn label(usage: Option<ContextUsage>, compact_status: ContextOperationStatus) -> String {
    let fraction = usage.and_then(ContextUsage::fraction);
    match compact_status {
        ContextOperationStatus::Running => "Compacting…".into(),
        ContextOperationStatus::Unknown => "Context?".into(),
        _ => fraction
            .map(|f| format!("{:.0}%", f * 100.0))
            .unwrap_or_else(|| "—".into()),
    }
}

const CHIP_PAD_X: f32 = 6.0;
const RING: f32 = 16.0;
const RING_GAP: f32 = 5.0;

/// The chip's `(with label, ring only)` widths in the composer's controls
/// row, so the row can give the label up before it truncates anything.
pub fn chip_widths(label: &str, window: &gpui::Window, theme: &Theme) -> (f32, f32) {
    let text = crate::pickers::chip_text_width(label, window, theme);
    (
        2.0 * CHIP_PAD_X + RING + RING_GAP + text,
        2.0 * CHIP_PAD_X + RING,
    )
}

/// The context-window chip in the composer's controls row: a ring and,
/// when the row has room (`show_label`), the share; the details card on
/// hover either way.
pub fn render(
    usage: Option<ContextUsage>,
    compact_status: ContextOperationStatus,
    state: gpui::Entity<crate::state::AppState>,
    binding: crate::chat_binding::ChatBinding,
    theme: &Theme,
    show_label: bool,
) -> gpui::Stateful<gpui::Div> {
    let fraction = usage.and_then(ContextUsage::fraction);
    let tone = tone(compact_status, fraction, theme);
    // On the composer pill, floored for the share (text) and the ring
    // (non-text), keeping each state's hue.
    let color = theme.readable_on_composer(tone, crate::theme::TEXT_CONTRAST_TARGET);
    let ring_color = theme.readable_on_composer(tone, crate::theme::NON_TEXT_CONTRAST_TARGET);
    let track = theme.text_faint.opacity(0.25);
    let ring = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let mut arc = |fraction: f32, color| {
                if fraction <= 0.0 {
                    return;
                }
                let steps = (64.0 * fraction).ceil().max(2.0) as usize;
                let mut path = PathBuilder::stroke(px(1.8));
                for i in 0..=steps {
                    let angle = -std::f32::consts::FRAC_PI_2
                        + std::f32::consts::TAU * fraction * i as f32 / steps as f32;
                    let p = point(
                        center.x + px(6.0 * angle.cos()),
                        center.y + px(6.0 * angle.sin()),
                    );
                    if i == 0 {
                        path.move_to(p);
                    } else {
                        path.line_to(p);
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            arc(1.0, track);
            arc(fraction.unwrap_or(0.0).clamp(0.0, 1.0) as f32, ring_color);
        },
    )
    .size(px(RING));
    let label = label(usage, compact_status);
    let accessible: SharedString = match compact_status {
        ContextOperationStatus::Running => "Context window: compacting".into(),
        ContextOperationStatus::Unknown => "Context window: state unknown".into(),
        _ => format!("Context window: {label} used").into(),
    };
    div()
        .id("context-usage")
        .role(gpui::Role::Status)
        .aria_label(accessible)
        .flex_none()
        .flex()
        .items_center()
        .gap(px(RING_GAP))
        .h(px(24.0))
        .px(px(CHIP_PAD_X))
        .rounded(px(6.0))
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(color)
        .hover(|s| s.bg(crate::theme::ink(0.05)))
        .child(ring)
        .when(show_label, |el| el.child(SharedString::from(label)))
        .tooltip(move |_, cx| {
            cx.new(|cx| UsageCard {
                _subscription: cx.observe(&state, |_, _, cx| cx.notify()),
                state: state.clone(),
                binding: binding.clone(),
            })
            .into()
        })
}

struct UsageCard {
    state: gpui::Entity<crate::state::AppState>,
    /// The chat whose usage this card describes (selected or pane-bound).
    binding: crate::chat_binding::ChatBinding,
    _subscription: gpui::Subscription,
}

fn with_separators(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

fn details(usage: Option<ContextUsage>) -> String {
    match usage.unwrap_or_default() {
        ContextUsage {
            tokens: Some(tokens),
            window: Some(window),
        } if window > 0 => {
            format!(
                "{} / {} tokens\n{} tokens remaining",
                with_separators(tokens),
                with_separators(window),
                with_separators(window.saturating_sub(tokens))
            )
        }
        ContextUsage {
            tokens: Some(tokens),
            ..
        } => format!(
            "{} tokens used\nContext limit not reported",
            with_separators(tokens)
        ),
        ContextUsage {
            window: Some(window),
            ..
        } if window > 0 => format!(
            "{} token capacity\nWaiting for context usage",
            with_separators(window)
        ),
        _ => "Context usage not reported by this harness yet".into(),
    }
}

impl Render for UsageCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &Theme::of(cx).for_popup();
        let card = crate::popover::popover_card(theme)
            .bg(theme.tooltip_surface())
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(12.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child("Context window"),
            )
            .child(
                // the lines break only at their own newlines: a tooltip sizes
                // from the unwrapped text, so soft wrapping clipped the last line
                div()
                    .text_size(crate::typography::ui_rems(12.0))
                    .line_height(crate::typography::ui_rems(19.0))
                    .whitespace_nowrap()
                    .text_color(theme.text_muted)
                    .child(SharedString::from(details(
                        self.binding.context_usage(self.state.read(cx)),
                    ))),
            )
            .child({
                let context = self.binding.context_control(self.state.read(cx));
                let mut detail = context
                    .compact
                    .detail
                    .clone()
                    .or_else(|| context.notice.clone())
                    .unwrap_or_else(|| "No provider context operation is pending.".into());
                detail.push_str(
                    "\nContext pressure is separate from account quota, rate limits, and cost.",
                );
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .line_height(crate::typography::ui_rems(17.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(detail))
            });
        crate::frost::frosted(crate::popover::CARD_RADIUS, crate::frost::MENU_BLUR, card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_usage_is_distinct_from_zero_and_overflow() {
        assert!(details(None).contains("not reported"));
        assert!(
            details(Some(ContextUsage {
                tokens: Some(0),
                window: Some(200)
            }))
            .contains("200 tokens remaining")
        );
        assert!(
            details(Some(ContextUsage {
                tokens: Some(250),
                window: Some(200)
            }))
            .contains("0 tokens remaining")
        );
        assert!(
            details(Some(ContextUsage {
                tokens: Some(10),
                window: Some(0)
            }))
            .contains("limit not reported")
        );
    }

    #[test]
    fn token_counts_are_grouped_by_thousands() {
        assert_eq!(with_separators(0), "0");
        assert_eq!(with_separators(999), "999");
        assert_eq!(with_separators(5417), "5,417");
        assert_eq!(with_separators(1_048_576), "1,048,576");
        assert_eq!(
            details(Some(ContextUsage {
                tokens: Some(5417),
                window: Some(1_048_576)
            })),
            "5,417 / 1,048,576 tokens\n1,043,159 tokens remaining"
        );
    }
}
