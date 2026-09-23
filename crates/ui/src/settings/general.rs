//! Device-local task behavior preferences shown at the Settings entry point.

use gpui::{Context, EventEmitter, SharedString, Window, div, prelude::*, px};
use zeron_proto::FollowUpBehavior;

use crate::popover::{self, ScrollRailHost};
use crate::settings::widgets;
use crate::theme::Theme;

#[derive(Debug, Clone, Copy)]
pub enum GeneralEvent {
    FollowUpBehaviorChanged(FollowUpBehavior),
    AutomaticRetriesChanged(bool),
}

pub struct GeneralPage {
    scroll: widgets::PageScroll,
    follow_up_behavior: FollowUpBehavior,
    automatic_retries_enabled: bool,
}

impl EventEmitter<GeneralEvent> for GeneralPage {}

impl GeneralPage {
    pub fn new(
        follow_up_behavior: FollowUpBehavior,
        automatic_retries_enabled: bool,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            scroll: widgets::PageScroll::default(),
            follow_up_behavior,
            automatic_retries_enabled,
        }
    }

    fn set_follow_up_behavior(&mut self, behavior: FollowUpBehavior, cx: &mut Context<Self>) {
        if self.follow_up_behavior != behavior {
            self.follow_up_behavior = behavior;
            cx.emit(GeneralEvent::FollowUpBehaviorChanged(behavior));
            cx.notify();
        }
    }

    fn set_automatic_retries(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.automatic_retries_enabled != enabled {
            self.automatic_retries_enabled = enabled;
            cx.emit(GeneralEvent::AutomaticRetriesChanged(enabled));
            cx.notify();
        }
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl ScrollRailHost for GeneralPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for GeneralPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let accent = theme.accent;
        let follow_up_behavior = self.follow_up_behavior;
        let automatic_retries_enabled = self.automatic_retries_enabled;

        let follow_up_row = widgets::card_row(&theme, true)
            .min_h(px(84.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(&theme, "Follow-up behavior"))
                    .child(
                        div()
                            .mt(px(4.0))
                            .max_w(px(430.0))
                            .text_size(px(11.5))
                            .line_height(px(17.0))
                            .text_color(theme.text_muted.opacity(0.65))
                            .child(SharedString::from(
                                "Choose what sending a message does during a live run. Cmd/Ctrl+Shift+Enter performs the opposite once; Shift+Enter remains a line break.",
                            )),
                    ),
            )
            .child(
                div()
                    .id("general-follow-up-behavior")
                    .flex()
                    .flex_row()
                    .rounded(px(9.0))
                    .p(px(2.0))
                    .bg(crate::theme::ink(0.04))
                    .children(
                        [(FollowUpBehavior::Queue, "Queue"), (FollowUpBehavior::Steer, "Steer")]
                            .into_iter()
                            .enumerate()
                            .map(|(ix, (behavior, label))| {
                                let selected = follow_up_behavior == behavior;
                                div()
                                    .id(("general-follow-up-option", ix))
                                    .role(gpui::Role::Button)
                                    .aria_label(format!("Follow-up behavior: {label}"))
                                    .aria_toggled(if selected { gpui::Toggled::True } else { gpui::Toggled::False })
                                    .tab_index(0)
                                    .focus_visible(move |style| style.border_2().border_color(accent))
                                    .min_w(px(72.0))
                                    .px(px(12.0))
                                    .py(px(6.0))
                                    .rounded(px(7.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(12.0))
                                    .text_color(if selected { theme.text } else { theme.text_muted })
                                    .when(selected, |el| el.bg(theme.bg).border_1().border_color(theme.border.opacity(0.8)))
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(theme.text))
                                    .on_click(cx.listener(move |this, _, _, cx| this.set_follow_up_behavior(behavior, cx)))
                                    .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                                        if !event.is_held && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                            cx.stop_propagation();
                                            this.set_follow_up_behavior(behavior, cx);
                                        }
                                    }))
                                    .child(label)
                            }),
                    ),
            );

        let retries_row = widgets::card_row(&theme, false)
            .min_h(px(84.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(&theme, "Automatic retries"))
                    .child(
                        div()
                            .mt(px(4.0))
                            .max_w(px(430.0))
                            .text_size(px(11.5))
                            .line_height(px(17.0))
                            .text_color(theme.text_muted.opacity(0.65))
                            .child(SharedString::from(
                                "Retry temporary overloads and known subscription resets within four attempts and six hours. Unknown delivery, authentication, credits, model, and configuration failures still require review.",
                            )),
                    ),
            )
            .child(
                div()
                    .id("general-automatic-retries-toggle")
                    .role(gpui::Role::Switch)
                    .aria_label("Automatic retries")
                    .aria_toggled(if automatic_retries_enabled { gpui::Toggled::True } else { gpui::Toggled::False })
                    .tab_index(0)
                    .focus_visible(move |style| style.border_2().border_color(accent))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.set_automatic_retries(!automatic_retries_enabled, cx)))
                    .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                        if !event.is_held && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            cx.stop_propagation();
                            this.set_automatic_retries(!automatic_retries_enabled, cx);
                        }
                    }))
                    .child(widgets::toggle_switch(&theme, automatic_retries_enabled)),
            );

        let scrollbar = popover::rail(self, "general-page-scrollbar", &theme, cx);
        div()
            .id("general-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("general-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_header(&theme, "General", None))
                            .child(
                                widgets::page_subtitle(&theme, "Choose how LUNA handles messages and temporary failures while an agent runs. These preferences stay on this device.")
                                    .max_w(px(512.0))
                                    .line_height(px(20.0)),
                            )
                            .child(widgets::section_card(&theme).child(follow_up_row).child(retries_row)),
                    ),
            )
            .children(scrollbar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn general_page_renders_existing_preferences_and_changes_them(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let window = cx.add_window(|_, cx| GeneralPage::new(FollowUpBehavior::Queue, true, cx));
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();
        window
            .update(cx, |page, _, cx| {
                page.set_follow_up_behavior(FollowUpBehavior::Steer, cx);
                page.set_automatic_retries(false, cx);
                assert_eq!(page.follow_up_behavior, FollowUpBehavior::Steer);
                assert!(!page.automatic_retries_enabled);
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();
    }
}
