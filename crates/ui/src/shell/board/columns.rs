//! Board columns (A8): Backlog / Ready / Done side by side, each with its
//! count and short help ("Held or stopped", "Runs automatically"), its cards
//! in rank order and a drop target. A drop between columns is one
//! `BoardMove`; a drop within a column is one `BoardRank`; Done accepts no
//! drop from another column (it is set by the engine, never by a drag).
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use luna_proto::{BoardColumn, WorkItemView};

use super::page::{BoardCardDrag, BoardPage};
use crate::theme::Theme;
use crate::typography::ui_rems;

/// Narrowest a column gets before the page switches to the List view.
pub(super) const COLUMN_MIN_WIDTH: f32 = 220.0;

impl BoardPage {
    pub(super) fn render_columns(
        &mut self,
        theme: &Theme,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let columns: Vec<(BoardColumn, Vec<WorkItemView>)> = {
            let model = self.model.read(cx);
            BoardColumn::ALL
                .into_iter()
                .map(|column| (column, model.column(column).cloned().collect()))
                .collect()
        };
        let elements: Vec<AnyElement> = columns
            .into_iter()
            .map(|(column, cards)| self.render_column(column, cards, theme, cx))
            .collect();
        let geometry = self.geometry.clone();
        div()
            .on_children_prepainted(move |bounds, _, _| {
                geometry.borrow_mut().columns = BoardColumn::ALL.into_iter().zip(bounds).collect();
            })
            .id("board-columns")
            .role(gpui::Role::Group)
            .aria_label("Board columns")
            .w_full()
            .flex()
            .flex_row()
            .items_start()
            .gap(ui_rems(12.0))
            .children(elements)
            .into_any_element()
    }

    fn render_column(
        &mut self,
        column: BoardColumn,
        cards: Vec<WorkItemView>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = cards.len();
        let drop_slot = self
            .drag_over
            .as_ref()
            .filter(|target| target.column == column)
            .map(|target| target.index);
        let dragging = self.dragging.clone();
        let mut slot = 0;
        let mut elements = Vec::with_capacity(count);
        let ids: Vec<String> = cards.iter().map(|view| view.item.id.clone()).collect();
        for view in &cards {
            let is_dragged = dragging.as_deref() == Some(view.item.id.as_str());
            let line = !is_dragged && drop_slot == Some(slot);
            if !is_dragged {
                slot += 1;
            }
            elements.push(self.render_card(view, line, theme, cx));
        }
        let end_line = drop_slot == Some(slot) && drop_slot.is_some();
        let refuses = column == BoardColumn::Done;
        let geometry = self.geometry.clone();
        let list = div()
            .on_children_prepainted(move |bounds, _, _| {
                geometry
                    .borrow_mut()
                    .cards
                    .insert(column, ids.iter().cloned().zip(bounds).collect());
            })
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(8.0))
            .children(elements);
        let accent_wash = theme.accent_wash;
        let refuse_wash = theme.danger.opacity(0.06);
        let empty_text = match column {
            BoardColumn::Backlog => "Drop a ticket here to hold or stop it",
            BoardColumn::Ready => "Drop a ticket here to start it",
            BoardColumn::Done => "Tickets arrive here when checks and review pass",
        };
        div()
            .id(SharedString::from(format!(
                "board-column-{}",
                column.as_str()
            )))
            .role(gpui::Role::Group)
            .aria_label(SharedString::from(format!(
                "{} — {}, {} ticket{}",
                column.label(),
                column.help().to_lowercase(),
                count,
                if count == 1 { "" } else { "s" }
            )))
            .flex_1()
            .min_w(ui_rems(COLUMN_MIN_WIDTH))
            .flex()
            .flex_col()
            .gap(ui_rems(8.0))
            .p(ui_rems(8.0))
            .rounded(px(crate::theme::radius::LG))
            .bg(theme.fill(crate::theme::FillTier::Quaternary).opacity(0.5))
            .drag_over::<BoardCardDrag>(move |style, drag, _, _| {
                if refuses && drag.from != BoardColumn::Done {
                    style.bg(refuse_wash)
                } else {
                    style.bg(accent_wash)
                }
            })
            .on_drop(cx.listener(move |this, drag: &BoardCardDrag, _, cx| {
                this.drop_card(drag, column, cx);
            }))
            .child(column_header(column, count, theme))
            .child(list)
            .when(end_line, |el| el.child(super::card::drop_indicator(theme)))
            .when(count == 0, |el| {
                el.child(
                    div()
                        .px(ui_rems(4.0))
                        .py(ui_rems(14.0))
                        .text_size(ui_rems(11.0))
                        .line_height(ui_rems(15.0))
                        .text_color(theme.readable_muted(0.7))
                        .child(empty_text),
                )
            })
            .into_any_element()
    }
}

/// A column's title, count and help.
pub(super) fn column_header(column: BoardColumn, count: usize, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(ui_rems(2.0))
        .px(ui_rems(2.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(ui_rems(6.0))
                .text_size(ui_rems(12.0))
                .line_height(ui_rems(16.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(super::card::column_mark(column, theme))
                .child(column.label())
                .child(
                    div()
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme.readable_muted(0.7))
                        .child(SharedString::from(count.to_string())),
                ),
        )
        .child(
            div()
                .text_size(ui_rems(11.0))
                .line_height(ui_rems(14.0))
                .text_color(theme.readable_muted(0.8))
                .child(column.help()),
        )
}
