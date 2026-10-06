//! Appshots settings share the shortcut recorder and persistence events with
//! keyboard settings, while keeping capture setup on its own route.
//!
//! Two section cards (spec `docs/design/settings-polish-2026-10-06`): Capture
//! (the enable and sound switches, the capture shortcut's recorder and the
//! destination `segmented`) and Permissions (each capability's status badge
//! and its kit action, with "Check again" in the section header).
use super::*;
use crate::appshots::{AppshotPlatform, CapabilityState};
use crate::settings::widgets;

/// A capability's status: the success pill when it is usable, the neutral
/// pill otherwise (the row's action, when there is one, says what to do).
fn capability_badge(theme: &Theme, state: CapabilityState) -> gpui::Div {
    if state.is_ready() {
        widgets::badge_active(theme, state.badge())
    } else {
        widgets::badge(theme, state.badge())
    }
}

/// The persisted key of a destination's segment (its element id suffix).
fn destination_key(destination: AppshotDestination) -> &'static str {
    match destination {
        AppshotDestination::Automatic => "automatic",
        AppshotDestination::LastSession => "last-session",
        AppshotDestination::NewSession => "new-session",
    }
}

impl ShortcutsPage {
    pub(super) fn render_appshots(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::of(cx).clone();
        let capabilities = self.appshot_capabilities;

        let capture = widgets::switch_control(
            "appshots-enabled",
            "Capture Appshots",
            self.appshots_enabled,
            &theme,
            cx,
            |this, cx| {
                this.appshots_enabled = !this.appshots_enabled;
                this.commit_appshots(cx);
                cx.notify();
            },
        );
        let sound = widgets::switch_control(
            "appshot-sound-enabled",
            "Capture sound",
            self.appshot_sound_enabled,
            &theme,
            cx,
            |this, cx| {
                this.appshot_sound_enabled = !this.appshot_sound_enabled;
                this.commit_appshots(cx);
                cx.notify();
            },
        );

        let shortcut_ix = ShortcutId::ALL
            .iter()
            .position(|&id| id == ShortcutId::CaptureAppshot)
            .unwrap_or(0);
        let shortcut_title = if capabilities.platform == AppshotPlatform::LinuxWayland {
            "Preferred shortcut"
        } else {
            "Global shortcut"
        };
        let shortcut_row = widgets::card_row(&theme, false)
            .child(self.shortcut_text(
                ShortcutId::CaptureAppshot,
                shortcut_title,
                Some(capabilities.shortcut_description().into()),
                &theme,
            ))
            .child(
                widgets::row_controls()
                    .child(capability_badge(&theme, capabilities.global_shortcut))
                    .child(self.render_binding_control(
                        ShortcutId::CaptureAppshot,
                        shortcut_ix,
                        self.recording,
                        &theme,
                        cx,
                    )),
            )
            .children(self.row_notice(ShortcutId::CaptureAppshot, &theme));

        let destination_description = match self.appshot_destination {
            AppshotDestination::Automatic => {
                "Use the open session, or the new-session composer when no session is open."
            }
            AppshotDestination::LastSession => {
                "Use the open session, or return to the last session used for an Appshot."
            }
            AppshotDestination::NewSession => {
                "Stage captures in a new-session composer, keeping existing drafts intact."
            }
        };
        let destination = button::segmented(
            "appshot-destination",
            AppshotDestination::ALL
                .into_iter()
                .map(|destination| {
                    button::Segment::new(destination_key(destination), destination.label())
                })
                .collect(),
            AppshotDestination::ALL
                .iter()
                .position(|&destination| destination == self.appshot_destination)
                .unwrap_or(0),
            288.0,
            ControlSize::Md,
            &theme,
            cx.listener(|this, ix: &usize, _, cx| {
                if let Some(&destination) = AppshotDestination::ALL.get(*ix)
                    && this.appshot_destination != destination
                {
                    this.appshot_destination = destination;
                    this.commit_appshots(cx);
                    cx.notify();
                }
            }),
        );

        let capture_card = widgets::section_card(&theme)
            .child(widgets::section_header(&theme, "Capture", None, Vec::new()))
            .child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        "Capture Appshots",
                        Some("Captures are staged for review and never sent automatically.".into()),
                    ))
                    .child(widgets::row_controls().child(capture)),
            )
            .child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        "Capture sound",
                        Some("Play a sound when an Appshot is ready.".into()),
                    ))
                    .child(widgets::row_controls().child(sound)),
            )
            .child(shortcut_row)
            .child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        "Destination",
                        Some(destination_description.into()),
                    ))
                    .child(widgets::row_controls().child(destination)),
            );

        // A permission's kit action: "Allow …" asks the system once; after
        // that the same slot opens the system's settings pane.
        let action = |id: &'static str, prompted: bool, label: &'static str| {
            let label = if prompted {
                "Open System Settings"
            } else {
                label
            };
            button::button(id, ButtonKind::Secondary, ControlSize::Md, true, &theme)
                .aria_label(label)
                .child(SharedString::from(label))
                .when(prompted, |el| {
                    el.child(button::glyph(crate::icons::ARROW_UP_RIGHT, ControlSize::Md))
                })
        };
        let window_action = (self.appshots_enabled
            && capabilities.window_capture == CapabilityState::PermissionRequired)
            .then(|| {
                action(
                    "appshots-capture-access",
                    self.capture_access_prompted,
                    "Allow window capture",
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.capture_access_prompted {
                        if let Some(url) = crate::appshots::capture_settings_url() {
                            cx.open_url(url);
                        }
                    } else {
                        this.capture_access_prompted = true;
                        crate::appshots::request_capture_access();
                    }
                    this.appshot_capabilities = crate::appshots::capabilities();
                    cx.notify();
                }))
            });
        let text_action = (self.appshots_enabled
            && capabilities.application_text == CapabilityState::PermissionRequired
            && crate::appshots::semantic_settings_url().is_some())
        .then(|| {
            action(
                "appshots-semantic-access",
                self.semantic_access_prompted,
                "Enable text capture",
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if this.semantic_access_prompted {
                    if let Some(url) = crate::appshots::semantic_settings_url() {
                        cx.open_url(url);
                    }
                } else {
                    this.semantic_access_prompted = true;
                    crate::appshots::request_semantic_access();
                }
                this.appshot_capabilities = crate::appshots::capabilities();
                cx.notify();
            }))
        });
        let check_again = button::button(
            "appshots-refresh-permissions",
            ButtonKind::Secondary,
            ControlSize::Md,
            true,
            &theme,
        )
        .aria_label("Check permissions again")
        .child(button::glyph(crate::icons::REFRESH, ControlSize::Md))
        .child(SharedString::from("Check again"))
        .on_click(cx.listener(|this, _, _, cx| {
            this.appshot_capabilities = crate::appshots::capabilities();
            cx.notify();
        }));
        let permissions_card = widgets::section_card(&theme)
            .child(widgets::section_header(
                &theme,
                "Permissions",
                Some("Changed a permission? Check again after returning to LUNA.".into()),
                vec![check_again.into_any_element()],
            ))
            .child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        "Window capture",
                        Some(capabilities.capture_description().into()),
                    ))
                    .child(
                        widgets::row_controls()
                            .child(capability_badge(&theme, capabilities.window_capture))
                            .children(window_action),
                    ),
            )
            .child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        "Application text",
                        Some(capabilities.semantic_description().into()),
                    ))
                    .child(
                        widgets::row_controls()
                            .child(capability_badge(&theme, capabilities.application_text))
                            .children(text_action),
                    ),
            );

        let orphan_notice = self.orphan_notice();
        let scrollbar = self.render_scrollbar(&theme, cx);
        div()
            .id("appshots-settings-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .on_drag_move(cx.listener(Self::on_bar_drag_move))
            .child(
                div()
                    .id("appshots-settings-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .track_focus(&self.focus)
                    .tab_group()
                    .tab_index(0)
                    .tab_stop(false)
                    .on_key_down(|event, window, cx| {
                        let key = &event.keystroke;
                        if key.key == "tab"
                            && !key.modifiers.control
                            && !key.modifiers.alt
                            && !key.modifiers.platform
                        {
                            if key.modifiers.shift {
                                window.focus_prev(cx);
                            } else {
                                window.focus_next(cx);
                            }
                            cx.stop_propagation();
                        }
                    })
                    .child(
                        widgets::page_column()
                            .child(widgets::page_title_bar(
                                &theme,
                                "Appshots",
                                None,
                                Vec::new(),
                            ))
                            .child(widgets::page_subtitle(
                                &theme,
                                capabilities.setup_description(),
                            ))
                            .when_some(orphan_notice, |column, notice| {
                                column
                                    .child(widgets::warning_strip(&theme, notice).mt(ui_rems(16.0)))
                            })
                            .child(capture_card)
                            .child(permissions_card),
                    ),
            )
            .children(scrollbar)
            .into_any_element()
    }
}
