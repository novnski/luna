//! Settings → Appearance: system behavior, independent light/dark variants,
//! and the optional interactive accent overlay.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    KeyDownEvent, ObjectFit, Render, SharedString, StyledImage as _, Subscription, Window, div,
    img, prelude::*, px,
};
use luna_theme::vscode::{ImportReport, SourceCompilation};
use luna_theme::{
    AccentPreset, AccentSelection, CustomThemeEntry, CustomThemeStatus, InstallMode,
    SurfacePreference, SurfaceTreatment, ThemeRegistry, ThemeSelection,
};

use crate::appearance::{self, AppearanceMode};
use crate::button::{ButtonKind, ControlSize, Segment};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons;
use crate::popover::{self, Popup};
use crate::settings::widgets;
use crate::settings::{DiffColorScheme, SavePolicy};
use crate::theme::{Appearance, FillTier, Theme};
use crate::theme_library;
use crate::typography::{
    self, FontAvailability, TextStyle, TypeStyleExt as _, UiFontFamily, UiFontSize, ui_rems,
};

struct ImportDialog {
    input: Entity<ComposerInput>,
    _events: Subscription,
    focus: FocusHandle,
    focus_pending: bool,
    mode: InstallMode,
    compilation: Option<SourceCompilation>,
    selected: HashSet<String>,
    review_variant: Option<String>,
    error: Option<SharedString>,
}

/// The three independently configurable font slots. Interface and code/diff
/// draw from the whole catalog — proportional faces are legal there. The
/// terminal draws from the fixed-width subset only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FontKind {
    Ui,
    Terminal,
    Code,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppearanceSettingsEvent {
    CodeFontSizeChanged(f32),
    FileWordWrapChanged(bool),
    DiffPreferencesChanged { split: bool, wrap: bool },
}

impl FontKind {
    const ALL: [Self; 3] = [Self::Ui, Self::Terminal, Self::Code];

    fn slug(self) -> &'static str {
        match self {
            Self::Ui => "interface",
            Self::Terminal => "terminal",
            Self::Code => "code",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ui => "Interface font",
            Self::Terminal => "Terminal font",
            Self::Code => "Code & diff font",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Ui => "Menus, sidebars, and conversation text. The size scales the whole interface.",
            Self::Terminal => "Terminal panes and shell output. Fixed-width families only.",
            Self::Code => "Code blocks, diffs, and workspace file editors.",
        }
    }

    /// The catalog this slot may pick from. Only the terminal narrows: its
    /// renderer positions cursor, selection, and hit-testing on an `m`-wide
    /// cell grid, which a proportional family silently breaks.
    fn choices_for(self, availability: &FontAvailability) -> &[UiFontFamily] {
        match self {
            Self::Terminal => availability.fixed_width_choices(),
            _ => availability.choices(),
        }
    }

    fn is_available_for(self, availability: &FontAvailability, family: &UiFontFamily) -> bool {
        match self {
            Self::Terminal => availability.is_fixed_width_available(family),
            _ => availability.is_available(family),
        }
    }

    fn requested(self, cx: &gpui::App) -> UiFontFamily {
        match self {
            Self::Ui => typography::requested(cx),
            Self::Terminal => typography::terminal_requested(cx),
            Self::Code => typography::code_requested(cx),
        }
    }

    fn effective(self, cx: &gpui::App) -> UiFontFamily {
        match self {
            Self::Ui => typography::effective(cx),
            Self::Terminal => typography::terminal_effective(cx),
            Self::Code => typography::code_effective(cx),
        }
    }

    fn apply_family(self, family: UiFontFamily, cx: &mut gpui::App) {
        match self {
            Self::Ui => typography::set_family(family, cx),
            Self::Terminal => typography::set_terminal_family(family, cx),
            Self::Code => typography::set_code_family(family, cx),
        };
    }

    fn pixel_size(self, cx: &gpui::App) -> f32 {
        match self {
            Self::Ui => typography::font_size(cx).pixels(),
            Self::Terminal => typography::terminal_font_size(cx),
            Self::Code => typography::code_font_size(cx),
        }
    }

    /// Labels for this kind's size ladder, in ladder order. Both ladders read
    /// as plain pixel values, so all three dropdowns look the same.
    fn size_labels(self) -> Vec<SharedString> {
        match self {
            Self::Ui => UiFontSize::ALL.iter().map(|size| size.label()).collect(),
            _ => MONO_FONT_SIZES
                .iter()
                .map(|size| SharedString::from(format_px(*size)))
                .collect(),
        }
    }

    fn size_count(self) -> usize {
        match self {
            Self::Ui => UiFontSize::ALL.len(),
            _ => MONO_FONT_SIZES.len(),
        }
    }

    /// Ladder position of the committed size. Terminal and code sizes are
    /// stored as free pixels (older settings, hand-edited files), so they snap
    /// to the nearest rung rather than falling off the list.
    fn size_ix(self, cx: &gpui::App) -> usize {
        match self {
            Self::Ui => UiFontSize::ALL
                .iter()
                .position(|size| *size == typography::font_size(cx))
                .unwrap_or_default(),
            _ => nearest_mono_ix(self.pixel_size(cx)),
        }
    }
}

/// Pixel ladder behind the terminal and code size dropdowns. Both defaults
/// (terminal 13, code 12.5) are rungs, so today's rendering is reachable.
const MONO_FONT_SIZES: [f32; 10] = [10.0, 11.0, 12.0, 12.5, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0];

// Control widths, in points at the default 16 px Interface size. Each is an
// interface rem ([`ui_rems`]) so a control keeps room for its label at every
// Interface size. A font row's family + size pickers and the conversation
// width control are about the same total width, so their right-aligned
// controls line up and wrap under their text at the same window widths.
const FAMILY_PICKER_WIDTH: f32 = 200.0;
const FAMILY_MENU_WIDTH: f32 = 240.0;
const FAMILY_MENU_MAX_HEIGHT: f32 = 280.0;
const SIZE_PICKER_WIDTH: f32 = 92.0;
const SIZE_MENU_WIDTH: f32 = 112.0;
const THEME_PICKER_WIDTH: f32 = 220.0;
const THEME_MENU_WIDTH: f32 = 260.0;
const WIDTH_SLIDER_WIDTH: f32 = 196.0;
const WIDTH_TRACK: f32 = 4.0;
const WIDTH_KNOB: f32 = 14.0;
/// Segmented controls: two short options, and the three Glass options.
const SEGMENTED_PAIR_WIDTH: f32 = 220.0;
const SEGMENTED_GLASS_WIDTH: f32 = 300.0;
const SEGMENTED_EFFECT_WIDTH: f32 = 360.0;

fn nearest_mono_ix(size: f32) -> usize {
    MONO_FONT_SIZES
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (**a - size).abs().total_cmp(&(**b - size).abs()))
        .map(|(ix, _)| ix)
        .unwrap_or_default()
}

#[derive(Clone)]
struct TranscriptWidthDrag;

impl Render for TranscriptWidthDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

pub struct AppearancePage {
    width_focus: FocusHandle,
    width_pressed: bool,
    width_bounds: Rc<Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    pending_width: Option<f32>,
    width_frame_pending: bool,
    scroll: crate::settings::widgets::PageScroll,
    selected_font: UiFontFamily,
    selected_terminal_font: UiFontFamily,
    selected_code_font: UiFontFamily,
    selected_size: UiFontSize,
    selected_terminal_size: f32,
    selected_code_size: f32,
    font_focus: FocusHandle,
    terminal_font_focus: FocusHandle,
    code_font_focus: FocusHandle,
    size_focus: FocusHandle,
    terminal_size_focus: FocusHandle,
    code_size_focus: FocusHandle,
    font_menu: Popup<()>,
    terminal_font_menu: Popup<()>,
    code_font_menu: Popup<()>,
    /// Floating rail for each family dropdown (the menu-scrollbar treatment,
    /// on the menu's own scroll host). One per kind: the menus scroll
    /// independently, so sharing a state would carry one menu's offset and
    /// rail timers into the next one opened.
    font_list: widgets::PageScroll,
    terminal_font_list: widgets::PageScroll,
    code_font_list: widgets::PageScroll,
    size_menu: Popup<()>,
    terminal_size_menu: Popup<()>,
    code_size_menu: Popup<()>,
    font_menu_dismissed_at: Option<std::time::Instant>,
    terminal_font_menu_dismissed_at: Option<std::time::Instant>,
    code_font_menu_dismissed_at: Option<std::time::Instant>,
    /// Filter for whichever family menu is open — a device can carry hundreds
    /// of families, so the list narrows as you type instead of asking you to
    /// scroll. One input serves all three kinds: only one menu is ever open.
    font_search: Entity<ComposerInput>,
    _font_search_events: Subscription,
    size_menu_dismissed_at: Option<std::time::Instant>,
    terminal_size_menu_dismissed_at: Option<std::time::Instant>,
    code_size_menu_dismissed_at: Option<std::time::Instant>,
    light_theme_menu: Popup<()>,
    dark_theme_menu: Popup<()>,
    import_dialog: Option<ImportDialog>,
    review_entry: Option<String>,
    library_error: Option<SharedString>,
    background_error: Option<SharedString>,
    reset_confirmation_open: bool,
    /// The library entry (id, name) awaiting the Remove confirmation.
    remove_confirmation: Option<(String, SharedString)>,
    /// Focus for the reset and remove confirmations, taken when one opens so
    /// Escape reaches the card instead of the control behind the scrim.
    confirm_focus: FocusHandle,
    confirm_focus_pending: bool,
}

impl AppearancePage {
    fn reset_to_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = crate::settings::remove_new_thread_composer_background(cx) {
            self.background_error = Some(error.into());
            self.reset_confirmation_open = false;
            cx.notify();
            return;
        }
        let defaults = crate::settings::UiSettings::default();
        crate::settings::update(SavePolicy::Immediate, cx, |settings| {
            settings.code_fences_fit_content = defaults.code_fences_fit_content;
            settings.diff_split = defaults.diff_split;
            settings.diff_wrap = defaults.diff_wrap;
            settings.diff_color_scheme = defaults.diff_color_scheme;
            settings.transcript_width = defaults.transcript_width;
            settings.new_thread_background_effect = defaults.new_thread_background_effect;
        });
        appearance::set_mode(defaults.appearance, cx);
        appearance::set_theme(Appearance::Light, defaults.theme_selection.light, cx);
        appearance::set_theme(Appearance::Dark, defaults.theme_selection.dark, cx);
        appearance::set_accent(defaults.accent, cx);
        appearance::set_surface(defaults.surface, cx);
        typography::set_family(defaults.ui_font_family, cx);
        typography::set_font_size(defaults.ui_font_size, window, cx);
        typography::set_terminal_family(defaults.terminal_font_family, cx);
        typography::set_terminal_font_size(defaults.terminal_font_size, cx);
        typography::set_code_family(defaults.code_font_family, cx);
        typography::set_code_font_size(defaults.code_font_size, cx);
        cx.emit(AppearanceSettingsEvent::CodeFontSizeChanged(
            defaults.code_font_size,
        ));
        cx.emit(AppearanceSettingsEvent::FileWordWrapChanged(
            defaults.files_word_wrap,
        ));
        self.emit_diff_preferences(cx);
        appearance::apply_registry_change(cx);
        self.pending_width = None;
        self.selected_font = typography::effective(cx);
        self.selected_terminal_font = typography::terminal_effective(cx);
        self.selected_code_font = typography::code_effective(cx);
        self.selected_size = typography::font_size(cx);
        self.selected_terminal_size = typography::terminal_font_size(cx);
        self.selected_code_size = typography::code_font_size(cx);
        self.background_error = None;
        self.reset_confirmation_open = false;
        cx.refresh_windows();
    }

    fn toggle_chat_code_wrap(&mut self, cx: &mut Context<Self>) {
        let enabled = !crate::settings::current(cx).code_fences_fit_content;
        crate::settings::update(SavePolicy::Immediate, cx, |settings| {
            settings.code_fences_fit_content = enabled;
        });
        cx.refresh_windows();
    }

    fn toggle_diff_wrap(&mut self, cx: &mut Context<Self>) {
        let wrap = !crate::settings::current(cx).diff_wrap;
        crate::settings::update(SavePolicy::Immediate, cx, |settings| {
            settings.diff_wrap = wrap;
        });
        self.emit_diff_preferences(cx);
    }

    fn toggle_file_wrap(&mut self, cx: &mut Context<Self>) {
        let wrap = !crate::settings::current(cx).files_word_wrap;
        cx.emit(AppearanceSettingsEvent::FileWordWrapChanged(wrap));
        cx.notify();
    }

    fn set_diff_layout(&mut self, split: bool, cx: &mut Context<Self>) {
        crate::settings::update(SavePolicy::Immediate, cx, |settings| {
            settings.diff_split = split;
        });
        self.emit_diff_preferences(cx);
    }

    fn emit_diff_preferences(&mut self, cx: &mut Context<Self>) {
        let settings = crate::settings::current(cx);
        cx.emit(AppearanceSettingsEvent::DiffPreferencesChanged {
            split: settings.diff_split,
            wrap: settings.diff_wrap,
        });
        cx.notify();
    }

    fn queue_width(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        let width = crate::settings::normalize_transcript_width(width);
        if self
            .pending_width
            .unwrap_or_else(|| crate::settings::transcript_width(cx))
            == width
        {
            return;
        }
        self.pending_width = Some(width);
        // Coalesce pointer events into one layout/settings update per frame.
        if !self.width_frame_pending {
            self.width_frame_pending = true;
            let page = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                let _ = page.update(cx, |this, cx| {
                    this.width_frame_pending = false;
                    this.apply_pending_width(cx);
                });
            });
        }
        cx.notify();
    }

    fn apply_pending_width(&mut self, cx: &mut Context<Self>) {
        if let Some(width) = self.pending_width.take() {
            crate::settings::set_transcript_width(width, cx);
            cx.notify();
        }
    }

    fn drag_width(&mut self, x: gpui::Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bounds) = self.width_bounds.get() else {
            return;
        };
        // The knob is an interface rem, so its half-width (the track's inset
        // at either end) follows the window's rem.
        let inset = f32::from(window.rem_size()) * (WIDTH_KNOB / 2.0) / 16.0;
        let fraction = (f32::from(x - bounds.left()) - inset)
            / (f32::from(bounds.size.width) - 2.0 * inset).max(1.0);
        self.queue_width(
            crate::settings::TRANSCRIPT_WIDTH_MIN
                + fraction.clamp(0.0, 1.0)
                    * (crate::settings::TRANSCRIPT_WIDTH_MAX
                        - crate::settings::TRANSCRIPT_WIDTH_MIN),
            window,
            cx,
        );
    }

    /// The conversation-width control for its row's trailing column: the
    /// slider, its live value, and a reset that is enabled only away from the
    /// default. Every dimension is an interface rem, so the control keeps its
    /// proportions at every Interface size.
    fn render_width_control(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        use crate::settings::{
            TRANSCRIPT_WIDTH_DEFAULT, TRANSCRIPT_WIDTH_MAX, TRANSCRIPT_WIDTH_MIN,
            TRANSCRIPT_WIDTH_STEP,
        };
        let width = self
            .pending_width
            .unwrap_or_else(|| crate::settings::transcript_width(cx));
        let fraction = ((width - TRANSCRIPT_WIDTH_MIN)
            / (TRANSCRIPT_WIDTH_MAX - TRANSCRIPT_WIDTH_MIN))
            .clamp(0.0, 1.0);
        let bounds = self.width_bounds.clone();
        let ring = theme.focus_ring();
        let half = WIDTH_KNOB / 2.0;
        let track = WIDTH_TRACK;
        let height = ControlSize::Md.height();
        let slider = div()
            .id("transcript-width-slider")
            .role(gpui::Role::Slider)
            .aria_label("Conversation width")
            .aria_value(format!("{} pixels", width as u32))
            .tab_index(0)
            .track_focus(&self.width_focus)
            .relative()
            .flex_none()
            .w(typography::ui_rems(WIDTH_SLIDER_WIDTH))
            .h(typography::ui_rems(height))
            .rounded(px(6.0))
            .cursor_pointer()
            .focus_visible(move |style| crate::theme::focus_ring(style, ring))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    window.focus(&this.width_focus, cx);
                    this.width_pressed = true;
                    cx.notify();
                    cx.stop_propagation();
                    this.drag_width(event.position.x, window, cx);
                }),
            )
            .on_drag(TranscriptWidthDrag, |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| drag.clone())
            })
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let width = this
                    .pending_width
                    .unwrap_or_else(|| crate::settings::transcript_width(cx));
                let next = match event.keystroke.key.as_str() {
                    "left" | "down" => width - TRANSCRIPT_WIDTH_STEP,
                    "right" | "up" => width + TRANSCRIPT_WIDTH_STEP,
                    "home" => TRANSCRIPT_WIDTH_MIN,
                    "end" => TRANSCRIPT_WIDTH_MAX,
                    _ => return,
                };
                cx.stop_propagation();
                window.prevent_default();
                this.queue_width(next, window, cx);
            }))
            .child(
                gpui::canvas(move |rect, _, _| bounds.set(Some(rect)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .left(typography::ui_rems(half))
                    .right(typography::ui_rems(half))
                    .top(typography::ui_rems((height - track) / 2.0))
                    .h(typography::ui_rems(track))
                    .rounded_full()
                    .bg(theme.border)
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(fraction))
                            .rounded_full()
                            .bg(theme.accent),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(gpui::relative(fraction))
                            .ml(typography::ui_rems(-half))
                            .top(typography::ui_rems((track - WIDTH_KNOB) / 2.0))
                            .size(typography::ui_rems(WIDTH_KNOB))
                            .rounded_full()
                            .bg(theme.accent),
                    ),
            );
        let at_default = (width - TRANSCRIPT_WIDTH_DEFAULT).abs() < 0.5;
        let reset = crate::button::icon_button(
            "reset-transcript-width",
            icons::RESTART,
            ControlSize::Sm,
            !at_default,
            theme,
        )
        .aria_label("Reset conversation width")
        .tooltip(popover::text_tooltip(format!(
            "Reset to {TRANSCRIPT_WIDTH_DEFAULT:.0} px"
        )))
        .when(!at_default, |reset| {
            reset.on_click(cx.listener(|this, _, _, cx| {
                this.pending_width = None;
                crate::settings::set_transcript_width(TRANSCRIPT_WIDTH_DEFAULT, cx);
                cx.notify();
            }))
        });
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(typography::ui_rems(8.0))
            .child(slider)
            .child(
                div()
                    .flex_none()
                    .w(typography::ui_rems(56.0))
                    .text_right()
                    .type_style(TextStyle::Footnote)
                    .font_features(typography::tabular_figures())
                    .text_color(theme.readable_muted(0.9))
                    .child(SharedString::from(format!("{width:.0} px"))),
            )
            .child(reset)
            .into_any_element()
    }

    pub fn new(cx: &mut Context<Self>) -> Self {
        // `PaletteSearch` binds text-editing keys only — arrows/Enter/Escape
        // stay unbound and bubble from the input to the menu card's own key
        // handler. `Submitted` never fires here, so Enter has exactly one path.
        let font_search =
            cx.new(|cx| ComposerInput::with_context("Search fonts", "PaletteSearch", cx));
        let font_search_events = cx.subscribe(&font_search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                this.on_font_search_edited(cx);
            }
        });
        Self {
            width_focus: cx.focus_handle(),
            width_pressed: false,
            width_bounds: Rc::default(),
            pending_width: None,
            width_frame_pending: false,
            scroll: crate::settings::widgets::PageScroll::default(),
            selected_font: typography::effective(cx),
            selected_terminal_font: typography::terminal_effective(cx),
            selected_code_font: typography::code_effective(cx),
            selected_size: typography::font_size(cx),
            selected_terminal_size: typography::terminal_font_size(cx),
            selected_code_size: typography::code_font_size(cx),
            font_focus: cx.focus_handle(),
            terminal_font_focus: cx.focus_handle(),
            code_font_focus: cx.focus_handle(),
            size_focus: cx.focus_handle(),
            terminal_size_focus: cx.focus_handle(),
            code_size_focus: cx.focus_handle(),
            font_menu: Popup::default(),
            terminal_font_menu: Popup::default(),
            code_font_menu: Popup::default(),
            font_list: widgets::PageScroll::default(),
            terminal_font_list: widgets::PageScroll::default(),
            code_font_list: widgets::PageScroll::default(),
            size_menu: Popup::default(),
            terminal_size_menu: Popup::default(),
            code_size_menu: Popup::default(),
            font_menu_dismissed_at: None,
            terminal_font_menu_dismissed_at: None,
            code_font_menu_dismissed_at: None,
            font_search,
            _font_search_events: font_search_events,
            size_menu_dismissed_at: None,
            terminal_size_menu_dismissed_at: None,
            code_size_menu_dismissed_at: None,
            light_theme_menu: Popup::default(),
            dark_theme_menu: Popup::default(),
            import_dialog: None,
            review_entry: None,
            library_error: None,
            background_error: None,
            reset_confirmation_open: false,
            remove_confirmation: None,
            confirm_focus: cx.focus_handle(),
            confirm_focus_pending: false,
        }
    }

    #[doc(hidden)]
    pub fn fixture_scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.scroll.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn font_menu(&self, kind: FontKind) -> &Popup<()> {
        match kind {
            FontKind::Ui => &self.font_menu,
            FontKind::Terminal => &self.terminal_font_menu,
            FontKind::Code => &self.code_font_menu,
        }
    }

    fn font_menu_mut(&mut self, kind: FontKind) -> &mut Popup<()> {
        match kind {
            FontKind::Ui => &mut self.font_menu,
            FontKind::Terminal => &mut self.terminal_font_menu,
            FontKind::Code => &mut self.code_font_menu,
        }
    }

    fn font_list_mut(&mut self, kind: FontKind) -> &mut widgets::PageScroll {
        match kind {
            FontKind::Ui => &mut self.font_list,
            FontKind::Terminal => &mut self.terminal_font_list,
            FontKind::Code => &mut self.code_font_list,
        }
    }

    fn selected_font(&self, kind: FontKind) -> &UiFontFamily {
        match kind {
            FontKind::Ui => &self.selected_font,
            FontKind::Terminal => &self.selected_terminal_font,
            FontKind::Code => &self.selected_code_font,
        }
    }

    fn set_selected_font(&mut self, kind: FontKind, family: UiFontFamily) {
        match kind {
            FontKind::Ui => self.selected_font = family,
            FontKind::Terminal => self.selected_terminal_font = family,
            FontKind::Code => self.selected_code_font = family,
        }
    }

    fn font_focus(&self, kind: FontKind) -> &FocusHandle {
        match kind {
            FontKind::Ui => &self.font_focus,
            FontKind::Terminal => &self.terminal_font_focus,
            FontKind::Code => &self.code_font_focus,
        }
    }

    fn font_dismissed_at(&mut self, kind: FontKind) -> &mut Option<std::time::Instant> {
        match kind {
            FontKind::Ui => &mut self.font_menu_dismissed_at,
            FontKind::Terminal => &mut self.terminal_font_menu_dismissed_at,
            FontKind::Code => &mut self.code_font_menu_dismissed_at,
        }
    }

    fn commit_font(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        let family = self.selected_font(kind).clone();
        if kind.is_available_for(&typography::availability(cx), &family) {
            kind.apply_family(family, cx);
            let effective = kind.effective(cx);
            self.set_selected_font(kind, effective);
            self.close_font_menu(kind, cx);
            cx.notify();
        }
    }

    fn size_menu(&self, kind: FontKind) -> &Popup<()> {
        match kind {
            FontKind::Ui => &self.size_menu,
            FontKind::Terminal => &self.terminal_size_menu,
            FontKind::Code => &self.code_size_menu,
        }
    }

    fn size_menu_mut(&mut self, kind: FontKind) -> &mut Popup<()> {
        match kind {
            FontKind::Ui => &mut self.size_menu,
            FontKind::Terminal => &mut self.terminal_size_menu,
            FontKind::Code => &mut self.code_size_menu,
        }
    }

    fn size_focus(&self, kind: FontKind) -> &FocusHandle {
        match kind {
            FontKind::Ui => &self.size_focus,
            FontKind::Terminal => &self.terminal_size_focus,
            FontKind::Code => &self.code_size_focus,
        }
    }

    fn size_dismissed_at(&mut self, kind: FontKind) -> &mut Option<std::time::Instant> {
        match kind {
            FontKind::Ui => &mut self.size_menu_dismissed_at,
            FontKind::Terminal => &mut self.terminal_size_menu_dismissed_at,
            FontKind::Code => &mut self.code_size_menu_dismissed_at,
        }
    }

    /// Highlighted rung of this kind's size ladder.
    fn selected_size_ix(&self, kind: FontKind) -> usize {
        match kind {
            FontKind::Ui => UiFontSize::ALL
                .iter()
                .position(|size| *size == self.selected_size)
                .unwrap_or_default(),
            FontKind::Terminal => nearest_mono_ix(self.selected_terminal_size),
            FontKind::Code => nearest_mono_ix(self.selected_code_size),
        }
    }

    fn set_selected_size_ix(&mut self, kind: FontKind, ix: usize) {
        let ix = ix.min(kind.size_count() - 1);
        match kind {
            FontKind::Ui => self.selected_size = UiFontSize::ALL[ix],
            FontKind::Terminal => self.selected_terminal_size = MONO_FONT_SIZES[ix],
            FontKind::Code => self.selected_code_size = MONO_FONT_SIZES[ix],
        }
    }

    fn commit_size(&mut self, kind: FontKind, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected_size_ix(kind);
        match kind {
            FontKind::Ui => {
                typography::set_font_size(UiFontSize::ALL[ix], window, cx);
                self.selected_size = typography::font_size(cx);
            }
            FontKind::Terminal => {
                let next = MONO_FONT_SIZES[ix];
                typography::set_terminal_font_size(next, cx);
                self.selected_terminal_size = next;
            }
            FontKind::Code => {
                let next = MONO_FONT_SIZES[ix];
                typography::set_code_font_size(next, cx);
                self.selected_code_size = next;
                // Open file editors only learn the new size through this event.
                cx.emit(AppearanceSettingsEvent::CodeFontSizeChanged(next));
            }
        }
        self.close_size_menu(kind, cx);
        cx.notify();
    }

    /// This kind's catalog, narrowed and ranked by the typed query.
    fn visible_choices(
        &self,
        kind: FontKind,
        availability: &FontAvailability,
        cx: &gpui::App,
    ) -> Vec<UiFontFamily> {
        filter_families(
            self.font_search.read(cx).text(),
            kind.choices_for(availability),
        )
    }

    /// The kind whose menu is open, if any. Opening one closes the others.
    fn open_font_kind(&self) -> Option<FontKind> {
        FontKind::ALL
            .into_iter()
            .find(|kind| self.font_menu(*kind).is_open())
    }

    fn on_font_search_edited(&mut self, cx: &mut Context<Self>) {
        if let Some(kind) = self.open_font_kind() {
            self.clamp_highlight(kind, cx);
        }
    }

    /// Keep the highlighted row inside the filtered list, so the next Enter
    /// cannot commit a family the query has already filtered out of existence.
    fn clamp_highlight(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        let availability = typography::availability(cx);
        let visible = self.visible_choices(kind, &availability, cx);
        if !visible.contains(self.selected_font(kind)) {
            let next = if visible.is_empty() {
                kind.effective(cx)
            } else {
                first_available(&visible, kind, &availability)
            };
            self.set_selected_font(kind, next);
        }
        cx.notify();
    }

    fn close_font_menu(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        if !self.font_menu_mut(kind).begin_close() {
            return;
        }
        match kind {
            FontKind::Ui => popover::reap_popup(cx, |page| &mut page.font_menu),
            FontKind::Terminal => popover::reap_popup(cx, |page| &mut page.terminal_font_menu),
            FontKind::Code => popover::reap_popup(cx, |page| &mut page.code_font_menu),
        }
    }

    /// Only one of this page's six font dropdowns may be open at a time;
    /// opening any one closes the other five.
    fn close_other_menus(
        &mut self,
        keep_family: Option<FontKind>,
        keep_size: Option<FontKind>,
        cx: &mut Context<Self>,
    ) {
        for kind in FontKind::ALL {
            if Some(kind) != keep_family {
                self.close_font_menu(kind, cx);
            }
            if Some(kind) != keep_size {
                self.close_size_menu(kind, cx);
            }
        }
    }

    fn close_size_menu(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        if !self.size_menu_mut(kind).begin_close() {
            return;
        }
        match kind {
            FontKind::Ui => popover::reap_popup(cx, |page| &mut page.size_menu),
            FontKind::Terminal => popover::reap_popup(cx, |page| &mut page.terminal_size_menu),
            FontKind::Code => popover::reap_popup(cx, |page| &mut page.code_size_menu),
        }
    }

    fn dismiss_font_menu(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        *self.font_dismissed_at(kind) = Some(std::time::Instant::now());
        self.close_font_menu(kind, cx);
    }

    fn dismiss_size_menu(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        *self.size_dismissed_at(kind) = Some(std::time::Instant::now());
        self.close_size_menu(kind, cx);
    }

    fn toggle_font_menu(&mut self, kind: FontKind, window: &mut Window, cx: &mut Context<Self>) {
        self.close_other_menus(Some(kind), None, cx);
        let just_dismissed = self
            .font_dismissed_at(kind)
            .take()
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(400));
        if self.font_menu(kind).is_open() {
            self.close_font_menu(kind, cx);
        } else if !just_dismissed {
            // Clear before opening: the resulting `Edited` finds no open menu,
            // so it cannot clobber the highlight we anchor on the next line.
            self.font_search
                .update(cx, |input, cx| input.set_text("", cx));
            let effective = kind.effective(cx);
            self.set_selected_font(kind, effective);
            // Every open starts at the top, and the rail's baseline with it —
            // no reopen flash from the previous session's offset.
            self.font_list_mut(kind).reset();
            self.font_menu_mut(kind).open(());
            window.focus(&self.font_search.read(cx).focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn toggle_size_menu(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        self.close_other_menus(None, Some(kind), cx);
        let just_dismissed = self
            .size_dismissed_at(kind)
            .take()
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(400));
        if self.size_menu(kind).is_open() {
            self.close_size_menu(kind, cx);
        } else if !just_dismissed {
            self.set_selected_size_ix(kind, kind.size_ix(cx));
            self.size_menu_mut(kind).open(());
        }
        cx.notify();
    }

    /// Open on the first navigation key, so ↑↓/Home/End work from the closed
    /// trigger exactly as they do inside the list.
    fn open_size_menu(&mut self, kind: FontKind, cx: &mut Context<Self>) {
        if !self.size_menu(kind).is_open() {
            *self.size_dismissed_at(kind) = None;
            self.toggle_size_menu(kind, cx);
        }
    }

    /// Returns whether the key was consumed. The card and its trigger both
    /// listen, and the trigger's guard re-reads `is_open` — which Enter has
    /// already flipped — so a consumed key must not bubble on.
    fn on_font_key_down(
        &mut self,
        kind: FontKind,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();

        if !self.font_menu(kind).is_open() {
            // Closed: any list key opens the menu, and the filter takes over
            // from there — no stepping through hundreds of rows by hand.
            if matches!(
                key,
                "up" | "down" | "left" | "right" | "home" | "end" | "enter" | "space"
            ) {
                *self.font_dismissed_at(kind) = None;
                self.toggle_font_menu(kind, window, cx);
                return true;
            }
            return false;
        }

        // Open: the filter input owns text and caret keys. Only navigation,
        // commit, and dismiss bubble out to us.
        let availability = typography::availability(cx);
        let choices = self.visible_choices(kind, &availability, cx);
        let modifiers = event.keystroke.modifiers;
        let next = match (
            key,
            popover::classify_key(key, modifiers.platform, modifiers.control),
        ) {
            (_, popover::MenuKey::Up) => {
                step_font(self.selected_font(kind), -1, &choices, kind, &availability)
            }
            (_, popover::MenuKey::Down) => {
                step_font(self.selected_font(kind), 1, &choices, kind, &availability)
            }
            ("home", _) => first_available(&choices, kind, &availability),
            ("end", _) => last_available(&choices, kind, &availability),
            (_, popover::MenuKey::Enter) => {
                self.commit_font(kind, cx);
                return true;
            }
            (_, popover::MenuKey::Escape) => {
                let effective = kind.effective(cx);
                self.set_selected_font(kind, effective);
                self.close_font_menu(kind, cx);
                window.focus(&self.font_focus(kind).clone(), cx);
                cx.notify();
                return true;
            }
            _ => return false,
        };
        self.set_selected_font(kind, next);
        cx.notify();
        true
    }

    fn on_size_key_down(
        &mut self,
        kind: FontKind,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let last = kind.size_count() - 1;
        let current = self.selected_size_ix(kind);
        match event.keystroke.key.as_str() {
            "up" | "left" => {
                self.open_size_menu(kind, cx);
                self.set_selected_size_ix(kind, current.saturating_sub(1));
                cx.notify();
            }
            "down" | "right" => {
                self.open_size_menu(kind, cx);
                self.set_selected_size_ix(kind, current + 1);
                cx.notify();
            }
            "home" => {
                self.open_size_menu(kind, cx);
                self.set_selected_size_ix(kind, 0);
                cx.notify();
            }
            "end" => {
                self.open_size_menu(kind, cx);
                self.set_selected_size_ix(kind, last);
                cx.notify();
            }
            // Activation rides GPUI's key-up keyboard click on the trigger
            // (`activate_size_from_keyboard`). Acting here as well opened the
            // menu on key-down and toggled it shut again on key-up.
            "enter" | "space" => cx.stop_propagation(),
            "escape" => {
                self.set_selected_size_ix(kind, kind.size_ix(cx));
                self.close_size_menu(kind, cx);
                cx.notify();
            }
            _ => {}
        }
    }

    fn activate_size_from_keyboard(
        &mut self,
        kind: FontKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.size_menu(kind).is_open() {
            self.commit_size(kind, window, cx);
        } else {
            *self.size_dismissed_at(kind) = None;
            self.toggle_size_menu(kind, cx);
        }
    }

    fn open_import(&mut self, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            ComposerInput::with_context(
                "Theme file, package.json, or extension folder",
                "PaletteSearch",
                cx,
            )
        });
        let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Edited => {
                let source = this
                    .import_dialog
                    .as_ref()
                    .map(|dialog| PathBuf::from(dialog.input.read(cx).text().trim()));
                if let Some(dialog) = this.import_dialog.as_mut()
                    && dialog
                        .compilation
                        .as_ref()
                        .zip(source.as_ref())
                        .is_some_and(|(compilation, source)| compilation.path != *source)
                {
                    dialog.compilation = None;
                    dialog.selected.clear();
                    dialog.review_variant = None;
                    dialog.error = None;
                    cx.notify();
                }
            }
            ComposerInputEvent::Submitted => {
                if this
                    .import_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.compilation.is_some())
                {
                    this.finish_import(cx);
                } else {
                    this.compile_import(cx);
                }
            }
            _ => {}
        });
        self.import_dialog = Some(ImportDialog {
            input,
            _events: events,
            focus: cx.focus_handle(),
            focus_pending: true,
            mode: InstallMode::Snapshot,
            compilation: None,
            selected: HashSet::new(),
            review_variant: None,
            error: None,
        });
        cx.notify();
    }

    fn compile_import(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.import_dialog.as_mut() else {
            return;
        };
        let source = dialog.input.read(cx).text().trim().to_owned();
        if source.is_empty() {
            dialog.error = Some("Choose a local theme file or extension folder.".into());
            cx.notify();
            return;
        }
        let path = PathBuf::from(&source);
        let family_name = source_name(&path);
        let family_id = format!("custom-{}", slug(&family_name));
        match theme_library::compile(&path, &family_id, &family_name) {
            Ok(compilation) => {
                dialog.selected = compilation
                    .family
                    .variants
                    .iter()
                    .map(|variant| variant.id.clone())
                    .collect();
                // Mapping diagnostics are useful, but they are an advanced
                // inspection surface rather than part of the happy path.
                dialog.review_variant = None;
                dialog.compilation = Some(compilation);
                dialog.error = None;
            }
            Err(error) => dialog.error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn choose_import_source(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("Choose Theme Source".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(mut paths))) => paths.pop(),
                _ => None,
            };
            let Some(path) = path else {
                return;
            };
            let _ = this.update(cx, |page, cx| {
                if let Some(dialog) = page.import_dialog.as_mut() {
                    dialog.input.update(cx, |input, cx| {
                        input.set_text(path.display().to_string(), cx)
                    });
                }
                page.compile_import(cx);
            });
        })
        .detach();
    }

    fn choose_new_thread_background(&mut self, cx: &mut Context<Self>) {
        self.background_error = None;
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose New Thread Composer Background".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(mut paths))) => paths.pop(),
                _ => None,
            };
            let Some(path) = path else {
                return;
            };
            let _ = this.update(cx, |page, cx| {
                page.background_error =
                    crate::settings::install_new_thread_composer_background(&path, cx)
                        .err()
                        .map(SharedString::from);
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_new_thread_background(&mut self, cx: &mut Context<Self>) {
        self.background_error = crate::settings::remove_new_thread_composer_background(cx)
            .err()
            .map(SharedString::from);
        cx.notify();
    }

    fn finish_import(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.import_dialog.as_mut() else {
            return;
        };
        if dialog.selected.is_empty() {
            dialog.error = Some("Select at least one variant to import.".into());
            cx.notify();
            return;
        }
        let Some(compilation) = dialog.compilation.take() else {
            return;
        };
        let selected = dialog.selected.iter().cloned().collect::<Vec<_>>();
        match theme_library::install(compilation.clone(), &selected, dialog.mode, cx) {
            Ok(_) => self.import_dialog = None,
            Err(error) => {
                dialog.compilation = Some(compilation);
                dialog.error = Some(error.to_string().into());
            }
        }
        cx.notify();
    }

    fn toggle_import_variant(&mut self, variant_id: &str, cx: &mut Context<Self>) {
        if let Some(dialog) = self.import_dialog.as_mut()
            && !dialog.selected.remove(variant_id)
        {
            dialog.selected.insert(variant_id.to_owned());
        }
        cx.notify();
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for AppearancePage {
    // The page's rail; each font dropdown's rail goes through
    // [`widgets::rail`], which can serve further scroll hosts on the same view.
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

fn source_name(path: &Path) -> String {
    let path = if path.file_name().and_then(|name| name.to_str()) == Some("package.json") {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    path.file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Custom theme")
        .to_owned()
}

fn slug(value: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            if separator && !result.is_empty() {
                result.push('-');
            }
            result.push(character);
            separator = false;
        } else {
            separator = true;
        }
    }
    if result.is_empty() {
        "theme".into()
    } else {
        result
    }
}

fn step_font(
    current: &UiFontFamily,
    delta: isize,
    choices: &[UiFontFamily],
    kind: FontKind,
    availability: &FontAvailability,
) -> UiFontFamily {
    if choices.is_empty() {
        return current.clone();
    }
    let current = choices
        .iter()
        .position(|family| family == current)
        .unwrap_or_default() as isize;
    let mut ix = current + delta.signum();
    while (0..choices.len() as isize).contains(&ix) {
        let candidate = &choices[ix as usize];
        if kind.is_available_for(availability, candidate) {
            return candidate.clone();
        }
        ix += delta.signum();
    }
    choices[current as usize].clone()
}

/// Narrow and rank families by a typed query: prefix matches first, then
/// substring matches, catalog order preserved within each rank. An empty query
/// keeps the catalog untouched, so bundled entries still sort first.
fn filter_families(query: &str, choices: &[UiFontFamily]) -> Vec<UiFontFamily> {
    if query.trim().is_empty() {
        return choices.to_vec();
    }
    let labels: Vec<&str> = choices.iter().map(UiFontFamily::label).collect();
    popover::filter_indices(query, &labels)
        .into_iter()
        .map(|ix| choices[ix].clone())
        .collect()
}

fn first_available(
    choices: &[UiFontFamily],
    kind: FontKind,
    availability: &FontAvailability,
) -> UiFontFamily {
    choices
        .iter()
        .find(|family| kind.is_available_for(availability, family))
        .cloned()
        .unwrap_or_else(|| fallback_selection(kind))
}

fn last_available(
    choices: &[UiFontFamily],
    kind: FontKind,
    availability: &FontAvailability,
) -> UiFontFamily {
    choices
        .iter()
        .rev()
        .find(|family| kind.is_available_for(availability, family))
        .cloned()
        .unwrap_or_else(|| fallback_selection(kind))
}

/// Highlight target when a kind's catalog offers nothing: System UI is
/// proportional, so the terminal cannot land there.
fn fallback_selection(kind: FontKind) -> UiFontFamily {
    match kind {
        FontKind::Terminal => UiFontFamily::GeistMono,
        _ => UiFontFamily::System,
    }
}

fn format_px(size: f32) -> String {
    if size.fract().abs() < f32::EPSILON {
        format!("{size:.0} px")
    } else {
        format!("{size:.1} px")
    }
}

fn bar(fraction: f32, tone: Hsla) -> gpui::Div {
    div()
        .h(px(5.0))
        .w(gpui::relative(fraction))
        .rounded(px(3.0))
        .bg(tone)
}

fn accent_helper(accent: AccentSelection) -> String {
    match accent {
        AccentSelection::ThemeDefault => {
            "Theme default · Uses the palette's intended color.".into()
        }
        AccentSelection::Preset(preset) => format!(
            "{} · Controls, glyphs, selections, code, and activity.",
            preset.label()
        ),
    }
}

fn surface_label(surface: SurfacePreference) -> &'static str {
    match surface {
        SurfacePreference::ThemeDefault => "Theme default",
        SurfacePreference::Frosted => "Frosted",
        SurfacePreference::Opaque => "Opaque",
    }
}

fn surface_helper(surface: SurfacePreference, resolved: SurfaceTreatment) -> String {
    match surface {
        SurfacePreference::ThemeDefault => format!(
            "Uses this theme's {} default.",
            match resolved {
                SurfaceTreatment::Frosted => "frosted",
                SurfaceTreatment::Opaque => "opaque",
            }
        ),
        SurfacePreference::Frosted => "Theme-colored glass where supported.".into(),
        SurfacePreference::Opaque => "Solid surfaces for every theme.".into(),
    }
}

/// One settings row (spec §3): the wrapping title and description at the
/// left, the controls in the trailing column at the right. When the controls
/// no longer fit beside the text they wrap under it, right-aligned.
fn setting_row(
    theme: &Theme,
    first: bool,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    controls: Vec<AnyElement>,
) -> gpui::Div {
    widgets::card_row(theme, first)
        .child(widgets::row_text(theme, title, Some(description.into())))
        .child(widgets::row_controls().children(controls))
}

/// An error or warning strip as the last row of the section it belongs to
/// (spec §7), spanning the row's content width.
fn strip_row(theme: &Theme, strip: gpui::Div) -> AnyElement {
    widgets::card_row(theme, false)
        .child(strip.mt(px(0.0)).flex_1().min_w_0())
        .into_any_element()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Corners {
    All,
    Left,
    Right,
}

fn miniature(theme: &Theme, corners: Corners) -> AnyElement {
    let line = theme.text.opacity(0.22);
    let strong = theme.text.opacity(0.34);
    let r = px(widgets::OPTION_CARD_RADIUS);
    let root = div().size_full().flex().flex_row().bg(theme.surface);
    let root = match corners {
        Corners::All => root.rounded(r),
        Corners::Left => root.rounded_tl(r).rounded_bl(r),
        Corners::Right => root.rounded_tr(r).rounded_br(r),
    };
    root.child(
        div()
            .w(px(44.0))
            .h_full()
            .flex_none()
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(7.0))
            .px(px(8.0))
            .pt(px(14.0))
            .child(bar(0.70, strong))
            .child(bar(1.0, line))
            .child(bar(0.85, line))
            .child(bar(1.0, line)),
    )
    .child(
        div()
            .flex_1()
            .min_w_0()
            .my(px(8.0))
            .mr(px(8.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.bg)
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(7.0))
            .p(px(10.0))
            .child(bar(0.62, strong))
            .child(bar(0.88, line))
            .child(bar(0.76, line))
            .child(bar(0.52, line)),
    )
    .into_any_element()
}

fn miniature_split(
    themes: &ThemeSelection,
    accent: AccentSelection,
    surface: SurfacePreference,
) -> AnyElement {
    let light = Theme::for_selection(Appearance::Light, &themes.light, accent, surface);
    let dark = Theme::for_selection(Appearance::Dark, &themes.dark, accent, surface);
    div()
        .size_full()
        .flex()
        .flex_row()
        .child(
            div()
                .w_1_2()
                .h_full()
                .overflow_hidden()
                .child(miniature(&light, Corners::Left)),
        )
        .child(
            div()
                .w_1_2()
                .h_full()
                .overflow_hidden()
                .child(miniature(&dark, Corners::Right)),
        )
        .into_any_element()
}

fn preview(
    mode: AppearanceMode,
    themes: &ThemeSelection,
    accent: AccentSelection,
    surface: SurfacePreference,
) -> AnyElement {
    match mode {
        AppearanceMode::System => miniature_split(themes, accent, surface),
        AppearanceMode::Light => miniature(
            &Theme::for_selection(Appearance::Light, &themes.light, accent, surface),
            Corners::All,
        ),
        AppearanceMode::Dark => miniature(
            &Theme::for_selection(Appearance::Dark, &themes.dark, accent, surface),
            Corners::All,
        ),
    }
}

fn model_appearance(appearance: Appearance) -> luna_theme::Appearance {
    match appearance {
        Appearance::Dark => luna_theme::Appearance::Dark,
        Appearance::Light => luna_theme::Appearance::Light,
    }
}

fn palette_preview(theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .w(ui_rems(30.0))
        .h(ui_rems(18.0))
        .rounded(px(5.0))
        .overflow_hidden()
        .border_1()
        .border_color(theme.border)
        .flex()
        .child(div().w_1_3().h_full().bg(theme.surface))
        .child(div().w_1_3().h_full().bg(theme.bg))
        .child(div().w_1_3().h_full().bg(theme.accent))
}

/// A labelled kit button: `Md` in rows and section headers, `Sm` in dense
/// list rows, `Lg` in dialogs (spec §5).
fn labelled_button(
    theme: &Theme,
    kind: ButtonKind,
    size: ControlSize,
    label: &str,
    id: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    crate::button::button(id, kind, size, true, theme)
        .aria_label(SharedString::from(label.to_string()))
        .child(SharedString::from(label.to_string()))
}

/// The Secondary row action.
fn compact_action(
    theme: &Theme,
    label: &str,
    id: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    labelled_button(theme, ButtonKind::Secondary, ControlSize::Md, label, id)
}

/// The Secondary dialog action (Cancel, Browse…).
fn dialog_action(
    theme: &Theme,
    label: &str,
    id: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    labelled_button(theme, ButtonKind::Secondary, ControlSize::Lg, label, id)
}

/// A dialog card sized in interface rems, but never wider or taller than the
/// window, so a large Interface size cannot push it off screen.
fn sized_dialog(theme: &Theme, viewport: gpui::Size<gpui::Pixels>, width: f32) -> gpui::Div {
    popover::dialog_card(theme)
        .w(ui_rems(width))
        .p(ui_rems(20.0))
        .max_w(px((f32::from(viewport.width) - 48.0).max(280.0)))
        .max_h(px((f32::from(viewport.height) - 48.0).max(240.0)))
}

/// A dialog's button row: the actions at the right, wrapping when a narrow
/// window or a large Interface size cannot fit them on one line.
fn dialog_footer() -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_end()
        .gap(ui_rems(8.0))
}

/// A sub-heading inside a dialog ("Source", "Detected themes"): the row title
/// style, so dialogs share the settings type ladder.
fn dialog_label(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    widgets::row_title(theme, label)
}

fn import_scene_preview(variant: &luna_theme::ThemeVariant) -> AnyElement {
    let theme = Theme::from_variant(
        variant,
        AccentSelection::ThemeDefault,
        SurfacePreference::ThemeDefault,
    );
    // The colours here are the imported variant's own: this is a preview of
    // that theme, not page chrome. The sizes are interface rems so the code
    // sample never clips at a large Interface size.
    div()
        .w_full()
        .h(ui_rems(86.0))
        .flex()
        .gap(ui_rems(8.0))
        .child(
            div()
                .w(ui_rems(152.0))
                .h_full()
                .overflow_hidden()
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border)
                .child(miniature(&theme, Corners::All)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_hidden()
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.bg)
                .p(ui_rems(9.0))
                .flex()
                .flex_col()
                .gap(ui_rems(6.0))
                .child(
                    div()
                        .text_size(ui_rems(11.0))
                        .font_family(theme.font_mono.clone())
                        .child(
                            div()
                                .text_color(theme.syntax.keyword)
                                .child("fn ")
                                .child(div().text_color(theme.syntax.function).child("preview"))
                                .child(div().text_color(theme.syntax.punctuation).child("() {")),
                        ),
                )
                .child(
                    div()
                        .text_size(ui_rems(11.0))
                        .font_family(theme.font_mono.clone())
                        .text_color(theme.syntax.string)
                        .child("  \"Theme mapping\""),
                )
                .child(
                    div()
                        .mt_auto()
                        .h(ui_rems(12.0))
                        .flex_none()
                        .flex()
                        .rounded(px(3.0))
                        .overflow_hidden()
                        .children(
                            theme
                                .terminal
                                .ansi
                                .iter()
                                .take(8)
                                .map(|color| div().flex_1().h_full().bg(*color)),
                        ),
                ),
        )
        .child(
            div()
                .w(ui_rems(84.0))
                .h_full()
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface)
                .p(ui_rems(8.0))
                .flex()
                .flex_col()
                .gap(ui_rems(6.0))
                .child(
                    div()
                        .h(ui_rems(12.0))
                        .rounded(px(3.0))
                        .bg(theme.diff_add.opacity(0.35)),
                )
                .child(
                    div()
                        .h(ui_rems(12.0))
                        .rounded(px(3.0))
                        .bg(theme.diff_del.opacity(0.35)),
                )
                .child(
                    div()
                        .h(ui_rems(12.0))
                        .rounded(px(3.0))
                        .bg(theme.accent_wash),
                ),
        )
        .into_any_element()
}

fn report_panel(theme: &Theme, report: &ImportReport) -> gpui::Stateful<gpui::Div> {
    let summary = format!(
        "{} mapped · {} adjusted · {} inferred/fallback · {} unsupported · {} warnings · {} validation",
        report.mappings.len(),
        report.adjustments.len(),
        report.fallbacks.len(),
        report.dropped.len(),
        report.warnings.len(),
        report.validation.len(),
    );
    div()
        .id(SharedString::from(format!(
            "theme-report-{}",
            report.source_hash
        )))
        .mt(ui_rems(8.0))
        .w_full()
        .max_h(ui_rems(168.0))
        .overflow_y_scroll()
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.fill(FillTier::Quaternary))
        .p(ui_rems(10.0))
        .type_style(TextStyle::Caption)
        .line_height(ui_rems(16.0))
        .text_color(theme.readable_muted(0.85))
        .child(div().text_color(theme.text).child(summary))
        .children(report.adjustments.iter().map(|adjustment| {
            div().mt(ui_rems(4.0)).child(SharedString::from(format!(
                "Adjusted · {} {} → {} · {}",
                adjustment.luna_role, adjustment.original, adjustment.resolved, adjustment.reason
            )))
        }))
        .children(report.fallbacks.iter().map(|message| {
            div()
                .mt(ui_rems(4.0))
                .child(SharedString::from(format!("Fallback · {message}")))
        }))
        .children(report.warnings.iter().map(|message| {
            div()
                .mt(ui_rems(4.0))
                .child(SharedString::from(format!("Warning · {message}")))
        }))
        .children(report.validation.iter().map(|issue| {
            div().mt(ui_rems(4.0)).child(SharedString::from(format!(
                "Validation {:?} {:?} · {}",
                issue.category, issue.severity, issue.message
            )))
        }))
        .children(report.dropped.iter().map(|message| {
            div()
                .mt(ui_rems(4.0))
                .child(SharedString::from(format!("Unsupported · {message}")))
        }))
        .children(report.mappings.iter().map(|mapping| {
            div().mt(ui_rems(4.0)).child(SharedString::from(format!(
                "{} ← {}",
                mapping.luna_role, mapping.vscode_key
            )))
        }))
}

fn accent_swatch(
    page_theme: &Theme,
    selection: AccentSelection,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    let swatch_theme = Theme::for_selection(
        page_theme.appearance,
        page_theme.variant_id.as_ref(),
        selection,
        page_theme.surface_preference,
    );
    // Swatch geometry is in interface rems so the row of swatches keeps its
    // proportion to the row text at every Interface size.
    let sample = match selection {
        AccentSelection::ThemeDefault => div()
            .size_full()
            .rounded(px(6.0))
            .bg(swatch_theme.accent_wash)
            .flex()
            .items_center()
            .justify_center()
            .gap(ui_rems(2.0))
            .child(
                div()
                    .w(ui_rems(4.0))
                    .h(ui_rems(13.0))
                    .rounded(px(2.0))
                    .bg(swatch_theme.glyph.light),
            )
            .child(
                div()
                    .w(ui_rems(4.0))
                    .h(ui_rems(16.0))
                    .rounded(px(2.0))
                    .bg(swatch_theme.glyph.mid),
            )
            .child(
                div()
                    .w(ui_rems(4.0))
                    .h(ui_rems(11.0))
                    .rounded(px(2.0))
                    .bg(swatch_theme.glyph.deep),
            ),
        AccentSelection::Preset(_) => div().size_full().rounded(px(6.0)).bg(swatch_theme.accent),
    };
    div()
        .id(SharedString::from(format!("accent-{}", selection.label())))
        .role(gpui::Role::Button)
        .aria_label(format!("Accent color: {}", selection.label()))
        .aria_toggled(if selected {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        })
        .tab_index(0)
        .flex_none()
        .rounded(px(12.0))
        .cursor_pointer()
        .focus_visible({
            let ring = page_theme.focus_ring();
            move |style| crate::theme::focus_ring(style, ring)
        })
        // A ring, not a tab-style underline, marks the chosen accent
        // (audit DS-16); the ring takes the swatch's own accent.
        .tooltip(popover::text_tooltip(selection.label()))
        .child(crate::button::selection_ring(
            selected,
            8.0,
            &swatch_theme,
            div()
                .size(ui_rems(30.0))
                .p(ui_rems(2.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(page_theme.border)
                .bg(page_theme.fill(FillTier::Quaternary))
                .child(sample),
        ))
}

impl AppearancePage {
    fn theme_menu(&self, appearance: Appearance) -> &Popup<()> {
        match appearance {
            Appearance::Light => &self.light_theme_menu,
            Appearance::Dark => &self.dark_theme_menu,
        }
    }

    fn theme_menu_mut(&mut self, appearance: Appearance) -> &mut Popup<()> {
        match appearance {
            Appearance::Light => &mut self.light_theme_menu,
            Appearance::Dark => &mut self.dark_theme_menu,
        }
    }

    fn close_theme_menu(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        if !self.theme_menu_mut(appearance).begin_close() {
            return;
        }
        match appearance {
            Appearance::Light => popover::reap_popup(cx, |page| &mut page.light_theme_menu),
            Appearance::Dark => popover::reap_popup(cx, |page| &mut page.dark_theme_menu),
        }
    }

    fn toggle_theme_menu(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        if self.theme_menu(appearance).is_open() {
            self.close_theme_menu(appearance, cx);
        } else {
            let other = if appearance.is_light() {
                Appearance::Dark
            } else {
                Appearance::Light
            };
            self.close_theme_menu(other, cx);
            self.theme_menu_mut(appearance).open(());
        }
        cx.notify();
    }

    fn render_font_picker(
        &mut self,
        kind: FontKind,
        theme: &Theme,
        availability: &FontAvailability,
        fixed: SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let slug = kind.slug();
        // The scroll helpers key off `&'static str`, so the ids are spelled
        // out rather than formatted from the slug.
        let (host_id, list_id, rail_id) = match kind {
            FontKind::Ui => (
                "interface-font-host",
                "interface-font-scroll",
                "interface-font-scrollbar",
            ),
            FontKind::Terminal => (
                "terminal-font-host",
                "terminal-font-scroll",
                "terminal-font-scrollbar",
            ),
            FontKind::Code => ("code-font-host", "code-font-scroll", "code-font-scrollbar"),
        };
        let effective = kind.effective(cx);
        let selected = self.selected_font(kind).clone();
        let visible = self.visible_choices(kind, availability, cx);
        let filtered = !self.font_search.read(cx).text().trim().is_empty();
        let rows: Vec<AnyElement> = visible
            .into_iter()
            .enumerate()
            .map(|(ix, family)| {
                let available = kind.is_available_for(availability, &family);
                let active = family == effective;
                let focused = family == selected;
                let label = SharedString::from(family.label().to_owned());
                let hover_family = family.clone();
                popover::menu_row_nav(theme, active, focused)
                    .id(SharedString::from(format!("{slug}-font-option-{ix}")))
                    .when(available, |row| {
                        row.on_mouse_move(cx.listener(
                            move |this, _: &gpui::MouseMoveEvent, _, cx| {
                                if *this.selected_font(kind) != hover_family {
                                    this.set_selected_font(kind, hover_family.clone());
                                    cx.notify();
                                }
                            },
                        ))
                    })
                    .role(gpui::Role::Button)
                    .aria_label(format!("{} family: {}", kind.label(), family.label()))
                    .when(available, |row| {
                        row.on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.set_selected_font(kind, family.clone());
                            this.commit_font(kind, cx);
                        }))
                    })
                    .when(!available, |row| row.opacity(0.45))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .into_any_element()
            })
            .collect();

        let list: AnyElement = if rows.is_empty() {
            div()
                .px(ui_rems(8.0))
                .py(ui_rems(6.0))
                .type_style(TextStyle::Footnote)
                .text_color(theme.for_popup().readable_muted(0.85))
                .child(SharedString::from(if filtered {
                    "No matching fonts"
                } else {
                    "No fonts"
                }))
                .into_any_element()
        } else {
            // Card-bleed scroll host (see [`popover::menu_scroll_host`]): the
            // rail mounts as a sibling of the scroller, above its clip. The
            // bleed stays horizontal — the search input sits above the list.
            let rail = widgets::rail(self.font_list_mut(kind), rail_id, theme, cx, move |page| {
                page.font_list_mut(kind)
            });
            let scroll = self.font_list_mut(kind).scroll.clone();
            popover::menu_scroll_host(host_id)
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if this.font_list_mut(kind).set_list_hovered(*hovered) {
                        cx.notify();
                    }
                }))
                .child(
                    popover::menu_scroll_list(list_id, &scroll)
                        .max_h(ui_rems(FAMILY_MENU_MAX_HEIGHT))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .children(rows),
                )
                .children(rail)
                .into_any_element()
        };

        // The key handler sits on the card, not just the trigger: focus moves
        // into the filter input, and `PaletteSearch` lets arrows/Enter/Escape
        // bubble to exactly this ancestor.
        let menu = popover::popover_card(theme)
            .w(ui_rems(FAMILY_MENU_WIDTH))
            .font_family(fixed)
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| this.dismiss_font_menu(kind, cx)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if this.on_font_key_down(kind, event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .child(popover::search_input_frame(
                theme,
                self.font_search.clone().into_any_element(),
            ))
            .child(list)
            .into_any_element();

        let open = self.font_menu(kind).is_open();
        let closing = self.font_menu(kind).closing_since();
        // One pop-up recipe for every picker on the page (audit DS-02).
        crate::button::select_frame(
            format!("{slug}-font-dropdown"),
            ControlSize::Md,
            open,
            true,
            theme,
        )
        .aria_label(format!("{} family: {}", kind.label(), effective.label()))
        .relative()
        .w(ui_rems(FAMILY_PICKER_WIDTH))
        .track_focus(self.font_focus(kind))
        // Only the closed state: once open, the card above owns these keys
        // and stops their propagation before they reach us.
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            if !this.font_menu(kind).is_open() {
                this.on_font_key_down(kind, event, window, cx);
            }
        }))
        .on_click(cx.listener(move |this, _, window, cx| {
            window.focus(&this.font_focus(kind).clone(), cx);
            this.toggle_font_menu(kind, window, cx);
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(SharedString::from(effective.label().to_owned())),
        )
        .child(crate::button::select_chevron(theme))
        .when_some(self.font_menu(kind).get(), |trigger, _| {
            trigger.child(popover::anchored_menu_below(
                SharedString::from(format!("{slug}-font-menu")),
                menu,
                closing,
            ))
        })
        .into_any_element()
    }

    /// The size dropdown, identical in shape to the family picker beside it.
    /// Every kind picks from a discrete ladder, so all three rows read alike.
    fn render_size_picker(
        &mut self,
        kind: FontKind,
        theme: &Theme,
        fixed: SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let slug = kind.slug();
        let labels = kind.size_labels();
        let current = kind.size_ix(cx);
        let selected = self.selected_size_ix(kind);
        let rows: Vec<AnyElement> = labels
            .iter()
            .enumerate()
            .map(|(ix, label)| {
                popover::menu_row_nav(theme, ix == current, ix == selected)
                    .id(SharedString::from(format!("{slug}-font-size-option-{ix}")))
                    .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                        if this.selected_size_ix(kind) != ix {
                            this.set_selected_size_ix(kind, ix);
                            cx.notify();
                        }
                    }))
                    .role(gpui::Role::Button)
                    .aria_label(format!("{} size: {}", kind.label(), label))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.set_selected_size_ix(kind, ix);
                        this.commit_size(kind, window, cx);
                    }))
                    .child(div().flex_1().child(label.clone()))
                    .into_any_element()
            })
            .collect();

        let menu = popover::popover_card(theme)
            .w(ui_rems(SIZE_MENU_WIDTH))
            .font_family(fixed)
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| this.dismiss_size_menu(kind, cx)))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(rows)
            .into_any_element();

        let open = self.size_menu(kind).is_open();
        let closing = self.size_menu(kind).closing_since();
        crate::button::select_frame(
            format!("{slug}-font-size-dropdown"),
            ControlSize::Md,
            open,
            true,
            theme,
        )
        .aria_label(format!("{} size: {}", kind.label(), labels[current]))
        .relative()
        .w(ui_rems(SIZE_PICKER_WIDTH))
        .track_focus(self.size_focus(kind))
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            this.on_size_key_down(kind, event, window, cx)
        }))
        .on_click(
            cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                if event.is_keyboard() {
                    this.activate_size_from_keyboard(kind, window, cx);
                    return;
                }
                window.focus(&this.size_focus(kind).clone(), cx);
                this.toggle_size_menu(kind, cx);
            }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(labels[current].clone()),
        )
        .child(crate::button::select_chevron(theme))
        .when_some(self.size_menu(kind).get(), |trigger, _| {
            trigger.child(popover::anchored_menu_below(
                SharedString::from(format!("{slug}-font-size-menu")),
                menu,
                closing,
            ))
        })
        .into_any_element()
    }

    fn render_theme_selector(
        &mut self,
        appearance_kind: Appearance,
        selections: &ThemeSelection,
        theme: &Theme,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let registry = ThemeRegistry::active();
        let selected_id = selections
            .variant_id(model_appearance(appearance_kind))
            .to_owned();
        let selected_variant = registry
            .variant(&selected_id)
            .or_else(|| {
                registry
                    .variants_for(model_appearance(appearance_kind))
                    .next()
            })
            .expect("the built-in registry has both appearances");
        let selected_theme = Theme::for_selection(
            appearance_kind,
            &selected_variant.id,
            AccentSelection::ThemeDefault,
            theme.surface_preference,
        );
        let open = self.theme_menu(appearance_kind).is_open();

        let mut trigger = crate::button::select_frame(
            format!(
                "{}-theme-selector",
                if appearance_kind.is_light() {
                    "light"
                } else {
                    "dark"
                }
            ),
            ControlSize::Md,
            open,
            true,
            theme,
        )
        .aria_label(format!(
            "{} theme: {}",
            if appearance_kind.is_light() {
                "Light"
            } else {
                "Dark"
            },
            selected_variant.name
        ))
        .relative()
        .w(ui_rems(THEME_PICKER_WIDTH))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |this, _, _, _| {
                this.theme_menu_mut(appearance_kind).note_trigger_press();
            }),
        )
        .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
            // GPUI's Enter/Space keyboard click has no mouse-down press
            // note, so the press check would always reopen; toggle instead.
            if event.is_keyboard() {
                this.toggle_theme_menu(appearance_kind, cx);
            } else if this.theme_menu_mut(appearance_kind).take_press_was_open() {
                this.close_theme_menu(appearance_kind, cx);
            } else {
                let other = if appearance_kind.is_light() {
                    Appearance::Dark
                } else {
                    Appearance::Light
                };
                this.close_theme_menu(other, cx);
                this.theme_menu_mut(appearance_kind).open(());
            }
            cx.notify();
        }))
        .on_key_down(crate::swallow_activation_key)
        .child(palette_preview(&selected_theme))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(SharedString::from(selected_variant.name.clone())),
        )
        .child(crate::button::select_chevron(theme));

        if self.theme_menu(appearance_kind).get().is_some() {
            let closing = self.theme_menu(appearance_kind).closing_since();
            let heading = if appearance_kind.is_light() {
                "Light themes"
            } else {
                "Dark themes"
            };
            // A family with many variants (20 dark themes today) grows taller
            // than a short window at a large Interface size: the menu scrolls
            // rather than running off screen. The anchored layer snaps it
            // inside the window's 8 px margin, so the budget leaves room for
            // that margin and the trigger.
            let max_height = px((f32::from(viewport.height) - 96.0).max(160.0));
            let menu = popover::popover_card(theme)
                .id(SharedString::from(format!(
                    "appearance-{}-theme-menu-card",
                    if appearance_kind.is_light() {
                        "light"
                    } else {
                        "dark"
                    }
                )))
                .w(ui_rems(THEME_MENU_WIDTH))
                .max_h(max_height)
                .overflow_y_scroll()
                .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    this.close_theme_menu(appearance_kind, cx);
                    cx.notify();
                }))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(popover::menu_heading(theme, heading))
                .children(
                    registry
                        .variants_for(model_appearance(appearance_kind))
                        .enumerate()
                        .map(|(index, variant)| {
                            let id = variant.id.clone();
                            let name = variant.name.clone();
                            let active = id == selected_id;
                            let sample = Theme::for_selection(
                                appearance_kind,
                                &id,
                                AccentSelection::ThemeDefault,
                                theme.surface_preference,
                            );
                            popover::menu_row(
                                theme,
                                active,
                                SharedString::from(format!(
                                    "appearance-theme-menu-{appearance_kind:?}-{index}"
                                )),
                            )
                            .id(SharedString::from(format!(
                                "appearance-theme-row-{appearance_kind:?}-{index}"
                            )))
                            .role(gpui::Role::Button)
                            .aria_label(format!(
                                "{} theme: {}",
                                if appearance_kind.is_light() {
                                    "Light"
                                } else {
                                    "Dark"
                                },
                                name
                            ))
                            .aria_toggled(if active {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .tab_index(0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                appearance::set_theme(appearance_kind, id.clone(), cx);
                                this.close_theme_menu(appearance_kind, cx);
                                cx.notify();
                            }))
                            .on_key_down(crate::swallow_activation_key)
                            .child(palette_preview(&sample))
                            .child(div().flex_1().min_w_0().truncate().child(name))
                            .when(active, |row| {
                                row.child(
                                    icons::icon(icons::CHECK)
                                        .size(ui_rems(14.0))
                                        .flex_none()
                                        .text_color(theme.selected_row_icon(theme.accent)),
                                )
                            })
                        }),
                )
                .into_any_element();
            trigger = trigger.child(popover::anchored_menu_below(
                SharedString::from(format!(
                    "appearance-{}-theme-menu",
                    if appearance_kind.is_light() {
                        "light"
                    } else {
                        "dark"
                    }
                )),
                menu,
                closing,
            ));
        }

        trigger.into_any_element()
    }

    fn render_import_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        {
            let dialog = self.import_dialog.as_mut()?;
            if std::mem::take(&mut dialog.focus_pending) {
                let input_focus = dialog.input.focus_handle(cx);
                window.focus(&input_focus, cx);
            }
        }
        let dialog = self.import_dialog.as_ref()?;
        let input = dialog.input.clone();
        let focus = dialog.focus.clone();
        let mode = dialog.mode;
        let compilation = dialog.compilation.clone();
        let selected = dialog.selected.clone();
        let review_variant = dialog.review_variant.clone();
        let error = dialog.error.clone();
        let ready = compilation.is_some() && !selected.is_empty();

        // How the import stays current: a two-option choice, so a segmented
        // control (spec §5), with the chosen option explained underneath.
        let modes = [
            (
                InstallMode::Snapshot,
                "Import a copy",
                "Works independently from the original file.",
            ),
            (
                InstallMode::Link,
                "Link to source",
                "Reloads changes from the file on disk.",
            ),
        ];
        let mode_ix = modes
            .iter()
            .position(|(value, _, _)| *value == mode)
            .unwrap_or(0);
        let mode_control = crate::button::segmented(
            "theme-import-mode",
            modes
                .iter()
                .map(|(_, label, _)| Segment::new(slug(label), *label))
                .collect(),
            mode_ix,
            320.0,
            ControlSize::Lg,
            theme,
            cx.listener(move |this, ix: &usize, _, cx| {
                if let Some(dialog) = this.import_dialog.as_mut() {
                    dialog.mode = modes[(*ix).min(modes.len() - 1)].0;
                }
                cx.notify();
            }),
        );

        let mut main = div()
            .id("theme-import-main")
            .min_h_0()
            .overflow_y_scroll()
            .px(ui_rems(20.0))
            .pb(ui_rems(20.0))
            .flex()
            .flex_col()
            .gap(ui_rems(8.0))
            .child(dialog_label(theme, "Source"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(8.0))
                    .child(
                        popover::dialog_field(input.into_any_element())
                            .flex_1()
                            .min_w_0()
                            .min_h(ui_rems(ControlSize::Lg.height()))
                            .py(px(0.0))
                            .flex()
                            .items_center(),
                    )
                    .child(
                        dialog_action(theme, "Browse…", "theme-import-browse")
                            .on_click(cx.listener(|this, _, _, cx| this.choose_import_source(cx))),
                    ),
            )
            .child(dialog_label(theme, "Keep it up to date").mt(ui_rems(12.0)))
            .child(mode_control)
            .child(widgets::row_description(theme, modes[mode_ix].2));

        if let Some(ref compilation) = compilation {
            let count = compilation.family.variants.len();
            main = main.child(
                div()
                    .mt(ui_rems(12.0))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_baseline()
                    .justify_between()
                    .gap_x(ui_rems(12.0))
                    .child(dialog_label(theme, "Detected themes"))
                    .child(
                        div()
                            .type_style(TextStyle::Caption)
                            .text_color(theme.readable_muted(0.85))
                            .child(SharedString::from(format!(
                                "{count} variant{}",
                                if count == 1 { "" } else { "s" }
                            ))),
                    ),
            );
            let mut list = widgets::section_card(theme).mt(px(0.0));
            for (ix, variant) in compilation.family.variants.iter().enumerate() {
                let variant_id = variant.id.clone();
                let selected_now = selected.contains(&variant_id);
                let review_open = review_variant.as_deref() == Some(variant_id.as_str());
                let appearance_label = if variant.appearance.is_dark() {
                    "Dark"
                } else {
                    "Light"
                };
                let report = compilation.reports.get(&variant.id);
                let sample = Theme::from_variant(
                    variant,
                    AccentSelection::ThemeDefault,
                    SurfacePreference::ThemeDefault,
                );
                let details = labelled_button(
                    theme,
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    if review_open {
                        "Hide details"
                    } else {
                        "Details"
                    },
                    format!("theme-import-review-{variant_id}"),
                )
                .aria_expanded(review_open)
                .on_click(cx.listener({
                    let variant_id = variant_id.clone();
                    move |this, _, _, cx| {
                        if let Some(dialog) = this.import_dialog.as_mut() {
                            dialog.review_variant =
                                if dialog.review_variant.as_deref() == Some(variant_id.as_str()) {
                                    None
                                } else {
                                    Some(variant_id.clone())
                                };
                        }
                        cx.notify();
                    }
                }));
                let toggle = widgets::switch_control(
                    SharedString::from(format!("theme-import-select-{variant_id}")),
                    format!("Import {}", variant.name),
                    selected_now,
                    theme,
                    cx,
                    {
                        let variant_id = variant_id.clone();
                        move |this, cx| this.toggle_import_variant(&variant_id, cx)
                    },
                );
                list = list.child(
                    widgets::card_row(theme, ix == 0)
                        .child(palette_preview(&sample))
                        .child(widgets::row_text(
                            theme,
                            variant.name.clone(),
                            Some(appearance_label.into()),
                        ))
                        .child(widgets::row_controls().child(details).child(toggle))
                        .when(review_open, |row| {
                            row.child(
                                widgets::row_block()
                                    .flex_col()
                                    .child(import_scene_preview(variant))
                                    .when_some(report, |block, report| {
                                        block.child(report_panel(theme, report))
                                    }),
                            )
                        }),
                );
            }
            main = main.child(list);
            for failure in &compilation.failures {
                main = main.child(
                    widgets::warning_strip(
                        theme,
                        format!(
                            "{} could not be compiled · {}",
                            failure.name, failure.message
                        ),
                    )
                    .mt(px(0.0)),
                );
            }
        } else {
            main = main.child(
                div()
                    .mt(ui_rems(4.0))
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(ui_rems(6.0))
                    .type_style(TextStyle::Footnote)
                    .text_color(theme.readable_muted(0.85))
                    .child(
                        icons::icon(icons::INFO_CIRCLE)
                            .size(ui_rems(14.0))
                            .mt(ui_rems(1.0))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child("LUNA finds light and dark variants automatically."),
                    ),
            );
        }

        if let Some(error) = error {
            main = main.child(widgets::error_strip(theme, error).mt(ui_rems(4.0)));
        }

        let header = div()
            .flex_none()
            .px(ui_rems(20.0))
            .pt(ui_rems(18.0))
            .pb(ui_rems(16.0))
            .flex()
            .flex_row()
            .items_start()
            .gap(ui_rems(16.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(ui_rems(4.0))
                    .child(popover::dialog_title(theme, "Add a theme"))
                    .child(popover::dialog_body(
                        theme,
                        "Import a local VS Code theme into your library, or keep it linked to its source.",
                    )),
            )
            .child(
                crate::button::icon_button(
                    "theme-import-close",
                    icons::CLOSE,
                    ControlSize::Md,
                    true,
                    theme,
                )
                .aria_label("Close theme import")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.import_dialog = None;
                    cx.notify();
                })),
            );

        let action_label = if compilation.is_some() {
            "Import selected"
        } else {
            "Analyze theme"
        };
        // Importing needs at least one selected variant; until then the
        // action shows the kit's disabled state and takes no clicks.
        let action_enabled = compilation.is_none() || ready;
        let footer = dialog_footer()
            .flex_none()
            .border_t_1()
            .border_color(theme.border)
            .px(ui_rems(20.0))
            .py(ui_rems(12.0))
            .child(
                dialog_action(theme, "Cancel", "theme-import-cancel").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.import_dialog = None;
                        cx.notify();
                    },
                )),
            )
            .child(
                crate::button::button(
                    "theme-import-action",
                    ButtonKind::Primary,
                    ControlSize::Lg,
                    action_enabled,
                    theme,
                )
                .aria_label(action_label)
                .child(action_label)
                .when(action_enabled, |button| {
                    // The kit swallows the Enter/Space key-down, so GPUI's
                    // key-up click is the only activation: a key-down Analyze
                    // followed by the click used to import before review.
                    button.on_click(cx.listener(move |this, _, _, cx| {
                        if this
                            .import_dialog
                            .as_ref()
                            .is_some_and(|dialog| dialog.compilation.is_some())
                        {
                            this.finish_import(cx);
                        } else {
                            this.compile_import(cx);
                        }
                    }))
                }),
            );

        let card = sized_dialog(theme, viewport, 560.0)
            .id("theme-import-card")
            .p(px(0.0))
            .overflow_hidden()
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                match popover::classify_key(
                    event.keystroke.key.as_str(),
                    event.keystroke.modifiers.platform,
                    event.keystroke.modifiers.control,
                ) {
                    popover::MenuKey::Escape => {
                        this.import_dialog = None;
                        cx.notify();
                    }
                    popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                        if this
                            .import_dialog
                            .as_ref()
                            .is_some_and(|dialog| dialog.compilation.is_some())
                        {
                            this.finish_import(cx);
                        } else {
                            this.compile_import(cx);
                        }
                    }
                    _ => {}
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.import_dialog = None;
                cx.notify();
            }))
            .child(header)
            .child(main)
            .child(footer)
            .into_any_element();

        Some(popover::modal("theme-import-dialog", viewport, card))
    }

    fn render_review_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let entry_id = self.review_entry.as_ref()?;
        let entry = theme_library::entries(cx)
            .into_iter()
            .find(|entry| &entry.id == entry_id)?;
        let mut card = sized_dialog(theme, viewport, 620.0)
            .id("theme-review-card")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.review_entry = None;
                    cx.notify();
                }
            }))
            .overflow_y_scroll()
            .child(popover::dialog_title(theme, "Theme mapping"))
            .child(
                popover::dialog_body(theme, format!("{} · {}", entry.name, entry.source.label()))
                    .mt(ui_rems(6.0)),
            );
        for variant in &entry.family.variants {
            card = card
                .child(
                    dialog_label(theme, variant.name.clone())
                        .mt(ui_rems(16.0))
                        .mb(ui_rems(8.0)),
                )
                .child(import_scene_preview(variant));
            if let Some(report) = entry.reports.get(&variant.id) {
                card = card.child(report_panel(theme, report));
            }
        }
        card = card.child(
            dialog_footer().mt(ui_rems(20.0)).child(
                popover::btn_primary(theme, "Done")
                    .id("theme-review-close")
                    .aria_label("Done reviewing theme")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.review_entry = None;
                        cx.notify();
                    })),
            ),
        );
        Some(popover::modal(
            "theme-review-dialog",
            viewport,
            card.into_any_element(),
        ))
    }

    /// Move focus into a confirmation card the first frame it shows.
    fn take_confirm_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.confirm_focus_pending) {
            window.focus(&self.confirm_focus, cx);
        }
    }

    fn render_reset_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.reset_confirmation_open {
            return None;
        }
        self.take_confirm_focus(window, cx);
        let card = sized_dialog(theme, viewport, 440.0)
            .id("appearance-reset-card")
            .track_focus(&self.confirm_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.reset_confirmation_open = false;
                    cx.notify();
                }
            }))
            .child(popover::dialog_title(theme, "Reset appearance?"))
            .child(
                popover::dialog_body(
                    theme,
                    "Restore the first-install themes, fonts, accent, glass, width, wrapping, and diff choices. The new-thread image is removed. Imported themes stay in your library.",
                )
                .mt(ui_rems(8.0)),
            )
            .child(
                dialog_footer()
                    .mt(ui_rems(20.0))
                    .child(
                        dialog_action(theme, "Cancel", "appearance-reset-cancel").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.reset_confirmation_open = false;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        popover::btn_primary(theme, "Reset appearance")
                            .id("appearance-reset-confirm")
                            .aria_label("Reset appearance to first-install defaults")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_to_defaults(window, cx);
                            })),
                    ),
            );
        Some(popover::modal(
            "appearance-reset-dialog",
            viewport,
            card.into_any_element(),
        ))
    }

    /// Confirmation for removing a library theme (spec §5: a destructive
    /// action is a Secondary button with a confirm). An imported snapshot is
    /// gone for good once removed.
    fn render_remove_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (id, name) = self.remove_confirmation.clone()?;
        self.take_confirm_focus(window, cx);
        let linked = theme_library::entries(cx)
            .iter()
            .any(|entry| entry.id == id && entry.source.is_linked());
        let body = if linked {
            format!("\u{201c}{name}\u{201d} leaves your theme library. Its source file stays where it is.")
        } else {
            format!(
                "\u{201c}{name}\u{201d} leaves your theme library. Import it again to get it back."
            )
        };
        let card = sized_dialog(theme, viewport, 420.0)
            .id("theme-remove-card")
            .track_focus(&self.confirm_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.remove_confirmation = None;
                    cx.notify();
                }
            }))
            .child(popover::dialog_title(theme, "Remove theme?"))
            .child(popover::dialog_body(theme, body).mt(ui_rems(8.0)))
            .child(
                dialog_footer()
                    .mt(ui_rems(20.0))
                    .child(
                        dialog_action(theme, "Cancel", "theme-remove-cancel").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.remove_confirmation = None;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        popover::btn_danger(theme, "Remove theme")
                            .id("theme-remove-confirm")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_confirmation = None;
                                this.library_error = theme_library::remove(&id, cx)
                                    .err()
                                    .map(|error| error.to_string().into());
                                cx.notify();
                            })),
                    ),
            );
        Some(popover::modal(
            "theme-remove-dialog",
            viewport,
            card.into_any_element(),
        ))
    }

    /// One library theme as a list row (spec §3): identity tile · name and
    /// source line · trailing `Sm` actions. The source line is user data (a
    /// path), so it truncates and carries the full text as a tooltip.
    fn render_library_entry(
        &mut self,
        entry: CustomThemeEntry,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = entry.id.clone();
        let name = SharedString::from(entry.name.clone());
        let linked = entry.source.is_linked();
        let has_location = entry.source.path().is_some();
        let source = entry
            .source
            .path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "Self-contained snapshot".into());
        let warning = matches!(entry.status, CustomThemeStatus::Warning { .. });
        let status = SharedString::from(match &entry.status {
            CustomThemeStatus::Ready => format!(
                "{} · {} variant{} · {}",
                entry.source.label(),
                entry.family.variants.len(),
                if entry.family.variants.len() == 1 {
                    ""
                } else {
                    "s"
                },
                source
            ),
            CustomThemeStatus::Warning { message } => {
                format!("Using last known good · {message}")
            }
        });

        let action = |label: &str, id: String| {
            labelled_button(theme, ButtonKind::Secondary, ControlSize::Sm, label, id)
        };
        let mut actions: Vec<AnyElement> = Vec::new();
        if linked {
            actions.push(
                action("Reload", format!("theme-reload-{id}"))
                    .tooltip(popover::text_tooltip("Reload from the source file"))
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| {
                            this.library_error = theme_library::reload(&id, cx)
                                .err()
                                .map(|error| error.to_string().into());
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
            );
        }
        if has_location {
            actions.push(
                action("Reveal", format!("theme-reveal-{id}"))
                    .tooltip(popover::text_tooltip("Show the source in Finder"))
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| {
                            this.library_error = theme_library::reveal(&id, cx)
                                .err()
                                .map(|error| error.to_string().into());
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
            );
        }
        actions.push(
            action("Review", format!("theme-review-{id}"))
                .tooltip(popover::text_tooltip("Review how its colors were mapped"))
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        this.review_entry = Some(id.clone());
                        cx.notify();
                    }
                }))
                .into_any_element(),
        );
        actions.push(
            action("Duplicate", format!("theme-duplicate-{id}"))
                .aria_label("Duplicate as editable")
                .tooltip(popover::text_tooltip("Duplicate as an editable copy"))
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        this.library_error = theme_library::duplicate_as_editable(&id, cx)
                            .err()
                            .map(|error| error.to_string().into());
                        cx.notify();
                    }
                }))
                .into_any_element(),
        );
        if linked {
            actions.push(
                action("Unlink", format!("theme-unlink-{id}"))
                    .tooltip(popover::text_tooltip(
                        "Keep a copy and stop following the source",
                    ))
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| {
                            this.library_error = theme_library::unlink(&id, cx)
                                .err()
                                .map(|error| error.to_string().into());
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
            );
        }
        actions.push(
            action("Remove", format!("theme-remove-{id}"))
                .on_click(cx.listener({
                    let id = id.clone();
                    let name = name.clone();
                    move |this, _, _, cx| {
                        this.remove_confirmation = Some((id.clone(), name.clone()));
                        this.confirm_focus_pending = true;
                        cx.notify();
                    }
                }))
                .into_any_element(),
        );

        widgets::card_row(theme, false)
            .child(widgets::row_tile(
                theme,
                if linked {
                    icons::GLOBAL
                } else {
                    icons::DOCUMENT
                },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(ui_rems(widgets::ROW_TEXT_MIN))
                    .flex()
                    .flex_col()
                    .gap(ui_rems(2.0))
                    .child(widgets::row_title(theme, name))
                    .child(
                        div()
                            .id(SharedString::from(format!("theme-status-{id}")))
                            .min_w_0()
                            .truncate()
                            .text_size(ui_rems(widgets::ROW_DESCRIPTION_SIZE))
                            .line_height(ui_rems(16.0))
                            .text_color(if warning {
                                theme.readable_tone(theme.warning, 1.0)
                            } else {
                                theme.readable_muted(0.85)
                            })
                            .tooltip(popover::text_tooltip(status.clone()))
                            .child(status),
                    ),
            )
            .child(widgets::row_controls().children(actions))
            .into_any_element()
    }

    /// The library's rows: imported snapshots first, then linked themes (the
    /// tile and source line tell them apart), or one empty-state row.
    fn render_theme_library_rows(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let entries = theme_library::entries(cx);
        if entries.is_empty() {
            return vec![
                widgets::card_row(theme, false)
                    .child(
                        widgets::row_description(
                            theme,
                            "No custom themes yet. Add a theme file or an extension folder.",
                        )
                        .flex_1(),
                    )
                    .into_any_element(),
            ];
        }
        let (linked, imported): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|entry| entry.source.is_linked());
        imported
            .into_iter()
            .chain(linked)
            .map(|entry| self.render_library_entry(entry, theme, cx))
            .collect()
    }
}

impl EventEmitter<AppearanceSettingsEvent> for AppearancePage {}

impl Render for AppearancePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let availability = typography::availability(cx);
        let fixed = theme.font_sans.clone();
        let viewport = window.viewport_size();
        let current_mode = appearance::mode(cx);
        let current_themes = appearance::themes(cx);
        let current_accent = appearance::accent(cx);
        let current_surface = appearance::surface(cx);
        let ui_settings = crate::settings::current(cx);
        let current_background = ui_settings.new_thread_composer_background;
        let current_background_effect = ui_settings.new_thread_background_effect;

        // ── Theme ────────────────────────────────────────────────────────
        let cards = AppearanceMode::ALL
            .into_iter()
            .map(|mode| {
                widgets::option_card(
                    &theme,
                    mode.icon(),
                    mode.label(),
                    mode == current_mode,
                    preview(mode, &current_themes, current_accent, current_surface),
                )
                .id(SharedString::from(format!("appearance-{}", mode.label())))
                .role(gpui::Role::Button)
                .aria_label(format!("Appearance: {}", mode.label()))
                .aria_toggled(if mode == current_mode {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .tab_index(0)
                .on_click(cx.listener(move |_, _, _, cx| {
                    appearance::set_mode(mode, cx);
                    cx.notify();
                }))
                .on_key_down(crate::swallow_activation_key)
            })
            .collect::<Vec<_>>();
        let light_selector =
            self.render_theme_selector(Appearance::Light, &current_themes, &theme, viewport, cx);
        let dark_selector =
            self.render_theme_selector(Appearance::Dark, &current_themes, &theme, viewport, cx);
        let mut accent_choices = vec![AccentSelection::ThemeDefault];
        accent_choices.extend(AccentPreset::ALL.map(AccentSelection::Preset));
        let accent_controls = accent_choices
            .into_iter()
            .map(|selection| {
                let selected = selection == current_accent;
                accent_swatch(&theme, selection, selected)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        appearance::set_accent(selection, cx);
                        cx.notify();
                    }))
                    .on_key_down(crate::swallow_activation_key)
            })
            .collect::<Vec<_>>();
        // One segmented control per fixed choice (audit DS-16).
        let surface_control = crate::button::segmented(
            "appearance-surface",
            SurfacePreference::ALL
                .into_iter()
                .map(|surface| {
                    Segment::new(
                        surface_label(surface).to_lowercase().replace(' ', "-"),
                        surface_label(surface),
                    )
                })
                .collect(),
            SurfacePreference::ALL
                .iter()
                .position(|surface| *surface == current_surface)
                .unwrap_or(0),
            SEGMENTED_GLASS_WIDTH,
            ControlSize::Md,
            &theme,
            cx.listener(|_, ix: &usize, _, cx| {
                appearance::set_surface(SurfacePreference::ALL[*ix], cx);
                cx.notify();
            }),
        );
        let theme_section = widgets::section_card(&theme)
            .child(widgets::section_header(&theme, "Theme", None, Vec::new()))
            .child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        "Appearance",
                        Some("Follow macOS, or keep LUNA light or dark.".into()),
                    ))
                    .child(widgets::row_block().child(widgets::option_card_row().children(cards))),
            )
            .child(setting_row(
                &theme,
                false,
                "Light theme",
                "Used whenever LUNA is light.",
                vec![light_selector],
            ))
            .child(setting_row(
                &theme,
                false,
                "Dark theme",
                "Used whenever LUNA is dark.",
                vec![dark_selector],
            ))
            .child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        "Accent color",
                        Some(accent_helper(current_accent).into()),
                    ))
                    .child(
                        widgets::row_block()
                            .gap(ui_rems(6.0))
                            .children(accent_controls),
                    ),
            )
            .child(setting_row(
                &theme,
                false,
                "Glass",
                surface_helper(current_surface, theme.surface_treatment),
                vec![surface_control.into_any_element()],
            ));

        // ── Theme library ────────────────────────────────────────────────
        let library_notice = self
            .library_error
            .clone()
            .map(|error| widgets::error_strip(&theme, error))
            .or_else(|| {
                theme_library::load_warning(cx)
                    .map(|warning| widgets::warning_strip(&theme, warning))
            });
        let add_theme = crate::button::button(
            "theme-library-add",
            ButtonKind::Secondary,
            ControlSize::Md,
            true,
            &theme,
        )
        .aria_label("Add theme")
        .child(crate::button::glyph(icons::PLUS, ControlSize::Md))
        .child("Add theme")
        .on_click(cx.listener(|this, _, _, cx| this.open_import(cx)));
        let library_rows = self.render_theme_library_rows(&theme, cx);
        let library_section = widgets::section_card(&theme)
            .child(widgets::section_header(
                &theme,
                "Theme library",
                Some("Imported and linked themes appear in the Light and Dark theme menus.".into()),
                vec![add_theme.into_any_element()],
            ))
            .children(library_rows)
            .when_some(library_notice, |section, strip| {
                section.child(strip_row(&theme, strip))
            });

        // ── Text ─────────────────────────────────────────────────────────
        let ui_picker =
            self.render_font_picker(FontKind::Ui, &theme, &availability, fixed.clone(), cx);
        let terminal_picker =
            self.render_font_picker(FontKind::Terminal, &theme, &availability, fixed.clone(), cx);
        let code_picker =
            self.render_font_picker(FontKind::Code, &theme, &availability, fixed.clone(), cx);
        let ui_size = self.render_size_picker(FontKind::Ui, &theme, fixed.clone(), cx);
        let terminal_size = self.render_size_picker(FontKind::Terminal, &theme, fixed.clone(), cx);
        let code_size = self.render_size_picker(FontKind::Code, &theme, fixed, cx);
        let mut text_section = widgets::section_card(&theme)
            .child(widgets::section_header(&theme, "Text", None, Vec::new()));
        for (kind, picker, size_control) in [
            (FontKind::Ui, ui_picker, ui_size),
            (FontKind::Terminal, terminal_picker, terminal_size),
            (FontKind::Code, code_picker, code_size),
        ] {
            let (requested, effective) = (kind.requested(cx), kind.effective(cx));
            // A family missing on this device is a fallback, not a failure:
            // an amber note under the row it affects.
            let fallback = (requested != effective).then(|| {
                widgets::warning_strip(
                    &theme,
                    format!(
                        "{} \u{201c}{}\u{201d} isn't available on this device. Using {}.",
                        kind.label(),
                        requested.label(),
                        effective.label()
                    ),
                )
                .mt(px(0.0))
                .flex_1()
                .min_w_0()
            });
            text_section = text_section.child(
                setting_row(
                    &theme,
                    false,
                    kind.label(),
                    kind.description(),
                    vec![picker, size_control],
                )
                .when_some(fallback, |row, strip| {
                    row.child(widgets::row_block().child(strip))
                }),
            );
        }
        text_section = text_section.child(setting_row(
            &theme,
            false,
            "Conversation width",
            "Maximum width of messages. Adapts to smaller windows.",
            vec![self.render_width_control(&theme, cx)],
        ));

        // ── Code & diffs ─────────────────────────────────────────────────
        let chat_wrap = widgets::switch_control(
            "appearance-chat-code-wrap",
            "Chat code wrapping",
            ui_settings.code_fences_fit_content,
            &theme,
            cx,
            |this, cx| this.toggle_chat_code_wrap(cx),
        );
        let file_wrap = widgets::switch_control(
            "appearance-file-wrap",
            "Workspace file wrapping",
            ui_settings.files_word_wrap,
            &theme,
            cx,
            |this, cx| this.toggle_file_wrap(cx),
        );
        let diff_wrap = widgets::switch_control(
            "appearance-diff-wrap",
            "Diff wrapping",
            ui_settings.diff_wrap,
            &theme,
            cx,
            |this, cx| this.toggle_diff_wrap(cx),
        );
        let layout_controls = crate::button::segmented(
            "appearance-diff-layout",
            vec![
                Segment::new("unified", "Unified"),
                Segment::new("split", "Side by side"),
            ],
            usize::from(ui_settings.diff_split),
            SEGMENTED_PAIR_WIDTH,
            ControlSize::Md,
            &theme,
            cx.listener(|this, ix: &usize, _, cx| this.set_diff_layout(*ix == 1, cx)),
        );
        let color_controls = crate::button::segmented(
            "appearance-diff-colors",
            vec![
                Segment::new("red-green", "Red & green"),
                Segment::new("blue-orange", "Blue & orange"),
            ],
            usize::from(ui_settings.diff_color_scheme == DiffColorScheme::BlueOrange),
            SEGMENTED_PAIR_WIDTH,
            ControlSize::Md,
            &theme,
            cx.listener(|_, ix: &usize, _, cx| {
                appearance::set_diff_color_scheme(
                    if *ix == 1 {
                        DiffColorScheme::BlueOrange
                    } else {
                        DiffColorScheme::RedGreen
                    },
                    cx,
                );
                cx.notify();
            }),
        );
        let code_section = widgets::section_card(&theme)
            .child(widgets::section_header(&theme, "Code & diffs", None, Vec::new()))
            .child(setting_row(
                &theme,
                false,
                "Chat code wrapping",
                "Fit long code lines to the conversation instead of scrolling sideways.",
                vec![chat_wrap.into_any_element()],
            ))
            .child(setting_row(
                &theme,
                false,
                "Workspace file wrapping",
                "Wrap long lines in open and future file editors and previews.",
                vec![file_wrap.into_any_element()],
            ))
            .child(setting_row(
                &theme,
                false,
                "Diff layout",
                "Show changes in one column or compare old and new side by side.",
                vec![layout_controls.into_any_element()],
            ))
            .child(setting_row(
                &theme,
                false,
                "Diff wrapping",
                "Wrap long source lines in open and future diff panes.",
                vec![diff_wrap.into_any_element()],
            ))
            .child(setting_row(
                &theme,
                false,
                "Diff colors",
                "Choose an addition and deletion pair that is easy for you to distinguish.",
                vec![color_controls.into_any_element()],
            ));

        // ── New thread background ────────────────────────────────────────
        let background_available = current_background
            .as_ref()
            .is_some_and(|background| Path::new(&background.path).is_file());
        let background_tile: AnyElement = if let Some(background) =
            current_background.as_ref().filter(|_| background_available)
        {
            div()
                .flex_none()
                .size(ui_rems(36.0))
                .rounded(px(8.0))
                .overflow_hidden()
                .border_1()
                .border_color(theme.border)
                .child(
                    img(PathBuf::from(background.path.clone()))
                        .size_full()
                        .rounded(px(7.0))
                        .object_fit(ObjectFit::Cover),
                )
                .into_any_element()
        } else {
            widgets::row_tile(&theme, icons::FILE_IMAGE).into_any_element()
        };
        let (background_title, background_description): (SharedString, SharedString) =
            match current_background.as_ref() {
                Some(background) if background_available => (
                    background.name.clone().into(),
                    "Softened automatically on frosted themes.".into(),
                ),
                Some(background) => (
                    "Image unavailable".into(),
                    format!(
                        "\u{201c}{}\u{201d} can't be found. Choose a replacement or remove it.",
                        background.name
                    )
                    .into(),
                ),
                None => ("No image".into(), "Choose a picture from this Mac.".into()),
            };
        let background_actions: Vec<AnyElement> = if current_background.is_some() {
            vec![
                compact_action(&theme, "Remove", "new-thread-background-remove")
                    .on_click(cx.listener(|this, _, _, cx| this.remove_new_thread_background(cx)))
                    .into_any_element(),
                compact_action(&theme, "Replace image", "new-thread-background-replace")
                    .on_click(cx.listener(|this, _, _, cx| this.choose_new_thread_background(cx)))
                    .into_any_element(),
            ]
        } else {
            vec![
                compact_action(&theme, "Choose image", "new-thread-background-choose")
                    .on_click(cx.listener(|this, _, _, cx| this.choose_new_thread_background(cx)))
                    .into_any_element(),
            ]
        };
        let mut background_section = widgets::section_card(&theme)
            .child(widgets::section_header(
                &theme,
                "New thread background",
                Some("An image behind the composer on empty new threads.".into()),
                Vec::new(),
            ))
            .child(
                widgets::card_row(&theme, false)
                    .child(background_tile)
                    .child(widgets::row_text(
                        &theme,
                        background_title,
                        Some(background_description),
                    ))
                    .child(widgets::row_controls().children(background_actions)),
            );
        if background_available {
            let effects = crate::settings::NewThreadBackgroundEffect::ALL;
            let effect_control = crate::button::segmented(
                "new-thread-background-effect",
                effects
                    .into_iter()
                    .map(|effect| Segment::new(effect.label().to_lowercase(), effect.label()))
                    .collect(),
                effects
                    .iter()
                    .position(|effect| *effect == current_background_effect)
                    .unwrap_or(0),
                SEGMENTED_EFFECT_WIDTH,
                ControlSize::Md,
                &theme,
                cx.listener(move |_, ix: &usize, _, cx| {
                    crate::settings::set_new_thread_background_effect(effects[*ix], cx);
                    cx.notify();
                }),
            );
            background_section = background_section.child(setting_row(
                &theme,
                false,
                "Effect",
                current_background_effect.description(),
                vec![effect_control.into_any_element()],
            ));
        }
        if let Some(error) = self.background_error.clone() {
            background_section =
                background_section.child(strip_row(&theme, widgets::error_strip(&theme, error)));
        }

        // ── Reset ────────────────────────────────────────────────────────
        // A single-row section: its row title carries the meaning (spec §2).
        let reset_section = widgets::section_card(&theme).child(setting_row(
            &theme,
            true,
            "Reset appearance",
            "Restore the first-install look without deleting imported themes.",
            vec![
                compact_action(&theme, "Restore defaults", "appearance-reset-open")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reset_confirmation_open = true;
                        this.confirm_focus_pending = true;
                        cx.notify();
                    }))
                    .into_any_element(),
            ],
        ));

        let modal = self
            .render_import_dialog(viewport, &theme, window, cx)
            .or_else(|| self.render_review_dialog(viewport, &theme, cx))
            .or_else(|| self.render_reset_dialog(viewport, &theme, window, cx))
            .or_else(|| self.render_remove_dialog(viewport, &theme, window, cx));

        let scrollbar = popover::rail(self, "appearance-page-scrollbar", &theme, cx);
        div()
            .id("appearance-page-host")
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<TranscriptWidthDrag>, window, cx| {
                    this.drag_width(event.event.position.x, window, cx);
                },
            ))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    if this.width_pressed {
                        this.width_pressed = false;
                        cx.notify();
                    }
                    this.apply_pending_width(cx);
                }),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    if this.width_pressed {
                        this.width_pressed = false;
                        cx.notify();
                    }
                    this.apply_pending_width(cx);
                }),
            )
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("appearance-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_title_bar(
                                &theme,
                                "Appearance",
                                None,
                                Vec::new(),
                            ))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Choose how LUNA looks. These settings stay on this device.",
                            ))
                            .child(theme_section)
                            .child(library_section)
                            .child(text_section)
                            .child(code_section)
                            .child(background_section)
                            .child(reset_section),
                    ),
            )
            .children(scrollbar)
            .children(modal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_gets_a_card() {
        assert_eq!(AppearanceMode::ALL.len(), 3);
        for mode in AppearanceMode::ALL {
            assert!(!mode.label().is_empty());
        }
    }

    #[test]
    fn registry_offers_both_appearances_and_keeps_single_dark_families_valid() {
        let registry = ThemeRegistry::builtin();
        assert_eq!(
            registry.variants_for(luna_theme::Appearance::Light).count(),
            10
        );
        assert_eq!(
            registry.variants_for(luna_theme::Appearance::Dark).count(),
            20
        );
    }

    #[test]
    fn accent_helper_explains_default_and_override_scope() {
        assert!(accent_helper(AccentSelection::ThemeDefault).contains("intended"));
        let copy = accent_helper(AccentSelection::Preset(AccentPreset::Pink));
        assert!(copy.starts_with("Pink ·"));
        assert!(copy.contains("glyphs"));
    }

    #[test]
    fn surface_helper_explains_theme_default_and_global_overrides() {
        let default = surface_helper(SurfacePreference::ThemeDefault, SurfaceTreatment::Opaque);
        assert!(default.contains("opaque default"));
        assert!(
            surface_helper(SurfacePreference::Frosted, SurfaceTreatment::Opaque)
                .contains("where supported")
        );
        assert!(
            surface_helper(SurfacePreference::Opaque, SurfaceTreatment::Frosted)
                .contains("every theme")
        );
    }

    #[test]
    fn font_options_appear_once_in_stable_order() {
        let catalog = FontAvailability::all();
        let labels: Vec<_> = catalog.choices().iter().map(UiFontFamily::label).collect();
        assert_eq!(labels.len(), 5);
        assert_eq!(
            labels,
            ["Geist", "Geist Mono", "System UI", "Arial", "Menlo"]
        );
        let unique = labels.into_iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), 5);
    }

    #[test]
    fn only_the_terminal_catalog_is_narrowed_to_fixed_width() {
        let all = FontAvailability::all();
        let terminal: Vec<_> = FontKind::Terminal
            .choices_for(&all)
            .iter()
            .map(UiFontFamily::label)
            .collect();
        assert_eq!(terminal, ["Geist Mono", "Menlo"]);

        for kind in [FontKind::Ui, FontKind::Code] {
            assert_eq!(kind.choices_for(&all), all.choices());
            assert!(kind.is_available_for(&all, &UiFontFamily::System));
            assert!(kind.is_available_for(&all, &UiFontFamily::Installed("Arial".into())));
        }
        for proportional in [
            UiFontFamily::System,
            UiFontFamily::Geist,
            UiFontFamily::Installed("Arial".into()),
        ] {
            assert!(!FontKind::Terminal.is_available_for(&all, &proportional));
        }
        assert!(FontKind::Terminal.is_available_for(&all, &UiFontFamily::GeistMono));
        // A query that only matches proportional families leaves the terminal
        // highlight on the bundled mono face rather than on System UI.
        assert_eq!(
            first_available(&[], FontKind::Terminal, &all),
            UiFontFamily::GeistMono
        );
    }

    #[test]
    fn font_keyboard_navigation_stops_at_edges_and_skips_unavailable() {
        let all = FontAvailability::all();
        let choices = all.choices().to_vec();
        assert_eq!(
            step_font(&UiFontFamily::Geist, -1, &choices, FontKind::Ui, &all),
            UiFontFamily::Geist
        );
        assert_eq!(
            step_font(
                &UiFontFamily::Installed("Menlo".into()),
                1,
                &choices,
                FontKind::Ui,
                &all
            ),
            UiFontFamily::Installed("Menlo".into())
        );
        let without_arial = all.without(&UiFontFamily::Installed("Arial".into()));
        assert_eq!(
            step_font(
                &UiFontFamily::System,
                1,
                &choices,
                FontKind::Ui,
                &without_arial
            ),
            UiFontFamily::Installed("Menlo".into())
        );
    }

    #[test]
    fn filtering_narrows_to_matches_and_navigation_stays_inside_them() {
        let all = FontAvailability::all();
        let catalog = all.choices().to_vec();

        assert_eq!(filter_families("", &catalog), catalog);
        assert_eq!(filter_families("   ", &catalog), catalog);
        assert!(filter_families("helvetica", &catalog).is_empty());
        // Case-insensitive, and the bundled entries still lead when they match.
        assert_eq!(
            filter_families("GEIST", &catalog),
            vec![UiFontFamily::Geist, UiFontFamily::GeistMono]
        );

        // "Menlo" prefix-matches, so it outranks the "System UI" substring hit.
        let matches = filter_families("m", &catalog);
        assert_eq!(
            matches,
            vec![
                UiFontFamily::Installed("Menlo".into()),
                UiFontFamily::GeistMono,
                UiFontFamily::System,
            ]
        );
        // Stepping never escapes the filtered list.
        assert_eq!(
            step_font(&UiFontFamily::System, 1, &matches, FontKind::Ui, &all),
            UiFontFamily::System
        );
        assert_eq!(
            first_available(&matches, FontKind::Ui, &all),
            UiFontFamily::Installed("Menlo".into())
        );
        assert_eq!(
            last_available(&matches, FontKind::Ui, &all),
            UiFontFamily::System
        );
    }

    #[test]
    fn each_font_kind_gets_distinct_labels_and_element_ids() {
        let slugs: Vec<_> = FontKind::ALL.iter().map(|kind| kind.slug()).collect();
        let labels: Vec<_> = FontKind::ALL.iter().map(|kind| kind.label()).collect();
        assert_eq!(
            slugs.iter().collect::<std::collections::HashSet<_>>().len(),
            3
        );
        assert_eq!(
            labels
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn pixel_sizes_render_whole_and_fractional_values() {
        assert_eq!(format_px(13.0), "13 px");
        assert_eq!(format_px(typography::CODE_FONT_SIZE_DEFAULT), "12.5 px");
        assert_eq!(
            typography::clamp_font_size(typography::FONT_SIZE_MAX + 1.0),
            typography::FONT_SIZE_MAX
        );
        assert_eq!(
            typography::clamp_font_size(typography::FONT_SIZE_MIN - 1.0),
            typography::FONT_SIZE_MIN
        );
    }

    #[test]
    fn font_size_options_are_ordered_and_include_the_default() {
        let values = UiFontSize::ALL.map(UiFontSize::pixels);
        assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(UiFontSize::ALL.contains(&UiFontSize::default()));
    }

    #[test]
    fn mono_size_ladder_keeps_both_defaults_exactly_reachable() {
        assert!(MONO_FONT_SIZES.windows(2).all(|pair| pair[0] < pair[1]));
        for default in [
            typography::TERMINAL_FONT_SIZE_DEFAULT,
            typography::CODE_FONT_SIZE_DEFAULT,
        ] {
            assert_eq!(MONO_FONT_SIZES[nearest_mono_ix(default)], default);
        }
        // Off-ladder values (older settings, hand-edited files) snap, never drop.
        assert_eq!(MONO_FONT_SIZES[nearest_mono_ix(0.0)], MONO_FONT_SIZES[0]);
        assert_eq!(MONO_FONT_SIZES[nearest_mono_ix(99.0)], 20.0);
        assert_eq!(MONO_FONT_SIZES[nearest_mono_ix(12.4)], 12.5);
    }

    #[test]
    fn every_size_dropdown_labels_its_whole_ladder_in_pixels() {
        for kind in FontKind::ALL {
            let labels = kind.size_labels();
            assert_eq!(labels.len(), kind.size_count());
            assert!(labels.iter().all(|label| label.ends_with(" px")));
        }
        assert!(FontKind::Terminal.size_labels().contains(&"13 px".into()));
        assert!(FontKind::Code.size_labels().contains(&"12.5 px".into()));
    }

    #[gpui::test]
    fn conversation_width_drag_coalesces_and_persists_the_last_value(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(Default::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| AppearancePage::new(cx));
        window
            .update(cx, |page, window, cx| {
                page.width_bounds.set(Some(gpui::Bounds::new(
                    gpui::point(px(100.0), px(0.0)),
                    gpui::size(px(240.0), px(28.0)),
                )));
                page.drag_width(px(0.0), window, cx);
                assert_eq!(page.pending_width, Some(560.0));
                page.drag_width(px(1000.0), window, cx);
                assert_eq!(page.pending_width, Some(1200.0));
                page.drag_width(px(220.0), window, cx);
                assert_eq!(page.pending_width, Some(880.0));
                assert_eq!(
                    crate::settings::transcript_width(cx),
                    736.0,
                    "pointer events must coalesce before publishing"
                );
                page.apply_pending_width(cx);
                assert_eq!(crate::settings::transcript_width(cx), 880.0);
                crate::settings::flush(cx);
                assert_eq!(
                    crate::settings::UiSettings::load(dir.path()).transcript_width,
                    880.0
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn size_dropdowns_commit_from_the_ladder_and_enter_leaves_menus_closed(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            typography::init(
                UiFontFamily::Geist,
                UiFontSize::default(),
                UiFontFamily::GeistMono,
                typography::TERMINAL_FONT_SIZE_DEFAULT,
                UiFontFamily::GeistMono,
                typography::CODE_FONT_SIZE_DEFAULT,
                FontAvailability::all(),
                cx,
            );
        });
        let window = cx.add_window(|_, cx| AppearancePage::new(cx));
        window
            .update(cx, |page, window, cx| {
                for kind in [FontKind::Terminal, FontKind::Code] {
                    page.toggle_size_menu(kind, cx);
                    assert!(page.size_menu(kind).is_open());
                    assert_eq!(page.selected_size_ix(kind), kind.size_ix(cx));
                    let target = kind.size_ix(cx) + 1;
                    page.set_selected_size_ix(kind, target);
                    page.commit_size(kind, window, cx);
                    assert_eq!(kind.pixel_size(cx), MONO_FONT_SIZES[target]);
                    assert!(!page.size_menu(kind).is_open());
                }

                // Opening a family menu closes a size menu left open elsewhere.
                page.toggle_size_menu(FontKind::Ui, cx);
                assert!(page.size_menu(FontKind::Ui).is_open());
                page.toggle_font_menu(FontKind::Code, window, cx);
                assert!(!page.size_menu(FontKind::Ui).is_open());
                assert!(page.font_menu(FontKind::Code).is_open());

                // Enter commits and reports the key consumed — without that the
                // trigger's `is_open` guard reads the just-closed menu and
                // reopens it on the very same event.
                let enter = KeyDownEvent {
                    keystroke: gpui::Keystroke::parse("enter").expect("valid keystroke"),
                    is_held: false,
                    prefer_character_input: false,
                };
                assert!(page.on_font_key_down(FontKind::Code, &enter, window, cx));
                assert!(!page.font_menu(FontKind::Code).is_open());
            })
            .expect("window is open");
    }

    /// A settings file written before the terminal picker was constrained can
    /// still name a proportional family; startup must not hand it to the grid.
    #[gpui::test]
    fn persisted_proportional_terminal_family_resolves_to_geist_mono(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            typography::init(
                UiFontFamily::Geist,
                UiFontSize::default(),
                UiFontFamily::Installed("Arial".into()),
                typography::TERMINAL_FONT_SIZE_DEFAULT,
                UiFontFamily::Installed("Arial".into()),
                typography::CODE_FONT_SIZE_DEFAULT,
                FontAvailability::all(),
                cx,
            );
            assert_eq!(
                typography::terminal_effective(cx),
                UiFontFamily::GeistMono,
                "proportional terminal family must fall back"
            );
            assert_eq!(
                typography::code_effective(cx),
                UiFontFamily::Installed("Arial".into()),
                "code and diffs keep proportional picks"
            );
            // The setter path rejects the same family too.
            assert!(!typography::set_terminal_family(UiFontFamily::System, cx));
            assert_eq!(typography::terminal_effective(cx), UiFontFamily::GeistMono);
        });
    }
}
