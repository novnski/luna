//! Shared metrics for sidebar surface toolbars and their controls.

use gpui::{Div, div, prelude::*, px};

use crate::theme::Theme;

pub(crate) const HEADER_HEIGHT: f32 = Theme::TITLEBAR_HEIGHT;
pub(crate) const CONTROL_SIZE: f32 = 24.0;
pub(crate) const CONTROL_RADIUS: f32 = 6.0;
pub(crate) const ICON_SIZE: f32 = 14.0;
pub(crate) const CONTROL_GAP: f32 = 4.0;
pub(crate) const EDGE_INSET: f32 = 8.0;

/// Shared field treatment for the file search and browser address controls.
/// Where every surface's content starts (audit SS-14): toolbar text and
/// icon glyphs (which carry 4 pt of internal air) and body content share it.
pub(crate) const CONTENT_INSET: f32 = 12.0;

pub(crate) fn input() -> Div {
    div()
        .h(px(CONTROL_SIZE))
        .min_w_0()
        .flex_1()
        .px(px(8.0))
        .rounded(px(CONTROL_RADIUS))
        .bg(crate::theme::ink(0.035))
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(crate::typography::ui_rems(12.0))
}

/// A sidebar surface's control row. It sits on the shared chrome surface
/// (the shell frost every sidebar, header and footer uses): no band of its
/// own and no border stack. Spacing and alignment group the controls; the
/// row height matches the workbench tab strip so both sidebars' second rows
/// line up with the canvas. Separators belong to real list/diff structure.
pub(crate) fn toolbar(theme: &Theme) -> Div {
    let _ = theme;
    div()
        .h(px(HEADER_HEIGHT))
        .w_full()
        .flex_none()
        .px(px(EDGE_INSET))
        .flex()
        .items_center()
        .gap(px(CONTROL_GAP))
}

/// One empty / loading / error recipe for every surface (audit SS-12): a
/// 28 pt glyph in the faint tone (or the loader), a 13 pt medium title, a
/// 12 pt secondary line capped at 280 pt, and an optional action under it.
pub(crate) fn surface_state(
    glyph: gpui::AnyElement,
    title: impl Into<gpui::SharedString>,
    body: Option<gpui::SharedString>,
    action: Option<gpui::AnyElement>,
    theme: &Theme,
) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .px(px(CONTENT_INSET))
        .child(div().mb(px(4.0)).child(glyph))
        .child(
            div()
                .text_size(crate::typography::ui_rems(13.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(title.into()),
        )
        .when_some(body, |el, body| {
            el.child(
                div()
                    .max_w(px(280.0))
                    .text_center()
                    .text_size(crate::typography::ui_rems(12.0))
                    .line_height(crate::typography::ui_rems(18.0))
                    .text_color(theme.text_muted)
                    .child(body),
            )
        })
        .when_some(action, |el, action| {
            el.child(div().mt(px(6.0)).child(action))
        })
}

/// The surface-state glyph: a 28 pt icon in the faint tone.
pub(crate) fn state_glyph(icon: &'static str, theme: &Theme) -> gpui::AnyElement {
    crate::icons::icon(icon)
        .size(px(28.0))
        .text_color(theme.text_faint)
        .into_any_element()
}
