//! Board columns (A8). Foundation version: Backlog / Ready / Done side by
//! side with their help text and counts, each listing its tickets in rank
//! order. A8 adds drop targets, reorder, keyboard/menu moves and focus
//! preservation across live updates.
use gpui::{AnyElement, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use luna_proto::BoardColumn;

use super::model::BoardModel;
use crate::theme::Theme;

pub(crate) fn render_columns(model: &BoardModel, theme: &Theme) -> AnyElement {
    div()
        .id("board-columns")
        .w_full()
        .flex()
        .flex_row()
        .items_start()
        .gap(crate::typography::ui_rems(12.0))
        .children(
            BoardColumn::ALL
                .into_iter()
                .map(|column| render_column(model, column, theme)),
        )
        .into_any_element()
}

fn render_column(model: &BoardModel, column: BoardColumn, theme: &Theme) -> AnyElement {
    let cards: Vec<AnyElement> = model
        .column(column)
        .map(|view| super::card::render_card(view, theme))
        .collect();
    let count = cards.len();
    div()
        .id(SharedString::from(format!(
            "board-column-{}",
            column.as_str()
        )))
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(crate::typography::ui_rems(8.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .gap(crate::typography::ui_rems(6.0))
                .text_size(crate::typography::ui_rems(12.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(column.label())
                .child(
                    div()
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(theme.readable_muted(0.7))
                        .child(SharedString::from(count.to_string())),
                ),
        )
        .child(
            div()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.readable_muted(0.8))
                .child(column.help()),
        )
        .children(cards)
        .child(div().h(px(1.0)))
        .into_any_element()
}
