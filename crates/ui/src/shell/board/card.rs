//! Ticket card (A8). Foundation version: key, title and the engine's
//! observed phase. A8 adds attention/provider/PR indicators, the working
//! chat location, drag handles, selection and the detail drawer trigger.
use gpui::{AnyElement, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use luna_proto::WorkItemView;

use crate::theme::Theme;

pub(crate) fn render_card(view: &WorkItemView, theme: &Theme) -> AnyElement {
    let item = &view.item;
    div()
        .id(SharedString::from(format!("board-card-{}", item.id)))
        .w_full()
        .flex()
        .flex_col()
        .gap(crate::typography::ui_rems(4.0))
        .px(crate::typography::ui_rems(10.0))
        .py(crate::typography::ui_rems(8.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.separator(true))
        .bg(theme.fill(crate::theme::FillTier::Quaternary))
        .child(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .gap(crate::typography::ui_rems(6.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.readable_muted(0.8))
                .child(SharedString::from(item.key.clone()))
                .child(view.phase.label()),
        )
        .child(
            div()
                .min_w_0()
                .text_size(crate::typography::ui_rems(13.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(SharedString::from(item.title.clone())),
        )
        .into_any_element()
}
