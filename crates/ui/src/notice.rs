//! In-context composer notice for an unresolved action or unsaved draft.

use gpui::{Div, FontWeight, SharedString, div, prelude::*, px};

use crate::theme::Theme;

/// Keep actionable feedback beside the composer. The caller owns the notice's
/// lifetime and action; this component only supplies the shared presentation.
pub fn notice_chip(
    theme: &Theme,
    warning: bool,
    label: &'static str,
    message: impl Into<SharedString>,
) -> Div {
    let (accent, muted) = if warning {
        (theme.warning, theme.warning_muted)
    } else {
        (theme.danger, theme.danger_muted)
    };
    let message = message.into();
    let copy_message = message.clone();
    // The notice floats in the composer dock above scrolling transcript rows,
    // so its plate is opaque: a light status tint flattened over the canvas as
    // composited. A translucent tint let message text print through it.
    let canvas = crate::theme::flatten(theme.canvas_fill(), theme.chrome_surface());
    let plate = crate::theme::flatten(accent.opacity(0.08), canvas);
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(accent.opacity(0.22))
        .bg(plate)
        .px(px(10.0))
        .py(px(8.0))
        .text_size(crate::typography::ui_rems(12.0))
        .line_height(crate::typography::ui_rems(16.0))
        .text_color(theme.readable_tone(muted, 0.9))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                        .size(px(14.0))
                        .text_color(theme.readable_tone(muted, 0.9)),
                )
                .child(div().font_weight(FontWeight::MEDIUM).child(label))
                .child(div().flex_1())
                .child(
                    div()
                        .id(SharedString::from(format!("notice-copy-{label}")))
                        .flex_none()
                        .size(px(20.0))
                        .rounded(px(6.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(move |style| style.bg(accent.opacity(0.12)))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                copy_message.to_string(),
                            ));
                        })
                        .child(
                            crate::icons::icon(crate::icons::COPY)
                                .size(px(12.0))
                                .text_color(theme.readable_tone(muted, 0.8)),
                        ),
                ),
        )
        .child(div().min_w_0().w_full().child(message))
}
