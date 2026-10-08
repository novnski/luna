//! List view (A8): the narrow-window alternative to columns (also chosen
//! from the Board/List switch). Rows keep the column grouping, order,
//! selection and every card action: the same keys, the same menu, the same
//! engine operations. Rows are not drag sources; moves go through the menu
//! or the keyboard.
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use luna_proto::{BoardColumn, WorkItemView};

use super::card::{
    Tone, activity_age, attention_text, badge, card_label, phase_badge, provider_of,
};
use super::page::BoardPage;
use crate::icons;
use crate::theme::Theme;
use crate::typography::ui_rems;

impl BoardPage {
    pub(super) fn render_list(
        &mut self,
        theme: &Theme,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Column bounds belong to the Board view's drop targets.
        self.geometry.borrow_mut().columns.clear();
        let groups: Vec<(BoardColumn, Vec<WorkItemView>)> = {
            let model = self.model.read(cx);
            BoardColumn::ALL
                .into_iter()
                .map(|column| (column, model.column(column).cloned().collect()))
                .collect()
        };
        let sections: Vec<AnyElement> = groups
            .into_iter()
            .map(|(column, rows)| self.render_list_group(column, rows, theme, cx))
            .collect();
        div()
            .id("board-list")
            .role(gpui::Role::List)
            .aria_label("Board tickets")
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(14.0))
            .children(sections)
            .into_any_element()
    }

    fn render_list_group(
        &mut self,
        column: BoardColumn,
        rows: Vec<WorkItemView>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = rows.len();
        let ids: Vec<String> = rows.iter().map(|view| view.item.id.clone()).collect();
        let elements: Vec<AnyElement> = rows
            .iter()
            .map(|view| self.render_list_row(view, theme, cx))
            .collect();
        let geometry = self.geometry.clone();
        div()
            .id(SharedString::from(format!(
                "board-list-{}",
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
            .flex()
            .flex_col()
            .gap(ui_rems(6.0))
            .child(super::columns::column_header(column, count, theme))
            .child(
                div()
                    .on_children_prepainted(move |bounds, _, _| {
                        geometry
                            .borrow_mut()
                            .cards
                            .insert(column, ids.iter().cloned().zip(bounds).collect());
                    })
                    .flex()
                    .flex_col()
                    .rounded(px(crate::theme::radius::LG))
                    .border_1()
                    .border_color(theme.separator(true))
                    .overflow_hidden()
                    .children(elements),
            )
            .when(count == 0, |el| {
                el.child(
                    div()
                        .px(ui_rems(4.0))
                        .text_size(ui_rems(11.0))
                        .text_color(theme.readable_muted(0.7))
                        .child("No tickets"),
                )
            })
            .into_any_element()
    }

    fn render_list_row(
        &mut self,
        view: &WorkItemView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let item = &view.item;
        let id = item.id.clone();
        let focus = self.card_focus(&id, cx);
        let selected = self.selected.as_deref() == Some(id.as_str());
        let phase = phase_badge(view);
        let project = self.project_label_for(item, cx);
        let label = card_label(view, project.as_deref());
        let attention = attention_text(&view.attention);
        let age = activity_age(view.last_activity_ms);
        let provider = provider_of(view);
        let ring = theme.focus_ring();
        div()
            .id(SharedString::from(format!("board-row-{id}")))
            .role(gpui::Role::ListItem)
            .aria_label(SharedString::from(label))
            .aria_description(SharedString::from(phase.description))
            .aria_selected(selected)
            .track_focus(&focus)
            .tab_index(0)
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_x(ui_rems(10.0))
            .gap_y(ui_rems(4.0))
            .px(ui_rems(12.0))
            .py(ui_rems(8.0))
            .border_b_1()
            .border_color(theme.separator(false))
            .bg(if selected {
                theme.accent_wash
            } else {
                gpui::transparent_black()
            })
            .when(item.archived, |row| row.opacity(0.7))
            .cursor_pointer()
            .hover(|style| style.bg(theme.fill(crate::theme::FillTier::Quaternary)))
            .focus_visible(move |style| crate::theme::focus_ring(style, ring))
            .on_key_down(cx.listener({
                let id = id.clone();
                move |this, event: &KeyDownEvent, window, cx| {
                    if this.on_card_key(&id, event, window, cx) {
                        cx.stop_propagation();
                    }
                }
            }))
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, window, cx| {
                    this.select(&id, true, window, cx);
                    this.open_detail(id.clone(), window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let id = id.clone();
                    move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.select(&id, true, window, cx);
                        this.open_card_menu(&id, Some(event.position), window, cx);
                    }
                }),
            )
            .child(
                div()
                    .flex_none()
                    .w(ui_rems(64.0))
                    .text_size(ui_rems(11.0))
                    .text_color(theme.readable_muted(0.85))
                    .child(SharedString::from(item.key.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(ui_rems(160.0))
                    .flex()
                    .flex_col()
                    .gap(ui_rems(2.0))
                    .child(
                        div()
                            .truncate()
                            .text_size(ui_rems(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(item.title.clone())),
                    )
                    .when_some(attention, |el, text| {
                        el.child(badge(
                            theme,
                            Some(icons::DANGER_TRIANGLE),
                            text,
                            Tone::Caution,
                        ))
                    })
                    .when_some(project, |el, project| {
                        el.child(
                            div()
                                .truncate()
                                .text_size(ui_rems(11.0))
                                .text_color(theme.readable_muted(0.7))
                                .child(SharedString::from(project)),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(10.0))
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(theme.readable_muted(0.8))
                            .child(item.column.label()),
                    )
                    .child(badge(theme, phase.glyph, phase.label, phase.tone))
                    .when_some(provider, |el, harness| {
                        let (glyph, tint) = crate::pickers::harness_brand_icon(harness);
                        el.child(
                            icons::icon(glyph)
                                .size(ui_rems(12.0))
                                .flex_none()
                                .text_color(tint.unwrap_or(theme.readable_muted(0.85))),
                        )
                    })
                    .when_some(age, |el, age| {
                        el.child(
                            div()
                                .text_size(ui_rems(11.0))
                                .text_color(theme.readable_muted(0.75))
                                .child(SharedString::from(age)),
                        )
                    })
                    .child(
                        crate::button::icon_button(
                            format!("board-row-menu-{id}"),
                            icons::MORE_HORIZONTAL,
                            crate::button::ControlSize::Sm,
                            true,
                            theme,
                        )
                        .aria_label(format!("Actions for {}", item.key))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.select(&id, true, window, cx);
                                this.open_card_menu(&id, None, window, cx);
                            }
                        })),
                    ),
            )
            .into_any_element()
    }
}
