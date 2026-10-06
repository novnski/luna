//! Settings → Notifications: independently configurable session chimes plus
//! desktop banners for events that need attention (`shell::on_state_changed`).
//!
//! The ShortcutsPage arrangement: the page holds a working copy, every flip
//! emits [`NotificationsEvent::Changed`], and the shell persists it. Nothing
//! here talks RPC — all preferences are device-local UI settings.

use gpui::{AnyElement, Context, EventEmitter, SharedString, Window, div, prelude::*, px};

use crate::popover;
use crate::settings::widgets;
use crate::settings::{TaskNotificationPolicy, UiSettings};
use crate::theme::Theme;

#[derive(Debug, Clone)]
pub enum NotificationsEvent {
    /// A toggle flipped — persist the complete notification preference set.
    Changed {
        sound: bool,
        completion_sound: bool,
        input_sound: bool,
        attention_sound: bool,
        desktop: bool,
        background_only: bool,
    },
    TaskPolicyChanged {
        task_id: String,
        policy: TaskNotificationPolicy,
    },
    KeepAwakeChanged {
        task_id: String,
        enabled: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationTask {
    pub id: String,
    pub title: String,
    pub policy: TaskNotificationPolicy,
    pub keep_awake: bool,
}

#[derive(Clone, Copy)]
pub struct NotificationPreferences {
    sound: bool,
    completion_sound: bool,
    input_sound: bool,
    attention_sound: bool,
    desktop: bool,
    background_only: bool,
}

impl From<&UiSettings> for NotificationPreferences {
    fn from(settings: &UiSettings) -> Self {
        Self {
            sound: settings.sound_enabled,
            completion_sound: settings.sound_completion_enabled,
            input_sound: settings.sound_input_enabled,
            attention_sound: settings.sound_attention_enabled,
            desktop: settings.notifications_enabled,
            background_only: settings.notifications_background_only,
        }
    }
}

pub struct NotificationsPage {
    scroll: widgets::PageScroll,
    sound: bool,
    completion_sound: bool,
    input_sound: bool,
    attention_sound: bool,
    desktop: bool,
    background_only: bool,
    tasks: Vec<NotificationTask>,
}

impl EventEmitter<NotificationsEvent> for NotificationsPage {}

#[derive(Clone, Copy)]
enum NotificationPreference {
    Sound,
    CompletionSound,
    InputSound,
    AttentionSound,
    Desktop,
    BackgroundOnly,
}

impl NotificationPreference {
    /// The switch's element id and accessible name.
    fn switch(self) -> (&'static str, &'static str) {
        match self {
            Self::Sound => ("notifications-sound-toggle", "Session sounds"),
            Self::CompletionSound => (
                "notifications-completion-sound-toggle",
                "Task completed sound",
            ),
            Self::InputSound => ("notifications-input-sound-toggle", "Input required sound"),
            Self::AttentionSound => (
                "notifications-attention-sound-toggle",
                "Errors and disconnections sound",
            ),
            Self::Desktop => ("notifications-desktop-toggle", "Desktop notifications"),
            Self::BackgroundOnly => (
                "notifications-background-toggle",
                "Only notify when LUNA is in the background",
            ),
        }
    }
}

/// The per-task policies in their segmented order.
const TASK_POLICIES: [TaskNotificationPolicy; 3] = [
    TaskNotificationPolicy::All,
    TaskNotificationPolicy::ImportantOnly,
    TaskNotificationPolicy::Muted,
];

/// Width of the per-task policy control, in points at the default
/// Interface size: three equal segments that fit "Important".
const POLICY_CONTROL_WIDTH: f32 = 228.0;

fn policy_segment(policy: TaskNotificationPolicy) -> crate::button::Segment {
    let (key, label) = match policy {
        TaskNotificationPolicy::All => ("all", "All"),
        TaskNotificationPolicy::ImportantOnly => ("important", "Important"),
        TaskNotificationPolicy::Muted => ("muted", "Muted"),
    };
    crate::button::Segment::new(key, label)
}

/// What the chosen policy delivers, under the task's title
/// (`shell::activity_center::route_transition`).
fn policy_description(policy: TaskNotificationPolicy) -> &'static str {
    match policy {
        TaskNotificationPolicy::All => "Every sound and banner you allow above.",
        TaskNotificationPolicy::ImportantOnly => {
            "Completions stay quiet; input requests and failures still alert."
        }
        TaskNotificationPolicy::Muted => "No sounds or banners from this task.",
    }
}

impl NotificationsPage {
    pub fn new(
        preferences: NotificationPreferences,
        tasks: Vec<NotificationTask>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            scroll: widgets::PageScroll::default(),
            sound: preferences.sound,
            completion_sound: preferences.completion_sound,
            input_sound: preferences.input_sound,
            attention_sound: preferences.attention_sound,
            desktop: preferences.desktop,
            background_only: preferences.background_only,
            tasks,
        }
    }

    fn emit(&self, cx: &mut Context<Self>) {
        cx.emit(NotificationsEvent::Changed {
            sound: self.sound,
            completion_sound: self.completion_sound,
            input_sound: self.input_sound,
            attention_sound: self.attention_sound,
            desktop: self.desktop,
            background_only: self.background_only,
        });
    }

    fn toggle(&mut self, preference: NotificationPreference, cx: &mut Context<Self>) {
        let value = match preference {
            NotificationPreference::Sound => &mut self.sound,
            NotificationPreference::CompletionSound => &mut self.completion_sound,
            NotificationPreference::InputSound => &mut self.input_sound,
            NotificationPreference::AttentionSound => &mut self.attention_sound,
            NotificationPreference::Desktop => &mut self.desktop,
            NotificationPreference::BackgroundOnly => &mut self.background_only,
        };
        *value = !*value;
        self.emit(cx);
        cx.notify();
    }

    pub fn set_tasks(&mut self, tasks: Vec<NotificationTask>, cx: &mut Context<Self>) {
        if self.tasks != tasks {
            self.tasks = tasks;
            cx.notify();
        }
    }

    fn set_task_policy(
        &mut self,
        task_id: &str,
        policy: TaskNotificationPolicy,
        cx: &mut Context<Self>,
    ) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == task_id)
            && task.policy != policy
        {
            task.policy = policy;
            cx.emit(NotificationsEvent::TaskPolicyChanged {
                task_id: task.id.clone(),
                policy: task.policy,
            });
            cx.notify();
        }
    }

    fn toggle_task_awake(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == task_id) {
            task.keep_awake = !task.keep_awake;
            cx.emit(NotificationsEvent::KeepAwakeChanged {
                task_id: task.id.clone(),
                enabled: task.keep_awake,
            });
            cx.notify();
        }
    }

    fn preference_value(&self, preference: NotificationPreference) -> bool {
        match preference {
            NotificationPreference::Sound => self.sound,
            NotificationPreference::CompletionSound => self.completion_sound,
            NotificationPreference::InputSound => self.input_sound,
            NotificationPreference::AttentionSound => self.attention_sound,
            NotificationPreference::Desktop => self.desktop,
            NotificationPreference::BackgroundOnly => self.background_only,
        }
    }

    /// The kit switch for `preference`. While its parent setting is off
    /// (`available` false) the same switch is drawn inert: still announced
    /// with its name and value, but with no focus stop, pointer cursor or
    /// handlers. It keeps the kit switch's geometry and motion key, so the
    /// row never shifts when the parent flips.
    fn preference_switch(
        &self,
        preference: NotificationPreference,
        available: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (id, label) = preference.switch();
        let on = self.preference_value(preference);
        if available {
            return widgets::switch_control(id, label, on, theme, cx, move |this, cx| {
                this.toggle(preference, cx)
            })
            .into_any_element();
        }
        div()
            .id(id)
            .role(gpui::Role::Switch)
            .aria_label(label)
            .aria_toggled(if on {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .aria_description("Unavailable while its parent setting is off")
            .flex_none()
            .m(px(-2.0))
            .rounded_full()
            .border_2()
            .border_color(gpui::transparent_black())
            .cursor_default()
            .child(widgets::toggle_switch(theme, on, id))
            .into_any_element()
    }

    /// A master switch for a section header's actions slot, centred on the
    /// header title's first line at every Interface size (the title's line
    /// grows with the rem; the switch keeps its kit size).
    fn header_switch(
        &self,
        preference: NotificationPreference,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex_none()
            .h(crate::typography::TextStyle::Title3.line_height())
            .flex()
            .items_center()
            .child(self.preference_switch(preference, true, theme, cx))
            .into_any_element()
    }

    /// A dependent row under a section header whose switch is the parent:
    /// its text and an inert switch dim while the parent is off. A plain
    /// setting row, so no identity tile.
    fn dependent_row(
        &self,
        theme: &Theme,
        title: &'static str,
        description: &'static str,
        preference: NotificationPreference,
        available: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        // The contents dim, not the row, so the hairline above it keeps the
        // same weight as every other separator in the card.
        let dim = |el: gpui::Div| el.when(!available, |el| el.opacity(0.5));
        widgets::card_row(theme, false)
            .child(dim(widgets::row_text(
                theme,
                title,
                Some(SharedString::from(description)),
            )))
            .child(dim(widgets::row_controls().child(
                self.preference_switch(preference, available, theme, cx),
            )))
    }

    /// One task on this device: its title and what its policy delivers, then
    /// the policy choice and its labelled keep-awake switch at the right.
    fn task_row(
        &self,
        theme: &Theme,
        task: &NotificationTask,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let policy_task = task.id.clone();
        let awake_task = task.id.clone();
        let selected = TASK_POLICIES
            .iter()
            .position(|policy| *policy == task.policy)
            .unwrap_or(0);
        let policy = crate::button::segmented(
            format!("task-notification-policy-{}", task.id),
            TASK_POLICIES.into_iter().map(policy_segment).collect(),
            selected,
            POLICY_CONTROL_WIDTH,
            crate::button::ControlSize::Md,
            theme,
            cx.listener(move |this, ix: &usize, _, cx| {
                if let Some(policy) = TASK_POLICIES.get(*ix) {
                    this.set_task_policy(&policy_task, *policy, cx);
                }
            }),
        )
        .aria_label(format!("Notification policy for {}", task.title));
        let keep_awake = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(crate::typography::ui_rems(8.0))
            .child(
                widgets::row_description(theme, "Keep awake")
                    .flex_none()
                    .whitespace_nowrap(),
            )
            .child(widgets::switch_control(
                SharedString::from(format!("task-keep-awake-{}", task.id)),
                format!("Keep awake for {}", task.title),
                task.keep_awake,
                theme,
                cx,
                move |this, cx| this.toggle_task_awake(&awake_task, cx),
            ));
        widgets::card_row(theme, false)
            .child(widgets::row_text(
                theme,
                task.title.clone(),
                Some(SharedString::from(policy_description(task.policy))),
            ))
            .child(widgets::row_controls().child(policy).child(keep_awake))
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for NotificationsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for NotificationsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let sound = self.sound;
        let desktop = self.desktop;

        // Each master switch sits in its section's header, so the event rows
        // under it read as its sub-options (like the providers' enable
        // switch); while it is off they dim and their switches go inert.
        let sounds = widgets::section_card(&theme)
            .child(widgets::section_header(
                &theme,
                "Session sounds",
                Some("Play a sound for the session events you choose below.".into()),
                vec![self.header_switch(NotificationPreference::Sound, &theme, cx)],
            ))
            .child(self.dependent_row(
                &theme,
                "Task completed",
                "Play a sound when an agent finishes a run.",
                NotificationPreference::CompletionSound,
                sound,
                cx,
            ))
            .child(self.dependent_row(
                &theme,
                "Input required",
                "Play a sound when an agent needs your response.",
                NotificationPreference::InputSound,
                sound,
                cx,
            ))
            .child(self.dependent_row(
                &theme,
                "Errors and disconnections",
                "Play a sound when a run fails or the connection remains unavailable.",
                NotificationPreference::AttentionSound,
                sound,
                cx,
            ));

        let banners = widgets::section_card(&theme)
            .child(widgets::section_header(
                &theme,
                "Desktop notifications",
                Some(
                    "Show a system banner when a task needs your attention. Completed \
                     tasks stay in Activity."
                        .into(),
                ),
                vec![self.header_switch(NotificationPreference::Desktop, &theme, cx)],
            ))
            .child(self.dependent_row(
                &theme,
                "Only when in the background",
                "Skip the banner while a LUNA window is focused.",
                NotificationPreference::BackgroundOnly,
                desktop,
                cx,
            ));

        let task_card = widgets::section_card(&theme)
            .child(widgets::section_header(
                &theme,
                "Tasks",
                Some(
                    "Choose which alerts each task on this device sends. Keep awake stops \
                     this device from sleeping while that task is selected and working."
                        .into(),
                ),
                Vec::new(),
            ))
            .when(self.tasks.is_empty(), |card| {
                card.child(
                    widgets::card_row(&theme, false).child(
                        widgets::row_description(
                            &theme,
                            "No tasks on this device yet. Tasks you start here appear in \
                             this list.",
                        )
                        .flex_1(),
                    ),
                )
            })
            .children(
                self.tasks
                    .iter()
                    .map(|task| self.task_row(&theme, task, cx))
                    .collect::<Vec<_>>(),
            );

        let scrollbar = popover::rail(self, "notifications-page-scrollbar", &theme, cx);
        div()
            .id("notifications-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("notifications-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_title_bar(
                                &theme,
                                "Notifications",
                                None,
                                Vec::new(),
                            ))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Choose which session events can play a sound, and when desktop \
                                 notifications appear.",
                            ))
                            .child(sounds)
                            .child(banners)
                            .child(task_card),
                    ),
            )
            .children(scrollbar)
    }
}
