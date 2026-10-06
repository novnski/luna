//! Settings → Files: local preferences for workspace-file editing.

use gpui::{Context, EventEmitter, Window, div, prelude::*};

use super::widgets;
use crate::button::{ControlSize, Segment};
use crate::popover;
use crate::theme::Theme;

const DELAY_OPTIONS: [u64; 5] = [300, 600, 900, 1_500, 3_000];

/// Width of one autosave-delay segment, in points at the default interface
/// size. The control is an interface rem, so it scales with its labels.
const DELAY_SEGMENT_WIDTH: f32 = 56.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilesSettingsEvent {
    AutosaveChanged(bool),
    AutosaveDelayChanged(u64),
    WordWrapChanged(bool),
    ShowAllFilesChanged(bool),
}

pub struct FilesSettingsPage {
    scroll: widgets::PageScroll,
    autosave_enabled: bool,
    autosave_delay_ms: u64,
    word_wrap: bool,
    show_all_files: bool,
}

impl EventEmitter<FilesSettingsEvent> for FilesSettingsPage {}

impl FilesSettingsPage {
    pub fn new(
        autosave_enabled: bool,
        autosave_delay_ms: u64,
        word_wrap: bool,
        show_all_files: bool,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            scroll: widgets::PageScroll::default(),
            autosave_enabled,
            autosave_delay_ms,
            word_wrap,
            show_all_files,
        }
    }

    pub fn set_word_wrap(&mut self, word_wrap: bool, cx: &mut Context<Self>) {
        if self.word_wrap == word_wrap {
            return;
        }
        self.word_wrap = word_wrap;
        cx.notify();
    }

    pub fn set_show_all_files(&mut self, show_all_files: bool, cx: &mut Context<Self>) {
        if self.show_all_files == show_all_files {
            return;
        }
        self.show_all_files = show_all_files;
        cx.notify();
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for FilesSettingsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

/// Visible label for an autosave delay: milliseconds under a second, seconds
/// from one second up. Pure.
fn delay_label(delay: u64) -> String {
    if delay >= 1_000 {
        format!("{} s", delay as f32 / 1_000.0)
    } else {
        format!("{delay} ms")
    }
}

impl Render for FilesSettingsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let autosave_enabled = self.autosave_enabled;
        let word_wrap = self.word_wrap;
        let show_all_files = self.show_all_files;

        // Five fixed delays are a segmented control. A hand-edited delay
        // outside the presets (settings clamp to 100 ms–10 s) gets its own
        // trailing segment, so the control never claims a preset that is not
        // in effect.
        let preset = DELAY_OPTIONS
            .iter()
            .position(|delay| *delay == self.autosave_delay_ms);
        let mut segments: Vec<Segment> = DELAY_OPTIONS
            .iter()
            .map(|delay| Segment::new(delay.to_string(), delay_label(*delay)))
            .collect();
        if preset.is_none() {
            segments.push(Segment::new("custom", delay_label(self.autosave_delay_ms)));
        }
        let delay_width = DELAY_SEGMENT_WIDTH * segments.len() as f32;
        let delay_control = crate::button::segmented(
            "files-autosave-delay",
            segments,
            preset.unwrap_or(DELAY_OPTIONS.len()),
            delay_width,
            ControlSize::Md,
            &theme,
            cx.listener(move |this, ix: &usize, _, cx| {
                // The custom segment is the value already in effect.
                let Some(delay) = DELAY_OPTIONS.get(*ix).copied() else {
                    return;
                };
                if this.autosave_delay_ms == delay {
                    return;
                }
                this.autosave_delay_ms = delay;
                cx.emit(FilesSettingsEvent::AutosaveDelayChanged(delay));
                cx.notify();
            }),
        )
        .aria_label("Autosave delay");

        let saving = widgets::section_card(&theme)
            .child(widgets::section_header(&theme, "Saving", None, Vec::new()))
            .child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        "Autosave",
                        Some("Save edited workspace files to disk automatically.".into()),
                    ))
                    .child(widgets::row_controls().child(widgets::switch_control(
                        "files-autosave-toggle",
                        "Autosave",
                        autosave_enabled,
                        &theme,
                        cx,
                        |this, cx| {
                            this.autosave_enabled = !this.autosave_enabled;
                            cx.emit(FilesSettingsEvent::AutosaveChanged(this.autosave_enabled));
                            cx.notify();
                        },
                    ))),
            )
            .when(autosave_enabled, |card| {
                card.child(
                    widgets::card_row(&theme, false)
                        .child(widgets::row_text(
                            &theme,
                            "Autosave delay",
                            Some("Save files after editing has been idle for this long.".into()),
                        ))
                        .child(widgets::row_controls().child(delay_control)),
                )
            });

        let display = widgets::section_card(&theme)
            .child(widgets::section_header(&theme, "Display", None, Vec::new()))
            .child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        "Word wrap",
                        Some("Wrap long lines in every workspace file.".into()),
                    ))
                    .child(widgets::row_controls().child(widgets::switch_control(
                        "files-word-wrap-toggle",
                        "Word wrap",
                        word_wrap,
                        &theme,
                        cx,
                        |this, cx| {
                            this.word_wrap = !this.word_wrap;
                            cx.emit(FilesSettingsEvent::WordWrapChanged(this.word_wrap));
                            cx.notify();
                        },
                    ))),
            )
            .child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        "Show all files",
                        Some("Include hidden and ignored files in every file tree.".into()),
                    ))
                    .child(widgets::row_controls().child(widgets::switch_control(
                        "files-show-all-toggle",
                        "Show all files",
                        show_all_files,
                        &theme,
                        cx,
                        |this, cx| {
                            this.show_all_files = !this.show_all_files;
                            cx.emit(FilesSettingsEvent::ShowAllFilesChanged(this.show_all_files));
                            cx.notify();
                        },
                    ))),
            );

        let scrollbar = popover::rail(self, "files-settings-page-scrollbar", &theme, cx);
        div()
            .id("files-settings-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("files-settings-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_title_bar(&theme, "Files", None, Vec::new()))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Control how workspace files are displayed and saved while you edit.",
                            ))
                            .child(saving)
                            .child(display),
                    ),
            )
            .children(scrollbar)
    }
}
