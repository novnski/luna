//! Device-local task behavior preferences shown at the Settings entry point.

use gpui::{Context, EventEmitter, SharedString, Window, div, prelude::*};
use luna_proto::FollowUpBehavior;

use crate::popover::{self, ScrollRailHost};
use crate::settings::{ComposerSendBehavior, TaskConfirmation, UiSettings, widgets};
use crate::theme::Theme;

#[derive(Debug, Clone, Copy)]
pub enum GeneralEvent {
    ComposerSendBehaviorChanged(ComposerSendBehavior),
    FollowUpBehaviorChanged(FollowUpBehavior),
    AutomaticRetriesChanged(bool),
    UnattendedAnswerChanged(u64),
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
    unattended_answer_seconds: u64,
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
            unattended_answer_seconds: settings.unattended_answer_seconds,
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

    fn set_unattended_answer(&mut self, seconds: u64, cx: &mut Context<Self>) {
        if self.unattended_answer_seconds != seconds {
            self.unattended_answer_seconds = seconds;
            cx.emit(GeneralEvent::UnattendedAnswerChanged(seconds));
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
                "Retry temporary overloads and known subscription resets, up to four attempts within six hours. Other failures still need your review.",
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
        setting_row(
            theme,
            title,
            description,
            widgets::switch_control(id, title, enabled, theme, cx, move |this, cx| {
                this.toggle(toggle, cx)
            })
            .into_any_element(),
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

/// One settings row on the spec's anatomy: a wrapping title and description,
/// and the row's control in the trailing column. Every row here follows its
/// section's header, so each draws the hairline above it.
fn setting_row(
    theme: &Theme,
    title: &'static str,
    description: &'static str,
    control: gpui::AnyElement,
) -> gpui::Div {
    widgets::card_row(theme, false)
        .child(widgets::row_text(
            theme,
            title,
            Some(SharedString::from(description)),
        ))
        .child(widgets::row_controls().child(control))
}

impl Render for GeneralPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let send_behavior = self.composer_send_behavior;
        let follow_up_behavior = self.follow_up_behavior;
        let modifier = if cfg!(target_os = "macos") {
            "⌘"
        } else {
            "Ctrl"
        };
        // Both choices share one width so the two segmented controls stack
        // as a single column in the Composer card.
        const CHOICE_WIDTH: f32 = 184.0;

        let send_shortcut = crate::button::segmented(
            "general-send-shortcut",
            vec![
                crate::button::Segment::new("enter", "Enter"),
                crate::button::Segment::new("mod-enter", format!("{modifier} Enter")),
            ],
            usize::from(send_behavior == ComposerSendBehavior::ModEnter),
            CHOICE_WIDTH,
            crate::button::ControlSize::Md,
            &theme,
            cx.listener(|this, ix: &usize, _, cx| {
                this.set_composer_send_behavior(
                    if *ix == 1 {
                        ComposerSendBehavior::ModEnter
                    } else {
                        ComposerSendBehavior::Enter
                    },
                    cx,
                )
            }),
        )
        .aria_label("Send shortcut");

        let follow_up = crate::button::segmented(
            "general-follow-up-behavior",
            vec![
                crate::button::Segment::new("queue", "Queue"),
                crate::button::Segment::new("steer", "Steer"),
            ],
            usize::from(follow_up_behavior == FollowUpBehavior::Steer),
            CHOICE_WIDTH,
            crate::button::ControlSize::Md,
            &theme,
            cx.listener(|this, ix: &usize, _, cx| {
                this.set_follow_up_behavior(
                    if *ix == 1 {
                        FollowUpBehavior::Steer
                    } else {
                        FollowUpBehavior::Queue
                    },
                    cx,
                )
            }),
        )
        .aria_label("Follow-up behavior");

        let unattended_choice = crate::settings::UNATTENDED_ANSWER_CHOICES
            .iter()
            .position(|seconds| *seconds == self.unattended_answer_seconds)
            .unwrap_or(2);
        let unattended = crate::button::segmented(
            "general-unattended-answer",
            crate::settings::UNATTENDED_ANSWER_CHOICES
                .iter()
                .map(|seconds| {
                    let label = match *seconds {
                        0 => "Off".to_owned(),
                        s if s < 60 => format!("{s}s"),
                        s => format!("{}m", s / 60),
                    };
                    crate::button::Segment::new(format!("unattended-{seconds}"), label)
                })
                .collect(),
            unattended_choice,
            CHOICE_WIDTH + 56.0,
            crate::button::ControlSize::Md,
            &theme,
            cx.listener(|this, ix: &usize, _, cx| {
                if let Some(seconds) = crate::settings::UNATTENDED_ANSWER_CHOICES.get(*ix) {
                    this.set_unattended_answer(*seconds, cx);
                }
            }),
        )
        .aria_label("Answer for me after");

        let composer = widgets::section_card(&theme)
            .child(widgets::section_header(
                &theme,
                "Composer",
                Some("How messages leave the conversation composer.".into()),
                Vec::new(),
            ))
            .child(setting_row(
                &theme,
                "Send shortcut",
                "Choose whether Enter sends or inserts a line break.",
                send_shortcut.into_any_element(),
            ))
            .child(setting_row(
                &theme,
                "Follow-up behavior",
                "Choose what sending a message does while a run is live.",
                follow_up.into_any_element(),
            ))
            .child(self.switch_row(&theme, GeneralToggle::ContextUsage, cx));
        let composer_footer = widgets::section_footer(
            &theme,
            format!(
                "During a live run, {modifier} Shift Enter does the opposite of your \
                 follow-up choice once. Shift Enter always inserts a line break."
            ),
        );

        let links = widgets::section_card(&theme)
            .child(widgets::section_header(
                &theme,
                "Links",
                Some("Where links in a conversation open.".into()),
                Vec::new(),
            ))
            .child(self.switch_row(&theme, GeneralToggle::OpenLinks, cx));

        let running = widgets::section_card(&theme)
            .child(widgets::section_header(
                &theme,
                "Running tasks",
                Some("What LUNA does while an agent is working.".into()),
                Vec::new(),
            ))
            .child(self.switch_row(&theme, GeneralToggle::AutomaticRetries, cx))
            .child(setting_row(
                &theme,
                "Answer for me after",
                "When an agent is waiting for your answer, LUNA can pick its recommended option after this delay. Questions you can answer later stay open. Approvals and questions without a recommendation always wait for you.",
                unattended.into_any_element(),
            ))
            .when(cfg!(target_os = "macos"), |card| {
                card.child(self.switch_row(&theme, GeneralToggle::KeepAwake, cx))
            });

        let confirmations = widgets::section_card(&theme)
            .child(widgets::section_header(
                &theme,
                "Task confirmations",
                Some("Ask before actions that move a task or remove it.".into()),
                Vec::new(),
            ))
            .children(
                TaskConfirmation::ALL
                    .into_iter()
                    .map(|action| {
                        self.switch_row(&theme, GeneralToggle::TaskConfirmation(action), cx)
                    })
                    .collect::<Vec<_>>(),
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
                            .child(widgets::page_title_bar(&theme, "General", None, Vec::new()))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Set default behavior for conversations and this device.",
                            ))
                            .child(composer)
                            .child(composer_footer)
                            .child(links)
                            .child(running)
                            .child(confirmations),
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
