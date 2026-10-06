//! Settings → Shortcuts (feature-inventory §1.4): the rebindable bindings,
//! grouped into section cards — click a key combo to record (Esc cancels),
//! conflicts are refused at record time and named inline on the row, per-row
//! Reset and a page-level Restore defaults. Changes emit [`ShortcutsEvent`];
//! the shell persists them and re-applies the app keymap.
//!
//! Layout follows `docs/design/settings-polish-2026-10-06/spec.md`: a page
//! title bar, section cards with their titles inside, and rows whose trailing
//! control is the key-combo recorder ([`ShortcutsPage::render_binding_control`]).
//! Every dimension next to text is an interface rem, so the keycaps follow
//! Appearance → Interface size.

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Keystroke, SharedString, Window, div, prelude::*,
    px,
};

use crate::appshots::{AppshotCapabilities, AppshotDestination};
use crate::button::{self, ButtonKind, ControlSize};
use crate::popover::{self, ScrollRailHost};

#[path = "appshots.rs"]
mod appshots_page;
use crate::settings::widgets;
use crate::settings::{
    ComposerSendBehavior, KeymapConfig, ShortcutId, combo_from_keystroke, display_combo,
};
use crate::state::AppState;
use crate::theme::{FillTier, Theme};
use crate::typography::ui_rems;

/// Shown in place of a row's description while its combo is recording.
const RECORDING_HINT: &str = "Press the new key combination, or Escape to cancel.";
/// Narrowest key-combo control, so short combos line up in one column.
const COMBO_MIN_WIDTH: f32 = 88.0;

/// Outcome of one keystroke while recording. Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Esc — abandon recording, keep the old combo.
    Cancelled,
    /// A bare modifier (or unusable key) — stay recording.
    Ignored,
    /// A full combo landed.
    Set(String),
}

pub fn record_key(key: &str, ctrl: bool, alt: bool, shift: bool, cmd: bool) -> RecordOutcome {
    if key.eq_ignore_ascii_case("escape") {
        return RecordOutcome::Cancelled;
    }
    match combo_from_keystroke(ctrl, alt, shift, cmd, key) {
        Some(combo) => RecordOutcome::Set(combo),
        None => RecordOutcome::Ignored,
    }
}

#[derive(Debug, Clone)]
pub enum ShortcutsEvent {
    /// The keymap changed — persist + re-apply.
    KeymapChanged(KeymapConfig),
    /// The Escape fallback changed — persist it locally.
    EscapeStopsActiveAgentChanged(bool),
    /// The composer send behavior changed — persist + re-apply.
    ComposerSendBehaviorChanged(ComposerSendBehavior),
    AppshotsChanged {
        enabled: bool,
        sound_enabled: bool,
        destination: AppshotDestination,
    },
}

pub struct ShortcutsPage {
    appshots_page: bool,
    appshots_focus_pending: bool,
    scroll: crate::settings::widgets::PageScroll,
    /// Working copy (kept in sync with the shell via change events).
    keymap: KeymapConfig,
    escape_stops_active_agent: bool,
    composer_send_behavior: ComposerSendBehavior,
    recording: Option<ShortcutId>,
    recording_blur: Option<gpui::Subscription>,
    recording_interceptor: Option<gpui::Subscription>,
    /// A rejected record attempt ("{Combo} is already assigned to {label}.") —
    /// conflicts never persist; they're refused at record time, as in luna.
    conflict_notice: Option<SharedString>,
    /// The row the notice belongs to, so it renders on that row (with the
    /// chip's warning edge) instead of in a helper line far below it.
    notice_row: Option<ShortcutId>,
    focus: FocusHandle,
    appshots_enabled: bool,
    appshot_sound_enabled: bool,
    appshot_destination: AppshotDestination,
    appshot_capabilities: AppshotCapabilities,
    capture_access_prompted: bool,
    semantic_access_prompted: bool,
    // The page never talks RPC; state is kept for parity with sibling pages
    // (and future per-device keymaps).
    _state: Entity<AppState>,
}

impl EventEmitter<ShortcutsEvent> for ShortcutsPage {}

impl ShortcutsPage {
    pub fn new(
        state: Entity<AppState>,
        keymap: KeymapConfig,
        escape_stops_active_agent: bool,
        composer_send_behavior: ComposerSendBehavior,
        appshots_enabled: bool,
        appshot_sound_enabled: bool,
        appshot_destination: AppshotDestination,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|_, _| crate::appshots::set_recording(false))
            .detach();
        Self {
            appshots_page: false,
            appshots_focus_pending: false,
            scroll: crate::settings::widgets::PageScroll::default(),
            keymap,
            escape_stops_active_agent,
            composer_send_behavior,
            recording: None,
            recording_blur: None,
            recording_interceptor: None,
            conflict_notice: None,
            notice_row: None,
            focus: cx.focus_handle(),
            appshots_enabled,
            appshot_sound_enabled,
            appshot_destination,
            appshot_capabilities: crate::appshots::capabilities(),
            capture_access_prompted: false,
            semantic_access_prompted: false,
            _state: state,
        }
    }

    pub fn show_appshots(&mut self, appshots: bool) {
        if self.appshots_page != appshots {
            self.stop_recording();
            self.conflict_notice = None;
            self.appshots_page = appshots;
            self.appshots_focus_pending = appshots;
            // One scroll state serves both pages — rewind it so each opens
            // at the top instead of where the other was left.
            self.scroll.reset();
        }
    }

    fn start_recording(&mut self, id: ShortcutId, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = Some(id);
        crate::appshots::set_recording(true);
        let page = cx.entity().downgrade();
        // Bound actions run before Div key listeners. Intercept first so a
        // conflicting chord is recorded/refused instead of running its action.
        self.recording_interceptor = Some(cx.intercept_keystrokes(move |event, window, cx| {
            let _ = page.update(cx, |page, cx| {
                if page.focus.is_focused(window) {
                    page.record_keystroke(&event.keystroke, cx);
                }
            });
        }));
        self.recording_blur = Some(cx.on_blur(&self.focus, window, |this, _, cx| {
            this.stop_recording();
            cx.notify();
        }));
        self.conflict_notice = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn stop_recording(&mut self) {
        self.recording = None;
        self.recording_blur = None;
        self.recording_interceptor = None;
        crate::appshots::set_recording(false);
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        cx.emit(ShortcutsEvent::KeymapChanged(self.keymap.clone()));
        cx.notify();
    }

    /// Refuse an assignment on `row`: the notice renders on that row.
    fn set_notice(&mut self, row: ShortcutId, notice: impl Into<SharedString>) {
        self.conflict_notice = Some(notice.into());
        self.notice_row = Some(row);
    }

    /// The notice owned by `id`'s row, if any.
    fn notice_for(&self, id: ShortcutId) -> Option<SharedString> {
        self.conflict_notice
            .clone()
            .filter(|_| self.notice_row == Some(id))
    }

    /// A notice whose row the current page does not render (it has no row
    /// to sit on, so the page shows it under its subtitle instead).
    fn orphan_notice(&self) -> Option<SharedString> {
        let capture_row = self.notice_row == Some(ShortcutId::CaptureAppshot);
        let on_this_page = self.notice_row.is_some() && capture_row == self.appshots_page;
        self.conflict_notice.clone().filter(|_| !on_this_page)
    }

    fn set_escape_stops_active_agent(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.escape_stops_active_agent != enabled {
            self.escape_stops_active_agent = enabled;
            cx.emit(ShortcutsEvent::EscapeStopsActiveAgentChanged(enabled));
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
            self.conflict_notice = None;
            cx.emit(ShortcutsEvent::ComposerSendBehaviorChanged(behavior));
            cx.notify();
        }
    }

    /// General can edit this same preference while Shortcuts remains cached.
    pub fn sync_composer_send_behavior(
        &mut self,
        behavior: ComposerSendBehavior,
        cx: &mut Context<Self>,
    ) {
        if self.composer_send_behavior != behavior {
            self.composer_send_behavior = behavior;
            self.conflict_notice = None;
            cx.notify();
        }
    }

    fn commit_appshots(&self, cx: &mut Context<Self>) {
        cx.emit(ShortcutsEvent::AppshotsChanged {
            enabled: self.appshots_enabled,
            sound_enabled: self.appshot_sound_enabled,
            destination: self.appshot_destination,
        });
    }

    /// Why `id` may not take `combo`, if it may not: the one check both
    /// recording and Reset use (fixed and contextual chords, the composer's
    /// send, Activity's reserved chords, then another shortcut's binding).
    fn assignment_refusal(&self, id: ShortcutId, combo: &str) -> Option<String> {
        if combo.is_empty() {
            return None;
        }
        if let Some(owner) = crate::settings::reserved_combo_owner(id, combo) {
            return Some(format!("{} is reserved for {owner}.", display_combo(combo)));
        }
        if send_combo_is_reserved(self.composer_send_behavior, combo) {
            return Some(format!(
                "{} is reserved for the composer.",
                display_combo(combo)
            ));
        }
        if id == ShortcutId::OpenActivity && crate::settings::activity_combo_is_reserved(combo) {
            return Some(format!(
                "{} is reserved for an application action.",
                display_combo(combo)
            ));
        }
        // A combo already bound elsewhere is REFUSED, naming the owner
        // (luna settings.shortcuts.tsx: "… is already assigned to …").
        conflict_owner(&self.keymap, id, combo).map(|owner| {
            format!(
                "{} is already assigned to {}.",
                display_combo(combo),
                owner.label()
            )
        })
    }

    /// A row's Reset: back to the default unless that chord is now taken
    /// (then nothing changes and the notice names the owner; saved
    /// bindings are never moved to make room).
    fn reset_shortcut(&mut self, id: ShortcutId, cx: &mut Context<Self>) {
        self.stop_recording();
        let default = id.default_combo();
        if let Some(refusal) = self.assignment_refusal(id, default) {
            self.set_notice(id, format!("Can't reset: {refusal}"));
            cx.notify();
            return;
        }
        self.keymap.reset(id);
        self.conflict_notice = None;
        self.commit(cx);
    }

    fn record_keystroke(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let Some(recording) = self.recording else {
            return;
        };
        let mods = &keystroke.modifiers;
        match record_key(
            &keystroke.key,
            mods.control,
            mods.alt,
            mods.shift,
            mods.platform,
        ) {
            RecordOutcome::Cancelled => {
                self.stop_recording();
                cx.notify();
            }
            RecordOutcome::Ignored => {}
            RecordOutcome::Set(combo) => {
                if recording == ShortcutId::CaptureAppshot
                    && crate::appshots::validate_shortcut(&combo).is_err()
                {
                    self.set_notice(
                        recording,
                        format!(
                            "Use {} with a letter, number, function key or navigation key.",
                            if cfg!(target_os = "macos") {
                                "Control, Option or Command"
                            } else {
                                "Control or Alt"
                            }
                        ),
                    );
                    self.stop_recording();
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                if let Some(refusal) = self.assignment_refusal(recording, &combo) {
                    self.set_notice(recording, refusal);
                    self.stop_recording();
                    cx.notify();
                    cx.stop_propagation();
                } else {
                    self.keymap.set(recording, combo);
                    self.stop_recording();
                    self.conflict_notice = None;
                    self.commit(cx);
                }
            }
        }
        cx.stop_propagation();
    }

    /// One shortcut row: the action and its purpose on the left, the
    /// key-combo control at the right, and — when this row's last attempt
    /// was refused — the notice under it. `ix` is the id's position in
    /// [`ShortcutId::ALL`] (unique element ids across the group cards).
    fn render_row(
        &self,
        id: ShortcutId,
        ix: usize,
        recording: Option<ShortcutId>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        // The nine jump slots share one purpose, carried by their section
        // header; repeating it per row read as a wall of identical copy.
        let description = match id {
            ShortcutId::JumpSession(_) => None,
            _ => Some(SharedString::from(description(id))),
        };
        widgets::card_row(theme, false)
            .child(self.shortcut_text(id, id.label(), description, theme))
            .child(
                widgets::row_controls()
                    .child(self.render_binding_control(id, ix, recording, theme, cx)),
            )
            .children(self.row_notice(id, theme))
    }

    /// A shortcut row's text column. While `id` records, its description
    /// gives way to the recording hint, in the accent tone, so the state is
    /// told where the user is looking.
    fn shortcut_text(
        &self,
        id: ShortcutId,
        title: impl Into<SharedString>,
        description: Option<SharedString>,
        theme: &Theme,
    ) -> gpui::Div {
        let recording = self.recording == Some(id);
        widgets::row_text(theme, title, description.filter(|_| !recording)).when(
            recording,
            |column| {
                column.child(
                    widgets::row_description(theme, RECORDING_HINT)
                        .text_color(theme.readable_tone(theme.accent, 1.0)),
                )
            },
        )
    }

    /// The refusal notice for `id`'s row, full width under its text.
    fn row_notice(&self, id: ShortcutId, theme: &Theme) -> Option<gpui::Div> {
        let notice = self.notice_for(id)?;
        Some(
            div()
                .w_full()
                .child(widgets::warning_strip(theme, notice).mt_0()),
        )
    }

    /// The key-combo control: a secondary push button showing the combo as
    /// mono keycaps (click to record), with the row's ghost Reset beside it
    /// when the binding is customized. Recording turns the button into the
    /// kit's selected chip ("Press keys…") and puts Cancel in Reset's place;
    /// a refused attempt gives it a warning edge.
    fn render_binding_control(
        &self,
        id: ShortcutId,
        ix: usize,
        recording: Option<ShortcutId>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let combo = self.keymap.get(id).to_string();
        let default = id.default_combo();
        let is_recording = recording == Some(id);
        let non_default = combo != default;
        let notice = self.notice_for(id);
        let spoken = if combo.is_empty() {
            "not set".to_string()
        } else {
            display_combo(&combo)
        };

        let side = if is_recording {
            Some(
                button::icon_button(
                    format!("shortcut-cancel-{ix}"),
                    crate::icons::CLOSE,
                    ControlSize::Md,
                    true,
                    theme,
                )
                .aria_label(format!("Cancel recording the {} shortcut", id.label()))
                .tooltip(popover::text_tooltip("Cancel (Escape)"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.stop_recording();
                    cx.notify();
                })),
            )
        } else if non_default {
            let tip = if default.is_empty() {
                "Reset to no shortcut".to_string()
            } else {
                format!("Reset to {}", display_combo(default))
            };
            Some(
                button::icon_button(
                    format!("shortcut-reset-{ix}"),
                    crate::icons::RESTART,
                    ControlSize::Md,
                    true,
                    theme,
                )
                .aria_label(format!("Reset {} shortcut", id.label()))
                .tooltip(popover::text_tooltip(tip))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.reset_shortcut(id, cx);
                })),
            )
        } else {
            None
        };

        let chip_id = format!("shortcut-combo-{ix}");
        let chip = if is_recording {
            button::chip(chip_id, true, ControlSize::Md, true, theme)
                .child(button::glyph(crate::icons::KEYBOARD, ControlSize::Md))
                .child(SharedString::from("Press keys…"))
                .aria_description(RECORDING_HINT)
        } else {
            let frame = button::button(chip_id, ButtonKind::Secondary, ControlSize::Md, true, theme)
                .px(ui_rems(6.0))
                .gap(ui_rems(3.0))
                .aria_toggled(gpui::Toggled::False)
                .when_some(notice, |frame, notice| {
                    frame
                        .border_color(theme.warning.opacity(0.7))
                        .aria_description(notice)
                });
            if combo.is_empty() {
                frame.child(
                    div()
                        .px(ui_rems(4.0))
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(theme.readable_muted(0.85))
                        .child(SharedString::from("Not set")),
                )
            } else {
                frame.children(
                    keycaps_on(cfg!(target_os = "macos"), &combo)
                        .into_iter()
                        .map(|cap| keycap(theme, cap)),
                )
            }
        };
        let chip = chip
            .min_w(ui_rems(COMBO_MIN_WIDTH))
            .aria_label(format!("Change {} shortcut: {spoken}", id.label()))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.start_recording(id, window, cx);
            }));

        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui_rems(6.0))
            .children(side)
            .child(chip)
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    // Kept for the appshots half of this page (appshots.rs), whose host still
    // carries the drag-move listener itself.
    fn on_bar_drag_move(
        &mut self,
        event: &gpui::DragMoveEvent<popover::MenuScrollbarDrag>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rail_drag_to(event.event.position.y) {
            cx.notify();
        }
    }

    /// The shared rail under the id of whichever page is showing. Kept as a
    /// method because the appshots half of this page renders through it.
    fn render_scrollbar(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let id = if self.appshots_page {
            "appshots-settings-page-scrollbar"
        } else {
            "shortcuts-page-scrollbar"
        };
        popover::rail(self, id, theme, cx)
    }
}

impl popover::ScrollRailHost for ShortcutsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

/// The shortcut (other than `id`) already bound to `combo`, if any. Pure.
pub fn conflict_owner(keymap: &KeymapConfig, id: ShortcutId, combo: &str) -> Option<ShortcutId> {
    ShortcutId::ALL.into_iter().find(|&other| {
        other.available() && other != id && crate::settings::same_combo(keymap.get(other), combo)
    })
}

pub fn send_combo_is_reserved(_behavior: ComposerSendBehavior, combo: &str) -> bool {
    combo == "mod-enter"
}

pub fn modifier_send_label(is_macos: bool) -> &'static str {
    if is_macos { "⌘ Enter" } else { "Ctrl Enter" }
}

/// The page's shortcut sections, in display order. [`group`] is a total
/// match, so every [`ShortcutId::ALL`] entry lands in exactly one — a shortcut
/// added later extends the match and appears on the page by construction
/// (`every_shortcut_lands_in_a_rendered_group` holds the other half: its group
/// name must be listed here). One card per topic: the single-row Files,
/// Browser and Projects cards are folded into their neighbours. "Appshots"
/// renders on the Appshots page, beside its capture settings.
const GROUP_ORDER: [&str; 6] = [
    "Sessions and projects",
    "Panels",
    "Tabs and splits",
    "Files and browser",
    "Jump to session",
    "Appshots",
];

/// The section a shortcut's row renders under.
fn group(id: ShortcutId) -> &'static str {
    match id {
        ShortcutId::CaptureAppshot => "Appshots",
        ShortcutId::SaveFile | ShortcutId::BrowserReload => "Files and browser",
        ShortcutId::ToggleSidebar | ShortcutId::ToggleChanges | ShortcutId::ToggleTerminal => {
            "Panels"
        }
        ShortcutId::NewSession
        | ShortcutId::NewProject
        | ShortcutId::OpenModelPicker
        | ShortcutId::OpenActivity
        | ShortcutId::NextSession
        | ShortcutId::PrevSession
        | ShortcutId::ArchiveSession => "Sessions and projects",
        ShortcutId::NewThreadChooser
        | ShortcutId::NewThreadSplit
        | ShortcutId::PreviousTab
        | ShortcutId::NextTab
        | ShortcutId::PreviousGroup
        | ShortcutId::NextGroup
        | ShortcutId::ReopenTab => "Tabs and splits",
        ShortcutId::JumpSession(_) => "Jump to session",
    }
}

/// A section's header description, when its rows share one purpose.
fn group_description(name: &str) -> Option<SharedString> {
    match name {
        "Jump to session" => Some("Open the session at each place in the sidebar list.".into()),
        _ => None,
    }
}

/// Application shortcuts that can't be rebound, listed read-only at the end.
const FIXED_SHORTCUTS: [(&str, &str); 3] = [
    ("Search conversations and commands", "mod-k"),
    ("Open Settings", "mod-,"),
    ("Close the focused tab, pane or window", "mod-w"),
];

/// The keycaps a stored combo reads as: the modifiers in the platform's order
/// and spelling (⌃ ⌥ ⇧ ⌘ on macOS; Ctrl, Alt, Shift elsewhere), then the
/// key. Parsed by known modifier prefixes rather than a blind split on `-`,
/// so the minus key ("mod--") and brace chords ("mod-{") keep their key.
/// Pure.
fn keycaps_on(mac: bool, combo: &str) -> Vec<SharedString> {
    let (mut primary, mut ctrl, mut alt, mut shift) = (false, false, false, false);
    let mut rest = combo;
    while let Some((head, tail)) = rest.split_once('-') {
        if tail.is_empty() {
            break;
        }
        match head {
            "mod" => primary = true,
            "ctrl" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            _ => break,
        }
        rest = tail;
    }
    let mut caps: Vec<SharedString> = Vec::new();
    if mac {
        for (on, glyph) in [(ctrl, "⌃"), (alt, "⌥"), (shift, "⇧"), (primary, "⌘")] {
            if on {
                caps.push(glyph.into());
            }
        }
    } else {
        // Off macOS ctrl IS the primary: a legacy "ctrl-" spelling and
        // "mod-" are one key.
        for (on, label) in [(primary || ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift")] {
            if on {
                caps.push(label.into());
            }
        }
    }
    if !rest.is_empty() {
        caps.push(key_label(mac, rest).into());
    }
    caps
}

/// A combo's final key as a keycap label. Pure.
fn key_label(mac: bool, key: &str) -> String {
    let named = match key {
        "up" => Some("↑"),
        "down" => Some("↓"),
        "left" => Some("←"),
        "right" => Some("→"),
        "enter" if mac => Some("↩"),
        "tab" if mac => Some("⇥"),
        "backspace" if mac => Some("⌫"),
        "delete" if mac => Some("⌦"),
        "escape" => Some("Esc"),
        "pageup" => Some("Page Up"),
        "pagedown" => Some("Page Down"),
        _ => None,
    };
    if let Some(named) = named {
        return named.to_string();
    }
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// One mono keycap. Interface rems throughout, so it scales with Interface
/// size beside the row text; it inherits its host's text colour.
fn keycap(theme: &Theme, label: SharedString) -> gpui::Div {
    div()
        .flex_none()
        .min_w(ui_rems(18.0))
        .min_h(ui_rems(18.0))
        .px(ui_rems(4.0))
        .rounded(px(crate::theme::radius::XS))
        .border_1()
        .border_color(theme.separator(false))
        .bg(theme.fill(FillTier::Quaternary))
        .flex()
        .items_center()
        .justify_center()
        .font_family(theme.font_mono.clone())
        .text_size(ui_rems(11.0))
        .line_height(ui_rems(14.0))
        .font_weight(gpui::FontWeight::NORMAL)
        .child(label)
}

/// A read-only combo: its keycaps without a control frame, announced as text.
fn static_keycaps(
    theme: &Theme,
    id: impl Into<gpui::ElementId>,
    combo: &str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(gpui::Role::Label)
        .aria_label(display_combo(combo))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(ui_rems(3.0))
        .text_color(theme.readable_muted(0.9))
        .children(
            keycaps_on(cfg!(target_os = "macos"), combo)
                .into_iter()
                .map(|cap| keycap(theme, cap)),
        )
}

/// One-line purpose copy per shortcut (luna lib/shortcuts.ts
/// `SHORTCUT_DEFINITIONS` descriptions, verbatim).
fn description(id: ShortcutId) -> &'static str {
    match id {
        ShortcutId::OpenActivity => "Open the Activity and attention center.",
        ShortcutId::CaptureAppshot => {
            "Capture the focused application from anywhere on your desktop."
        }
        ShortcutId::SaveFile => "Save the active workspace file.",
        ShortcutId::BrowserReload => "Reload the focused browser tab.",
        ShortcutId::ToggleSidebar => "Show or hide sessions and settings navigation.",
        ShortcutId::ToggleChanges => "Show or hide the right sidebar for the current session.",
        ShortcutId::ToggleTerminal => "Show or hide the terminal for the current session.",
        ShortcutId::NewSession => "Open a blank session canvas to start a new session.",
        ShortcutId::NewProject => "Open the new project dialog.",
        ShortcutId::OpenModelPicker => "Open the model picker for the current session.",
        ShortcutId::NextSession => {
            "Select the next conversation in the sidebar list, wrapping at the end."
        }
        ShortcutId::PrevSession => {
            "Select the previous conversation in the sidebar list, wrapping at the start."
        }
        ShortcutId::NewThreadChooser => {
            "Choose a New thread or a Side chat from the focused group's chat."
        }
        ShortcutId::NewThreadSplit => {
            "Start a fresh New thread from the focused group's chat in a new group to the right."
        }
        ShortcutId::PreviousTab => "Select the previous tab in the focused group.",
        ShortcutId::NextTab => "Select the next tab in the focused group.",
        ShortcutId::PreviousGroup => "Focus the previous group and its composer.",
        ShortcutId::NextGroup => "Focus the next group and its composer.",
        ShortcutId::ReopenTab => "Reopen the most recently closed workbench tab.",
        ShortcutId::ArchiveSession => "Move the current session to the archived shelf.",
        // One line per slot would repeat itself nine times; the ordinal is
        // already in the row's label.
        ShortcutId::JumpSession(_) => "Open the session at this place in the sidebar list.",
    }
}

/// Headless QA drives Reset and recording through the page's own paths.
#[cfg(feature = "appshots-fixture")]
impl ShortcutsPage {
    /// A row's Reset, exactly as its button runs it.
    pub fn fixture_reset(&mut self, id: ShortcutId, cx: &mut Context<Self>) {
        self.reset_shortcut(id, cx);
    }
    /// Record `keystroke` (e.g. "cmd-]") for `id` through the recorder.
    pub fn fixture_record(&mut self, id: ShortcutId, keystroke: &str, cx: &mut Context<Self>) {
        let Ok(keystroke) = Keystroke::parse(keystroke) else {
            return;
        };
        self.recording = Some(id);
        self.record_keystroke(&keystroke, cx);
        self.stop_recording();
    }
    /// The page's working keymap and its notice.
    pub fn fixture_readback(&self) -> serde_json::Value {
        serde_json::json!({
            "keymap": serde_json::to_value(&self.keymap).unwrap_or_default(),
            "notice": self.conflict_notice.as_ref().map(|notice| notice.to_string()),
        })
    }
}

impl ShortcutsPage {
    /// The page action: restores every shortcut, the send key and the Escape
    /// setting. Disabled — with a tooltip saying why — while nothing is
    /// customized or a combo is recording.
    fn render_restore_defaults(
        &self,
        customized: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let recording = self.recording.is_some();
        let enabled = customized && !recording;
        let tip = if recording {
            "Finish or cancel recording first."
        } else if !customized {
            "Everything on this page is already at its default."
        } else {
            "Restore every shortcut, the send key and the Escape setting."
        };
        button::button(
            "shortcuts-restore-defaults",
            ButtonKind::Secondary,
            ControlSize::Md,
            enabled,
            theme,
        )
        .aria_label("Restore default shortcuts")
        .tooltip(popover::text_tooltip(tip))
        .child(button::glyph(crate::icons::RESTART, ControlSize::Md))
        .child(SharedString::from("Restore defaults"))
        .when(enabled, |el| {
            el.on_click(cx.listener(|this, _, _, cx| {
                this.keymap = KeymapConfig::default();
                this.stop_recording();
                this.conflict_notice = None;
                this.commit(cx);
                this.set_escape_stops_active_agent(false, cx);
                this.set_composer_send_behavior(ComposerSendBehavior::Enter, cx);
            }))
        })
        .into_any_element()
    }

    /// The Chat section: the composer's send key and Escape's stop fallback.
    fn render_chat_section(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let escape_stops_active_agent = self.escape_stops_active_agent;
        let send_behavior = self.composer_send_behavior;
        let send_copy = format!(
            "Choose whether Enter sends immediately or starts a new paragraph. {} always \
             submits; with an empty composer it sends the most recently queued message. \
             {} always inserts a line break.",
            display_combo("mod-enter"),
            display_combo("shift-enter"),
        );
        let send_row = widgets::card_row(theme, false)
            .child(widgets::row_text(
                theme,
                "Send messages with",
                Some(send_copy.into()),
            ))
            .child(
                widgets::row_controls()
                    .when(send_behavior != ComposerSendBehavior::Enter, |el| {
                        el.child(
                            button::icon_button(
                                "composer-send-reset",
                                crate::icons::RESTART,
                                ControlSize::Md,
                                true,
                                theme,
                            )
                            .aria_label("Reset send shortcut")
                            .tooltip(popover::text_tooltip("Reset to Enter"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_composer_send_behavior(ComposerSendBehavior::Enter, cx)
                            })),
                        )
                    })
                    .child(button::segmented(
                        "composer-send-behavior",
                        vec![
                            button::Segment::new("enter", "Enter"),
                            button::Segment::new(
                                "mod-enter",
                                modifier_send_label(cfg!(target_os = "macos")),
                            ),
                        ],
                        usize::from(send_behavior == ComposerSendBehavior::ModEnter),
                        184.0,
                        ControlSize::Md,
                        theme,
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
                    )),
            );
        let escape_row = widgets::card_row(theme, false)
            .child(widgets::row_text(
                theme,
                "Stop active agent with Escape",
                Some(
                    "When no dialog, menu, picker, or terminal handles Escape, stop the agent \
                     in the active session."
                        .into(),
                ),
            ))
            .child(widgets::row_controls().child(widgets::switch_control(
                "escape-stops-active-agent-toggle",
                "Stop active agent with Escape",
                escape_stops_active_agent,
                theme,
                cx,
                move |this, cx| {
                    this.set_escape_stops_active_agent(!escape_stops_active_agent, cx);
                },
            )));
        widgets::section_card(theme)
            .child(widgets::section_header(theme, "Chat", None, Vec::new()))
            .child(send_row)
            .child(escape_row)
    }

    /// The read-only application shortcuts.
    fn render_fixed_section(theme: &Theme) -> gpui::Div {
        FIXED_SHORTCUTS.into_iter().enumerate().fold(
            widgets::section_card(theme).child(widgets::section_header(
                theme,
                "Fixed shortcuts",
                Some("Built into LUNA; these can't be changed.".into()),
                Vec::new(),
            )),
            |card, (ix, (title, combo))| {
                card.child(
                    widgets::card_row(theme, false)
                        .child(widgets::row_text(theme, title, None))
                        .child(
                            widgets::row_controls()
                                .child(static_keycaps(theme, ("fixed-shortcut", ix), combo)),
                        ),
                )
            },
        )
    }
}

impl Render for ShortcutsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.appshot_capabilities = crate::appshots::capabilities();
        if self.appshots_page {
            if std::mem::take(&mut self.appshots_focus_pending) {
                window.focus(&self.focus, cx);
            }
            return self.render_appshots(cx);
        }
        let theme = Theme::of(cx).clone();
        let recording = self.recording;
        let customized = self.keymap != KeymapConfig::default()
            || self.escape_stops_active_agent
            || self.composer_send_behavior != ComposerSendBehavior::default();

        let restore = self.render_restore_defaults(customized, &theme, cx);
        let mut sections: Vec<gpui::AnyElement> =
            vec![self.render_chat_section(&theme, cx).into_any_element()];
        // One card per group, its title inside. `ix` (the id's position in
        // ALL) keys the interactive elements, so ids stay unique across cards.
        for name in GROUP_ORDER {
            if name == "Appshots" {
                continue;
            }
            let mut card = widgets::section_card(&theme).child(widgets::section_header(
                &theme,
                name,
                group_description(name),
                Vec::new(),
            ));
            for id in ShortcutId::ALL.into_iter().filter(|&id| group(id) == name) {
                let ix = ShortcutId::ALL.iter().position(|&a| a == id).unwrap_or(0);
                card = card.child(self.render_row(id, ix, recording, &theme, cx));
            }
            sections.push(card.into_any_element());
        }
        sections.push(Self::render_fixed_section(&theme).into_any_element());
        let orphan_notice = self.orphan_notice();

        let scrollbar = self.render_scrollbar(&theme, cx);
        div()
            .id("shortcuts-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("shortcuts-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .track_focus(&self.focus)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_title_bar(
                                &theme,
                                "Keyboard shortcuts",
                                None,
                                vec![restore],
                            ))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Click a shortcut, then press the key combination you want to \
                                 use. Each shortcut must be unique. Changes apply immediately \
                                 and stay on this device.",
                            ))
                            .when_some(orphan_notice, |column, notice| {
                                column.child(
                                    widgets::warning_strip(&theme, notice).mt(ui_rems(16.0)),
                                )
                            })
                            .children(sections),
                    ),
            )
            .children(scrollbar)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn appshots_setup_can_be_enabled_and_configured_by_keyboard(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            let mut page = ShortcutsPage::new(
                state,
                KeymapConfig::default(),
                false,
                ComposerSendBehavior::default(),
                false,
                false,
                AppshotDestination::Automatic,
                cx,
            );
            page.show_appshots(true);
            page
        });
        window
            .update(cx, |page, w, cx| w.focus(&page.focus, cx))
            .unwrap();
        cx.update_window(window.into(), |_, w, cx| {
            w.draw(cx).clear();
        })
        .unwrap();
        let press = |cx: &mut gpui::TestAppContext, key: &str| {
            cx.update_window(window.into(), |_, w, cx| {
                w.draw(cx).clear();
                let keystroke = gpui::Keystroke::parse(key).unwrap();
                w.dispatch_event(
                    gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                        keystroke: keystroke.clone(),
                        is_held: false,
                        prefer_character_input: false,
                    }),
                    cx,
                );
                w.dispatch_event(
                    gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }),
                    cx,
                );
            })
            .unwrap();
        };
        press(cx, "tab");
        press(cx, "space");
        window
            .update(cx, |page, _, _| assert!(page.appshots_enabled))
            .unwrap();
        press(cx, "tab");
        press(cx, "enter");
        window
            .update(cx, |page, _, _| assert!(page.appshot_sound_enabled))
            .unwrap();
        // Skip the shortcut trigger and Automatic to choose Last session.
        for key in ["tab", "tab", "tab", "space"] {
            press(cx, key);
        }
        window
            .update(cx, |page, _, _| {
                assert_eq!(page.appshot_destination, AppshotDestination::LastSession)
            })
            .unwrap();
        for key in ["tab", "enter"] {
            press(cx, key);
        }
        window
            .update(cx, |page, _, _| {
                assert_eq!(page.appshot_destination, AppshotDestination::NewSession)
            })
            .unwrap();
        for key in ["shift-tab", "space"] {
            press(cx, key);
        }
        window
            .update(cx, |page, _, _| {
                assert_eq!(page.appshot_destination, AppshotDestination::LastSession)
            })
            .unwrap();
    }

    #[gpui::test]
    fn recorder_refuses_bound_actions_before_they_can_run(cx: &mut gpui::TestAppContext) {
        use std::{cell::Cell, rc::Rc};
        let fired = Rc::new(Cell::new(false));
        let observed = fired.clone();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            cx.bind_keys([gpui::KeyBinding::new(
                &crate::settings::platform_combo("mod-n"),
                crate::shell::NewSession,
                None,
            )]);
            cx.on_action(move |_: &crate::shell::NewSession, _| observed.set(true));
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            ShortcutsPage::new(
                state,
                KeymapConfig::default(),
                false,
                ComposerSendBehavior::default(),
                false,
                true,
                AppshotDestination::Automatic,
                cx,
            )
        });
        window
            .update(cx, |page, window, cx| {
                page.start_recording(ShortcutId::CaptureAppshot, window, cx)
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), &crate::settings::platform_combo("mod-n"));
        window
            .update(cx, |page, _, _| {
                assert!(!fired.get(), "the existing action ran while recording");
                assert!(
                    page.conflict_notice
                        .as_deref()
                        .unwrap()
                        .contains("New session")
                );
                assert_eq!(
                    page.keymap.capture_appshot,
                    ShortcutId::CaptureAppshot.default_combo()
                );
                assert!(page.recording.is_none());
                assert!(page.recording_interceptor.is_none());
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), &crate::settings::platform_combo("mod-n"));
        assert!(
            fired.get(),
            "finishing recording must restore normal actions"
        );
    }

    #[test]
    fn recording_outcomes() {
        assert_eq!(
            record_key("escape", false, false, false, false),
            RecordOutcome::Cancelled
        );
        assert_eq!(
            record_key("Escape", true, false, false, false),
            RecordOutcome::Cancelled
        );
        assert_eq!(
            record_key("s", false, false, false, true),
            RecordOutcome::Set("mod-s".into())
        );
        assert_eq!(
            record_key("k", false, true, true, true),
            RecordOutcome::Set("mod-alt-shift-k".into())
        );
        // macOS-only: elsewhere ctrl IS the primary and records as "mod".
        #[cfg(target_os = "macos")]
        assert_eq!(
            record_key("tab", true, false, true, false),
            RecordOutcome::Set("ctrl-shift-tab".into())
        );
        // Bare modifiers stay recording.
        assert_eq!(
            record_key("shift", false, false, true, false),
            RecordOutcome::Ignored
        );
        assert_eq!(
            record_key("ctrl", true, false, false, false),
            RecordOutcome::Ignored
        );
    }

    #[test]
    fn every_shortcut_lands_in_a_rendered_group() {
        // The page renders GROUP_ORDER's cards and nothing else — a group()
        // arm returning a name missing from GROUP_ORDER would silently drop
        // its rows from Settings.
        for id in ShortcutId::ALL {
            assert!(
                GROUP_ORDER.contains(&group(id)),
                "{:?} is grouped under {:?}, which GROUP_ORDER does not render",
                id,
                group(id)
            );
        }
        // And every named group has at least one row — no empty cards.
        for name in GROUP_ORDER {
            assert!(
                ShortcutId::ALL.into_iter().any(|id| group(id) == name),
                "group {:?} would render an empty card",
                name
            );
        }
    }

    #[test]
    fn conflicting_records_are_refused() {
        // luna parity: a combo bound elsewhere is refused at record time (the
        // helper names the owner) — conflicts never persist into the keymap.
        let keymap = KeymapConfig::default();
        let RecordOutcome::Set(combo) = record_key("r", false, false, false, true) else {
            panic!("expected Set");
        };
        assert_eq!(
            conflict_owner(&keymap, ShortcutId::ToggleSidebar, &combo),
            Some(ShortcutId::ToggleChanges)
        );
        // Re-recording a shortcut's own combo is not a conflict.
        assert_eq!(
            conflict_owner(&keymap, ShortcutId::ToggleChanges, &combo),
            None
        );
        // A free combo conflicts with nothing.
        assert_eq!(
            conflict_owner(&keymap, ShortcutId::ToggleSidebar, "mod-shift-x"),
            None
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn appshot_binding_participates_in_existing_conflict_checks() {
        let mut keymap = KeymapConfig::default();
        assert_eq!(
            conflict_owner(&keymap, ShortcutId::CaptureAppshot, "mod-n"),
            Some(ShortcutId::NewSession)
        );
        keymap.set(ShortcutId::CaptureAppshot, "mod-alt-k".into());
        assert_eq!(
            conflict_owner(&keymap, ShortcutId::NewSession, "mod-alt-k"),
            Some(ShortcutId::CaptureAppshot)
        );
        assert_eq!(
            conflict_owner(&keymap, ShortcutId::CaptureAppshot, "mod-alt-k"),
            None
        );
    }

    #[test]
    fn modifier_send_labels_are_platform_specific() {
        assert_eq!(modifier_send_label(true), "⌘ Enter");
        assert_eq!(modifier_send_label(false), "Ctrl Enter");
    }

    #[test]
    fn modifier_send_is_always_reserved_for_the_composer() {
        assert!(send_combo_is_reserved(
            ComposerSendBehavior::Enter,
            "mod-enter"
        ));
        assert!(send_combo_is_reserved(
            ComposerSendBehavior::ModEnter,
            "mod-enter"
        ));
        assert!(!send_combo_is_reserved(
            ComposerSendBehavior::ModEnter,
            "mod-shift-enter"
        ));
    }
}
