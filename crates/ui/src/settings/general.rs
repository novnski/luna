//! Device-local task behavior preferences shown at the Settings entry point.

use gpui::{Context, EventEmitter, SharedString, Window, div, prelude::*, px};
use luna_proto::FollowUpBehavior;

use crate::popover::{self, ScrollRailHost};
use crate::settings::{ComposerSendBehavior, TaskConfirmation, UiSettings, widgets};
use crate::theme::Theme;

#[derive(Debug, Clone, Copy)]
pub enum GeneralEvent {
    ComposerSendBehaviorChanged(ComposerSendBehavior),
    FollowUpBehaviorChanged(FollowUpBehavior),
    AutomaticRetriesChanged(bool),
    ShowContextUsageChanged(bool),
    OpenWebLinksInLunaChanged(bool),
    KeepAwakeWhileRunningChanged(bool),
    TaskConfirmationChanged(TaskConfirmation, bool),
}

#[derive(Clone, Copy)]
enum GeneralToggle {
    ContextUsage,
    OpenLinks,
    KeepAwake,
    AutomaticRetries,
    TaskConfirmation(TaskConfirmation),
}

pub struct GeneralPage {
    scroll: widgets::PageScroll,
    composer_send_behavior: ComposerSendBehavior,
    follow_up_behavior: FollowUpBehavior,
    automatic_retries_enabled: bool,
    show_context_usage: bool,
    open_web_links_in_luna: bool,
    keep_awake_while_running: bool,
    confirm_archive: bool,
    confirm_settle: bool,
    confirm_delete: bool,
}

impl EventEmitter<GeneralEvent> for GeneralPage {}

impl GeneralPage {
    pub fn new(settings: &UiSettings, _cx: &mut Context<Self>) -> Self {
        Self {
            scroll: widgets::PageScroll::default(),
            composer_send_behavior: settings.composer_send_behavior,
            follow_up_behavior: settings.follow_up_behavior,
            automatic_retries_enabled: settings.automatic_retries_enabled,
            show_context_usage: settings.show_context_usage,
            open_web_links_in_luna: settings.open_web_links_in_luna,
            keep_awake_while_running: settings.keep_awake_while_running,
            confirm_archive: settings.confirm_archive,
            confirm_settle: settings.confirm_settle,
            confirm_delete: settings.confirm_delete,
        }
    }

    /// Shortcuts and transcript link menus can edit these values too.
    pub fn sync(&mut self, settings: &UiSettings, cx: &mut Context<Self>) {
        if self.composer_send_behavior != settings.composer_send_behavior
            || self.open_web_links_in_luna != settings.open_web_links_in_luna
            || self.confirm_archive != settings.confirm_archive
            || self.confirm_settle != settings.confirm_settle
            || self.confirm_delete != settings.confirm_delete
        {
            self.composer_send_behavior = settings.composer_send_behavior;
            self.open_web_links_in_luna = settings.open_web_links_in_luna;
            self.confirm_archive = settings.confirm_archive;
            self.confirm_settle = settings.confirm_settle;
            self.confirm_delete = settings.confirm_delete;
            cx.notify();
        }
    }

    fn set_composer_send_behavior(
        &mut self,
        behavior: ComposerSendBehavior,
        cx: &mut Context<Self>,
    ) {
        if self.composer_send_behavior != behavior {
            self.composer_send_behavior = behavior;
            cx.emit(GeneralEvent::ComposerSendBehaviorChanged(behavior));
            cx.notify();
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

    fn toggle(&mut self, toggle: GeneralToggle, cx: &mut Context<Self>) {
        match toggle {
            GeneralToggle::ContextUsage => {
                self.show_context_usage = !self.show_context_usage;
                cx.emit(GeneralEvent::ShowContextUsageChanged(
                    self.show_context_usage,
                ));
            }
            GeneralToggle::OpenLinks => {
                self.open_web_links_in_luna = !self.open_web_links_in_luna;
                cx.emit(GeneralEvent::OpenWebLinksInLunaChanged(
                    self.open_web_links_in_luna,
                ));
            }
            GeneralToggle::KeepAwake => {
                self.keep_awake_while_running = !self.keep_awake_while_running;
                cx.emit(GeneralEvent::KeepAwakeWhileRunningChanged(
                    self.keep_awake_while_running,
                ));
            }
            GeneralToggle::AutomaticRetries => {
                self.set_automatic_retries(!self.automatic_retries_enabled, cx);
                return;
            }
            GeneralToggle::TaskConfirmation(action) => {
                let enabled = match action {
                    TaskConfirmation::Archive => &mut self.confirm_archive,
                    TaskConfirmation::Settle => &mut self.confirm_settle,
                    TaskConfirmation::Delete => &mut self.confirm_delete,
                };
                *enabled = !*enabled;
                cx.emit(GeneralEvent::TaskConfirmationChanged(action, *enabled));
            }
        }
        cx.notify();
    }

    fn switch_row(
        &self,
        theme: &Theme,
        toggle: GeneralToggle,
        first: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let (title, description, enabled, id) = match toggle {
            GeneralToggle::ContextUsage => (
                "Show context window usage",
                "Show the context indicator beside the conversation composer.",
                self.show_context_usage,
                "general-context-usage-toggle",
            ),
            GeneralToggle::OpenLinks => (
                "Open web links in LUNA",
                "Open conversation links in LUNA's browser; turn off to use your default browser.",
                self.open_web_links_in_luna,
                "general-open-links-toggle",
            ),
            GeneralToggle::KeepAwake => (
                "Prevent sleep while running",
                "Keep this Mac awake while a local task is working. The assertion ends when work stops or LUNA closes.",
                self.keep_awake_while_running,
                "general-keep-awake-toggle",
            ),
            GeneralToggle::AutomaticRetries => (
                "Automatic retries",
                "Retry temporary overloads and known subscription resets within four attempts and six hours. Other failures still require review.",
                self.automatic_retries_enabled,
                "general-automatic-retries-toggle",
            ),
            GeneralToggle::TaskConfirmation(TaskConfirmation::Archive) => (
                "Confirm archive",
                "Ask before moving a task to Archived.",
                self.confirm_archive,
                "general-confirm-archive-toggle",
            ),
            GeneralToggle::TaskConfirmation(TaskConfirmation::Settle) => (
                "Confirm settle",
                "Ask before moving a task to Settled.",
                self.confirm_settle,
                "general-confirm-settle-toggle",
            ),
            GeneralToggle::TaskConfirmation(TaskConfirmation::Delete) => (
                "Confirm delete",
                "Ask before permanently deleting a task and its LUNA-owned records.",
                self.confirm_delete,
                "general-confirm-delete-toggle",
            ),
        };
        let accent = theme.accent;
        widgets::card_row(theme, first)
            .min_h(px(76.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(theme, title))
                    .child(
                        div()
                            .mt(px(4.0))
                            .max_w(px(430.0))
                            .text_size(px(11.5))
                            .line_height(px(17.0))
                            .text_color(theme.readable_muted(0.65))
                            .child(SharedString::from(description)),
                    ),
            )
            .child(
                div()
                    .id(id)
                    .role(gpui::Role::Switch)
                    .aria_label(title)
                    .aria_toggled(if enabled {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .tab_index(0)
                    .focus_visible(move |style| style.border_2().border_color(accent))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(toggle, cx)))
                    .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                        if !event.is_held
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            cx.stop_propagation();
                            this.toggle(toggle, cx);
                        }
                    }))
                    .child(widgets::toggle_switch(theme, enabled)),
            )
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
        let send_behavior = self.composer_send_behavior;
        let follow_up_behavior = self.follow_up_behavior;
        let modifier_label = if cfg!(target_os = "macos") {
            "⌘ Enter"
        } else {
            "Ctrl Enter"
        };

        let send_row = widgets::card_row(&theme, true)
            .min_h(px(84.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(&theme, "Send shortcut"))
                    .child(
                        div()
                            .mt(px(4.0))
                            .text_size(px(11.5))
                            .text_color(theme.readable_muted(0.65))
                            .child("Choose whether Enter sends or inserts a line break."),
                    ),
            )
            .child(
                div()
                    .id("general-send-shortcut")
                    .flex()
                    .flex_row()
                    .rounded(px(9.0))
                    .p(px(2.0))
                    .bg(crate::theme::ink(0.04))
                    .children(
                        [
                            (ComposerSendBehavior::Enter, "Enter"),
                            (ComposerSendBehavior::ModEnter, modifier_label),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(ix, (behavior, label))| {
                            let selected = send_behavior == behavior;
                            div()
                                .id(("general-send-option", ix))
                                .role(gpui::Role::Button)
                                .aria_label(format!("Send shortcut: {label}"))
                                .aria_toggled(if selected {
                                    gpui::Toggled::True
                                } else {
                                    gpui::Toggled::False
                                })
                                .tab_index(0)
                                .focus_visible(move |style| style.border_2().border_color(accent))
                                .min_w(px(78.0))
                                .px(px(10.0))
                                .py(px(6.0))
                                .rounded(px(7.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.0))
                                .text_color(if selected {
                                    theme.text
                                } else {
                                    theme.text_muted
                                })
                                .when(selected, |el| {
                                    el.bg(theme.bg)
                                        .border_1()
                                        .border_color(theme.border.opacity(0.8))
                                })
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme.text))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_composer_send_behavior(behavior, cx)
                                }))
                                .on_key_down(cx.listener(
                                    move |this, event: &gpui::KeyDownEvent, _, cx| {
                                        if !event.is_held
                                            && matches!(
                                                event.keystroke.key.as_str(),
                                                "enter" | "space"
                                            )
                                        {
                                            cx.stop_propagation();
                                            this.set_composer_send_behavior(behavior, cx);
                                        }
                                    },
                                ))
                                .child(label)
                        }),
                    ),
            );

        let follow_up_row = widgets::card_row(&theme, false)
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
                            .text_color(theme.readable_muted(0.65))
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
                                widgets::page_subtitle(
                                    &theme,
                                    "Set default behavior for conversations and this device.",
                                )
                                .max_w(px(512.0))
                                .line_height(px(20.0)),
                            )
                            .child(
                                div()
                                    .mt(px(26.0))
                                    .child(widgets::field_label(&theme, "Composer"))
                                    .child(
                                        widgets::section_card(&theme)
                                            .child(send_row)
                                            .child(follow_up_row)
                                            .child(self.switch_row(
                                                &theme,
                                                GeneralToggle::ContextUsage,
                                                false,
                                                cx,
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .mt(px(26.0))
                                    .child(widgets::field_label(&theme, "Links"))
                                    .child(widgets::section_card(&theme).child(self.switch_row(
                                        &theme,
                                        GeneralToggle::OpenLinks,
                                        true,
                                        cx,
                                    ))),
                            )
                            .child(
                                div()
                                    .mt(px(26.0))
                                    .child(widgets::field_label(&theme, "Running tasks"))
                                    .child(
                                        widgets::section_card(&theme)
                                            .child(self.switch_row(
                                                &theme,
                                                GeneralToggle::AutomaticRetries,
                                                true,
                                                cx,
                                            ))
                                            .when(cfg!(target_os = "macos"), |card| {
                                                card.child(self.switch_row(
                                                    &theme,
                                                    GeneralToggle::KeepAwake,
                                                    false,
                                                    cx,
                                                ))
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .mt(px(26.0))
                                    .child(widgets::field_label(&theme, "Task confirmations"))
                                    .child(widgets::section_card(&theme).children(
                                        TaskConfirmation::ALL.into_iter().enumerate().map(
                                            |(index, action)| {
                                                self.switch_row(
                                                    &theme,
                                                    GeneralToggle::TaskConfirmation(action),
                                                    index == 0,
                                                    cx,
                                                )
                                            },
                                        ),
                                    )),
                            ),
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
        let window = cx.add_window(|_, cx| GeneralPage::new(&UiSettings::default(), cx));
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
