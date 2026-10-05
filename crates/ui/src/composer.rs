//! The composer: a hand-rolled multiline text input (adapted from gpui's
//! `examples/input.rs`), the compact↔expanded flip, the Send/Queue/Stop morph,
//! optimistic send with failure recovery, per-chat drafts, and an independent
//! question card above the ordinary composer.
//!
//! Pure decision logic (flip, auto-grow math, button morph, wizard reducer,
//! pending-input detection) lives in free functions/structs with unit tests;
//! the gpui element only feeds them measurements.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyTooltip, App, BorderStyle, Bounds, ClipboardEntry, ClipboardItem, Context, CursorStyle,
    DispatchPhase, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, KeyBinding, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, PaintQuad, PathPromptOptions, Pixels, Point, Role,
    ScrollWheelEvent, SharedString, Style, StyledImage as _, Subscription, Task, TextRun,
    TextStyle, UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, fill, img, point,
    prelude::*, px, quad, relative, size,
};
use sha2::{Digest, Sha256};
use unicode_segmentation::UnicodeSegmentation;

use luna_doc::{MessagePart, MessageRole, SessionCommandPayload, SessionMessageEntry};
use luna_engine::sessions::{LunaApprovalOutcome, SecretInputOutcome};
use luna_proto::{
    CommandReceipt, CommandReceiptStatus, ComposerDraft, ComposerDraftSaveOutcome, DraftAppshot,
    DraftBlob, DraftBlobState, DraftContext, DraftSubmission, DraftSubmissionState,
    FileSearchMatch, HarnessId, ProjectCommandStatus, ProjectCommandsSnapshot, PromptStashEntry,
    RunRequest, SandboxLevel, SlashCommand, UserInputAnswer, UserInputQuestion, capabilities,
};
use luna_rpc::{RpcError, methods};

use crate::appshots::{self, CapturedAppshot};
use crate::attachments::{self, StagedAttachment};
use crate::motion;
use crate::notice::notice_chip;
use crate::pickers::{DraftConfig, Pickers};
use crate::settings::{ComposerSendBehavior, platform_combo};
use crate::state::{AppState, Indicator};
use crate::theme::Theme;

pub(crate) mod chat_reference_search;
pub(crate) mod chat_references;
mod integrations;
#[cfg(feature = "appshots-fixture")]
pub use chat_references::run_chat_references_e2e;
#[cfg(feature = "appshots-fixture")]
pub use goals::run_goal_card_e2e;
#[cfg(feature = "appshots-fixture")]
pub use integrations::run_integrations_picker_e2e;

// ---------------------------------------------------------------------------
// Constants + pure decision logic
// ---------------------------------------------------------------------------

/// Textarea vertical padding: `pt-4 pb-1` (luna composer.tsx line 578) =
/// 16 + 4.
pub const TEXTAREA_PAD_V: f32 = 20.0;
/// The top half of [`TEXTAREA_PAD_V`].
pub const TEXTAREA_PAD_TOP: f32 = 16.0;
/// The expanded textarea BOX (content + padding) is clamped by the original's
/// auto-grow effect: `ta.style.height = Math.min(Math.max(scrollHeight, 76),
/// 260)` (luna composer.tsx line 235). The 76px floor applies even when
/// empty — it's what makes the always-expanded new-chat composer tall.
pub const TEXTAREA_MIN: f32 = 76.0;
pub const TEXTAREA_MAX: f32 = 260.0;
/// Controls row: `pt-1` (4) + h-8 picker chips (32 — the tallest
/// children; composer/styles.tsx pickerChip) + `pb-2.5` (10) — luna
/// composer-actions.tsx line 60.
pub const ACTIONS_ROW_HEIGHT: f32 = 46.0;
/// The pill's 1px hairline, top + bottom (`rounded-[26px] border`).
pub const PILL_BORDER_V: f32 = 2.0;
/// Corner radius shared by the composer and the queue tray behind it.
pub(crate) const COMPOSER_RADIUS: f32 = 26.0;
fn composer_surface_radius(dock_amount: f32) -> f32 {
    COMPOSER_RADIUS - 4.0 * dock_amount
}

/// The pill chrome for pane views that render their own input (side chats).
pub(crate) fn pane_composer_surface(theme: &Theme) -> gpui::Div {
    composer_surface(theme, COMPOSER_RADIUS)
}

/// The question wizard and the ordinary composer use the same pill chrome.
fn composer_surface(theme: &Theme, radius: f32) -> gpui::Div {
    let border = theme.border;
    div()
        .rounded(px(radius))
        .border_1()
        .border_color(border)
        .when(theme.is_frost(), |el| el.bg(theme.composer_sidebar_tint()))
        .when(!theme.is_frost(), |el| {
            el.bg(theme.input_glass_bg()).shadow_lg()
        })
}
/// New-chat composer bounds, border-box: 76 + 46 + 2 = 124 when empty (the
/// blank canvas), 260 + 46 + 2 = 308 at the content cap.
pub const COMPOSER_MIN_HEIGHT: f32 = TEXTAREA_MIN + ACTIONS_ROW_HEIGHT + PILL_BORDER_V;
pub const COMPOSER_MAX_HEIGHT: f32 = TEXTAREA_MAX + ACTIONS_ROW_HEIGHT + PILL_BORDER_V;
/// One-line answer pill (question wizard), border-box: `py-3` (24) + one
/// 22.75px line (rounds to 47) + the 2px hairline = 49.
pub const COMPACT_TOTAL_HEIGHT: f32 = 49.0;
/// An established chat's textarea floor: one input line with breathing room
/// above the controls row (60 + 46 + 2 = 108 when empty).
pub const SESSION_TEXTAREA_MIN: f32 = TEXTAREA_MIN - 16.0;
/// `max-w-3xl`: stable outer width of the centered composer column.
pub const COMPOSER_MAX_WIDTH: f32 = 768.0;
/// The queue reads as a narrower tray emerging from behind the composer.
const QUEUE_SIDE_INSET: f32 = 16.0;
/// The composer covers the tray's lower padding so the queue reads as emerging
/// from behind it instead of as a separate rounded pill.
pub(crate) const QUEUE_COMPOSER_OVERLAP: f32 = 18.0;
/// The original floating selector rows use the same 20px chip height as the
/// established-thread footer. Their surrounding rows own no plate or border.
const NEW_THREAD_SELECTOR_ROW_HEIGHT: f32 = 20.0;
// Accommodate the 24px usage indicator and PR badge without overflowing the
// row's equal 8px top/bottom gutters.
const SESSION_FOOTER_HEIGHT: f32 = 24.0;

/// Route chrome dissolves around the middle of the shared-element move. The
/// two ramps never overlap, which avoids duplicate picker ids/popovers while
/// still letting their surrounding geometry collapse continuously.
fn route_chrome_opacities(new_thread_chrome: f32) -> (f32, f32) {
    let new_thread = ((new_thread_chrome.clamp(0.0, 1.0) - 0.5) * 2.0).clamp(0.0, 1.0);
    let session = (((1.0 - new_thread_chrome.clamp(0.0, 1.0)) - 0.5) * 2.0).clamp(0.0, 1.0);
    (new_thread, session)
}
/// Ignore subpixel noise when the shell reports the conversation width.
const COMPOSER_WIDTH_EPSILON: f32 = 0.5;
/// Input text metrics: `text-[14px] leading-relaxed` = 14 × 1.625 = 22.75.
pub const INPUT_LINE_HEIGHT: f32 = 22.75;
pub const INPUT_TEXT_SIZE: f32 = 14.0;
/// A compact ramp; the glyph-ascent inset keeps the clip edge invisible.
const INPUT_FADE_BAND: f32 = 12.0;
/// Single-select questions auto-advance after this long.
pub const AUTO_ADVANCE_MS: u64 = 220;
/// Drag-selection autoscroll runs at the display-friendly 60fps cadence.
pub const DRAG_SCROLL_FRAME_MS: u64 = 16;

fn composer_width_changed(previous: Option<f32>, current: f32) -> bool {
    previous.is_none_or(|previous| (current - previous).abs() > COMPOSER_WIDTH_EPSILON)
}

/// Caret blink half-period (standard textarea cadence: ~500ms on / 500ms off).
pub const CARET_BLINK_MS: u64 = 500;

/// Caret blink phase for a time since the last keystroke/caret move: solid
/// through the first half-period (typing bursts never blink — each keystroke
/// resets the phase), then alternating.
pub fn caret_visible(ms_since_activity: u64) -> bool {
    (ms_since_activity / CARET_BLINK_MS) % 2 == 0
}

/// Auto-grow: content height for a wrapped-line count.
pub fn input_content_height(wrapped_lines: usize) -> f32 {
    wrapped_lines.max(1) as f32 * INPUT_LINE_HEIGHT
}

/// Total new-chat composer height (border-box) for a content height: the
/// textarea BOX (content + `pt-4 pb-1`) clamps to 76–260 exactly like the
/// original's auto-grow effect, then the 46px actions row and the hairline
/// ride on top. Range 124–308.
pub fn composer_total_height(content_height: f32) -> f32 {
    (content_height + TEXTAREA_PAD_V).clamp(TEXTAREA_MIN, TEXTAREA_MAX)
        + ACTIONS_ROW_HEIGHT
        + PILL_BORDER_V
}

/// Total height of an established chat's two-row composer (main, side and
/// child chats alike): the same auto-grow with a one-line floor. Range
/// 108–308.
pub fn session_total_height(content_height: f32) -> f32 {
    (content_height + TEXTAREA_PAD_V).clamp(SESSION_TEXTAREA_MIN, TEXTAREA_MAX)
        + ACTIONS_ROW_HEIGHT
        + PILL_BORDER_V
}

fn input_max_scroll(content_height: f32, viewport_height: f32) -> f32 {
    (content_height - viewport_height).max(0.0)
}

/// Only settled overflow gets a scroll fade. The animated viewport can be
/// smaller for a few frames while an otherwise fitting draft grows into it.
fn input_overflow_edges(
    content_height: f32,
    settled_height: f32,
    visible_height: f32,
    scroll_top: f32,
) -> (bool, bool) {
    if input_max_scroll(content_height, settled_height) <= 1.0 {
        return (false, false);
    }
    let max_scroll = input_max_scroll(content_height, visible_height);
    (scroll_top > 1.0, scroll_top < max_scroll - 1.0)
}

/// During the reveal, stop at a complete row boundary instead of slicing
/// glyphs with a moving clip. Scrolling offsets the row grid inside the box.
fn input_reveal_height(visible: f32, scroll: f32, line_height: f32, resizing: bool) -> f32 {
    if !resizing {
        return visible;
    }
    let row_end = ((scroll + visible + 0.001) / line_height).floor() * line_height;
    (row_end - scroll).clamp(0.0, visible)
}

/// Apply GPUI's wheel delta to a top-origin input offset. Positive deltas mean
/// scrolling toward the start, matching gpui's built-in list/div behavior.
fn input_scroll_offset(
    current: f32,
    delta_y: f32,
    content_height: f32,
    viewport_height: f32,
) -> f32 {
    (current - delta_y).clamp(0.0, input_max_scroll(content_height, viewport_height))
}

/// Minimally adjust the viewport so the caret row is fully visible.
fn input_scroll_offset_for_cursor(
    current: f32,
    cursor_top: f32,
    cursor_height: f32,
    content_height: f32,
    viewport_height: f32,
    settled_height: Option<f32>,
) -> f32 {
    // Resize the reveal, not the scroll position: existing text stays fixed
    // relative to the input origin throughout the height animation.
    let viewport_height = settled_height.unwrap_or(viewport_height);
    let mut next = current;
    if cursor_top < next {
        next = cursor_top;
    } else if cursor_top + cursor_height > next + viewport_height {
        next = cursor_top + cursor_height - viewport_height;
    }
    next.clamp(0.0, input_max_scroll(content_height, viewport_height))
}

/// What a mouse press in a text field asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PressIntent {
    /// Take the whole field.
    SelectAll,
    /// Grow the current selection to the pressed position.
    ExtendSelection,
    /// Put the caret at the pressed position.
    PlaceCaret,
}

impl PressIntent {
    /// Whether the press starts a drag selection. A select-all must not, or
    /// the next mouse move shrinks it back to a drag from the press position.
    fn arms_drag(self) -> bool {
        !matches!(self, Self::SelectAll)
    }
}

/// Read the intent from the press. Two clicks or more take the whole field,
/// and every further click keeps it, so holding the button through a third
/// click does not change what is selected.
fn press_intent(click_count: usize, shift: bool) -> PressIntent {
    if click_count >= 2 {
        PressIntent::SelectAll
    } else if shift {
        PressIntent::ExtendSelection
    } else {
        PressIntent::PlaceCaret
    }
}

/// Per-frame drag-selection scroll. Distance increases speed, capped at one
/// text row per frame so crossing the input boundary never causes a jump.
fn input_drag_scroll_delta(
    pointer_y: f32,
    viewport_top: f32,
    viewport_bottom: f32,
    line_height: f32,
) -> f32 {
    let distance = if pointer_y < viewport_top {
        pointer_y - viewport_top
    } else if pointer_y > viewport_bottom {
        pointer_y - viewport_bottom
    } else {
        return 0.0;
    };
    distance.signum() * (distance.abs() * 0.2).clamp(1.0, line_height)
}

/// Staged-attachment strip metrics (luna attachment-ui.tsx AttachmentStrip:
/// `flex flex-wrap gap-2 px-4 pt-3`, `size-14` thumbs).
pub const STRIP_THUMB: f32 = 56.0;
pub const STRIP_GAP: f32 = 8.0;
pub const STRIP_PAD_TOP: f32 = 12.0;
pub const STRIP_PAD_X: f32 = 16.0;

/// Height the wrap strip adds to the pill for `count` staged thumbnails at an
/// `inner_width` pill content width (0 when empty). Mirrors flex-wrap: as many
/// 56px thumbs per row as fit with 8px gaps inside the 16px side insets.
pub fn attachment_strip_height(count: usize, inner_width: f32) -> f32 {
    if count == 0 {
        return 0.0;
    }
    let usable = (inner_width - 2.0 * STRIP_PAD_X).max(STRIP_THUMB);
    let per_row = (((usable + STRIP_GAP) / (STRIP_THUMB + STRIP_GAP)).floor() as usize).max(1);
    let rows = count.div_ceil(per_row);
    STRIP_PAD_TOP + rows as f32 * STRIP_THUMB + (rows - 1) as f32 * STRIP_GAP
}

pub fn comment_strip_height(count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    STRIP_PAD_TOP + crate::badges::BADGE_HEIGHT
}

/// Composer height morph (round 9). The original has no height transition (its
/// shell carries only `transition-colors`), so this is a native nicety: a
/// committed height change (auto-grow, staged attachments) starts one 180ms
/// ease-out morph ([`motion::COLLAPSE`]); the blank-thread handoff swaps in the
/// coordinated 420ms route-transition spec. Both use the manual-drive pattern
/// from shell.rs `WidthTween` — never `with_animation`, whose element-id keying
/// replays tweens on remount, round-6 §1–3.
///
/// The morph animates the pill's COMMITTED height: the final layout commits
/// immediately (the input entity never remounts — the caret survives) while
/// the pill clips toward the live target. The pill's bottom edge is stationary
/// on screen, so the controls row stays pinned to it and only the text glides
/// with the sweeping top edge. [`flip_morph_step`] never restarts a morph
/// while the target holds. Reduced motion snaps: no morph is ever created.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlipMorph {
    /// Rendered height when the flip committed — the animation's start point.
    pub from: f32,
    /// Commit time in ms on the caller's monotonic clock.
    pub start_ms: f32,
    /// Ordinary typing flips use the quick collapse spec; the first-send
    /// handoff uses the shell's longer coordinated route timeline.
    pub spec: motion::MotionSpec,
}

impl FlipMorph {
    fn collapse(from: f32, start_ms: f32) -> Self {
        Self {
            from,
            start_ms,
            spec: motion::COLLAPSE,
        }
    }

    fn new_thread_transition(from: f32, start_ms: f32) -> Self {
        Self {
            from,
            start_ms,
            spec: motion::NEW_THREAD_TRANSITION,
        }
    }

    /// Raw timeline position 0..1 over this morph's motion spec.
    fn raw(&self, now_ms: f32) -> f32 {
        let total = self.spec.total().as_secs_f32() * 1000.0;
        ((now_ms - self.start_ms) / total).clamp(0.0, 1.0)
    }

    /// Eased progress 0..1 — also drives the inner geometry handoff.
    pub fn progress(&self, now_ms: f32) -> f32 {
        self.spec.progress(self.raw(now_ms))
    }

    pub fn done(&self, now_ms: f32) -> bool {
        self.raw(now_ms) >= 1.0
    }

    /// Committed-height evaluation: eased lerp from the flip-time height to
    /// the LIVE target (auto-grow may move the target mid-morph — the morph
    /// tracks it instead of finishing on a stale height).
    pub fn height(&self, target: f32, now_ms: f32) -> f32 {
        motion::lerp(self.from, target, self.progress(now_ms))
    }
}

// -- controls row -------------------------------------------------------------
// Every composer (main, side chat, child chat, new chat) shares one two-row
// geometry: the input above, this row pinned to the pill's stationary bottom.

/// Leading and trailing inset of the controls row inside the pill.
pub const ACTIONS_ROW_INSET: f32 = 12.0;
/// Optical join between the picker group and the paperclip. This is tighter
/// than the structural spacing ladder because the narrow paperclip glyph
/// otherwise looks farther away than its hit target actually is.
pub const ACTION_UTILITY_GAP: f32 = 2.0;
/// Structural separation between utility actions and the primary Send action.
pub const ACTION_PRIMARY_GAP: f32 = Theme::SPACE_SM;
/// Gap between the model, access and Goal chips.
pub const CHIP_GAP: f32 = 4.0;
/// Attach and Send are 28px circles.
const ROW_BUTTON: f32 = 28.0;

/// Narrow composers (slice 6): below this available width the pill's outer
/// gutter and the input's side padding shrink with the width.
const NARROW_COMPOSER_WIDTH: f32 = 360.0;
/// The compact controls row: a tray button and Send at this size, with this
/// inset (the pill's other controls move into the tray).
const COMPACT_BUTTON: f32 = 24.0;
const COMPACT_INSET: f32 = 4.0;
/// Room the tray gives the picker chips (labels shown as on a wide row).
const TRAY_CHIP_BUDGET: f32 = 320.0;

/// The composer column's side gutter for an available width.
pub fn composer_gutter(available: f32) -> f32 {
    if available >= NARROW_COMPOSER_WIDTH {
        Theme::SPACE_LG
    } else {
        (available * 0.04).clamp(2.0, Theme::SPACE_LG)
    }
}

/// The text input's side padding inside the pill for an available width.
pub fn input_side_padding(available: f32) -> f32 {
    if available >= NARROW_COMPOSER_WIDTH {
        16.0
    } else {
        (available * 0.06).clamp(6.0, 16.0)
    }
}

/// Whether the full controls row (Attach, an icon-only model chip, access
/// and Send at their normal sizes) fits a pill `surface_width` wide.
pub fn controls_row_fits(surface_width: f32) -> bool {
    controls_row_budget(surface_width) >= 2.0 * crate::pickers::ACCESS_CHIP_SIZE + CHIP_GAP
}

/// Width left for the model, access and Goal chips in a pill `surface_width`
/// wide (border-box).
pub fn controls_row_budget(surface_width: f32) -> f32 {
    surface_width
        - PILL_BORDER_V
        - 2.0 * ACTIONS_ROW_INSET
        - ROW_BUTTON
        - ACTION_UTILITY_GAP
        - ACTION_PRIMARY_GAP
        - ROW_BUTTON
}

/// What the controls row shows at its current width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlsRowFit {
    pub model: crate::pickers::ModelChipFit,
    pub goal_icon_only: bool,
    /// The context-usage chip shows its ring only (share in its card).
    pub usage_icon_only: bool,
}

/// One measured controls row (see [`controls_row_fit`]).
#[derive(Clone, Copy, Debug)]
struct ControlsRowMeasure {
    budget: f32,
    model_full: f32,
    model_name_only: f32,
    goal: Option<(f32, f32)>,
    usage: Option<(f32, f32)>,
    fit: ControlsRowFit,
}

/// Choose the richest controls row that fits `budget` without truncating:
/// the Goal label yields first, then the context share (its ring stays),
/// then the model chip's effort suffix, then its name (leaving the brand
/// mark, like Codex's narrowest composer). The icon-only access chip always
/// stays, and every dropped piece remains in a tooltip or card. `goal` and
/// `usage` are `(label, icon-only)` widths when the Goal switch and the
/// context-usage chip are shown.
pub fn controls_row_fit(
    budget: f32,
    model_full: f32,
    model_name_only: f32,
    goal: Option<(f32, f32)>,
    usage: Option<(f32, f32)>,
) -> ControlsRowFit {
    use crate::pickers::{ACCESS_CHIP_SIZE, ModelChipFit};
    let need = |model: f32, goal: Option<f32>, usage: Option<f32>| {
        model
            + CHIP_GAP
            + ACCESS_CHIP_SIZE
            + goal.map_or(0.0, |goal| CHIP_GAP + goal)
            + usage.map_or(0.0, |usage| CHIP_GAP + usage)
    };
    let candidates = [
        (ModelChipFit::Full, false, false),
        (ModelChipFit::Full, true, false),
        (ModelChipFit::Full, true, true),
        (ModelChipFit::NameOnly, true, true),
        (ModelChipFit::IconOnly, true, true),
    ];
    for (model, goal_icon_only, usage_icon_only) in candidates {
        let model_width = match model {
            ModelChipFit::Full => model_full,
            ModelChipFit::NameOnly => model_name_only,
            ModelChipFit::IconOnly => ACCESS_CHIP_SIZE,
        };
        let goal_width = goal.map(|(label, icon)| if goal_icon_only { icon } else { label });
        let usage_width = usage.map(|(label, icon)| if usage_icon_only { icon } else { label });
        if need(model_width, goal_width, usage_width) <= budget {
            return ControlsRowFit {
                model,
                goal_icon_only,
                usage_icon_only,
            };
        }
    }
    ControlsRowFit {
        model: ModelChipFit::IconOnly,
        goal_icon_only: true,
        usage_icon_only: true,
    }
}

/// Session/route changes SNAP the composer (same rule as the header inset
/// tween, round 6: route swaps remount in the original — zero motion). The
/// nav-driven flip doesn't commit on the first render after a switch (the
/// draft swap has to be laid out and re-measured first), so a plain reset at
/// the nav instant leaks: `last_rendered_height` is repopulated before the
/// flip lands and the session change morphs 49↔124. Instead, every flip
/// committed within this wall-clock window of a navigation snaps. User-driven
/// flips need typing and can't land this fast after a switch.
pub const ROUTE_SNAP_MS: u64 = 250;

/// Advance the flip morph across one render pass. While the committed mode
/// holds, the morph is kept (a finished one clears) — same-mode renders can
/// NEVER restart the animation. A committed mode change starts one morph from
/// the last rendered height, which mid-flight is the CURRENT animated height,
/// so a reverse flip hands off seamlessly instead of popping to an endpoint.
/// Reduced motion (or a first paint with no measured height yet) snaps, and
/// `route_snap` (a session/route change within [`ROUTE_SNAP_MS`]) both blocks
/// arming AND kills anything in flight — navigation never animates the pill.
pub fn flip_morph_step(
    morph: Option<FlipMorph>,
    mode_changed: bool,
    last_height: f32,
    now_ms: f32,
    reduced_motion: bool,
    route_snap: bool,
) -> Option<FlipMorph> {
    if route_snap || reduced_motion {
        return None;
    }
    if !mode_changed {
        return morph.filter(|m| !m.done(now_ms));
    }
    if reduced_motion || last_height <= 0.0 {
        return None;
    }
    Some(FlipMorph::collapse(last_height, now_ms))
}

/// Engines at or above this version understand `pending://` attachment refs
/// and QueueCommand `transfers` (send-is-a-local-write attachments). Gated on
/// BOTH the local engine (an IPC daemon may be older than this UI) and, for
/// remotely-hosted chats, the host device's stamped registry version.
const QUEUED_ATTACHMENTS_MIN: (u64, u64, u64) = (0, 2, 12);

/// What the send button is right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendButtonMode {
    /// No live run: plain send.
    Send,
    /// Live run with text typed: queue for the next turn.
    Queue,
    /// Live run, nothing typed: red stop square.
    Stop,
}

/// What the composer holds that a send could carry. A staged image or diff
/// comment counts: both synthesize their own prompt body, so either alone is
/// a legal send — and during a live run has to read as Queue, not Stop.
pub fn composer_has_content(text: &str, attachments: usize, comments: usize) -> bool {
    !text.trim().is_empty() || attachments > 0 || comments > 0
}

pub const APPSHOT_TILE_MIN_WIDTH: f32 = 96.0;
pub const APPSHOT_IMAGE_INSET: f32 = 12.0;
pub const APPSHOT_PREVIEW_HEIGHT: f32 = 148.0;
pub const APPSHOT_IMAGE_MAX_WIDTH: f32 = 320.0;
pub const APPSHOT_IMAGE_MAX_HEIGHT: f32 = 132.0;
pub const APPSHOT_TILE_HEIGHT: f32 = 192.0;

struct AppshotActionTooltip(SharedString);

impl Render for AppshotActionTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.tooltip_surface())
            .shadow_md()
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

/// Give ordinary captures a shared height while their width follows the
/// source window. Extreme panoramas and narrow composers cap width without
/// cropping. Explicit dimensions also bound the native image while decoding.
pub fn appshot_contained_size(dimensions: Option<(u32, u32)>, max_width: f32) -> (f32, f32) {
    let max_width = if max_width.is_finite() {
        max_width.clamp(1.0, APPSHOT_IMAGE_MAX_WIDTH)
    } else {
        APPSHOT_IMAGE_MAX_WIDTH
    };
    let (width, height) = dimensions
        .filter(|(width, height)| *width > 0 && *height > 0)
        .unwrap_or((16, 10));
    let scale = (max_width / width as f32).min(APPSHOT_IMAGE_MAX_HEIGHT / height as f32);
    (width as f32 * scale, height as f32 * scale)
}

pub fn appshot_strip_height(count: usize) -> f32 {
    if count == 0 {
        0.0
    } else {
        STRIP_PAD_TOP + APPSHOT_TILE_HEIGHT
    }
}

pub fn send_button_mode(run_live: bool, has_text: bool) -> SendButtonMode {
    match (run_live, has_text) {
        (false, _) => SendButtonMode::Send,
        (true, true) => SendButtonMode::Queue,
        (true, false) => SendButtonMode::Stop,
    }
}

/// Queue rows are represented by the queue panel until the host promotes them
/// into the transcript. They must never publish (or refresh) a local echo.
fn should_publish_optimistic_echo(queue: bool) -> bool {
    !queue
}

fn begin_interrupt(pending: &mut HashSet<String>, chat_id: &str) -> bool {
    pending.insert(chat_id.to_string())
}

fn retain_live_interrupts(pending: &mut HashSet<String>, mut is_live: impl FnMut(&str) -> bool) {
    pending.retain(|chat_id| is_live(chat_id));
}

fn interrupt_intent() -> serde_json::Value {
    serde_json::json!({
        "kind": "session",
        "command": { "kind": "interrupt" },
    })
}

fn escape_dismisses_completion(key: &str, completion_open: bool) -> bool {
    key == "escape" && completion_open
}

fn wizard_escape_goes_back(key: &str, input_focused: bool, input_empty: bool) -> bool {
    key == "escape" && (!input_focused || input_empty)
}

fn wizard_number_shortcut(key: &str, modified: bool, answer_focused: bool) -> Option<usize> {
    if modified || answer_focused {
        return None;
    }
    let digit = key.parse::<usize>().ok()?;
    (1..=9).contains(&digit).then(|| digit - 1)
}

/// Find the unresolved input request the panel should serve, if any: an
/// unresolved input part on the LAST assistant entry — regardless of the
/// entry's run status. The question stays answerable until the user actually
/// answers it (user requirement): a run that died under its question (engine
/// restart reaping it) leaves an aborted entry whose answer the engine
/// delivers as a resumed turn (`RespondInput`'s dead-run fallback). A newer
/// assistant entry supersedes an unanswered question. Assistant-entry-scoped,
/// not last-entry: a steer prompt sent while the agent waits appends a USER
/// entry after the streaming assistant entry, and a last-entry-only read made
/// the QuestionPanel vanish exactly when the user typed (earlier forensics;
/// matches the original composer.tsx, which reads the live-assistant fold —
/// rebuilt from replay even after the run died).
pub fn pending_input_request(
    transcript: &[SessionMessageEntry],
) -> Option<(String, Vec<UserInputQuestion>)> {
    let latest = transcript
        .iter()
        .rev()
        .find(|entry| entry.role == MessageRole::Assistant)
        .and_then(|entry| {
            entry.parts.iter().find_map(|part| match part {
                MessagePart::Input {
                    request_id,
                    questions,
                    resolved: false,
                    ..
                } => Some((request_id.clone(), questions.clone())),
                _ => None,
            })
        });
    latest.or_else(|| {
        transcript
            .iter()
            .rev()
            .filter(|entry| entry.role == MessageRole::Assistant)
            .find_map(|entry| {
                entry.parts.iter().find_map(|part| match part {
                    MessagePart::Input {
                        request_id,
                        questions,
                        resolved: false,
                        ..
                    } if questions.iter().any(|q| q.mcp.is_some()) => {
                        Some((request_id.clone(), questions.clone()))
                    }
                    _ => None,
                })
            })
    })
}

fn approval_decision(
    question: &UserInputQuestion,
    answers: &[UserInputAnswer],
) -> luna_proto::ApprovalDecision {
    let selected = answers
        .iter()
        .find(|answer| answer.question_id == question.id)
        .and_then(|answer| answer.labels.first())
        .map(String::as_str);
    if let Some(details) = question
        .approval
        .as_ref()
        .and_then(|approval| approval.details.as_ref())
    {
        if let Some(index) = selected.and_then(|label| {
            details
                .choices
                .iter()
                .position(|choice| choice.label == label)
        }) {
            return luna_proto::ApprovalDecision::ProviderChoice {
                index: index as u32,
            };
        }
        return luna_proto::ApprovalDecision::Deny;
    }
    match selected {
        Some("Allow once") => luna_proto::ApprovalDecision::AllowOnce,
        Some("Allow for task") => luna_proto::ApprovalDecision::AllowTask,
        Some("Cancel") => luna_proto::ApprovalDecision::Cancel,
        _ => luna_proto::ApprovalDecision::Deny,
    }
}

/// Codex requests carry no deadline (`APPROVAL_NO_EXPIRY`): say what ends
/// the wait instead of a clock time.
pub(crate) fn approval_expiry_row(approval: &luna_proto::ApprovalRequest) -> (String, String) {
    match approval.expires() {
        None => (
            "Approval waits".into(),
            "Until you answer, the agent stops or its turn ends".into(),
        ),
        Some(at) => (
            "Approval expires".into(),
            chrono::DateTime::from_timestamp_millis(at)
                .map(|time| {
                    time.with_timezone(&chrono::Local)
                        .format("%H:%M:%S")
                        .to_string()
                })
                .unwrap_or_else(|| "unknown".into()),
        ),
    }
}

fn luna_approval_question(
    questions: &[UserInputQuestion],
) -> Result<Option<&UserInputQuestion>, ()> {
    let has_luna_marker = questions.iter().any(|question| {
        question
            .approval
            .as_ref()
            .is_some_and(luna_proto::ApprovalRequest::is_luna_cross_task)
    });
    if !has_luna_marker {
        return Ok(None);
    }
    if questions.len() != 1 {
        return Err(());
    }
    Ok(questions.first())
}

/// Whether the transcript shows `request_id` explicitly resolved (here or on
/// another device) — the wizard latch's release condition.
pub fn input_request_resolved(transcript: &[SessionMessageEntry], request_id: &str) -> bool {
    transcript.iter().any(|entry| {
        entry.parts.iter().any(|part| {
            matches!(
                part,
                MessagePart::Input {
                    request_id: rid,
                    resolved: true,
                    ..
                } if rid == request_id
            )
        })
    })
}

/// Latest open async card across assistant entries. Codex may keep speaking
/// after emitting it, so a newer assistant message does not supersede it.
pub fn pending_async_question(
    transcript: &[SessionMessageEntry],
) -> Option<(String, Vec<luna_proto::AsyncUserInputQuestion>)> {
    transcript
        .iter()
        .rev()
        .filter(|entry| entry.role == MessageRole::Assistant)
        .find_map(|entry| {
            entry.parts.iter().rev().find_map(|part| match part {
                MessagePart::AsyncQuestion {
                    id,
                    questions,
                    resolved: false,
                } if !questions.is_empty() => Some((id.clone(), questions.clone())),
                _ => None,
            })
        })
}

fn async_question_resolved(transcript: &[SessionMessageEntry], message_id: &str) -> bool {
    transcript.iter().any(|entry| {
        entry.parts.iter().any(|part| {
            matches!(part,
                MessagePart::AsyncQuestion { id, resolved: true, .. } if id == message_id
            )
        })
    })
}

fn async_question_open(transcript: &[SessionMessageEntry], message_id: &str) -> bool {
    transcript.iter().any(|entry| {
        entry.parts.iter().any(|part| {
            matches!(part,
                MessagePart::AsyncQuestion { id, resolved: false, .. } if id == message_id
            )
        })
    })
}

fn async_question_command_id(chat_id: &str, request_id: &str, attempt: u32) -> String {
    let mut digest = Sha256::new();
    digest.update(chat_id.len().to_be_bytes());
    digest.update(chat_id.as_bytes());
    digest.update(request_id.as_bytes());
    digest.update(attempt.to_be_bytes());
    let hex = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("async-question-{hex}")
}

// ---------------------------------------------------------------------------
// Question wizard (pure reducer)
// ---------------------------------------------------------------------------

/// Reducer outcome of a wizard interaction.
#[derive(Debug, Clone, PartialEq)]
pub enum WizardStep {
    Stay,
    /// Single-select landed — advance after [`AUTO_ADVANCE_MS`].
    AutoAdvance,
    /// All pages answered — submit these answers.
    Done(Vec<UserInputAnswer>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionSource {
    /// A provider server request with a live answer resolver.
    Blocking,
    /// Codex `request_user_input_async` agent-message metadata. Answer by
    /// ordinary user steering; Skip only dismisses the card.
    Async,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AsyncDeliveryState {
    Sending,
    Applied,
    Uncertain,
}

#[derive(Debug, Clone)]
struct AsyncQuestionSubmission {
    command_id: String,
    intent: serde_json::Value,
    state: AsyncDeliveryState,
}

/// Paged question state ("1/3"): single-select auto-advances, multi-select and
/// typed answers advance explicitly, number keys 1-9 select, Back pages back.
#[derive(Debug, Clone)]
pub struct Wizard {
    pub request_id: String,
    pub source: QuestionSource,
    pub questions: Vec<UserInputQuestion>,
    pub page: usize,
    picked: Vec<Vec<usize>>,
    typed: Vec<String>,
    url_opening: bool,
}

impl Wizard {
    pub fn new(request_id: String, questions: Vec<UserInputQuestion>) -> Self {
        let n = questions.len();
        let mut wizard = Self {
            request_id,
            source: QuestionSource::Blocking,
            questions,
            page: 0,
            picked: vec![Vec::new(); n],
            typed: vec![String::new(); n],
            url_opening: false,
        };
        for (index, question) in wizard.questions.iter().enumerate() {
            if let Some(luna_proto::McpInputKind::Field { schema, .. }) =
                question.mcp.as_ref().map(|m| &m.kind)
                && let Some(default) = &schema.default
            {
                if !schema.choices.is_empty() {
                    let values = default
                        .as_array()
                        .cloned()
                        .unwrap_or_else(|| vec![default.clone()]);
                    wizard.picked[index] = values
                        .iter()
                        .filter_map(|value| {
                            schema
                                .choices
                                .iter()
                                .position(|choice| Some(choice.value.as_str()) == value.as_str())
                        })
                        .collect();
                } else if schema.value_type == "boolean" {
                    wizard.picked[index] = question
                        .options
                        .iter()
                        .position(|label| label == &default.to_string())
                        .into_iter()
                        .collect();
                } else {
                    wizard.typed[index] = default
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| default.to_string());
                }
            }
        }
        wizard
    }

    pub fn new_async(
        message_id: String,
        questions: Vec<luna_proto::AsyncUserInputQuestion>,
    ) -> Self {
        let mut wizard = Self::new(
            message_id,
            questions
                .into_iter()
                .enumerate()
                .map(|(ix, question)| UserInputQuestion {
                    id: ix.to_string(),
                    header: "Question".into(),
                    question: question.title,
                    options: question.options.unwrap_or_default(),
                    multi_select: false,
                    mcp: None,
                    approval: None,
                    input: None,
                })
                .collect(),
        );
        wizard.source = QuestionSource::Async;
        wizard
    }

    pub fn counter(&self) -> String {
        format!("{}/{}", self.page + 1, self.questions.len().max(1))
    }

    pub fn current(&self) -> Option<&UserInputQuestion> {
        self.questions.get(self.page)
    }

    pub fn is_picked(&self, option_ix: usize) -> bool {
        self.picked
            .get(self.page)
            .is_some_and(|p| p.contains(&option_ix))
    }

    /// Whether the current page has any picked option.
    pub fn page_has_pick(&self) -> bool {
        self.picked.get(self.page).is_some_and(|p| !p.is_empty())
    }

    /// Click/tap an option.
    pub fn select(&mut self, option_ix: usize) -> WizardStep {
        let Some(question) = self.questions.get(self.page) else {
            return WizardStep::Stay;
        };
        if option_ix >= question.options.len() {
            return WizardStep::Stay;
        }
        let multi = question.multi_select;
        let Some(picked) = self.picked.get_mut(self.page) else {
            return WizardStep::Stay;
        };
        if multi {
            match picked.iter().position(|&p| p == option_ix) {
                Some(at) => {
                    picked.remove(at);
                }
                None => picked.push(option_ix),
            }
            WizardStep::Stay
        } else {
            *picked = vec![option_ix];
            if self.source == QuestionSource::Blocking && question.mcp.is_none() {
                WizardStep::AutoAdvance
            } else {
                WizardStep::Stay
            }
        }
    }

    /// Number key 1-9.
    pub fn press_number(&mut self, number: usize) -> WizardStep {
        if number == 0 {
            return WizardStep::Stay;
        }
        self.select(number - 1)
    }

    pub fn set_typed(&mut self, text: String) {
        if let Some(slot) = self.typed.get_mut(self.page) {
            *slot = text;
        }
    }

    pub fn current_typed(&self) -> &str {
        self.typed.get(self.page).map(String::as_str).unwrap_or("")
    }

    pub fn page_has_answer(&self) -> bool {
        self.page_has_pick()
            || if self.current().is_some_and(|q| q.mcp.is_some()) { !self.current_typed().is_empty() } else { !self.current_typed().trim().is_empty() }
            || self.current().is_some_and(|q| q.mcp.as_ref().is_some_and(|m| matches!(&m.kind, luna_proto::McpInputKind::Field { schema, .. } if !schema.required || schema.default.is_some() || (schema.value_type == "string" && schema.choices.is_empty() && schema.min_length.unwrap_or(0) == 0))))
    }

    /// Explicit submit / auto-advance landing.
    pub fn advance(&mut self) -> WizardStep {
        if self.page + 1 < self.questions.len() {
            self.page += 1;
            WizardStep::Stay
        } else {
            WizardStep::Done(self.answers())
        }
    }

    /// Page back; false when already on the first page.
    pub fn back(&mut self) -> bool {
        if self.page > 0 {
            self.page -= 1;
            true
        } else {
            false
        }
    }

    /// Answers per question: free text overrides picked labels.
    pub fn answers(&self) -> Vec<UserInputAnswer> {
        self.questions
            .iter()
            .enumerate()
            .map(|(ix, q)| {
                let typed = self
                    .typed
                    .get(ix)
                    .map(|s| {
                        if q.mcp.is_some() {
                            s.as_str()
                        } else {
                            s.trim()
                        }
                    })
                    .unwrap_or("");
                let required_empty = q.mcp.as_ref().is_some_and(|m| matches!(&m.kind, luna_proto::McpInputKind::Field { schema, .. } if schema.required && schema.value_type == "string" && schema.choices.is_empty() && schema.default.is_none() && schema.min_length.unwrap_or(0) == 0));
                let labels = if !typed.is_empty() || required_empty {
                    vec![typed.to_string()]
                } else {
                    self.picked
                        .get(ix)
                        .map(|picked| {
                            picked
                                .iter()
                                .filter_map(|&p| q.options.get(p).cloned())
                                .collect()
                        })
                        .unwrap_or_default()
                };
                UserInputAnswer {
                    question_id: q.id.clone(),
                    labels,
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Multiline text input (adapted from gpui examples/input.rs)
// ---------------------------------------------------------------------------

actions!(
    composer,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        DocStart,
        DocEnd,
        SelectDocStart,
        SelectDocEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        DeleteToLineEnd,
        Copy,
        Cut,
        Paste,
        Newline,
        MessageNewlineOrAccept,
        ModifierSubmit,
        OppositeSubmit,
        Submit,
        Undo,
        Redo,
        MentionTab,
        StashPrompt,
    ]
);

/// How long a run of single-character edits keeps merging into one undo step.
/// A pause longer than this starts a fresh step, so undo rewinds in the
/// bursts the user actually typed rather than one character at a time.
const UNDO_COALESCE: Duration = Duration::from_millis(700);

/// Cap on retained undo steps — a long-lived composer must not grow forever.
const UNDO_LIMIT: usize = 200;

/// The literal `@` a file chip displays before its name. The icon occupies
/// shaped non-breaking spaces, so its position follows the editor's text
/// layout instead of a guessed character width.
const MENTION_PREFIX: char = '@';
const MENTION_TOOLTIP_DELAY: Duration = Duration::from_millis(420);
const MENTION_TOOLTIP_HEIGHT: f32 = 24.0;
const MENTION_SIDE_PAD: &str = "\u{00A0}";
// Four mono spaces center the 15px glyph with matching space on both sides.
const CHIP_ICON_SLOT: &str = "\u{00A0}\u{00A0}\u{00A0}\u{00A0}";
/// A private URI scheme keeps file mentions distinguishable from ordinary
/// Markdown links pasted into the composer.
const FILE_MENTION_SCHEME: &str = "luna-file:";

/// A restorable point in the input's history: text plus where the caret and
/// selection sat when the edit landed.
#[derive(Clone)]
struct EditSnapshot {
    native_owner: Option<integrations::NativeEditorOwner>,
    native_spans: Vec<integrations::NativeLink>,
    content: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

/// A strict, local-only Markdown representation of a file mention. The
/// underlying prompt always contains this form; the editor projects it to a
/// chip for display without leaking a second data model into submission.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileMentionLink {
    range: Range<usize>,
    basename: String,
    path: String,
    is_dir: bool,
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

fn percent_decode_path(encoded: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(encoded.len());
    let raw = encoded.as_bytes();
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' {
            let hex = std::str::from_utf8(raw.get(at + 1..at + 3)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            bytes.push(raw[at]);
            at += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

fn escape_mention_label(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn local_file_link(path: &str, is_dir: bool) -> String {
    let path = path.trim_end_matches('/');
    let basename = path
        .rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(path);
    format!(
        "[{}]({}{})",
        escape_mention_label(basename),
        FILE_MENTION_SCHEME,
        percent_encode_path(&format!("{path}{}", if is_dir { "/" } else { "" }))
    )
}

/// Build the text inserted when a workspace item is dropped at an arbitrary
/// selection. Unlike completion, a drop does not necessarily happen at a
/// token boundary, so it supplies its own leading separator when needed.
fn dropped_file_mention(
    content: &str,
    range: Range<usize>,
    path: &str,
    is_dir: bool,
) -> Option<(String, usize)> {
    if range.start > range.end
        || !local_path_is_safe(path)
        || !content.is_char_boundary(range.start)
    {
        return None;
    }
    let suffix = content.get(range.end..)?;
    let prefix = if range.start > 0
        && content[..range.start]
            .chars()
            .next_back()
            .is_some_and(|ch| !ch.is_whitespace())
    {
        " "
    } else {
        ""
    };
    let existing_separator = suffix
        .chars()
        .next()
        .filter(|ch| ch.is_whitespace() && *ch != '\n' && *ch != '\r');
    let trailing = if existing_separator.is_some() {
        ""
    } else {
        " "
    };
    let inserted = format!("{prefix}{}{trailing}", local_file_link(path, is_dir));
    let cursor_advance = inserted.len() + existing_separator.map(char::len_utf8).unwrap_or(0);
    Some((inserted, cursor_advance))
}

fn local_path_is_safe(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

fn label_close(text: &str, start: usize) -> Option<usize> {
    let mut escaped = false;
    for (at, ch) in text[start..].char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == ']' && text[start + at + 1..].starts_with('(') {
            return Some(start + at);
        }
    }
    None
}

fn file_mention_links(text: &str) -> Vec<FileMentionLink> {
    let mut links = Vec::new();
    let mut search = 0;
    while let Some(relative_start) = text[search..].find('[') {
        let start = search + relative_start;
        let Some(label_end) = label_close(text, start + 1) else {
            search = start + 1;
            continue;
        };
        let target_start = label_end + 2;
        let Some(relative_end) = text[target_start..].find(')') else {
            search = start + 1;
            continue;
        };
        let end = target_start + relative_end + 1;
        let label = &text[start + 1..label_end];
        let Some(encoded) = text[target_start..end - 1].strip_prefix(FILE_MENTION_SCHEME) else {
            search = end;
            continue;
        };
        let parsed = percent_decode_path(encoded).and_then(|target| {
            let is_dir = target.ends_with('/');
            let path = target.strip_suffix('/').unwrap_or(&target);
            (local_path_is_safe(path)
                && percent_encode_path(&target) == encoded
                && path
                    .rsplit('/')
                    .next()
                    .is_some_and(|basename| escape_mention_label(basename) == label))
            .then(|| (path.to_string(), is_dir))
        });
        if let Some((path, is_dir)) = parsed {
            let basename = path.rsplit('/').next().unwrap_or_default().to_string();
            links.push(FileMentionLink {
                range: start..end,
                basename,
                path,
                is_dir,
            });
        }
        search = end;
    }
    links
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerChipKind {
    Skill,
    Folder,
    File,
    Chat,
}

impl ComposerChipKind {
    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::Skill => crate::icons::MAGIC_WAND_3,
            Self::Folder => crate::icons::FOLDER,
            Self::File => crate::icons::FILE,
            Self::Chat => crate::icons::CHAT_ROUND_LINE,
        }
    }
}

pub(crate) fn composer_chip_palette(
    kind: ComposerChipKind,
    theme: &Theme,
) -> (gpui::Hsla, gpui::Hsla) {
    let (text, dark_alpha) = match kind {
        ComposerChipKind::Skill => (theme.warning, 0.14),
        ComposerChipKind::Folder => (theme.accent, 0.17),
        ComposerChipKind::File => (theme.success, 0.16),
        ComposerChipKind::Chat => (theme.accent, 0.14),
    };
    let wash_alpha = if theme.appearance.is_dark() {
        dark_alpha
    } else {
        0.10
    };
    (text, text.opacity(wash_alpha))
}

#[derive(Debug, Clone)]
struct ComposerChipSpan {
    raw: Range<usize>,
    display: Range<usize>,
    icon_slot: Option<Range<usize>>,
    kind: ComposerChipKind,
}

#[derive(Debug, Clone, Default)]
struct TextProjection {
    display: String,
    mentions: Vec<(FileMentionLink, Range<usize>)>,
    chips: Vec<ComposerChipSpan>,
}

/// A path alone is not enough: two identical relative paths can appear in a
/// draft, so the raw range remains part of the hover identity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MentionTooltipTarget {
    range: Range<usize>,
    path: SharedString,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MentionTooltipPhase {
    Hidden,
    Waiting {
        target: MentionTooltipTarget,
        generation: u64,
    },
    Visible {
        target: MentionTooltipTarget,
        generation: u64,
    },
}

impl MentionTooltipPhase {
    fn target(&self) -> Option<&MentionTooltipTarget> {
        match self {
            Self::Hidden => None,
            Self::Waiting { target, .. } | Self::Visible { target, .. } => Some(target),
        }
    }
}

/// Pure tooltip lifecycle reducer. Motion within the same chip preserves both
/// waiting and visible phases, so normal pointer jitter cannot starve the
/// delay or flicker an already-visible tooltip.
fn mention_tooltip_reduce(
    phase: MentionTooltipPhase,
    pointer_target: Option<MentionTooltipTarget>,
    pointer_in_popup: bool,
    generation: u64,
) -> MentionTooltipPhase {
    match pointer_target {
        Some(target) if phase.target() == Some(&target) => phase,
        Some(target) => MentionTooltipPhase::Waiting { target, generation },
        None if pointer_in_popup && matches!(phase, MentionTooltipPhase::Visible { .. }) => phase,
        None => MentionTooltipPhase::Hidden,
    }
}

fn mention_tooltip_promote(
    phase: MentionTooltipPhase,
    generation: u64,
    target_is_live: bool,
) -> MentionTooltipPhase {
    match phase {
        MentionTooltipPhase::Waiting {
            target,
            generation: current,
        } if current == generation && target_is_live => MentionTooltipPhase::Visible {
            target,
            generation: current,
        },
        MentionTooltipPhase::Waiting {
            generation: current,
            ..
        } if current == generation => MentionTooltipPhase::Hidden,
        phase => phase,
    }
}

fn mention_tooltip_contains(in_chip: bool, in_popup: bool) -> bool {
    in_chip || in_popup
}

fn display_row_segments(
    range: Range<usize>,
    row_ends: impl IntoIterator<Item = usize>,
) -> Vec<(usize, usize, Range<usize>)> {
    let mut segments = Vec::new();
    let mut row_start = 0usize;
    for (row_ix, row_end) in row_ends.into_iter().enumerate() {
        let start = range.start.max(row_start);
        let end = range.end.min(row_end);
        if start < end {
            segments.push((row_ix, row_start, start..end));
        }
        row_start = row_end;
        if row_start >= range.end {
            break;
        }
    }
    segments
}

#[derive(Debug, Clone)]
struct MentionHit {
    target: MentionTooltipTarget,
    bounds: Bounds<Pixels>,
    anchor: Point<Pixels>,
}

impl TextProjection {
    fn new(raw: &str) -> Self {
        Self::project(raw, false, &[])
    }

    fn push_chip(
        &mut self,
        raw: Range<usize>,
        label: &str,
        kind: ComposerChipKind,
        with_icon: bool,
    ) -> Range<usize> {
        let display_start = self.display.len();
        if !with_icon {
            self.display.push_str(MENTION_SIDE_PAD);
        }
        let icon_slot = with_icon.then(|| {
            let start = self.display.len();
            self.display.push_str(CHIP_ICON_SLOT);
            start..self.display.len()
        });
        self.display.push_str(label);
        self.display.push('\u{00A0}');
        let display = display_start..self.display.len();
        self.chips.push(ComposerChipSpan {
            raw,
            display: display.clone(),
            icon_slot,
            kind,
        });
        display
    }

    fn project(raw: &str, with_icons: bool, native_spans: &[integrations::NativeLink]) -> Self {
        let links = file_mention_links(raw);
        let labels = mention_display_labels(&links);
        let mut projection = Self::default();
        let mut raw_at = 0;
        // Completion inserts a trailing separator. Requiring it keeps a slash
        // query editable until the user finishes or accepts the command.
        if with_icons && let Some(end) = raw.find(char::is_whitespace) {
            let command = raw
                .strip_prefix('/')
                .and_then(|tail| tail.get(..end.saturating_sub(1)));
            if command.is_some_and(|name| !name.is_empty() && !name.contains('/')) {
                projection.push_chip(0..end, &raw[..end], ComposerChipKind::Skill, true);
                raw_at = end;
            }
        }
        let mut projected: Vec<_> = links
            .into_iter()
            .zip(labels)
            .map(|(link, label)| {
                let kind = if link.is_dir {
                    ComposerChipKind::Folder
                } else {
                    ComposerChipKind::File
                };
                (link.range.clone(), label, kind, Some(link))
            })
            .collect();
        projected.extend(native_spans.iter().cloned().map(|link| {
            let (name, _) = integrations::identity(&link.item);
            (link.range, name.to_owned(), ComposerChipKind::Skill, None)
        }));
        projected.extend(chat_references::chat_links(raw).into_iter().map(|link| {
            (
                link.range.clone(),
                link.basename.clone(),
                ComposerChipKind::Chat,
                Some(link),
            )
        }));
        projected.sort_by_key(|(range, _, _, _)| range.start);
        for (range, label, kind, file) in projected {
            if range.start < raw_at {
                continue;
            }
            projection.display.push_str(&raw[raw_at..range.start]);
            let mut chip_label = String::with_capacity(label.len() + 1);
            chip_label.push(if kind == ComposerChipKind::Chat {
                '#'
            } else {
                MENTION_PREFIX
            });
            for ch in label.chars() {
                chip_label.push(if ch == ' ' { '\u{00A0}' } else { ch });
            }
            let display = projection.push_chip(range.clone(), &chip_label, kind, with_icons);
            if let Some(link) = file {
                projection.mentions.push((link, display));
            }
            raw_at = range.end;
        }
        projection.display.push_str(&raw[raw_at..]);
        projection
    }

    fn raw_to_display(&self, raw: usize) -> usize {
        let mut raw_at = 0;
        let mut display_at = 0;
        for chip in &self.chips {
            if raw <= chip.raw.start {
                return display_at + raw.saturating_sub(raw_at);
            }
            if raw < chip.raw.end {
                return chip.display.start;
            }
            raw_at = chip.raw.end;
            display_at = chip.display.end;
        }
        display_at + raw.saturating_sub(raw_at)
    }

    fn display_to_raw(&self, display_offset: usize) -> usize {
        let mut raw_at = 0;
        let mut display_at = 0;
        for chip in &self.chips {
            if display_offset <= chip.display.start {
                return raw_at + display_offset.saturating_sub(display_at);
            }
            if display_offset < chip.display.end {
                return if display_offset - chip.display.start < chip.display.len() / 2 {
                    chip.raw.start
                } else {
                    chip.raw.end
                };
            }
            raw_at = chip.raw.end;
            display_at = chip.display.end;
        }
        raw_at + display_offset.saturating_sub(display_at)
    }

    fn normalize_range(&self, range: Range<usize>) -> Range<usize> {
        if range.is_empty() {
            for chip in &self.chips {
                if chip.raw.start < range.start && range.start < chip.raw.end {
                    let midpoint = chip.raw.start + chip.raw.len() / 2;
                    let at = if range.start < midpoint {
                        chip.raw.start
                    } else {
                        chip.raw.end
                    };
                    return at..at;
                }
            }
            return range;
        }
        let mut normalized = range;
        for chip in &self.chips {
            if normalized.start < chip.raw.end && normalized.end > chip.raw.start {
                normalized.start = normalized.start.min(chip.raw.start);
                normalized.end = normalized.end.max(chip.raw.end);
            }
        }
        normalized
    }

    fn previous_boundary(&self, raw: usize) -> Option<usize> {
        self.chips
            .iter()
            .find_map(|chip| (raw == chip.raw.end).then_some(chip.raw.start))
    }

    fn next_boundary(&self, raw: usize) -> Option<usize> {
        self.chips
            .iter()
            .find_map(|chip| (raw == chip.raw.start).then_some(chip.raw.end))
    }
}

/// Basenames are compact in the common case. When the same basename appears
/// more than once, use the shortest unique path suffix so chips remain
/// distinguishable without always expanding to full paths.
fn mention_display_labels(links: &[FileMentionLink]) -> Vec<String> {
    links
        .iter()
        .enumerate()
        .map(|(ix, link)| {
            if links
                .iter()
                .filter(|other| other.basename == link.basename)
                .count()
                == 1
            {
                return link.basename.clone();
            }
            let parts: Vec<_> = link.path.split('/').collect();
            (1..=parts.len())
                .map(|count| parts[parts.len() - count..].join("/"))
                .find(|suffix| {
                    let suffix: Vec<_> = suffix.split('/').collect();
                    links.iter().enumerate().all(|(other_ix, other)| {
                        other_ix == ix
                            || !other
                                .path
                                .split('/')
                                .rev()
                                .take(suffix.len())
                                .eq(suffix.iter().rev().copied())
                    })
                })
                .unwrap_or_else(|| link.path.clone())
        })
        .collect()
}

/// One chip in a *sent* message: its byte range over the projected display
/// string (`@label` between side bearings). The transcript renders these
/// read-only — no editing state, no tooltip machinery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentMentionSpan {
    pub range: Range<usize>,
    /// Full workspace-relative path (labels can be shortened to basenames).
    pub path: SharedString,
    pub is_dir: bool,
}

/// Project a sent message's raw Markdown for transcript display: mention links
/// collapse to the same chip labels the composer shows, everything else passes
/// through untouched. `None` when the text has no valid mention — the
/// substring probe keeps ordinary prompts on the zero-allocation path, so this
/// is safe to call for every user row.
pub fn sent_mention_display(raw: &str) -> Option<(String, Vec<SentMentionSpan>)> {
    if !raw.contains(FILE_MENTION_SCHEME) && !raw.contains("luna://open/chat/") {
        return None;
    }
    let projection = TextProjection::new(raw);
    if projection.mentions.is_empty() {
        return None;
    }
    let spans = projection
        .mentions
        .iter()
        .map(|(link, display)| SentMentionSpan {
            range: display.clone(),
            path: SharedString::from(format!(
                "{}{}",
                link.path,
                if link.is_dir { "/" } else { "" }
            )),
            is_dir: link.is_dir,
        })
        .collect();
    Some((projection.display, spans))
}

/// Render-only chip positions for a sent user bubble. The row retains its
/// existing projected text for copying and storage; these ranges add icon
/// space only to the visible text layout.
#[derive(Debug, Clone)]
pub(crate) struct SentVisualChipSpan {
    pub(crate) range: Range<usize>,
    pub(crate) icon_slot: Range<usize>,
    pub(crate) kind: ComposerChipKind,
    pub(crate) chat_link: Option<SharedString>,
}

fn push_sent_visual_chip(
    display: &mut String,
    spans: &mut Vec<SentVisualChipSpan>,
    label: &str,
    kind: ComposerChipKind,
    append_right_pad: bool,
    chat_link: Option<SharedString>,
) {
    let start = display.len();
    display.push_str(CHIP_ICON_SLOT);
    let icon_slot = start..display.len();
    display.push_str(label);
    if append_right_pad {
        display.push_str(MENTION_SIDE_PAD);
    }
    spans.push(SentVisualChipSpan {
        range: start..display.len(),
        icon_slot,
        kind,
        chat_link,
    });
}

/// Decorate the already-projected user-row text for native rendering. A
/// command may arrive without the trailing space completion inserted because
/// send trims the prompt; it is still a complete chip in a sent message.
pub(crate) fn sent_chip_visual(
    text: &str,
    mentions: &[SentMentionSpan],
) -> Option<(String, Vec<SentVisualChipSpan>)> {
    let slash_end = text.strip_prefix('/').and_then(|_| {
        let end = text.find(char::is_whitespace).unwrap_or(text.len());
        let name = &text[1..end];
        (!name.is_empty() && !name.contains('/')).then_some(end)
    });
    if slash_end.is_none() && mentions.is_empty() {
        return None;
    }

    let mut display =
        String::with_capacity(text.len() + (mentions.len() + 1) * CHIP_ICON_SLOT.len());
    let mut spans = Vec::with_capacity(mentions.len() + usize::from(slash_end.is_some()));
    let mut at = 0;
    if let Some(end) = slash_end {
        push_sent_visual_chip(
            &mut display,
            &mut spans,
            &text[..end],
            ComposerChipKind::Skill,
            true,
            None,
        );
        at = end;
    }
    for mention in mentions {
        if mention.range.start < at {
            continue;
        }
        display.push_str(&text[at..mention.range.start]);
        let label = &text[mention.range.clone()];
        let label = label.strip_prefix(MENTION_SIDE_PAD).unwrap_or(label);
        push_sent_visual_chip(
            &mut display,
            &mut spans,
            label,
            if mention.path.starts_with("luna://open/chat/") {
                ComposerChipKind::Chat
            } else if mention.is_dir {
                ComposerChipKind::Folder
            } else {
                ComposerChipKind::File
            },
            false,
            mention
                .path
                .starts_with("luna://open/chat/")
                .then(|| mention.path.clone()),
        );
        at = mention.range.end;
    }
    display.push_str(&text[at..]);
    Some((display, spans))
}

/// Direction of the last edit — a run only merges with edits of its own kind.
#[derive(Clone, Copy, PartialEq)]
enum EditKind {
    Insert,
    Delete,
}

const GENERIC_COMPOSER_CONTEXT: &str = "Composer";
const MESSAGE_COMPOSER_CONTEXT: &str = "MessageComposer";
const PALETTE_SEARCH_CONTEXT: &str = "PaletteSearch";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageEnterBindingAction {
    Submit,
    ModifierSubmit,
    OppositeSubmit,
    NewlineOrAccept,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageEnterBinding {
    keystroke: String,
    action: MessageEnterBindingAction,
}

fn message_enter_bindings(
    behavior: ComposerSendBehavior,
    modifier_combo: &str,
) -> Vec<MessageEnterBinding> {
    let mut bindings = match behavior {
        ComposerSendBehavior::Enter => vec![
            MessageEnterBinding {
                keystroke: "enter".into(),
                action: MessageEnterBindingAction::Submit,
            },
            MessageEnterBinding {
                keystroke: modifier_combo.into(),
                action: MessageEnterBindingAction::ModifierSubmit,
            },
        ],
        ComposerSendBehavior::ModEnter => vec![
            MessageEnterBinding {
                keystroke: "enter".into(),
                action: MessageEnterBindingAction::NewlineOrAccept,
            },
            MessageEnterBinding {
                keystroke: modifier_combo.into(),
                action: MessageEnterBindingAction::ModifierSubmit,
            },
        ],
    };
    bindings.push(MessageEnterBinding {
        keystroke: format!("shift-{modifier_combo}"),
        action: MessageEnterBindingAction::OppositeSubmit,
    });
    bindings
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnterOutcome {
    AcceptCompletion,
    Submit,
    Newline,
}

fn enter_outcome(has_completion: bool, fallback: EnterOutcome) -> EnterOutcome {
    if has_completion {
        EnterOutcome::AcceptCompletion
    } else {
        fallback
    }
}

fn input_bindings(context: &'static str) -> Vec<KeyBinding> {
    let ctx = Some(context);
    let mut bindings = vec![
        KeyBinding::new("tab", MentionTab, ctx),
        KeyBinding::new("shift-enter", Newline, ctx),
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("shift-home", SelectHome, ctx),
        KeyBinding::new("shift-end", SelectEnd, ctx),
        // macOS line/document motion — a laptop keyboard has no home/end keys,
        // so Cmd+arrow is the only way users reach either edge.
        KeyBinding::new("cmd-left", Home, ctx),
        KeyBinding::new("cmd-right", End, ctx),
        KeyBinding::new("cmd-up", DocStart, ctx),
        KeyBinding::new("cmd-down", DocEnd, ctx),
        KeyBinding::new("shift-cmd-left", SelectHome, ctx),
        KeyBinding::new("shift-cmd-right", SelectEnd, ctx),
        KeyBinding::new("shift-cmd-up", SelectDocStart, ctx),
        KeyBinding::new("shift-cmd-down", SelectDocEnd, ctx),
        // Line-edge deletion (Cmd+Delete on macOS).
        KeyBinding::new("cmd-backspace", DeleteToLineStart, ctx),
        KeyBinding::new("cmd-delete", DeleteToLineEnd, ctx),
    ];
    for prefix in ["cmd", "ctrl"] {
        bindings.push(KeyBinding::new(&format!("{prefix}-z"), Undo, ctx));
        bindings.push(KeyBinding::new(&format!("shift-{prefix}-z"), Redo, ctx));
    }
    // Word-level editing: Option on macOS, Ctrl on Windows/Linux.
    let word_edit_prefix = if cfg!(target_os = "macos") {
        "alt"
    } else {
        "ctrl"
    };
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-backspace"),
        DeleteWordLeft,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-delete"),
        DeleteWordRight,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-left"),
        WordLeft,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-right"),
        WordRight,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-left"),
        SelectWordLeft,
        ctx,
    ));
    bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-right"),
        SelectWordRight,
        ctx,
    ));
    for prefix in ["cmd", "ctrl"] {
        bindings.push(KeyBinding::new(&format!("{prefix}-a"), SelectAll, ctx));
        bindings.push(KeyBinding::new(&format!("{prefix}-c"), Copy, ctx));
        bindings.push(KeyBinding::new(&format!("{prefix}-x"), Cut, ctx));
        bindings.push(KeyBinding::new(&format!("{prefix}-v"), Paste, ctx));
    }
    bindings
}

/// Bind the composer keymap. Call once at app boot.
pub fn init(cx: &mut App, send_behavior: ComposerSendBehavior) {
    let mut generic_bindings = input_bindings(GENERIC_COMPOSER_CONTEXT);
    generic_bindings.push(KeyBinding::new(
        "enter",
        Submit,
        Some(GENERIC_COMPOSER_CONTEXT),
    ));

    let mut message_bindings = input_bindings(MESSAGE_COMPOSER_CONTEXT);
    message_bindings.push(KeyBinding::new(
        &platform_combo("mod-s"),
        StashPrompt,
        Some(MESSAGE_COMPOSER_CONTEXT),
    ));
    for binding in message_enter_bindings(send_behavior, &platform_combo("mod-enter")) {
        match binding.action {
            MessageEnterBindingAction::Submit => message_bindings.push(KeyBinding::new(
                &binding.keystroke,
                Submit,
                Some(MESSAGE_COMPOSER_CONTEXT),
            )),
            MessageEnterBindingAction::ModifierSubmit => message_bindings.push(KeyBinding::new(
                &binding.keystroke,
                ModifierSubmit,
                Some(MESSAGE_COMPOSER_CONTEXT),
            )),
            MessageEnterBindingAction::OppositeSubmit => message_bindings.push(KeyBinding::new(
                &binding.keystroke,
                OppositeSubmit,
                Some(MESSAGE_COMPOSER_CONTEXT),
            )),
            MessageEnterBindingAction::NewlineOrAccept => message_bindings.push(KeyBinding::new(
                &binding.keystroke,
                MessageNewlineOrAccept,
                Some(MESSAGE_COMPOSER_CONTEXT),
            )),
        }
    }

    let word_edit_prefix = if cfg!(target_os = "macos") {
        "alt"
    } else {
        "ctrl"
    };
    // Palette-search context: TEXT-EDITING keys only. gpui dispatches matched
    // keybindings BEFORE raw key listeners (window.rs `dispatch_key_event`),
    // so anything bound here can never reach a palette's `on_key_down` —
    // navigation keys (up/down/left/right/enter) are deliberately unbound and
    // bubble to the palette frame instead.
    let palette = Some(PALETTE_SEARCH_CONTEXT);
    let mut palette_bindings = vec![
        KeyBinding::new("backspace", Backspace, palette),
        KeyBinding::new("delete", Delete, palette),
        KeyBinding::new("home", Home, palette),
        KeyBinding::new("end", End, palette),
        KeyBinding::new("shift-left", SelectLeft, palette),
        KeyBinding::new("shift-right", SelectRight, palette),
        // Modifier-qualified motion is safe here: the palette's own navigation
        // uses BARE arrows/enter, which stay unbound and bubble to its frame.
        KeyBinding::new("cmd-left", Home, palette),
        KeyBinding::new("cmd-right", End, palette),
        KeyBinding::new("shift-cmd-left", SelectHome, palette),
        KeyBinding::new("shift-cmd-right", SelectEnd, palette),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, palette),
    ];
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-backspace"),
        DeleteWordLeft,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-delete"),
        DeleteWordRight,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-left"),
        WordLeft,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("{word_edit_prefix}-right"),
        WordRight,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-left"),
        SelectWordLeft,
        palette,
    ));
    palette_bindings.push(KeyBinding::new(
        &format!("shift-{word_edit_prefix}-right"),
        SelectWordRight,
        palette,
    ));
    for prefix in ["cmd", "ctrl"] {
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-a"), SelectAll, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-c"), Copy, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-x"), Cut, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-v"), Paste, palette));
        palette_bindings.push(KeyBinding::new(&format!("{prefix}-z"), Undo, palette));
        palette_bindings.push(KeyBinding::new(&format!("shift-{prefix}-z"), Redo, palette));
    }
    cx.bind_keys(palette_bindings);
    cx.bind_keys(generic_bindings);
    cx.bind_keys(message_bindings);
}

/// Events the composer wrapper listens for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerInputEvent {
    Submitted,
    ModifierSubmitted,
    OppositeSubmitted,
    Edited,
    CursorMoved,
    ViewportChanged,
    MentionNavigate(isize),
    MentionAccept,
    MentionDismiss,
    StashPrompt,
    HistoryNavigate(isize),
    /// Images pasted from the clipboard (screenshots / copied image data) —
    /// the wrapper stages them as attachments (use-attachments.ts onPaste).
    PastedImages(Vec<gpui::Image>),
    /// File paths pasted from the clipboard (a file manager "Copy").
    PastedPaths(Vec<PathBuf>),
}

/// Shaping inputs excluding mutable viewport and selection geometry.
#[derive(Clone, PartialEq)]
struct InputLayoutKey {
    width: Pixels,
    font: gpui::Font,
    font_size: Pixels,
    color: gpui::Hsla,
    chip_family: SharedString,
    chip_colors: [gpui::Hsla; 4],
    marked_range: Option<Range<usize>>,
    placeholder: SharedString,
    mentions_enabled: bool,
}

/// Multiline input entity: content + selection + IME marked text + measured
/// layout (wrapped lines) for mouse mapping and auto-grow.
pub struct ComposerInput {
    /// Key context for the binding map ("Composer", or "PaletteSearch" for
    /// palette filters whose navigation keys must bubble).
    key_context: &'static str,
    accessibility_role: Role,
    focus_handle: FocusHandle,
    content: String,
    native_owner: Option<integrations::NativeEditorOwner>,
    native_spans: Vec<integrations::NativeLink>,
    pub(crate) read_only: bool,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    is_selecting: bool,
    drag_position: Option<Point<Pixels>>,
    drag_generation: u64,
    drag_autoscroll_active: bool,
    /// Vertical scroll inside the input once content exceeds the max height.
    scroll_top: f32,
    /// Visible content budget supplied by the animated composer.
    viewport_height: Option<f32>,
    /// Final content budget, excluding temporary overflow during a resize.
    settled_viewport_height: Option<f32>,
    resizing: bool,
    overflow_top_padding: f32,
    needs_measure: bool,
    last_layout_key: Option<InputLayoutKey>,
    last_notified_layout: Option<(Pixels, f32)>,
    max_ascent: f32,
    #[cfg(test)]
    layout_rebuilds: usize,
    /// Normally keeps the caret visible through edits and rewraps. Manual
    /// wheel scrolling pauses it until the next caret move or edit.
    follow_cursor: bool,
    text_size: f32,
    configured_line_height: f32,
    single_line: bool,
    scroll_left: f32,
    // -- measured state (written during layout/paint) --
    last_lines: Vec<WrappedLine>,
    line_starts: Vec<usize>,
    last_bounds: Option<Bounds<Pixels>>,
    line_height: Pixels,
    content_height: f32,
    max_line_width: f32,
    last_width: f32,
    /// Raw Markdown → chip display projection from the last layout pass.
    projection: TextProjection,
    /// Inline completion preview: painted in faint ink after the text while
    /// the caret sits at the end (palette tab-completion). Owned by the
    /// wrapper — it recomputes and re-sets this on every render pass, so the
    /// input never has to know what the completion means.
    ghost: Option<SharedString>,
    /// File mentions are a composer feature, not a behavior of generic inputs
    /// (picker searches and rename fields also use this type).
    mentions_enabled: bool,
    /// Secret answer (Codex `isSecret`): paint a same-length mask and never
    /// copy the plaintext to the clipboard.
    masked: bool,
    /// Bumped once per `layout_text` pass — the flip logic uses it to apply at
    /// most one compact↔expanded flip per layout (a flip is only re-evaluated
    /// after the input has been measured in the new mode).
    layout_epoch: u64,
    display_is_placeholder: bool,
    /// Caret blink anchor: reset on every keystroke/caret move so the caret is
    /// solid while typing and blinks at [`CARET_BLINK_MS`] when idle.
    blink_anchor: Instant,
    /// Half-period repaint driver, alive only while the input is focused.
    blink_task: Option<Task<()>>,
    // -- undo history --
    undo_stack: Vec<EditSnapshot>,
    redo_stack: Vec<EditSnapshot>,
    /// Kind, trailing offset, and time of the last edit — the merge test that
    /// decides whether the next edit extends the current undo step.
    last_edit: Option<(EditKind, usize, Instant)>,
    /// The wrapper owns mention state; this only redirects bound keys while a
    /// mention token is active, keeping input focus and native text editing.
    mention_open: bool,
    mention_has_selection: bool,
    /// Set only while Up/Down is walking sent prompts. Ordinary multiline
    /// caret movement takes over as soon as the user moves or edits the text.
    history_navigation_active: bool,
    /// Last prepainted chip bounds; the paint-phase pointer listener uses
    /// these instead of attempting to infer text geometry from the cursor.
    mention_hits: Vec<MentionHit>,
    mention_tooltip: MentionTooltipPhase,
    mention_tooltip_generation: u64,
    mention_tooltip_popup: Option<Bounds<Pixels>>,
    mention_tooltip_task: Option<Task<()>>,
    /// Created once when Waiting promotes; retaining this entity preserves
    /// GPUI's global animation state across prepaint frames.
    mention_tooltip_view: Option<Entity<MentionPathTooltip>>,
}

impl ComposerInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self::with_context(placeholder, GENERIC_COMPOSER_CONTEXT, cx)
    }

    /// An input in a custom KEY context — palettes use `"PaletteSearch"`,
    /// whose keymap binds only text-editing keys so navigation keys bubble to
    /// the surrounding frame (see `init`).
    pub fn with_context(
        placeholder: impl Into<SharedString>,
        key_context: &'static str,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            key_context,
            accessibility_role: Role::MultilineTextInput,
            focus_handle: cx.focus_handle(),
            content: String::new(),
            native_owner: None,
            native_spans: Vec::new(),
            read_only: false,
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            is_selecting: false,
            drag_position: None,
            drag_generation: 0,
            drag_autoscroll_active: false,
            scroll_top: 0.0,
            viewport_height: None,
            settled_viewport_height: None,
            resizing: false,
            overflow_top_padding: 0.0,
            needs_measure: true,
            last_layout_key: None,
            last_notified_layout: None,
            max_ascent: INPUT_TEXT_SIZE,
            #[cfg(test)]
            layout_rebuilds: 0,
            follow_cursor: true,
            text_size: INPUT_TEXT_SIZE,
            configured_line_height: INPUT_LINE_HEIGHT,
            single_line: false,
            scroll_left: 0.0,
            last_lines: Vec::new(),
            line_starts: vec![0],
            last_bounds: None,
            line_height: px(INPUT_LINE_HEIGHT),
            content_height: INPUT_LINE_HEIGHT,
            max_line_width: 0.0,
            last_width: 0.0,
            projection: TextProjection::default(),
            ghost: None,
            mentions_enabled: false,
            masked: false,
            history_navigation_active: false,
            layout_epoch: 0,
            display_is_placeholder: true,
            blink_anchor: Instant::now(),
            blink_task: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
            mention_open: false,
            mention_has_selection: false,
            mention_hits: Vec::new(),
            mention_tooltip: MentionTooltipPhase::Hidden,
            mention_tooltip_generation: 0,
            mention_tooltip_popup: None,
            mention_tooltip_task: None,
            mention_tooltip_view: None,
        }
    }

    /// Override the text metrics for compact one-line surfaces such as
    /// toolbar searches without changing the main composer typography.
    pub fn with_text_metrics(mut self, text_size: f32, line_height: f32) -> Self {
        self.text_size = text_size;
        self.configured_line_height = line_height;
        self.line_height = px(line_height);
        self.content_height = line_height;
        self
    }

    /// Keep compact fields on one row and reveal the caret horizontally.
    pub fn with_single_line(mut self) -> Self {
        self.single_line = true;
        self
    }

    fn set_key_context(&mut self, key_context: &'static str, cx: &mut Context<Self>) {
        if self.key_context != key_context {
            self.key_context = key_context;
            cx.notify();
        }
    }

    pub fn with_accessibility_role(mut self, role: Role) -> Self {
        self.accessibility_role = role;
        self
    }

    /// Reset the caret blink phase (solid again) — called on every edit and
    /// caret move, matching textarea behavior.
    fn reset_blink(&mut self) {
        self.blink_anchor = Instant::now();
    }

    /// Caret paint gate: focused input in an active window, in the "on" blink
    /// phase. Also (re)arms the half-period repaint driver while focused, and
    /// drops it on blur so an unfocused input schedules no frames.
    fn caret_shown(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let focused = self.focus_handle.is_focused(window);
        if !focused || !window.is_window_active() {
            self.blink_task = None;
            return false;
        }
        if self.blink_task.is_none() {
            self.blink_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(CARET_BLINK_MS))
                        .await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
        caret_visible(self.blink_anchor.elapsed().as_millis() as u64)
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_mention_controls(
        &mut self,
        open: bool,
        has_selection: bool,
        cx: &mut Context<Self>,
    ) {
        if self.mention_open == open && self.mention_has_selection == has_selection {
            return;
        }
        self.mention_open = open;
        self.mention_has_selection = has_selection;
        cx.notify();
    }

    fn enable_mentions(&mut self) {
        self.mentions_enabled = true;
        self.refresh_projection();
    }

    /// Mask characters keep each UTF-8 length, so caret/selection byte
    /// offsets stay valid against `content`.
    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        if self.masked != masked {
            self.masked = masked;
            self.needs_measure = true;
            self.last_layout_key = None;
            cx.notify();
        }
    }

    fn refresh_projection(&mut self) {
        if self.masked {
            self.projection = TextProjection {
                display: self
                    .content
                    .chars()
                    .map(|ch| match ch.len_utf8() {
                        1 => '*',
                        2 => '\u{00B7}',
                        3 => '\u{2022}',
                        _ => '\u{1F512}',
                    })
                    .collect(),
                mentions: Vec::new(),
                chips: Vec::new(),
            };
            return;
        }
        self.projection = if self.mentions_enabled {
            TextProjection::project(&self.content, true, &self.native_spans)
        } else {
            TextProjection {
                display: self.content.clone(),
                mentions: Vec::new(),
                chips: Vec::new(),
            }
        };
    }

    /// Replace a completed `@query` token as one non-coalescing undo step.
    pub fn replace_mention(
        &mut self,
        range: Range<usize>,
        path: &str,
        is_dir: bool,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        self.invalidate_mention_tooltip();
        let path = local_file_link(path, is_dir);
        let next = self.content[range.end..].chars().next();
        let existing_separator = next.filter(|ch| ch.is_whitespace() && *ch != '\n' && *ch != '\r');
        let inserted = if existing_separator.is_some() {
            path
        } else {
            format!("{path} ")
        };
        self.record_edit(&range, &inserted);
        self.apply_native_edit(&range, inserted.len());
        self.content =
            self.content[..range.start].to_owned() + &inserted + &self.content[range.end..];
        self.refresh_projection();
        let cursor =
            range.start + inserted.len() + existing_separator.map(char::len_utf8).unwrap_or(0);
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.follow_cursor = true;
        self.reset_blink();
        self.needs_measure = true;
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
    }

    /// Insert a workspace reference at the current selection. Drag-and-drop
    /// uses the same strict local Markdown transport and projected chip as an
    /// `@` mention selected from completion.
    fn insert_dropped_mention(&mut self, path: &str, is_dir: bool, cx: &mut Context<Self>) -> bool {
        if self.read_only {
            return false;
        }
        let range = self.selected_range.clone();
        let Some((inserted, cursor_advance)) =
            dropped_file_mention(&self.content, range.clone(), path, is_dir)
        else {
            return false;
        };
        self.invalidate_mention_tooltip();
        self.record_edit(&range, &inserted);
        self.apply_native_edit(&range, inserted.len());
        self.content =
            self.content[..range.start].to_owned() + &inserted + &self.content[range.end..];
        self.refresh_projection();
        let cursor = range.start + cursor_advance;
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.follow_cursor = true;
        self.reset_blink();
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
        true
    }

    /// Replace a completed plain-text token (slash commands) as one
    /// non-coalescing undo step. The prompt keeps ordinary text; only the
    /// editor projects the completed command as a chip.
    pub fn replace_plain_token(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let next = self.content[range.end..].chars().next();
        let existing_separator = next.filter(|ch| ch.is_whitespace() && *ch != '\n' && *ch != '\r');
        let inserted = if existing_separator.is_some() {
            replacement.to_owned()
        } else {
            format!("{replacement} ")
        };
        self.record_edit(&range, &inserted);
        self.apply_native_edit(&range, inserted.len());
        self.content =
            self.content[..range.start].to_owned() + &inserted + &self.content[range.end..];
        self.refresh_projection();
        let cursor =
            range.start + inserted.len() + existing_separator.map(char::len_utf8).unwrap_or(0);
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.follow_cursor = true;
        self.reset_blink();
        self.needs_measure = true;
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
    }

    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// Set (or clear) the inline completion preview. Only paints while the
    /// caret sits at the end of a non-empty draft — see the prepaint gate.
    pub fn set_ghost(&mut self, ghost: Option<SharedString>, cx: &mut Context<Self>) {
        if self.ghost == ghost {
            return;
        }
        self.ghost = ghost;
        cx.notify();
    }

    pub fn has_newline(&self) -> bool {
        self.content.contains('\n')
    }

    /// Unwrapped width of the widest line — feeds the compact/expanded flip.
    pub fn measured_text_width(&self) -> f32 {
        self.max_line_width
    }

    pub fn measured_content_height(&self) -> f32 {
        self.content_height
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.invalidate_mention_tooltip();
        self.native_spans.clear();
        self.content = text.into();
        if self.single_line {
            self.content = self.content.replace(['\r', '\n'], " ");
        }
        self.refresh_projection();
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_top = 0.0;
        self.scroll_left = 0.0;
        self.follow_cursor = true;
        // Programmatic replacement (draft load, clear-on-submit) is a new
        // document, not an edit — undo must not reach back past it.
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit = None;
        self.reset_blink();
        self.needs_measure = true;
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
    }

    fn invalidate_mention_tooltip(&mut self) {
        self.mention_tooltip_generation = self.mention_tooltip_generation.wrapping_add(1);
        self.mention_tooltip = MentionTooltipPhase::Hidden;
        self.mention_tooltip_popup = None;
        self.mention_tooltip_task = None;
        self.mention_tooltip_view = None;
    }

    fn set_mention_hits(&mut self, hits: Vec<MentionHit>) {
        self.mention_hits = hits;
        let live = self
            .mention_tooltip
            .target()
            .is_none_or(|target| self.mention_hits.iter().any(|hit| &hit.target == target));
        if !live {
            self.invalidate_mention_tooltip();
        }
    }

    fn start_mention_tooltip_wait(&mut self, target: MentionTooltipTarget, cx: &mut Context<Self>) {
        self.mention_tooltip_generation = self.mention_tooltip_generation.wrapping_add(1);
        let generation = self.mention_tooltip_generation;
        self.mention_tooltip = MentionTooltipPhase::Waiting { target, generation };
        self.mention_tooltip_popup = None;
        self.mention_tooltip_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MENTION_TOOLTIP_DELAY).await;
            this.update(cx, |input, cx| {
                let live = input.mention_tooltip.target().is_some_and(|target| {
                    input.mention_hits.iter().any(|hit| &hit.target == target)
                });
                let next = mention_tooltip_promote(input.mention_tooltip.clone(), generation, live);
                if next != input.mention_tooltip {
                    input.mention_tooltip = next;
                    input.mention_tooltip_task = None;
                    if let MentionTooltipPhase::Visible { target, generation } =
                        &input.mention_tooltip
                    {
                        let chat_label = input
                            .projection
                            .mentions
                            .iter()
                            .find(|(mention, _)| {
                                mention.range == target.range
                                    && mention.path.starts_with("luna://open/chat/")
                            })
                            .map(|(mention, _)| {
                                SharedString::from(format!("#{}", mention.basename))
                            });
                        input.mention_tooltip_view = Some(cx.new(|_| MentionPathTooltip {
                            chat: chat_label.is_some(),
                            path: chat_label.unwrap_or_else(|| target.path.clone()),
                            activation: *generation,
                        }));
                    }
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn on_mention_pointer_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.invalidate_mention_tooltip();
            return;
        }
        let target = self
            .mention_hits
            .iter()
            .find(|hit| hit.bounds.contains(&position))
            .map(|hit| hit.target.clone());
        let in_popup = self
            .mention_tooltip_popup
            .is_some_and(|popup| popup.contains(&position));
        let next_generation = self.mention_tooltip_generation.wrapping_add(1);
        let next = mention_tooltip_reduce(
            self.mention_tooltip.clone(),
            target.clone(),
            in_popup,
            next_generation,
        );
        if next == self.mention_tooltip {
            return;
        }
        match next {
            MentionTooltipPhase::Waiting { target, .. } => {
                self.start_mention_tooltip_wait(target, cx)
            }
            _ => {
                self.invalidate_mention_tooltip();
                self.mention_tooltip = next;
                cx.notify();
            }
        }
    }

    fn visible_mention_tooltip(
        &self,
    ) -> Option<(
        MentionTooltipTarget,
        Point<Pixels>,
        u64,
        Entity<MentionPathTooltip>,
    )> {
        let MentionTooltipPhase::Visible { target, generation } = &self.mention_tooltip else {
            return None;
        };
        self.mention_hits
            .iter()
            .find(|hit| hit.target == *target)
            .and_then(|hit| {
                let view = self.mention_tooltip_view.clone()?;
                Some((target.clone(), hit.anchor, *generation, view))
            })
    }

    fn check_mention_tooltip_visibility(
        &mut self,
        popup: Bounds<Pixels>,
        pointer: Point<Pixels>,
    ) -> bool {
        let Some((target, _, _, _)) = self.visible_mention_tooltip() else {
            return false;
        };
        let in_chip = self
            .mention_hits
            .iter()
            .any(|hit| hit.target == target && hit.bounds.contains(&pointer));
        if mention_tooltip_contains(in_chip, popup.contains(&pointer)) {
            self.mention_tooltip_popup = Some(popup);
            true
        } else {
            self.invalidate_mention_tooltip();
            false
        }
    }

    // ---- undo history ----

    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            native_owner: self.native_owner.clone(),
            native_spans: self.native_spans.clone(),
            content: self.content.clone(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    /// Called with the range about to be replaced, BEFORE the content changes,
    /// so the pushed snapshot is the pre-edit state.
    fn record_edit(&mut self, range: &Range<usize>, new_text: &str) {
        let kind = if new_text.is_empty() {
            EditKind::Delete
        } else {
            EditKind::Insert
        };
        // A run merges only while it stays single-character, contiguous with
        // the previous edit, of the same kind, and inside the idle window. A
        // pause, a word break, a paste, or a caret jump all break the run so
        // undo lands on a boundary the user recognizes.
        let mergeable = match (kind, &self.last_edit) {
            (EditKind::Insert, Some((EditKind::Insert, at, when))) => {
                range.is_empty()
                    && range.start == *at
                    && new_text.chars().count() == 1
                    && !new_text.starts_with(['\n', ' ', '\t'])
                    && when.elapsed() < UNDO_COALESCE
            }
            (EditKind::Delete, Some((EditKind::Delete, at, when))) => {
                range.end == *at && when.elapsed() < UNDO_COALESCE
            }
            _ => false,
        };
        if !mergeable {
            self.undo_stack.push(self.snapshot());
            if self.undo_stack.len() > UNDO_LIMIT {
                self.undo_stack.remove(0);
            }
        }
        // Any fresh edit invalidates the redo branch.
        self.redo_stack.clear();
        let tail = match kind {
            EditKind::Insert => range.start + new_text.len(),
            EditKind::Delete => range.start,
        };
        self.last_edit = Some((kind, tail, Instant::now()));
    }

    fn restore(&mut self, snapshot: EditSnapshot, cx: &mut Context<Self>) {
        // Snapshots carry their typed anchors: only a real Undo/Redo of the
        // same owner can bring an identity back; pasted text never does.
        self.native_spans = snapshot.native_spans;
        self.invalidate_mention_tooltip();
        self.content = snapshot.content;
        self.refresh_projection();
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = snapshot.selection_reversed;
        self.marked_range = None;
        self.follow_cursor = true;
        // Never merge a subsequent edit into a step that undo just crossed.
        self.last_edit = None;
        self.reset_blink();
        self.needs_measure = true;
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
    }

    /// History recorded under another draft owner must never be replayed
    /// here; owner binding clears it, and this guard keeps the stacks honest.
    fn history_owned(&mut self, snapshot: &EditSnapshot) -> bool {
        if snapshot.native_owner == self.native_owner {
            return true;
        }
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit = None;
        false
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let Some(previous) = self.undo_stack.pop() else {
            return;
        };
        if !self.history_owned(&previous) {
            return;
        }
        self.redo_stack.push(self.snapshot());
        self.restore(previous, cx);
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let Some(next) = self.redo_stack.pop() else {
            return;
        };
        if !self.history_owned(&next) {
            return;
        }
        self.undo_stack.push(self.snapshot());
        self.restore(next, cx);
    }

    // ---- editing ops ----

    pub fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.projection.normalize_range(offset..offset).start;
        self.selected_range = offset..offset;
        self.follow_cursor = true;
        self.reset_blink();
        cx.emit(ComposerInputEvent::CursorMoved);
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.projection.normalize_range(offset..offset).start;
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.follow_cursor = true;
        self.reset_blink();
        cx.emit(ComposerInputEvent::CursorMoved);
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        if let Some(boundary) = self.projection.previous_boundary(offset) {
            return boundary;
        }
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(ix, _)| (ix < offset).then_some(ix))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        if let Some(boundary) = self.projection.next_boundary(offset) {
            return boundary;
        }
        self.content
            .grapheme_indices(true)
            .find_map(|(ix, _)| (ix > offset).then_some(ix))
            .unwrap_or(self.content.len())
    }

    fn previous_word_boundary(&self, offset: usize) -> usize {
        if let Some(boundary) = self.projection.previous_boundary(offset) {
            return boundary;
        }
        self.content
            .split_word_bound_indices()
            .rev()
            .find_map(|(ix, word)| (ix < offset && !word.trim().is_empty()).then_some(ix))
            .unwrap_or(0)
    }

    fn next_word_boundary(&self, offset: usize) -> usize {
        if let Some(boundary) = self.projection.next_boundary(offset) {
            return boundary;
        }
        self.content
            .split_word_bound_indices()
            .find_map(|(ix, word)| {
                let end = ix + word.len();
                (end > offset && !word.trim().is_empty()).then_some(end)
            })
            .unwrap_or(self.content.len())
    }

    /// Byte range of the logical line containing `offset`.
    fn line_range_at(&self, offset: usize) -> Range<usize> {
        let start = self.content[..offset]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let end = self.content[offset..]
            .find('\n')
            .map(|i| offset + i)
            .unwrap_or(self.content.len());
        start..end
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.select_to(prev, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.select_to(next, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            self.move_to(prev, cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.selected_range.end);
            self.move_to(next, cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.mention_has_selection {
            cx.emit(ComposerInputEvent::MentionNavigate(-1));
            return;
        }
        if self.key_context == MESSAGE_COMPOSER_CONTEXT
            && (self.history_navigation_active || self.content.is_empty())
        {
            cx.emit(ComposerInputEvent::HistoryNavigate(-1));
            return;
        }
        if let Some(ix) = self.vertical_target(-1.0) {
            self.move_to(ix, cx);
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.mention_has_selection {
            cx.emit(ComposerInputEvent::MentionNavigate(1));
            return;
        }
        if self.key_context == MESSAGE_COMPOSER_CONTEXT && self.history_navigation_active {
            cx.emit(ComposerInputEvent::HistoryNavigate(1));
            return;
        }
        if let Some(ix) = self.vertical_target(1.0) {
            self.move_to(ix, cx);
        }
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.vertical_target(-1.0) {
            self.select_to(ix, cx);
        }
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.vertical_target(1.0) {
            self.select_to(ix, cx);
        }
    }

    /// Offset one wrapped line above/below the cursor, keeping its x column.
    /// Clamps to the document edges, matching the platform's behavior on the
    /// first and last line.
    fn vertical_target(&self, dir: f32) -> Option<usize> {
        let current = self.point_for_index(self.cursor_offset())?;
        let target_y = f32::from(current.y) + dir * f32::from(self.line_height);
        if target_y < 0.0 {
            return Some(0);
        }
        if target_y >= self.content_height {
            return Some(self.content.len());
        }
        Some(self.index_for_point(point(current.x, px(target_y))))
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range_at(self.cursor_offset());
        self.move_to(line.start, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range_at(self.cursor_offset());
        self.move_to(line.end, cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range_at(self.cursor_offset());
        self.select_to(line.start, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range_at(self.cursor_offset());
        self.select_to(line.end, cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let prev = self.previous_word_boundary(self.cursor_offset());
        self.move_to(prev, cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        let next = self.next_word_boundary(self.cursor_offset());
        self.move_to(next, cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let prev = self.previous_word_boundary(self.cursor_offset());
        self.select_to(prev, cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        let next = self.next_word_boundary(self.cursor_offset());
        self.select_to(next, cx);
    }

    /// Opt/Cmd + Delete family. With a live selection these delete the
    /// selection only (platform behavior) — the extend runs off the cursor.
    fn delete_to(&mut self, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            if self.cursor_offset() == offset {
                return;
            }
            self.select_to(offset, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prev = self.previous_word_boundary(self.cursor_offset());
        self.delete_to(prev, window, cx);
    }

    fn delete_word_right(
        &mut self,
        _: &DeleteWordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = self.next_word_boundary(self.cursor_offset());
        self.delete_to(next, window, cx);
    }

    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = self.line_range_at(self.cursor_offset()).start;
        self.delete_to(start, window, cx);
    }

    fn delete_to_line_end(
        &mut self,
        _: &DeleteToLineEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let end = self.line_range_at(self.cursor_offset()).end;
        self.delete_to(end, window, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.masked {
            return;
        }
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        } else if let Some(text) = crate::markdown::selection::selected_text() {
            // The composer keeps focus while the user reads the transcript —
            // Cmd+C with no input selection copies the markdown selection.
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only || self.masked {
            return;
        }
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        // Image data (or copied files) beats text — the original composer's
        // onPaste prevents the default text insert when `clipboardData.files`
        // is non-empty and stages the images instead.
        let mut images: Vec<gpui::Image> = Vec::new();
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in &item.entries {
            match entry {
                ClipboardEntry::Image(image) => images.push(image.clone()),
                ClipboardEntry::ExternalPaths(files) => {
                    paths.extend(files.paths().iter().cloned());
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if !images.is_empty() {
            cx.emit(ComposerInputEvent::PastedImages(images));
            return;
        }
        if !paths.is_empty() {
            cx.emit(ComposerInputEvent::PastedPaths(paths));
            return;
        }
        if let Some(text) = item.text() {
            // Compact fields normalize newlines in the input handler.
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn message_newline_or_accept(
        &mut self,
        _: &MessageNewlineOrAccept,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match enter_outcome(self.mention_has_selection, EnterOutcome::Newline) {
            EnterOutcome::AcceptCompletion => cx.emit(ComposerInputEvent::MentionAccept),
            EnterOutcome::Newline => self.replace_text_in_range(None, "\n", window, cx),
            EnterOutcome::Submit => unreachable!("newline action cannot submit"),
        }
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        match enter_outcome(self.mention_has_selection, EnterOutcome::Submit) {
            EnterOutcome::AcceptCompletion => cx.emit(ComposerInputEvent::MentionAccept),
            EnterOutcome::Submit => cx.emit(ComposerInputEvent::Submitted),
            EnterOutcome::Newline => unreachable!("submit action cannot insert a newline"),
        }
    }

    fn modifier_submit(&mut self, _: &ModifierSubmit, _: &mut Window, cx: &mut Context<Self>) {
        match enter_outcome(self.mention_has_selection, EnterOutcome::Submit) {
            EnterOutcome::AcceptCompletion => cx.emit(ComposerInputEvent::MentionAccept),
            EnterOutcome::Submit => cx.emit(ComposerInputEvent::ModifierSubmitted),
            EnterOutcome::Newline => unreachable!("submit action cannot insert a newline"),
        }
    }

    fn opposite_submit(&mut self, _: &OppositeSubmit, _: &mut Window, cx: &mut Context<Self>) {
        match enter_outcome(self.mention_has_selection, EnterOutcome::Submit) {
            EnterOutcome::AcceptCompletion => cx.emit(ComposerInputEvent::MentionAccept),
            EnterOutcome::Submit => cx.emit(ComposerInputEvent::OppositeSubmitted),
            EnterOutcome::Newline => unreachable!("submit action cannot insert a newline"),
        }
    }

    fn mention_tab(&mut self, _: &MentionTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.mention_has_selection {
            cx.emit(ComposerInputEvent::MentionAccept);
        } else {
            cx.propagate();
        }
    }

    fn stash_prompt(&mut self, _: &StashPrompt, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ComposerInputEvent::StashPrompt);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if escape_dismisses_completion(&event.keystroke.key, self.mention_open) {
            cx.emit(ComposerInputEvent::MentionDismiss);
            cx.stop_propagation();
        }
    }

    // ---- geometry ----

    /// Content-local point for a byte index (y grows down from content top).
    fn point_for_index(&self, index: usize) -> Option<Point<Pixels>> {
        self.point_for_display_index(self.projection.raw_to_display(index))
    }

    /// Content-local point for a shaped projection byte index. The icon layer
    /// uses this to occupy its explicit projection slot without inventing a
    /// second coordinate system beside the custom text editor.
    fn point_for_display_index(&self, index: usize) -> Option<Point<Pixels>> {
        for (line_ix, line) in self.last_lines.iter().enumerate() {
            let line_start = *self.line_starts.get(line_ix)?;
            let line_len = line.len();
            if index < line_start {
                continue;
            }
            if index <= line_start + line_len {
                let local = line.position_for_index(index - line_start, self.line_height)?;
                let y_offset: f32 = self
                    .last_lines
                    .iter()
                    .take(line_ix)
                    .map(|l| f32::from(l.size(self.line_height).height))
                    .sum();
                return Some(point(local.x, local.y + px(y_offset)));
            }
        }
        None
    }

    /// Content-local boxes occupied by a projected byte range, split at every
    /// soft wrap. A caret exactly at a wrap boundary belongs visually to both
    /// rows in GPUI; using the explicit wrap indices lets the range's first
    /// glyph start at x=0 on the new row instead of inheriting the old row's
    /// end caret (which previously caused mention washes to be discarded).
    fn bounds_for_display_range(&self, range: Range<usize>) -> Vec<Bounds<Pixels>> {
        let mut bounds = Vec::new();
        let mut y_offset = px(0.0);
        for (line_ix, line) in self.last_lines.iter().enumerate() {
            let line_start = self.line_starts.get(line_ix).copied().unwrap_or(0);
            let local_start = range.start.saturating_sub(line_start).min(line.len());
            let local_end = range.end.saturating_sub(line_start).min(line.len());
            if local_start >= local_end
                || range.end <= line_start
                || range.start >= line_start + line.len()
            {
                y_offset += line.size(self.line_height).height;
                continue;
            }

            let row_ends = line
                .wrap_boundaries()
                .iter()
                .map(|boundary| line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index)
                .chain(std::iter::once(line.len()));
            for (row_ix, row_start, segment) in
                display_row_segments(local_start..local_end, row_ends)
            {
                let row_y = y_offset + self.line_height * row_ix;
                let start_x = if segment.start == row_start {
                    px(0.0)
                } else {
                    line.position_for_index(segment.start, self.line_height)
                        .map(|point| point.x)
                        .unwrap_or(px(0.0))
                };
                if let Some(end_point) = line.position_for_index(segment.end, self.line_height)
                    && end_point.x > start_x
                {
                    bounds.push(Bounds::new(
                        point(start_x, row_y),
                        size(end_point.x - start_x, self.line_height),
                    ));
                }
            }
            y_offset += line.size(self.line_height).height;
        }
        bounds
    }

    /// Byte index closest to a content-local point.
    fn index_for_point(&self, position: Point<Pixels>) -> usize {
        if self.display_is_placeholder {
            return 0;
        }
        let mut y = f32::from(position.y);
        if y < 0.0 {
            return 0;
        }
        for (line_ix, line) in self.last_lines.iter().enumerate() {
            let height = f32::from(line.size(self.line_height).height);
            let line_start = self.line_starts.get(line_ix).copied().unwrap_or(0);
            if y < height || line_ix + 1 == self.last_lines.len() {
                let local = point(position.x, px(y.min(height - 1.0).max(0.0)));
                let ix = line
                    .closest_index_for_position(local, self.line_height)
                    .unwrap_or_else(|ix| ix);
                return self
                    .projection
                    .display_to_raw((line_start + ix).min(self.projection.display.len()));
            }
            y -= height;
        }
        self.content.len()
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        let local = point(
            position.x - bounds.left() + px(self.scroll_left),
            position.y - bounds.top() + px(self.scroll_top),
        );
        self.index_for_point(local)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_mention_tooltip();
        window.focus(&self.focus_handle, cx);
        let intent = press_intent(event.click_count, event.modifiers.shift);
        self.is_selecting = intent.arms_drag();
        self.drag_position = intent.arms_drag().then_some(event.position);
        self.drag_generation = self.drag_generation.wrapping_add(1);
        self.drag_autoscroll_active = false;
        match intent {
            PressIntent::SelectAll => {
                self.move_to(0, cx);
                self.select_to(self.content.len(), cx);
            }
            PressIntent::ExtendSelection => {
                let index = self.index_for_mouse_position(event.position);
                self.select_to(index, cx);
            }
            PressIntent::PlaceCaret => {
                let index = self.index_for_mouse_position(event.position);
                self.move_to(index, cx);
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
        self.drag_position = None;
        self.drag_generation = self.drag_generation.wrapping_add(1);
        self.drag_autoscroll_active = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        self.on_mention_pointer_move(event.position, cx);
        if self.is_selecting {
            self.drag_position = Some(event.position);
            let position = self.drag_selection_position(event.position);
            self.select_to(self.index_for_mouse_position(position), cx);
            if self.drag_scroll_delta(event.position) != 0.0 && !self.drag_autoscroll_active {
                self.start_drag_autoscroll(cx);
            }
        }
    }

    fn start_drag_autoscroll(&mut self, cx: &mut Context<Self>) {
        self.drag_autoscroll_active = true;
        let generation = self.drag_generation;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(DRAG_SCROLL_FRAME_MS))
                    .await;
                let keep_running = this
                    .update(cx, |input, cx| input.drag_autoscroll_tick(generation, cx))
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }

    fn drag_selection_position(&self, position: Point<Pixels>) -> Point<Pixels> {
        let Some(bounds) = self.last_bounds else {
            return position;
        };
        point(
            position.x.clamp(bounds.left(), bounds.right() - px(0.5)),
            position.y.clamp(bounds.top(), bounds.bottom() - px(0.5)),
        )
    }

    fn drag_scroll_delta(&self, position: Point<Pixels>) -> f32 {
        let Some(bounds) = self.last_bounds else {
            return 0.0;
        };
        input_drag_scroll_delta(
            f32::from(position.y),
            f32::from(bounds.top()),
            f32::from(bounds.bottom()),
            f32::from(self.line_height),
        )
    }

    fn drag_autoscroll_tick(&mut self, generation: u64, cx: &mut Context<Self>) -> bool {
        if !self.is_selecting || self.drag_generation != generation {
            return false;
        }
        let (Some(position), Some(bounds)) = (self.drag_position, self.last_bounds) else {
            self.drag_autoscroll_active = false;
            return false;
        };
        let delta = self.drag_scroll_delta(position);
        if delta == 0.0 {
            self.drag_autoscroll_active = false;
            return false;
        }
        let next = (self.scroll_top + delta).clamp(
            0.0,
            input_max_scroll(
                self.content_height,
                self.settled_viewport_height
                    .unwrap_or(f32::from(bounds.size.height)),
            ),
        );
        if next == self.scroll_top {
            self.drag_autoscroll_active = false;
            return false;
        }
        self.scroll_top = next;
        let edge_position = self.drag_selection_position(position);
        self.select_to(self.index_for_mouse_position(edge_position), cx);
        // Selection motion normally resumes caret following. During an edge
        // drag the autoscroll loop owns the viewport instead.
        self.follow_cursor = false;
        true
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bounds) = self.last_bounds else {
            return;
        };
        let viewport_height = self
            .settled_viewport_height
            .unwrap_or(f32::from(bounds.size.height));
        let delta_y = f32::from(event.delta.pixel_delta(self.line_height).y);
        let next = input_scroll_offset(
            self.scroll_top,
            delta_y,
            self.content_height,
            viewport_height,
        );
        if next == self.scroll_top {
            // Overscroll guard: when the input itself is scrollable (content
            // taller than the viewport), swallow the wheel event even at the
            // scroll boundary so it never chains into the outer transcript
            // list (the native equivalent of `overscroll-behavior: contain`).
            if delta_y != 0.0 && input_max_scroll(self.content_height, viewport_height) > 0.0 {
                cx.stop_propagation();
            }
            return;
        }
        self.invalidate_mention_tooltip();
        self.scroll_top = next;
        self.follow_cursor = false;
        cx.stop_propagation();
        cx.emit(ComposerInputEvent::ViewportChanged);
        cx.notify();
    }

    // ---- utf16 mapping (IME) ----

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    /// Shape the text at a width; store measured layout; return content height.
    /// Called from the element's measured-layout closure.
    fn layout_text(
        &mut self,
        width: Pixels,
        style: &TextStyle,
        window: &mut Window,
        cx: &App,
    ) -> f32 {
        let theme = Theme::of(cx);
        let chip_colors = [
            ComposerChipKind::Skill,
            ComposerChipKind::Folder,
            ComposerChipKind::File,
            ComposerChipKind::Chat,
        ]
        .map(|kind| composer_chip_palette(kind, theme).0);
        let key = InputLayoutKey {
            width,
            font: style.font(),
            font_size: style.font_size.to_pixels(window.rem_size()),
            color: style.color,
            chip_family: theme.font_mono.clone(),
            chip_colors,
            marked_range: self.marked_range.clone(),
            placeholder: self.placeholder.clone(),
            mentions_enabled: self.mentions_enabled,
        };
        // Height-only animation, scrolling, selection and caret blinking do
        // not change shaping. Reuse the entity's single retained layout,
        // including the parent's early measurement of this same edit.
        if !self.needs_measure && self.last_layout_key.as_ref() == Some(&key) {
            self.layout_epoch += 1;
            return self.content_height;
        }
        #[cfg(test)]
        {
            self.layout_rebuilds += 1;
        }
        // Rebuild this even for an empty draft. Otherwise deleting the final
        // mention can leave its previous paint geometry alive while the
        // placeholder is already being shaped, tinting "Do anything" for a
        // frame (or longer when no subsequent layout is requested).
        self.refresh_projection();
        let (display, is_placeholder) = if self.content.is_empty() {
            (self.placeholder.clone(), true)
        } else {
            (SharedString::from(self.projection.display.clone()), false)
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        self.line_height = px(self.configured_line_height);

        // Each chip keeps the familiar mono label while its semantic tone
        // separates slash commands, directories, and files.
        let chip_font = gpui::font(theme.font_mono.clone());
        let run_for = |len: usize, underline: bool, chip: Option<ComposerChipKind>| TextRun {
            len,
            font: if chip.is_some() {
                chip_font.clone()
            } else {
                style.font()
            },
            color: chip
                .map(|kind| composer_chip_palette(kind, theme).0)
                .unwrap_or(style.color),
            // Rounded chip washes are painted explicitly beneath the text;
            // TextRun backgrounds are square and can disappear in wrapped runs.
            background_color: None,
            underline: underline.then_some(UnderlineStyle {
                color: Some(style.color),
                thickness: px(1.0),
                wavy: false,
            }),
            strikethrough: None,
        };
        let runs: Vec<TextRun> = match self.marked_range.as_ref() {
            Some(marked) if !is_placeholder => {
                let start = self.projection.raw_to_display(marked.start);
                let end = self.projection.raw_to_display(marked.end);
                vec![
                    run_for(start, false, None),
                    run_for(end.saturating_sub(start), true, None),
                    run_for(display.len() - end, false, None),
                ]
                .into_iter()
                .filter(|r| r.len > 0)
                .collect()
            }
            _ if is_placeholder => vec![run_for(display.len(), false, None)],
            _ => {
                let mut runs = Vec::new();
                let mut at = 0;
                for chip in &self.projection.chips {
                    if at < chip.display.start {
                        runs.push(run_for(chip.display.start - at, false, None));
                    }
                    if chip.kind == ComposerChipKind::Chat {
                        // A chat reference is a conversation title, not a
                        // path: its label shapes in the input's own face.
                        // Only the icon slot keeps the mono metrics every
                        // chip's 15px glyph is centered in, so the icon
                        // geometry (and file/skill chips) are unchanged.
                        let label = chip
                            .icon_slot
                            .as_ref()
                            .map_or(chip.display.start, |slot| slot.end);
                        if label > chip.display.start {
                            runs.push(run_for(label - chip.display.start, false, Some(chip.kind)));
                        }
                        runs.push(TextRun {
                            font: style.font(),
                            ..run_for(chip.display.end - label, false, Some(chip.kind))
                        });
                    } else {
                        runs.push(run_for(chip.display.len(), false, Some(chip.kind)));
                    }
                    at = chip.display.end;
                }
                if at < display.len() {
                    runs.push(run_for(display.len() - at, false, None));
                }
                runs
            }
        };

        let lines = window
            .text_system()
            .shape_text(
                display,
                font_size,
                &runs,
                (!self.single_line).then_some(width),
                None,
            )
            .map(|small| small.into_vec())
            .unwrap_or_default();

        // Logical line byte offsets (each shaped line covers one \n-split line).
        let mut line_starts = Vec::with_capacity(lines.len());
        let mut at = 0usize;
        for line in &lines {
            line_starts.push(at);
            at += line.len() + 1; // + '\n'
        }
        if line_starts.is_empty() {
            line_starts.push(0);
        }

        let content_height: f32 = lines
            .iter()
            .map(|l| f32::from(l.size(self.line_height).height))
            .sum();
        let max_line_width: f32 = lines
            .iter()
            .map(|l| f32::from(l.unwrapped_layout.width))
            .fold(0.0, f32::max);

        self.display_is_placeholder = is_placeholder;
        self.max_ascent = lines
            .iter()
            .map(|line| f32::from(line.unwrapped_layout.ascent))
            .fold(INPUT_TEXT_SIZE, f32::max);
        self.last_layout_key = Some(key);
        self.last_lines = lines;
        self.line_starts = line_starts;
        self.content_height = content_height.max(self.configured_line_height);
        self.max_line_width = if is_placeholder { 0.0 } else { max_line_width };
        self.last_width = f32::from(width);
        self.needs_measure = false;
        self.layout_epoch += 1;
        self.content_height
    }

    fn paint_bounds(&self, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        let visible = f32::from(bounds.size.height);
        let top_overflow = input_overflow_edges(
            self.content_height,
            self.settled_viewport_height.unwrap_or(visible),
            visible,
            self.scroll_top,
        )
        .0;
        let top_padding = if top_overflow {
            self.overflow_top_padding
        } else {
            0.0
        };
        let height = input_reveal_height(
            visible,
            self.scroll_top,
            f32::from(self.line_height),
            self.resizing,
        );
        Bounds::new(
            point(bounds.left(), bounds.top() - px(top_padding)),
            size(bounds.size.width, px(height + top_padding)),
        )
    }

    /// Keep the cursor visible when content exceeds the element height.
    fn clamp_scroll(&mut self, element_height: f32) -> bool {
        if self.single_line {
            let previous = self.scroll_left;
            let width = (self.last_width - 2.0).max(1.0);
            if let Some(cursor) = self.point_for_index(self.cursor_offset()) {
                let x = f32::from(cursor.x);
                self.scroll_left = self.scroll_left.min(x).max(x - width).max(0.0);
            }
            self.scroll_left = self.scroll_left.min((self.max_line_width - width).max(0.0));
            self.scroll_top = 0.0;
            return self.scroll_left != previous;
        }
        let previous = self.scroll_top;
        if self.follow_cursor {
            if let Some(cursor) = self.point_for_index(self.cursor_offset()) {
                self.scroll_top = input_scroll_offset_for_cursor(
                    self.scroll_top,
                    f32::from(cursor.y),
                    f32::from(self.line_height),
                    self.content_height,
                    element_height,
                    self.settled_viewport_height,
                );
            }
        }
        self.scroll_top = self.scroll_top.clamp(
            0.0,
            input_max_scroll(
                self.content_height,
                self.settled_viewport_height.unwrap_or(element_height),
            ),
        );
        self.scroll_top != previous
    }
}

impl EventEmitter<ComposerInputEvent> for ComposerInput {}

impl Focusable for ComposerInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for ComposerInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self
            .projection
            .normalize_range(self.range_from_utf16(&range_utf16));
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content.get(range)?.to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        self.selected_range = self.projection.normalize_range(self.selected_range.clone());
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let single_line_text;
        let new_text = if self.single_line {
            single_line_text = new_text.replace(['\r', '\n'], " ");
            single_line_text.as_str()
        } else {
            new_text
        };
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let range = self.projection.normalize_range(range);
        self.invalidate_mention_tooltip();
        // An IME commit is the tail of a composition whose pre-composition
        // snapshot was already taken (`replace_and_mark_text_in_range`);
        // recording here would pin undo to the half-composed text instead.
        if self.marked_range.is_none() {
            self.record_edit(&range, new_text);
        }
        self.apply_native_edit(&range, new_text.len());
        self.content =
            self.content[0..range.start].to_owned() + new_text + &self.content[range.end..];
        self.refresh_projection();
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.marked_range.take();
        self.follow_cursor = true;
        self.reset_blink();
        self.needs_measure = true;
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let single_line_text;
        let new_text = if self.single_line {
            single_line_text = new_text.replace(['\r', '\n'], " ");
            single_line_text.as_str()
        } else {
            new_text
        };
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let range = self.projection.normalize_range(range);
        self.invalidate_mention_tooltip();
        // First keystroke of a composition: snapshot the text as it stood
        // before any of it existed, so one undo drops the whole composition.
        if self.marked_range.is_none() {
            self.undo_stack.push(self.snapshot());
            if self.undo_stack.len() > UNDO_LIMIT {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
            self.last_edit = None;
        }
        self.apply_native_edit(&range, new_text.len());
        self.content =
            self.content[0..range.start].to_owned() + new_text + &self.content[range.end..];
        self.refresh_projection();
        if new_text.is_empty() {
            self.marked_range = None;
        } else {
            self.marked_range = Some(range.start..range.start + new_text.len());
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|new_range| new_range.start + range.start..new_range.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.follow_cursor = true;
        self.reset_blink();
        self.needs_measure = true;
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self
            .projection
            .normalize_range(self.range_from_utf16(&range_utf16));
        let start = self.point_for_index(range.start)?;
        let origin = point(
            bounds.left() + start.x - px(self.scroll_left),
            bounds.top() + start.y - px(self.scroll_top),
        );
        Some(Bounds::new(origin, size(px(2.0), self.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        point_in_window: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.index_for_mouse_position(point_in_window);
        Some(self.offset_to_utf16(index))
    }
}

/// The custom element: measured auto-grow layout + shaped-line painting.
struct ComposerTextElement {
    input: Entity<ComposerInput>,
    /// Max content height before internal scrolling kicks in.
    max_content_height: f32,
}

struct MentionPathTooltip {
    path: SharedString,
    chat: bool,
    /// Stable for one `Waiting → Visible` promotion; a later activation gets
    /// a new key and therefore exactly one fresh fade-in.
    activation: u64,
}

impl Render for MentionPathTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        motion::fade_quick(
            ("file-mention-path-tooltip", self.activation),
            div()
                .h(px(MENTION_TOOLTIP_HEIGHT))
                .max_w(px(480.0))
                .flex()
                .items_center()
                .px(px(8.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.tooltip_surface())
                .font_family(if self.chat {
                    theme.font_sans.clone()
                } else {
                    theme.font_mono.clone()
                })
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .child(self.path.clone()),
        )
    }
}

struct ComposerTextPrepaint {
    cursor: Option<PaintQuad>,
    chip_quads: Vec<PaintQuad>,
    chip_icons: Vec<(Bounds<Pixels>, &'static str, gpui::Hsla)>,
    mention_hits: Vec<MentionHit>,
    selection_quads: Vec<PaintQuad>,
    /// Completion preview: window-space origin of the end-of-text caret plus
    /// the suffix to paint there (shaped at paint time — it never joins the
    /// content's own layout, so hit-testing and the caret ignore it).
    ghost: Option<(Point<Pixels>, SharedString)>,
}

impl IntoElement for ComposerTextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for ComposerTextElement {
    type RequestLayoutState = ();
    type PrepaintState = ComposerTextPrepaint;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        let input = self.input.clone();
        let text_style = window.text_style();
        let max_content = self.max_content_height;
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, cx| {
                let width = known.width.unwrap_or(match available.width {
                    gpui::AvailableSpace::Definite(width) => width,
                    _ => px(320.0),
                });
                let content_height = input.update(cx, |input, cx| {
                    input.layout_text(width, &text_style, window, cx)
                });
                size(width, px(content_height.min(max_content)))
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let text_style = window.text_style();
        self.input.update(cx, |input, cx| {
            // Intrinsic measurement may try several widths in one layout.
            // Only publish the resolved geometry, once: notifying for each
            // provisional width starts an endless measure/notify loop.
            input.layout_text(bounds.size.width, &text_style, window, cx);
            let layout = (bounds.size.width, input.content_height);
            let layout_changed = input.last_notified_layout != Some(layout);
            input.last_notified_layout = Some(layout);
            let scrolled = input.clamp_scroll(f32::from(bounds.size.height));
            input.last_bounds = Some(bounds);
            if scrolled || layout_changed {
                cx.emit(ComposerInputEvent::ViewportChanged);
            }
        });
        let input = self.input.read(cx);
        let paint_bounds = input.paint_bounds(bounds);
        let scroll = px(input.scroll_top);
        let origin = point(bounds.left() - px(input.scroll_left), bounds.top() - scroll);
        let theme = Theme::of(cx);
        let selection_color = theme.selection;
        let caret_color = theme.caret;

        let mut chip_quads = Vec::new();
        let mut chip_icons = Vec::new();
        for chip in &input.projection.chips {
            let (text_color, wash) = composer_chip_palette(chip.kind, theme);
            for local_bounds in input.bounds_for_display_range(chip.display.clone()) {
                chip_quads.push(quad(
                    Bounds::new(
                        point(
                            origin.x + local_bounds.origin.x,
                            origin.y + local_bounds.origin.y + px(2.0),
                        ),
                        size(local_bounds.size.width, local_bounds.size.height - px(4.0)),
                    ),
                    px(5.0),
                    wash,
                    px(0.0),
                    gpui::transparent_black(),
                    BorderStyle::default(),
                ));
            }
            if let Some(slot) = &chip.icon_slot
                && let Some(local_bounds) = input.bounds_for_display_range(slot.clone()).first()
            {
                let icon_size = px(15.0);
                chip_icons.push((
                    Bounds::new(
                        point(
                            origin.x
                                + local_bounds.origin.x
                                + (local_bounds.size.width - icon_size) / 2.0,
                            origin.y
                                + local_bounds.origin.y
                                + (local_bounds.size.height - icon_size) / 2.0,
                        ),
                        size(icon_size, icon_size),
                    ),
                    chip.kind.icon(),
                    text_color,
                ));
            }
        }
        let mut mention_hits = Vec::new();
        for (mention, display) in &input.projection.mentions {
            let target = MentionTooltipTarget {
                range: mention.range.clone(),
                path: SharedString::from(format!(
                    "{}{}",
                    mention.path,
                    if mention.is_dir { "/" } else { "" }
                )),
            };
            for local_bounds in input.bounds_for_display_range(display.clone()) {
                let chip_bounds = Bounds::new(
                    point(
                        origin.x + local_bounds.origin.x,
                        origin.y + local_bounds.origin.y + px(2.0),
                    ),
                    size(local_bounds.size.width, local_bounds.size.height - px(4.0)),
                );
                let above_anchor = chip_bounds.top() - px(MENTION_TOOLTIP_HEIGHT) - px(1.0);
                let anchor_y = if above_anchor >= px(0.0) {
                    above_anchor
                } else {
                    // GPUI positions at anchor + 1px; subtracting one keeps the
                    // below fallback flush so the pointer can enter the popup.
                    chip_bounds.bottom() - px(1.0)
                };
                let visible_bounds = chip_bounds.intersect(&paint_bounds);
                if visible_bounds.size.width == px(0.0) || visible_bounds.size.height == px(0.0) {
                    continue;
                }
                mention_hits.push(MentionHit {
                    target: target.clone(),
                    bounds: visible_bounds,
                    // The fixed-height popup starts at anchor + 1px. Moving
                    // the anchor above the chip therefore yields conventional
                    // above-target placement without cursor tracking.
                    anchor: point(chip_bounds.left(), anchor_y),
                });
            }
        }
        let mut selection_quads = Vec::new();
        let mut cursor = None;
        if input.selected_range.is_empty() || input.display_is_placeholder {
            if let Some(p) = input.point_for_index(input.cursor_offset()) {
                cursor = Some(fill(
                    Bounds::new(
                        point(origin.x + p.x, origin.y + p.y),
                        size(px(2.0), input.line_height),
                    ),
                    caret_color,
                ));
            } else if input.display_is_placeholder {
                cursor = Some(fill(
                    Bounds::new(origin, size(px(2.0), input.line_height)),
                    caret_color,
                ));
            }
        } else if let (Some(start), Some(end)) = (
            input.point_for_index(input.selected_range.start),
            input.point_for_index(input.selected_range.end),
        ) {
            let lh = input.line_height;
            if start.y == end.y {
                selection_quads.push(fill(
                    Bounds::from_corners(
                        point(origin.x + start.x, origin.y + start.y),
                        point(origin.x + end.x, origin.y + start.y + lh),
                    ),
                    selection_color,
                ));
            } else {
                // First visual row, full middle rows, last visual row.
                selection_quads.push(fill(
                    Bounds::from_corners(
                        point(origin.x + start.x, origin.y + start.y),
                        point(bounds.right(), origin.y + start.y + lh),
                    ),
                    selection_color,
                ));
                if end.y > start.y + lh {
                    selection_quads.push(fill(
                        Bounds::from_corners(
                            point(origin.x, origin.y + start.y + lh),
                            point(bounds.right(), origin.y + end.y),
                        ),
                        selection_color,
                    ));
                }
                selection_quads.push(fill(
                    Bounds::from_corners(
                        point(origin.x, origin.y + end.y),
                        point(origin.x + end.x, origin.y + end.y + lh),
                    ),
                    selection_color,
                ));
            }
        }
        let tooltip = input.visible_mention_tooltip();
        if let Some((_target, anchor, _activation, view)) = tooltip {
            let view = view.into();
            let input = self.input.clone();
            window.set_tooltip(AnyTooltip {
                view,
                mouse_position: anchor,
                check_visible_and_update: Rc::new(move |popup, window, cx| {
                    input.update(cx, |input, _| {
                        input.check_mention_tooltip_visibility(popup, window.mouse_position())
                    })
                }),
            });
        }
        // The ghost only shows where accepting it would insert: a collapsed
        // caret at the end of real (non-placeholder, non-IME) text.
        let ghost = input
            .ghost
            .clone()
            .filter(|g| {
                !g.is_empty()
                    && !input.display_is_placeholder
                    && input.marked_range.is_none()
                    && input.selected_range.is_empty()
                    && input.cursor_offset() == input.content.len()
            })
            .and_then(|g| {
                input
                    .point_for_index(input.content.len())
                    .map(|p| (point(origin.x + p.x, origin.y + p.y), g))
            });
        ComposerTextPrepaint {
            cursor,
            chip_quads,
            chip_icons,
            mention_hits,
            selection_quads,
            ghost,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        self.input.update(cx, |input, _| {
            input.set_mention_hits(prepaint.mention_hits.clone())
        });
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let input = self.input.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                input.update(cx, |input, cx| input.on_mouse_move(event, cx));
            }
        });

        // WrappedLine isn't Clone — temporarily take the shaped lines out of the
        // entity for painting, then put them back for mouse mapping.
        let (lines, line_height, scroll, scroll_left) = self.input.update(cx, |input, _| {
            (
                std::mem::take(&mut input.last_lines),
                input.line_height,
                input.scroll_top,
                input.scroll_left,
            )
        });

        let paint_bounds = self.input.read(cx).paint_bounds(bounds);
        window.with_content_mask(
            Some(gpui::ContentMask {
                bounds: paint_bounds,
            }),
            |window| {
                for quad in prepaint.chip_quads.drain(..) {
                    window.paint_quad(quad);
                }
                for quad in prepaint.selection_quads.drain(..) {
                    window.paint_quad(quad);
                }
                let mut y = bounds.top() - px(scroll);
                for line in &lines {
                    let height = line.size(line_height).height;
                    let _ = line.paint(
                        point(bounds.left() - px(scroll_left), y),
                        line_height,
                        gpui::TextAlign::Left,
                        Some(bounds),
                        window,
                        cx,
                    );
                    y += height;
                }
                for (bounds, path, color) in prepaint.chip_icons.drain(..) {
                    if let Err(error) = window.paint_svg(
                        bounds,
                        path.into(),
                        None,
                        gpui::TransformationMatrix::default(),
                        color,
                        cx,
                    ) {
                        tracing::warn!(%error, path, "composer chip icon render failed");
                    }
                }
                if let Some((ghost_origin, ghost)) = prepaint.ghost.take() {
                    let style = window.text_style();
                    let font_size = style.font_size.to_pixels(window.rem_size());
                    let run = TextRun {
                        len: ghost.len(),
                        font: style.font(),
                        color: Theme::of(cx).text_faint,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let line = window
                        .text_system()
                        .shape_line(ghost, font_size, &[run], None);
                    // (Clipping comes from the surrounding content mask.)
                    let _ = line.paint(
                        ghost_origin,
                        line_height,
                        gpui::TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                // Caret only when this input is actually focused in an active
                // window (Electron hides it on window deactivation too), and only
                // in the "on" blink phase — solid while typing, ~500ms blink idle.
                if self
                    .input
                    .update(cx, |input, cx| input.caret_shown(window, cx))
                    && let Some(cursor) = prepaint.cursor.take()
                {
                    window.paint_quad(cursor);
                }
            },
        );
        self.input.update(cx, |input, _| {
            input.last_lines = lines;
        });
    }
}

impl Render for ComposerInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let popup_theme = theme.for_popup();
        let theme = if self.key_context == "PaletteSearch"
            || self.accessibility_role == gpui::Role::SearchInput
        {
            &popup_theme
        } else {
            theme
        };
        let text_color = if self.content.is_empty() {
            theme.text_faint
        } else {
            theme.text
        };
        div()
            .id(("composer-input", cx.entity_id()))
            .role(self.accessibility_role)
            .aria_label(self.placeholder.clone())
            .aria_placeholder(self.placeholder.clone())
            .key_context(self.key_context)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::mention_tab))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::delete_to_line_end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::message_newline_or_accept))
            .on_action(cx.listener(Self::modifier_submit))
            .on_action(cx.listener(Self::opposite_submit))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::stash_prompt))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, _| {
                // Capture runs before the clicked control handles the press, so
                // another input can take focus normally during bubbling.
                if event.button == MouseButton::Left && this.focus_handle.is_focused(window) {
                    window.blur();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .w_full()
            .text_size(crate::typography::ui_rems(self.text_size))
            .line_height(crate::typography::ui_rems(self.configured_line_height))
            .text_color(text_color)
            .font_family(theme.font_sans.clone())
            .child({
                let input = cx.entity();
                let ascent = self.max_ascent;
                crate::edge_fade::edge_faded(
                    INPUT_FADE_BAND,
                    true,
                    true,
                    ComposerTextElement {
                        input: input.clone(),
                        max_content_height: self
                            .viewport_height
                            .unwrap_or(TEXTAREA_MAX - TEXTAREA_PAD_V),
                    },
                )
                // Fade through the existing top padding, like the transcript
                // scrolling under its chrome. Account for GPUI's baseline
                // sampling without consuming another inset inside the text box.
                .inset_top(ascent - self.overflow_top_padding)
                .fade_overflow_y_with(move |cx| {
                    let input = input.read(cx);
                    let visible_height = input
                        .last_bounds
                        .map_or(0.0, |bounds| f32::from(bounds.size.height));
                    input_overflow_edges(
                        input.content_height,
                        input
                            .settled_viewport_height
                            .unwrap_or(TEXTAREA_MAX - TEXTAREA_PAD_V),
                        visible_height,
                        input.scroll_top,
                    )
                })
            })
    }
}

// ---------------------------------------------------------------------------
// Composer wrapper
// ---------------------------------------------------------------------------

/// Events the shell listens for.
#[derive(Debug, Clone)]
pub enum ComposerEvent {
    /// Arm the shared-element transition before the draft route is replaced
    /// by the newly-created session. Emitting this before `select_chat` keeps
    /// the first destination frame on the same timeline as the source frame.
    NewThreadTransitionStarted,
    /// A prompt was sent optimistically — give the transcript its exact row
    /// identity so it can anchor the prompt at the top with the reply's
    /// reserved space below it.
    Sent { chat_id: String, message_id: String },
    /// A locally-authored queue row was accepted. It is not a transcript send
    /// yet: the transcript remembers the stable id and promotes it to an
    /// own-turn anchor only when the host materializes the matching bubble.
    Queued { chat_id: String, message_id: String },
    /// Wake the shell after an orderly-close flush completes.
    DraftFlushCompleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MentionToken {
    range: Range<usize>,
    query: String,
}

/// The `@` must begin a token. This intentionally excludes `name@example.com`
/// and ordinary words while allowing punctuation such as `(@src`.
fn mention_token(text: &str, cursor: usize) -> Option<MentionToken> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let token_start = text[..cursor]
        .char_indices()
        .rev()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(at + ch.len_utf8()))
        .unwrap_or(0);
    let Some(relative_at) = text[token_start..cursor].rfind('@') else {
        return None;
    };
    let at = token_start + relative_at;
    let valid_boundary = at == 0
        || text[..at]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{'));
    if text[at + 1..cursor].contains('@') || !valid_boundary {
        return None;
    }
    let end = text[cursor..]
        .char_indices()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(cursor + at))
        .unwrap_or(text.len());
    Some(MentionToken {
        range: at..end,
        query: text[at + 1..cursor].to_string(),
    })
}

/// The `/` must open the input: slash commands are whole-prompt prefixes
/// (`/compact`, `/goal ship it`), so only the first token triggers, and a
/// query containing another `/` (a typed path) never does.
fn slash_token(text: &str, cursor: usize) -> Option<MentionToken> {
    if cursor > text.len() || !text.is_char_boundary(cursor) || !text.starts_with('/') {
        return None;
    }
    let end = text
        .char_indices()
        .find_map(|(at, ch)| ch.is_whitespace().then_some(at))
        .unwrap_or(text.len());
    // Cursor outside the command token (typing the argument): popup closed.
    if cursor == 0 || cursor > end {
        return None;
    }
    let query = &text[1..cursor];
    if query.contains('/') {
        return None;
    }
    Some(MentionToken {
        range: 0..end,
        query: query.to_string(),
    })
}

/// Slash-command completion state: like [`FileMentionState`] but the
/// candidate list is fetched per harness (`ListCommands`, revalidated once per
/// popup open) and filtered locally per keystroke — no RPC, debounce, or
/// skeleton churn while typing.
#[derive(Debug, Clone, Default)]
struct SlashState {
    token: Option<MentionToken>,
    /// Indices into the cached command list, filter-ranked for the query.
    filtered: Vec<usize>,
    active: Option<usize>,
    /// Harness the popup is showing commands for (cache key).
    harness: Option<HarnessId>,
    /// Execution scope (task/project + effective mode) of that list: Claude
    /// loads different commands per workspace and policy (E23).
    scope: String,
    request: u64,
    loading: bool,
    error: Option<SharedString>,
    dismissed: Option<(Range<usize>, String)>,
}

#[derive(Debug, Clone, Default)]
struct FileMentionState {
    native_results: Vec<luna_proto::ComposerCatalogEntry>,
    token: Option<MentionToken>,
    results: Vec<FileSearchMatch>,
    active: Option<usize>,
    request: u64,
    loading: bool,
    /// Why the last search failed, for the popup. A failure MUST NOT render
    /// as "No matching files": cross-device searches fail for reasons the
    /// user can act on (host daemon too old for `SearchFiles`, device
    /// offline), and the empty state hid them (user report).
    error: Option<SharedString>,
    /// Full token text, not just the cursor-relative query: moving within a
    /// dismissed token keeps it closed, while any edit re-enables completion.
    dismissed: Option<(Range<usize>, String)>,
}

fn mention_response_is_current(state: &FileMentionState, request: u64) -> bool {
    state.request == request && state.token.is_some()
}

/// A failed file search, translated for the popup. `UnknownMethod` is the
/// version-skew case: `SearchFiles` shipped after v0.1.9, so a session hosted
/// by a device on an older daemon answers "unknown method" while the same
/// search works for local sessions.
fn mention_error_message(err: &RpcError) -> SharedString {
    match err {
        RpcError::UnknownMethod(_) => {
            "The session's device runs an older luna — update it to search its files".into()
        }
        RpcError::Transport(_) | RpcError::Closed => "The session's device is unreachable".into(),
        RpcError::BadParams(_) | RpcError::Failed(_) => "File search failed".into(),
    }
}

/// A failed command discovery, translated for the popup.
fn slash_error_message(err: &RpcError) -> SharedString {
    match err {
        RpcError::UnknownMethod(_) => {
            "The session's device runs an older luna — update it to list commands".into()
        }
        RpcError::Transport(_) | RpcError::Closed => "The session's device is unreachable".into(),
        RpcError::BadParams(_) | RpcError::Failed(_) => {
            "Couldn't load this agent's commands".into()
        }
    }
}

struct PendingDraftOwnerStage {
    origin_key: String,
    target_key: String,
    engine_generation: u64,
    previous_text: Option<String>,
    target_seen: bool,
}

#[cfg(feature = "appshots-fixture")]
mod claude_prompts_e2e;
#[cfg(feature = "appshots-fixture")]
mod codex_prompts_e2e;
mod goals;
mod work;
#[cfg(feature = "appshots-fixture")]
pub use claude_prompts_e2e::run_claude_prompts_e2e;
#[cfg(feature = "appshots-fixture")]
pub use codex_prompts_e2e::run_codex_prompts_e2e;

pub struct Composer {
    goal_ui: goals::GoalUi,
    pub(crate) state: Entity<AppState>,
    /// Which chat this composer drives: the selected chat (main view) or a
    /// fixed pane chat. Every "own chat" read goes through it.
    pub(crate) binding: crate::chat_binding::ChatBinding,
    pub(crate) input: Entity<ComposerInput>,
    answer_input: Entity<ComposerInput>,
    /// Draft displaced while a queued message occupies the composer.
    pub(crate) queue_edit_draft: Option<(String, Vec<StagedAttachment>, Vec<CapturedAppshot>)>,
    /// Composer actions row plus the new-session floating target tab
    /// ([`Pickers::render_new_thread_target_selectors`]).
    pickers: Entity<Pickers>,
    /// Only changed canvas selections dirty its draft; picker catalog and
    /// layout notifications may arrive while the saved draft is loading.
    last_canvas_draft_config: DraftConfig,
    /// Draft text per chat key ("" = new-chat canvas), surviving navigation.
    drafts: HashMap<String, String>,
    /// Staged-but-unsent attachments per chat key (use-attachments.ts `stash`):
    /// navigating away and back restores them; memory-only, like the original.
    pub(crate) attachments: HashMap<String, Vec<StagedAttachment>>,
    /// Native selections are durable content, keyed identically to drafts.
    pub(crate) input_items: HashMap<String, Vec<luna_proto::ComposerInput>>,
    pub(crate) queue_edit_input_items: Option<Vec<luna_proto::ComposerInput>>,
    /// Rich window captures keyed exactly like drafts and ordinary staged
    /// attachments. Each owns one screenshot that joins the existing upload
    /// path only at send time.
    pub(crate) appshots: HashMap<String, Vec<CapturedAppshot>>,
    /// Stable identity for the blank canvas before a task exists.
    canvas_draft_id: String,
    draft_revisions: HashMap<String, u64>,
    draft_saved_revisions: HashMap<String, u64>,
    draft_blobs: HashMap<String, HashMap<String, DraftBlob>>,
    draft_unavailable: HashMap<String, Vec<DraftBlob>>,
    draft_unavailable_appshots: HashMap<String, HashSet<String>>,
    draft_unavailable_appshot_metadata: HashMap<String, Vec<DraftAppshot>>,
    draft_loaded: HashSet<String>,
    draft_save_tasks: HashMap<String, Task<()>>,
    draft_saves_in_flight: HashMap<String, u64>,
    draft_load_tasks: HashMap<String, Task<()>>,
    draft_blob_tasks: HashMap<(String, String), Task<()>>,
    draft_blobs_in_flight: HashSet<(String, String)>,
    draft_receipt_tasks: HashMap<String, Task<()>>,
    draft_submitting: HashMap<String, u64>,
    draft_close_flushing: bool,
    draft_close_failed: bool,
    suppress_next_draft_edit: bool,
    draft_comment_fingerprints: HashMap<String, u64>,
    recoverable_submissions: HashMap<String, Vec<DraftSubmission>>,
    draft_merge_on_load: HashSet<String>,
    pending_draft_owner_stage: Option<PendingDraftOwnerStage>,
    draft_engine_generation: Option<u64>,
    stash_entries: Vec<PromptStashEntry>,
    stash_menu_open: bool,
    stash_task: Option<Task<()>>,
    history_recall: Option<(String, String)>,
    history_recall_edit_pending: Option<String>,
    appshot_entrances: HashMap<String, Instant>,
    /// The staged attachment being viewed full-size (click a thumbnail).
    preview: Option<attachments::PreviewImage>,
    /// Focused while the lightbox is open so Escape reaches it; the input
    /// gets focus back on close.
    preview_focus: FocusHandle,
    /// Focus grab deferred to the next render (open sites don't all have a
    /// `Window` — the `LUNA_ATTACH_PREVIEW` boot knob opens in `new`).
    preview_focus_pending: bool,
    /// In-flight file-picker prompt (paperclip).
    picker_task: Option<Task<()>>,
    mention_task: Option<Task<()>>,
    mention: FileMentionState,
    integrations: integrations::IntegrationPicker,
    slash_task: Option<Task<()>>,
    slash: SlashState,
    /// Advertised commands per (harness, discovery scope): shown at once on
    /// reopen and revalidated by one `ListCommands` per popup open (CC-12);
    /// the engine re-discovers only after the provider's extension files change.
    slash_cache: HashMap<(HarnessId, String), Vec<SlashCommand>>,
    /// Slash-popup row scroll — the stack overflows into a wheel/keyboard-
    /// scrollable list once it outgrows the card.
    slash_scroll: gpui::ScrollHandle,
    /// File-mention popup row scroll (same treatment).
    mention_scroll: gpui::ScrollHandle,
    chat_reference: chat_references::ChatReferenceState,
    /// Shared scrollbar hover/drag state for both popups' floating rails —
    /// they never show at once (mutually exclusive by token shape).
    popup_bar: crate::popover::MenuScrollbarState,
    pub(crate) current_key: String,
    sending: bool,
    /// Armed immediately before a blank-canvas send selects its minted chat.
    /// The state observer consumes it to distinguish that handoff from normal
    /// session navigation, which must continue to snap.
    launching_new_chat: bool,
    pub(crate) failure: Option<SharedString>,
    /// The chat key `failure` belongs to (`None` = global, e.g. "Engine not
    /// connected"). Chat-scoped failures survive navigation and render only
    /// under their own chat — a blanket clear-on-switch erased the one
    /// visible trace of a failed send (2026-08-19).
    failure_key: Option<String>,
    /// The exact autosave warning; only a confirmed save of this draft clears it.
    draft_save_failure: Option<(String, SharedString)>,
    /// A concurrent draft merge is temporary while its merged copy is saving.
    draft_merge_notice: Option<(String, SharedString)>,
    /// Unconfirmed delivery is a warning until this command's receipt settles.
    delivery_failure: Option<(String, SharedString)>,
    /// The recovery control already explains this saved-copy state.
    recovery_notice: Option<(String, SharedString)>,
    wizard: Option<Wizard>,
    /// The last pending-question scan, keyed by binding key and the bound
    /// transcript's content token: unchanged history is not rescanned.
    pending_scan_cache: Option<((String, u64), Option<Wizard>)>,
    /// The compact controls tray (narrow composers) is open.
    controls_tray_open: bool,
    /// The press on More found the tray open (its outside-press already
    /// closed it), so that click must not reopen it.
    controls_tray_press_was_open: bool,
    /// A pane composer not painted recently skips the rescan; its next render
    /// catches up (a hidden tab's composer cannot be answered meanwhile).
    last_painted: Option<Instant>,
    pending_scan_deferred: bool,
    wizard_focus: FocusHandle,
    /// Requests already answered locally (suppresses the panel until the doc
    /// frame marks them resolved).
    answered_requests: HashSet<String>,
    /// Preserve the exact command id and payload across uncertain transport
    /// outcomes. Retry can ask the host about the same command without
    /// producing a second user steer.
    async_submissions: HashMap<(String, String), AsyncQuestionSubmission>,
    async_attempts: HashMap<(String, String), u32>,
    /// The exact banner set by one async question, so its later doc readback
    /// cannot erase an unrelated composer failure on the same chat.
    async_failure: Option<((String, String), SharedString)>,
    advance_task: Option<Task<()>>,
    send_task: Option<Task<()>>,
    /// The queued message being edited in the composer (see
    /// [`Composer::begin_queue_edit`]).
    pub(crate) editing_queued: Option<String>,
    /// Host-issued generation protecting `editing_queued` from automatic
    /// delivery. The text buffer is kept until Finish receives an ACK.
    pub(crate) queue_edit_lease_id: Option<String>,
    pub(crate) queue_edit_base_text_hash: Option<String>,
    pub(crate) queue_edit_revision: Option<u64>,
    pub(crate) queue_edit_command_id: Option<String>,
    pub(crate) queue_edit_chat_id: Option<String>,
    pub(crate) queue_edit_host_device_id: Option<String>,
    pub(crate) queue_edit_instance_id: String,
    pub(crate) queue_edit_pending_id: Option<String>,
    pub(crate) queue_edit_finishing: bool,
    pub(crate) queue_edit_task: Option<Task<()>>,
    pub(crate) queue_edit_renew_task: Option<Task<()>>,
    /// Focus once on mount, navigation, or after opening/closing a queue edit.
    pub(crate) focus_pending: bool,
    /// Live drag over the queue panel: which row, and where it would land.
    pub(crate) queue_drag: Option<crate::queue::QueueDragState>,
    pub(crate) queue_scroll: gpui::ScrollHandle,
    pub(crate) queue_full_preview: Option<Task<()>>,
    pub(crate) queue_previews: HashMap<(String, String), crate::queue::QueuePreview>,
    /// Rows awaiting a host-authoritative removal acknowledgement. They stay
    /// visible but inert until the host wins the race against queue delivery.
    pub(crate) queue_removing: HashSet<String>,
    /// Interrupt/answer commands get their own slot: assigning `send_task`
    /// DROPPED an in-flight send future mid-upload — no banner, no cleanup,
    /// `sending` stuck true forever (2026-08-19 incident, "press Stop while
    /// a send grinds" shape).
    action_task: Option<Task<()>>,
    /// Chats whose durable Interrupt command has been accepted or is still
    /// being queued. Kept independently so stopping one chat cannot replace
    /// another chat's request when the user navigates quickly.
    interrupting: HashSet<String>,
    /// The Work popover (slice 3), while open, and its in-flight request.
    work_menu: Option<work::WorkMenu>,
    work_task: Option<Task<()>>,
    interrupt_tasks: HashMap<String, Task<()>>,
    /// QA: the outcome of each chat's latest Stop — the engine's receipt, or
    /// the error the user saw.
    #[cfg(feature = "appshots-fixture")]
    pub(crate) last_stop: HashMap<String, Result<luna_proto::CommandReceipt, String>>,
    /// Stable outer composer width supplied by the shell. Unlike Taffy's
    /// provisional input measurements, this changes only when the actual
    /// conversation column changes and can safely drive a follow-up render.
    last_available_width: Option<f32>,
    /// The workbench frame's bottom row names the session's checkout and
    /// branch, so the session footer leaves them out (drafts keep theirs).
    frame_footer: bool,
    /// The controls row's last measured fit (fixture/diagnostic readback).
    controls_row: Option<ControlsRowMeasure>,
    /// In-flight compact↔expanded morph (one per committed flip; manual
    /// drive — see [`FlipMorph`]).
    flip_morph: Option<FlipMorph>,
    /// Pill height actually rendered last frame — a committed flip morphs
    /// from here, so mid-flight reversals hand off without a jump.
    last_rendered_height: f32,
    dock_frame: Option<crate::composer_dock::DockFrame>,
    /// The shared clock owns this frame's height, including its final step.
    dock_height_changed: bool,
    dock_clearance_correction: f32,
    surface_bounds: crate::new_thread_background_mask::SurfaceBounds,
    last_target_height: f32,
    height_morph: Option<FlipMorph>,
    /// Monotonic clock anchor for the morph timeline.
    morph_clock: Instant,
    /// Set on every session/route change: flips committed before this instant
    /// SNAP instead of morphing (see [`ROUTE_SNAP_MS`]).
    route_snap_until: Option<Instant>,
    _observe: Subscription,
    _pickers_observe: Subscription,
    _picker_focus: Subscription,
    _input_events: Subscription,
    _answer_input_events: Subscription,
}

struct RestoredComposerDraft {
    record: ComposerDraft,
    ordinary: Vec<StagedAttachment>,
    appshots: Vec<CapturedAppshot>,
    unavailable: Vec<DraftBlob>,
    unavailable_appshots: HashSet<String>,
    unavailable_appshot_metadata: Vec<DraftAppshot>,
    warning: Option<String>,
    recovery_action: Option<String>,
    pending_submission: bool,
    pending_same_revision: bool,
    recoverable_submission: bool,
    recoverable_separate: bool,
    recoverable_entries: Vec<DraftSubmission>,
    pending_entries: Vec<DraftSubmission>,
}

enum ComposerSendOutcome {
    Applied,
    Pending,
    Queued(String),
}

enum ComposerSendFailure {
    Rejected(String),
    Unknown(String),
}

fn receipt_failure_message(receipt: &CommandReceipt, fallback: &str) -> String {
    crate::task_commands::failure_message(receipt, fallback)
}

// A rejected first Run may have already created a checkout and a durable setup
// receipt. The task is the only native route to inspect/retry that receipt.
async fn retain_failed_worktree_setup(engine: &crate::state::EngineHandle, task_id: &str) -> bool {
    match engine
        .client()
        .call_as::<ProjectCommandsSnapshot>(
            methods::PROJECT_COMMAND_STATUS,
            serde_json::json!({ "chatId": task_id }),
        )
        .await
    {
        Ok(snapshot) => snapshot.runs.iter().any(|run| {
            run.setup
                && run.chat_id == task_id
                && run.cwd == snapshot.cwd
                && run.action.wait_for_setup
                && run.status != ProjectCommandStatus::Succeeded
        }),
        Err(error) => {
            // A lost status read cannot prove the worktree was never created.
            tracing::warn!(task_id, %error, "retaining rejected new task until setup status is readable");
            true
        }
    }
}

fn submission_has_newer_draft(
    submitted_revision: u64,
    current_revision: u64,
    current_text: &str,
) -> bool {
    current_revision > submitted_revision || !current_text.trim().is_empty()
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub(crate) fn set_dock_frame(
        &mut self,
        frame: crate::composer_dock::DockFrame,
        cx: &mut Context<Self>,
    ) {
        let changed = self.dock_frame != Some(frame);
        self.dock_height_changed |= self
            .dock_frame
            .is_none_or(|previous| previous.amount != frame.amount);
        self.dock_frame = Some(frame);
        if frame.active {
            self.flip_morph = None;
            self.height_morph = None;
        }
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn dock_clearance_correction(&self) -> f32 {
        self.dock_clearance_correction
    }

    pub(crate) fn surface_bounds(&self) -> crate::new_thread_background_mask::SurfaceBounds {
        self.surface_bounds.clone()
    }

    /// The picker entity, for the shell's canvas target selectors.
    pub fn pickers(&self) -> &Entity<Pickers> {
        &self.pickers
    }

    /// Feed the stable conversation-column width into responsive composer
    /// controls.
    /// See [`Self::frame_footer`]: the workbench frame owns the session's
    /// checkout/branch labels while this composer renders inside it.
    pub fn set_frame_footer(&mut self, frame_footer: bool, cx: &mut Context<Self>) {
        if self.frame_footer != frame_footer {
            self.frame_footer = frame_footer;
            cx.notify();
        }
    }

    pub fn set_available_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let composer_width = width.clamp(0.0, COMPOSER_MAX_WIDTH);
        if composer_width_changed(self.last_available_width, composer_width) {
            self.last_available_width = Some(composer_width);
            // The shell renders before this child, so this queues one more
            // pass after the input has been laid out at its final width. That
            // pass can consume the completed measurement without emitting an
            // event from inside Taffy's multi-pass measurement callback.
            cx.notify();
        }
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        Self::new_bound(state, crate::chat_binding::ChatBinding::Selected, cx)
    }

    /// A composer bound to `binding`. The main view uses `Selected`; the
    /// right pane's shared chat view binds one fixed chat.
    pub fn new_bound(
        state: Entity<AppState>,
        binding: crate::chat_binding::ChatBinding,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|this, cx| this.release_queue_previews(cx))
            .detach();
        let input = cx.new(|cx| {
            let mut input =
                ComposerInput::with_context("Do anything…", MESSAGE_COMPOSER_CONTEXT, cx);
            input.enable_mentions();
            input
        });
        let answer_input = cx.new(|cx| {
            ComposerInput::with_context(
                "Type your own answer, or pick an option above",
                MESSAGE_COMPOSER_CONTEXT,
                cx,
            )
        });
        let pickers = cx.new(|cx| Pickers::new_bound(state.clone(), binding.clone(), cx));
        // A context transfer preview sizes its budget with the draft (never
        // sends it).
        let draft_input = input.downgrade();
        pickers.update(cx, |pickers, _| pickers.set_draft_input(draft_input));
        // The footer toolbar (checkout kind + ref picker) is rendered INLINE
        // by the composer from picker state — a pickers-side notify (refs
        // loaded, popover toggled, pick made) must repaint the composer too.
        let pickers_observe = cx.observe(&pickers, |this: &mut Self, _, cx| {
            let config = this.pickers.read(cx).draft().clone();
            if this.last_canvas_draft_config != config {
                this.last_canvas_draft_config = config;
                if this.editing_queued.is_none()
                    && this.current_key.is_empty()
                    && !this.draft_submitting.contains_key(&this.current_key)
                {
                    this.mark_draft_dirty(cx);
                }
            }
            this.ensure_native_catalog(true, cx);
            cx.notify();
        });
        let picker_focus = cx.subscribe(
            &pickers,
            |this: &mut Self, _, _: &crate::pickers::ReturnComposerFocus, cx| {
                this.focus_pending = true;
                cx.notify();
            },
        );
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.on_state_changed(cx));
        let input_events = cx.subscribe(&input, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Submitted => this.on_submit(cx),
            ComposerInputEvent::ModifierSubmitted => this.on_modifier_submit(cx),
            ComposerInputEvent::OppositeSubmitted => this.on_opposite_submit(cx),
            ComposerInputEvent::Edited => {
                // set_text emits Edited after a history replacement. Keep the
                // browsing state for that one programmatic edit; a later user
                // edit must end recall as usual.
                let history_edit = this.history_recall_edit_pending.take();
                let from_history = history_edit
                    .as_deref()
                    .is_some_and(|expected| this.input.read(cx).text() == expected);
                this.on_input_edited(cx);
                if !from_history {
                    this.clear_history_recall(cx);
                }
                if !std::mem::take(&mut this.suppress_next_draft_edit) {
                    this.mark_draft_dirty(cx);
                }
            }
            ComposerInputEvent::CursorMoved => {
                this.clear_history_recall(cx);
                this.on_input_edited(cx);
            }
            ComposerInputEvent::ViewportChanged => cx.notify(),
            ComposerInputEvent::StashPrompt => this.stash_current_prompt(cx),
            ComposerInputEvent::HistoryNavigate(delta) => this.navigate_sent_prompts(*delta, cx),
            // The slash popup and the mention popup share the input's
            // completion key routing; they are mutually exclusive by token
            // shape (`/` at offset 0 vs `@` at a token boundary).
            ComposerInputEvent::MentionNavigate(delta) => {
                if this.slash.token.is_some() {
                    this.move_slash(*delta, cx)
                } else if this.chat_reference.token.is_some() {
                    this.move_chat_reference(*delta, cx)
                } else {
                    this.move_mention(*delta, cx)
                }
            }
            ComposerInputEvent::MentionAccept => {
                if this.slash.token.is_some() {
                    this.accept_slash(cx)
                } else if this.chat_reference.token.is_some() {
                    this.accept_chat_reference(cx)
                } else {
                    this.accept_mention(cx)
                }
            }
            ComposerInputEvent::MentionDismiss => {
                if this.slash.token.is_some() {
                    this.dismiss_slash(cx)
                } else if this.chat_reference.token.is_some() {
                    this.dismiss_chat_references(cx)
                } else {
                    this.dismiss_mention(cx)
                }
            }
            ComposerInputEvent::PastedImages(images) => {
                let staged = images
                    .iter()
                    .map(|image| attachments::stage_clipboard_image(image.clone()))
                    .collect();
                this.add_staged(staged, cx);
            }
            ComposerInputEvent::PastedPaths(paths) => this.add_paths(paths.clone(), cx),
        });
        let answer_input_events =
            cx.subscribe(&answer_input, |this: &mut Self, _, event, cx| match event {
                ComposerInputEvent::Submitted
                | ComposerInputEvent::ModifierSubmitted
                | ComposerInputEvent::OppositeSubmitted => this.wizard_advance(cx),
                ComposerInputEvent::Edited | ComposerInputEvent::ViewportChanged => cx.notify(),
                _ => {}
            });
        let current_key = binding.key(state.read(cx));
        let mut composer = Self {
            goal_ui: goals::GoalUi::default(),
            state,
            binding,
            input,
            answer_input,
            queue_edit_draft: None,
            last_canvas_draft_config: pickers.read(cx).draft().clone(),
            pickers,
            drafts: HashMap::new(),
            attachments: HashMap::new(),
            input_items: HashMap::new(),
            queue_edit_input_items: None,
            appshots: HashMap::new(),
            canvas_draft_id: uuid::Uuid::new_v4().to_string(),
            draft_revisions: HashMap::new(),
            draft_saved_revisions: HashMap::new(),
            draft_blobs: HashMap::new(),
            draft_unavailable: HashMap::new(),
            draft_unavailable_appshots: HashMap::new(),
            draft_unavailable_appshot_metadata: HashMap::new(),
            draft_loaded: HashSet::new(),
            draft_save_tasks: HashMap::new(),
            draft_saves_in_flight: HashMap::new(),
            draft_load_tasks: HashMap::new(),
            draft_blob_tasks: HashMap::new(),
            draft_blobs_in_flight: HashSet::new(),
            draft_receipt_tasks: HashMap::new(),
            draft_submitting: HashMap::new(),
            draft_close_flushing: false,
            draft_close_failed: false,
            suppress_next_draft_edit: false,
            draft_comment_fingerprints: HashMap::new(),
            recoverable_submissions: HashMap::new(),
            draft_merge_on_load: HashSet::new(),
            pending_draft_owner_stage: None,
            draft_engine_generation: None,
            stash_entries: Vec::new(),
            stash_menu_open: false,
            stash_task: None,
            history_recall: None,
            history_recall_edit_pending: None,
            appshot_entrances: HashMap::new(),
            preview: None,
            preview_focus: cx.focus_handle(),
            preview_focus_pending: false,
            picker_task: None,
            mention_task: None,
            mention: FileMentionState::default(),
            integrations: integrations::IntegrationPicker::default(),
            slash_task: None,
            slash: SlashState::default(),
            slash_cache: HashMap::new(),
            slash_scroll: gpui::ScrollHandle::new(),
            mention_scroll: gpui::ScrollHandle::new(),
            chat_reference: Default::default(),
            popup_bar: crate::popover::MenuScrollbarState::default(),
            current_key,
            sending: false,
            launching_new_chat: false,
            failure: None,
            wizard: None,
            pending_scan_cache: None,
            controls_tray_open: false,
            controls_tray_press_was_open: false,
            last_painted: None,
            pending_scan_deferred: false,
            wizard_focus: cx.focus_handle(),
            answered_requests: HashSet::new(),
            async_submissions: HashMap::new(),
            async_attempts: HashMap::new(),
            async_failure: None,
            failure_key: None,
            draft_save_failure: None,
            draft_merge_notice: None,
            delivery_failure: None,
            recovery_notice: None,
            action_task: None,
            advance_task: None,
            send_task: None,
            interrupting: HashSet::new(),
            work_menu: None,
            work_task: None,
            interrupt_tasks: HashMap::new(),
            #[cfg(feature = "appshots-fixture")]
            last_stop: HashMap::new(),
            editing_queued: None,
            queue_edit_lease_id: None,
            queue_edit_base_text_hash: None,
            queue_edit_revision: None,
            queue_edit_command_id: None,
            queue_edit_chat_id: None,
            queue_edit_host_device_id: None,
            queue_edit_instance_id: uuid::Uuid::new_v4().to_string(),
            queue_edit_pending_id: None,
            queue_edit_finishing: false,
            queue_edit_task: None,
            queue_edit_renew_task: None,
            focus_pending: true,
            queue_drag: None,
            queue_scroll: gpui::ScrollHandle::new(),
            queue_full_preview: None,
            queue_previews: HashMap::new(),
            queue_removing: HashSet::new(),
            last_available_width: None,
            frame_footer: false,
            controls_row: None,
            flip_morph: None,
            last_rendered_height: 0.0,
            dock_frame: None,
            dock_height_changed: false,
            dock_clearance_correction: 0.0,
            surface_bounds: Default::default(),
            last_target_height: 0.0,
            height_morph: None,
            morph_clock: Instant::now(),
            route_snap_until: None,
            _observe: observe,
            _pickers_observe: pickers_observe,
            _picker_focus: picker_focus,
            _input_events: input_events,
            _answer_input_events: answer_input_events,
        };
        // Dev knob: pre-stage attachments (drop/paste can't be synthesized on
        // a rig) — `LUNA_ATTACH=/path/a.png[,/path/b.png]`, and
        // `LUNA_ATTACH_PREVIEW=1` boots with the first one's lightbox open.
        if let Ok(spec) = std::env::var("LUNA_ATTACH") {
            let staged: Vec<StagedAttachment> = spec
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .filter_map(|path| {
                    match attachments::stage_file(std::path::Path::new(path.trim())) {
                        Ok(att) => Some(att),
                        Err(err) => {
                            tracing::warn!(%path, error = %err, "LUNA_ATTACH stage failed");
                            None
                        }
                    }
                })
                .collect();
            if std::env::var("LUNA_ATTACH_PREVIEW").is_ok_and(|v| v == "1")
                && let Some(first) = staged.first()
            {
                composer.preview = Some(attachments::PreviewImage::new(
                    first.name.clone(),
                    first.image.clone(),
                ));
                composer.preview_focus_pending = true;
            }
            if !staged.is_empty() {
                composer
                    .attachments
                    .entry(composer.current_key.clone())
                    .or_default()
                    .extend(staged);
            }
        }
        composer
    }

    /// Capture-knob passthrough (`LUNA_OPEN_DIALOG=model`): open the
    /// combined harness/model menu.
    pub fn debug_open_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pickers
            .update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
    }

    pub fn is_sending(&self) -> bool {
        self.sending
    }

    pub(crate) fn can_edit_queue_in_composer(&self) -> bool {
        !self.sending
    }

    fn draft_id_for(&self, key: &str) -> String {
        if key.is_empty() {
            self.canvas_draft_id.clone()
        } else {
            key.to_string()
        }
    }

    /// Move focus into this composer's input on its next render (a pane
    /// view reopening a cached composer).
    pub(crate) fn request_focus(&mut self, cx: &mut Context<Self>) {
        self.focus_pending = true;
        cx.notify();
    }

    /// A send, receipt check or Stop still owned by this composer. Dropping
    /// the composer would cancel it.
    pub(crate) fn has_work_in_flight(&self) -> bool {
        self.sending
            || !self.draft_submitting.is_empty()
            || !self.draft_receipt_tasks.is_empty()
            || !self.interrupt_tasks.is_empty()
            || self.goal_ui.has_work_in_flight()
            || self
                .action_task
                .as_ref()
                .is_some_and(|task| !task.is_ready())
            || self.queue_edit_pending_id.is_some()
            || self
                .queue_edit_task
                .as_ref()
                .is_some_and(|task| !task.is_ready())
    }

    /// Drop this composer's in-memory copy of `key`'s draft so the next
    /// navigation reloads the durable record. Used when another composer (a
    /// pane composer handing its chat to the main view) saved newer text.
    /// Refused while that draft has unsaved or submitting state here.
    pub(crate) fn forget_cached_draft(&mut self, key: &str) -> bool {
        if key.is_empty()
            || key == self.current_key
            || !self.draft_is_saved(key)
            || self.draft_submitting.contains_key(key)
            || self.draft_load_tasks.contains_key(key)
        {
            return false;
        }
        self.drafts.remove(key);
        self.draft_loaded.remove(key);
        true
    }

    /// Text-level readback for headless fixtures and diagnostics.
    pub(crate) fn readback(&self, cx: &App) -> serde_json::Value {
        let state = self.state.read(cx);
        serde_json::json!({
            "binding": self.binding.pane_chat(),
            "boundChat": self.binding.chat_id(state),
            "currentKey": self.current_key,
            "input": self.input.read(cx).text().to_string(),
            "sending": self.sending,
            "failure": self.failure.as_ref().map(|failure| failure.to_string()),
            "failureKey": self.failure_key,
            "deliveryFailure": self.delivery_failure.as_ref().map(|(id, text)| serde_json::json!({"commandId": id, "text": text.to_string()})),
            "submitting": self.draft_submitting.keys().cloned().collect::<Vec<_>>(),
            "receiptWatches": self.draft_receipt_tasks.keys().cloned().collect::<Vec<_>>(),
            "wizard": self.wizard.as_ref().map(|wizard| wizard.request_id.clone()),
            "queue": self.binding.queue(state).iter().map(|item| item.id.clone()).collect::<Vec<_>>(),
            "goal": self.binding.context_control(state).goal.as_ref().and_then(|goal| goal.goal.as_ref()).map(|goal| goal.objective.clone()),
            "contextUsage": self.binding.context_usage(state).map(|usage| serde_json::json!({"tokens": usage.tokens, "window": usage.window})),
            "draftLoaded": self.draft_loaded.contains(&self.current_key),
            "surface": self.surface_bounds.get().map(|bounds| serde_json::json!({
                "x": f32::from(bounds.origin.x),
                "y": f32::from(bounds.origin.y),
                "width": f32::from(bounds.size.width),
                "height": f32::from(bounds.size.height),
            })),
            "chips": self.pickers.read(cx).chip_readback(self.pickers.entity_id(), cx),
            "work": self.work_readback(cx),
            "controlsRow": self.controls_row.map(|row| serde_json::json!({
                "budget": row.budget,
                "modelFull": row.model_full,
                "modelNameOnly": row.model_name_only,
                "goal": row.goal.map(|(label, icon)| [label, icon]),
                "usage": row.usage.map(|(label, icon)| [label, icon]),
                "modelFit": format!("{:?}", row.fit.model),
                "goalIconOnly": row.fit.goal_icon_only,
                "usageIconOnly": row.fit.usage_icon_only,
            })),
        })
    }

    /// Explicit result handoff only stages the current parent's durable draft.
    /// It never queues a command, changes attachments, or submits a provider turn.
    pub(crate) fn insert_side_chat_result(
        &mut self,
        parent: &str,
        result: &str,
        engine_generation: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.current_key != parent
            || self.binding.chat_id(self.state.read(cx)) != Some(parent)
            || self.state.read(cx).engine_generation() != engine_generation
            || !self.draft_loaded.contains(parent)
            || self.editing_queued.is_some()
            || self.pending_draft_owner_stage.is_some()
            || self.is_sending()
        {
            return false;
        }
        let existing = self.input.read(cx).text();
        let text = if existing.trim().is_empty() {
            result.to_string()
        } else {
            format!("{existing}\n\n{result}")
        };
        // Appending keeps existing chips; the inserted result stays plain text.
        self.input.update(cx, |input, cx| {
            input.set_text_keeping_native_prefix(text, cx)
        });
        self.focus_pending = true;
        true
    }

    pub(crate) fn set_input_text_without_draft_edit(
        &mut self,
        text: String,
        cx: &mut Context<Self>,
    ) {
        self.suppress_next_draft_edit = true;
        self.input.update(cx, |input, cx| input.set_text(text, cx));
    }

    fn clear_history_recall(&mut self, cx: &mut Context<Self>) {
        if self.history_recall.take().is_some() {
            self.input
                .update(cx, |input, _| input.history_navigation_active = false);
        }
    }

    /// Up starts from an empty editor; while browsing, Up/Down move among
    /// sent user prompts. An ordinary edit or caret move ends browsing, so
    /// multiline navigation and an in-progress draft are left alone.
    fn navigate_sent_prompts(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.current_key.is_empty()
            || self.editing_queued.is_some()
            || self.queue_edit_draft.is_some()
        {
            return;
        }
        let current = self.input.read(cx).text().to_string();
        if self
            .attachments
            .get(&self.current_key)
            .is_some_and(|a| !a.is_empty())
            || self
                .appshots
                .get(&self.current_key)
                .is_some_and(|a| !a.is_empty())
            || self
                .draft_unavailable
                .get(&self.current_key)
                .is_some_and(|a| !a.is_empty())
            || !self
                .state
                .read(cx)
                .review_comments(&self.current_key)
                .is_empty()
            || (self.history_recall.is_none() && !current.is_empty())
        {
            return;
        }
        let mut entries: Vec<(String, String)> = Vec::new();
        for message in self.binding.transcript(self.state.read(cx)) {
            if message.role != MessageRole::User {
                continue;
            }
            let raw = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            let visible = attachments::parse_user_message_images(&raw).text;
            let visible = appshots::strip_context_for_display(&visible);
            let (visible, _) = crate::badges::split(visible);
            let prompt = visible.trim().to_string();
            if prompt.is_empty() || prompt == attachments::ATTACHMENT_ONLY_TEXT {
                continue;
            }
            if entries
                .last()
                .is_some_and(|(_, previous)| previous == &prompt)
            {
                entries.pop();
            }
            entries.push((message.id.clone(), prompt));
        }
        let active = self.history_recall.as_ref().and_then(|(id, recalled)| {
            (recalled == &current)
                .then(|| entries.iter().position(|(entry_id, _)| entry_id == id))
                .flatten()
        });
        let next = if delta < 0 {
            let index = active.unwrap_or(entries.len());
            index
                .checked_sub(1)
                .and_then(|index| entries.get(index).cloned())
        } else if let Some(index) = active {
            entries.get(index + 1).cloned()
        } else {
            return;
        };
        if delta < 0 && next.is_none() {
            return;
        }
        let text = next
            .as_ref()
            .map(|(_, prompt)| prompt.clone())
            .unwrap_or_default();
        self.history_recall_edit_pending = Some(text.clone());
        self.input.update(cx, |input, cx| input.set_text(text, cx));
        self.history_recall = next;
        let active = self.history_recall.is_some();
        self.input.update(cx, |input, _| {
            input.history_navigation_active = active;
        });
    }

    fn stash_failure(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.failure = Some(message.into());
        self.failure_key = Some(self.current_key.clone());
        cx.notify();
    }

    /// JOINT-INTEGRATION POINT `trusted-envelope-restore`
    /// (native-inputs -> integrations-picker).
    ///
    /// Called only by trusted persistence restores (draft load and merge,
    /// saved prompt, queue-edit start and end, displaced private draft,
    /// failed send, recoverable submission) right after `key`'s exact text and
    /// typed `input_items` were assembled under an established owner (engine
    /// generation + draft key). Typed identities are written before the text.
    ///
    /// For the visible draft this hydrates the editor's owner-bound anchors
    /// from the assembled envelope (`hydrate_native_input_envelope`). A
    /// background key needs nothing now: opening that draft hydrates its
    /// `input_items` on navigation. Raw or pasted text must never reach this
    /// hook or mint typed identity.
    pub(crate) fn trusted_native_envelope_restored(&mut self, key: &str, cx: &mut Context<Self>) {
        if key == self.current_key {
            self.hydrate_native_input_envelope(cx);
        }
    }

    fn current_prompt_is_empty(&self, cx: &App) -> bool {
        self.input.read(cx).text().is_empty()
            && self
                .input_items
                .get(&self.current_key)
                .is_none_or(Vec::is_empty)
            && self
                .attachments
                .get(&self.current_key)
                .is_none_or(Vec::is_empty)
            && self
                .appshots
                .get(&self.current_key)
                .is_none_or(Vec::is_empty)
            && self
                .draft_unavailable
                .get(&self.current_key)
                .is_none_or(Vec::is_empty)
            && self
                .draft_unavailable_appshot_metadata
                .get(&self.current_key)
                .is_none_or(Vec::is_empty)
            && self
                .state
                .read(cx)
                .review_comments(&self.current_key)
                .is_empty()
    }

    fn load_stash(&mut self, cx: &mut Context<Self>) {
        if self.stash_task.is_some() {
            return;
        }
        let (engine, generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(engine) = engine else {
            self.stash_failure("Saved prompts need the local engine.", cx);
            return;
        };
        self.stash_task = Some(cx.spawn(async move |this, cx| {
            let result = crate::drafts::list_stash(&engine).await;
            this.update(cx, |composer, cx| {
                if composer.state.read(cx).engine_generation() != generation {
                    return;
                }
                composer.stash_task = None;
                match result {
                    Ok(entries) => composer.stash_entries = entries,
                    Err(error) => composer
                        .stash_failure(format!("Saved prompts could not be loaded: {error}"), cx),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn toggle_stash_menu(&mut self, cx: &mut Context<Self>) {
        self.stash_menu_open = !self.stash_menu_open;
        if self.stash_menu_open {
            self.load_stash(cx);
        }
        cx.notify();
    }

    /// The app-wide Save File shortcut reaches the shell after keymap
    /// registration. Route it here only when this message input owns focus;
    /// file editors and picker searches retain their own Save behavior.
    pub(crate) fn stash_if_message_input_focused(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.input.read(cx).focus_handle.is_focused(window) {
            return false;
        }
        self.stash_current_prompt(cx);
        true
    }

    fn stash_current_prompt(&mut self, cx: &mut Context<Self>) {
        if self.stash_task.is_some() {
            return;
        }
        let staged = self.input_items.get(&self.current_key).map_or(0, Vec::len)
            + self.attachments.get(&self.current_key).map_or(0, Vec::len)
            + self.appshots.get(&self.current_key).map_or(0, Vec::len);
        let comments = self.state.read(cx).review_comments(&self.current_key).len();
        if !composer_has_content(self.input.read(cx).text(), staged, comments) {
            self.toggle_stash_menu(cx);
            return;
        }
        let key = self.current_key.clone();
        if !self.draft_loaded.contains(&key)
            || self.editing_queued.is_some()
            || self.queue_edit_draft.is_some()
            || self.pending_draft_owner_stage.is_some()
            || self.is_sending()
        {
            self.stash_failure(
                "Wait for this draft to finish loading or sending before saving it.",
                cx,
            );
            return;
        }
        let snapshot = self.build_draft_record(&key, self.input.read(cx).text().to_string(), cx);
        if snapshot
            .attachments
            .iter()
            .any(|blob| blob.state != DraftBlobState::Saved || blob.path.is_none())
            || self
                .draft_blobs_in_flight
                .iter()
                .any(|(owner, _)| owner == &key)
        {
            self.stash_failure(
                "Wait for attachments to finish saving or reattach missing files.",
                cx,
            );
            return;
        }
        let label = snapshot
            .text
            .split_whitespace()
            .take(12)
            .collect::<Vec<_>>()
            .join(" ");
        let label = if label.is_empty() {
            if snapshot.attachments.is_empty() {
                "Review comment".to_string()
            } else {
                format!(
                    "{} attachment{}",
                    snapshot.attachments.len(),
                    if snapshot.attachments.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                )
            }
        } else {
            label
        };
        let label = label.chars().take(120).collect();
        let entry = PromptStashEntry {
            input_items: snapshot.input_items,
            id: uuid::Uuid::new_v4().to_string(),
            label,
            text: snapshot.text,
            attachments: snapshot.attachments,
            appshots: snapshot.appshots,
            context: snapshot.context,
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        let source_revision = snapshot.revision;
        let (engine, generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(engine) = engine else {
            self.stash_failure("Saved prompts need the local engine.", cx);
            return;
        };
        self.stash_task = Some(cx.spawn(async move |this, cx| {
            let result = crate::drafts::save_stash(&engine, &entry).await;
            let readback = if result.is_ok() {
                crate::drafts::list_stash(&engine).await
            } else {
                result.map(|_| Vec::new())
            };
            this.update(cx, |composer, cx| {
                if composer.state.read(cx).engine_generation() != generation {
                    return;
                }
                composer.stash_task = None;
                match readback {
                    Ok(entries) if entries.iter().any(|saved| saved == &entry) => {
                        composer.stash_entries = entries;
                        composer.stash_menu_open = false;
                        if composer.current_key == key
                            && composer.draft_revisions.get(&key).copied().unwrap_or(0)
                                == source_revision
                        {
                            composer.attachments.remove(&key);
                            composer.input_items.remove(&key);
                            composer.appshots.remove(&key);
                            composer.draft_blobs.remove(&key);
                            composer.draft_unavailable.remove(&key);
                            composer.draft_unavailable_appshots.remove(&key);
                            composer.draft_unavailable_appshot_metadata.remove(&key);
                            composer.draft_comment_fingerprints.remove(&key);
                            composer.state.update(cx, |state, _| state.purge_review_comments(&key));
                            composer.drafts.insert(key.clone(), String::new());
                            composer.set_input_text_without_draft_edit(String::new(), cx);
                            composer.mark_draft_dirty(cx);
                        } else {
                            composer.stash_failure(
                                "Prompt saved. The draft changed while saving, so it was kept in the editor.",
                                cx,
                            );
                        }
                    }
                    Ok(_) => composer.stash_failure(
                        "Saved prompt was not confirmed; the draft was kept in the editor.",
                        cx,
                    ),
                    Err(error) => composer.stash_failure(
                        format!("Prompt could not be saved; the draft was kept: {error}"),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn delete_stashed_prompt(&mut self, id: String, cx: &mut Context<Self>) {
        if self.stash_task.is_some() {
            return;
        }
        let (engine, generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(engine) = engine else {
            self.stash_failure("Saved prompts need the local engine.", cx);
            return;
        };
        self.stash_task = Some(cx.spawn(async move |this, cx| {
            let result = crate::drafts::delete_stash(&engine, &id).await;
            let entries = if result.is_ok() {
                crate::drafts::list_stash(&engine).await
            } else {
                result.map(|_| Vec::new())
            };
            this.update(cx, |composer, cx| {
                if composer.state.read(cx).engine_generation() != generation {
                    return;
                }
                composer.stash_task = None;
                match entries {
                    Ok(entries) => composer.stash_entries = entries,
                    Err(error) => composer
                        .stash_failure(format!("Saved prompt could not be deleted: {error}"), cx),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn restore_stashed_prompt(&mut self, entry: PromptStashEntry, cx: &mut Context<Self>) {
        let key = self.current_key.clone();
        let revision = self.draft_revisions.get(&key).copied().unwrap_or(0);
        if self.stash_task.is_some()
            || !self.draft_loaded.contains(&key)
            || !self.current_prompt_is_empty(cx)
            || self.editing_queued.is_some()
            || self.queue_edit_draft.is_some()
            || self.pending_draft_owner_stage.is_some()
            || self.is_sending()
            || self.draft_saves_in_flight.contains_key(&key)
            || self.draft_saved_revisions.get(&key).copied().unwrap_or(0) < revision
        {
            self.stash_failure("Restore needs an empty, fully saved composer. Save this prompt first or wait for draft saving.", cx);
            return;
        }
        let (engine, generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(engine) = engine else {
            self.stash_failure("Saved prompts need the local engine.", cx);
            return;
        };
        self.stash_task = Some(cx.spawn(async move |this, cx| {
            let mut loaded = HashMap::new();
            let mut load_error = None;
            for blob in &entry.attachments {
                if blob.state != DraftBlobState::Saved {
                    load_error = Some(format!("{} needs reattachment; the saved prompt was kept.", blob.name));
                    break;
                }
                let Some(path) = blob.path.as_deref() else {
                    load_error = Some(format!("{} has no saved path; the prompt was kept.", blob.name));
                    break;
                };
                match attachments::read_attachment_image(
                    &engine,
                    cx.background_executor(),
                    None,
                    path,
                    None,
                )
                .await
                {
                    Some(image) => {
                        loaded.insert(blob.attachment_id.clone(), StagedAttachment {
                            id: blob.attachment_id.clone(),
                            name: blob.name.clone(),
                            image: image.image,
                        });
                    }
                    None => {
                        load_error = Some(format!("{} could not be read; the saved prompt was kept.", blob.name));
                        break;
                    }
                }
            }
            if let Some(error) = load_error {
                this.update(cx, |composer, cx| {
                    if composer.state.read(cx).engine_generation() == generation {
                        composer.stash_task = None;
                        composer.stash_failure(error, cx);
                    }
                }).ok();
                return;
            }
            let appshot_attachment_ids: HashSet<&str> = entry.appshots.iter()
                .map(|shot| shot.screenshot_attachment_id.as_str()).collect();
            let ordinary = entry.attachments.iter()
                .filter(|blob| !appshot_attachment_ids.contains(blob.attachment_id.as_str()))
                .filter_map(|blob| loaded.get(&blob.attachment_id).cloned())
                .collect::<Vec<_>>();
            let shots = entry.appshots.iter().filter_map(|shot| {
                Some(CapturedAppshot {
                    id: shot.id.clone(),
                    app_name: shot.app_name.clone(),
                    bundle_identifier: shot.bundle_identifier.clone(),
                    window_title: shot.window_title.clone(),
                    accessibility: appshots::AccessibilitySnapshot {
                        format_version: shot.accessibility_format_version,
                        content: shot.accessibility_content.clone(),
                        truncated: shot.accessibility_truncated,
                    },
                    screenshot: loaded.get(&shot.screenshot_attachment_id)?.clone(),
                    screenshot_dimensions: shot.screenshot_dimensions,
                    app_icon: None,
                    captured_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(shot.captured_at_ms)
                        .unwrap_or_else(chrono::Utc::now),
                })
            }).collect::<Vec<_>>();
            if shots.len() != entry.appshots.len() {
                this.update(cx, |composer, cx| {
                    if composer.state.read(cx).engine_generation() == generation {
                        composer.stash_task = None;
                        composer.stash_failure("A saved window capture is incomplete; the prompt was kept.", cx);
                    }
                }).ok();
                return;
            }
            let record = this.update(cx, |composer, cx| {
                if composer.state.read(cx).engine_generation() != generation {
                    return None;
                }
                if composer.current_key != key
                    || composer.draft_revisions.get(&key).copied().unwrap_or(0) != revision
                    || !composer.current_prompt_is_empty(cx)
                {
                    composer.stash_task = None;
                    return None;
                }
                composer.input_items.insert(key.clone(), entry.input_items.clone());
                composer.attachments.insert(key.clone(), ordinary);
                composer.appshots.insert(key.clone(), shots);
                composer.draft_blobs.insert(key.clone(), entry.attachments.iter()
                    .cloned().map(|blob| (blob.attachment_id.clone(), blob)).collect());
                composer.state.update(cx, |state, _| {
                    for value in &entry.context.review_comments {
                        if let Ok(comment) = serde_json::from_value::<crate::comments::ReviewComment>(value.clone()) {
                            state.add_review_comment(&key, comment);
                        }
                    }
                });
                composer.draft_comment_fingerprints.remove(&key);
                composer.drafts.insert(key.clone(), entry.text.clone());
                composer.set_input_text_without_draft_edit(entry.text.clone(), cx);
                composer.trusted_native_envelope_restored(&key, cx);
                let next_revision = composer.draft_revisions.entry(key.clone()).or_insert(0);
                *next_revision = next_revision.saturating_add(1);
                composer.stash_menu_open = false;
                composer.focus_pending = true;
                cx.notify();
                Some(composer.build_draft_record(&key, entry.text.clone(), cx))
            }).ok().flatten();
            let Some(record) = record else {
                return;
            };
            let save = crate::drafts::save(&engine, &record).await;
            let verified = if matches!(save, Ok(ComposerDraftSaveOutcome::Saved | ComposerDraftSaveOutcome::Unchanged)) {
                let task_id = (!key.is_empty()).then_some(key.as_str());
                crate::drafts::read(&engine, task_id, Some(&record.draft_id)).await
                    .ok()
                    .and_then(|read| read.draft)
                    .is_some_and(|saved| saved.draft_id == record.draft_id
                        && saved.revision >= record.revision
                        && saved.same_message_content(&record))
            } else {
                false
            };
            let can_delete = this.update(cx, |composer, cx| {
                if composer.state.read(cx).engine_generation() != generation {
                    return false;
                }
                if verified {
                    let saved = composer.draft_saved_revisions.entry(key.clone()).or_default();
                    *saved = (*saved).max(record.revision);
                } else {
                    composer.schedule_draft_save(key.clone(), Duration::ZERO, cx);
                    composer.stash_failure("Restored prompt is in the editor, but draft readback failed. The saved copy was kept.", cx);
                }
                verified && composer.current_key == key
                    && composer.draft_revisions.get(&key) == Some(&record.revision)
            }).unwrap_or(false);
            let deleted = if can_delete {
                crate::drafts::delete_stash(&engine, &entry.id).await
            } else {
                Ok(false)
            };
            let entries = crate::drafts::list_stash(&engine).await;
            this.update(cx, |composer, cx| {
                if composer.state.read(cx).engine_generation() != generation {
                    return;
                }
                composer.stash_task = None;
                if let Ok(entries) = entries {
                    composer.stash_entries = entries;
                }
                if can_delete && !matches!(deleted, Ok(true)) {
                    composer.stash_failure("Restored prompt is saved, but its stash entry could not be removed.", cx);
                }
                cx.notify();
            }).ok();
        }));
    }

    /// Stage an explicit context handoff for the blank-canvas draft without
    /// writing through the still-current source composer. The target owner is
    /// allowed to display and persist the text only after navigation makes it
    /// current and its durable draft has been loaded/merged.
    pub(crate) fn stage_new_task_context_handoff(
        &mut self,
        origin_key: &str,
        handoff: String,
        engine_generation: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = self.state.read(cx);
        if self.pending_draft_owner_stage.is_some()
            || self.current_key != origin_key
            || self.binding.is_pane()
            || state.selected_chat.as_deref() != Some(origin_key)
            || state.engine_generation() != engine_generation
        {
            return false;
        }
        let target_key = String::new();
        let previous_text = self.drafts.get(&target_key).cloned();
        let existing = previous_text.clone().unwrap_or_default();
        let marker = handoff.trim_end();
        let staged = if existing.contains(marker) {
            existing
        } else if existing.trim().is_empty() {
            handoff
        } else {
            format!("{handoff}{existing}")
        };
        self.drafts.insert(target_key.clone(), staged);
        self.draft_merge_on_load.insert(target_key.clone());
        self.pending_draft_owner_stage = Some(PendingDraftOwnerStage {
            origin_key: origin_key.to_string(),
            target_key,
            engine_generation,
            previous_text,
            target_seen: false,
        });
        true
    }

    fn cancel_pending_draft_owner_stage(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_draft_owner_stage.take() else {
            return;
        };
        self.draft_merge_on_load.remove(&pending.target_key);
        match pending.previous_text {
            Some(previous) => {
                self.drafts
                    .insert(pending.target_key.clone(), previous.clone());
                if self.current_key == pending.target_key {
                    self.set_input_text_without_draft_edit(previous, cx);
                }
            }
            None => {
                self.drafts.remove(&pending.target_key);
                if self.current_key == pending.target_key {
                    self.set_input_text_without_draft_edit(String::new(), cx);
                }
            }
        }
    }

    fn observe_pending_draft_owner(&mut self, next_key: &str, cx: &mut Context<Self>) {
        let current_generation = self.state.read(cx).engine_generation();
        let cancel = self
            .pending_draft_owner_stage
            .as_ref()
            .is_some_and(|pending| {
                pending.engine_generation != current_generation
                    || (next_key != pending.origin_key && next_key != pending.target_key)
                    || (pending.target_seen && next_key != pending.target_key)
            });
        if cancel {
            self.cancel_pending_draft_owner_stage(cx);
            return;
        }
        if let Some(pending) = self.pending_draft_owner_stage.as_mut()
            && next_key == pending.target_key
        {
            pending.target_seen = true;
        }
    }

    fn finish_pending_draft_owner_stage(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_draft_owner_stage.as_ref() else {
            return;
        };
        if self.state.read(cx).engine_generation() != pending.engine_generation {
            self.cancel_pending_draft_owner_stage(cx);
            return;
        }
        if self.current_key != pending.target_key
            || !pending.target_seen
            || !self.draft_loaded.contains(&pending.target_key)
        {
            return;
        }
        let pending = self.pending_draft_owner_stage.take().unwrap();
        // A durable target draft went through `merge_loaded_draft`, which
        // already removed this marker and scheduled the higher revision. A
        // brand-new canvas has no durable row, so persist the staged text now.
        if self.draft_merge_on_load.remove(&pending.target_key) {
            let revision = self
                .draft_revisions
                .entry(pending.target_key.clone())
                .or_insert(0);
            *revision = revision.saturating_add(1);
            self.schedule_draft_save(pending.target_key, Duration::ZERO, cx);
        }
    }

    fn build_draft_record(&self, key: &str, text: String, cx: &App) -> ComposerDraft {
        let (ordinary, shots) = if key == self.current_key {
            if let Some((saved_text, saved_attachments, saved_appshots)) = &self.queue_edit_draft {
                let text = saved_text.clone();
                return self.build_draft_record_parts(
                    key,
                    text,
                    saved_attachments,
                    saved_appshots,
                    cx,
                );
            }
            (self.attachments.get(key), self.appshots.get(key))
        } else {
            (self.attachments.get(key), self.appshots.get(key))
        };
        self.build_draft_record_parts(
            key,
            text,
            ordinary.map(Vec::as_slice).unwrap_or_default(),
            shots.map(Vec::as_slice).unwrap_or_default(),
            cx,
        )
    }

    fn build_draft_record_parts(
        &self,
        key: &str,
        text: String,
        ordinary: &[StagedAttachment],
        shots: &[CapturedAppshot],
        cx: &App,
    ) -> ComposerDraft {
        let blobs = self.draft_blobs.get(key);
        let mut attachments: Vec<DraftBlob> = ordinary
            .iter()
            .chain(shots.iter().map(|shot| &shot.screenshot))
            .map(|attachment| {
                blobs
                    .and_then(|blobs| blobs.get(&attachment.id))
                    .cloned()
                    .unwrap_or_else(|| {
                        crate::drafts::waiting_blob(&attachment.id, &attachment.name)
                    })
            })
            .collect();
        attachments.extend(
            self.draft_unavailable
                .get(key)
                .into_iter()
                .flatten()
                .cloned(),
        );
        let mut appshots: Vec<DraftAppshot> = shots
            .iter()
            .map(|shot| DraftAppshot {
                id: shot.id.clone(),
                screenshot_attachment_id: shot.screenshot.id.clone(),
                app_name: shot.app_name.clone(),
                bundle_identifier: shot.bundle_identifier.clone(),
                window_title: shot.window_title.clone(),
                accessibility_format_version: shot.accessibility.format_version,
                accessibility_content: shot.accessibility.content.clone(),
                accessibility_truncated: shot.accessibility.truncated,
                screenshot_dimensions: shot.screenshot_dimensions,
                captured_at_ms: shot.captured_at.timestamp_millis(),
            })
            .collect();
        appshots.extend(
            self.draft_unavailable_appshot_metadata
                .get(key)
                .into_iter()
                .flatten()
                .cloned(),
        );
        let review_comments = self
            .state
            .read(cx)
            .review_comments(key)
            .iter()
            .filter_map(|comment| serde_json::to_value(comment).ok())
            .collect();
        ComposerDraft {
            input_items: if key == self.current_key && self.queue_edit_draft.is_some() {
                self.queue_edit_input_items.clone().unwrap_or_default()
            } else {
                self.input_items.get(key).cloned().unwrap_or_default()
            },
            version: luna_proto::COMPOSER_DRAFT_VERSION,
            draft_id: self.draft_id_for(key),
            task_id: (!key.is_empty()).then(|| key.to_string()),
            revision: self.draft_revisions.get(key).copied().unwrap_or(1),
            text,
            attachments,
            appshots,
            context: DraftContext {
                review_comments,
                quoted: None,
                terminal: None,
            },
            execution: if key.is_empty() {
                self.pickers.read(cx).persisted_draft_execution(cx)
            } else {
                serde_json::Value::Null
            },
            updated_at_ms: chrono::Utc::now().timestamp_millis(),
        }
    }

    fn mark_draft_dirty(&mut self, cx: &mut Context<Self>) {
        if self.editing_queued.is_some() && !self.queue_edit_finishing {
            return;
        }
        let key = self.current_key.clone();
        let revision = self.draft_revisions.entry(key.clone()).or_insert(0);
        *revision = revision.saturating_add(1);
        self.schedule_draft_save(key, crate::drafts::SAVE_DEBOUNCE, cx);
    }

    fn show_draft_save_failure(&mut self, key: &str, message: SharedString) {
        self.failure = Some(message.clone());
        self.failure_key = Some(key.to_string());
        self.draft_save_failure = Some((key.to_string(), message));
    }

    fn clear_saved_draft_notices(&mut self, key: &str) {
        if !self.draft_is_saved(key) {
            return;
        }
        for notice in [&mut self.draft_save_failure, &mut self.draft_merge_notice] {
            if notice
                .as_ref()
                .is_some_and(|(notice_key, _)| notice_key == key)
                && let Some((_, message)) = notice.take()
                && self.failure_key.as_deref() == Some(key)
                && self.failure.as_ref() == Some(&message)
            {
                self.failure = None;
                self.failure_key = None;
            }
        }
    }

    fn activate_failure_notice(
        &mut self,
        key: &str,
        message: &SharedString,
        retry_draft: bool,
        cx: &mut Context<Self>,
    ) {
        if self.current_key != key || self.failure.as_ref() != Some(message) {
            return;
        }
        if retry_draft {
            self.schedule_draft_save(key.to_string(), Duration::ZERO, cx);
        } else {
            self.failure = None;
            self.failure_key = None;
        }
        cx.notify();
    }

    /// The accepted snapshot may be deleted by settlement. Advance the empty
    /// composer above it so a late save cannot recreate the sent text, while
    /// leaving any newer typing or attachments at their own revision.
    fn finish_applied_draft(&mut self, key: &str, submitted_revision: u64, cx: &mut Context<Self>) {
        if self.draft_revisions.get(key).copied().unwrap_or(0) <= submitted_revision {
            self.draft_revisions
                .insert(key.to_string(), submitted_revision.saturating_add(1));
            self.schedule_draft_save(key.to_string(), Duration::ZERO, cx);
        }
    }

    fn sync_review_comment_draft(&mut self, cx: &mut Context<Self>) {
        let key = self.current_key.clone();
        let encoded =
            serde_json::to_string(self.state.read(cx).review_comments(&key)).unwrap_or_default();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        encoded.hash(&mut hasher);
        let fingerprint = hasher.finish();
        match self.draft_comment_fingerprints.insert(key, fingerprint) {
            Some(previous) if previous != fingerprint => self.mark_draft_dirty(cx),
            _ => {}
        }
    }

    pub(crate) fn schedule_draft_save(
        &mut self,
        key: String,
        delay: Duration,
        cx: &mut Context<Self>,
    ) {
        if self.draft_is_saved(&key) {
            self.finish_close_flush_if_ready(cx);
            return;
        }
        if self.draft_submitting.get(&key).is_some_and(|submitted| {
            self.draft_revisions.get(&key).copied().unwrap_or(0) <= *submitted
        }) {
            return;
        }
        let (engine, engine_generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(engine) = engine else {
            if key == self.current_key {
                self.show_draft_save_failure(
                    &key,
                    "Draft not saved. Your text is still here. Reconnect, then retry saving."
                        .into(),
                );
            }
            self.fail_close_flush(cx);
            return;
        };
        let scheduled_revision = self.draft_revisions.get(&key).copied().unwrap_or(0);
        self.draft_saves_in_flight
            .insert(key.clone(), scheduled_revision);
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let Some(record) = this
                .update(cx, |composer, cx| {
                    if composer.state.read(cx).engine_generation() != engine_generation {
                        return None;
                    }
                    if composer
                        .draft_submitting
                        .get(&task_key)
                        .is_some_and(|submitted| {
                            composer
                                .draft_revisions
                                .get(&task_key)
                                .copied()
                                .unwrap_or(0)
                                <= *submitted
                        })
                    {
                        return None;
                    }
                    let text = if task_key == composer.current_key {
                        composer.input.read(cx).text().to_string()
                    } else {
                        composer.drafts.get(&task_key).cloned().unwrap_or_default()
                    };
                    let record = composer.build_draft_record(&task_key, text, cx);
                    composer
                        .draft_saves_in_flight
                        .insert(task_key.clone(), record.revision);
                    Some(record)
                })
                .ok()
                .flatten()
            else {
                return;
            };
            let revision = record.revision;
            let result = crate::drafts::save(&engine, &record).await;
            this.update(cx, |composer, cx| {
                if composer.state.read(cx).engine_generation() != engine_generation {
                    return;
                }
                if composer.draft_saves_in_flight.get(&task_key) == Some(&revision) {
                    composer.draft_saves_in_flight.remove(&task_key);
                }
                match result {
                    Ok(ComposerDraftSaveOutcome::Saved | ComposerDraftSaveOutcome::Unchanged) => {
                        let saved = composer
                            .draft_saved_revisions
                            .entry(task_key.clone())
                            .or_default();
                        *saved = (*saved).max(revision);
                        composer.draft_close_failed = false;
                        composer.clear_saved_draft_notices(&task_key);
                    }
                    Ok(ComposerDraftSaveOutcome::Stale { current_revision }) => {
                        let saved = composer
                            .draft_saved_revisions
                            .entry(task_key.clone())
                            .or_default();
                        *saved = (*saved).max(current_revision);
                        let local = composer
                            .draft_revisions
                            .entry(task_key.clone())
                            .or_default();
                        *local = (*local).max(current_revision.saturating_add(1));
                        composer.draft_merge_on_load.insert(task_key.clone());
                        composer.draft_loaded.remove(&task_key);
                        composer.load_draft(task_key.clone(), cx);
                    }
                    Err(error) => {
                        tracing::warn!(draft = %task_key, revision, %error, "draft autosave failed");
                        if task_key == composer.current_key {
                            composer.show_draft_save_failure(
                                &task_key,
                                "Draft not saved. Your text is still here. Retry saving."
                                    .into(),
                            );
                        }
                        composer.fail_close_flush(cx);
                    }
                }
                composer.finish_close_flush_if_ready(cx);
                cx.notify();
            })
            .ok();
        });
        self.draft_save_tasks.insert(key, task);
    }

    fn draft_is_saved(&self, key: &str) -> bool {
        self.draft_saved_revisions.get(key).copied().unwrap_or(0)
            >= self.draft_revisions.get(key).copied().unwrap_or(0)
    }

    fn all_drafts_saved(&self) -> bool {
        self.draft_revisions
            .keys()
            .all(|key| self.draft_is_saved(key))
            && self.draft_saves_in_flight.is_empty()
            && self.draft_blobs_in_flight.is_empty()
    }

    fn finish_close_flush_if_ready(&mut self, cx: &mut Context<Self>) {
        if self.draft_close_flushing && self.all_drafts_saved() {
            self.draft_close_flushing = false;
            self.draft_close_failed = false;
            cx.emit(ComposerEvent::DraftFlushCompleted);
        }
    }

    fn fail_close_flush(&mut self, cx: &mut Context<Self>) {
        if self.draft_close_flushing {
            self.draft_close_flushing = false;
            self.draft_close_failed = true;
            cx.emit(ComposerEvent::DraftFlushCompleted);
        }
    }

    pub(crate) fn prepare_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.all_drafts_saved() {
            return true;
        }
        self.draft_close_failed = false;
        self.draft_close_flushing = true;
        let dirty = self
            .draft_revisions
            .keys()
            .filter(|key| !self.draft_is_saved(key))
            .cloned()
            .collect::<Vec<_>>();
        for key in dirty {
            self.schedule_draft_save(key, Duration::ZERO, cx);
        }
        self.finish_close_flush_if_ready(cx);
        false
    }

    pub(crate) fn draft_flush_complete(&self) -> bool {
        self.all_drafts_saved() && !self.draft_close_failed
    }

    fn stage_draft_blob(
        &mut self,
        key: String,
        attachment: StagedAttachment,
        cx: &mut Context<Self>,
    ) {
        self.draft_blobs.entry(key.clone()).or_default().insert(
            attachment.id.clone(),
            crate::drafts::waiting_blob(&attachment.id, &attachment.name),
        );
        let (engine, engine_generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(engine) = engine else {
            self.draft_blobs.entry(key.clone()).or_default().insert(
                attachment.id.clone(),
                crate::drafts::failed_blob(
                    &attachment.id,
                    &attachment.name,
                    "The local engine was unavailable; reattach after restart.",
                ),
            );
            self.failure = Some("Attachment is not saved and will need reattachment.".into());
            self.failure_key = Some(key.clone());
            let revision = self.draft_revisions.entry(key.clone()).or_insert(0);
            *revision = revision.saturating_add(1);
            self.schedule_draft_save(key, Duration::ZERO, cx);
            return;
        };
        let id = attachment.id.clone();
        let name = attachment.name.clone();
        let task_key = (key.clone(), id.clone());
        self.draft_blobs_in_flight.insert(task_key.clone());
        let blob_key = key.clone();
        let callback_key = task_key.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = attachments::upload_attachment(
                &engine,
                cx.background_executor(),
                None,
                &id,
                &attachment,
                None,
            )
            .await;
            this.update(cx, |composer, cx| {
                if composer.state.read(cx).engine_generation() != engine_generation {
                    return;
                }
                composer.draft_blobs_in_flight.remove(&callback_key);
                let blob = match result {
                    Ok(path) => crate::drafts::saved_blob(&id, &name, path, attachment.bytes()),
                    Err(error) => {
                        composer.failure = Some(
                            format!("{name} is not saved and will need reattachment: {error}")
                                .into(),
                        );
                        composer.failure_key = Some(blob_key.clone());
                        crate::drafts::failed_blob(&id, &name, error)
                    }
                };
                composer
                    .draft_blobs
                    .entry(blob_key.clone())
                    .or_default()
                    .insert(id.clone(), blob);
                let revision = composer
                    .draft_revisions
                    .entry(blob_key.clone())
                    .or_insert(0);
                *revision = revision.saturating_add(1);
                composer.schedule_draft_save(blob_key.clone(), Duration::ZERO, cx);
                composer.finish_close_flush_if_ready(cx);
                cx.notify();
            })
            .ok();
        });
        self.draft_blob_tasks.insert(task_key, task);
        let revision = self.draft_revisions.entry(key.clone()).or_insert(0);
        *revision = revision.saturating_add(1);
        self.schedule_draft_save(key, Duration::ZERO, cx);
    }

    fn merge_loaded_draft(
        &mut self,
        key: &str,
        mut restored: RestoredComposerDraft,
        cx: &mut Context<Self>,
    ) {
        let durable_revision = restored.record.revision;
        let local_revision = self.draft_revisions.get(key).copied().unwrap_or(0);
        if key.is_empty() {
            self.canvas_draft_id = restored.record.draft_id.clone();
        }
        if restored.recoverable_entries.is_empty() {
            self.recoverable_submissions.remove(key);
        } else {
            self.recoverable_submissions
                .insert(key.to_string(), restored.recoverable_entries.clone());
        }

        if !restored.pending_same_revision {
            let local_text = if key == self.current_key {
                self.input.read(cx).text().to_string()
            } else {
                self.drafts.get(key).cloned().unwrap_or_default()
            };
            let merged_text = match (
                restored.record.text.trim().is_empty(),
                local_text.trim().is_empty(),
                restored.record.text == local_text,
            ) {
                (_, _, true) => local_text,
                (true, _, _) => local_text,
                (_, true, _) => restored.record.text.clone(),
                _ => format!("{}\n\n{}", restored.record.text, local_text),
            };
            let selections = self.input_items.entry(key.to_string()).or_default();
            for selection in &restored.record.input_items {
                if !selections.contains(selection) {
                    selections.push(selection.clone());
                }
            }
            let local_attachments = self.attachments.remove(key).unwrap_or_default();
            for attachment in local_attachments {
                if !restored
                    .ordinary
                    .iter()
                    .any(|existing| existing.id == attachment.id)
                {
                    restored.ordinary.push(attachment);
                }
            }
            let local_appshots = self.appshots.remove(key).unwrap_or_default();
            for shot in local_appshots {
                if !restored
                    .appshots
                    .iter()
                    .any(|existing| existing.id == shot.id)
                {
                    restored.appshots.push(shot);
                }
            }
            let mut blobs = restored
                .record
                .attachments
                .iter()
                .cloned()
                .map(|blob| (blob.attachment_id.clone(), blob))
                .collect::<HashMap<_, _>>();
            if let Some(local) = self.draft_blobs.remove(key) {
                blobs.extend(local);
            }
            self.draft_blobs.insert(key.to_string(), blobs);
            let mut unavailable = restored.unavailable;
            for blob in self.draft_unavailable.remove(key).unwrap_or_default() {
                if !unavailable
                    .iter()
                    .any(|existing| existing.attachment_id == blob.attachment_id)
                {
                    unavailable.push(blob);
                }
            }
            self.draft_unavailable.insert(key.to_string(), unavailable);
            self.draft_unavailable_appshots
                .entry(key.to_string())
                .or_default()
                .extend(restored.unavailable_appshots);
            self.draft_unavailable_appshot_metadata
                .entry(key.to_string())
                .or_default()
                .extend(restored.unavailable_appshot_metadata);
            self.attachments.insert(key.to_string(), restored.ordinary);
            self.appshots.insert(key.to_string(), restored.appshots);
            self.drafts.insert(key.to_string(), merged_text.clone());
            self.draft_comment_fingerprints.remove(key);
            self.state.update(cx, |state, _| {
                let mut seen = state
                    .review_comments(key)
                    .iter()
                    .map(|comment| comment.id.clone())
                    .collect::<HashSet<_>>();
                for value in &restored.record.context.review_comments {
                    if let Ok(comment) =
                        serde_json::from_value::<crate::comments::ReviewComment>(value.clone())
                        && seen.insert(comment.id.clone())
                    {
                        state.add_review_comment(key, comment);
                    }
                }
            });
            if key == self.current_key {
                self.set_input_text_without_draft_edit(merged_text, cx);
            }
            self.trusted_native_envelope_restored(key, cx);
            let message: SharedString =
                "This draft changed while it was loading. Both versions were kept and are being saved."
                    .into();
            self.failure = Some(message.clone());
            self.failure_key = Some(key.to_string());
            self.draft_merge_notice = Some((key.to_string(), message));
        } else {
            let warning: SharedString =
                "A submitted message is still awaiting an authoritative delivery receipt; newer typing is being saved separately."
                    .into();
            self.failure = Some(warning.clone());
            self.failure_key = Some(key.to_string());
            self.delivery_failure = restored
                .pending_entries
                .last()
                .map(|submission| (submission.command_id.clone(), warning));
        }
        self.draft_saved_revisions
            .entry(key.to_string())
            .and_modify(|saved| *saved = (*saved).max(durable_revision))
            .or_insert(durable_revision);
        self.draft_revisions.insert(
            key.to_string(),
            local_revision.max(durable_revision).saturating_add(1),
        );
        self.draft_merge_on_load.remove(key);
        self.draft_loaded.insert(key.to_string());
        self.schedule_draft_save(key.to_string(), Duration::ZERO, cx);
    }

    fn forget_recoverable_submissions(&mut self, key: &str, command_ids: &[String]) {
        if command_ids.is_empty() {
            return;
        }
        if let Some(entries) = self.recoverable_submissions.get_mut(key) {
            entries.retain(|entry| !command_ids.contains(&entry.command_id));
            if entries.is_empty() {
                self.recoverable_submissions.remove(key);
            }
        }
        if !self.recoverable_submissions.contains_key(key)
            && self.failure_key.as_deref() == Some(key)
            && matches!(
                self.failure.as_deref().map(|message| message.as_ref()),
                Some(
                    "A previously rejected message was restored as a recoverable draft."
                        | "A previous unsent message was preserved separately from this newer draft."
                        | "The previous message was rejected and preserved separately from your newer draft."
                        | "The submitted message was rejected and its draft was preserved."
                )
            )
        {
            self.failure = None;
            self.failure_key = None;
        }
    }

    fn discard_latest_submission(&mut self, cx: &mut Context<Self>) {
        let key = self.current_key.clone();
        let Some(submission) = self
            .recoverable_submissions
            .get(&key)
            .and_then(|entries| entries.last())
            .cloned()
        else {
            return;
        };
        let command_id = submission.command_id.clone();
        if self.draft_receipt_tasks.contains_key(&command_id) {
            return;
        }
        let (engine, engine_generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(engine) = engine else {
            self.failure = Some("The saved copy cannot be discarded while offline.".into());
            self.failure_key = Some(key);
            cx.notify();
            return;
        };
        let task_id = command_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = crate::drafts::discard_recoverable_submission(&engine, &command_id).await;
            this.update(cx, |composer, cx| {
                if composer.state.read(cx).engine_generation() != engine_generation {
                    return;
                }
                composer.draft_receipt_tasks.remove(&command_id);
                match result {
                    Ok((true, cleared_revision)) => {
                        if let Some(revision) = cleared_revision {
                            let current_text = if key == composer.current_key {
                                composer.input.read(cx).text().to_string()
                            } else {
                                composer.drafts.get(&key).cloned().unwrap_or_default()
                            };
                            let matches_saved = composer
                                .build_draft_record(&key, current_text, cx)
                                .same_message_content(&submission.snapshot);
                            if matches_saved {
                                composer.draft_save_tasks.remove(&key);
                                composer.draft_saves_in_flight.remove(&key);
                                composer.drafts.insert(key.clone(), String::new());
                                composer.attachments.remove(&key);
                                composer.input_items.remove(&key);
                                composer.appshots.remove(&key);
                                composer.draft_blobs.remove(&key);
                                composer.draft_unavailable.remove(&key);
                                composer.draft_unavailable_appshots.remove(&key);
                                composer.draft_unavailable_appshot_metadata.remove(&key);
                                composer.state.update(cx, |state, _| {
                                    state.purge_review_comments(&key);
                                });
                                if key == composer.current_key {
                                    composer.set_input_text_without_draft_edit(String::new(), cx);
                                }
                                composer.draft_revisions.insert(key.clone(), revision);
                                composer.draft_saved_revisions.insert(key.clone(), revision);
                            } else {
                                let next = composer
                                    .draft_revisions
                                    .get(&key)
                                    .copied()
                                    .unwrap_or(0)
                                    .max(revision)
                                    .saturating_add(1);
                                composer.draft_revisions.insert(key.clone(), next);
                                composer.schedule_draft_save(key.clone(), Duration::ZERO, cx);
                            }
                        }
                        composer.forget_recoverable_submissions(&key, &[command_id.clone()]);
                    }
                    Ok((false, _)) => {
                        composer.draft_loaded.remove(&key);
                        composer.load_draft(key.clone(), cx);
                    }
                    Err(error) => {
                        composer.failure =
                            Some(format!("Saved copy could not be discarded: {error}").into());
                        composer.failure_key = Some(key.clone());
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.draft_receipt_tasks.insert(task_id, task);
    }

    /// Merge the newest recoverable submission into the live draft. The
    /// recoverable row is deleted only after the merged draft is durably
    /// saved, so a failed read/save remains retryable after restart.
    fn recover_latest_submission(&mut self, cx: &mut Context<Self>) {
        let key = self.current_key.clone();
        let Some(submission) = self
            .recoverable_submissions
            .get(&key)
            .and_then(|entries| entries.last())
            .cloned()
        else {
            return;
        };
        let (engine, engine_generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(engine) = engine else {
            self.failure =
                Some("Recovery is unavailable while the local engine is offline.".into());
            self.failure_key = Some(key);
            cx.notify();
            return;
        };
        if self
            .draft_receipt_tasks
            .contains_key(&submission.command_id)
        {
            return;
        }

        let command_id = submission.command_id.clone();
        let task_command_id = command_id.clone();
        let prior_failure = self.failure.clone();
        let prior_failure_key = self.failure_key.clone();
        let task = cx.spawn(async move |this, cx| {
            let appshot_attachment_ids = submission
                .snapshot
                .appshots
                .iter()
                .map(|shot| shot.screenshot_attachment_id.clone())
                .collect::<HashSet<_>>();
            let mut loaded = HashMap::new();
            for blob in &submission.snapshot.attachments {
                if blob.state != DraftBlobState::Saved {
                    continue;
                }
                let Some(path) = blob.path.as_deref() else {
                    continue;
                };
                if let Some(image) = attachments::read_attachment_image(
                    &engine,
                    cx.background_executor(),
                    None,
                    path,
                    None,
                )
                .await
                {
                    loaded.insert(
                        blob.attachment_id.clone(),
                        StagedAttachment {
                            id: blob.attachment_id.clone(),
                            name: blob.name.clone(),
                            image: image.image,
                        },
                    );
                }
            }

            let record = this
                .update(cx, |composer, cx| {
                    if composer.state.read(cx).engine_generation() != engine_generation {
                        return None;
                    }
                    let local_text = if key == composer.current_key {
                        composer.input.read(cx).text().to_string()
                    } else {
                        composer.drafts.get(&key).cloned().unwrap_or_default()
                    };
                    let recovered_text = submission.snapshot.text.clone();
                    let merged_text = match (
                        local_text.trim().is_empty(),
                        recovered_text.trim().is_empty(),
                        local_text == recovered_text,
                    ) {
                        (_, _, true) => local_text,
                        (true, _, _) => recovered_text,
                        (_, true, _) => local_text,
                        _ => format!("{local_text}\n\n{recovered_text}"),
                    };

                    let selections = composer.input_items.entry(key.clone()).or_default();
                    for selection in &submission.snapshot.input_items {
                        if !selections.contains(selection) {
                            selections.push(selection.clone());
                        }
                    }
                    let ordinary = composer.attachments.entry(key.clone()).or_default();
                    for blob in &submission.snapshot.attachments {
                        if appshot_attachment_ids.contains(&blob.attachment_id) {
                            continue;
                        }
                        if let Some(attachment) = loaded.get(&blob.attachment_id)
                            && !ordinary.iter().any(|item| item.id == attachment.id)
                        {
                            ordinary.push(attachment.clone());
                        }
                    }
                    let shots = composer.appshots.entry(key.clone()).or_default();
                    for shot in &submission.snapshot.appshots {
                        let Some(screenshot) = loaded.get(&shot.screenshot_attachment_id).cloned()
                        else {
                            continue;
                        };
                        if shots.iter().any(|item| item.id == shot.id) {
                            continue;
                        }
                        shots.push(CapturedAppshot {
                            id: shot.id.clone(),
                            app_name: shot.app_name.clone(),
                            bundle_identifier: shot.bundle_identifier.clone(),
                            window_title: shot.window_title.clone(),
                            accessibility: appshots::AccessibilitySnapshot {
                                format_version: shot.accessibility_format_version,
                                content: shot.accessibility_content.clone(),
                                truncated: shot.accessibility_truncated,
                            },
                            screenshot,
                            screenshot_dimensions: shot.screenshot_dimensions,
                            app_icon: None,
                            captured_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                                shot.captured_at_ms,
                            )
                            .unwrap_or_else(chrono::Utc::now),
                        });
                    }
                    let unavailable = composer.draft_unavailable.entry(key.clone()).or_default();
                    let blob_map = composer.draft_blobs.entry(key.clone()).or_default();
                    for blob in &submission.snapshot.attachments {
                        let mut restored = blob.clone();
                        if !loaded.contains_key(&blob.attachment_id) {
                            restored.state = DraftBlobState::NeedsReattachment;
                            restored.path = None;
                            restored.error =
                                Some("The recovered blob is unavailable or corrupt".into());
                        }
                        blob_map
                            .entry(restored.attachment_id.clone())
                            .or_insert_with(|| restored.clone());
                        if restored.state == DraftBlobState::NeedsReattachment
                            && !unavailable
                                .iter()
                                .any(|item| item.attachment_id == restored.attachment_id)
                        {
                            unavailable.push(restored);
                        }
                    }
                    let unavailable_appshots = composer
                        .draft_unavailable_appshots
                        .entry(key.clone())
                        .or_default();
                    let unavailable_metadata = composer
                        .draft_unavailable_appshot_metadata
                        .entry(key.clone())
                        .or_default();
                    for shot in &submission.snapshot.appshots {
                        if !loaded.contains_key(&shot.screenshot_attachment_id) {
                            unavailable_appshots.insert(shot.screenshot_attachment_id.clone());
                            if !unavailable_metadata.iter().any(|item| item.id == shot.id) {
                                unavailable_metadata.push(shot.clone());
                            }
                        }
                    }

                    composer.drafts.insert(key.clone(), merged_text.clone());
                    if key == composer.current_key {
                        composer.set_input_text_without_draft_edit(merged_text.clone(), cx);
                    }
                    composer.trusted_native_envelope_restored(&key, cx);
                    let revision = composer.draft_revisions.entry(key.clone()).or_default();
                    *revision = (*revision)
                        .max(submission.snapshot.revision)
                        .saturating_add(1);
                    Some(composer.build_draft_record(&key, merged_text, cx))
                })
                .ok()
                .flatten();
            let Some(record) = record else {
                return;
            };
            let revision = record.revision;
            let saved = crate::drafts::save(&engine, &record).await;
            let settled = match &saved {
                Ok(ComposerDraftSaveOutcome::Saved | ComposerDraftSaveOutcome::Unchanged) => {
                    crate::drafts::settle_submission(
                        &engine,
                        &submission.command_id,
                        "recovered",
                        None,
                    )
                    .await
                }
                _ => Ok(()),
            };
            this.update(cx, |composer, cx| {
                if composer.state.read(cx).engine_generation() != engine_generation {
                    return;
                }
                composer.draft_receipt_tasks.remove(&submission.command_id);
                match (saved, settled) {
                    (
                        Ok(ComposerDraftSaveOutcome::Saved | ComposerDraftSaveOutcome::Unchanged),
                        Ok(()),
                    ) => {
                        composer
                            .draft_saved_revisions
                            .entry(key.clone())
                            .and_modify(|value| *value = (*value).max(revision))
                            .or_insert(revision);
                        composer.clear_saved_draft_notices(&key);
                        if let Some(entries) = composer.recoverable_submissions.get_mut(&key) {
                            entries.retain(|entry| entry.command_id != submission.command_id);
                            if entries.is_empty() {
                                composer.recoverable_submissions.remove(&key);
                            }
                        }
                        if composer.failure == prior_failure
                            && composer.failure_key == prior_failure_key
                        {
                            composer.failure = None;
                            composer.failure_key = None;
                        }
                    }
                    (Ok(ComposerDraftSaveOutcome::Stale { current_revision }), _) => {
                        composer
                            .draft_saved_revisions
                            .entry(key.clone())
                            .and_modify(|value| *value = (*value).max(current_revision))
                            .or_insert(current_revision);
                        composer.draft_merge_on_load.insert(key.clone());
                        composer.draft_loaded.remove(&key);
                        composer.load_draft(key.clone(), cx);
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        composer.failure = Some(
                            format!("The previous message is still recoverable: {error}").into(),
                        );
                        composer.failure_key = Some(key.clone());
                    }
                }
                composer.finish_close_flush_if_ready(cx);
                cx.notify();
            })
            .ok();
        });
        self.draft_receipt_tasks.insert(task_command_id, task);
    }

    /// Composer state is profile-local. Runtime replacement changes the
    /// engine generation before any callbacks from the old profile can land;
    /// clear every cached draft surface and cancel its tasks at that boundary.
    fn sync_draft_engine_generation(&mut self, cx: &mut Context<Self>) {
        let (generation, key) = {
            let state = self.state.read(cx);
            (state.engine_generation(), self.binding.key(state))
        };
        let Some(previous) = self.draft_engine_generation.replace(generation) else {
            return;
        };
        if previous == generation {
            return;
        }

        self.draft_save_tasks.clear();
        self.stash_task = None;
        self.stash_entries.clear();
        self.stash_menu_open = false;
        self.history_recall = None;
        self.history_recall_edit_pending = None;
        self.draft_load_tasks.clear();
        self.draft_blob_tasks.clear();
        self.draft_receipt_tasks.clear();
        self.drafts.clear();
        self.attachments.clear();
        self.input_items.clear();
        self.queue_edit_input_items = None;
        self.appshots.clear();
        self.draft_revisions.clear();
        self.draft_saved_revisions.clear();
        self.draft_blobs.clear();
        self.draft_unavailable.clear();
        self.draft_unavailable_appshots.clear();
        self.draft_unavailable_appshot_metadata.clear();
        self.draft_loaded.clear();
        self.draft_saves_in_flight.clear();
        self.draft_blobs_in_flight.clear();
        self.draft_submitting.clear();
        self.draft_comment_fingerprints.clear();
        self.recoverable_submissions.clear();
        self.draft_merge_on_load.clear();
        self.pending_draft_owner_stage = None;
        self.draft_close_flushing = false;
        self.draft_close_failed = false;
        self.queue_edit_draft = None;
        self.editing_queued = None;
        self.queue_edit_lease_id = None;
        self.queue_edit_base_text_hash = None;
        self.queue_edit_revision = None;
        self.queue_edit_command_id = None;
        self.queue_edit_chat_id = None;
        self.queue_edit_host_device_id = None;
        self.queue_edit_pending_id = None;
        self.queue_edit_finishing = false;
        self.queue_edit_task = None;
        self.queue_edit_renew_task = None;
        self.queue_full_preview = None;
        self.release_queue_previews(cx);
        self.queue_removing.clear();
        self.preview = None;
        self.failure = None;
        self.failure_key = None;
        self.draft_save_failure = None;
        self.draft_merge_notice = None;
        self.delivery_failure = None;
        self.recovery_notice = None;
        self.wizard = None;
        self.answered_requests.clear();
        self.async_submissions.clear();
        self.async_attempts.clear();
        self.async_failure = None;
        self.advance_task = None;
        self.send_task = None;
        self.action_task = None;
        self.sending = false;
        self.launching_new_chat = false;
        self.interrupt_tasks.clear();
        self.interrupting.clear();
        self.picker_task = None;
        self.mention_task = None;
        self.mention = FileMentionState::default();
        self.slash_task = None;
        self.slash = SlashState::default();
        self.slash_cache.clear();
        self.canvas_draft_id = uuid::Uuid::new_v4().to_string();
        self.current_key = key;
        self.reset_native_picker_owner(cx);
        self.last_canvas_draft_config = self.pickers.read(cx).draft().clone();
        self.input.update(cx, |input, cx| {
            input.read_only = false;
            cx.notify();
        });
        self.set_input_text_without_draft_edit(String::new(), cx);
    }

    fn watch_loaded_pending_submissions(
        &mut self,
        key: &str,
        pending: &[DraftSubmission],
        engine: &crate::state::EngineHandle,
        cx: &mut Context<Self>,
    ) {
        for submission in pending {
            let task_id = submission
                .task_id
                .clone()
                .or_else(|| (!key.is_empty()).then(|| key.to_string()));
            if let Some(task_id) = task_id {
                self.watch_draft_receipt(
                    submission.command_id.clone(),
                    task_id,
                    key.to_string(),
                    submission.snapshot.revision,
                    Vec::new(),
                    key.is_empty(),
                    engine.clone(),
                    cx,
                );
            }
        }
    }

    fn load_draft(&mut self, key: String, cx: &mut Context<Self>) {
        if self.draft_loaded.contains(&key) || self.draft_load_tasks.contains_key(&key) {
            return;
        }
        let (engine, engine_generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(engine) = engine else {
            return;
        };
        let task_id = (!key.is_empty()).then(|| key.clone());
        let canvas_id = key.is_empty().then(|| self.canvas_draft_id.clone());
        let starting_revision = self.draft_revisions.get(&key).copied().unwrap_or(0);
        let load_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let read = crate::drafts::read(
                &engine,
                task_id.as_deref(),
                canvas_id.as_deref().filter(|_| starting_revision > 0),
            )
            .await;
            let restored = match read {
                Ok(mut read) => {
                    let mut applied = HashSet::new();
                    for submission in &mut read.submissions {
                        if submission.state != DraftSubmissionState::Pending {
                            continue;
                        }
                        let receipt = engine
                            .client()
                            .call_as::<Option<luna_proto::CommandReceipt>>(
                                methods::GET_TASK_COMMAND_RECEIPT,
                                serde_json::json!({
                                    "commandId": submission.command_id.clone()
                                }),
                            )
                            .await;
                        match receipt {
                            Ok(Some(receipt)) if receipt.status == CommandReceiptStatus::Applied => {
                                if crate::drafts::settle_submission(
                                    &engine,
                                    &submission.command_id,
                                    "applied",
                                    submission.queue_id.as_deref(),
                                )
                                .await
                                .is_ok()
                                {
                                    applied.insert(submission.command_id.clone());
                                }
                            }
                            Ok(Some(receipt)) if receipt.status != CommandReceiptStatus::Pending => {
                                if crate::drafts::settle_submission(
                                    &engine,
                                    &submission.command_id,
                                    "recoverable",
                                    None,
                                )
                                .await
                                .is_ok()
                                {
                                    submission.state = DraftSubmissionState::Recoverable;
                                }
                            }
                            _ => {}
                        }
                    }
                    read.submissions
                        .retain(|submission| !applied.contains(&submission.command_id));
                    if !applied.is_empty()
                        && let Ok(refreshed) = crate::drafts::read(
                            &engine,
                            task_id.as_deref(),
                            canvas_id.as_deref().filter(|_| starting_revision > 0),
                        )
                        .await
                    {
                        read = refreshed;
                    }
                    let pending_entries: Vec<_> = read
                        .submissions
                        .iter()
                        .filter(|submission| submission.state == DraftSubmissionState::Pending)
                        .cloned()
                        .collect();
                    let pending_submission = !pending_entries.is_empty();
                    let recoverable_submission = read.submissions.iter().any(|submission| {
                        submission.state == DraftSubmissionState::Recoverable
                    });
                    let recoverable_entries = read
                        .submissions
                        .iter()
                        .filter(|submission| {
                            submission.state == DraftSubmissionState::Recoverable
                        })
                        .cloned()
                        .collect();
                    let recovery_action = if let Some(submission) = read
                        .submissions
                        .iter()
                        .rev()
                        .find(|submission| submission.state == DraftSubmissionState::Recoverable)
                    {
                        engine
                            .client()
                            .call_as::<Option<CommandReceipt>>(
                                methods::GET_TASK_COMMAND_RECEIPT,
                                serde_json::json!({ "commandId": submission.command_id.clone() }),
                            )
                            .await
                            .ok()
                            .flatten()
                            .and_then(|receipt| receipt.recovery_action)
                    } else {
                        None
                    };
                    let mut record = match read.draft {
                        Some(record) => record,
                        None => match read.submissions.last() {
                            Some(submission) => {
                                let mut record = submission.snapshot.clone();
                                if submission.state == DraftSubmissionState::Recoverable {
                                    // Recovery is an explicit choice. Keeping the
                                    // submitted snapshot as the live draft here
                                    // reinserted an already-sent prompt on reopen.
                                    record.text.clear();
                                    record.input_items.clear();
                                    record.attachments.clear();
                                    record.appshots.clear();
                                    record.context = Default::default();
                                }
                                record
                            }
                            None => {
                                this.update(cx, |composer, cx| {
                                    if composer.state.read(cx).engine_generation()
                                        != engine_generation
                                    {
                                        return;
                                    }
                                    composer.draft_loaded.insert(load_key.clone());
                                    composer.draft_load_tasks.remove(&load_key);
                                    composer.draft_submitting.remove(&load_key);
                                    composer.finish_pending_draft_owner_stage(cx);
                                })
                                .ok();
                                return;
                            }
                        },
                    };
                    let pending_same_revision = read.submissions.iter().any(|submission| {
                        submission.state == DraftSubmissionState::Pending
                            && submission.snapshot.revision == record.revision
                    });
                    let task_has_newer_content = !record.text.is_empty()
                        || !record.attachments.is_empty()
                        || !record.appshots.is_empty()
                        || record.context != DraftContext::default();
                    let recoverable_separate = read.submissions.iter().any(|submission| {
                        if submission.state != DraftSubmissionState::Recoverable {
                            return false;
                        }
                        // The first send begins on the canvas, but its durable
                        // submission belongs to the newly minted task. A blank
                        // task draft with a different id is that ordinary
                        // re-key, not newer user writing. Revisions from those
                        // two draft ids are not comparable.
                        let first_submit_rekey = task_id.as_deref().is_some_and(|task| {
                            submission.task_id.as_deref() == Some(task)
                                && submission.snapshot.task_id.is_none()
                                && record.task_id.as_deref() == Some(task)
                                && submission.snapshot.draft_id != record.draft_id
                        });
                        if first_submit_rekey {
                            task_has_newer_content
                        } else {
                            submission.snapshot.draft_id != record.draft_id
                                || submission.snapshot.revision < record.revision
                        }
                    });
                    let appshot_attachment_ids = record
                        .appshots
                        .iter()
                        .map(|shot| shot.screenshot_attachment_id.clone())
                        .collect::<HashSet<_>>();
                    let mut loaded = HashMap::new();
                    let mut warnings = Vec::new();
                    for blob in &mut record.attachments {
                        if blob.state != DraftBlobState::Saved {
                            warnings.push(blob.name.clone());
                            continue;
                        }
                        let Some(path) = blob.path.as_deref() else {
                            blob.state = DraftBlobState::NeedsReattachment;
                            warnings.push(blob.name.clone());
                            continue;
                        };
                        match attachments::read_attachment_image(
                            &engine,
                            cx.background_executor(),
                            None,
                            path,
                            None,
                        )
                        .await
                        {
                            Some(image) => {
                                loaded.insert(
                                    blob.attachment_id.clone(),
                                    StagedAttachment {
                                        id: blob.attachment_id.clone(),
                                        name: blob.name.clone(),
                                        image: image.image,
                                    },
                                );
                            }
                            None => {
                                blob.state = DraftBlobState::NeedsReattachment;
                                blob.path = None;
                                blob.error = Some("The saved blob is unavailable or corrupt".into());
                                warnings.push(blob.name.clone());
                            }
                        }
                    }
                    let ordinary = record
                        .attachments
                        .iter()
                        .filter(|blob| !appshot_attachment_ids.contains(&blob.attachment_id))
                        .filter_map(|blob| loaded.get(&blob.attachment_id).cloned())
                        .collect();
                    let unavailable = record
                        .attachments
                        .iter()
                        .filter(|blob| !loaded.contains_key(&blob.attachment_id))
                        .cloned()
                        .collect::<Vec<_>>();
                    let unavailable_appshots = unavailable
                        .iter()
                        .filter(|blob| appshot_attachment_ids.contains(&blob.attachment_id))
                        .map(|blob| blob.attachment_id.clone())
                        .collect();
                    let unavailable_appshot_metadata = record
                        .appshots
                        .iter()
                        .filter(|shot| {
                            !loaded.contains_key(&shot.screenshot_attachment_id)
                        })
                        .cloned()
                        .collect();
                    let appshots = record
                        .appshots
                        .iter()
                        .filter_map(|shot| {
                            let screenshot = loaded.get(&shot.screenshot_attachment_id)?.clone();
                            Some(CapturedAppshot {
                                id: shot.id.clone(),
                                app_name: shot.app_name.clone(),
                                bundle_identifier: shot.bundle_identifier.clone(),
                                window_title: shot.window_title.clone(),
                                accessibility: appshots::AccessibilitySnapshot {
                                    format_version: shot.accessibility_format_version,
                                    content: shot.accessibility_content.clone(),
                                    truncated: shot.accessibility_truncated,
                                },
                                screenshot,
                                screenshot_dimensions: shot.screenshot_dimensions,
                                app_icon: None,
                                captured_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                                    shot.captured_at_ms,
                                )
                                .unwrap_or_else(chrono::Utc::now),
                            })
                        })
                        .collect();
                    Some(RestoredComposerDraft {
                        record,
                        ordinary,
                        appshots,
                        unavailable,
                        unavailable_appshots,
                        unavailable_appshot_metadata,
                        warning: (!warnings.is_empty()).then(|| {
                            format!(
                                "{} needs reattachment before this draft can be sent.",
                                warnings.join(", ")
                            )
                        }),
                        recovery_action,
                        pending_submission,
                        pending_same_revision,
                        recoverable_submission,
                        recoverable_separate,
                        recoverable_entries,
                        pending_entries,
                    })
                }
                Err(error) => {
                    this.update(cx, |composer, cx| {
                        if composer.state.read(cx).engine_generation() != engine_generation {
                            return;
                        }
                        if load_key == composer.current_key {
                            composer.failure = Some(
                                format!("Draft could not be loaded; new changes remain in memory: {error}")
                                    .into(),
                            );
                            composer.failure_key = Some(load_key.clone());
                            cx.notify();
                        }
                    })
                    .ok();
                    None
                }
            };
            this.update(cx, |composer, cx| {
                if composer.state.read(cx).engine_generation() != engine_generation {
                    return;
                }
                composer.draft_load_tasks.remove(&load_key);
                let Some(restored) = restored else {
                    composer.draft_loaded.insert(load_key.clone());
                    return;
                };
                // A higher local revision means typing or attachment intake
                // raced this read. Merge the durable row and local state,
                // then persist a revision strictly above both versions.
                if composer
                    .draft_revisions
                    .get(&load_key)
                    .copied()
                    .unwrap_or(0)
                    > starting_revision
                    || composer.draft_merge_on_load.contains(&load_key)
                {
                    let pending = restored.pending_entries.clone();
                    composer.merge_loaded_draft(&load_key, restored, cx);
                    composer.watch_loaded_pending_submissions(&load_key, &pending, &engine, cx);
                    composer.finish_pending_draft_owner_stage(cx);
                    return;
                }
                composer.draft_loaded.insert(load_key.clone());
                let pending_same_revision = restored.pending_same_revision;
                if load_key.is_empty() {
                    composer.canvas_draft_id = restored.record.draft_id.clone();
                }
                composer
                    .draft_revisions
                    .insert(load_key.clone(), restored.record.revision);
                composer
                    .draft_saved_revisions
                    .insert(load_key.clone(), restored.record.revision);
                composer.draft_blobs.insert(
                    load_key.clone(),
                    restored
                        .record
                        .attachments
                        .iter()
                        .cloned()
                        .map(|blob| (blob.attachment_id.clone(), blob))
                        .collect(),
                );
                composer
                    .draft_unavailable
                    .insert(load_key.clone(), restored.unavailable);
                composer
                    .draft_unavailable_appshots
                    .insert(load_key.clone(), restored.unavailable_appshots);
                composer.draft_unavailable_appshot_metadata.insert(
                    load_key.clone(),
                    restored.unavailable_appshot_metadata,
                );
                if restored.recoverable_entries.is_empty() {
                    composer.recoverable_submissions.remove(&load_key);
                } else {
                    composer
                        .recoverable_submissions
                        .insert(load_key.clone(), restored.recoverable_entries.clone());
                }
                if !pending_same_revision {
                    composer.input_items.insert(load_key.clone(), restored.record.input_items.clone());
                    composer
                        .drafts
                        .insert(load_key.clone(), restored.record.text.clone());
                    composer
                        .attachments
                        .insert(load_key.clone(), restored.ordinary);
                    composer.appshots.insert(load_key.clone(), restored.appshots);
                    composer.draft_comment_fingerprints.remove(&load_key);
                    composer.state.update(cx, |state, _| {
                        state.purge_review_comments(&load_key);
                        for value in &restored.record.context.review_comments {
                            if let Ok(comment) = serde_json::from_value(value.clone()) {
                                state.add_review_comment(&load_key, comment);
                            }
                        }
                    });
                    if load_key == composer.current_key {
                        composer.set_input_text_without_draft_edit(
                            restored.record.text.clone(),
                            cx,
                        );
                        composer.trusted_native_envelope_restored(&load_key, cx);
                        composer.pickers.update(cx, |pickers, cx| {
                            pickers.restore_draft_execution(&restored.record.execution, cx)
                        });
                        composer.last_canvas_draft_config =
                            composer.pickers.read(cx).draft().clone();
                    }
                }
                if let Some(warning) = restored.warning {
                    composer.failure = Some(warning.into());
                    composer.failure_key = Some(load_key.clone());
                } else if restored.pending_submission {
                    let warning: SharedString =
                        "A submitted message is still awaiting an authoritative delivery receipt; it was preserved and will not be resent automatically."
                            .into();
                    composer.failure = Some(warning.clone());
                    composer.failure_key = Some(load_key.clone());
                    composer.delivery_failure = restored
                        .pending_entries
                        .last()
                        .map(|submission| (submission.command_id.clone(), warning));
                } else if restored.recoverable_separate {
                    let message: SharedString =
                        "A previous unsent message was preserved separately from this newer draft."
                            .into();
                    composer.failure = Some(message.clone());
                    composer.failure_key = Some(load_key.clone());
                    composer.recovery_notice = Some((load_key.clone(), message));
                } else if restored.recoverable_submission {
                    let generic = restored.recovery_action.is_none();
                    let message: SharedString = restored
                        .recovery_action
                        .unwrap_or_else(|| {
                            "A previously rejected message was restored as a recoverable draft."
                                .into()
                        })
                        .into();
                    composer.failure = Some(message.clone());
                    composer.failure_key = Some(load_key.clone());
                    if generic {
                        composer.recovery_notice = Some((load_key.clone(), message));
                    }
                }
                composer.watch_loaded_pending_submissions(
                    &load_key,
                    &restored.pending_entries,
                    &engine,
                    cx,
                );
                composer.finish_pending_draft_owner_stage(cx);
                cx.notify();
            })
            .ok();
        });
        self.draft_load_tasks.insert(key, task);
    }

    fn watch_draft_receipt(
        &mut self,
        command_id: String,
        task_id: String,
        draft_key: String,
        submitted_revision: u64,
        superseded_recoverable: Vec<String>,
        may_have_setup: bool,
        engine: crate::state::EngineHandle,
        cx: &mut Context<Self>,
    ) {
        if self.draft_receipt_tasks.contains_key(&command_id) {
            return;
        }
        let receipt_key = command_id.clone();
        let map_key = command_id.clone();
        let receipt_target = {
            let state = self.state.read(cx);
            state
                .chats
                .iter()
                .find(|chat| chat.id == task_id)
                .map(|chat| chat.device_id.clone())
                .filter(|device| state.local_device_id.as_deref() != Some(device.as_str()))
        };
        let task = cx.spawn(async move |this, cx| {
            let mut stream_params = serde_json::json!({ "taskId": task_id.clone() });
            let mut receipt_params = serde_json::json!({ "commandId": command_id.clone() });
            if let Some(target) = receipt_target.as_deref() {
                stream_params["targetDeviceId"] = target.into();
                receipt_params["targetDeviceId"] = target.into();
            }
            let mut stream = engine
                .client()
                .subscribe(methods::WATCH_TASK_COMMANDS, stream_params)
                .await
                .ok();
            let mut current = None;
            let receipt = loop {
                let candidate = match current.take() {
                    Some(receipt) => Some(receipt),
                    None => engine
                        .client()
                        .call_as::<Option<luna_proto::CommandReceipt>>(
                            methods::GET_TASK_COMMAND_RECEIPT,
                            receipt_params.clone(),
                        )
                        .await
                        .ok()
                        .flatten(),
                };
                if let Some(receipt) = candidate
                    && receipt.command_id == command_id
                    && receipt.status != CommandReceiptStatus::Pending
                {
                    break receipt;
                }
                if let Some(events) = stream.as_mut() {
                    let receive = Box::pin(events.recv());
                    let deadline = Box::pin(cx.background_executor().timer(Duration::from_secs(3)));
                    let received = match futures::future::select(receive, deadline).await {
                        futures::future::Either::Left((value, _)) => Some(value),
                        futures::future::Either::Right(_) => None,
                    };
                    match received {
                        Some(Some(value)) => {
                            current = value
                                .get("receipt")
                                .and_then(|value| serde_json::from_value(value.clone()).ok());
                        }
                        Some(None) => stream = None,
                        None => {}
                    }
                } else {
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                }
            };
            let applied = receipt.status == CommandReceiptStatus::Applied;
            let recovery_text = receipt_failure_message(
                &receipt,
                "The submitted message was rejected and its draft was preserved.",
            );
            let retain_setup_task = !applied
                && draft_key.is_empty()
                && may_have_setup
                && retain_failed_worktree_setup(&engine, &task_id).await;
            let outcome = if applied { "applied" } else { "recoverable" };
            let queue_id = (receipt.intent == "queueMessage").then_some(command_id.as_str());
            let mut retry_secs = 2;
            loop {
                match crate::drafts::settle_submission(&engine, &command_id, outcome, queue_id)
                    .await
                {
                    Ok(()) => break,
                    Err(error) => {
                        tracing::warn!(%command_id, %error, "draft receipt settlement will retry");
                        cx.background_executor()
                            .timer(Duration::from_secs(retry_secs))
                            .await;
                        retry_secs = (retry_secs * 2).min(30);
                    }
                }
            }
            if !applied && draft_key.is_empty() && !retain_setup_task {
                let _ = crate::task_commands::execute(
                    &engine,
                    &task_id,
                    serde_json::json!({ "kind": "delete" }),
                )
                .await;
            }
            this.update(cx, |composer, cx| {
                composer.draft_receipt_tasks.remove(&receipt_key);
                composer.draft_submitting.remove(&draft_key);
                if !applied {
                    let restore_key = if retain_setup_task {
                        task_id.clone()
                    } else {
                        draft_key.clone()
                    };
                    composer.draft_loaded.remove(&restore_key);
                    if draft_key.is_empty()
                        && !retain_setup_task
                        && composer.state.read(cx).selected_chat.as_deref()
                            == Some(task_id.as_str())
                    {
                        composer
                            .state
                            .update(cx, |state, cx| state.select_chat(None, cx));
                    }
                    if retain_setup_task {
                        // The immutable rejected submission belongs to this
                        // task; clear only the old canvas's submitted revision.
                        composer.finish_applied_draft(&draft_key, submitted_revision, cx);
                    }
                    composer.failure = Some(recovery_text.into());
                    composer.failure_key = Some(restore_key.clone());
                    composer.delivery_failure = None;
                    composer.load_draft(restore_key, cx);
                } else {
                    composer.finish_applied_draft(&draft_key, submitted_revision, cx);
                    if queue_id.is_none() {
                        composer
                            .forget_recoverable_submissions(&draft_key, &superseded_recoverable);
                    }
                    if composer
                        .delivery_failure
                        .as_ref()
                        .is_some_and(|(id, message)| {
                                id == &command_id
                                    && matches!(
                                        composer.failure_key.as_deref(),
                                        Some(key) if key == task_id.as_str() || key == draft_key.as_str()
                                    )
                                    && composer.failure.as_ref() == Some(message)
                        })
                    {
                        composer.failure = None;
                        composer.failure_key = None;
                    }
                    composer.delivery_failure = None;
                }
                cx.notify();
            })
            .ok();
        });
        self.draft_receipt_tasks.insert(map_key, task);
    }

    // ---- attachment staging (use-attachments.ts) ----

    /// Staged attachments for the chat the composer is showing.
    pub(crate) fn staged(&self) -> &[StagedAttachment] {
        self.attachments
            .get(&self.current_key)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn queue_preview_limit(&self) -> usize {
        if self.last_available_width.unwrap_or(COMPOSER_MAX_WIDTH) < 520.0 {
            1
        } else {
            2
        }
    }

    pub(crate) fn show_queue_image(
        &mut self,
        preview: attachments::PreviewImage,
        cx: &mut Context<Self>,
    ) {
        self.preview = Some(preview);
        self.preview_focus_pending = true;
        cx.notify();
    }

    pub(crate) fn staged_appshots(&self) -> &[CapturedAppshot] {
        self.appshots
            .get(&self.current_key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn stage_appshot(&mut self, appshot: CapturedAppshot, cx: &mut Context<Self>) {
        self.stage_appshot_for(self.current_key.clone(), appshot, cx);
    }

    pub fn stage_appshot_for(
        &mut self,
        key: String,
        appshot: CapturedAppshot,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(ids) = self.draft_unavailable_appshots.remove(&key) {
            if let Some(unavailable) = self.draft_unavailable.get_mut(&key) {
                unavailable.retain(|blob| !ids.contains(&blob.attachment_id));
            }
            self.draft_unavailable_appshot_metadata.remove(&key);
        }
        let screenshot = appshot.screenshot.clone();
        let staged_bytes = self
            .appshots
            .get(&key)
            .into_iter()
            .flatten()
            .map(|shot| shot.screenshot.bytes().len() as u64)
            .sum::<u64>();
        let saved_bytes = self
            .queue_edit_draft
            .as_ref()
            .filter(|_| key == self.current_key)
            .map(|(_, _, shots)| {
                shots
                    .iter()
                    .map(|shot| shot.screenshot.bytes().len() as u64)
                    .sum::<u64>()
            })
            .unwrap_or_default();
        let staged_bytes = staged_bytes.saturating_add(saved_bytes);
        let incoming = appshot.screenshot.bytes().len() as u64;
        if incoming > attachments::MAX_ATTACHMENT_BYTES
            || staged_bytes.saturating_add(incoming) > appshots::MAX_STAGED_APPSHOT_BYTES
        {
            self.failure = Some(
                "Remove an Appshot before adding another (96 MB staged Appshot limit).".into(),
            );
            self.failure_key = Some(key);
            cx.notify();
            return false;
        }
        self.appshot_entrances
            .retain(|_, start| start.elapsed().as_secs_f32() < motion::speed_scale());
        if !motion::reduced_motion(cx) {
            self.appshot_entrances
                .insert(appshot.id.clone(), Instant::now());
        }
        // A capture completing while a queue edit is being saved belongs to
        // the displaced draft; it must not be lost or change the in-flight edit.
        if self.queue_edit_finishing && key == self.current_key {
            if let Some((_, _, saved_appshots)) = &mut self.queue_edit_draft {
                saved_appshots.push(appshot);
            } else {
                self.appshots.entry(key.clone()).or_default().push(appshot);
            }
        } else {
            self.appshots.entry(key.clone()).or_default().push(appshot);
        }
        self.failure = None;
        self.failure_key = None;
        self.stage_draft_blob(key, screenshot, cx);
        cx.notify();
        true
    }

    pub fn show_appshot_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.failure = Some(message.into());
        self.failure_key = Some(self.current_key.clone());
        cx.notify();
    }

    fn add_staged(&mut self, staged: Vec<StagedAttachment>, cx: &mut Context<Self>) {
        if self.queue_edit_finishing {
            return;
        }
        if staged.is_empty() {
            return;
        }
        let key = self.current_key.clone();
        let names = staged
            .iter()
            .map(|attachment| attachment.name.as_str())
            .collect::<HashSet<_>>();
        let unavailable_appshots = self
            .draft_unavailable_appshots
            .get(&key)
            .cloned()
            .unwrap_or_default();
        if let Some(unavailable) = self.draft_unavailable.get_mut(&key) {
            unavailable.retain(|blob| {
                unavailable_appshots.contains(&blob.attachment_id)
                    || !names.contains(blob.name.as_str())
            });
        }
        self.attachments
            .entry(self.current_key.clone())
            .or_default()
            .extend(staged.iter().cloned());
        for attachment in staged {
            self.stage_draft_blob(key.clone(), attachment, cx);
        }
        self.focus_pending = true;
        cx.notify();
    }

    /// Stage image files (picker / drop / pasted paths). Non-images are
    /// skipped silently (matching the original's `image/*` filter); read
    /// failures and oversize files surface in the failure notice.
    pub(crate) fn add_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        // Opt-in evidence (slice 6): which composer received the paths, how
        // many have a known format, and how many staged.
        let perf_scope = crate::perf_counters::enabled().then(|| self.perf_scope(cx));
        let known = paths
            .iter()
            .filter(|path| attachments::format_by_extension(path).is_some())
            .count();
        let mut staged = Vec::new();
        for path in &paths {
            if attachments::format_by_extension(path).is_none() {
                continue;
            }
            match attachments::stage_file(path) {
                Ok(att) => staged.push(att),
                Err(message) => {
                    self.failure = Some(message.into());
                    self.failure_key = Some(self.current_key.clone());
                    cx.notify();
                }
            }
        }
        if let Some(scope) = perf_scope {
            crate::perf_counters::count(
                "composer.add_paths",
                &scope,
                known as u64,
                staged.len() as u64,
            );
            if self.queue_edit_finishing && !staged.is_empty() {
                crate::perf_counters::count("composer.add_staged_blocked", &scope, 1, 0);
            }
        }
        self.add_staged(staged, cx);
    }

    /// Add a file-tree or file-tab drop through the existing file-mention
    /// pipeline. This keeps the reference workspace-relative and therefore
    /// valid for local and remote sessions alike.
    pub(crate) fn add_workspace_path(
        &mut self,
        path: &str,
        is_directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let inserted = self.input.update(cx, |input, cx| {
            input.insert_dropped_mention(path, is_directory, cx)
        });
        if inserted {
            self.reset_mention(None, cx);
            self.reset_slash(None, cx);
            let focus = self.input.read(cx).focus_handle.clone();
            window.focus(&focus, cx);
            cx.notify();
        }
    }

    fn remove_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.queue_edit_finishing {
            return;
        }
        if let Some(list) = self.attachments.get_mut(&self.current_key) {
            list.retain(|a| a.id != id);
            if list.is_empty() {
                self.attachments.remove(&self.current_key);
            }
        }
        if let Some(blobs) = self.draft_blobs.get_mut(&self.current_key) {
            blobs.remove(id);
        }
        self.mark_draft_dirty(cx);
        cx.notify();
    }

    fn restore_failed_appshots(
        &mut self,
        sent: &[CapturedAppshot],
        failed_key: &str,
        restore_key: &str,
    ) {
        if sent.is_empty() {
            return;
        }
        let mut merged = sent.to_vec();
        for key in [failed_key, restore_key] {
            for shot in self.appshots.remove(key).unwrap_or_default() {
                if !merged.iter().any(|existing| existing.id == shot.id) {
                    merged.push(shot);
                }
            }
        }
        self.appshots.insert(restore_key.to_string(), merged);
    }

    fn remove_appshot(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.queue_edit_finishing {
            return;
        }
        let mut screenshot_id = None;
        if let Some(list) = self.appshots.get_mut(&self.current_key) {
            screenshot_id = list
                .iter()
                .find(|appshot| appshot.id == id)
                .map(|appshot| appshot.screenshot.id.clone());
            list.retain(|appshot| appshot.id != id);
            if list.is_empty() {
                self.appshots.remove(&self.current_key);
            }
        }
        if let Some(screenshot_id) = screenshot_id
            && let Some(blobs) = self.draft_blobs.get_mut(&self.current_key)
        {
            blobs.remove(&screenshot_id);
        }
        self.mark_draft_dirty(cx);
        cx.notify();
    }

    /// Drop a deleted chat's per-chat composer state — staged attachments hold
    /// raw image bytes, and a deleted chat's stage could never be sent again.
    pub fn purge_chat(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        self.attachments.remove(chat_id);
        self.input_items.remove(chat_id);
        self.appshots.remove(chat_id);
        self.draft_blobs.remove(chat_id);
        self.draft_unavailable.remove(chat_id);
        self.draft_unavailable_appshots.remove(chat_id);
        self.draft_unavailable_appshot_metadata.remove(chat_id);
        self.state.update(cx, |state, _| {
            state.purge_review_comments(chat_id);
        });
    }

    /// Staged in `AppState` because the changes pane writes them.
    fn staged_comments(&self, cx: &App) -> Vec<crate::comments::ReviewComment> {
        self.state
            .read(cx)
            .review_comments(&self.current_key)
            .to_vec()
    }

    fn render_comments_chip(&self, theme: &Theme, cx: &App) -> Option<gpui::Div> {
        let count = self.staged_comments(cx).len();
        if count == 0 {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_row()
                .px(px(STRIP_PAD_X))
                .pt(px(STRIP_PAD_TOP))
                .child(crate::badges::render(
                    "composer-comments",
                    &crate::badges::MessageBadge {
                        icon: crate::icons::CHAT_ROUND_LINE,
                        label: crate::comments::chip_label(count).into(),
                        // The staged set is already on screen in the changes
                        // pane, so a hover card would only repeat it.
                        details: Vec::new(),
                    },
                    theme,
                )),
        )
    }

    /// The staged-thumbnail strip (attachment-ui.tsx AttachmentStrip):
    /// `flex flex-wrap gap-2 px-4 pt-3`, 56px rounded thumbs, a remove button
    /// revealed on hover, click opens the full-size preview.
    fn render_attachment_strip(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let staged = self.staged();
        if staged.is_empty() {
            return None;
        }
        let mut strip = div()
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(STRIP_GAP))
            .px(px(STRIP_PAD_X))
            .pt(px(STRIP_PAD_TOP));
        for (ix, att) in staged.iter().enumerate() {
            let group: SharedString = format!("composer-att-{}", att.id).into();
            let preview = attachments::PreviewImage::new(att.name.clone(), att.image.clone());
            let remove_id = att.id.clone();
            strip = strip.child(
                div()
                    .group(group.clone())
                    .flex_none()
                    .relative()
                    .child(
                        div()
                            .id(("composer-att-thumb", ix))
                            .size(px(STRIP_THUMB))
                            .rounded(px(8.0))
                            .overflow_hidden()
                            .border_1()
                            .border_color(crate::theme::hairline(0.10))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                preview.viewer.reset();
                                this.preview = Some(preview.clone());
                                this.preview_focus_pending = true;
                                cx.notify();
                            }))
                            .child(
                                img(att.image.clone())
                                    // EXPLICIT dims, not size_full: img layout
                                    // honors the image's intrinsic aspect
                                    // ratio over a percent height (gpui
                                    // f8d8a90 repoint), so size_full let a
                                    // tall photo grow past the frame — the
                                    // rectangular overflow clip then squared
                                    // the bottom corners (2026-08-19 report).
                                    // 56−2 = frame minus its 1px borders.
                                    .w(px(STRIP_THUMB - 2.0))
                                    .h(px(STRIP_THUMB - 2.0))
                                    // Own radii — the frame's rounding only
                                    // clips rectangularly (7 = 8 - border).
                                    .rounded(px(7.0))
                                    .object_fit(ObjectFit::Cover),
                            ),
                    )
                    // Own layer: inside the frosted pill everything shares one
                    // draw order and images render last, so without it the
                    // thumbnail paints OVER this button (user report).
                    .child(crate::frost::layered(
                        div()
                            .id(("composer-att-remove", ix))
                            .absolute()
                            .top(px(-6.0))
                            .right(px(-6.0))
                            .size(px(18.0))
                            .rounded_full()
                            .bg(theme.bg)
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .shadow_sm()
                            .opacity(0.0)
                            .group_hover(group, |s| s.opacity(1.0))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                // The button overhangs the thumbnail, whose
                                // hitbox is right underneath — don't let the
                                // same click also open the preview.
                                cx.stop_propagation();
                                this.remove_attachment(&remove_id, cx);
                            }))
                            .child(
                                crate::icons::icon(crate::icons::CLOSE_CIRCLE)
                                    .size(px(14.0))
                                    .text_color(theme.text_muted),
                            ),
                    )),
            );
        }
        Some(strip)
    }

    fn render_appshot_strip(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let appshots = self.staged_appshots();
        if appshots.is_empty() {
            return None;
        }
        let mut strip = div()
            .id("composer-appshots-strip")
            .flex()
            .flex_row()
            .gap(px(STRIP_GAP))
            .px(px(STRIP_PAD_X))
            .pt(px(STRIP_PAD_TOP))
            .overflow_x_scroll();
        let max_image_width = self.last_available_width.unwrap_or(COMPOSER_MAX_WIDTH)
            - 2.0 * Theme::SPACE_LG
            - 2.0
            - 2.0 * STRIP_PAD_X
            - 2.0 * APPSHOT_IMAGE_INSET;
        for (ix, appshot) in appshots.iter().enumerate() {
            let group: SharedString = format!("composer-appshot-{}", appshot.id).into();
            let preview = crate::attachments::PreviewImage::new(
                appshot.screenshot.name.clone(),
                appshot.screenshot.image.clone(),
            );
            let preview_on_a11y = preview.clone();
            let composer_for_preview = cx.entity().downgrade();
            let remove_id = appshot.id.clone();
            let remove_on_a11y_id = remove_id.clone();
            let composer_for_remove = cx.entity().downgrade();
            let source: SharedString = appshot
                .window_title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| appshot.app_name.clone())
                .into();
            let preview_label: SharedString = format!("Preview {source}").into();
            let remove_label: SharedString = format!("Remove {source}").into();
            let preview_aria = preview_label.clone();
            let remove_aria = remove_label.clone();
            let (image_width, image_height) =
                appshot_contained_size(appshot.screenshot_dimensions, max_image_width);
            let tile_width = (image_width + 2.0 * APPSHOT_IMAGE_INSET).max(APPSHOT_TILE_MIN_WIDTH);
            let mut card = div()
                .id(("composer-appshot", ix))
                .group(group.clone())
                .relative()
                .w(px(tile_width))
                .h(px(APPSHOT_TILE_HEIGHT))
                .flex_none()
                .flex()
                .flex_col()
                .items_center()
                .rounded(px(14.0))
                .overflow_hidden()
                .cursor_pointer()
                .hover(|style| style.bg(crate::theme::ink(0.045)))
                .tooltip(move |_, cx| {
                    cx.new(|_| AppshotActionTooltip(preview_label.clone()))
                        .into()
                })
                .role(gpui::Role::Button)
                .aria_label(preview_aria)
                .tab_index(0)
                .focus_visible(|style| {
                    style
                        .bg(crate::theme::ink(0.06))
                        .border_1()
                        .border_color(theme.accent)
                })
                .on_key_down(crate::swallow_activation_key)
                .on_a11y_action(gpui::AccessibleAction::Click, move |_, _, cx| {
                    composer_for_preview
                        .update(cx, |this, cx| {
                            preview_on_a11y.viewer.reset();
                            this.preview = Some(preview_on_a11y.clone());
                            this.preview_focus_pending = true;
                            cx.notify();
                        })
                        .ok();
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    preview.viewer.reset();
                    this.preview = Some(preview.clone());
                    this.preview_focus_pending = true;
                    cx.notify();
                }))
                .child(
                    div()
                        .id(("composer-appshot-preview", ix))
                        .w(px(tile_width))
                        .h(px(APPSHOT_PREVIEW_HEIGHT))
                        .relative()
                        .flex_none()
                        .flex()
                        .items_end()
                        .justify_center()
                        .overflow_hidden()
                        .rounded(px(12.0))
                        .child(
                            div()
                                .w(px(image_width))
                                .h(px(image_height))
                                .flex_none()
                                .overflow_hidden()
                                .rounded(px(4.0))
                                .shadow_sm()
                                .child(crate::edge_fade::edge_faded(
                                    44.0,
                                    false,
                                    true,
                                    img(appshot.screenshot.image.clone())
                                        .w(px(image_width))
                                        .h(px(image_height))
                                        .object_fit(ObjectFit::Contain),
                                )),
                        ),
                )
                .child(
                    div()
                        .mt(px(20.0))
                        .max_w(px(tile_width - 20.0))
                        .truncate()
                        .text_center()
                        .text_size(crate::typography::ui_rems(12.5))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(source),
                );
            if let Some(icon) = &appshot.app_icon {
                card = card.child(crate::frost::layered(
                    div()
                        .absolute()
                        .top(px(APPSHOT_PREVIEW_HEIGHT - 22.0))
                        .left(px((tile_width - 28.0) / 2.0))
                        .size(px(28.0))
                        .rounded(px(7.0))
                        .bg(theme.bg)
                        .border_1()
                        .border_color(theme.border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            img(icon.clone())
                                .size(px(24.0))
                                .rounded(px(5.0))
                                .object_fit(ObjectFit::Contain),
                        ),
                ));
            }
            card = card.child(crate::frost::layered(
                div()
                    .id(("composer-appshot-remove", ix))
                    .absolute()
                    .top(px(6.0))
                    .right(px(6.0))
                    .size(px(22.0))
                    .rounded_full()
                    .bg(theme.bg.opacity(0.92))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .shadow_sm()
                    .opacity(0.0)
                    .group_hover(group, |style| style.opacity(1.0))
                    .tooltip(move |_, cx| {
                        cx.new(|_| AppshotActionTooltip(remove_label.clone()))
                            .into()
                    })
                    .role(gpui::Role::Button)
                    .aria_label(remove_aria)
                    .tab_index(0)
                    .focus_visible(|style| style.opacity(1.0).border_1().border_color(theme.accent))
                    // A key-down removal let the next appshot inherit this
                    // index's focus and pending key, so key-up removed it too.
                    .on_key_down(crate::swallow_activation_key)
                    .on_a11y_action(gpui::AccessibleAction::Click, move |_, _, cx| {
                        composer_for_remove
                            .update(cx, |this, cx| {
                                this.remove_appshot(&remove_on_a11y_id, cx);
                            })
                            .ok();
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.remove_appshot(&remove_id, cx);
                    }))
                    .child(
                        crate::icons::icon(crate::icons::CLOSE_CIRCLE)
                            .size(px(15.0))
                            .text_color(theme.text_muted),
                    ),
            ));
            // Entity-owned timestamps prevent the entrance replaying on route remount.
            if let Some(start) = self.appshot_entrances.get(&appshot.id) {
                let raw = (start.elapsed().as_secs_f32() / (0.24 * motion::speed_scale()))
                    .clamp(0.0, 1.0);
                if raw < 1.0 && !motion::reduced_motion(cx) {
                    let progress =
                        motion::MotionSpec::new(240, motion::EASE_OUT_EXPO).progress(raw);
                    card = card.opacity(progress).top(px(8.0 * (1.0 - progress)));
                    crate::perf_counters::count("frame.request", "composer#1", 1, 0);
                    window.request_animation_frame();
                }
            }
            strip = strip.child(card);
        }
        Some(strip.into_any_element())
    }

    /// Paperclip: the native image picker (the original's hidden
    /// `<input type=file accept=image/* multiple>`).
    fn open_file_picker(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        self.picker_task = Some(cx.spawn(async move |this, cx| {
            let result = rx.await;
            this.update(cx, |composer, cx| {
                if let Ok(Ok(Some(paths))) = result {
                    composer.add_paths(paths, cx);
                }
                // Both Attach and Cancel return to the draft.
                composer.focus_pending = true;
                cx.notify();
            })
            .ok();
        }));
    }

    fn sync_mention_controls(&mut self, cx: &mut Context<Self>) {
        let open = self.mention.token.is_some()
            || self.slash.token.is_some()
            || self.chat_reference.token.is_some();
        let has_selection = if self.slash.token.is_some() {
            self.slash.active.is_some()
        } else if self.chat_reference.token.is_some() {
            !self.chat_reference.loading && self.chat_reference.active.is_some()
        } else {
            self.mention.active.is_some()
        };
        self.input.update(cx, |input, cx| {
            input.set_mention_controls(open, has_selection, cx)
        });
    }

    /// Tear down the entire completion lifecycle. Advancing the generation is
    /// important even when the spawned task is dropped: an RPC response may
    /// already be queued for delivery on the UI executor.
    fn reset_mention(&mut self, dismissed: Option<(Range<usize>, String)>, cx: &mut Context<Self>) {
        let request = self.mention.request.wrapping_add(1);
        self.mention_task = None;
        self.mention = FileMentionState {
            request,
            dismissed,
            ..FileMentionState::default()
        };
        self.sync_mention_controls(cx);
    }

    fn on_input_edited(&mut self, cx: &mut Context<Self>) {
        self.reconcile_native_selections(cx);
        let (text, cursor) = {
            let input = self.input.read(cx);
            (input.text().to_string(), input.cursor_offset())
        };
        self.update_slash(&text, cursor, cx);
        self.update_chat_references(&text, cursor, cx);
        let token = mention_token(&text, cursor);
        let still_dismissed = token.as_ref().is_some_and(|token| {
            self.mention
                .dismissed
                .as_ref()
                .is_some_and(|(range, value)| {
                    token.range == *range && text.get(range.clone()) == Some(value.as_str())
                })
        });
        if still_dismissed {
            self.mention.token = None;
            self.mention_task = None;
            self.sync_mention_controls(cx);
            cx.notify();
            return;
        }
        self.mention.dismissed = None;
        if token == self.mention.token {
            self.ensure_native_catalog(false, cx);
            self.sync_mention_controls(cx);
            cx.notify();
            return;
        }
        self.mention.request = self.mention.request.wrapping_add(1);
        self.mention_task = None;
        // Refining an open menu keeps the stale rows visible until the new
        // response lands — clearing here made the popup bounce through the
        // skeleton (and a different height) on every keystroke.
        let refining = self.mention.token.is_some() && token.is_some();
        self.mention.token = token.clone();
        if !refining {
            self.mention.results.clear();
            self.mention.active = None;
            // Fresh open: the row stack restarts at the top.
            crate::popover::reset_menu_scroll(&self.mention_scroll, &mut self.popup_bar);
        }
        self.mention.error = None;
        self.mention.loading = token.is_some();
        self.ensure_native_catalog(!refining, cx);
        self.sync_mention_controls(cx);
        let Some(token) = token else {
            cx.notify();
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.mention.loading = false;
            cx.notify();
            return;
        };
        let selected_worktree = match self.pickers.read(cx).checkout_plan() {
            crate::pickers::CheckoutPlan::ReuseWorktree { path, .. } => Some(path),
            _ => None,
        };
        let (params, target) = {
            let state = self.state.read(cx);
            let mut params = serde_json::Map::new();
            params.insert("query".into(), token.query.clone().into());
            let target = if let Some(chat) = self.binding.chat_row(state) {
                params.insert("chatId".into(), chat.id.clone().into());
                Some(chat.device_id.clone())
            } else if let Some(space) = state.selected_space_row() {
                params.insert("spaceId".into(), space.id.clone().into());
                if let Some(path) = selected_worktree {
                    params.insert("path".into(), path.into());
                }
                Some(space.device_id.clone())
            } else {
                None
            };
            if let Some(target) = &target {
                params.insert("targetDeviceId".into(), target.clone().into());
            }
            (serde_json::Value::Object(params), target)
        };
        if target.is_none() {
            self.mention.loading = false;
            cx.notify();
            return;
        }
        let request = self.mention.request;
        self.mention_task = Some(cx.spawn(async move |this, cx| {
            // A short debounce prevents one full workspace walk per keystroke
            // during normal typing. The generation check below still guards
            // requests that were already in flight when the query changed.
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let mut result = engine
                .client()
                .call(methods::SEARCH_FILES, params.clone())
                .await;
            if matches!(result, Err(RpcError::Transport(_)) | Err(RpcError::Closed)) {
                // One retry rides out a cold relay dial to the host device
                // (the diffs pane retries forever; a keystroke-scoped search
                // gets a single second chance).
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                result = engine.client().call(methods::SEARCH_FILES, params).await;
            }
            this.update(cx, |composer, cx| {
                if !mention_response_is_current(&composer.mention, request) {
                    return;
                }
                composer.mention.loading = false;
                match result {
                    Ok(value) => match serde_json::from_value::<Vec<FileSearchMatch>>(value) {
                        Ok(results) => {
                            composer.mention.error = None;
                            composer.mention.active = (!results.is_empty()).then_some(0);
                            composer.mention.results = results;
                            // New result set: the row stack restarts at the top.
                            crate::popover::reset_menu_scroll(
                                &composer.mention_scroll,
                                &mut composer.popup_bar,
                            );
                        }
                        Err(err) => tracing::warn!(%err, "file mention response decode failed"),
                    },
                    Err(err) => {
                        tracing::warn!(%err, "file mention search failed");
                        composer.mention.results.clear();
                        composer.mention.active = None;
                        composer.mention.error = Some(mention_error_message(&err));
                    }
                }
                composer.update_native_rows();
                composer.sync_mention_controls(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn move_mention(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.mention.active =
            crate::popover::menu_step(self.mention.active, self.mention_item_count(), delta);
        if let Some(active) = self.mention.active.filter(|active| {
            *active < self.mention.results.len() + self.mention.native_results.len()
        }) {
            // Keep the keyboard cursor visible in the scrolled row stack.
            // Retry sits in the fixed footer below the list.
            self.mention_scroll.scroll_to_item(active);
        }
        self.sync_mention_controls(cx);
        cx.notify();
    }

    fn dismiss_mention(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.mention.token.as_ref().and_then(|token| {
            self.input
                .read(cx)
                .text()
                .get(token.range.clone())
                .map(|text| (token.range.clone(), text.to_string()))
        });
        self.reset_mention(dismissed, cx);
        cx.notify();
    }

    fn accept_mention(&mut self, cx: &mut Context<Self>) {
        if self.mention.active.is_some() && self.mention.active == self.native_retry_index() {
            self.retry_native_catalog(cx);
            return;
        }
        if let Some(index) = self
            .mention
            .active
            .filter(|index| *index >= self.mention.results.len())
        {
            self.accept_native_mention(index - self.mention.results.len(), cx);
            return;
        }
        let Some(token) = self.mention.token.clone() else {
            return;
        };
        let Some((path, is_dir)) = self
            .mention
            .active
            .and_then(|active| self.mention.results.get(active))
            .map(|result| (result.path.clone(), result.is_dir))
        else {
            return;
        };
        self.input.update(cx, |input, cx| {
            input.replace_mention(token.range, &path, is_dir, cx)
        });
        self.reset_mention(None, cx);
        cx.notify();
    }

    fn render_file_mention_popup(
        &mut self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        self.ensure_native_catalog(false, cx);
        let theme = &theme.for_popup();
        let token = self.mention.token.as_ref()?;
        let mut card = crate::popover::popover_card(theme)
            .w_full()
            .max_h(px(320.0))
            .overflow_hidden()
            // Completion choices belong to the input. Keep it focused until
            // mouse-up can accept a choice (or while dragging the scrollbar).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.input.focus_handle(cx), cx);
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_mention(cx)));
        let file_count = self.mention.results.len();
        // Options only; Retry is a separate button after the listbox.
        let set_size = file_count + self.mention.native_results.len();
        let retry_index = self.native_retry_index();
        if self.mention.loading
            && self.mention.results.is_empty()
            && self.mention.native_results.is_empty()
        {
            card = card.child(crate::popover::skeleton_rows(
                "file-mention-loading",
                theme,
                3,
                cx.entity_id(),
                cx,
            ));
        } else if self.mention.native_results.is_empty()
            && let Some(error) = self.mention.error.clone()
        {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.readable_tone(theme.danger_muted, 1.0))
                    .child(error),
            );
        } else if self.mention.results.is_empty() && self.mention.native_results.is_empty() {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(if token.query.is_empty() {
                        "No files available"
                    } else {
                        "No matching files"
                    }),
            );
        } else {
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(self.mention.results.len());
            for (ix, result) in self.mention.results.iter().enumerate() {
                let selected = self.mention.active == Some(ix);
                let (directory, name) = match result.path.rsplit_once('/') {
                    Some((directory, name)) => (directory.to_string(), name.to_string()),
                    None => (String::new(), result.path.clone()),
                };
                let kind = if result.is_dir { "Folder" } else { "File" };
                rows.push(
                    crate::popover::menu_row(theme, selected, format!("file-mention-result-{ix}"))
                        .id(("file-mention-result", ix))
                        .role(Role::ListBoxOption)
                        .aria_label(format!("{kind} {}", result.path))
                        .aria_selected(selected)
                        .aria_position_in_set(ix + 1)
                        .aria_size_of_set(set_size)
                        .when(selected, |row| row.aria_active_descendant())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mention.active = Some(ix);
                            this.accept_mention(cx);
                        }))
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    crate::file_icons::icon(
                                        if result.is_dir {
                                            crate::file_icons::FileIconIdentity::directory(
                                                &result.path,
                                                false,
                                            )
                                        } else {
                                            crate::file_icons::FileIconIdentity::file(&result.path)
                                        },
                                        theme.appearance,
                                    )
                                    .size(px(14.0))
                                    .flex_none(),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(crate::typography::ui_rems(13.0))
                                        .text_color(theme.text)
                                        .child(name),
                                )
                                .when(!directory.is_empty(), |row| {
                                    row.child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .overflow_hidden()
                                            .truncate()
                                            .text_size(crate::typography::ui_rems(12.5))
                                            .text_color(theme.text_muted)
                                            .child(directory),
                                    )
                                }),
                        )
                        .into_any_element(),
                );
            }
            // Overflowing rows wheel-scroll inside a bounded viewport; the
            // floating rail mirrors the model-list scrollbar treatment.
            for (index, result) in self.mention.native_results.iter().enumerate() {
                let ix = file_count + index;
                let selected = self.mention.active == Some(ix);
                let Some(semantics) = self.native_row_semantics(index) else {
                    continue;
                };
                let enabled = semantics.enabled;
                rows.push(
                    crate::popover::menu_row(
                        theme,
                        selected,
                        format!("native-mention-result-{ix}"),
                    )
                    .id(("native-mention-result", ix))
                    .role(Role::ListBoxOption)
                    .aria_label(semantics.label)
                    .aria_description(semantics.description)
                    .aria_selected(selected)
                    .aria_position_in_set(ix + 1)
                    .aria_size_of_set(set_size)
                    .when(selected, |row| row.aria_active_descendant())
                    // Disabled rows expose no Click action and report Disabled.
                    .when(enabled, |row| {
                        row.on_click(cx.listener(move |this, _, _, cx| {
                            this.mention.active = Some(ix);
                            this.accept_mention(cx);
                        }))
                    })
                    .when(!enabled, |row| {
                        row.cursor_default()
                            .opacity(0.55)
                            .a11y_synthetic_children(|builder| builder.parent_node().set_disabled())
                    })
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                crate::icons::icon(crate::icons::MAGIC_WAND_3)
                                    .size(px(14.0))
                                    .text_color(theme.readable_icon(theme.warning)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .text_color(theme.text)
                                    .child(result.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.text_muted)
                                    .child(integrations::kind_label(result.kind)),
                            ),
                    )
                    .child(
                        div()
                            .pl(px(22.0))
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child(
                                result
                                    .description
                                    .clone()
                                    .unwrap_or_else(|| result.id.clone()),
                            ),
                    )
                    .into_any_element(),
                );
            }
            card = card.child(
                div()
                    .id("mention-scroll-host")
                    .relative()
                    .on_hover(cx.listener(Self::on_popup_list_hover))
                    .child(
                        div()
                            .id("mention-list")
                            .role(Role::ListBox)
                            .aria_label("Mention suggestions")
                            .max_h(px(312.0))
                            .flex()
                            .flex_col()
                            .gap(px(crate::popover::MENU_GAP))
                            .overflow_y_scroll()
                            .track_scroll(&self.mention_scroll)
                            .children(rows),
                    )
                    .children(crate::popover::rail(self, "mention-scrollbar", theme, cx)),
            );
        }
        if let (Some(notice), Some(retry_index)) = (self.native_catalog_notice(), retry_index) {
            // Status footer: keyboard order (rows, then Retry) matches the
            // visual order, and Retry is reachable by arrows, Tab and AT.
            let retry_active = self.mention.active == Some(retry_index);
            let retry_focus = self.retry_focus_handle(cx);
            // Active descendant is for arrow navigation from the input; a
            // Retry that holds real focus is already the focused node.
            let retry_descendant = retry_active && !retry_focus.is_focused(window);
            let accent = theme.accent;
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .px(px(4.0))
                    .pt(px(6.0))
                    .child(
                        div()
                            .id("native-catalog-status")
                            .role(Role::Status)
                            .aria_label(notice.clone())
                            .px(px(8.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(notice.clone()),
                    )
                    .child(
                        crate::popover::menu_row(theme, retry_active, "retry-native-catalog")
                            .id("retry-native-catalog")
                            .role(Role::Button)
                            .aria_label("Retry native integrations discovery")
                            .aria_description(notice)
                            .aria_keyshortcuts("Enter")
                            .when(retry_descendant, |row| row.aria_active_descendant())
                            .track_focus(&retry_focus)
                            .tab_index(0)
                            .focus_visible(move |style| {
                                style.bg(accent.opacity(0.18)).text_color(accent)
                            })
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.readable_tone(theme.accent, 1.0))
                            .child("Retry integrations")
                            .on_click(cx.listener(|this, _, _, cx| this.retry_native_catalog(cx))),
                    ),
            );
        }
        Some(crate::popover::full_width_menu_above(
            "file-mention-popup",
            card.into_any_element(),
            None,
        ))
    }

    fn render_input_with_completion(&self) -> gpui::Div {
        div().relative().child(self.input.clone())
    }

    // ---- slash commands ---------------------------------------------------

    /// Track the `/` token on every edit: open/refresh the popup, fetch the
    /// harness's command list on first open, filter locally per keystroke.
    fn update_slash(&mut self, text: &str, cursor: usize, cx: &mut Context<Self>) {
        let token = slash_token(text, cursor);
        let still_dismissed = token.as_ref().is_some_and(|token| {
            self.slash.dismissed.as_ref().is_some_and(|(range, value)| {
                token.range == *range && text.get(range.clone()) == Some(value.as_str())
            })
        });
        if still_dismissed {
            self.slash.token = None;
            self.sync_mention_controls(cx);
            return;
        }
        self.slash.dismissed = None;
        let resolved = self.pickers.read(cx).resolved(cx);
        let harness = resolved.harness;
        let (scope_params, scope) = {
            let state = self.state.read(cx);
            let mode = resolved.effective_permission_mode;
            match (self.binding.chat_row(state), state.selected_space_row()) {
                (Some(chat), _) => (
                    serde_json::json!({ "chatId": chat.id, "permissionMode": mode }),
                    format!("chat:{}:{mode:?}", chat.id),
                ),
                (None, Some(space)) => (
                    serde_json::json!({ "spaceId": space.id, "permissionMode": mode }),
                    format!("space:{}:{mode:?}", space.id),
                ),
                (None, None) => (
                    serde_json::json!({ "permissionMode": mode }),
                    format!("none:{mode:?}"),
                ),
            }
        };
        let harness_changed = self.slash.harness != harness || self.slash.scope != scope;
        if token == self.slash.token && !harness_changed {
            self.refilter_slash(cx);
            return;
        }
        let opening = self.slash.token.is_none() || harness_changed;
        self.slash.token = token.clone();
        self.slash.harness = harness;
        self.slash.scope = scope.clone();
        self.slash.error = None;
        if token.is_none() {
            self.slash.active = None;
            self.sync_mention_controls(cx);
            return;
        }
        // No resolved harness (catalog still loading): empty popup, no fetch.
        let Some(harness) = harness else {
            self.slash.loading = false;
            self.refilter_slash(cx);
            return;
        };
        let cached = self.slash_cache.contains_key(&(harness, scope.clone()));
        if cached {
            self.slash.loading = false;
            self.refilter_slash(cx);
            // Revalidate once per popup open, showing the cached list
            // meanwhile: the engine re-discovers only after the provider's
            // settings/commands/skills changed.
            if !opening {
                return;
            }
        }
        // First open for this harness: one ListCommands, targeted like file
        // search (the chat/space host device owns the agent binary).
        self.slash.request = self.slash.request.wrapping_add(1);
        self.slash.loading = !cached;
        self.refilter_slash(cx);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.slash.loading = false;
            return;
        };
        let target = {
            let state = self.state.read(cx);
            self.binding
                .chat_row(state)
                .map(|chat| chat.device_id.clone())
                .or_else(|| state.selected_space_row().map(|s| s.device_id.clone()))
        };
        let request = self.slash.request;
        self.slash_task = Some(cx.spawn(async move |this, cx| {
            let mut params = scope_params;
            if let Some(object) = params.as_object_mut() {
                object.insert("harness".into(), serde_json::json!(harness));
                if let Some(target) = &target {
                    object.insert("targetDeviceId".into(), target.clone().into());
                }
            }
            let result = engine.client().call(methods::LIST_COMMANDS, params).await;
            this.update(cx, |composer, cx| {
                if composer.slash.request != request {
                    return;
                }
                composer.slash.loading = false;
                match result {
                    Ok(value) => match serde_json::from_value::<Vec<SlashCommand>>(value) {
                        Ok(commands) => {
                            composer.slash_cache.insert((harness, scope), commands);
                        }
                        Err(err) => tracing::warn!(%err, "slash command decode failed"),
                    },
                    Err(err) => {
                        tracing::debug!(%err, "slash command discovery failed");
                        // A failed revalidation keeps the last good list.
                        if !composer.slash_cache.contains_key(&(harness, scope.clone())) {
                            composer.slash.error = Some(slash_error_message(&err));
                        }
                    }
                }
                composer.refilter_slash(cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// The popup's command list: LUNA's own `/goal` first on Codex chats, then
    /// the harness's cached commands (minus any that `/goal` shadows).
    fn slash_commands(&self, cx: &App) -> Vec<SlashCommand> {
        let cached = self
            .slash
            .harness
            .and_then(|h| self.slash_cache.get(&(h, self.slash.scope.clone())))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let goal = self
            .slash
            .harness
            .and_then(goals::GoalProvider::of)
            .filter(|_| self.goal_command_available(cx));
        let mut commands = Vec::with_capacity(cached.len() + usize::from(goal.is_some()));
        if let Some(provider) = goal {
            commands.push(goals::goal_slash_command(provider));
        }
        commands.extend(
            cached
                .iter()
                .filter(|command| goal.is_none() || command.name != goals::GOAL_COMMAND)
                .cloned(),
        );
        commands
    }

    /// Re-rank the cached list for the current query (pure local filter).
    fn refilter_slash(&mut self, cx: &mut Context<Self>) {
        let query = self
            .slash
            .token
            .as_ref()
            .map(|t| t.query.clone())
            .unwrap_or_default();
        let commands = self.slash_commands(cx);
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        self.slash.filtered = crate::popover::filter_indices(&query, &names);
        self.slash.active = (!self.slash.filtered.is_empty()).then_some(0);
        // A fresh query/reopen restarts the row stack at the top.
        crate::popover::reset_menu_scroll(&self.slash_scroll, &mut self.popup_bar);
        self.sync_mention_controls(cx);
        cx.notify();
    }

    fn move_slash(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.slash.active =
            crate::popover::menu_step(self.slash.active, self.slash.filtered.len(), delta);
        if let Some(active) = self.slash.active {
            // Keep the keyboard cursor visible in the scrolled row stack.
            self.slash_scroll.scroll_to_item(active);
        }
        self.sync_mention_controls(cx);
        cx.notify();
    }

    fn dismiss_slash(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.slash.token.as_ref().and_then(|token| {
            self.input
                .read(cx)
                .text()
                .get(token.range.clone())
                .map(|text| (token.range.clone(), text.to_string()))
        });
        self.reset_slash(dismissed, cx);
        cx.notify();
    }

    fn accept_slash(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.slash.token.clone() else {
            return;
        };
        let Some(command) = self
            .slash
            .active
            .and_then(|active| self.slash.filtered.get(active))
            .and_then(|&ix| self.slash_commands(cx).get(ix).cloned())
        else {
            return;
        };
        self.input.update(cx, |input, cx| {
            input.replace_plain_token(token.range, &format!("/{}", command.name), cx)
        });
        self.reset_slash(None, cx);
        cx.notify();
    }

    /// Tear down the slash completion (mirrors [`Self::reset_mention`]).
    fn reset_slash(&mut self, dismissed: Option<(Range<usize>, String)>, cx: &mut Context<Self>) {
        let request = self.slash.request.wrapping_add(1);
        self.slash_task = None;
        self.slash = SlashState {
            request,
            dismissed,
            harness: self.slash.harness,
            scope: self.slash.scope.clone(),
            ..SlashState::default()
        };
        self.sync_mention_controls(cx);
    }

    fn render_slash_popup(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = &theme.for_popup();
        // Only while a slash token is active.
        self.slash.token.as_ref()?;
        let commands = self.slash_commands(cx);
        // Full pill width at the mention card's height budget — both composer
        // completions share the same surface shape.
        let mut card = crate::popover::popover_card(theme)
            .w_full()
            .max_h(px(320.0))
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.input.focus_handle(cx), cx);
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_slash(cx)));
        if self.slash.loading && commands.is_empty() {
            card = card.child(crate::popover::skeleton_rows(
                "slash-loading",
                theme,
                3,
                cx.entity_id(),
                cx,
            ));
        } else if let Some(error) = self
            .slash
            .error
            .clone()
            .filter(|_| self.slash.filtered.is_empty())
        {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.readable_tone(theme.danger_muted, 1.0))
                    .child(error),
            );
        } else if self.slash.filtered.is_empty() {
            card = card.child(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(if commands.is_empty() {
                        "This agent has no slash commands"
                    } else {
                        "No matching commands"
                    }),
            );
        } else {
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(self.slash.filtered.len());
            for (row_ix, &cmd_ix) in self.slash.filtered.iter().enumerate() {
                let Some(command) = commands.get(cmd_ix) else {
                    continue;
                };
                let selected = self.slash.active == Some(row_ix);
                let name: SharedString = format!("/{}", command.name).into();
                let mut description = command.description.clone();
                if let Some(hint) = &command.input_hint {
                    if description.is_empty() {
                        description = format!("<{hint}>");
                    } else {
                        description = format!("{description} · <{hint}>");
                    }
                }
                let description: SharedString = description.into();
                rows.push(
                    crate::popover::menu_row(theme, selected, format!("slash-result-{row_ix}"))
                        .id(("slash-result", row_ix))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.slash.active = Some(row_ix);
                            this.accept_slash(cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    crate::icons::icon(if command.name == goals::GOAL_COMMAND {
                                        crate::icons::TARGET
                                    } else {
                                        crate::icons::COMMAND
                                    })
                                    .size(px(14.0))
                                    .text_color(theme.text_muted),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(crate::typography::ui_rems(12.5))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.text)
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .overflow_hidden()
                                        .truncate()
                                        .text_size(crate::typography::ui_rems(12.0))
                                        .text_color(theme.text_muted)
                                        .child(description),
                                ),
                        )
                        .into_any_element(),
                );
            }
            // Overflowing rows wheel-scroll inside a bounded viewport; the
            // floating rail mirrors the model-list scrollbar treatment.
            card = card.child(
                div()
                    .id("slash-scroll-host")
                    .relative()
                    .on_hover(cx.listener(Self::on_popup_list_hover))
                    .child(
                        div()
                            .id("slash-list")
                            .max_h(px(312.0))
                            .flex()
                            .flex_col()
                            .gap(px(crate::popover::MENU_GAP))
                            .overflow_y_scroll()
                            .track_scroll(&self.slash_scroll)
                            .children(rows),
                    )
                    .children(crate::popover::rail(self, "slash-scrollbar", theme, cx)),
            );
            // LUNA's own `/goal` still lists when the agent's discovery fails.
            if let Some(error) = self.slash.error.clone() {
                card = card.child(
                    div()
                        .px(px(12.0))
                        .py(px(8.0))
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.readable_tone(theme.danger_muted, 1.0))
                        .child(error),
                );
            }
        }
        // Full pill width above the composer, matching the file-mention popup.
        Some(crate::popover::full_width_menu_above(
            "slash-popup",
            card.into_any_element(),
            None,
        ))
    }

    /// The popup whose rows a scrollbar drag is moving — the tokens are
    /// mutually exclusive, so at most one exists.
    fn active_popup_scroll(&self) -> Option<gpui::ScrollHandle> {
        if self.slash.token.is_some() {
            Some(self.slash_scroll.clone())
        } else if self.mention.token.is_some() {
            Some(self.mention_scroll.clone())
        } else if self.chat_reference.token.is_some() {
            Some(self.chat_reference.scroll.clone())
        } else {
            None
        }
    }

    fn on_popup_list_hover(
        &mut self,
        hovered: &bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.popup_bar.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    /// Counter scope (slice 6): `main:<chat>` for Main's composer,
    /// `pane:<chat>` for a tab's.
    fn perf_scope(&self, cx: &gpui::App) -> String {
        match &self.binding {
            crate::chat_binding::ChatBinding::Chat(chat) => format!("pane:{chat}"),
            crate::chat_binding::ChatBinding::Selected => format!(
                "main:{}",
                self.state.read(cx).selected_chat.as_deref().unwrap_or("-")
            ),
        }
    }

    fn on_state_changed(&mut self, cx: &mut Context<Self>) {
        self.refresh_chat_reference_source(cx);
        // Opt-in baseline counters (slice 6): observe only.
        let _perf = crate::perf_counters::Timer::start("composer.observe", || self.perf_scope(cx));
        self.sync_draft_engine_generation(cx);
        self.sync_review_comment_draft(cx);
        {
            let state = self.state.read(cx);
            let now = chrono::Utc::now();
            retain_live_interrupts(&mut self.interrupting, |chat_id| {
                matches!(
                    state.indicator_for(chat_id, now),
                    Indicator::Working | Indicator::AwaitingApproval | Indicator::AwaitingInput
                )
            });
            let queue = self.binding.queue(state);
            self.queue_removing
                .retain(|id| queue.iter().any(|item| item.id == *id));
        }
        self.interrupt_tasks
            .retain(|chat_id, _| self.interrupting.contains(chat_id));

        let editing_id = self.editing_queued.clone();
        let (key, pending, edited_row_exists, scan_key) = {
            let s = self.state.read(cx);
            let transcript = self.binding.transcript(s);
            let queue = self.binding.queue(s);
            let key = self.binding.key(s);
            // Rescan the history only when this binding's content changed.
            let scan_key = self
                .binding
                .transcript_token(s)
                .map(|token| (key.clone(), token));
            let hidden = matches!(self.binding, crate::chat_binding::ChatBinding::Chat(_))
                && !self
                    .last_painted
                    .is_some_and(|at| at.elapsed() < Duration::from_millis(500));
            let cached = self
                .pending_scan_cache
                .as_ref()
                .filter(|(cached, _)| {
                    Some(cached) == scan_key.as_ref()
                        // Hidden: keep the last result for this same binding
                        // until the next render rescans.
                        || (hidden && scan_key.as_ref().is_some_and(|k| k.0 == cached.0))
                })
                .map(|(_, pending)| pending.clone());
            if hidden && cached.is_some() {
                self.pending_scan_deferred = true;
            }
            let pending = cached.unwrap_or_else(|| {
                let mut scan = crate::perf_counters::Timer::start("composer.pending_scan", || {
                    self.perf_scope(cx)
                });
                if let Some(scan) = scan.as_mut() {
                    scan.items(transcript.len() as u64);
                }
                pending_input_request(transcript)
                    .map(|(id, questions)| Wizard::new(id, questions))
                    .or_else(|| {
                        pending_async_question(transcript)
                            .map(|(id, questions)| Wizard::new_async(id, questions))
                    })
            });
            (
                key,
                pending,
                editing_id
                    .as_ref()
                    .is_none_or(|id| queue.iter().any(|item| item.id == *id)),
                scan_key,
            )
        };
        if !self.pending_scan_deferred {
            self.pending_scan_cache = scan_key.map(|scan_key| (scan_key, pending.clone()));
        }
        self.observe_pending_draft_owner(&key, cx);

        // A queue edit belongs to exactly one visible row. Navigation or a
        // remote drain/removal cancels it instead of leaving a focused but
        // unmounted editor entity behind.
        if key != self.current_key && self.editing_queued.is_some() {
            self.clear_queue_edit(cx);
        } else if !edited_row_exists && self.editing_queued.is_some() && !self.queue_edit_finishing
        {
            self.failure =
                Some("The queued message was removed; your edit remains in the composer".into());
            // Recover both drafts when another device removes the reserved row.
            if let Some((draft, mut attachments, mut appshots)) = self.queue_edit_draft.take() {
                // Both halves of the joined text keep their typed identities:
                // the private draft's envelope, then the edit's live anchors.
                let mut items = self.queue_edit_input_items.take().unwrap_or_default();
                for item in self.input.read(cx).native_input_items() {
                    if !items.contains(&item) {
                        items.push(item);
                    }
                }
                self.input_items.insert(self.current_key.clone(), items);
                appshots.extend(self.appshots.remove(&self.current_key).unwrap_or_default());
                self.appshots.insert(self.current_key.clone(), appshots);
                let edited = self.input.read(cx).text().to_string();
                let text = [draft, edited]
                    .into_iter()
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                self.input.update(cx, |input, cx| input.set_text(text, cx));
                attachments.extend(
                    self.attachments
                        .remove(&self.current_key)
                        .unwrap_or_default(),
                );
                self.attachments
                    .insert(self.current_key.clone(), attachments);
                let restored_key = self.current_key.clone();
                self.trusted_native_envelope_restored(&restored_key, cx);
            }
            self.clear_queue_edit(cx);
        }

        // Draft swap on chat navigation — the input entity itself survives.
        if key != self.current_key {
            let new_thread_launch =
                self.launching_new_chat && self.current_key.is_empty() && !key.is_empty();
            let returning_to_new_thread = !self.current_key.is_empty() && key.is_empty();
            self.launching_new_chat = false;
            let old_text = self.input.read(cx).text().to_string();
            if old_text.is_empty() {
                self.drafts.remove(&self.current_key);
            } else {
                self.drafts.insert(self.current_key.clone(), old_text);
            }
            self.schedule_draft_save(self.current_key.clone(), Duration::ZERO, cx);
            let draft = self.drafts.get(&key).cloned().unwrap_or_default();
            // Keep the leaving draft's exact native anchors for navigation back.
            self.park_native_editor(cx);
            self.current_key = key;
            // `failure` deliberately survives navigation: chat-scoped
            // failures render only under their own chat (see `failure_key`),
            // so switching away and back must not erase the one visible
            // trace of a failed send.
            self.wizard = None;
            self.answer_input
                .update(cx, |input, cx| input.set_text("", cx));
            // Attachments stay stashed under their chat key (the map swap IS
            // the navigation); only the transient chrome resets.
            self.preview = None;
            self.reset_mention(None, cx);
            self.reset_chat_references(None, cx);
            // Route changes snap (round 5/6): a height difference between
            // the old and new session's composer must not glide across
            // navigation. The swapped draft is re-measured one or two
            // renders later, so the whole window snaps (see ROUTE_SNAP_MS).
            self.height_morph = None;
            self.last_target_height = 0.0;
            if (new_thread_launch || returning_to_new_thread)
                && !motion::reduced_motion(cx)
                && self.last_rendered_height > 0.0
            {
                // Both directions share one timeline between the blank
                // canvas's taller pill and the session's two-row pill.
                let now_ms =
                    self.morph_clock.elapsed().as_secs_f32() * 1000.0 / motion::speed_scale();
                self.flip_morph = Some(FlipMorph::new_thread_transition(
                    self.last_rendered_height,
                    now_ms,
                ));
                self.route_snap_until = None;
            } else {
                self.flip_morph = None;
                self.last_rendered_height = 0.0;
                self.route_snap_until = Some(Instant::now() + Duration::from_millis(ROUTE_SNAP_MS));
            }
            self.set_input_text_without_draft_edit(draft, cx);
            // Bind the new owner now: exact parked anchors or the draft's
            // trusted typed envelope return; raw link text never does.
            self.restore_native_after_navigation(cx);
        }

        self.load_draft(self.current_key.clone(), cx);
        self.finish_pending_draft_owner_stage(cx);

        let transcript = self.binding.transcript(self.state.read(cx)).to_vec();
        let settled: Vec<_> = self
            .async_submissions
            .keys()
            .filter(|(chat, id)| {
                chat == &self.current_key && async_question_resolved(&transcript, id)
            })
            .cloned()
            .collect();
        for key in settled {
            self.clear_async_delivery_failure(&key);
            self.async_submissions.remove(&key);
            self.async_attempts.remove(&key);
            self.answered_requests.remove(&key.1);
        }
        // A pending agent question must not take over an active queue edit.
        // Its durable resolution still settles any delivery banner first.
        if self.editing_queued.is_some() {
            cx.notify();
            return;
        }
        // Question card lifecycle; the regular composer keeps its own draft.
        match pending.as_ref() {
            Some(candidate)
                if candidate.source == QuestionSource::Async
                    || !self.answered_requests.contains(&candidate.request_id) =>
            {
                let same = self.wizard.as_ref().is_some_and(|w| {
                    w.request_id == candidate.request_id && w.source == candidate.source
                });
                if !same {
                    self.wizard = Some(candidate.clone());
                    self.advance_task = None;
                    let initial = candidate.current_typed().to_string();
                    self.answer_input
                        .update(cx, |input, cx| input.set_text(initial, cx));
                }
            }
            _ => {
                if let Some(wizard) = self.wizard.clone() {
                    let transcript = self.binding.transcript(self.state.read(cx)).to_vec();
                    let released = match wizard.source {
                        QuestionSource::Blocking => {
                            input_request_resolved(&transcript, &wizard.request_id)
                                || (!transcript.is_empty()
                                    && !self.answered_requests.contains(&wizard.request_id))
                        }
                        QuestionSource::Async => {
                            async_question_resolved(&transcript, &wizard.request_id)
                                || pending.as_ref().is_some_and(|candidate| {
                                    candidate.request_id != wizard.request_id
                                })
                        }
                    };
                    if released {
                        if wizard.source == QuestionSource::Async {
                            let key = (self.current_key.clone(), wizard.request_id.clone());
                            self.clear_async_delivery_failure(&key);
                            self.async_submissions.remove(&key);
                            self.async_attempts.remove(&key);
                            self.answered_requests.remove(&wizard.request_id);
                        }
                        self.wizard = None;
                        self.advance_task = None;
                        self.answer_input
                            .update(cx, |input, cx| input.set_text("", cx));
                    }
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn run_live(&self, cx: &App) -> bool {
        let s = self.state.read(cx);
        let Some(chat_id) = self.binding.chat_id(s) else {
            return false;
        };
        matches!(
            s.indicator_for(chat_id, chrono::Utc::now()),
            Indicator::Working | Indicator::AwaitingApproval | Indicator::AwaitingInput
        )
    }

    /// New sessions require a selected project and a runnable agent. Existing
    /// sessions carry their own run config.
    fn send_blocked(&self, cx: &App) -> bool {
        if self.queue_edit_finishing {
            return true;
        }
        let state = self.state.read(cx);
        if state.review_comment_flush_pending(&self.current_key) {
            return true;
        }
        if self.binding.chat_id(state).is_some() {
            return false;
        }
        if state.selected_space_row().is_none() {
            return true;
        }
        // New-session canvas: needs a runnable agent. The
        // no-agents check only fires once the catalog is loaded — offline
        // and still-loading states must not block (the harness resolves from
        // the remembered default and the engine reports real failures).
        self.pickers.read(cx).no_agents_available()
    }

    fn require_project_for_new_session(&mut self, cx: &mut Context<Self>) -> bool {
        let state = self.state.read(cx);
        let missing_project =
            self.binding.chat_id(state).is_none() && state.selected_space_row().is_none();
        if missing_project {
            self.failure = Some("Select a project before starting a session.".into());
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
        }
        !missing_project
    }

    fn context_send_block_reason(&self, cx: &App) -> Option<&'static str> {
        match self
            .binding
            .context_control(self.state.read(cx))
            .compact
            .status
        {
            luna_proto::ContextOperationStatus::Running => {
                Some("Context compaction is still running. Your draft was not changed or sent.")
            }
            luna_proto::ContextOperationStatus::Unknown => Some(
                "Context compaction completion is unresolved. Your draft was not changed or sent; clear context before sending again.",
            ),
            luna_proto::ContextOperationStatus::Idle
            | luna_proto::ContextOperationStatus::Completed
            | luna_proto::ContextOperationStatus::Failed => None,
        }
    }

    fn button_mode(&self, cx: &App) -> SendButtonMode {
        if self.editing_queued.is_some() {
            return SendButtonMode::Send;
        }
        let has_text = composer_has_content(
            self.input.read(cx).text(),
            self.staged().len()
                + self.staged_appshots().len()
                + self.input_items.get(&self.current_key).map_or(0, Vec::len),
            self.staged_comments(cx).len(),
        );
        send_button_mode(self.run_live(cx), has_text)
    }

    fn on_submit(&mut self, cx: &mut Context<Self>) {
        if self.commit_queue_edit(cx) {
            return;
        }
        let text = self.input.read(cx).text().trim().to_string();
        let no_content = !composer_has_content(
            &text,
            self.staged().len()
                + self.staged_appshots().len()
                + self.input_items.get(&self.current_key).map_or(0, Vec::len),
            self.staged_comments(cx).len(),
        );
        match self.button_mode(cx) {
            // Enter never stops a run: Stop mode implies an empty composer,
            // so a stray extra Enter right after sending landed an interrupt
            // on the just-dispatched prompt and the agent ate it silently
            // (issue #406). Stop stays on the button — and on Esc when
            // escape_stops_active_agent is enabled.
            SendButtonMode::Stop => {}
            _ if no_content => {}
            _ if !self.require_project_for_new_session(cx) => {}
            _ if self.send_blocked(cx) => {}
            SendButtonMode::Send => self.send(text, None, cx),
            // Busy: keep the message queued until the current turn ends.
            SendButtonMode::Queue => {
                let behavior = crate::settings::current(cx).follow_up_behavior;
                self.send(text, Some(behavior), cx)
            }
        }
    }

    /// Cmd/Ctrl+Enter is the explicit empty-composer queue shortcut. Keep it
    /// distinct from plain Enter so issue #406's no-op/never-stop boundary
    /// remains intact. With content it is the ordinary configured submit.
    fn on_modifier_submit(&mut self, cx: &mut Context<Self>) {
        let no_content = !composer_has_content(
            self.input.read(cx).text(),
            self.staged().len()
                + self.staged_appshots().len()
                + self.input_items.get(&self.current_key).map_or(0, Vec::len),
            self.staged_comments(cx).len(),
        );
        if no_content && self.editing_queued.is_none() {
            self.activate_latest_queued_primary(cx);
        } else {
            self.on_submit(cx);
        }
    }

    /// Cmd/Ctrl+Shift+Enter performs the opposite active-turn follow-up once
    /// without changing the persisted preference. Outside a live run it is
    /// just a normal submit, and an empty chord never interrupts.
    fn on_opposite_submit(&mut self, cx: &mut Context<Self>) {
        if self.commit_queue_edit(cx) {
            return;
        }
        let text = self.input.read(cx).text().trim().to_string();
        let has_content = composer_has_content(
            &text,
            self.staged().len()
                + self.staged_appshots().len()
                + self.input_items.get(&self.current_key).map_or(0, Vec::len),
            self.staged_comments(cx).len(),
        );
        if !has_content || !self.require_project_for_new_session(cx) || self.send_blocked(cx) {
            return;
        }
        if self.run_live(cx) {
            let behavior = crate::settings::current(cx).follow_up_behavior.opposite();
            self.send(text, Some(behavior), cx);
        } else {
            self.on_submit(cx);
        }
    }

    /// Queue a Run doc command with an optimistic echo — or, with the agent
    /// busy, park the message on the chat's pending queue instead. New chats
    /// thread the picked config in: worktree creation (when the isolated toggle
    /// is on), `Mutate createChat` with the `ChatConfig` + cwd, and the model /
    /// reasoning / options on the Run request itself (§1.7).
    fn send(
        &mut self,
        text: String,
        follow_up_behavior: Option<luna_proto::FollowUpBehavior>,
        cx: &mut Context<Self>,
    ) {
        // Goal mode or `/goal <objective>` starts a Codex goal: the engine
        // runs Codex's own goal start on a loaded runtime, so work begins
        // immediately. A goal is never queued behind the current turn.
        let (text, as_goal) = match self.goal_send(&text, cx) {
            goals::GoalSend::Message => (text, false),
            goals::GoalSend::Consumed => return,
            goals::GoalSend::Goal(objective) => (objective, true),
        };
        let follow_up_behavior = follow_up_behavior.filter(|_| !as_goal);
        if self.defer_native_send(&text, follow_up_behavior, cx) {
            return;
        }
        if !self.require_project_for_new_session(cx) {
            return;
        }
        if let Some(reason) = self.context_send_block_reason(cx) {
            self.failure = Some(reason.into());
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
            return;
        }
        let queue = follow_up_behavior.is_some();
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.failure = Some("Engine not connected".into());
            self.failure_key = None; // global — meaningful on every chat
            cx.notify();
            return;
        };
        if let Some(reason) = self.pickers.read(cx).permission_run_block_reason(cx) {
            self.failure = Some(reason.into());
            self.failure_key = None;
            cx.notify();
            return;
        }
        // Chat id: existing selection, or client-minted for the new-chat canvas
        // (the chat then appears from the doc host once the doc materializes).
        let (chat_id, is_new) = match self.binding.chat_id(self.state.read(cx)) {
            Some(id) => (id.to_string(), false),
            None => (uuid::Uuid::new_v4().to_string(), true),
        };
        // Where the new session runs (Current checkout / reuse an existing
        // worktree / fresh worktree off the picked base) — resolved NOW so
        // the async block needs no picker access.
        let plan = self.pickers.read(cx).checkout_plan();
        // Fully-resolved model/reasoning/options — concrete values (chat config
        // or defaults), so the engine never has to guess a "default".
        let resolved = self.pickers.read(cx).resolved(cx);
        let existing_cwd = self
            .binding
            .chat_row(self.state.read(cx))
            .and_then(|c| c.cwd.clone());
        // The project fixes the new session's device and base folder.
        let space = self.state.read(cx).selected_space_row().cloned();
        let local_device_id = self.state.read(cx).local_device_id.clone();
        let target_device_id = self.state.read(cx).effective_device_id();
        let device_id = if is_new {
            target_device_id
                .clone()
                .unwrap_or_else(|| "local".to_string())
        } else {
            self.binding
                .chat_row(self.state.read(cx))
                .map(|c| c.device_id.clone())
                .or_else(|| local_device_id.clone())
                .unwrap_or_else(|| "local".to_string())
        };
        // Uploads/read-backs target the chat's HOST device (forwardable RPCs);
        // for a new chat that's the target device (None when it's local).
        let host_device_id = if is_new {
            target_device_id
                .clone()
                .filter(|id| local_device_id.as_deref() != Some(id.as_str()))
        } else {
            self.binding
                .chat_row(self.state.read(cx))
                .map(|c| c.device_id.clone())
        };
        let space_id = space.as_ref().map(|s| s.id.clone());
        let space_path = space.as_ref().map(|s| s.path.clone());
        let may_have_setup = is_new
            && space_path.is_some()
            && matches!(&plan, crate::pickers::CheckoutPlan::NewWorktree { .. });
        if queue && !is_new {
            let capability = if self.staged().is_empty() && self.staged_appshots().is_empty() {
                capabilities::MESSAGE_QUEUE_V1
            } else {
                capabilities::MESSAGE_QUEUE_ATTACHMENTS_V1
            };
            if !engine.engine_info().supports(capability)
                || !self.state.read(cx).chat_host_supports(&chat_id, capability)
            {
                self.failure =
                    Some("Update the chat's engine to queue messages during a response.".into());
                cx.notify();
                return;
            }
        }
        if let Some(blob) = self
            .draft_unavailable
            .get(&self.current_key)
            .and_then(|blobs| blobs.first())
        {
            self.failure = Some(
                format!(
                    "{} needs reattachment before this draft can be sent.",
                    blob.name
                )
                .into(),
            );
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
            return;
        }
        let unsaved = self
            .staged()
            .iter()
            .chain(self.staged_appshots().iter().map(|shot| &shot.screenshot))
            .find(|attachment| {
                self.draft_blobs
                    .get(&self.current_key)
                    .and_then(|blobs| blobs.get(&attachment.id))
                    .is_none_or(|blob| blob.state != DraftBlobState::Saved)
            });
        if let Some(attachment) = unsaved {
            self.failure = Some(
                format!(
                    "{} is not durably saved yet. Wait for saving to finish or reattach it.",
                    attachment.name
                )
                .into(),
            );
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
            return;
        }
        let message_id = uuid::Uuid::new_v4().to_string();
        // Autosave owns the literal editor text at its revision. Goal slash
        // parsing and ordinary trimming can normalize the outbound snapshot;
        // changed content must reserve a new immutable draft revision.
        if self.input.read(cx).text() != text.as_str() {
            let revision = self
                .draft_revisions
                .entry(self.current_key.clone())
                .or_insert(0);
            *revision = revision.saturating_add(1);
        }
        let draft_snapshot = self.build_draft_record(&self.current_key, text.clone(), cx);
        let submitted_revision = draft_snapshot.revision;
        self.draft_submitting
            .insert(self.current_key.clone(), submitted_revision);
        // A debounce captured the submitted revision before the input clears.
        // Cancel it now: otherwise it can write the cleared text at that same
        // revision and collide with the immutable submission snapshot.
        self.draft_save_tasks.remove(&self.current_key);
        self.draft_saves_in_flight.remove(&self.current_key);
        // Snapshot-and-clear NOW (use-attachments.ts takeAttachments): the
        // strip empties the instant you hit send; a failure hands the files
        // back into the chat's stash.
        let submitted_input_items = self
            .input_items
            .remove(&self.current_key)
            .unwrap_or_default();
        let ordinary_staged = self
            .attachments
            .remove(&self.current_key)
            .unwrap_or_default();
        let staged_appshots = self.appshots.remove(&self.current_key).unwrap_or_default();
        let mut staged = ordinary_staged.clone();
        staged.extend(
            staged_appshots
                .iter()
                .map(|appshot| appshot.screenshot.clone()),
        );
        // `typed` keeps the user's own words for the failure hand-back below:
        // restoring the folded prompt would paste the comment block into the
        // input as literal text.
        let key = self.current_key.clone();
        let superseded_recoverable: Vec<String> = self
            .recoverable_submissions
            .get(&key)
            .into_iter()
            .flatten()
            .filter(|entry| entry.snapshot.same_message_content(&draft_snapshot))
            .map(|entry| entry.command_id.clone())
            .collect();
        let comments = self.state.update(cx, |state, cx| {
            let taken = state.take_review_comments(&key);
            if !taken.is_empty() {
                cx.notify();
            }
            taken
        });
        self.draft_comment_fingerprints.remove(&key);
        let typed = text.clone();
        let text = crate::comments::with_comments(&text, &comments);
        self.preview = None;
        let created_at = chrono::Utc::now().timestamp_millis();
        // Existing busy chats always queue; compatibility was checked before
        // taking the draft, attachments, or review comments.
        let queue = queue && !is_new;
        let clean_queue_attachment_text = staged.is_empty()
            || (engine
                .engine_info()
                .supports(capabilities::MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1)
                && self.state.read(cx).chat_host_supports(
                    &chat_id,
                    capabilities::MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1,
                ));

        // Queued-attachment flow (durable-by-design): stage the bytes on the
        // LOCAL engine, queue the command immediately with `pending://` refs,
        // and let the engine push the bytes to a remote host afterwards —
        // staging must never gate the queue (2026-08-19 incident: a send
        // died with a zombie peer link because the upload sat in front of
        // QueueCommand). Requires every engine involved to understand the
        // ref scheme — the local engine (an IPC daemon may be older than
        // this UI) and, for remotely-hosted chats, the host; anything older
        // keeps the legacy blocking upload.
        let host_is_remote = host_device_id
            .as_deref()
            .is_some_and(|id| local_device_id.as_deref() != Some(id));
        // Queue rows do not carry upstream's attachment-transfer escort, so
        // they retain the proven host-upload path and store absolute refs.
        // Appshot XML attributes require escaped final paths. The engine's plain
        // string replacement of pending refs cannot safely rewrite those, so
        // rich captures use the existing upload-before-send path.
        let queued_flow = !queue && staged_appshots.is_empty() && !staged.is_empty() && {
            let state = self.state.read(cx);
            let local_ok = local_device_id
                .as_deref()
                .is_some_and(|id| state.device_version_at_least(id, QUEUED_ATTACHMENTS_MIN));
            let host_ok = !host_is_remote
                || host_device_id
                    .as_deref()
                    .is_some_and(|id| state.device_version_at_least(id, QUEUED_ATTACHMENTS_MIN));
            local_ok && host_ok
        };
        // Upload identities minted NOW: in the queued flow the `pending://`
        // ref IS the persisted transport until the host rewrites it, so the
        // id must exist before any bytes move.
        let upload_ids: Vec<String> = staged
            .iter()
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect();
        // The echo carries attachment refs from the first frame, so photos
        // render while the send is still pending. Queued flow: the refs are
        // the real `pending://` identities (stable — no post-upload refresh).
        // Legacy flow: synthetic `pending/…` paths that the post-upload
        // refresh replaces with the host's absolute paths. Either way the
        // staged bytes are seeded into the transcript cache under every
        // device key the transcript consults.
        let echo_paths: Vec<String> = if queued_flow {
            staged
                .iter()
                .zip(&upload_ids)
                .map(|(att, id)| format!("pending://{id}/{}", att.name))
                .collect()
        } else {
            staged
                .iter()
                .map(|att| format!("pending/{}/{}", att.id, att.name))
                .collect()
        };
        let echo_appshot_paths: HashMap<String, String> = staged
            .iter()
            .zip(&echo_paths)
            .map(|(attachment, path)| (attachment.id.clone(), path.clone()))
            .collect();
        let echo_text = attachments::with_attachments(
            &appshots::with_appshots(&text, &staged_appshots, &echo_appshot_paths),
            &echo_paths,
        );
        // Queued flow also seeds the UPLOAD ALIAS: the host rewrites the
        // persisted ref to `{its uploads dir}/{id8}-{name}` — an absolute
        // path the sender can't predict, but whose id8 it minted. The alias
        // keeps the thumbnail on the already-local bytes through that
        // rewrite instead of blanking into a reload skeleton.
        if queued_flow {
            for (upload_id, att) in upload_ids.iter().zip(&staged) {
                attachments::seed_attachment_alias(
                    &device_id,
                    upload_id,
                    &att.name,
                    att.image.clone(),
                );
                if let Some(local) = local_device_id.as_deref()
                    && local != device_id
                {
                    attachments::seed_attachment_alias(
                        local,
                        upload_id,
                        &att.name,
                        att.image.clone(),
                    );
                }
            }
        }
        for (path, att) in echo_paths.iter().zip(&staged) {
            attachments::seed_attachment(&device_id, path, &att.name, att.image.clone());
            if let Some(local) = local_device_id.as_deref()
                && local != device_id
            {
                attachments::seed_attachment(local, path, &att.name, att.image.clone());
            }
        }

        // Optimistic echo (client-minted id doubles as the persisted message id,
        // so the doc frame dedups it away).
        let echo = SessionMessageEntry {
            id: message_id.clone(),
            role: luna_doc::MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "t0".into(),
                text: echo_text.clone(),
            }],
            created_at,
            device_id: "local".into(),
            status: None,
            continuation_of: None,
        };
        self.launching_new_chat = is_new;
        if as_goal {
            self.goal_handed_off();
        }
        if is_new {
            cx.emit(ComposerEvent::NewThreadTransitionStarted);
        }
        // A queued message is not in the transcript yet — the queue panel is
        // its echo, and it gets a real bubble when the host sends it.
        self.state.update(cx, |s, cx| {
            if is_new {
                s.select_chat(Some(chat_id.clone()), cx);
            }
            if should_publish_optimistic_echo(queue) {
                s.push_echo(&chat_id, echo);
                // Working overlay until the host executes the queued command —
                // without it a remote send flashed Completed (and could ring
                // the done-chime) in the queue→drain→sync gap.
                s.begin_pending_send(&chat_id, &message_id, chrono::Utc::now());
            }
            cx.notify();
        });

        self.set_input_text_without_draft_edit(String::new(), cx);
        self.drafts.remove(&self.current_key);
        self.failure = None;
        self.draft_save_failure = None;
        self.draft_merge_notice = None;
        self.delivery_failure = None;
        self.recovery_notice = None;
        self.sending = true;
        // A queued row is represented by the queue panel, not the transcript.
        // Claiming an own-turn anchor for it here would replace the live
        // turn's runway with an id that has no transcript row yet.
        if should_publish_optimistic_echo(queue) {
            cx.emit(ComposerEvent::Sent {
                chat_id: chat_id.clone(),
                message_id: message_id.clone(),
            });
        }
        cx.notify();

        let restore_text = typed;
        let submitted_key = key.clone();
        let err_chat_id = chat_id.clone();
        let err_message_id = message_id.clone();
        let send_engine_generation = self.state.read(cx).engine_generation();
        self.send_task = Some(cx.spawn(async move |this, cx| {
            let mut create_chat_attempted = false;
            let mut result: Result<ComposerSendOutcome, ComposerSendFailure> = async {
                let saved = crate::drafts::save(&engine, &draft_snapshot)
                    .await
                    .map_err(|error| {
                        ComposerSendFailure::Rejected(format!(
                            "Draft was not saved; nothing was sent: {error}"
                        ))
                    })?;
                if let ComposerDraftSaveOutcome::Stale { current_revision } = saved {
                    return Err(ComposerSendFailure::Rejected(format!(
                        "Draft changed before submission (current revision {current_revision}); nothing was sent"
                    )));
                }
                crate::drafts::begin_submission(
                    &engine,
                    &message_id,
                    Some(&chat_id),
                    &draft_snapshot,
                )
                .await
                .map_err(|error| {
                    ComposerSendFailure::Rejected(format!(
                        "Draft submission could not be recorded; nothing was sent: {error}"
                    ))
                })?;
                // The snapshot and its pending submission are both durable.
                // Provider acceptance is a separate receipt decision: a
                // pending or unknown delivery can close safely now, while any
                // newer typing still has its own unsaved revision to flush.
                this.update(cx, |composer, cx| {
                    if composer.state.read(cx).engine_generation() != send_engine_generation {
                        return;
                    }
                    let saved = composer
                        .draft_saved_revisions
                        .entry(submitted_key.clone())
                        .or_default();
                    *saved = (*saved).max(submitted_revision);
                    composer.finish_close_flush_if_ready(cx);
                })
                .ok();
                // Attachments stage FIRST — before the chat row or anything
                // else exists. Staging is chat-independent (keyed by
                // uploadId), and ordering it first makes a new-chat send
                // atomic: a staging failure aborts with NOTHING created,
                // instead of stranding a just-minted empty chat (v0.2.12
                // "failed to stage → empty transcript" report).
                //
                // Queued flow: commit the bytes to the LOCAL engine's uploads
                // dir (fast, offline-safe) — the queued command carries the
                // `pending://` refs and the engine delivers the bytes to a
                // remote host afterwards, retrying until they land. Legacy
                // flow (old engines): stage on the host device up front,
                // bounded by a total budget so a degraded link fails the send
                // loudly instead of grinding through silent per-chunk retries
                // for minutes.
                let mut content = text.clone();
                let mut attachment_paths: Vec<String> = Vec::new();
                let mut transfers: Vec<serde_json::Value> = Vec::new();
                if !staged.is_empty() && queued_flow {
                    // Local staging is disk-speed; publish progress anyway so
                    // huge files still narrate.
                    let progress = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let total: u64 = staged.iter().map(|a| a.bytes().len() as u64).sum();
                    {
                        let progress = progress.clone();
                        this.update(cx, |composer, cx| {
                            composer.state.update(cx, |s, cx| {
                                s.begin_upload_progress(total, progress);
                                cx.notify();
                            });
                        })
                        .ok();
                    }
                    for (att, upload_id) in staged.iter().zip(&upload_ids) {
                        if let Err(err) = attachments::upload_attachment(
                            &engine,
                            cx.background_executor(),
                            None,
                            upload_id,
                            att,
                            Some(progress.clone()),
                        )
                        .await
                        {
                            tracing::warn!(name = %att.name, error = %err, "local attachment stage failed");
                            return Err(ComposerSendFailure::Rejected(
                                "Couldn't stage the attachment locally.".to_string(),
                            ));
                        }
                        transfers.push(serde_json::json!({
                            "uploadId": upload_id,
                            "fileName": att.name,
                        }));
                    }
                    // The echo refs ARE the persisted refs — no refresh pass.
                    attachment_paths = echo_paths.clone();
                    content = echo_text.clone();
                } else if !staged.is_empty() {
                    let progress = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let total: u64 = staged.iter().map(|a| a.bytes().len() as u64).sum();
                    {
                        let progress = progress.clone();
                        this.update(cx, |composer, cx| {
                            composer.state.update(cx, |s, cx| {
                                s.begin_upload_progress(total, progress);
                                cx.notify();
                            });
                        })
                        .ok();
                    }
                    for (att, upload_id) in staged.iter().zip(&upload_ids) {
                        match attachments::upload_attachment(
                            &engine,
                            cx.background_executor(),
                            host_device_id.as_deref(),
                            upload_id,
                            att,
                            Some(progress.clone()),
                        )
                        .await
                        {
                            Ok(path) => attachment_paths.push(path),
                            Err(err) => {
                                tracing::warn!(name = %att.name, error = %err, "attachment upload failed");
                                return Err(ComposerSendFailure::Rejected(
                                    "Couldn't upload the attachment — the device may be offline."
                                        .to_string(),
                                ));
                            }
                        }
                    }
                    // Seed the transcript cache from local bytes so the sent
                    // bubble's thumbnails never round-trip (seedTranscript-
                    // Attachment in the original send path).
                    let seed_device = host_device_id.clone().unwrap_or_else(|| device_id.clone());
                    for (path, att) in attachment_paths.iter().zip(&staged) {
                        attachments::seed_attachment(&seed_device, path, &att.name, att.image.clone());
                        if seed_device != device_id {
                            attachments::seed_attachment(&device_id, path, &att.name, att.image.clone());
                        }
                    }
                    let appshot_paths: HashMap<String, String> = staged
                        .iter()
                        .zip(&attachment_paths)
                        .map(|(attachment, path)| (attachment.id.clone(), path.clone()))
                        .collect();
                    content = attachments::with_attachments(
                        &appshots::with_appshots(&text, &staged_appshots, &appshot_paths),
                        &attachment_paths,
                    );
                    // A normal send already has an optimistic echo: refresh it
                    // in place with the uploaded refs so its thumbnails never
                    // flicker. A queued message has no transcript echo at all;
                    // its queue row is the only representation until dispatch.
                    if should_publish_optimistic_echo(queue) {
                        let refreshed = SessionMessageEntry {
                            id: message_id.clone(),
                            role: luna_doc::MessageRole::User,
                            parts: vec![MessagePart::Text {
                                id: "t0".into(),
                                text: content.clone(),
                            }],
                            created_at,
                            device_id: "local".into(),
                            status: None,
                            continuation_of: None,
                        };
                        let echo_chat_id = chat_id.clone();
                        this.update(cx, |composer, cx| {
                            composer.state.update(cx, |s, cx| {
                                s.remove_echo(&echo_chat_id, &message_id);
                                s.push_echo(&echo_chat_id, refreshed);
                                cx.notify();
                            });
                        })
                        .ok();
                    }
                }

                // Resolve the working directory: existing chats keep theirs;
                // new chats run per the checkout plan (t3code env-mode): the
                // space's folder as-is, an EXISTING worktree of the picked ref
                // (a plain cwd override — multiple sessions share one
                // worktree), or a fresh isolated worktree created off the
                // picked base ref (CreateWorktree on send, targeted at the
                // space's device; the RPC relay-forwards).
                let mut cwd = if is_new {
                    space_path
                        .clone()
                        .expect("new sessions must have a selected project")
                } else {
                    existing_cwd.unwrap_or_else(|| ".".to_string())
                };
                let mut worktree_cwd: Option<String> = None;
                // Fresh-worktree plans ride the QUEUED Run command (a
                // WorktreeSpec the HOST materializes at drain time) instead of
                // a blocking CreateWorktree relay RPC here: the RPC had no
                // timeout, so a lost relay frame wedged the send on "Sending…"
                // forever while the session ran remotely anyway (2026-08-18).
                let mut run_worktree: Option<luna_proto::WorktreeSpec> = None;
                // The picked ref rides createChat so the session footer names
                // it from the first frame (it read "Select ref" until the
                // host's diff reconciler got around to stamping the branch).
                let mut chat_branch: Option<String> = None;
                if is_new {
                    match &plan {
                        crate::pickers::CheckoutPlan::CurrentCheckout { branch } => {
                            chat_branch = branch.clone();
                        }
                        crate::pickers::CheckoutPlan::ReuseWorktree { path, branch } => {
                            cwd = path.clone();
                            worktree_cwd = Some(path.clone());
                            chat_branch = Some(branch.clone());
                        }
                        crate::pickers::CheckoutPlan::NewWorktree { base } => {
                            // Footer shows the base until the host stamps the
                            // actual luna/<name> branch post-creation. cwd
                            // stays the repo folder — an old host that doesn't
                            // know the spec degrades to the main checkout
                            // instead of failing the run.
                            chat_branch = base.clone();
                            // A remote repo's branch list loads over the
                            // relay — on a bad link it may never arrive
                            // and the picker has no base. That must NOT
                            // silently drop the isolation the user picked
                            // (2026-08-19: "New worktree" ran in the main
                            // checkout): default to HEAD, which git — any
                            // host version — resolves as the repo's
                            // current checkout state.
                            let base = base.clone().unwrap_or_else(|| "HEAD".to_string());
                            run_worktree = Some(luna_proto::WorktreeSpec {
                                repo_path: space_path
                                    .clone()
                                    .expect("new sessions must have a selected project"),
                                base: base.clone(),
                            });
                        }
                    }
                }

                // Best-effort Mutate createChat with the picked config: the
                // engine resolves device and cwd from the project row.
                // The doc host would materialize the session on the first
                // command anyway, so failures are non-fatal.
                if is_new {
                    let mut mutate = serde_json::json!({
                        "op": "createChat",
                        "chatId": chat_id,
                    });
                    if let Some(object) = mutate.as_object_mut() {
                        object.insert(
                            "spaceId".into(),
                            serde_json::Value::String(
                                space_id
                                    .clone()
                                    .expect("new sessions must have a selected project"),
                            ),
                        );
                    }
                    if let Some(object) = mutate.as_object_mut() {
                        if let Some(worktree_cwd) = &worktree_cwd {
                            object.insert(
                                "cwd".into(),
                                serde_json::Value::String(worktree_cwd.clone()),
                            );
                        }
                        if let Some(branch) = &chat_branch {
                            object.insert(
                                "branch".into(),
                                serde_json::Value::String(branch.clone()),
                            );
                        }
                        if let Some(config) = resolved.chat_config()
                            && let Ok(config) = serde_json::to_value(&config)
                        {
                            object.insert("config".into(), config);
                        }
                    }
                    if let Some(object) = mutate.as_object_mut() {
                        object.remove("op");
                        object.remove("chatId");
                        object.insert("kind".into(), "createChat".into());
                    }
                    create_chat_attempted = true;
                    if let Err(err) = crate::task_commands::execute(&engine, &chat_id, mutate).await
                    {
                        tracing::warn!(error = %err, "CreateChat mutate unavailable; doc host will materialize the chat");
                    }
                }

                if queue {
                    // A queue row is editable UI state, so its text must stay
                    // free of the internal attachment-path trailer. The host
                    // rebuilds that transport when it promotes the row.
                    let appshot_paths = staged.iter().zip(&attachment_paths)
                        .map(|(attachment, path)| (attachment.id.clone(), path.clone())).collect();
                    let queue_body = appshots::with_appshots(&text, &staged_appshots, &appshot_paths);
                    let queue_text = if !clean_queue_attachment_text {
                        content.as_str()
                    } else if queue_body.trim().is_empty() && !attachment_paths.is_empty() {
                        attachments::ATTACHMENT_ONLY_TEXT
                    } else {
                        queue_body.as_str()
                    };
                    let intent = serde_json::json!({
                        "kind": "queueMessage",
                        "messageId": message_id,
                        "text": queue_text,
                        "attachments": attachment_paths,
                        "inputItems": submitted_input_items,
                        "holdForTurnEnd": true,
                        "followUpBehavior": follow_up_behavior.unwrap_or_default(),
                        "execution": resolved.chat_config(),
                    });
                    let receipt = crate::task_commands::execute_receipt_on_with_id(
                        &engine,
                        &chat_id,
                        host_device_id.as_deref(),
                        message_id.clone(),
                        intent,
                    )
                    .await
                    .map_err(|error| {
                        ComposerSendFailure::Unknown(format!(
                            "Queue acceptance is unknown: {error}"
                        ))
                    })?;
                    return match receipt.status {
                        CommandReceiptStatus::Applied => {
                            Ok(ComposerSendOutcome::Queued(message_id.clone()))
                        }
                        CommandReceiptStatus::Pending => Ok(ComposerSendOutcome::Pending),
                        _ => Err(ComposerSendFailure::Rejected(
                            receipt
                                .reason
                                .map(|reason| reason.message)
                                .unwrap_or_else(|| "The queue rejected this message".into()),
                        )),
                    };
                }

                let command = SessionCommandPayload::Run {
                    request: RunRequest {
                        review: None,
                        goal: as_goal.then(|| luna_proto::GoalStartRequest {
                            objective: content.clone(),
                            token_budget: None,
                        }),
                        input_items: submitted_input_items.clone(),
                        prompt: content.clone(),
                        harness: resolved.harness,
                        provider_account_id: resolved.provider_account_id.clone(),
                        model: resolved.model.clone(),
                        reasoning: resolved.reasoning,
                        model_options: resolved.model_options.clone(),
                        cwd,
                        sandbox: SandboxLevel::WorkspaceWrite,
                        auto_approve: false,
                        permission_mode: resolved.permission_mode,
                        parent_permission_ceiling: None,
                        environment_permission_ceiling: None,
                        permission_policy: None,
                        resume: None,
                        attachments: attachment_paths,
                        worktree: run_worktree,
                    },
                    message_id: message_id.clone(),
                };
                let command = serde_json::to_value(&command)
                    .map_err(|e| ComposerSendFailure::Rejected(format!("Send failed: {e}")))?;
                let mut intent = serde_json::json!({ "kind": "session", "command": command });
                if !transfers.is_empty() {
                    intent["transfers"] = serde_json::Value::Array(transfers);
                }
                // Deadline-bounded: QueueCommand is a local write (in-process
                // or IPC), but a deferred engine handle can park forever —
                // the send task must never grind silently (2026-08-19).
                let receipt = crate::task_commands::execute_receipt_with_id(
                    &engine,
                    &chat_id,
                    message_id.clone(),
                    intent,
                )
                .await
                .map_err(|error| {
                    ComposerSendFailure::Unknown(format!("Delivery is unknown: {error}"))
                })?;
                match receipt.status {
                    CommandReceiptStatus::Applied => Ok(ComposerSendOutcome::Applied),
                    CommandReceiptStatus::Pending => Ok(ComposerSendOutcome::Pending),
                    _ => Err(ComposerSendFailure::Rejected(receipt_failure_message(
                        &receipt,
                        "The task rejected this message",
                    ))),
                }
            }
            .await;
            if matches!(&result, Ok(ComposerSendOutcome::Pending)) {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if let Ok(Some(receipt)) = engine
                    .client()
                    .call_as::<Option<luna_proto::CommandReceipt>>(
                        methods::GET_TASK_COMMAND_RECEIPT,
                        serde_json::json!({ "commandId": message_id.clone() }),
                    )
                    .await
                {
                    result = match receipt.status {
                        CommandReceiptStatus::Applied => Ok(ComposerSendOutcome::Applied),
                        CommandReceiptStatus::Pending => Ok(ComposerSendOutcome::Pending),
                        _ => Err(ComposerSendFailure::Rejected(receipt_failure_message(
                            &receipt,
                            "The task rejected this message",
                        ))),
                    };
                }
            }
            let settlement = match &result {
                Ok(ComposerSendOutcome::Applied) => {
                    crate::drafts::settle_submission(&engine, &message_id, "applied", None).await
                }
                Ok(ComposerSendOutcome::Queued(queue_id)) => {
                    crate::drafts::settle_submission(
                        &engine,
                        &message_id,
                        "applied",
                        Some(queue_id),
                    )
                    .await
                }
                Err(ComposerSendFailure::Rejected(_)) => {
                    crate::drafts::settle_submission(
                        &engine,
                        &message_id,
                        "recoverable",
                        None,
                    )
                    .await
                }
                Ok(ComposerSendOutcome::Pending) | Err(ComposerSendFailure::Unknown(_)) => Ok(()),
            };
            if let Err(error) = settlement {
                tracing::warn!(%error, "draft submission settlement failed");
                if matches!(
                    &result,
                    Ok(ComposerSendOutcome::Applied | ComposerSendOutcome::Queued(_))
                ) {
                    result = Ok(ComposerSendOutcome::Pending);
                }
            }
            let rejected_new_chat =
                is_new && matches!(&result, Err(ComposerSendFailure::Rejected(_)));
            let retain_setup_task = rejected_new_chat
                && create_chat_attempted
                && may_have_setup
                && retain_failed_worktree_setup(&engine, &err_chat_id).await;
            if rejected_new_chat && create_chat_attempted && !retain_setup_task {
                // Clean up only a minted task without a durable setup receipt.
                // Failed setup needs this task for output and Retry.
                let _ = crate::task_commands::execute(
                    &engine,
                    &err_chat_id,
                    serde_json::json!({ "kind": "delete" }),
                )
                .await;
            }
            this.update(cx, |composer, cx| {
                composer.sending = false;
                if matches!(
                    &result,
                    Ok(ComposerSendOutcome::Applied | ComposerSendOutcome::Queued(_))
                        | Err(ComposerSendFailure::Rejected(_))
                ) {
                    composer.draft_submitting.remove(&submitted_key);
                }
                if matches!(
                    &result,
                    Ok(ComposerSendOutcome::Applied | ComposerSendOutcome::Queued(_))
                ) {
                    composer.finish_applied_draft(&submitted_key, submitted_revision, cx);
                }
                if matches!(&result, Ok(ComposerSendOutcome::Applied)) {
                    composer.forget_recoverable_submissions(
                        &submitted_key,
                        &superseded_recoverable,
                    );
                }
                composer
                    .state
                    .update(cx, |s, _| s.end_upload_progress());
                if let Ok(ComposerSendOutcome::Queued(message_id)) = &result {
                    cx.emit(ComposerEvent::Queued {
                        chat_id: err_chat_id.clone(),
                        message_id: message_id.clone(),
                    });
                }
                match &result {
                    Ok(ComposerSendOutcome::Pending) => {
                        // Run commands are durably queued before the session
                        // executor finalizes their receipts. Pending is normal
                        // during that gap; the watcher settles the draft once
                        // the authoritative receipt becomes terminal.
                        composer.watch_draft_receipt(
                            err_message_id.clone(),
                            err_chat_id.clone(),
                            submitted_key.clone(),
                            submitted_revision,
                            superseded_recoverable.clone(),
                            may_have_setup,
                            engine.clone(),
                            cx,
                        );
                    }
                    Err(ComposerSendFailure::Unknown(message)) => {
                        tracing::warn!(task_id = %err_chat_id, detail = %message, "send delivery unconfirmed");
                        let warning: SharedString =
                            "Delivery is unconfirmed. LUNA is checking the receipt; do not resend yet."
                                .into();
                        composer.failure = Some(warning.clone());
                        composer.failure_key = Some(err_chat_id.clone());
                        composer.delivery_failure = Some((err_message_id.clone(), warning));
                        composer.watch_draft_receipt(
                            err_message_id.clone(),
                            err_chat_id.clone(),
                            submitted_key.clone(),
                            submitted_revision,
                            superseded_recoverable.clone(),
                            may_have_setup,
                            engine.clone(),
                            cx,
                        );
                    }
                    _ => {}
                }
                if let Err(ComposerSendFailure::Rejected(message)) = result {
                    composer.delivery_failure = None;
                    // Failure: red banner, echo removed, prompt back in the
                    // draft, staged files back in the stash. A failed setup
                    // keeps its materialized task and task-scoped recovery;
                    // an empty or pre-creation rejection returns to the canvas.
                    let restore_key = if is_new && !retain_setup_task {
                        String::new()
                    } else {
                        err_chat_id.clone()
                    };
                    if retain_setup_task {
                        // The submission record is already durable under the
                        // task id. Clear only the submitted canvas revision so
                        // reopening New chat cannot offer a duplicate send.
                        composer.finish_applied_draft(&submitted_key, submitted_revision, cx);
                    }
                    let current_text = (composer.current_key == restore_key)
                        .then(|| composer.input.read(cx).text().to_string())
                        .unwrap_or_default();
                    let has_newer_draft = submission_has_newer_draft(
                        submitted_revision,
                        composer
                            .draft_revisions
                            .get(&restore_key)
                            .copied()
                            .unwrap_or(0),
                        &current_text,
                    );
                    composer.failure = Some(if has_newer_draft {
                        "The previous message was rejected and preserved separately from your newer draft."
                            .into()
                    } else {
                        message.into()
                    });
                    composer.failure_key = Some(restore_key.clone());
                    if as_goal && !has_newer_draft {
                        composer.restore_goal_mode(&restore_key);
                    }
                    if !has_newer_draft {
                        let selections = composer.input_items.entry(restore_key.clone()).or_default();
                        for selection in &submitted_input_items {
                            if !selections.contains(selection) { selections.push(selection.clone()); }
                        }
                    }
                    composer.state.update(cx, |s, cx| {
                        s.remove_echo(&err_chat_id, &err_message_id);
                        s.end_pending_send(&err_chat_id, &err_message_id);
                        if is_new
                            && !retain_setup_task
                            && s.selected_chat.as_deref() == Some(err_chat_id.as_str())
                        {
                            // Back to the canvas; the navigation draft-swap
                            // loads the restored draft below.
                            s.select_chat(None, cx);
                        }
                        if !has_newer_draft {
                            for comment in &comments {
                                s.add_review_comment(&restore_key, comment.clone());
                            }
                        }
                        cx.notify();
                    });
                    if has_newer_draft {
                        // The immutable submission record remains recoverable;
                        // never merge it into text typed after dispatch.
                    } else if is_new && composer.current_key != restore_key {
                        // A navigation re-key is pending; avoid clobbering its
                        // input before the restored draft key becomes current.
                        composer.drafts.insert(restore_key.clone(), restore_text.clone());
                    } else {
                        // Already keyed to the restore target; no navigation
                        // swap can overwrite this prompt.
                        composer.set_input_text_without_draft_edit(restore_text, cx);
                        composer.trusted_native_envelope_restored(&restore_key, cx);
                    }
                    if !has_newer_draft && !ordinary_staged.is_empty() {
                        // Merge by id (stashAttachments): files the user staged
                        // while the send was in flight survive the hand-back —
                        // draining the minted chat's slot too when the restore
                        // target is the canvas.
                        let mut merged = ordinary_staged.clone();
                        for key in [err_chat_id.clone(), restore_key.clone()] {
                            if let Some(slot) = composer.attachments.get_mut(&key) {
                                let fresh: Vec<_> = slot
                                    .drain(..)
                                    .filter(|e| !merged.iter().any(|f| f.id == e.id))
                                    .collect();
                                merged.extend(fresh);
                            }
                        }
                        composer.attachments.insert(restore_key.clone(), merged);
                    }
                    if !has_newer_draft {
                        composer.restore_failed_appshots(
                            &staged_appshots,
                            &err_chat_id,
                            &restore_key,
                        );
                    }
                    // The send canceled the old debounce before clearing the
                    // input. Persist the restored or newer draft with its own
                    // revision, even when the snapshot save itself failed.
                    let revision = composer
                        .draft_revisions
                        .entry(restore_key.clone())
                        .or_insert(0);
                    *revision = (*revision).max(submitted_revision.saturating_add(1));
                    composer.schedule_draft_save(restore_key, Duration::ZERO, cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub(crate) fn interrupt_selected(&mut self, cx: &mut Context<Self>) {
        let Some(chat_id) = self
            .binding
            .chat_id(self.state.read(cx))
            .map(str::to_string)
        else {
            return;
        };
        self.interrupt_chat(chat_id, cx);
    }

    pub(crate) fn interrupt_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        if !begin_interrupt(&mut self.interrupting, &chat_id) {
            return;
        }
        let intent = interrupt_intent();
        let task_chat_id = chat_id.clone();
        let failure_chat = chat_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = crate::task_commands::execute(&engine, &task_chat_id, intent).await;
            #[cfg(feature = "appshots-fixture")]
            {
                let outcome = result
                    .as_ref()
                    .map(Clone::clone)
                    .map_err(|err| err.to_string());
                let chat = task_chat_id.clone();
                this.update(cx, |composer, _| {
                    composer.last_stop.insert(chat, outcome);
                })
                .ok();
            }
            if let Err(err) = result {
                this.update(cx, |composer, cx| {
                    composer.interrupting.remove(&task_chat_id);
                    composer.failure = Some(format!("Stop failed: {err}").into());
                    composer.failure_key = Some(failure_chat);
                    cx.notify();
                })
                .ok();
            }
        });
        self.interrupt_tasks.insert(chat_id, task);
    }

    pub(crate) fn is_interrupting(&self, chat_id: &str) -> bool {
        self.interrupting.contains(chat_id)
    }

    // ---- wizard glue ----

    fn set_async_delivery_failure(&mut self, key: &(String, String), message: SharedString) {
        self.failure = Some(message.clone());
        self.failure_key = Some(key.0.clone());
        self.async_failure = Some((key.clone(), message));
    }

    fn clear_async_delivery_failure(&mut self, key: &(String, String)) {
        let Some((owner, message)) = self.async_failure.as_ref() else {
            return;
        };
        if owner != key {
            return;
        }
        if self.failure_key.as_deref() == Some(key.0.as_str())
            && self.failure.as_ref() == Some(message)
        {
            self.failure = None;
            self.failure_key = None;
        }
        self.async_failure = None;
    }

    fn wizard_select(&mut self, option_ix: usize, cx: &mut Context<Self>) {
        if self
            .wizard
            .as_ref()
            .is_some_and(|wizard| self.answered_requests.contains(&wizard.request_id))
        {
            self.retry_async_submission(cx);
            return;
        }
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        let step = wizard.select(option_ix);
        let has_pick = wizard.page_has_pick();
        self.answer_input.update(cx, |input, cx| {
            input.set_placeholder(
                if has_pick {
                    "Type your own answer, or leave this blank to use the selected option"
                } else {
                    "Type your own answer, or pick an option above"
                },
                cx,
            )
        });
        match step {
            WizardStep::AutoAdvance => self.schedule_auto_advance(cx),
            WizardStep::Done(answers) => self.wizard_finish(answers, cx),
            WizardStep::Stay => {}
        }
        cx.notify();
    }

    fn schedule_auto_advance(&mut self, cx: &mut Context<Self>) {
        self.advance_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(AUTO_ADVANCE_MS))
                .await;
            this.update(cx, |composer, cx| composer.wizard_advance(cx))
                .ok();
        }));
    }

    fn wizard_advance(&mut self, cx: &mut Context<Self>) {
        if self
            .wizard
            .as_ref()
            .is_some_and(|wizard| self.answered_requests.contains(&wizard.request_id))
        {
            return;
        }
        let typed = self.answer_input.read(cx).text().to_string();
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        wizard.set_typed(typed);
        if let Some(luna_proto::McpInputKind::Field { schema, .. }) = wizard
            .current()
            .and_then(|q| q.mcp.as_ref())
            .map(|m| &m.kind)
        {
            let answers = wizard.answers();
            if let Err(message) = schema.value_for_labels(&answers[wizard.page].labels) {
                self.failure = Some(message.into());
                self.failure_key = Some(self.current_key.clone());
                cx.notify();
                return;
            }
            self.failure = None;
            self.failure_key = None;
        }
        if !wizard.page_has_answer() {
            return;
        }
        match wizard.advance() {
            WizardStep::Done(answers) => self.wizard_finish(answers, cx),
            _ => {
                let next = wizard.current_typed().to_string();
                self.answer_input
                    .update(cx, |input, cx| input.set_text(next, cx));
                cx.notify();
            }
        }
    }

    fn wizard_back(&mut self, cx: &mut Context<Self>) {
        if self
            .wizard
            .as_ref()
            .is_some_and(|wizard| self.answered_requests.contains(&wizard.request_id))
        {
            return;
        }
        let typed = self.answer_input.read(cx).text().to_string();
        if let Some(wizard) = self.wizard.as_mut() {
            wizard.set_typed(typed);
            wizard.back();
            let previous = wizard.current_typed().to_string();
            self.answer_input
                .update(cx, |input, cx| input.set_text(previous, cx));
            cx.notify();
        }
    }

    fn wizard_mcp_action(&mut self, action: &str, cx: &mut Context<Self>) {
        let Some(wizard) = &self.wizard else {
            return;
        };
        let Some(question) = wizard.questions.iter().find(|q| {
            q.mcp.as_ref().is_some_and(|m| {
                matches!(
                    m.kind,
                    luna_proto::McpInputKind::FormAction | luna_proto::McpInputKind::Url { .. }
                )
            })
        }) else {
            return;
        };
        let answer = UserInputAnswer {
            question_id: question.id.clone(),
            labels: vec![action.to_owned()],
        };
        self.wizard_finish(vec![answer], cx);
    }

    fn wizard_open_mcp_url(&mut self, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        if wizard.url_opening || self.answered_requests.contains(&wizard.request_id) {
            return;
        }
        let request_id = wizard.request_id.clone();
        let chat_id = self.current_key.clone();
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let target = self
            .binding
            .chat_row(self.state.read(cx))
            .map(|chat| chat.device_id.clone());
        if let Some(wizard) = self.wizard.as_mut() {
            wizard.url_opening = true;
        }
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(luna_rpc::methods::MCP_ELICITATION_URL, serde_json::json!({ "chatId": chat_id, "requestId": request_id, "targetDeviceId": target })).await;
            this.update(cx, |composer, cx| {
                if composer.current_key != chat_id || composer.wizard.as_ref().is_none_or(|w| w.request_id != request_id) { return; }
                if let Some(wizard) = composer.wizard.as_mut() { wizard.url_opening = false; }
                match result {
                    Ok(value) => {
                        if let Some(url) = value.get("url").and_then(serde_json::Value::as_str)
                            && luna_proto::mcp_url_destination(url).is_ok() { cx.open_url(url); }
                        else { composer.failure = Some("Authorization URL is unavailable".into()); composer.failure_key = Some(chat_id.clone()); }
                    }
                    Err(error) => { composer.failure = Some(error.to_string().into()); composer.failure_key = Some(chat_id.clone()); }
                }
                cx.notify();
            }).ok();
        }));
    }

    /// Answers to a question flagged secret (Codex `isSecret`) go straight
    /// to the local engine's live resolver: never a durable session command,
    /// the RPC transport or a log. A remote viewport cannot send them.
    fn finish_secret_wizard(
        &mut self,
        engine: crate::state::EngineHandle,
        chat_id: String,
        wizard: Wizard,
        answers: Vec<UserInputAnswer>,
        cx: &mut Context<Self>,
    ) {
        let request_id = wizard.request_id.clone();
        self.failure = None;
        self.failure_key = None;
        self.wizard = None;
        self.answered_requests.insert(request_id.clone());
        self.answer_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .respond_secret_input(&chat_id, &request_id, answers)
                .await;
            this.update(cx, |composer, cx| {
                let failure: Option<&str> = match result {
                    Ok(SecretInputOutcome::Delivered) => None,
                    Ok(SecretInputOutcome::Closed) => Some(
                        "This request ended before your answer was sent; nothing was stored",
                    ),
                    Ok(SecretInputOutcome::NotSecret | SecretInputOutcome::Unavailable) => {
                        composer.answered_requests.remove(&request_id);
                        Some("This request is no longer pending")
                    }
                    Err(_) => {
                        composer.answered_requests.remove(&request_id);
                        if composer.current_key == chat_id {
                            composer.wizard = Some(wizard.clone());
                        }
                        Some("Secret answers can only be sent from the LUNA window running this task")
                    }
                };
                if let Some(message) = failure {
                    composer.failure = Some(message.into());
                    composer.failure_key = Some(chat_id.clone());
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn wizard_skip(&mut self, cx: &mut Context<Self>) {
        if self.wizard.as_ref().is_some_and(|wizard| {
            wizard.source == QuestionSource::Async
                && !self.answered_requests.contains(&wizard.request_id)
        }) {
            self.wizard_finish(Vec::new(), cx);
        }
    }

    fn retry_async_submission(&mut self, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_ref() else {
            return;
        };
        let key = (self.current_key.clone(), wizard.request_id.clone());
        if self
            .async_submissions
            .get(&key)
            .is_none_or(|submission| submission.state != AsyncDeliveryState::Uncertain)
        {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.failure = Some("Engine not connected".into());
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
            return;
        };
        self.dispatch_async_submission(key, engine, cx);
    }

    fn dispatch_async_submission(
        &mut self,
        key: (String, String),
        engine: crate::state::EngineHandle,
        cx: &mut Context<Self>,
    ) {
        let Some(submission) = self.async_submissions.get_mut(&key) else {
            return;
        };
        submission.state = AsyncDeliveryState::Sending;
        let command_id = submission.command_id.clone();
        let intent = submission.intent.clone();
        let chat_id = key.0.clone();
        self.clear_async_delivery_failure(&key);
        cx.spawn(async move |this, cx| {
            let result = crate::task_commands::execute_with_id(
                &engine, &chat_id, command_id.clone(), intent,
            ).await;
            this.update(cx, |composer, cx| {
                // The doc may resolve before a Pending/transport reply reaches
                // the UI. A settled card must not resurrect its error banner.
                if !composer.async_submissions.get(&key).is_some_and(|submission| submission.command_id == command_id) {
                    return;
                }
                match result {
                    Ok(receipt) if receipt.status == CommandReceiptStatus::Applied => {
                        if let Some(submission) = composer.async_submissions.get_mut(&key) {
                            submission.state = AsyncDeliveryState::Applied;
                        }
                        composer.clear_async_delivery_failure(&key);
                        let check_key = key.clone();
                        cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(Duration::from_secs(3)).await;
                            this.update(cx, |composer, cx| {
                                let unresolved = composer.current_key != check_key.0
                                    || async_question_open(composer.binding.transcript(composer.state.read(cx)), &check_key.1);
                                if unresolved && let Some(submission) = composer.async_submissions.get_mut(&check_key)
                                    && submission.state == AsyncDeliveryState::Applied {
                                        submission.state = AsyncDeliveryState::Uncertain;
                                        composer.set_async_delivery_failure(&check_key, "Answer accepted, but its readback is delayed. Retry checks the same command.".into());
                                        cx.notify();
                                    }
                            }).ok();
                        }).detach();
                    }
                    Ok(_) => {
                        if let Some(submission) = composer.async_submissions.get_mut(&key) {
                            submission.state = AsyncDeliveryState::Uncertain;
                        }
                        composer.set_async_delivery_failure(&key, "Answer delivery is pending. Retry checks the same command.".into());
                    }
                    Err(err) if matches!(err, RpcError::Failed(_) | RpcError::BadParams(_) | RpcError::UnknownMethod(_)) => {
                        composer.async_submissions.remove(&key);
                        *composer.async_attempts.entry(key.clone()).or_default() += 1;
                        composer.answered_requests.remove(&key.1);
                        composer.set_async_delivery_failure(&key, format!("Answer failed: {err}").into());
                    }
                    Err(err) => {
                        if let Some(submission) = composer.async_submissions.get_mut(&key) {
                            submission.state = AsyncDeliveryState::Uncertain;
                        }
                        composer.set_async_delivery_failure(&key, format!("Answer delivery is uncertain: {err}. Retry checks the same command.").into());
                    }
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    /// Submit the exact question command without touching the prompt draft.
    fn wizard_finish(&mut self, answers: Vec<UserInputAnswer>, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.clone() else {
            return;
        };
        if wizard.questions.iter().any(|q| q.mcp.is_some())
            && let Err(message) = luna_proto::mcp_response(&wizard.questions, &answers)
        {
            self.failure = Some(message.into());
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.failure = Some("Engine not connected".into());
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
            return;
        };
        let Some(chat_id) = self
            .binding
            .chat_id(self.state.read(cx))
            .map(str::to_string)
        else {
            return;
        };
        let async_question = wizard.source == QuestionSource::Async;
        self.advance_task = None;
        let request_id = wizard.request_id.clone();
        if async_question {
            let key = (chat_id.clone(), request_id.clone());
            let attempt = self.async_attempts.get(&key).copied().unwrap_or(0);
            let command_id = async_question_command_id(&chat_id, &request_id, attempt);
            let command = SessionCommandPayload::RespondAsyncQuestion {
                message_id: request_id.clone(),
                answers,
            };
            let Ok(value) = serde_json::to_value(command) else {
                return;
            };
            let submission = AsyncQuestionSubmission {
                command_id,
                intent: serde_json::json!({ "kind": "session", "command": value }),
                state: AsyncDeliveryState::Sending,
            };
            self.async_submissions.insert(key.clone(), submission);
            self.answered_requests.insert(request_id);
            self.dispatch_async_submission(key, engine, cx);
            cx.notify();
            return;
        }
        if wizard.questions.iter().any(UserInputQuestion::is_secret) {
            self.finish_secret_wizard(engine, chat_id, wizard, answers, cx);
            return;
        }
        let luna_question = match luna_approval_question(&wizard.questions) {
            Ok(question) => question,
            Err(()) => {
                self.failure = Some("Invalid LUNA approval request".into());
                self.failure_key = Some(chat_id);
                cx.notify();
                return;
            }
        };
        if let Some(question) = luna_question {
            let decision = approval_decision(question, &answers);
            self.failure = None;
            self.failure_key = None;
            self.answered_requests.insert(request_id.clone());
            // The server-owned pending kind is authoritative. This marker
            // only chooses the direct in-process UI route; never fall back to
            // RespondApproval over JSON RPC if that route is unavailable.
            self.action_task = Some(cx.spawn(async move |this, cx| {
                let result = engine
                    .respond_luna_local_ui_approval(&chat_id, &request_id, decision)
                    .await;
                this.update(cx, |composer, cx| {
                    if composer.binding.chat_id(composer.state.read(cx)) != Some(chat_id.as_str()) {
                        composer.answered_requests.remove(&request_id);
                        return;
                    }
                    match result {
                        Ok(LunaApprovalOutcome::AllowedOnce | LunaApprovalOutcome::Denied) => {
                            composer.wizard = None;
                            composer
                                .answer_input
                                .update(cx, |input, cx| input.set_text("", cx));
                        }
                        Ok(LunaApprovalOutcome::Stale) => {
                            composer.answered_requests.remove(&request_id);
                            composer.wizard = Some(wizard.clone());
                            composer.failure = Some("Approval is no longer pending".into());
                            composer.failure_key = Some(chat_id.clone());
                        }
                        Ok(LunaApprovalOutcome::Expired | LunaApprovalOutcome::RunEnded) => {
                            composer.answered_requests.remove(&request_id);
                            composer.wizard = None;
                            composer.failure = Some("Approval expired or its run ended".into());
                            composer.failure_key = Some(chat_id.clone());
                        }
                        Err(error) => {
                            composer.answered_requests.remove(&request_id);
                            composer.wizard = Some(wizard.clone());
                            composer.failure = Some(error.to_string().into());
                            composer.failure_key = Some(chat_id.clone());
                        }
                    }
                    cx.notify();
                })
                .ok();
            }));
            cx.notify();
            return;
        }
        self.failure = None;
        self.failure_key = None;
        self.wizard = None;
        self.answered_requests.insert(request_id.clone());
        self.answer_input
            .update(cx, |input, cx| input.set_text("", cx));
        let approval = wizard
            .questions
            .iter()
            .find(|question| question.approval.is_some());
        let command = if let Some(question) = approval {
            let decision = approval_decision(question, &answers);
            SessionCommandPayload::RespondApproval {
                request_id: request_id.clone(),
                decision,
            }
        } else {
            SessionCommandPayload::RespondInput {
                request_id: request_id.clone(),
                answers,
            }
        };
        let failure_chat = chat_id.clone();
        let intent = match serde_json::to_value(&command) {
            Ok(value) => serde_json::json!({ "kind": "session", "command": value }),
            Err(_) => return,
        };
        // `action_task`, NOT `send_task` — see `interrupt`.
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = crate::task_commands::execute(&engine, &failure_chat, intent).await;
            if let Err(err) = result {
                this.update(cx, |composer, cx| {
                    composer.failure = Some(format!("Answer failed: {err}").into());
                    composer.failure_key = Some(failure_chat.clone());
                    composer.answered_requests.remove(&request_id);
                    if wizard.questions.iter().any(|q| q.mcp.is_some())
                        && composer.current_key == failure_chat
                    {
                        let current = wizard.current_typed().to_string();
                        composer.wizard = Some(wizard.clone());
                        composer
                            .answer_input
                            .update(cx, |input, cx| input.set_text(current, cx));
                    }
                    composer.on_state_changed(cx);
                    cx.notify();
                })
                .ok();
                return;
            }
            // Safety net against a dead-looking session: the command queued,
            // but the host may still REJECT it (e.g. the run's resolver is
            // gone). If the very same request is still the live pending input
            // once the host has had ample time to execute and the resolved
            // flag to sync back, the answer demonstrably didn't take —
            // un-hide the panel instead of leaving the question unanswerable.
            cx.background_executor().timer(Duration::from_secs(2)).await;
            this.update(cx, |composer, cx| {
                let transcript = composer.binding.transcript(composer.state.read(cx));
                let still_pending = pending_input_request(transcript)
                    .is_some_and(|(pending_id, _)| pending_id == request_id);
                if still_pending && composer.answered_requests.remove(&request_id) {
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn on_wizard_key(&mut self, event: &KeyDownEvent, window: &Window, cx: &mut Context<Self>) {
        // Keys bubbling out of the free-text input must not double-handle:
        // digits select options only while the input is empty, and Enter is the
        // input's own Submit action when it has focus.
        let input_visible = self.wizard.as_ref().and_then(Wizard::current).is_none_or(|q| q.mcp.as_ref().is_none_or(|m| matches!(&m.kind, luna_proto::McpInputKind::Field { schema, .. } if schema.choices.is_empty() && schema.value_type != "boolean")));
        let input_focused =
            input_visible && self.answer_input.read(cx).focus_handle.is_focused(window);
        let input_empty = self.answer_input.read(cx).is_empty();
        let key = event.keystroke.key.as_str();
        // A bare digit picks an option only while focus is on the card, not
        // the answer field. Focused free text must accept numeric answers.
        // Modified digits still belong to app shortcuts such as ⌘1..⌘9.
        if let Some(option_ix) =
            wizard_number_shortcut(key, event.keystroke.modifiers.modified(), input_focused)
        {
            self.wizard_select(option_ix, cx);
            cx.stop_propagation();
        } else if key == "enter" {
            if !input_focused {
                self.wizard_advance(cx);
                cx.stop_propagation();
            }
        } else if key == "escape" {
            if wizard_escape_goes_back(key, input_focused, input_empty) {
                self.wizard_back(cx);
            }
            cx.stop_propagation();
        }
    }

    // ---- render pieces ----

    /// The agent-asked-a-question panel above the ordinary composer: the same
    /// theme-aware floating pill, uppercase
    /// header + "1/3" counter chip, option rows with number kbd chips, a
    /// composer-surface free-text override, and Back / Next-Submit footer.
    fn render_wizard(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(wizard) = self.wizard.clone() else {
            return gpui::Empty.into_any_element();
        };
        let counter = wizard.counter();
        let Some(question) = wizard.current().cloned() else {
            return gpui::Empty.into_any_element();
        };
        let page = wizard.page;
        let last = page + 1 >= wizard.questions.len();
        let typed_empty = self.answer_input.read(cx).is_empty();
        let submitting = self.answered_requests.contains(&wizard.request_id);
        let delivery_state = self
            .async_submissions
            .get(&(self.current_key.clone(), wizard.request_id.clone()))
            .map(|submission| submission.state);
        let retryable = delivery_state == Some(AsyncDeliveryState::Uncertain);
        let mcp_prompt = wizard.questions.iter().any(|q| q.mcp.is_some());
        let url_prompt = question
            .mcp
            .as_ref()
            .is_some_and(|m| matches!(m.kind, luna_proto::McpInputKind::Url { .. }));
        let mcp_field = question.mcp.as_ref().and_then(|m| match &m.kind {
            luna_proto::McpInputKind::Field { schema, .. } => Some(schema),
            _ => None,
        });
        // Codex `isOther:false` with options means pick one; no typed answer.
        let text_input = (!mcp_prompt && question.accepts_typed_answer())
            || mcp_field
                .is_some_and(|schema| schema.choices.is_empty() && schema.value_type != "boolean");
        let secret = question.is_secret();
        self.answer_input
            .update(cx, |input, cx| input.set_masked(secret, cx));
        let can_advance = retryable || (!submitting && (wizard.page_has_answer() || !typed_empty));
        let surface_radius =
            composer_surface_radius(self.dock_frame.map_or(0.0, |frame| frame.amount));

        let composer_for_a11y = cx.entity().downgrade();
        let options = question.options.iter().enumerate().map(|(ix, label)| {
            // Selection reads on the row only while no typed override exists
            // (typed answers win — luna question-panel.tsx `isSel`).
            let picked = wizard.is_picked(ix) && typed_empty;
            let composer_for_a11y = composer_for_a11y.clone();
            div()
                .id(("wizard-option", ix))
                .role(if question.multi_select {
                    gpui::Role::CheckBox
                } else {
                    gpui::Role::RadioButton
                })
                .aria_label(label.clone())
                .aria_toggled(if picked {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .tab_index(0)
                .focus_visible(|style| style.border_color(theme.accent))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .px(px(14.0))
                .py(px(10.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(if picked {
                    crate::theme::ink(0.16)
                } else {
                    gpui::transparent_black()
                })
                // luna question-panel.tsx option rows: `transition-colors`.
                .bg(if picked {
                    crate::theme::ink(0.09)
                } else {
                    motion::hover_blend(
                        &format!("wizard-option-{ix}"),
                        crate::theme::ink(0.025),
                        crate::theme::ink(0.06),
                    )
                })
                .on_hover(motion::hover_listener(format!("wizard-option-{ix}")))
                .cursor_pointer()
                // Selection rides GPUI's key-up keyboard click. Selecting on
                // key-down too toggled checkboxes back, and a single-select
                // advance let key-up answer the next question's same row.
                .on_key_down(crate::swallow_activation_key)
                .on_a11y_action(gpui::AccessibleAction::Click, move |_, _, cx| {
                    composer_for_a11y
                        .update(cx, |this, cx| this.wizard_select(ix, cx))
                        .ok();
                })
                .on_click(cx.listener(move |this, _, _, cx| this.wizard_select(ix, cx)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(crate::typography::ui_rems(13.5))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(if picked {
                            theme.text
                        } else {
                            theme.text.opacity(0.9)
                        })
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(SharedString::from(label.clone()))
                        .when_some(question.option_description(ix), |el, description| {
                            el.child(
                                div()
                                    .text_size(crate::typography::ui_rems(11.5))
                                    .font_weight(gpui::FontWeight::NORMAL)
                                    .text_color(theme.readable_muted(0.65))
                                    .child(SharedString::from(description.to_owned())),
                            )
                        })
                        .when_some(
                            question
                                .approval
                                .as_ref()
                                .and_then(|approval| approval.details.as_ref())
                                .and_then(|details| details.choices.get(ix)),
                            |el, choice| {
                                el.child(
                                    div()
                                        .text_size(crate::typography::ui_rems(11.5))
                                        .font_weight(gpui::FontWeight::NORMAL)
                                        .text_color(theme.readable_muted(0.65))
                                        .child(format!(
                                            "{} · {}",
                                            choice.scope, choice.description
                                        )),
                                )
                            },
                        ),
                )
                .when(ix < 9, |el| {
                    el.child(
                        // Number kbd chip: `size-[22px] rounded-md text-[11px]`.
                        div()
                            .flex_none()
                            .size(px(22.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .bg(if picked {
                                crate::theme::ink(0.16)
                            } else {
                                crate::theme::ink(0.05)
                            })
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(if picked {
                                theme.text
                            } else {
                                theme.readable_muted(0.6)
                            })
                            .child(SharedString::from(format!("{}", ix + 1))),
                    )
                })
        });

        let panel = composer_surface(&theme, surface_radius)
            .id("question-panel")
            .track_focus(&self.wizard_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_wizard_key(event, window, cx)
            }))
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(16.0))
                    .pt(px(16.0))
                    .flex()
                    .flex_col()
                    // Header: tracked uppercase + counter chip when paged.
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(10.5))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.readable_muted(0.6))
                                    .child(SharedString::from(crate::popover::tracked_upper(
                                        &question.header,
                                    ))),
                            )
                            .when(wizard.questions.len() > 1, |el| {
                                el.child(
                                    div()
                                        .h(px(20.0))
                                        .px(px(6.0))
                                        .flex()
                                        .items_center()
                                        .rounded(px(6.0))
                                        .bg(crate::theme::ink(0.06))
                                        .text_size(crate::typography::ui_rems(10.0))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.readable_muted(0.6))
                                        .child(SharedString::from(counter)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("wizard-question-text")
                            .max_h(px(160.0))
                            .overflow_y_scroll()
                            .mt(px(6.0))
                            .text_size(crate::typography::ui_rems(15.0))
                            .line_height(crate::typography::ui_rems(20.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(question.question.clone())),
                    )
                    .when_some(question.approval.as_ref(), |el, approval| {
                        let mut rows = approval
                            .details
                            .as_ref()
                            .map(|details| details.display_rows())
                            .unwrap_or_default();
                        rows.push(approval_expiry_row(approval));
                        let context = div()
                            .id("approval-context")
                            .mt(px(8.0))
                            .max_h(px(220.0))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .children(rows.into_iter().map(|(label, value)| {
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .text_size(crate::typography::ui_rems(10.5))
                                            .text_color(theme.readable_muted(0.65))
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .text_size(crate::typography::ui_rems(12.0))
                                            .text_color(theme.text)
                                            .child(value),
                                    )
                            }));
                        el.child(context)
                    })
                    .when(question.multi_select, |el| {
                        el.child(
                            div()
                                .mt(px(4.0))
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.readable_muted(0.65))
                                .child(SharedString::from("Select one or more options.")),
                        )
                    })
                    .child(
                        div()
                            .id("wizard-options")
                            .max_h(px(220.0))
                            .overflow_y_scroll()
                            .mt(px(12.0))
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .children(options),
                    )
                    // Same compact pill surface and input-row geometry as the
                    // ordinary composer; the input entity stays shared.
                    .when(text_input, |el| {
                        el.child(
                            div().mt(px(12.0)).pb(px(4.0)).px(px(4.0)).child(
                                composer_surface(&theme, surface_radius)
                                    .id("wizard-answer-surface")
                                    .w_full()
                                    .h(px(COMPACT_TOTAL_HEIGHT))
                                    .child(
                                        div()
                                            .h(px(COMPACT_TOTAL_HEIGHT - PILL_BORDER_V))
                                            .flex()
                                            .items_center()
                                            .min_w_0()
                                            .px(px(8.0))
                                            .child(self.answer_input.clone()),
                                    ),
                            ),
                        )
                    })
                    .when(url_prompt && !submitting, |el| {
                        el.child(
                            crate::popover::btn_ghost(
                                &theme,
                                if wizard.url_opening {
                                    "Opening…"
                                } else {
                                    "Open website"
                                },
                                "wizard-open-url",
                            )
                            .id("wizard-open-url")
                            .on_click(cx.listener(|this, _, _, cx| this.wizard_open_mcp_url(cx))),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .px(px(16.0))
                    .pb(px(16.0))
                    .pt(px(4.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .when(page > 0, |el| {
                                el.child(
                                    crate::popover::btn_ghost(&theme, "Back", "wizard-back")
                                        .id("wizard-back")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.wizard_back(cx)),
                                        ),
                                )
                            })
                            .when(mcp_prompt && !submitting, |el| {
                                el.child(
                                    crate::popover::btn_ghost(
                                        &theme,
                                        "Decline",
                                        "wizard-mcp-decline",
                                    )
                                    .id("wizard-mcp-decline")
                                    .on_click(cx.listener(
                                        |this, _, _, cx| this.wizard_mcp_action("Decline", cx),
                                    )),
                                )
                                .child(
                                    crate::popover::btn_ghost(
                                        &theme,
                                        "Cancel",
                                        "wizard-mcp-cancel",
                                    )
                                    .id("wizard-mcp-cancel")
                                    .on_click(cx.listener(
                                        |this, _, _, cx| this.wizard_mcp_action("Cancel", cx),
                                    )),
                                )
                            })
                            .when(
                                wizard.source == QuestionSource::Async && !submitting,
                                |el| {
                                    el.child(
                                        crate::popover::btn_ghost(&theme, "Skip", "wizard-skip")
                                            .id("wizard-skip")
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.wizard_skip(cx)),
                                            ),
                                    )
                                },
                            ),
                    )
                    .child(
                        crate::popover::btn_primary(
                            &theme,
                            if retryable {
                                "Retry delivery"
                            } else if submitting {
                                "Sending…"
                            } else if last {
                                "Send"
                            } else if mcp_field.is_some_and(|schema| schema.default.is_some())
                                && typed_empty
                                && !wizard.page_has_pick()
                            {
                                "Use default"
                            } else if mcp_field.is_some_and(|schema| !schema.required)
                                && typed_empty
                                && !wizard.page_has_pick()
                            {
                                "Skip"
                            } else {
                                "Next"
                            },
                        )
                        .id("wizard-submit")
                        .px(px(16.0))
                        .when(!can_advance, |el| el.opacity(0.4))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if retryable {
                                this.retry_async_submission(cx);
                            } else {
                                this.wizard_advance(cx);
                            }
                        })),
                    ),
            );
        crate::frost::frosted(surface_radius, 16.0, panel).into_any_element()
    }

    fn render_stash_menu(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        if !self.stash_menu_open {
            return None;
        }
        let busy = self.stash_task.is_some();
        let popup_theme = theme.for_popup();
        let popup_focus = popup_theme.border_strong;
        let mut rows = div()
            .id("composer-stash-list")
            .w_full()
            .max_h(px(220.0))
            .overflow_y_scroll()
            .rounded(px(crate::popover::CARD_RADIUS))
            .border_1()
            .border_color(theme.border)
            .bg(crate::popover::surface_bg(theme))
            .when(!theme.is_frost(), |panel| panel.shadow_lg())
            .p(px(crate::popover::CARD_INSET))
            .flex()
            .flex_col()
            .gap(px(2.0));
        if self.stash_entries.is_empty() {
            rows = rows.child(
                div()
                    .px(px(8.0))
                    .py(px(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(popup_theme.text_muted)
                    .child(if busy {
                        "Loading saved prompts…"
                    } else {
                        "No saved prompts yet"
                    }),
            );
        }
        for entry in self.stash_entries.iter().cloned() {
            let restore = entry.clone();
            let id = entry.id.clone();
            let attachment_count = entry.attachments.len();
            let label = entry.label.clone();
            rows = rows.child(
                div()
                    .id(SharedString::from(format!("composer-stash-row-{id}")))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(8.0))
                    .when(!busy, |row| {
                        row.hover(|style| style.bg(theme.glass_hover()))
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("composer-stash-restore-{id}")))
                            .role(gpui::Role::Button)
                            .aria_label(format!("Restore saved prompt: {label}"))
                            .flex_1()
                            .min_w_0()
                            .px(px(8.0))
                            .py(px(8.0))
                            .when(!busy, |button| {
                                button
                                    .tab_index(0)
                                    .focus_visible(move |style| {
                                        crate::theme::focus_ring(style, popup_focus)
                                    })
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.restore_stashed_prompt(restore.clone(), cx);
                                    }))
                            })
                            .when(busy, |button| button.opacity(0.5))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(crate::typography::ui_rems(12.0))
                                    .text_color(popup_theme.text)
                                    .child(entry.label),
                            ),
                    )
                    .when(attachment_count > 0, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(popup_theme.text_muted)
                                .child(format!(
                                    "{attachment_count} file{}",
                                    if attachment_count == 1 { "" } else { "s" }
                                )),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("composer-stash-delete-{id}")))
                            .role(gpui::Role::Button)
                            .aria_label(format!("Delete saved prompt: {label}"))
                            .px(px(8.0))
                            .py(px(8.0))
                            .rounded(px(6.0))
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(popup_theme.text_muted)
                            .when(!busy, |button| {
                                button
                                    .tab_index(0)
                                    .focus_visible(move |style| {
                                        crate::theme::focus_ring(style, popup_focus)
                                    })
                                    .cursor_pointer()
                                    .hover(|style| {
                                        style.text_color(
                                            theme.readable_tone(theme.danger_muted, 1.0),
                                        )
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete_stashed_prompt(id.clone(), cx);
                                    }))
                            })
                            .when(busy, |button| button.opacity(0.5))
                            .child("Delete"),
                    ),
            );
        }
        Some(rows)
    }

    /// Whether the compact controls tray (a narrow composer's More) is open.
    pub fn controls_tray_open(&self) -> bool {
        self.controls_tray_open
    }

    /// Escape (the shell's capture chain, after an open picker): close the
    /// tray without reaching the stop path or the draft.
    pub fn close_controls_tray(&mut self, cx: &mut Context<Self>) {
        self.controls_tray_open = false;
        self.controls_tray_press_was_open = false;
        cx.notify();
    }

    /// The compact controls row's leading slot: a tray button that opens
    /// Attach and the picker group (model, access, Goal, usage, work) above
    /// the pill. Picker popovers open from inside the tray; it stays open
    /// while one of them is.
    fn render_controls_tray(
        &mut self,
        attach: gpui::Stateful<gpui::Div>,
        pickers: gpui::Div,
        probe_scope: Option<String>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let open = self.controls_tray_open;
        let hover: SharedString = format!("composer-tray-{}", cx.entity_id()).into();
        let button = div()
            .id("composer-controls-tray")
            .role(gpui::Role::Button)
            .aria_label("Composer controls")
            .aria_expanded(open)
            .tab_index(0)
            .size(px(COMPACT_BUTTON))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .bg(if open {
                crate::theme::ink(0.10)
            } else {
                motion::hover_blend(&hover, gpui::transparent_black(), crate::theme::ink(0.10))
            })
            .on_hover(motion::hover_listener(hover))
            .capture_any_mouse_down(cx.listener(|this, _, _, _| {
                this.controls_tray_press_was_open = this.controls_tray_open;
            }))
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                let pressed_open = std::mem::take(&mut this.controls_tray_press_was_open);
                this.controls_tray_open = !pressed_open && !this.controls_tray_open;
                cx.notify();
            }))
            .tooltip(|_, cx| crate::pickers::ControlTooltip::view("Composer controls", None, cx))
            .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
            .relative()
            .child(
                crate::icons::icon(crate::icons::MORE_HORIZONTAL)
                    .size(px(16.0))
                    .text_color(theme.text_muted),
            )
            .when_some(probe_scope, |el, scope| {
                el.child(crate::perf_counters::probe(format!("{scope}:more")))
            });
        let button = if open {
            let tray = crate::popover::popover_card(theme)
                .id("composer-controls-tray-card")
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    // A picker opened from the tray keeps it open.
                    if !this.pickers.read(cx).is_open() {
                        this.controls_tray_open = false;
                        cx.notify();
                    }
                }))
                .p(px(6.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(ACTION_UTILITY_GAP))
                .child(attach)
                .child(pickers);
            button.child(crate::popover::anchored_menu_above(
                "composer-controls-tray-menu",
                tray.into_any_element(),
                None,
            ))
        } else {
            button
        };
        button.into_any_element()
    }

    fn render_send_button(
        &mut self,
        mode: SendButtonMode,
        size: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = Theme::of(cx);
        // Luna composer-actions.tsx: a size-7 filled circle — up-arrow to
        // send/queue, a dark rounded square on the same light circle to stop.
        match mode {
            SendButtonMode::Stop => div()
                .id("composer-stop")
                .size(px(size))
                .flex_none()
                .rounded_full()
                .bg(theme.text)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .on_click(cx.listener(|this, _, _, cx| this.interrupt_selected(cx)))
                .child(div().size(px(11.0)).rounded(px(3.0)).bg(theme.bg))
                .into_any_element(),
            SendButtonMode::Send | SendButtonMode::Queue => {
                // Share the submission guard with Enter, including pending
                // edits and the new-session runnable-agent check.
                let blocked = self.send_blocked(cx);
                div()
                    .id("composer-send")
                    .size(px(size))
                    .flex_none()
                    .rounded_full()
                    .bg(theme.text)
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(blocked, |el| el.opacity(0.35))
                    .when(!blocked, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.opacity(0.85))
                            .on_click(cx.listener(|this, _, _, cx| this.on_submit(cx)))
                    })
                    .child(
                        crate::icons::icon(crate::icons::ARROW_UP)
                            .size(px(14.0))
                            .text_color(theme.bg),
                    )
                    .into_any_element()
            }
        }
    }
}

/// The completion popups' floating rails run through
/// [`crate::popover::rail`]: the shared `popup_bar` state plus whichever
/// popup's rows are mounted — see [`Composer::active_popup_scroll`].
impl crate::popover::ScrollRailHost for Composer {
    fn rail_bar(&mut self) -> &mut crate::popover::MenuScrollbarState {
        &mut self.popup_bar
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.active_popup_scroll()
    }
}

/// Focus lands on the prompt input (window-level focus fallbacks — e.g. after
/// the focused terminal panel is hidden — route here).
impl Focusable for Composer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.last_painted = Some(Instant::now());
        if std::mem::take(&mut self.pending_scan_deferred) {
            // Shown again: catch up on the history changes skipped while
            // hidden, right after this render (the same path as an
            // app-state change).
            cx.defer_in(window, |this, _, cx| this.on_state_changed(cx));
        }
        if self.focus_pending {
            self.focus_pending = false;
            let focus = self.input.focus_handle(cx);
            window.focus(&focus, cx);
        }
        let theme = Theme::of(cx).clone();
        let wizard_active = self.wizard.is_some();
        // Focus on the popup's own Retry control keeps the completion open;
        // anywhere else closes it. Never leave focus on a vanished Retry.
        let retry_focused = self.native_retry_focused(window);
        if self.mention.token.is_some()
            && !retry_focused
            && !self.input.focus_handle(cx).is_focused(window)
        {
            self.reset_mention(None, cx);
        } else if self.mention.token.is_none() && retry_focused {
            let focus = self.input.focus_handle(cx);
            window.focus(&focus, cx);
        }
        if self.slash.token.is_some() && !self.input.focus_handle(cx).is_focused(window) {
            self.reset_slash(None, cx);
        }
        if self.chat_reference.token.is_some() && !self.input.focus_handle(cx).is_focused(window) {
            self.reset_chat_references(None, cx);
        }
        let mode = self.button_mode(cx);
        // Shape the current draft before sizing the pill. Waiting for the child
        // layout leaves the parent using the previous edit's height.
        self.input.update(cx, |input, cx| {
            if input.needs_measure && input.last_width > 0.0 {
                let mut style = window.text_style();
                style.font_family = theme.font_sans.clone();
                style.font_size = crate::typography::ui_rems(INPUT_TEXT_SIZE).into();
                style.color = if input.content.is_empty() {
                    theme.text_faint
                } else {
                    theme.text
                };
                input.layout_text(px(input.last_width), &style, window, cx);
            }
        });
        let content_height = self.input.read(cx).measured_content_height();
        // Every established composer uses the two-row layout (input above, a
        // controls row below) so the main thread, side chats and child chats
        // share one geometry at every width. New chats render the taller
        // blank-canvas pill; only route changes morph between the two.
        let new_chat = self.binding.chat_id(self.state.read(cx)).is_none();
        // Morph clock in ms; dividing by the measurement knob stretches the
        // timeline exactly like shell.rs eval_tween's scaled duration.
        let now_ms = self.morph_clock.elapsed().as_secs_f32() * 1000.0 / motion::speed_scale();
        let route_snap = self
            .route_snap_until
            .is_some_and(|until| Instant::now() < until);
        let dock_height_changed = std::mem::take(&mut self.dock_height_changed);
        self.flip_morph =
            if dock_height_changed || self.dock_frame.is_some_and(|frame| frame.active) {
                None
            } else {
                flip_morph_step(
                    self.flip_morph,
                    false,
                    self.last_rendered_height,
                    now_ms,
                    motion::reduced_motion(cx),
                    route_snap,
                )
            };

        let recoverable_available = self
            .recoverable_submissions
            .get(&self.current_key)
            .is_some_and(|entries| !entries.is_empty());
        // Chat-scoped failures render only under their own chat; a global
        // failure (no key) renders everywhere. The dedicated saved-message
        // control already explains its generic recovery state.
        let failure =
            self.failure
                .clone()
                .filter(|_| {
                    self.failure_key
                        .as_ref()
                        .is_none_or(|key| *key == self.current_key)
                })
                .filter(|message| {
                    !recoverable_available
                        || !self.recovery_notice.as_ref().is_some_and(|(key, notice)| {
                            key == &self.current_key && notice == message
                        })
                });
        let draft_save_failed = self
            .draft_save_failure
            .as_ref()
            .is_some_and(|(key, message)| {
                key == &self.current_key && failure.as_ref() == Some(message)
            });
        let draft_merge_saving = self
            .draft_merge_notice
            .as_ref()
            .is_some_and(|(key, message)| {
                key == &self.current_key && failure.as_ref() == Some(message)
            });
        let delivery_unconfirmed = self
            .delivery_failure
            .as_ref()
            .is_some_and(|(_, message)| failure.as_ref() == Some(message));
        let retry_draft_key = self.current_key.clone();
        // Composer honesty: when the target's delivery path is degraded, say
        // UP FRONT that a send will queue (a durable local write delivered on
        // reconnect) instead of letting the button imply instant delivery.
        let queue_notice: Option<(SharedString, bool)> = {
            use luna_proto::ConnectivityState as S;
            let state = self.state.read(cx);
            let degraded = match self.binding.chat_id(state) {
                Some(id) => state.chat_delivery_degraded(id),
                None => {
                    // New-chat canvas: judge by the picked target device.
                    let remote_target = state
                        .effective_device_id()
                        .is_some_and(|id| state.local_device_id.as_deref() != Some(id.as_str()));
                    remote_target
                        && (matches!(state.connectivity.state, S::Offline | S::Reconnecting)
                            || state
                                .effective_device_id()
                                .is_some_and(|id| !state.device_online(&id, chrono::Utc::now())))
                }
            };
            let offline = state.connectivity.state == S::Offline;
            degraded.then(|| {
                let text: SharedString = if offline {
                    "Offline — messages will send when you're back online.".into()
                } else {
                    "Messages will send once the connection recovers.".into()
                };
                (text, offline)
            })
        };
        // Narrow composers (slice 6) tighten from the CURRENT available
        // width: the column gutter, the input padding and the controls row.
        let available = self.last_available_width.unwrap_or(COMPOSER_MAX_WIDTH);
        let gutter = composer_gutter(available);
        let compact = !controls_row_fits(available - 2.0 * gutter);
        if !compact && self.controls_tray_open {
            self.controls_tray_open = false;
        }
        // Centered composer column (luna `mx-auto w-full max-w-3xl`).
        let container =
            div()
                .w_full()
                .max_w(px(COMPOSER_MAX_WIDTH))
                .mx_auto()
                .flex()
                .flex_col()
                .gap(px(Theme::SPACE_SM))
                .px(px(gutter))
                .pb(px(Theme::SPACE_LG))
                .when_some(failure, |el, message| {
                    let offline = message.as_ref() == "Engine not connected";
                    let warning = offline || delivery_unconfirmed || draft_merge_saving;
                    let action_message = message.clone();
                    let key_for_click = retry_draft_key.clone();
                    el.child(
                        notice_chip(
                            &theme,
                            warning,
                            if delivery_unconfirmed {
                                "Checking delivery"
                            } else if offline {
                                "Connection"
                            } else if draft_save_failed {
                                "Draft not saved"
                            } else if draft_merge_saving {
                                "Saving draft"
                            } else {
                                "Action failed"
                            },
                            message,
                        )
                        .child(
                            div()
                                .id("composer-failure-action")
                                .role(gpui::Role::Button)
                                .aria_label(if draft_save_failed {
                                    "Retry saving draft"
                                } else {
                                    "Dismiss message"
                                })
                                .tab_index(0)
                                .cursor_pointer()
                                .flex()
                                .justify_end()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(if draft_save_failed {
                                    "Retry save"
                                } else {
                                    "Dismiss"
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.activate_failure_notice(
                                        &key_for_click,
                                        &action_message,
                                        draft_save_failed,
                                        cx,
                                    );
                                }))
                                .on_key_down(crate::swallow_activation_key),
                        )
                        .id("composer-failure")
                        .mx(px(4.0))
                        .mt(px(6.0)),
                    )
                })
                .when(recoverable_available, |el| {
                    let hover_bg = theme.glass_hover();
                    el.child(
                        div()
                            .id("composer-recover-unsent")
                            .mx(px(4.0))
                            .mt(px(6.0))
                            .px(px(12.0))
                            .py(px(10.0))
                            .rounded(px(10.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.surface_card)
                            .flex()
                            .flex_col()
                            .gap(px(7.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(
                                        crate::icons::icon(crate::icons::ARCHIVE_UP_MINIMALISTIC)
                                            .size(px(14.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child("Unsent message saved"),
                            )
                            .child(div().text_color(theme.text_muted).child(
                                "An earlier message was not sent. Its saved copy is available.",
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .gap(px(12.0))
                                    .child(
                                        div()
                                            .id("composer-discard-unsent")
                                            .role(gpui::Role::Button)
                                            .aria_label("Discard saved message")
                                            .tab_index(0)
                                            .cursor_pointer()
                                            .text_color(theme.text_muted)
                                            .hover(move |style| style.bg(hover_bg))
                                            .child("Discard saved copy")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.discard_latest_submission(cx);
                                            }))
                                            .on_key_down(crate::swallow_activation_key),
                                    )
                                    .child(
                                        div()
                                            .id("composer-restore-unsent")
                                            .role(gpui::Role::Button)
                                            .aria_label("Restore saved message into draft")
                                            .tab_index(0)
                                            .cursor_pointer()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(theme.readable_tone(theme.accent, 1.0))
                                            .hover(move |style| style.bg(hover_bg))
                                            .child("Restore message")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.recover_latest_submission(cx);
                                            }))
                                            .on_key_down(crate::swallow_activation_key),
                                    ),
                            ),
                    )
                })
                .when_some(queue_notice, |el, (notice, offline)| {
                    // Not a warning box (v0.2.12 feedback: the amber Notice read
                    // as an error and flashed on every blip — pre-grace). One
                    // quiet caption line, amber dot only for hard offline; it
                    // clears itself the moment the path heals.
                    let dot = if offline {
                        theme.warning
                    } else {
                        theme.text_faint
                    };
                    el.child(crate::motion::fade_in(
                        "composer-queue-notice",
                        div()
                            .id("composer-queue-notice")
                            .mx(px(8.0))
                            .mt(px(6.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(crate::typography::ui_rems(11.0))
                            .line_height(crate::typography::ui_rems(14.0))
                            .text_color(theme.text_faint)
                            .child(div().size(px(5.0)).rounded_full().bg(dot))
                            .child(div().min_w_0().truncate().child(notice)),
                    ))
                });

        let container = if wizard_active {
            let wizard = self.render_wizard(cx);
            container.child(motion::fade_quick("composer-wizard", div().child(wizard)))
        } else {
            container
        };

        // The chat's goal rides on top of the stack (BB's goal card): a tray
        // tucked behind the queue tray or the composer, present only while a
        // goal exists or is being created.
        self.sync_goal_placeholder(cx);
        let container = container.when_some(self.render_goal_card(window, cx), |el, card| {
            el.child(motion::fade_quick(
                "composer-goal",
                div()
                    .mx(px(QUEUE_SIDE_INSET))
                    .mb(px(-(Theme::SPACE_SM + QUEUE_COMPOSER_OVERLAP)))
                    .child(card),
            ))
        });
        // What is waiting to be sent, stacked directly above the box it was
        // typed in — the queue is a property of this composer, not a panel
        // somewhere else.
        let container = container.when_some(self.render_queue_panel(window, cx), |el, panel| {
            el.child(motion::fade_quick(
                "composer-queue",
                div()
                    .mx(px(QUEUE_SIDE_INSET))
                    // Cancel the column gap, then tuck the tray one pixel
                    // behind the composer painted after it.
                    .mb(px(-(Theme::SPACE_SM + QUEUE_COMPOSER_OVERLAP)))
                    .child(panel),
            ))
        });
        // Escape backs out of a queue-row edit (the row keeps its old text).
        // Bound here rather than in the input: the input's own Escape belongs
        // to the mention/slash popups, which outrank this while they're open.
        let container = container.when(self.editing_queued.is_some(), |el| {
            el.on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape"
                    && this.mention.token.is_none()
                    && this.slash.token.is_none()
                    && this.cancel_queue_edit(cx)
                {
                    cx.stop_propagation();
                }
            }))
        });

        let dock_amount = self.dock_frame.map_or(0.0, |frame| frame.amount);
        let dock_height = |amount: f32| {
            motion::lerp(
                composer_total_height(content_height),
                session_total_height(content_height),
                amount,
            )
        };

        // Committed-height morph: only the pill's height animates; the layout
        // is the destination's from the first frame. Staged attachments add
        // the wrap strip's height (attachment-ui.tsx AttachmentStrip sits
        // above the input row). Wrap against the pill's content width,
        // accounting for the outer container padding and 1px borders.
        let staged_count = self.staged().len();
        let strip_width_hint =
            self.last_available_width.unwrap_or(COMPOSER_MAX_WIDTH) - 2.0 * Theme::SPACE_LG - 2.0;
        let appshot_count = self.staged_appshots().len();
        let strip_h = attachment_strip_height(staged_count, strip_width_hint);
        let comment_strip_h = comment_strip_height(self.staged_comments(cx).len());
        let base_height = if self.dock_frame.is_some() {
            dock_height(dock_amount)
        } else if new_chat {
            composer_total_height(content_height)
        } else {
            session_total_height(content_height)
        };
        let target_height =
            base_height + strip_h + appshot_strip_height(appshot_count) + comment_strip_h;
        let coordinated_route_morph = self
            .flip_morph
            .filter(|m| m.spec == motion::NEW_THREAD_TRANSITION && !m.done(now_ms));
        // The route state commits before its shared-element animation begins.
        // Reconstruct the departing chrome at t=0, then progressively trade
        // it for the destination chrome so neither route changes the outer
        // composer geometry in a single frame.
        let new_thread_chrome = self
            .dock_frame
            .map(|frame| frame.selectors())
            .unwrap_or_else(|| {
                coordinated_route_morph.map_or_else(
                    || if new_chat { 1.0 } else { 0.0 },
                    |morph| {
                        let progress = morph.progress(now_ms);
                        if new_chat { progress } else { 1.0 - progress }
                    },
                )
            });
        let (new_thread_chrome_opacity, session_chrome_opacity) = self.dock_frame.map_or_else(
            || route_chrome_opacities(new_thread_chrome),
            |frame| (frame.selectors(), frame.footer()),
        );
        self.height_morph =
            if dock_height_changed || self.dock_frame.is_some_and(|frame| frame.active) {
                None
            } else if coordinated_route_morph.is_some() {
                coordinated_route_morph
            } else {
                flip_morph_step(
                    self.height_morph,
                    (target_height - self.last_target_height).abs() > 0.5,
                    self.last_rendered_height,
                    now_ms,
                    motion::reduced_motion(cx),
                    route_snap,
                )
            };
        self.last_target_height = target_height;
        let pill_height = self
            .height_morph
            .map_or(target_height, |m| m.height(target_height, now_ms));
        if self.height_morph.is_some() {
            crate::perf_counters::count("frame.request", "composer#2", 1, 0);
            window.request_animation_frame();
        }
        if self.flip_morph.is_some_and(|m| m.done(now_ms)) {
            self.flip_morph = None;
        } else if self.flip_morph.is_some() {
            // Manual tween drive: keep frames coming (shell.rs motion_active).
            crate::perf_counters::count("frame.request", "composer#3", 1, 0);
            window.request_animation_frame();
        }
        self.last_rendered_height = pill_height;
        self.dock_clearance_correction = self.dock_frame.map_or(0.0, |frame| {
            dock_height(if frame.docked { 1.0 } else { 0.0 })
                + strip_h
                + appshot_strip_height(appshot_count)
                + comment_strip_h
                - pill_height
        });
        let surface_radius = composer_surface_radius(dock_amount);
        let textarea_height = (pill_height
            - strip_h
            - appshot_strip_height(appshot_count)
            - comment_strip_h
            - PILL_BORDER_V
            - ACTIONS_ROW_HEIGHT)
            .max(0.0);
        self.input.update(cx, |input, cx| {
            let height = (textarea_height - TEXTAREA_PAD_V).max(0.0);
            let settled_height =
                (base_height - PILL_BORDER_V - ACTIONS_ROW_HEIGHT - TEXTAREA_PAD_V).max(0.0);
            let resizing = self.height_morph.is_some();
            if input.viewport_height != Some(height)
                || input.settled_viewport_height != Some(settled_height)
                || input.resizing != resizing
                || input.overflow_top_padding != TEXTAREA_PAD_TOP
            {
                input.resizing = resizing;
                input.overflow_top_padding = TEXTAREA_PAD_TOP;
                input.viewport_height = Some(height);
                input.settled_viewport_height = Some(settled_height);
                cx.notify();
            }
        });

        // Opt-in layout probes (slice 6 narrow-pane checks): painted bounds
        // of each control; layout-neutral and absent unless layout probes
        // are on (never while measuring).
        let probe_scope = crate::perf_counters::probes_enabled()
            .then(|| format!("composer:{}", self.perf_scope(cx)));
        let send_button =
            self.render_send_button(mode, if compact { COMPACT_BUTTON } else { ROW_BUTTON }, cx);
        let send_button = match &probe_scope {
            Some(scope) => div()
                .flex_none()
                .relative()
                .child(send_button)
                .child(crate::perf_counters::probe(format!("{scope}:send")))
                .into_any_element(),
            None => send_button,
        };
        // Attach button — opens the native image picker (the original's hidden
        // `<input type=file accept="image/*" multiple>`); paste/drop also feed
        // the same strip. The leading utility group owns the spacing between
        // this button and the model picker. Hover keys are scoped to this
        // composer: a pane composer must not light up with the main one.
        let attach_hover: SharedString = format!("composer-attach-{}", cx.entity_id()).into();
        let attach = div()
            .id("composer-attach")
            .size(px(28.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            // luna composer-actions.tsx attach: `transition-colors`.
            .bg(motion::hover_blend(
                &attach_hover,
                gpui::transparent_black(),
                crate::theme::ink(0.10),
            ))
            .on_hover(motion::hover_listener(attach_hover))
            .on_click(cx.listener(|this, _, _, cx| this.open_file_picker(cx)))
            .tooltip(|_, cx| crate::pickers::ControlTooltip::view("Attach files", None, cx))
            .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
            .child(
                crate::icons::icon(crate::icons::PAPERCLIP)
                    .size(px(16.0))
                    // The source path's painted bounds are centered at x=11
                    // inside a 24px viewbox. Correct that optical offset while
                    // keeping the 28px hit target geometrically centered.
                    .relative()
                    .left(px(1.0))
                    .text_color(theme.text_muted),
            );
        let attach = match &probe_scope {
            Some(scope) => attach
                .relative()
                .child(crate::perf_counters::probe(format!("{scope}:attach"))),
            None => attach,
        };
        // Staged-thumbnail strip (attachment-ui.tsx AttachmentStrip), above
        // the input inside the pill.
        let strip = self.render_attachment_strip(&theme, cx);
        let appshot_strip = self.render_appshot_strip(&theme, window, cx);
        let comments_chip = self.render_comments_chip(&theme, cx);

        // Compensate for the transcript canvas beneath the frosted surface.
        // Keep the opaque fallback when frost is disabled or unsupported.
        // Over new-thread artwork the pill sits on the image's half-strength
        // cutout reveal: thicken its plate so text reads on any image.
        let artwork_strength = (new_thread_chrome_opacity > 0.0
            && crate::settings::current(cx)
                .new_thread_composer_background
                .is_some())
        .then(|| {
            crate::shell::new_thread_background_opacity(theme.is_frost())
                * crate::new_thread_background_mask::CUTOUT_REVEAL_OPACITY
        });
        let pill = composer_surface(&theme, surface_radius)
            .when_some(artwork_strength, |pill, strength| {
                pill.bg(theme.artwork_composer_plate(strength))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    // Padding and action controls are part of the text composer.
                    // Open menus keep their own keyboard/search focus.
                    if !this.pickers.read(cx).is_open() {
                        window.focus(&this.input.focus_handle(cx), cx);
                    }
                }),
            );
        let surface_width = self
            .surface_bounds
            .get()
            .map_or(strip_width_hint + PILL_BORDER_V, |bounds| {
                f32::from(bounds.size.width)
            });
        // One controls row for every composer width: whole elements give way
        // in priority order instead of ellipsizing mid-word. The Goal label
        // collapses to its icon first, then the effort suffix leaves the
        // model chip (both stay in the tooltips); the model name and the
        // access icon always remain.
        let goal_widths = self.goal_mode_chip_widths(window, cx);
        // The bound session's context window, beside Goal in this row (Main,
        // a child and a fork alike); no session, no chip.
        let usage_chip = {
            let state = self.state.read(cx);
            (crate::settings::current(cx).show_context_usage
                && self.binding.chat_row(state).is_some())
            .then(|| {
                (
                    self.binding.context_usage(state),
                    self.binding.context_control(state).compact.status,
                )
            })
        };
        let usage_widths = usage_chip.map(|(usage, status)| {
            crate::context_usage::chip_widths(
                &crate::context_usage::label(usage, status),
                window,
                &theme,
            )
        });
        let (model_full, model_name_only) = self.pickers.read(cx).model_chip_widths(window, cx);
        // The Work chip (focused chat only) takes a fixed slot first.
        let work_width = self.work_chip_width(cx);
        // Compact: the chips live in the tray, with room for their labels.
        let row_budget = if compact {
            TRAY_CHIP_BUDGET
        } else {
            controls_row_budget(surface_width)
        } - if work_width > 0.0 {
            work_width + CHIP_GAP
        } else {
            0.0
        };
        let fit = controls_row_fit(
            row_budget,
            model_full,
            model_name_only,
            goal_widths,
            usage_widths,
        );
        self.controls_row = Some(ControlsRowMeasure {
            budget: row_budget,
            model_full,
            model_name_only,
            goal: goal_widths,
            usage: usage_widths,
            fit,
        });
        self.pickers
            .update(cx, |pickers, cx| pickers.set_model_chip_fit(fit.model, cx));
        let goal_mode_chip = self.render_goal_mode_chip(fit.goal_icon_only, cx);
        let usage_chip = usage_chip.map(|(usage, status)| {
            crate::context_usage::render(
                usage,
                status,
                self.state.clone(),
                self.binding.clone(),
                &theme,
                !fit.usage_icon_only,
            )
        });
        let work_chip = self.render_work_chip(window, &theme, cx);
        let model_picker = div()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(CHIP_GAP))
            .child(div().min_w_0().child(self.pickers.clone()))
            .children(goal_mode_chip)
            .children(usage_chip)
            .children(work_chip)
            .when_some(probe_scope.clone(), |el, scope| {
                el.relative()
                    .child(crate::perf_counters::probe(format!("{scope}:pickers")))
            });
        // Textarea on top (`px-4 pb-1 pt-4`), actions row (12px bottom + 2px
        // top, 32px chips → 46px) ABSOLUTE at the pill's stationary bottom —
        // the pill sits at the bottom of its column and grows upward, so the
        // controls keep a constant screen-y while the text viewport follows
        // the animated height and never paints over them.
        let body = pill
            .h(px(pill_height))
            .overflow_hidden()
            .relative()
            .flex()
            .flex_col()
            .children(comments_chip)
            .children(appshot_strip)
            .children(strip)
            .child(
                div()
                    .h(px(textarea_height))
                    .flex_none()
                    .overflow_hidden()
                    .px(px(input_side_padding(available)))
                    .pt(px(TEXTAREA_PAD_TOP))
                    .pb(px(TEXTAREA_PAD_V - TEXTAREA_PAD_TOP))
                    .child(match probe_scope.clone() {
                        // Probe the input itself (its usable text width),
                        // not the padded container.
                        Some(scope) => self
                            .render_input_with_completion()
                            .child(crate::perf_counters::probe(format!("{scope}:input")))
                            .into_any_element(),
                        None => self.render_input_with_completion().into_any_element(),
                    }),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(ACTIONS_ROW_HEIGHT))
                    .flex()
                    .flex_row()
                    .items_center()
                    // The attachment belongs to the utility pickers, while
                    // Send has a larger structural separation.
                    .gap(px(if compact { 2.0 } else { ACTION_PRIMARY_GAP }))
                    .pl(px(if compact {
                        COMPACT_INSET
                    } else {
                        ACTIONS_ROW_INSET
                    }))
                    .pr(px(if compact {
                        COMPACT_INSET
                    } else {
                        ACTIONS_ROW_INSET
                    }))
                    .pt(px(2.0))
                    .pb(px(if compact { 8.0 } else { 12.0 }))
                    .child(if compact {
                        // Narrow: one tray button holds Attach and the
                        // pickers (all their popovers and options intact).
                        self.render_controls_tray(
                            attach,
                            model_picker,
                            probe_scope.clone(),
                            &theme,
                            cx,
                        )
                    } else {
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(ACTION_UTILITY_GAP))
                            .child(attach)
                            .child(model_picker)
                            .into_any_element()
                    })
                    .child(send_button),
            );
        // With new-thread artwork behind the composer, the selector labels
        // would sit directly on arbitrary image pixels (feathered above the
        // pill, half-strength below it). Give their rows the composer's own
        // plate so the text floor holds on light or dark artwork.
        let artwork_plate = {
            let artwork = crate::settings::current(cx)
                .new_thread_composer_background
                .is_some();
            let plate = Theme::of(cx).artwork_selector_plate();
            move |row: gpui::AnyElement| -> gpui::AnyElement {
                if artwork {
                    div()
                        .px(px(4.0))
                        .py(px(1.0))
                        .rounded(px(8.0))
                        .bg(plate)
                        .child(row)
                        .into_any_element()
                } else {
                    row
                }
            }
        };
        let new_thread_target_selectors = (new_thread_chrome_opacity > 0.0).then(|| {
            artwork_plate(self.pickers.update(cx, |pickers, cx| {
                pickers.render_new_thread_target_selectors(cx)
            }))
        });
        let new_thread_git_selectors = (new_thread_chrome_opacity > 0.0)
            .then(|| {
                self.pickers.update(cx, |pickers, cx| {
                    pickers.render_new_thread_git_selectors(cx)
                })
            })
            .flatten()
            .map(artwork_plate);
        let has_new_thread_git_selectors = self
            .state
            .read(cx)
            .selected_space_row()
            .is_some_and(|space| space.git_detected);
        // The file dropzone lives in the shell (the whole conversation column,
        // not just the pill — shell.rs `chat-dropzone`); drops land back here
        // via `add_paths`.
        // Frosted: the pill backdrop-blurs the transcript scrolling under it
        // (the popover glass treatment; radius matches the pill's rounding).
        // The shell keeps this entity under one parent on both routes. The
        // surface itself never fades, and frost follows the same morph radius.
        let pill_surface = div()
            .relative()
            .id("composer-surface")
            .when_some(probe_scope.clone(), |el, scope| {
                el.child(crate::perf_counters::probe(format!("{scope}:pill")))
            })
            .child(crate::frost::frosted(surface_radius, 16.0, body))
            .child({
                let measured = self.surface_bounds.clone();
                // All prepaint completes before any paint. The background
                // reads this cell during paint, never last frame's geometry.
                gpui::canvas(
                    move |bounds, _, _| measured.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
            })
            // Both completion popups span the full pill width above it —
            // the file-mention and slash tokens are mutually exclusive.
            .children(self.render_file_mention_popup(&theme, window, cx))
            .children(self.render_chat_reference_popup(&theme, cx))
            .children(self.render_slash_popup(&theme, cx));
        // Restore the original chip-only selector treatment: destination at
        // the top-right, no surrounding surface. Cancel the column gap as the
        // row collapses so the pill never jumps at the route boundary.
        let container = if self.dock_frame.is_some() {
            // Floating selectors share the surface's origin and never change its height.
            container.relative().child(
                div()
                    .id("dock-target-selectors")
                    .absolute()
                    .top(px(-28.0))
                    .left(px(Theme::SPACE_LG + 10.0))
                    .right(px(Theme::SPACE_LG + 10.0))
                    .h(px(NEW_THREAD_SELECTOR_ROW_HEIGHT))
                    .flex()
                    .items_start()
                    .justify_end()
                    .opacity(new_thread_chrome_opacity)
                    .children(new_thread_target_selectors),
            )
        } else if new_thread_chrome > 0.0 {
            container.child(
                div()
                    .w_full()
                    .h(px(NEW_THREAD_SELECTOR_ROW_HEIGHT * new_thread_chrome))
                    .mb(px(-Theme::SPACE_SM * (1.0 - new_thread_chrome)))
                    .px(px(10.0))
                    .flex()
                    .items_start()
                    .justify_end()
                    .opacity(new_thread_chrome_opacity)
                    .children(new_thread_target_selectors),
            )
        } else {
            container
        };
        let stash_menu = self.render_stash_menu(&theme, cx);
        let container = container.children(stash_menu).child(pill_surface);

        // The lower slot keeps a stable footprint for Git projects while its
        // old floating checkout/ref controls dissolve into the session footer.
        // Non-Git sessions grow the slot continuously from zero.
        let session_chrome = 1.0 - new_thread_chrome;
        // In the workbench frame the session's checkout/branch live in the
        // frame row and the context window in the controls row, so a
        // session has nothing under its pill: the slot gives its height
        // back as the session chrome comes in, and the pill sits lower.
        let framed_session =
            self.frame_footer && self.binding.chat_row(self.state.read(cx)).is_some();
        let bottom_slot = if framed_session {
            if has_new_thread_git_selectors {
                new_thread_chrome
            } else {
                0.0
            }
        } else if has_new_thread_git_selectors || self.dock_frame.is_some() {
            1.0
        } else {
            session_chrome
        };
        let container = if bottom_slot > 0.0 {
            let footer = (session_chrome_opacity > 0.0 && !framed_session).then(|| {
                self.pickers
                    .update(cx, |pickers, cx| pickers.render_footer(cx))
            });
            container.child(
                div()
                    .w_full()
                    .h(px(SESSION_FOOTER_HEIGHT * bottom_slot))
                    .mt(px(-Theme::SPACE_SM * (1.0 - bottom_slot)))
                    .mb(px(-Theme::SPACE_SM * bottom_slot))
                    .relative()
                    .when(new_thread_chrome_opacity > 0.0, |slot| {
                        slot.child(
                            div()
                                .absolute()
                                .inset_0()
                                .px(px(10.0))
                                .flex()
                                .items_center()
                                .opacity(new_thread_chrome_opacity)
                                .children(new_thread_git_selectors),
                        )
                    })
                    .when(session_chrome_opacity > 0.0, |slot| {
                        slot.child(
                            div()
                                .absolute()
                                .inset_0()
                                .w_full()
                                .h(px(SESSION_FOOTER_HEIGHT))
                                .flex()
                                .items_center()
                                .opacity(session_chrome_opacity)
                                .child(div().flex_1().min_w_0().children(footer.flatten())),
                        )
                    }),
            )
        } else {
            container
        };
        // Full-size preview of a staged thumbnail (AttachmentPreviewDialog).
        if let Some(preview) = self.preview.clone() {
            if std::mem::take(&mut self.preview_focus_pending) {
                window.focus(&self.preview_focus, cx);
            }
            let weak = cx.weak_entity();
            return container.child(attachments::lightbox(
                window,
                &preview,
                &self.preview_focus,
                move |window, cx| {
                    // Hand focus back to the input so typing (and the next
                    // Escape) lands where it did before the lightbox opened.
                    if let Ok(input_focus) = weak.update(cx, |this, cx| {
                        this.preview = None;
                        cx.notify();
                        this.input.read(cx).focus_handle.clone()
                    }) {
                        window.focus(&input_focus, cx);
                    }
                },
                cx,
            ));
        }
        container
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use luna_rpc::{RpcReply, RpcService};
    use tokio::sync::oneshot;

    fn composer_focus_window(
        cx: &mut gpui::TestAppContext,
    ) -> (tempfile::TempDir, gpui::WindowHandle<Composer>) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::app_menus::init(cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Composer::new(state, cx)
        });
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        (dir, window)
    }

    struct DraftTestRpc {
        drafts: Mutex<HashMap<String, ComposerDraft>>,
        submissions: Mutex<Vec<DraftSubmission>>,
        saves: Mutex<Vec<ComposerDraft>>,
        settlements: Mutex<Vec<(String, String)>>,
        fail_saves: AtomicBool,
        command_status: Mutex<Option<CommandReceiptStatus>>,
        command_recovery_action: Mutex<Option<String>>,
        command_tasks: Mutex<HashMap<String, String>>,
        command_calls: AtomicUsize,
        read_started: Mutex<Option<oneshot::Sender<()>>>,
        read_release: Mutex<Option<oneshot::Receiver<()>>>,
    }

    impl DraftTestRpc {
        fn new(drafts: Vec<ComposerDraft>) -> Self {
            Self {
                drafts: Mutex::new(
                    drafts
                        .into_iter()
                        .map(|draft| (draft.draft_id.clone(), draft))
                        .collect(),
                ),
                submissions: Mutex::new(Vec::new()),
                saves: Mutex::new(Vec::new()),
                settlements: Mutex::new(Vec::new()),
                fail_saves: AtomicBool::new(false),
                command_status: Mutex::new(Some(CommandReceiptStatus::Applied)),
                command_recovery_action: Mutex::new(None),
                command_tasks: Mutex::new(HashMap::new()),
                command_calls: AtomicUsize::new(0),
                read_started: Mutex::new(None),
                read_release: Mutex::new(None),
            }
        }

        fn delayed(
            drafts: Vec<ComposerDraft>,
        ) -> (Arc<Self>, oneshot::Receiver<()>, oneshot::Sender<()>) {
            let (started_tx, started_rx) = oneshot::channel();
            let (release_tx, release_rx) = oneshot::channel();
            let service = Arc::new(Self::new(drafts));
            *service.read_started.lock().unwrap() = Some(started_tx);
            *service.read_release.lock().unwrap() = Some(release_rx);
            (service, started_rx, release_tx)
        }
    }

    #[async_trait]
    impl RpcService for DraftTestRpc {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                methods::GET_COMPOSER_DRAFT => {
                    if let Some(started) = self.read_started.lock().unwrap().take() {
                        let _ = started.send(());
                    }
                    let release = self.read_release.lock().unwrap().take();
                    if let Some(release) = release {
                        let _ = release.await;
                    }
                    let task_id = params.get("taskId").and_then(|value| value.as_str());
                    let draft_id = params.get("draftId").and_then(|value| value.as_str());
                    let draft = self
                        .drafts
                        .lock()
                        .unwrap()
                        .values()
                        .find(|draft| {
                            task_id.is_some_and(|task_id| draft.task_id.as_deref() == Some(task_id))
                                || draft_id.is_some_and(|draft_id| draft.draft_id == draft_id)
                                || (task_id.is_none()
                                    && draft_id.is_none()
                                    && draft.task_id.is_none())
                        })
                        .cloned();
                    let submissions = self
                        .submissions
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|submission| submission.task_id.as_deref() == task_id)
                        .cloned()
                        .collect();
                    RpcReply::value(&luna_proto::ComposerDraftRead { draft, submissions })
                }
                methods::SAVE_COMPOSER_DRAFT => {
                    if self.fail_saves.load(Ordering::SeqCst) {
                        return Err(RpcError::Failed("synthetic save failure".into()));
                    }
                    let draft: ComposerDraft = serde_json::from_value(
                        params
                            .get("draft")
                            .cloned()
                            .ok_or_else(|| RpcError::BadParams("missing draft".into()))?,
                    )
                    .map_err(|error| RpcError::BadParams(error.to_string()))?;
                    let outcome = {
                        let mut drafts = self.drafts.lock().unwrap();
                        match drafts.get(&draft.draft_id) {
                            Some(current) if current.revision > draft.revision => {
                                ComposerDraftSaveOutcome::Stale {
                                    current_revision: current.revision,
                                }
                            }
                            Some(current) if current.revision == draft.revision => {
                                let mut current = current.clone();
                                current.updated_at_ms = draft.updated_at_ms;
                                if current != draft {
                                    return Err(RpcError::Failed(format!(
                                        "composer draft {} reused a revision with different content",
                                        draft.draft_id
                                    )));
                                }
                                ComposerDraftSaveOutcome::Unchanged
                            }
                            _ => {
                                drafts.insert(draft.draft_id.clone(), draft.clone());
                                self.saves.lock().unwrap().push(draft);
                                ComposerDraftSaveOutcome::Saved
                            }
                        }
                    };
                    RpcReply::value(&outcome)
                }
                methods::BEGIN_DRAFT_SUBMISSION => {
                    let snapshot: ComposerDraft =
                        serde_json::from_value(params["snapshot"].clone())
                            .map_err(|error| RpcError::BadParams(error.to_string()))?;
                    if self
                        .drafts
                        .lock()
                        .unwrap()
                        .get(&snapshot.draft_id)
                        .is_none_or(|current| current.revision != snapshot.revision)
                    {
                        return Err(RpcError::Failed("submitted draft was not saved".into()));
                    }
                    self.submissions.lock().unwrap().push(DraftSubmission {
                        command_id: params["commandId"].as_str().unwrap().into(),
                        state: DraftSubmissionState::Pending,
                        snapshot,
                        task_id: params["taskId"].as_str().map(str::to_string),
                        queue_id: None,
                        updated_at_ms: 1,
                    });
                    RpcReply::value(&serde_json::json!({ "ok": true }))
                }
                methods::SETTLE_DRAFT_SUBMISSION => {
                    let command = params["commandId"].as_str().unwrap().to_string();
                    let outcome = params["outcome"].as_str().unwrap().to_string();
                    self.settlements
                        .lock()
                        .unwrap()
                        .push((command.clone(), outcome));
                    let mut submissions = self.submissions.lock().unwrap();
                    let submitted = submissions
                        .iter()
                        .find(|submission| submission.command_id == command)
                        .cloned();
                    if params["outcome"] == "recoverable" {
                        if let Some(submission) = submissions
                            .iter_mut()
                            .find(|submission| submission.command_id == command)
                        {
                            submission.state = DraftSubmissionState::Recoverable;
                        }
                    } else {
                        submissions.retain(|submission| submission.command_id != command);
                    }
                    drop(submissions);
                    if params["outcome"] == "applied"
                        && let Some(submitted) = submitted
                    {
                        let mut drafts = self.drafts.lock().unwrap();
                        if drafts
                            .get(&submitted.snapshot.draft_id)
                            .is_some_and(|current| current.revision == submitted.snapshot.revision)
                        {
                            drafts.remove(&submitted.snapshot.draft_id);
                        }
                    }
                    RpcReply::value(&serde_json::json!({ "ok": true }))
                }
                methods::GET_TASK_COMMAND_SNAPSHOT => {
                    RpcReply::value(&luna_proto::TaskCommandRead {
                        snapshot: luna_proto::TaskCommandSnapshot {
                            task_id: params["taskId"].as_str().unwrap().into(),
                            ..Default::default()
                        },
                        events: vec![],
                        reload_required: false,
                        next_task_sequence: None,
                        has_more: false,
                    })
                }
                methods::EXECUTE_TASK_COMMAND => {
                    let task_id = params["taskId"].as_str().unwrap();
                    let command_id = params["commandId"].as_str().unwrap();
                    self.command_calls.fetch_add(1, Ordering::SeqCst);
                    self.command_tasks
                        .lock()
                        .unwrap()
                        .insert(command_id.into(), task_id.into());
                    match *self.command_status.lock().unwrap() {
                        Some(status) => {
                            RpcReply::value(&synthetic_receipt(task_id, command_id, status))
                        }
                        None => Err(RpcError::Transport("synthetic lost reply".into())),
                    }
                }
                methods::READ_ATTACHMENT_CHUNK => RpcReply::value(&serde_json::json!({
                    "name":"recovered.png",
                    "mimeType":"image/png",
                    "data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
                    "nextOffset":68,
                    "done":true
                })),
                methods::GET_TASK_COMMAND_RECEIPT => {
                    let command_id = params["commandId"].as_str().unwrap();
                    let task_id = self.command_tasks.lock().unwrap().get(command_id).cloned();
                    let status = *self.command_status.lock().unwrap();
                    RpcReply::value(&task_id.zip(status).map(|(task_id, status)| {
                        let mut receipt = synthetic_receipt(&task_id, command_id, status);
                        receipt.recovery_action =
                            self.command_recovery_action.lock().unwrap().clone();
                        receipt
                    }))
                }
                other => Err(RpcError::UnknownMethod(other.into())),
            }
        }
    }

    fn test_draft(task_id: &str, revision: u64, text: &str) -> ComposerDraft {
        ComposerDraft {
            input_items: Vec::new(),
            version: luna_proto::COMPOSER_DRAFT_VERSION,
            draft_id: task_id.into(),
            task_id: Some(task_id.into()),
            revision,
            text: text.into(),
            attachments: Vec::new(),
            appshots: Vec::new(),
            context: DraftContext::default(),
            execution: serde_json::Value::Null,
            updated_at_ms: revision as i64,
        }
    }

    fn synthetic_receipt(
        task_id: &str,
        command_id: &str,
        status: CommandReceiptStatus,
    ) -> luna_proto::CommandReceipt {
        luna_proto::CommandReceipt {
            command_id: command_id.into(),
            task_id: task_id.into(),
            intent: "session".into(),
            payload_hash: "synthetic-mock".into(),
            status,
            snapshot: luna_proto::TaskCommandSnapshot {
                task_id: task_id.into(),
                ..Default::default()
            },
            event_sequence: None,
            actor_id: "synthetic".into(),
            client_id: "synthetic".into(),
            client_session_id: "synthetic".into(),
            execution_environment: None,
            correlation_id: None,
            reason: (status == CommandReceiptStatus::Rejected).then(|| {
                luna_proto::CommandReceiptReason {
                    code: luna_proto::CommandReasonCode::OperationRejected,
                    message: "synthetic rejection".into(),
                }
            }),
            recovery_action: None,
            accepted_at_ms: 1,
            committed_at_ms: (status == CommandReceiptStatus::Applied).then_some(1),
            observed_at_ms: 1,
        }
    }

    fn recoverable_setup_service(
        task: &str,
        task_draft: ComposerDraft,
        snapshot: ComposerDraft,
    ) -> Arc<DraftTestRpc> {
        const COMMAND: &str = "setup-rejected-run";
        const ACTION: &str = "Worktree setup failed. Open Terminal → Commands to inspect output and retry setup, then retry the task.";
        let service = Arc::new(DraftTestRpc::new(vec![task_draft]));
        *service.command_status.lock().unwrap() = Some(CommandReceiptStatus::Rejected);
        *service.command_recovery_action.lock().unwrap() = Some(ACTION.into());
        service
            .command_tasks
            .lock()
            .unwrap()
            .insert(COMMAND.into(), task.into());
        service.submissions.lock().unwrap().push(DraftSubmission {
            command_id: COMMAND.into(),
            state: DraftSubmissionState::Recoverable,
            snapshot,
            task_id: Some(task.into()),
            queue_id: None,
            updated_at_ms: 100,
        });
        service
    }

    fn mock_task_chat(task: &str) -> luna_proto::Chat {
        serde_json::from_value(serde_json::json!({
            "id": task,
            "deviceId": "local",
            "title": "Synthetic mock task",
            "archived": false,
            "cwd": "/synthetic-project",
            "config": {
                "harness": "mock",
                "model": null,
                "sandbox": "workspace-write",
                "permissionMode": "ask",
                "effectivePermissionMode": "ask"
            },
            "createdAt": chrono::Utc::now()
        }))
        .unwrap()
    }

    fn composer_with_draft_rpc(
        cx: &mut gpui::TestAppContext,
        task_id: &str,
        service: Arc<DraftTestRpc>,
    ) -> (Entity<AppState>, Entity<Composer>) {
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.set_test_engine(crate::state::EngineHandle::from_test_client(
                luna_rpc::memory_client(service),
            ));
            state.selected_chat = Some(task_id.into());
        });
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        (state, composer)
    }

    #[track_caller]
    fn pump_until(
        cx: &mut gpui::TestAppContext,
        runtime: &tokio::runtime::Runtime,
        mut done: impl FnMut(&gpui::TestAppContext) -> bool,
    ) {
        for _ in 0..500 {
            cx.run_until_parked();
            runtime.block_on(tokio::time::sleep(Duration::from_millis(1)));
            if done(cx) {
                return;
            }
        }
        assert!(done(cx), "async fixture did not reach its expected state");
    }

    #[gpui::test]
    fn delayed_high_revision_load_merges_typing_and_saves_above_both(
        cx: &mut gpui::TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let (service, started, release) =
            DraftTestRpc::delayed(vec![test_draft("task-race", 50, "durable text")]);
        let (_state, composer) = composer_with_draft_rpc(cx, "task-race", service.clone());
        cx.run_until_parked();
        runtime
            .block_on(tokio::time::timeout(Duration::from_secs(2), started))
            .expect("draft read started")
            .expect("draft read signal");

        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("typed while loading".into(), cx);
            composer.mark_draft_dirty(cx);
        });
        release.send(()).unwrap();
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| {
                composer.draft_saved_revisions.get("task-race").copied() == Some(51)
            })
        });

        composer.read_with(cx, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).text(),
                "durable text\n\ntyped while loading"
            );
            assert_eq!(composer.draft_revisions["task-race"], 51);
            assert_eq!(composer.draft_saved_revisions["task-race"], 51);
        });
        let saves = service.saves.lock().unwrap();
        assert_eq!(saves.last().unwrap().revision, 51);
        assert_eq!(
            saves.last().unwrap().text,
            "durable text\n\ntyped while loading"
        );
    }

    #[gpui::test]
    fn canvas_picker_repaint_during_load_does_not_create_a_conflict(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let mut draft = test_draft("canvas-draft", 58, "L32 synthetic persistence check");
        draft.task_id = None;
        draft.execution = serde_json::json!({
            "draft": crate::pickers::DraftConfig {
                checkout: crate::pickers::CheckoutKind::NewWorktree,
                ..Default::default()
            },
            "resolved": crate::pickers::ResolvedRunConfig::default(),
        });
        let (service, started, release) = DraftTestRpc::delayed(vec![draft]);
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.set_test_engine(crate::state::EngineHandle::from_test_client(
                luna_rpc::memory_client(service.clone()),
            ));
        });
        let composer = cx.new(|cx| Composer::new(state, cx));
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        cx.run_until_parked();
        runtime
            .block_on(tokio::time::timeout(Duration::from_secs(2), started))
            .expect("draft read started")
            .expect("draft read signal");

        composer.update(cx, |composer, cx| {
            composer.pickers.update(cx, |_, cx| cx.notify());
        });
        cx.run_until_parked();
        composer.read_with(cx, |composer, _| {
            assert_eq!(composer.draft_revisions.get("").copied().unwrap_or(0), 0);
        });
        release.send(()).unwrap();
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_loaded.contains(""))
        });

        composer.read_with(cx, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).text(),
                "L32 synthetic persistence check"
            );
            assert_eq!(composer.draft_revisions[""], 58);
            assert_eq!(composer.draft_saved_revisions[""], 58);
            assert!(composer.failure.is_none());
            assert_eq!(
                composer.pickers.read(cx).draft().checkout,
                crate::pickers::CheckoutKind::NewWorktree
            );
        });
        cx.run_until_parked();
        composer.read_with(cx, |composer, _| {
            assert_eq!(composer.draft_revisions[""], 58);
            assert!(composer.failure.is_none());
        });
        assert!(service.saves.lock().unwrap().is_empty());

        let mut changed = composer.read_with(cx, |composer, cx| {
            composer.pickers.read(cx).persisted_draft_execution(cx)
        });
        changed["draft"]["checkout"] = "local".into();
        composer.update(cx, |composer, cx| {
            composer.pickers.update(cx, |pickers, cx| {
                pickers.restore_draft_execution(&changed, cx)
            });
        });
        cx.run_until_parked();
        composer.read_with(cx, |composer, _| {
            assert_eq!(composer.draft_revisions[""], 59)
        });
        composer.update(cx, |composer, cx| {
            composer.schedule_draft_save(String::new(), Duration::ZERO, cx)
        });
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| {
                composer.draft_saved_revisions.get("").copied() == Some(59)
            })
        });
        assert_eq!(service.saves.lock().unwrap().last().unwrap().revision, 59);
    }

    #[gpui::test]
    fn engine_generation_change_discards_an_old_profile_load(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let (old_service, started, release) =
            DraftTestRpc::delayed(vec![test_draft("shared-task", 9, "old profile")]);
        let (state, composer) = composer_with_draft_rpc(cx, "shared-task", old_service.clone());
        cx.run_until_parked();
        runtime
            .block_on(tokio::time::timeout(Duration::from_secs(2), started))
            .expect("old profile read started")
            .expect("old profile read signal");
        composer.update(cx, |composer, _| {
            composer.queue_edit_draft = Some(("old private queue edit".into(), vec![], vec![]));
            composer.editing_queued = Some("old-queue-row".into());
            composer.failure = Some("old profile warning".into());
            composer.failure_key = Some("shared-task".into());
        });

        let new_service = Arc::new(DraftTestRpc::new(vec![test_draft(
            "shared-task",
            3,
            "new profile",
        )]));
        state.update(cx, |state, _| {
            state.set_test_engine(crate::state::EngineHandle::from_test_client(
                luna_rpc::memory_client(new_service.clone()),
            ));
        });
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        release.send(()).unwrap();
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, cx| {
                composer.input.read(cx).text() == "new profile"
            })
        });
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.input.read(cx).text(), "new profile");
            assert_eq!(composer.draft_saved_revisions["shared-task"], 3);
            assert!(composer.queue_edit_draft.is_none());
            assert!(composer.editing_queued.is_none());
            assert!(composer.failure.is_none());
        });
        assert!(old_service.saves.lock().unwrap().is_empty());
    }

    #[gpui::test]
    fn close_flushes_dirty_drafts_from_both_sides_of_navigation(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let service = Arc::new(DraftTestRpc::new(Vec::new()));
        let (state, composer) = composer_with_draft_rpc(cx, "task-a", service.clone());
        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("draft A".into(), cx);
            composer.mark_draft_dirty(cx);
        });
        state.update(cx, |state, _| state.selected_chat = Some("task-b".into()));
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("draft B".into(), cx);
            composer.mark_draft_dirty(cx);
            assert!(!composer.prepare_close(cx));
        });
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_flush_complete())
        });

        let drafts = service.drafts.lock().unwrap();
        assert_eq!(drafts["task-a"].text, "draft A");
        assert_eq!(drafts["task-b"].text, "draft B");
    }

    #[gpui::test]
    fn applied_task_send_clears_pending_save_without_reusing_revision(
        cx: &mut gpui::TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let task = "l32-synthetic-mock-task";
        let service = Arc::new(DraftTestRpc::new(vec![test_draft(task, 26, "")]));
        let (state, composer) = composer_with_draft_rpc(cx, task, service.clone());
        state.update(cx, |state, _| state.chats.push(mock_task_chat(task)));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_loaded.contains(task))
        });

        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("L32 synthetic task readback".into(), cx);
            composer.mark_draft_dirty(cx);
            assert_eq!(composer.draft_revisions[task], 27);
            assert!(composer.draft_save_tasks.contains_key(task));
            composer.send("L32 synthetic task readback".into(), None, cx);
            assert_eq!(composer.input.read(cx).text(), "");
            assert!(!composer.draft_save_tasks.contains_key(task));
            assert!(!composer.draft_saves_in_flight.contains_key(task));
        });
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| {
                !composer.sending && composer.draft_revisions.get(task) == Some(&28)
            })
        });
        composer.update(cx, |composer, cx| assert!(!composer.prepare_close(cx)));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_flush_complete())
        });
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.input.read(cx).text(), "");
            assert_eq!(composer.draft_revisions[task], 28);
            assert!(composer.failure.is_none());
        });
        composer.update(cx, |composer, cx| assert!(composer.prepare_close(cx)));
        assert_eq!(service.drafts.lock().unwrap()[task].text, "");
        assert_eq!(service.settlements.lock().unwrap()[0].1, "applied");

        state.update(cx, |state, _| {
            state.selected_chat = Some("other-task".into())
        });
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        state.update(cx, |state, _| state.selected_chat = Some(task.into()));
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.input.read(cx).text(), "");
            assert!(composer.failure.is_none());
        });
        let (_reopened_state, reopened) = composer_with_draft_rpc(cx, task, service.clone());
        pump_until(cx, &runtime, |cx| {
            reopened.read_with(cx, |composer, _| composer.draft_loaded.contains(task))
        });
        reopened.read_with(cx, |composer, cx| {
            assert_eq!(composer.input.read(cx).text(), "");
            assert!(composer.failure.is_none());
        });

        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("new unsent text".into(), cx);
            composer.mark_draft_dirty(cx);
            assert_eq!(composer.draft_revisions[task], 29);
            composer.schedule_draft_save(task.into(), Duration::ZERO, cx);
        });
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| {
                composer.draft_saved_revisions.get(task) == Some(&29)
            })
        });
        assert_eq!(service.drafts.lock().unwrap()[task].text, "new unsent text");
        composer.update(cx, |composer, cx| assert!(composer.prepare_close(cx)));
    }

    #[gpui::test]
    fn applied_task_send_preserves_newer_typing(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let task = "task-with-newer-typing";
        let service = Arc::new(DraftTestRpc::new(vec![test_draft(task, 26, "")]));
        let (state, composer) = composer_with_draft_rpc(cx, task, service.clone());
        state.update(cx, |state, _| state.chats.push(mock_task_chat(task)));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_loaded.contains(task))
        });

        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("sent text".into(), cx);
            composer.mark_draft_dirty(cx);
            composer.send("sent text".into(), None, cx);
            composer.set_input_text_without_draft_edit("newer unsent text".into(), cx);
            composer.mark_draft_dirty(cx);
            assert_eq!(composer.draft_revisions[task], 28);
        });
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| !composer.sending)
        });
        composer.update(cx, |composer, cx| assert!(!composer.prepare_close(cx)));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_flush_complete())
        });
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.input.read(cx).text(), "newer unsent text");
            assert!(composer.draft_revisions[task] > 27);
            assert_eq!(
                service.drafts.lock().unwrap()[task].revision,
                composer.draft_revisions[task]
            );
            assert!(composer.failure.is_none());
        });
        assert_eq!(
            service.drafts.lock().unwrap()[task].text,
            "newer unsent text"
        );
        assert_eq!(service.settlements.lock().unwrap()[0].1, "applied");
    }

    #[gpui::test]
    fn rejected_task_send_restores_and_saves_its_draft(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let task = "rejected-mock-task";
        let service = Arc::new(DraftTestRpc::new(vec![test_draft(task, 26, "")]));
        *service.command_status.lock().unwrap() = Some(CommandReceiptStatus::Rejected);
        let (state, composer) = composer_with_draft_rpc(cx, task, service.clone());
        state.update(cx, |state, _| state.chats.push(mock_task_chat(task)));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_loaded.contains(task))
        });

        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("preserve rejected text".into(), cx);
            composer.mark_draft_dirty(cx);
        });
        cx.run_until_parked();
        composer.update(cx, |composer, cx| {
            composer.send("preserve rejected text".into(), None, cx);
        });
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| !composer.sending)
        });
        composer.update(cx, |composer, cx| assert!(!composer.prepare_close(cx)));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_flush_complete())
        });

        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.input.read(cx).text(), "preserve rejected text");
            assert!(composer.draft_revisions[task] > 27);
            assert!(composer.failure.is_some());
        });
        assert_eq!(
            service.drafts.lock().unwrap()[task].text,
            "preserve rejected text"
        );
        assert_eq!(service.settlements.lock().unwrap()[0].1, "recoverable");
        assert_eq!(
            service.submissions.lock().unwrap()[0].state,
            DraftSubmissionState::Recoverable
        );
    }

    #[gpui::test]
    fn pending_task_send_closes_with_durable_snapshot_and_keeps_new_canvas_draft(
        cx: &mut gpui::TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let task = "pending-mock-task";
        let service = Arc::new(DraftTestRpc::new(vec![test_draft(task, 26, "")]));
        *service.command_status.lock().unwrap() = Some(CommandReceiptStatus::Pending);
        let (state, composer) = composer_with_draft_rpc(cx, task, service.clone());
        state.update(cx, |state, _| state.chats.push(mock_task_chat(task)));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_loaded.contains(task))
        });

        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("pending mock prompt".into(), cx);
            composer.mark_draft_dirty(cx);
        });
        cx.run_until_parked();
        let submitted_revision =
            composer.read_with(cx, |composer, _| composer.draft_revisions[task]);
        composer.update(cx, |composer, cx| {
            composer.send("pending mock prompt".into(), None, cx)
        });
        pump_until(cx, &runtime, |_| {
            service.command_calls.load(Ordering::SeqCst) > 0
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| !composer.sending)
        });
        composer.read_with(cx, |composer, _| {
            assert_eq!(
                composer.draft_saved_revisions.get(task),
                Some(&submitted_revision)
            );
            assert_eq!(
                composer.draft_submitting.get(task),
                Some(&submitted_revision)
            );
            assert!(composer.failure.is_none());
        });
        composer.update(cx, |composer, cx| assert!(composer.prepare_close(cx)));
        assert_eq!(
            service.submissions.lock().unwrap()[0].state,
            DraftSubmissionState::Pending
        );
        assert_eq!(
            service.drafts.lock().unwrap()[task].text,
            "pending mock prompt"
        );

        state.update(cx, |state, _| state.selected_chat = None);
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("new unsent canvas text".into(), cx);
            composer.mark_draft_dirty(cx);
            composer.schedule_draft_save(String::new(), Duration::ZERO, cx);
        });
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_flush_complete())
        });
        composer.update(cx, |composer, cx| assert!(composer.prepare_close(cx)));
        let calls = service.command_calls.load(Ordering::SeqCst);

        let reopened_state = cx.new(|_| AppState::new());
        reopened_state.update(cx, |state, _| {
            state.set_test_engine(crate::state::EngineHandle::from_test_client(
                luna_rpc::memory_client(service.clone()),
            ));
        });
        let reopened = cx.new(|cx| Composer::new(reopened_state, cx));
        reopened.update(cx, |composer, cx| composer.on_state_changed(cx));
        pump_until(cx, &runtime, |cx| {
            reopened.read_with(cx, |composer, _| composer.draft_loaded.contains(""))
        });
        reopened.read_with(cx, |composer, cx| {
            assert_eq!(composer.input.read(cx).text(), "new unsent canvas text");
        });
        assert_eq!(service.command_calls.load(Ordering::SeqCst), calls);

        let (_pending_state, pending_task) = composer_with_draft_rpc(cx, task, service.clone());
        pump_until(cx, &runtime, |cx| {
            pending_task.read_with(cx, |composer, _| composer.draft_loaded.contains(task))
        });
        pending_task.read_with(cx, |composer, cx| {
            assert_eq!(composer.input.read(cx).text(), "");
        });
        assert_eq!(
            service.submissions.lock().unwrap()[0].state,
            DraftSubmissionState::Pending
        );
        assert_eq!(service.command_calls.load(Ordering::SeqCst), calls);

        *service.command_status.lock().unwrap() = Some(CommandReceiptStatus::Applied);
        let (_task_state, reopened_task) = composer_with_draft_rpc(cx, task, service.clone());
        pump_until(cx, &runtime, |cx| {
            reopened_task.read_with(cx, |composer, _| composer.draft_loaded.contains(task))
                && service.submissions.lock().unwrap().is_empty()
        });
        // Settlement deletes the submitted snapshot; the composer then fences
        // it with an empty row one revision above (`finish_applied_draft`), so
        // a late save of the sent text is rejected as stale. Either way the
        // sent message must never come back as this task's draft.
        let drafts = service.drafts.lock().unwrap();
        assert!(
            drafts.get(task).is_none_or(|draft| {
                draft.text.is_empty()
                    && draft.attachments.is_empty()
                    && draft.appshots.is_empty()
                    && draft.revision > submitted_revision
            }),
            "the applied message came back as a draft: {:?}",
            drafts
                .get(task)
                .map(|draft| (draft.revision, draft.text.clone()))
        );
        drop(drafts);
        assert_eq!(service.command_calls.load(Ordering::SeqCst), calls);
    }

    #[gpui::test]
    fn unknown_task_send_closes_and_later_rejection_remains_recoverable(
        cx: &mut gpui::TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let task = "unknown-mock-task";
        let service = Arc::new(DraftTestRpc::new(vec![test_draft(task, 26, "")]));
        *service.command_status.lock().unwrap() = None;
        let (state, composer) = composer_with_draft_rpc(cx, task, service.clone());
        state.update(cx, |state, _| state.chats.push(mock_task_chat(task)));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_loaded.contains(task))
        });
        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("unknown mock prompt".into(), cx);
            composer.mark_draft_dirty(cx);
        });
        cx.run_until_parked();
        let submitted_revision =
            composer.read_with(cx, |composer, _| composer.draft_revisions[task]);
        composer.update(cx, |composer, cx| {
            composer.send("unknown mock prompt".into(), None, cx)
        });
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| !composer.sending)
        });
        composer.read_with(cx, |composer, _| {
            assert_eq!(
                composer.draft_saved_revisions.get(task),
                Some(&submitted_revision)
            );
            assert!(composer.failure.is_some());
        });
        composer.update(cx, |composer, cx| assert!(composer.prepare_close(cx)));
        assert_eq!(
            service.submissions.lock().unwrap()[0].state,
            DraftSubmissionState::Pending
        );
        let calls = service.command_calls.load(Ordering::SeqCst);

        *service.command_status.lock().unwrap() = Some(CommandReceiptStatus::Rejected);
        let (_task_state, reopened) = composer_with_draft_rpc(cx, task, service.clone());
        pump_until(cx, &runtime, |cx| {
            reopened.read_with(cx, |composer, _| composer.draft_loaded.contains(task))
                && service.submissions.lock().unwrap()[0].state == DraftSubmissionState::Recoverable
        });
        assert_eq!(
            service.drafts.lock().unwrap()[task].text,
            "unknown mock prompt"
        );
        assert_eq!(service.command_calls.load(Ordering::SeqCst), calls);
    }

    #[gpui::test]
    fn failed_snapshot_save_does_not_allow_pending_send_to_close(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let task = "unsaved-mock-task";
        let service = Arc::new(DraftTestRpc::new(vec![test_draft(task, 26, "")]));
        service.fail_saves.store(true, Ordering::SeqCst);
        *service.command_status.lock().unwrap() = Some(CommandReceiptStatus::Pending);
        let (state, composer) = composer_with_draft_rpc(cx, task, service.clone());
        state.update(cx, |state, _| state.chats.push(mock_task_chat(task)));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_loaded.contains(task))
        });
        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("not durably saved".into(), cx);
            composer.mark_draft_dirty(cx);
        });
        cx.run_until_parked();
        composer.update(cx, |composer, cx| {
            composer.send("not durably saved".into(), None, cx)
        });
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| !composer.sending)
        });
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.input.read(cx).text(), "not durably saved");
            assert!(composer.draft_saved_revisions[task] < composer.draft_revisions[task]);
            assert!(composer.failure.is_some());
        });
        composer.update(cx, |composer, cx| assert!(!composer.prepare_close(cx)));
        assert!(service.submissions.lock().unwrap().is_empty());
        assert_eq!(service.command_calls.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn failed_save_keeps_orderly_close_blocked(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let service = Arc::new(DraftTestRpc::new(Vec::new()));
        service.fail_saves.store(true, Ordering::SeqCst);
        let (_state, composer) = composer_with_draft_rpc(cx, "task-fail", service);
        composer.update(cx, |composer, cx| {
            composer.set_input_text_without_draft_edit("must not be lost".into(), cx);
            composer.mark_draft_dirty(cx);
            assert!(!composer.prepare_close(cx));
        });
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_close_failed)
        });
        composer.read_with(cx, |composer, _| {
            assert!(!composer.draft_flush_complete());
            assert!(
                composer
                    .failure
                    .as_deref()
                    .is_some_and(|message| message.contains("Your text is still here"))
            );
        });
    }

    #[gpui::test]
    fn recovery_action_merges_older_text_and_attachment_into_newer_draft(
        cx: &mut gpui::TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let service = Arc::new(DraftTestRpc::new(vec![test_draft(
            "task-recover",
            20,
            "newer draft",
        )]));
        let mut snapshot = test_draft("older-snapshot", 10, "older unsent");
        snapshot.task_id = Some("task-recover".into());
        snapshot.attachments.push(DraftBlob {
            attachment_id: "recovered-image".into(),
            name: "recovered.png".into(),
            path: Some("/synthetic/recovered.png".into()),
            sha256: None,
            size: Some(68),
            state: DraftBlobState::Saved,
            error: None,
        });
        service.submissions.lock().unwrap().push(DraftSubmission {
            command_id: "recover-command".into(),
            state: DraftSubmissionState::Recoverable,
            snapshot,
            task_id: Some("task-recover".into()),
            queue_id: None,
            updated_at_ms: 10,
        });
        let (_state, composer) = composer_with_draft_rpc(cx, "task-recover", service.clone());
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| {
                composer
                    .recoverable_submissions
                    .get("task-recover")
                    .is_some_and(|entries| !entries.is_empty())
            })
        });
        composer.update(cx, |composer, cx| composer.recover_latest_submission(cx));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| {
                !composer
                    .recoverable_submissions
                    .contains_key("task-recover")
            })
        });

        composer.read_with(cx, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).text(),
                "newer draft\n\nolder unsent"
            );
            assert_eq!(composer.attachments["task-recover"].len(), 1);
            assert!(
                !composer
                    .recoverable_submissions
                    .contains_key("task-recover")
            );
        });
        assert_eq!(
            service.settlements.lock().unwrap().as_slice(),
            &[("recover-command".into(), "recovered".into())]
        );
        let saved = service.saves.lock().unwrap();
        let saved = saved.last().unwrap();
        assert_eq!(saved.text, "newer draft\n\nolder unsent");
        assert_eq!(saved.attachments.len(), 1);
    }

    #[gpui::test]
    fn first_submit_canvas_rekey_shows_setup_recovery_action(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let task = "new-task-after-failed-setup";
        let prompt =
            "Reply with exactly PROJECT_DEFAULTS_NATIVE_OK. Do not use tools or change files.";
        let mut snapshot = test_draft("canvas-draft", 86, prompt);
        snapshot.task_id = None;
        let service = recoverable_setup_service(task, test_draft(task, 1, ""), snapshot);
        let (state, composer) = composer_with_draft_rpc(cx, task, service.clone());
        state.update(cx, |state, _| state.chats.push(mock_task_chat(task)));
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| composer.draft_loaded.contains(task))
        });
        composer.read_with(cx, |composer, cx| {
            assert_eq!(state.read(cx).selected_chat.as_deref(), Some(task));
            assert_eq!(composer.current_key, task);
            assert_eq!(composer.input.read(cx).text(), "");
            assert_eq!(composer.failure_key.as_deref(), Some(task));
            assert_eq!(
                composer.failure.as_deref(),
                Some("Worktree setup failed. Open Terminal → Commands to inspect output and retry setup, then retry the task.")
            );
            assert_eq!(composer.recoverable_submissions[task][0].snapshot.text, prompt);
        });
        assert_eq!(service.command_calls.load(Ordering::SeqCst), 0);
        assert_eq!(service.submissions.lock().unwrap().len(), 1);
    }

    #[gpui::test]
    fn recoverable_setup_preserves_genuinely_newer_task_drafts(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let prompt = "synthetic original setup prompt";
        for case in ["text", "attachment", "context", "same-draft-revision"] {
            let task = format!("setup-newer-{case}");
            let mut draft = test_draft(&task, 1, "");
            let mut snapshot = test_draft("canvas-draft", 86, prompt);
            snapshot.task_id = None;
            match case {
                "text" => draft.text = "newer task text".into(),
                "attachment" => draft.attachments.push(DraftBlob {
                    attachment_id: "recovered-image".into(),
                    name: "recovered.png".into(),
                    path: Some("/synthetic/recovered.png".into()),
                    sha256: None,
                    size: Some(68),
                    state: DraftBlobState::Saved,
                    error: None,
                }),
                "context" => draft.context.quoted = Some(serde_json::json!("newer task quote")),
                "same-draft-revision" => {
                    draft.revision = 3;
                    snapshot = test_draft(&task, 2, prompt);
                }
                _ => unreachable!(),
            }
            let service = recoverable_setup_service(&task, draft, snapshot);
            let (state, composer) = composer_with_draft_rpc(cx, &task, service.clone());
            state.update(cx, |state, _| state.chats.push(mock_task_chat(&task)));
            pump_until(cx, &runtime, |cx| {
                composer.read_with(cx, |composer, _| composer.draft_loaded.contains(&task))
            });
            composer.read_with(cx, |composer, cx| {
                assert_eq!(state.read(cx).selected_chat.as_deref(), Some(task.as_str()));
                assert_eq!(composer.failure_key.as_deref(), Some(task.as_str()));
                assert_eq!(
                    composer.failure.as_deref(),
                    Some(
                        "A previous unsent message was preserved separately from this newer draft."
                    ),
                    "case {case}"
                );
                assert_eq!(
                    composer.input.read(cx).text(),
                    if case == "text" {
                        "newer task text"
                    } else {
                        ""
                    }
                );
                assert_eq!(
                    composer.recoverable_submissions[task.as_str()][0]
                        .snapshot
                        .text,
                    prompt
                );
                if case == "attachment" {
                    assert_eq!(composer.attachments[task.as_str()].len(), 1);
                }
            });
            assert_eq!(service.command_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[gpui::test]
    fn dock_morph_lands_on_the_two_row_session_pill_with_a_continuous_editor_origin(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_dir, handle) = composer_focus_window(cx);
        let input = handle
            .read_with(cx, |composer, _| composer.input.clone())
            .unwrap();
        let session = session_total_height(INPUT_LINE_HEIGHT);
        for docked in [true, false] {
            let amounts = if docked {
                [0.0, 0.2, 0.6, 0.98, 1.0]
            } else {
                [1.0, 0.98, 0.6, 0.2, 0.0]
            };
            for amount in amounts {
                handle
                    .update(cx, |composer, _, cx| {
                        composer.state.update(cx, |state, _| {
                            state.selected_chat = docked.then(|| "chat".into());
                        });
                        composer.on_state_changed(cx);
                        composer
                            .input
                            .update(cx, |input, cx| input.set_text("Hi", cx));
                        let mut frame = crate::composer_dock::DockFrame::settled(docked);
                        frame.amount = amount;
                        frame.active = amount != if docked { 1.0 } else { 0.0 };
                        composer.set_dock_frame(frame, cx);
                    })
                    .unwrap();
                cx.update_window(handle.into(), |_, window, cx| {
                    window.draw(cx).clear();
                })
                .unwrap();
                handle.read_with(cx, |composer, cx| {
                    assert_eq!(composer.input, input);
                    let surface = composer.surface_bounds.get().unwrap();
                    let origin = input.read(cx).last_bounds.unwrap().origin;
                    assert!((f32::from(origin.y - surface.top()) - (1.0 + TEXTAREA_PAD_TOP)).abs() <= 1.0,
                        "editor jumped: docked={docked}, amount={amount}, origin={origin:?}, surface={surface:?}");
                    let expected = if docked { session } else { COMPOSER_MIN_HEIGHT };
                    assert!((composer.last_rendered_height + composer.dock_clearance_correction - expected).abs() < 0.1);
                    assert!((composer.last_rendered_height - motion::lerp(COMPOSER_MIN_HEIGHT, session, amount)).abs() < 0.1);
                }).unwrap();
            }
        }
    }
    #[gpui::test]
    fn composer_padding_and_file_prompt_restore_focus(cx: &mut gpui::TestAppContext) {
        let (dir, handle) = composer_focus_window(cx);
        let image_path = dir.path().join("attachment.png");
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD,
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aF1cAAAAASUVORK5CYII=").unwrap();
        std::fs::write(&image_path, png).unwrap();
        let input = handle
            .read_with(cx, |composer, _| composer.input.clone())
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.blur();
            window.draw(cx).clear();
            let bounds = input.read(cx).last_bounds.unwrap();
            // Click padding immediately left of the actual editor.
            let position = point(bounds.left() - px(4.0), bounds.center().y);
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    click_count: 1,
                    ..Default::default()
                }),
                cx,
            );
            assert!(input.read(cx).focus_handle.is_focused(window));
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    ..Default::default()
                }),
                cx,
            );
        })
        .unwrap();
        for accepted in [false, true] {
            handle
                .update(cx, |composer, window, cx| {
                    composer
                        .input
                        .update(cx, |input, cx| input.set_text("Keep this draft", cx));
                    window.blur();
                    composer.open_file_picker(cx);
                })
                .unwrap();
            assert!(cx.did_prompt_for_paths());
            let path = image_path.clone();
            cx.simulate_path_prompt_response(move |_| accepted.then(|| vec![path]));
            cx.run_until_parked();
            cx.update_window(handle.into(), |_, window, cx| {
                window.draw(cx).clear();
                assert!(input.read(cx).focus_handle.is_focused(window));
                assert_eq!(input.read(cx).text(), "Keep this draft");
            })
            .unwrap();
            assert_eq!(
                handle
                    .read_with(cx, |composer, _| composer.staged().len())
                    .unwrap(),
                usize::from(accepted)
            );
        }
        // The same staging path handles external file drops.
        handle
            .update(cx, |composer, window, cx| {
                window.blur();
                composer.add_paths(vec![image_path], cx);
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear();
            assert!(input.read(cx).focus_handle.is_focused(window));
        })
        .unwrap();
    }

    #[gpui::test]
    fn composer_picker_escape_restores_focus_but_click_away_does_not(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_dir, handle) = composer_focus_window(cx);
        let input = handle
            .read_with(cx, |composer, _| composer.input.clone())
            .unwrap();
        for escape in [true, false] {
            handle
                .update(cx, |composer, window, cx| {
                    composer
                        .pickers
                        .update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
                })
                .unwrap();
            cx.update_window(handle.into(), |_, window, cx| {
                window.draw(cx).clear();
                assert!(!input.read(cx).focus_handle.is_focused(window));
            })
            .unwrap();
            if escape {
                cx.simulate_keystrokes(handle.into(), "escape");
            } else {
                cx.update_window(handle.into(), |_, window, cx| {
                    window.dispatch_event(
                        gpui::PlatformInput::MouseDown(MouseDownEvent {
                            button: MouseButton::Left,
                            position: point(px(5.0), window.viewport_size().height - px(1.0)),
                            click_count: 1,
                            ..Default::default()
                        }),
                        cx,
                    );
                })
                .unwrap();
            }
            cx.update_window(handle.into(), |_, window, cx| {
                window.draw(cx).clear();
                assert_eq!(input.read(cx).focus_handle.is_focused(window), escape);
            })
            .unwrap();
        }
    }

    #[gpui::test]
    fn inputs_release_focus_on_click_away(cx: &mut gpui::TestAppContext) {
        struct Inputs(Vec<Entity<ComposerInput>>);
        impl Render for Inputs {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().flex().flex_col().children(
                    self.0
                        .iter()
                        .map(|input| div().w(px(200.0)).child(input.clone())),
                )
            }
        }
        cx.update(|cx| cx.set_global(Theme::dark()));
        let host = cx.add_window(|_, cx| {
            Inputs(vec![
                cx.new(|cx| ComposerInput::new("Composer", cx)),
                cx.new(|cx| ComposerInput::new("URL", cx).with_single_line()),
            ])
        });
        cx.run_until_parked();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        let inputs = host.read_with(cx, |host, _| host.0.clone()).unwrap();
        // Visit both fields and return to the first: click-away capture must
        // never clear focus acquired by the clicked input during bubbling.
        for index in [0, 1, 0] {
            let position =
                inputs[index].read_with(cx, |input, _| input.last_bounds.unwrap().center());
            cx.update_window(host.into(), |_, window, cx| {
                window.dispatch_event(
                    gpui::PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        click_count: 1,
                        ..Default::default()
                    }),
                    cx,
                );
                assert!(inputs[index].read(cx).focus_handle.is_focused(window));
                window.dispatch_event(
                    gpui::PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        ..Default::default()
                    }),
                    cx,
                );
            })
            .unwrap();
        }
        cx.update_window(host.into(), |_, window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: point(px(400.0), px(300.0)),
                    click_count: 1,
                    ..Default::default()
                }),
                cx,
            );
            assert!(window.focused(cx).is_none());
        })
        .unwrap();
    }

    #[gpui::test]
    fn context_running_or_unknown_blocks_before_draft_capture(cx: &mut gpui::TestAppContext) {
        for (status, expected) in [
            (
                luna_proto::ContextOperationStatus::Running,
                "Context compaction is still running. Your draft was not changed or sent.",
            ),
            (
                luna_proto::ContextOperationStatus::Unknown,
                "Context compaction completion is unresolved. Your draft was not changed or sent; clear context before sending again.",
            ),
        ] {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.selected_chat = Some("context-task".into());
                state.context_control.compact.status = status;
                state
            });
            let composer = cx.new(|cx| Composer::new(state, cx));
            composer.update(cx, |composer, cx| {
                composer.current_key = "context-task".into();
                composer.draft_revisions.insert("context-task".into(), 7);
                composer
                    .draft_saved_revisions
                    .insert("context-task".into(), 7);
                composer.input.update(cx, |input, cx| {
                    input.set_text("preserve this restored rich draft", cx);
                });
                let revision = composer.draft_revisions["context-task"];
                composer.on_submit(cx);
                assert_eq!(
                    composer.input.read(cx).text(),
                    "preserve this restored rich draft"
                );
                assert_eq!(composer.draft_revisions["context-task"], revision);
                assert_eq!(composer.failure.as_deref(), Some(expected));
                assert_eq!(composer.failure_key.as_deref(), Some("context-task"));
                assert!(!composer.sending);
            });
        }
    }

    /// Issue #406: Enter submits — it must never stop a run. Stop mode only
    /// exists on a live run with an EMPTY composer, so a habitual
    /// double-Enter after sending interrupted the just-dispatched prompt
    /// and the agent ate it silently. Keyboard stop is the Esc setting's
    /// job; Enter on an empty composer is a no-op.
    #[gpui::test]
    fn enter_on_empty_composer_during_a_live_run_never_interrupts(cx: &mut gpui::TestAppContext) {
        // RpcClient::new spawns its reader on tokio — give the test a reactor.
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        // The client's write end: any RPC Enter dispatches lands here.
        let (out, mut server_in) = tokio::sync::mpsc::channel::<String>(16);
        let (_server_out, inbound) = tokio::sync::mpsc::channel::<String>(16);
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.set_test_engine(crate::state::EngineHandle::from_test_client(
                luna_rpc::RpcClient::new(out, inbound),
            ));
            state.selected_chat = Some("c".into());
            // A send in flight reads as Working — the double-Enter window.
            state.begin_pending_send("c", "m1", chrono::Utc::now());
        });
        let composer = cx.new(|cx| Composer::new(state, cx));
        composer.update(cx, |composer, cx| {
            assert_eq!(composer.button_mode(cx), SendButtonMode::Stop);
            composer.on_submit(cx);
            // interrupt_chat marks the chat before its RPC even flies.
            assert!(!composer.interrupting.contains("c"));
        });
        // If Enter had dispatched an interrupt, the spawned call would have
        // written its frame into the channel by the time the executor parks.
        cx.run_until_parked();
        assert!(server_in.try_recv().is_err());
    }

    /// The press intent is judged by eye everywhere except here: that a
    /// multi-click leaves the drag disarmed is invisible until a selection
    /// collapses under the pointer.
    #[test]
    fn a_press_of_two_or_more_clicks_takes_the_whole_field_and_leaves_the_drag_disarmed() {
        assert_eq!(press_intent(1, false), PressIntent::PlaceCaret);
        assert_eq!(press_intent(1, true), PressIntent::ExtendSelection);
        assert_eq!(press_intent(2, false), PressIntent::SelectAll);
        // A triple click keeps the whole field, so holding the button down
        // through a third click does not change what is selected.
        assert_eq!(press_intent(3, false), PressIntent::SelectAll);
        // The whole field wins over the shift modifier: shift has nothing
        // left to extend once everything is selected.
        assert_eq!(press_intent(2, true), PressIntent::SelectAll);
        // Only a caret press arms the drag. A select-all that armed it would
        // collapse to a drag selection on the next mouse move.
        assert!(press_intent(1, false).arms_drag());
        assert!(press_intent(1, true).arms_drag());
        assert!(!press_intent(2, false).arms_drag());
    }

    #[test]
    fn message_enter_bindings_cover_both_platform_modifiers() {
        assert_eq!(
            message_enter_bindings(ComposerSendBehavior::Enter, "cmd-enter"),
            vec![
                MessageEnterBinding {
                    keystroke: "enter".into(),
                    action: MessageEnterBindingAction::Submit,
                },
                MessageEnterBinding {
                    keystroke: "cmd-enter".into(),
                    action: MessageEnterBindingAction::ModifierSubmit,
                },
                MessageEnterBinding {
                    keystroke: "shift-cmd-enter".into(),
                    action: MessageEnterBindingAction::OppositeSubmit,
                },
            ]
        );
        assert_eq!(
            message_enter_bindings(ComposerSendBehavior::ModEnter, "cmd-enter"),
            vec![
                MessageEnterBinding {
                    keystroke: "enter".into(),
                    action: MessageEnterBindingAction::NewlineOrAccept,
                },
                MessageEnterBinding {
                    keystroke: "cmd-enter".into(),
                    action: MessageEnterBindingAction::ModifierSubmit,
                },
                MessageEnterBinding {
                    keystroke: "shift-cmd-enter".into(),
                    action: MessageEnterBindingAction::OppositeSubmit,
                },
            ]
        );
        assert_eq!(
            message_enter_bindings(ComposerSendBehavior::ModEnter, "ctrl-enter"),
            vec![
                MessageEnterBinding {
                    keystroke: "enter".into(),
                    action: MessageEnterBindingAction::NewlineOrAccept,
                },
                MessageEnterBinding {
                    keystroke: "ctrl-enter".into(),
                    action: MessageEnterBindingAction::ModifierSubmit,
                },
                MessageEnterBinding {
                    keystroke: "shift-ctrl-enter".into(),
                    action: MessageEnterBindingAction::OppositeSubmit,
                },
            ]
        );
    }

    #[test]
    fn message_enter_adds_only_the_fixed_opposite_modifier_binding() {
        let bindings = message_enter_bindings(ComposerSendBehavior::ModEnter, "cmd-enter");
        assert!(bindings.iter().any(|binding| {
            binding.keystroke == "shift-cmd-enter"
                && binding.action == MessageEnterBindingAction::OppositeSubmit
        }));
        assert!(!bindings.iter().any(|binding| {
            matches!(binding.keystroke.as_str(), "ctrl-enter" | "alt-cmd-enter")
        }));
    }

    #[test]
    fn enter_accepts_a_completion_before_submit_or_newline() {
        assert_eq!(
            enter_outcome(true, EnterOutcome::Submit),
            EnterOutcome::AcceptCompletion
        );
        assert_eq!(
            enter_outcome(true, EnterOutcome::Newline),
            EnterOutcome::AcceptCompletion
        );
        assert_eq!(
            enter_outcome(false, EnterOutcome::Submit),
            EnterOutcome::Submit
        );
        assert_eq!(
            enter_outcome(false, EnterOutcome::Newline),
            EnterOutcome::Newline
        );
    }

    #[test]
    fn numeric_answer_input_keeps_digit_keys_for_text() {
        assert_eq!(wizard_number_shortcut("1", false, false), Some(0));
        assert_eq!(wizard_number_shortcut("9", false, false), Some(8));
        assert_eq!(wizard_number_shortcut("1", false, true), None);
        assert_eq!(wizard_number_shortcut("9", false, true), None);
        assert_eq!(wizard_number_shortcut("1", true, false), None);
        assert_eq!(wizard_number_shortcut("0", false, false), None);
    }

    #[gpui::test]
    fn question_card_keeps_prompt_composer_independent_across_routes(
        cx: &mut gpui::TestAppContext,
    ) {
        let pending = |request_id: &str| SessionMessageEntry {
            id: format!("message-{request_id}"),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Input {
                id: format!("input-{request_id}"),
                request_id: request_id.into(),
                questions: vec![question("q", &["one", "two"], false)],
                resolved: false,
            }],
            created_at: 0,
            device_id: "synthetic".into(),
            status: Some(luna_doc::MessageStatus::Streaming),
            continuation_of: None,
        };
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.selected_chat = Some("question-a".into());
            state.transcript = vec![pending("request-a")];
        });
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.wizard.as_ref().unwrap().request_id, "request-a");
            assert_eq!(composer.input.read(cx).placeholder.as_ref(), "Do anything…");
            assert_eq!(
                composer.answer_input.read(cx).placeholder.as_ref(),
                "Type your own answer, or pick an option above"
            );
        });

        composer.update(cx, |composer, cx| {
            composer
                .input
                .update(cx, |input, cx| input.set_text("Independent prompt", cx));
            composer
                .answer_input
                .update(cx, |input, cx| input.set_text("Separate answer", cx));
        });

        state.update(cx, |state, _| state.transcript.clear());
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        composer.read_with(cx, |composer, cx| {
            assert!(
                composer.wizard.is_some(),
                "transient blip keeps the question"
            );
            assert_eq!(composer.answer_input.read(cx).text(), "Separate answer");
            assert_eq!(composer.input.read(cx).text(), "Independent prompt");
            assert_eq!(
                composer.answer_input.read(cx).placeholder.as_ref(),
                "Type your own answer, or pick an option above"
            );
        });

        state.update(cx, |state, _| state.selected_chat = Some("ordinary".into()));
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        composer.read_with(cx, |composer, cx| {
            assert!(composer.wizard.is_none());
            assert_eq!(composer.input.read(cx).placeholder.as_ref(), "Do anything…");
            assert!(composer.answer_input.read(cx).is_empty());
        });

        state.update(cx, |state, _| {
            state.selected_chat = Some("question-b".into());
            state.transcript = vec![pending("request-b")];
        });
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.wizard.as_ref().unwrap().request_id, "request-b");
            assert_eq!(composer.input.read(cx).placeholder.as_ref(), "Do anything…");
            assert_eq!(
                composer.answer_input.read(cx).placeholder.as_ref(),
                "Type your own answer, or pick an option above"
            );
        });
    }

    #[test]
    fn stable_outer_width_only_schedules_reflow_on_real_changes() {
        assert!(composer_width_changed(None, 400.0));
        assert!(!composer_width_changed(Some(400.0), 400.0));
        assert!(!composer_width_changed(Some(400.0), 400.5));
        assert!(composer_width_changed(Some(400.0), 400.51));
    }

    fn tooltip_target(range: Range<usize>, path: &str) -> MentionTooltipTarget {
        MentionTooltipTarget {
            range,
            path: path.into(),
        }
    }

    #[test]
    fn mention_tooltip_wait_survives_pointer_jitter_and_promotes_once() {
        let target = tooltip_target(3..20, "src/composer.rs");
        let waiting = MentionTooltipPhase::Waiting {
            target: target.clone(),
            generation: 1,
        };
        let restarted = mention_tooltip_reduce(waiting.clone(), Some(target.clone()), false, 2);
        assert_eq!(restarted, waiting);
        assert!(matches!(
            restarted,
            MentionTooltipPhase::Waiting { generation: 1, .. }
        ));
        assert_eq!(
            mention_tooltip_promote(restarted.clone(), 2, true),
            restarted,
            "a stale timer must not reveal the tooltip"
        );
        let visible = mention_tooltip_promote(restarted, 1, true);
        assert!(matches!(
            visible,
            MentionTooltipPhase::Visible { generation: 1, .. }
        ));
        assert_eq!(
            mention_tooltip_reduce(visible.clone(), Some(target), false, 3),
            visible,
            "one visible activation keeps its presentation generation stable"
        );
    }

    #[test]
    fn mention_tooltip_changes_target_and_cancels_disappeared_target() {
        let first = tooltip_target(0..10, "src/a.rs");
        let second = tooltip_target(20..30, "src/a.rs");
        let visible = MentionTooltipPhase::Visible {
            target: first,
            generation: 4,
        };
        assert!(matches!(
            mention_tooltip_reduce(visible, Some(second), false, 5),
            MentionTooltipPhase::Waiting { generation: 5, .. }
        ));
        assert_eq!(
            mention_tooltip_promote(
                MentionTooltipPhase::Waiting {
                    target: tooltip_target(20..30, "src/a.rs"),
                    generation: 5,
                },
                5,
                false,
            ),
            MentionTooltipPhase::Hidden
        );
    }

    #[test]
    fn mention_tooltip_stays_visible_over_chip_or_popup_only() {
        assert!(mention_tooltip_contains(true, false));
        assert!(mention_tooltip_contains(false, true));
        assert!(!mention_tooltip_contains(false, false));
    }

    #[test]
    fn mention_wash_moves_wholly_to_the_next_visual_row_at_a_wrap() {
        assert_eq!(
            display_row_segments(12..24, [12, 40]),
            vec![(1, 12, 12..24)]
        );
        assert_eq!(
            display_row_segments(8..24, [12, 40]),
            vec![(0, 0, 8..12), (1, 12, 12..24)]
        );
    }

    #[test]
    fn mention_token_requires_a_token_boundary_and_tracks_full_token() {
        assert_eq!(
            mention_token("Fix @src/com", 12),
            Some(MentionToken {
                range: 4..12,
                query: "src/com".into(),
            })
        );
        assert!(mention_token("mail@example.com", 16).is_none());
        assert!(mention_token("word@file", 9).is_none());
        assert!(mention_token("path/@file", 10).is_none());
        assert_eq!(
            mention_token("See (@lib", 9).map(|token| token.range),
            Some(5..9)
        );
    }

    #[test]
    fn slash_token_only_opens_the_prompt() {
        assert_eq!(
            slash_token("/comp", 5),
            Some(MentionToken {
                range: 0..5,
                query: "comp".into(),
            })
        );
        // Token range spans the whole command word even mid-cursor.
        assert_eq!(
            slash_token("/compact now", 3),
            Some(MentionToken {
                range: 0..8,
                query: "co".into(),
            })
        );
        // Not at offset 0 → prose, not a command.
        assert!(slash_token("run /compact", 12).is_none());
        // Cursor past the command word (typing the argument) → closed.
        assert!(slash_token("/goal ship it", 10).is_none());
        // A typed absolute path is not a command.
        assert!(slash_token("/usr/bin", 8).is_none());
        // Bare "/" with cursor at 0 → closed; cursor after it → open-all.
        assert!(slash_token("/", 0).is_none());
        assert_eq!(slash_token("/", 1).map(|t| t.query), Some(String::new()));
    }

    #[test]
    fn dismissed_mentions_reject_stale_responses() {
        let mut state = FileMentionState {
            token: mention_token("@src", 4),
            request: 7,
            ..FileMentionState::default()
        };
        assert!(mention_response_is_current(&state, 7));
        state.request += 1;
        state.token = None;
        assert!(!mention_response_is_current(&state, 7));
        assert!(!mention_response_is_current(&state, 8));
    }

    #[test]
    fn file_mentions_serialize_to_strict_local_markdown() {
        let raw = local_file_link("src/a file#[x].rs", false);
        assert_eq!(
            raw,
            "[a file#\\[x\\].rs](luna-file:src/a%20file%23%5Bx%5D.rs)"
        );
        let links = file_mention_links(&raw);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].path, "src/a file#[x].rs");
        assert_eq!(links[0].basename, "a file#[x].rs");
        assert!(!links[0].is_dir);

        let folder = local_file_link("src/components", true);
        assert_eq!(folder, "[components](luna-file:src/components/)");
        let links = file_mention_links(&folder);
        assert_eq!(links[0].path, "src/components");
        assert!(links[0].is_dir);
    }

    #[test]
    fn dropped_mentions_are_separated_from_surrounding_text() {
        let (inserted, cursor_advance) =
            dropped_file_mention("fixnow", 3..3, "src/lib.rs", false).expect("valid drop");
        assert_eq!(inserted, " [lib.rs](luna-file:src/lib.rs) ");
        assert_eq!(cursor_advance, inserted.len());

        let (inserted, cursor_advance) =
            dropped_file_mention("fix now", 3..3, "src/components", true).expect("valid drop");
        assert_eq!(inserted, " [components](luna-file:src/components/)");
        assert_eq!(cursor_advance, inserted.len() + 1);
    }

    #[test]
    fn dropped_mentions_reject_paths_outside_the_workspace() {
        assert!(dropped_file_mention("", 0..0, "/tmp/file.rs", false).is_none());
        assert!(dropped_file_mention("", 0..0, "../file.rs", false).is_none());
    }

    #[test]
    fn file_mentions_reject_external_or_noncanonical_markdown() {
        assert!(file_mention_links("[site](https://example.com/a)").is_empty());
        assert!(file_mention_links("[a.rs](../a.rs)").is_empty());
        assert!(file_mention_links("[a.rs](src/a file.rs)").is_empty());
        assert!(file_mention_links("[other](src/a.rs)").is_empty());
        assert!(file_mention_links("[a.rs](src/a.rs)").is_empty());
        assert!(file_mention_links("[a.rs](src%5Cfake%5Ca.rs)").is_empty());
        assert!(file_mention_links("[a.rs](src/a%0A.rs)").is_empty());
    }

    #[test]
    fn duplicate_mention_basenames_use_unique_suffixes() {
        let raw = format!(
            "{} {}",
            local_file_link("src/one/mod.rs", false),
            local_file_link("src/two/mod.rs", false)
        );
        let projection = TextProjection::new(&raw);
        assert!(projection.display.contains("one/mod.rs"));
        assert!(projection.display.contains("two/mod.rs"));
    }

    #[test]
    fn mention_suffixes_compare_path_components() {
        let links = vec![
            FileMentionLink {
                range: 0..0,
                basename: "mod.rs".into(),
                path: "foo/mod.rs".into(),
                is_dir: false,
            },
            FileMentionLink {
                range: 0..0,
                basename: "oomod.rs".into(),
                path: "bar/oomod.rs".into(),
                is_dir: false,
            },
        ];
        assert_eq!(
            mention_display_labels(&links),
            vec!["mod.rs".to_string(), "oomod.rs".to_string()]
        );
    }

    #[test]
    fn projection_maps_and_expands_atomic_chip_ranges() {
        let raw = format!("open {} now", local_file_link("src/composer.rs", false));
        let projection = TextProjection::new(&raw);
        let (link, chip) = &projection.mentions[0];
        assert_eq!(
            &projection.display[chip.clone()],
            "\u{00A0}@composer.rs\u{00A0}"
        );
        assert_eq!(projection.display_to_raw(chip.start + 1), link.range.start);
        assert_eq!(projection.display_to_raw(chip.end - 1), link.range.end);
        assert_eq!(
            projection.previous_boundary(link.range.end),
            Some(link.range.start)
        );
        assert_eq!(
            projection.next_boundary(link.range.start),
            Some(link.range.end)
        );
        assert_eq!(
            projection.normalize_range(link.range.start + 2..link.range.end - 2),
            link.range
        );
    }

    #[test]
    fn sent_mention_display_projects_chips_for_the_transcript() {
        let raw = format!(
            "check {} and {}",
            local_file_link("src/composer.rs", false),
            local_file_link("src/components", true)
        );
        let (display, spans) = sent_mention_display(&raw).expect("mentions project");
        assert!(!display.contains(FILE_MENTION_SCHEME));
        assert!(display.contains("composer.rs"));
        assert!(display.contains("components"));
        assert_eq!(spans.len(), 2);
        assert_eq!(
            &display[spans[0].range.clone()],
            "\u{00A0}@composer.rs\u{00A0}"
        );
        assert!(!spans[0].is_dir);
        assert_eq!(spans[0].path.as_ref(), "src/composer.rs");
        assert!(spans[1].is_dir);
        assert_eq!(spans[1].path.as_ref(), "src/components/");
    }

    /// Ordinary prompts must stay on the zero-cost path, including ones that
    /// merely *talk about* the scheme without containing a valid mention.
    #[test]
    fn sent_mention_display_leaves_plain_prompts_untouched() {
        assert_eq!(sent_mention_display("fix the composer"), None);
        assert_eq!(
            sent_mention_display("what is a luna-file: link?"),
            None,
            "scheme substring without a valid mention link"
        );
        assert_eq!(
            sent_mention_display("[a.rs](luna-file:../a.rs)"),
            None,
            "a hostile path never becomes a chip in the transcript either"
        );
    }

    fn question(id: &str, options: &[&str], multi: bool) -> UserInputQuestion {
        UserInputQuestion {
            id: id.into(),
            header: "Header".into(),
            question: format!("Question {id}"),
            options: options.iter().map(|s| s.to_string()).collect(),
            multi_select: multi,
            mcp: None,
            approval: None,
            input: None,
        }
    }

    #[test]
    fn luna_approval_marker_chooses_only_the_direct_single_question_route() {
        let mut provider = question("provider", &["Deny", "Allow once"], false);
        provider.approval = Some(luna_proto::ApprovalRequest {
            provider_request_id: "provider".into(),
            provider: HarnessId::Codex,
            turn_id: Some("turn-a".into()),
            machine_name: "synthetic".into(),
            action_kind: luna_proto::ApprovalActionKind::Tool,
            summary: "provider action".into(),
            requested_scope: luna_proto::PermissionScope::Workspace,
            expires_at_ms: i64::MAX,
            supports_task_allowance: false,
            details: None,
        });
        assert!(
            luna_approval_question(&[provider.clone()])
                .unwrap()
                .is_none()
        );

        let mut luna = provider.clone();
        luna.id = "luna".into();
        luna.approval.as_mut().unwrap().action_kind = luna_proto::ApprovalActionKind::LunaCrossTask;
        assert_eq!(
            luna_approval_question(&[luna.clone()]).unwrap(),
            Some(&luna)
        );
        assert!(luna_approval_question(&[provider, luna.clone()]).is_err());
        assert_eq!(
            approval_decision(
                &luna,
                &[UserInputAnswer {
                    question_id: "luna".into(),
                    labels: vec!["Allow once".into()],
                }],
            ),
            luna_proto::ApprovalDecision::AllowOnce
        );
    }

    #[test]
    fn caret_blink_phase() {
        // Solid through the first half-period (typing burst never blinks).
        assert!(caret_visible(0));
        assert!(caret_visible(CARET_BLINK_MS - 1));
        // Off for the second half-period, back on for the third.
        assert!(!caret_visible(CARET_BLINK_MS));
        assert!(!caret_visible(2 * CARET_BLINK_MS - 1));
        assert!(caret_visible(2 * CARET_BLINK_MS));
    }

    #[test]
    fn auto_grow_math() {
        // The source heights (luna composer.tsx line 235 clamp, composer-
        // actions.tsx row, 1px hairlines): 76+46+2 empty … 260+46+2 capped.
        assert_eq!(COMPOSER_MIN_HEIGHT, 124.0);
        assert_eq!(COMPOSER_MAX_HEIGHT, 308.0);
        // One line sits at the floor: the textarea BOX (content + `pt-4 pb-1`)
        // clamps UP to 76 exactly like `Math.max(scrollHeight, 76)` — this is
        // what makes the always-expanded new-chat composer 124px tall.
        assert_eq!(
            composer_total_height(input_content_height(1)),
            COMPOSER_MIN_HEIGHT
        );
        // Growth is linear once the textarea box exceeds its 76px floor.
        let h4 = composer_total_height(input_content_height(4));
        assert_eq!(
            h4,
            4.0 * INPUT_LINE_HEIGHT + TEXTAREA_PAD_V + ACTIONS_ROW_HEIGHT + PILL_BORDER_V
        );
        // Caps at a 260px textarea box (luna max-h-[260px] / the JS clamp).
        assert_eq!(
            composer_total_height(input_content_height(100)),
            COMPOSER_MAX_HEIGHT
        );
        // Zero lines still measures one.
        assert_eq!(input_content_height(0), INPUT_LINE_HEIGHT);
    }

    #[test]
    fn appshot_strip_height_tracks_cards() {
        assert_eq!(appshot_strip_height(0), 0.0);
        assert_eq!(appshot_strip_height(1), STRIP_PAD_TOP + APPSHOT_TILE_HEIGHT);
        assert_eq!(appshot_strip_height(2), appshot_strip_height(1));
    }

    #[test]
    fn appshot_images_share_height_and_adapt_width_without_losing_aspect_ratio() {
        let landscape = appshot_contained_size(Some((1600, 900)), 320.0);
        assert!((landscape.0 - 234.66667).abs() < 0.01);
        assert_eq!(landscape.1, APPSHOT_IMAGE_MAX_HEIGHT);
        let portrait = appshot_contained_size(Some((900, 1600)), 320.0);
        assert!((portrait.0 - 74.25).abs() < 0.01);
        assert_eq!(portrait.1, landscape.1);
        assert_eq!(
            appshot_contained_size(Some((1000, 1000)), 320.0),
            (132.0, 132.0)
        );
        // Narrow side-by-side layouts and panoramas fit without distortion.
        let narrow = appshot_contained_size(Some((1600, 900)), 160.0);
        assert_eq!(narrow, (160.0, 90.0));
        assert_eq!(
            appshot_contained_size(Some((4000, 1000)), 900.0),
            (320.0, 80.0)
        );
        let fallback = appshot_contained_size(None, 320.0);
        assert!((fallback.0 - 211.2).abs() < 0.01);
        assert_eq!(fallback.1, 132.0);
    }

    #[test]
    fn input_wheel_scroll_uses_gpui_direction_and_clamps() {
        // Positive wheel delta moves toward the start; negative moves down.
        assert_eq!(input_scroll_offset(40.0, 20.0, 200.0, 100.0), 20.0);
        assert_eq!(input_scroll_offset(40.0, -30.0, 200.0, 100.0), 70.0);
        // Neither edge can be overscrolled.
        assert_eq!(input_scroll_offset(10.0, 50.0, 200.0, 100.0), 0.0);
        assert_eq!(input_scroll_offset(90.0, -50.0, 200.0, 100.0), 100.0);
        // Short content has no internal scroll range.
        assert_eq!(input_scroll_offset(20.0, -50.0, 80.0, 100.0), 0.0);
    }

    #[test]
    fn input_scroll_reveals_only_when_caret_leaves_viewport() {
        // A visible caret preserves the user's viewport.
        assert_eq!(
            input_scroll_offset_for_cursor(40.0, 60.0, 20.0, 300.0, 100.0, None),
            40.0
        );
        // Moving above or below reveals the row with the smallest adjustment.
        assert_eq!(
            input_scroll_offset_for_cursor(80.0, 30.0, 20.0, 300.0, 100.0, None),
            30.0
        );
        assert_eq!(
            input_scroll_offset_for_cursor(20.0, 130.0, 20.0, 300.0, 100.0, None),
            50.0
        );
        // Revealing the final row clamps exactly to the content end.
        assert_eq!(
            input_scroll_offset_for_cursor(0.0, 290.0, 20.0, 300.0, 100.0, None),
            200.0
        );
    }

    #[test]
    fn input_drag_autoscroll_is_edge_proportional_and_capped() {
        let top = 100.0;
        let bottom = 300.0;
        let line = INPUT_LINE_HEIGHT;
        assert_eq!(input_drag_scroll_delta(200.0, top, bottom, line), 0.0);
        assert_eq!(input_drag_scroll_delta(90.0, top, bottom, line), -2.0);
        assert_eq!(input_drag_scroll_delta(315.0, top, bottom, line), 3.0);
        assert_eq!(input_drag_scroll_delta(-100.0, top, bottom, line), -line);
        assert_eq!(input_drag_scroll_delta(500.0, top, bottom, line), line);
    }

    /// One frame short of the full morph timeline (never rounds up to done).
    const ALMOST: f32 = 179.0;

    #[test]
    fn flip_morph_starts_once_per_committed_flip() {
        // No committed flip → no morph.
        assert_eq!(flip_morph_step(None, false, 49.0, 0.0, false, false), None);
        // A committed flip starts one, from the last rendered height…
        let m = flip_morph_step(None, true, 49.0, 100.0, false, false).unwrap();
        assert_eq!(m.from, 49.0);
        assert_eq!(m.start_ms, 100.0);
        // …and same-mode renders keep it UNCHANGED (no restart at the
        // boundary, whatever the heights are doing).
        assert_eq!(
            flip_morph_step(Some(m), false, 80.0, 150.0, false, false),
            Some(m)
        );
        // A finished morph clears on the next same-mode render.
        assert_eq!(
            flip_morph_step(Some(m), false, 124.0, 100.0 + ALMOST, false, false),
            Some(m)
        );
        assert_eq!(
            flip_morph_step(Some(m), false, 124.0, 300.0, false, false),
            None
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolved_layout_does_not_keep_notifying_on_repaint() {
        use std::cell::Cell;
        gpui_platform::headless().run(|cx| {
            cx.set_global(Theme::dark());
            let handle = cx.open_window(gpui::WindowOptions::default(), |_, cx| {
                cx.new(|cx| {
                    let mut input = ComposerInput::new("Draft", cx);
                    input.set_text("A long line whose wrapping differs between provisional and resolved widths.\n".repeat(100), cx);
                    input
                })
            }).unwrap();
            let changes = Rc::new(Cell::new(0));
            let observed = changes.clone();
            let subscription = cx.subscribe(&handle.entity(cx).unwrap(), move |_, event, _| {
                if matches!(event, ComposerInputEvent::ViewportChanged) {
                    observed.set(observed.get() + 1);
                }
            });
            cx.spawn(async move |cx| {
                let _subscription = subscription;
                cx.update(|cx| {
                    handle.update(cx, |input, _, _| input.last_notified_layout = None).unwrap();
                    cx.update_window(handle.into(), |_, window, cx| { window.refresh(); let _ = window.draw(cx); }).unwrap();
                });
                let settled = changes.get();
                assert!(settled > 0, "the first resolved layout must be published");
                for _ in 0..30 {
                    cx.update(|cx| {
                        cx.update_window(handle.into(), |_, window, cx| { window.refresh(); let _ = window.draw(cx); }).unwrap();
                    });
                }
                assert_eq!(changes.get(), settled, "unchanged draws must not schedule more layout");
                cx.update(|cx| cx.quit());
            }).detach();
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn single_line_address_reveals_caret_and_maps_scrolled_pointer() {
        gpui_platform::headless().run(|cx| {
            cx.set_global(Theme::dark());
            let handle = cx
                .open_window(gpui::WindowOptions::default(), |_, cx| {
                    cx.new(|cx| {
                        ComposerInput::new("Address", cx)
                            .with_single_line()
                            .with_text_metrics(11.0, 16.0)
                    })
                })
                .unwrap();
            handle
                .update(cx, |input, window, cx| {
                    let style = window.text_style();
                    input.set_text(
                        "http://device.a-very-long-project-name.localhost:7331/path",
                        cx,
                    );
                    input.layout_text(px(100.0), &style, window, cx);
                    assert_eq!(input.content_height, 16.0, "long hostnames must not wrap");
                    input.clamp_scroll(16.0);
                    assert!(input.scroll_left > 0.0);
                    let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(100.0), px(16.0)));
                    input.last_bounds = Some(bounds);
                    let caret = input
                        .bounds_for_range(
                            input.content.len()..input.content.len(),
                            bounds,
                            window,
                            cx,
                        )
                        .unwrap();
                    assert!(caret.left() >= bounds.left() && caret.right() <= bounds.right());
                    assert_eq!(
                        input.index_for_mouse_position(caret.origin),
                        input.content.len()
                    );
                    input.selected_range = 0..0;
                    input.clamp_scroll(16.0);
                    assert_eq!(input.scroll_left, 0.0, "Home must reveal the URL start");
                    input.replace_text_in_range(None, "one\r\ntwo", window, cx);
                    assert!(!input.content.contains(['\r', '\n']));
                    input.set_text("short", cx);
                    input.layout_text(px(100.0), &style, window, cx);
                    input.clamp_scroll(16.0);
                    assert_eq!(
                        input.scroll_left, 0.0,
                        "short replacement must reset scrolling"
                    );
                })
                .unwrap();
            cx.spawn(async move |cx| {
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn layout_cache_reuses_resize_frames_and_invalidates_text_inputs() {
        gpui_platform::headless().run(|cx| {
            cx.set_global(Theme::dark());
            let handle = cx
                .open_window(gpui::WindowOptions::default(), |_, cx| {
                    cx.new(|cx| ComposerInput::new("Draft", cx))
                })
                .unwrap();
            handle
                .update(cx, |input, window, cx| {
                    input.layout_rebuilds = 0; // Exclude the window's initial placeholder paint.
                    let mut style = window.text_style();
                    style.font_size = px(INPUT_TEXT_SIZE).into();
                    input.set_text(
                        "A wrapped draft with enough text to measure.\n".repeat(100),
                        cx,
                    );
                    input.layout_text(px(400.0), &style, window, cx);
                    assert_eq!(input.layout_rebuilds, 1);
                    let height = input.content_height;
                    for frame in 0..120 {
                        input.viewport_height = Some(40.0 + frame as f32);
                        input.scroll_top = frame as f32;
                        input.selected_range = 2..8;
                        assert_eq!(input.layout_text(px(400.0), &style, window, cx), height);
                    }
                    assert_eq!(
                        input.layout_rebuilds, 1,
                        "resize/scroll/selection must reuse shaping"
                    );
                    input.set_text("Edited draft", cx);
                    input.layout_text(px(400.0), &style, window, cx);
                    assert_eq!(input.layout_rebuilds, 2);
                    input.layout_text(px(200.0), &style, window, cx);
                    assert_eq!(input.layout_rebuilds, 3, "width changes must rewrap");
                    style.font_size = px(18.0).into();
                    input.layout_text(px(200.0), &style, window, cx);
                    assert_eq!(input.layout_rebuilds, 4);
                    input.marked_range = Some(0..2);
                    input.layout_text(px(200.0), &style, window, cx);
                    assert_eq!(
                        input.layout_rebuilds, 5,
                        "IME marking must repaint decoration"
                    );
                    input.unmark_text(window, cx);
                    input.layout_text(px(200.0), &style, window, cx);
                    assert_eq!(input.layout_rebuilds, 6, "IME unmark must also invalidate");
                    input.set_text("", cx);
                    input.layout_text(px(200.0), &style, window, cx);
                    input.set_placeholder("New placeholder", cx);
                    input.layout_text(px(200.0), &style, window, cx);
                    assert_eq!(input.layout_rebuilds, 8);
                    style.color = gpui::rgb(0xff0000).into();
                    input.layout_text(px(200.0), &style, window, cx);
                    assert_eq!(input.layout_rebuilds, 9);
                    input.enable_mentions();
                    input.layout_text(px(200.0), &style, window, cx);
                    assert_eq!(input.layout_rebuilds, 10);
                    cx.set_global(Theme::light());
                    input.layout_text(px(200.0), &style, window, cx);
                    assert_eq!(input.layout_rebuilds, 11, "mention colors follow the theme");
                })
                .unwrap();
            cx.spawn(async move |cx| {
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    }

    #[test]
    fn resize_reveals_only_complete_rows() {
        for visible in [0.0, 5.0, 22.0, 22.75, 30.0, 45.5, 70.0, 150.0] {
            let height = input_reveal_height(visible, 0.0, INPUT_LINE_HEIGHT, true);
            assert!(height <= visible);
            assert_eq!(height % INPUT_LINE_HEIGHT, 0.0);
        }
        // The row grid moves with scrolling; the clip still ends between rows.
        assert_eq!(input_reveal_height(39.0, 7.0, 20.0, true), 33.0);
        // Normal overflow scrolling keeps its full viewport and existing fades.
        assert_eq!(input_reveal_height(39.0, 7.0, 20.0, false), 39.0);
        assert_eq!(input_reveal_height(100.0, 0.0, 20.0, true), 100.0);
    }

    #[test]
    fn resize_keeps_text_anchored_to_the_input_origin() {
        // A fitting draft grows from one row to seven. Caret-follow must
        // never temporarily scroll earlier lines through the top clip.
        for visible in [0.0, 22.75, 60.0, 110.0, 159.25] {
            assert_eq!(
                input_scroll_offset_for_cursor(0.0, 136.5, 22.75, 159.25, visible, Some(159.25),),
                0.0
            );
        }
        // A genuinely overflowing draft keeps the same caret-follow offset
        // through every frame of the reveal, rather than chasing its height.
        for visible in [30.0, 100.0, 180.0, 240.0] {
            assert_eq!(
                input_scroll_offset_for_cursor(160.0, 377.25, 22.75, 400.0, visible, Some(240.0),),
                160.0
            );
        }
        // Deleting back to a fitting draft resets scroll immediately, even
        // while the old, larger viewport is still shrinking.
        assert_eq!(
            input_scroll_offset_for_cursor(160.0, 77.25, 22.75, 100.0, 240.0, Some(100.0),),
            0.0
        );
    }

    #[test]
    fn scroll_fade_ignores_temporary_resize_overflow() {
        for visible_height in [0.0, 20.0, 60.0, 100.0, 160.0] {
            let scroll = input_max_scroll(160.0, visible_height);
            assert_eq!(
                input_overflow_edges(160.0, 160.0, visible_height, scroll),
                (false, false)
            );
        }
        // Deleting a capped draft disables fading immediately, even while
        // its scroll position and outer height are still settling.
        assert_eq!(
            input_overflow_edges(100.0, 100.0, 240.0, 80.0),
            (false, false)
        );
    }

    #[test]
    fn scroll_fade_tracks_real_overflow_edges() {
        for (scroll, top, bottom) in [(0.0, false, true), (80.0, true, true), (160.0, true, false)]
        {
            assert_eq!(
                input_overflow_edges(400.0, 240.0, 240.0, scroll),
                (top, bottom)
            );
        }
    }

    #[test]
    fn content_resize_retargets_from_visible_height_and_settles() {
        let start = composer_total_height(input_content_height(3));
        let target = composer_total_height(input_content_height(6));
        let grow = flip_morph_step(None, true, start, 0.0, false, false).unwrap();
        let visible = grow.height(target, 60.0);
        assert!(visible > start && visible < target);
        // A delete during growth reverses from what is on screen, with no snap.
        let shrink = flip_morph_step(Some(grow), true, visible, 60.0, false, false).unwrap();
        assert_eq!(shrink.height(start, 60.0), visible);
        assert!(shrink.height(start, 120.0) < visible);
        assert_eq!(shrink.height(start, 240.0), start);
        assert_eq!(
            flip_morph_step(Some(shrink), false, start, 240.0, false, false),
            None
        );
        // Toggling reduced motion also cancels an already running resize.
        assert_eq!(
            flip_morph_step(Some(grow), false, visible, 60.0, true, false),
            None
        );
    }

    #[test]
    fn flip_morph_height_ramps_monotonically_to_target() {
        let m = FlipMorph {
            from: 49.0,
            start_ms: 0.0,
            spec: motion::COLLAPSE,
        };
        // Starts exactly at the committed height…
        let mut prev = m.height(124.0, 0.0);
        assert_eq!(prev, 49.0);
        // …ramps without ever moving backwards…
        for step in 1..=18 {
            let h = m.height(124.0, step as f32 * 10.0);
            assert!(h >= prev, "height regressed at {step}: {h} < {prev}");
            prev = h;
        }
        // …and lands exactly on the target when done (and stays there).
        assert_eq!(m.height(124.0, 180.0), 124.0);
        assert!(m.done(180.0));
        assert_eq!(m.height(124.0, 500.0), 124.0);
        // Collapse runs the same ramp downward.
        assert!(m.height(124.0, 90.0) > 49.0);
        let down = FlipMorph {
            from: 124.0,
            start_ms: 0.0,
            spec: motion::COLLAPSE,
        };
        assert!(down.height(49.0, 90.0) < 124.0);
        assert!(down.height(49.0, 90.0) > 49.0);
    }

    #[test]
    fn flip_morph_reverse_hands_off_from_current_height() {
        let m = FlipMorph {
            from: 49.0,
            start_ms: 0.0,
            spec: motion::COLLAPSE,
        };
        let mid = m.height(124.0, 90.0);
        assert!(mid > 49.0 && mid < 124.0);
        // A reverse flip mid-flight commits a new morph FROM the animated
        // height — continuous at the handoff, no pop to an endpoint.
        let rev = flip_morph_step(Some(m), true, mid, 90.0, false, false).unwrap();
        assert_eq!(rev.from, mid);
        assert_eq!(rev.height(49.0, 90.0), mid);
    }

    #[test]
    fn flip_morph_snaps_for_reduced_motion_and_first_paint() {
        // Reduced motion never creates a morph (the flip just snaps)…
        assert_eq!(flip_morph_step(None, true, 49.0, 0.0, true, false), None);
        // …and neither does a flip before anything was ever rendered.
        assert_eq!(flip_morph_step(None, true, 0.0, 0.0, false, false), None);
    }

    #[test]
    fn route_change_never_arms_the_morph() {
        // A flip committed inside the route-snap window must NOT animate —
        // switching sessions (chat↔chat or chat↔new-session) snaps the
        // composer straight to the target mode, like the header (round 6).
        assert_eq!(flip_morph_step(None, true, 49.0, 0.0, false, true), None);
        // The route change also kills anything already in flight…
        let m = FlipMorph {
            from: 49.0,
            start_ms: 0.0,
            spec: motion::COLLAPSE,
        };
        assert_eq!(
            flip_morph_step(Some(m), false, 80.0, 50.0, false, true),
            None
        );
        assert_eq!(
            flip_morph_step(Some(m), true, 80.0, 50.0, false, true),
            None
        );
        // …while outside the window the same flip animates as usual.
        let armed = flip_morph_step(None, true, 49.0, 300.0, false, false).unwrap();
        assert_eq!(armed.from, 49.0);
    }

    #[test]
    fn composer_surface_radius_tracks_the_dock_morph() {
        assert_eq!(composer_surface_radius(0.0), COMPOSER_RADIUS);
        assert_eq!(composer_surface_radius(1.0), COMPOSER_RADIUS - 4.0);
    }

    #[test]
    fn flip_morph_tracks_live_target_and_drives_fade() {
        let m = FlipMorph {
            from: 49.0,
            start_ms: 0.0,
            spec: motion::COLLAPSE,
        };
        // Auto-grow can move the target mid-morph: evaluation tracks the
        // live value instead of finishing on a stale height.
        assert!(m.height(159.0, 90.0) > m.height(124.0, 90.0));
        // The eased progress is the actions-row fade: 0 at commit, 1 at rest.
        assert_eq!(m.progress(0.0), 0.0);
        assert_eq!(m.progress(180.0), 1.0);
        let mid = m.progress(90.0);
        assert!(mid > 0.0 && mid < 1.0);
    }

    #[test]
    fn new_thread_route_changes_use_the_coordinated_timeline() {
        let m = FlipMorph::new_thread_transition(124.0, 0.0);
        assert_eq!(m.spec, motion::NEW_THREAD_TRANSITION);
        assert_eq!(m.height(49.0, 0.0), 124.0);
        assert!(m.height(49.0, 250.0) < 124.0);
        assert!(m.height(49.0, 250.0) > 49.0);
        assert_eq!(m.height(49.0, 420.0), 49.0);
        let reverse = FlipMorph::new_thread_transition(49.0, 0.0);
        assert_eq!(reverse.height(124.0, 0.0), 49.0);
        assert_eq!(reverse.height(124.0, 420.0), 124.0);
    }

    #[test]
    fn new_thread_selectors_restore_the_compact_floating_row() {
        assert_eq!(NEW_THREAD_SELECTOR_ROW_HEIGHT, 20.0);
        assert_eq!(SESSION_FOOTER_HEIGHT, 24.0);
    }

    #[test]
    fn route_chrome_crossfade_never_duplicates_picker_controls() {
        assert_eq!(route_chrome_opacities(1.0), (1.0, 0.0));
        assert_eq!(route_chrome_opacities(0.5), (0.0, 0.0));
        assert_eq!(route_chrome_opacities(0.0), (0.0, 1.0));
        for step in 0..=20 {
            let (new_thread, session) = route_chrome_opacities(step as f32 / 20.0);
            assert!(new_thread == 0.0 || session == 0.0);
        }
    }

    #[test]
    fn staged_comments_alone_are_content() {
        assert!(!composer_has_content("   ", 0, 0));
        assert!(composer_has_content("hi", 0, 0));
        assert!(composer_has_content("", 1, 0));
        assert!(composer_has_content("", 0, 1));
    }

    #[test]
    fn a_comment_only_stage_queues_during_a_live_run() {
        let live = true;
        let comment_only = composer_has_content("", 0, 2);
        assert_eq!(
            send_button_mode(live, comment_only),
            SendButtonMode::Queue,
            "comment-only submit must queue without interrupting the run"
        );
        // Nothing staged at all is still the stop square.
        assert_eq!(
            send_button_mode(live, composer_has_content("", 0, 0)),
            SendButtonMode::Stop
        );
    }

    #[test]
    fn send_button_morph() {
        assert_eq!(send_button_mode(false, false), SendButtonMode::Send);
        assert_eq!(send_button_mode(false, true), SendButtonMode::Send);
        assert_eq!(send_button_mode(true, true), SendButtonMode::Queue);
        assert_eq!(send_button_mode(true, false), SendButtonMode::Stop);
    }

    #[test]
    fn queued_submit_does_not_publish_an_optimistic_transcript_echo() {
        assert!(should_publish_optimistic_echo(false));
        assert!(!should_publish_optimistic_echo(true));
    }

    #[test]
    fn failed_submission_never_overwrites_newer_typing() {
        assert!(!submission_has_newer_draft(4, 4, ""));
        assert!(submission_has_newer_draft(4, 5, ""));
        assert!(submission_has_newer_draft(4, 4, "typed while sending"));
    }

    #[test]
    fn interrupt_tracking_is_idempotent_per_chat() {
        let mut pending = HashSet::new();
        assert!(begin_interrupt(&mut pending, "chat-a"));
        assert!(!begin_interrupt(&mut pending, "chat-a"));
        assert!(begin_interrupt(&mut pending, "chat-b"));
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn interrupt_tracking_releases_only_settled_chats() {
        let mut pending = HashSet::from(["chat-a".to_string(), "chat-b".to_string()]);
        retain_live_interrupts(&mut pending, |chat_id| chat_id == "chat-b");
        assert_eq!(pending, HashSet::from(["chat-b".to_string()]));
        assert!(begin_interrupt(&mut pending, "chat-a"));
    }

    #[test]
    fn interrupt_payload_targets_the_active_turn_command() {
        let intent = interrupt_intent();
        assert_eq!(intent["kind"], "session");
        assert_eq!(intent["command"]["kind"], "interrupt");
    }

    #[test]
    fn escape_consumers_keep_completion_and_wizard_priority() {
        assert!(escape_dismisses_completion("escape", true));
        assert!(!escape_dismisses_completion("escape", false));
        assert!(!escape_dismisses_completion("enter", true));

        assert!(wizard_escape_goes_back("escape", false, false));
        assert!(wizard_escape_goes_back("escape", true, true));
        assert!(!wizard_escape_goes_back("escape", true, false));
        assert!(!wizard_escape_goes_back("enter", false, true));
    }

    #[test]
    fn wizard_single_select_auto_advances_and_completes() {
        let mut w = Wizard::new(
            "req".into(),
            vec![
                question("q1", &["a", "b"], false),
                question("q2", &["x"], false),
            ],
        );
        assert_eq!(w.counter(), "1/2");
        assert_eq!(w.select(1), WizardStep::AutoAdvance);
        assert!(w.is_picked(1));
        assert_eq!(w.advance(), WizardStep::Stay);
        assert_eq!(w.counter(), "2/2");
        assert_eq!(w.select(0), WizardStep::AutoAdvance);
        let WizardStep::Done(answers) = w.advance() else {
            panic!("expected Done")
        };
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].labels, vec!["b"]);
        assert_eq!(answers[1].labels, vec!["x"]);
    }

    #[test]
    fn async_question_selection_waits_for_send_and_uses_index_ids() {
        let mut wizard = Wizard::new_async(
            "aq-call-1".into(),
            vec![luna_proto::AsyncUserInputQuestion {
                title: "Which option?".into(),
                options: Some(vec!["A".into(), "B".into()]),
            }],
        );
        assert_eq!(wizard.source, QuestionSource::Async);
        assert_eq!(wizard.select(1), WizardStep::Stay);
        assert!(wizard.page_has_answer());
        let WizardStep::Done(answers) = wizard.advance() else {
            panic!("answer expected")
        };
        assert_eq!(answers[0].question_id, "0");
        assert_eq!(answers[0].labels, vec!["B"]);
    }

    #[test]
    fn async_question_command_id_is_stable_and_bounded() {
        let chat = "chat".repeat(100);
        let request = "call".repeat(100);
        let first = async_question_command_id(&chat, &request, 0);
        assert_eq!(first, async_question_command_id(&chat, &request, 0));
        assert!(first.len() < 128);
        assert_ne!(first, async_question_command_id("other", &request, 0));
        assert_ne!(first, async_question_command_id(&chat, &request, 1));
    }

    #[test]
    fn async_question_stays_visible_after_later_assistant_text_until_resolved() {
        let question = SessionMessageEntry {
            id: "assistant-1".into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::AsyncQuestion {
                id: "aq-call-1".into(),
                questions: vec![luna_proto::AsyncUserInputQuestion {
                    title: "Which option?".into(),
                    options: Some(vec!["A".into(), "B".into()]),
                }],
                resolved: false,
            }],
            created_at: 1,
            device_id: "synthetic".into(),
            status: Some(luna_doc::MessageStatus::Complete),
            continuation_of: None,
        };
        let later = SessionMessageEntry {
            id: "assistant-2".into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Text {
                id: "t".into(),
                text: "Continuing".into(),
            }],
            created_at: 2,
            ..question.clone()
        };
        let mut transcript = vec![question, later];
        assert_eq!(
            pending_async_question(&transcript).map(|(id, _)| id),
            Some("aq-call-1".into())
        );
        assert!(!async_question_resolved(&transcript, "aq-call-1"));
        if let MessagePart::AsyncQuestion { resolved, .. } = &mut transcript[0].parts[0] {
            *resolved = true;
        }
        assert!(pending_async_question(&transcript).is_none());
        assert!(async_question_resolved(&transcript, "aq-call-1"));
    }

    #[gpui::test]
    fn pending_skip_readback_clears_only_its_own_banner(cx: &mut gpui::TestAppContext) {
        struct PendingSkipRpc {
            calls: std::sync::atomic::AtomicUsize,
        }
        #[async_trait]
        impl RpcService for PendingSkipRpc {
            async fn handle(
                &self,
                method: &str,
                params: serde_json::Value,
            ) -> Result<RpcReply, RpcError> {
                match method {
                    methods::GET_COMPOSER_DRAFT => {
                        RpcReply::value(&luna_proto::ComposerDraftRead {
                            draft: None,
                            submissions: vec![],
                        })
                    }
                    methods::GET_TASK_COMMAND_SNAPSHOT => {
                        RpcReply::value(&luna_proto::TaskCommandRead {
                            snapshot: luna_proto::TaskCommandSnapshot {
                                task_id: "async-task".into(),
                                ..Default::default()
                            },
                            events: vec![],
                            reload_required: false,
                            next_task_sequence: None,
                            has_more: false,
                        })
                    }
                    methods::EXECUTE_TASK_COMMAND => {
                        assert_eq!(params["intent"]["command"]["kind"], "respondAsyncQuestion");
                        assert_eq!(
                            params["intent"]["command"]["answers"],
                            serde_json::json!([])
                        );
                        self.calls.fetch_add(1, Ordering::SeqCst);
                        RpcReply::value(&luna_proto::CommandReceipt {
                            command_id: params["commandId"].as_str().unwrap().into(),
                            task_id: "async-task".into(),
                            intent: "respondAsyncQuestion".into(),
                            payload_hash: "synthetic".into(),
                            status: CommandReceiptStatus::Pending,
                            snapshot: luna_proto::TaskCommandSnapshot {
                                task_id: "async-task".into(),
                                ..Default::default()
                            },
                            event_sequence: None,
                            actor_id: "synthetic".into(),
                            client_id: "synthetic".into(),
                            client_session_id: "synthetic".into(),
                            execution_environment: None,
                            correlation_id: None,
                            reason: None,
                            recovery_action: None,
                            accepted_at_ms: 1,
                            committed_at_ms: None,
                            observed_at_ms: 1,
                        })
                    }
                    other => Err(RpcError::UnknownMethod(other.into())),
                }
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let service = Arc::new(PendingSkipRpc {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.set_test_engine(crate::state::EngineHandle::from_test_client(
                luna_rpc::memory_client(service.clone()),
            ));
            state.selected_chat = Some("async-task".into());
            state.transcript = vec![SessionMessageEntry {
                id: "assistant-question".into(),
                role: MessageRole::Assistant,
                parts: vec![MessagePart::AsyncQuestion {
                    id: "aq-call-1".into(),
                    questions: vec![luna_proto::AsyncUserInputQuestion {
                        title: "Optional question?".into(),
                        options: Some(vec!["A".into(), "B".into()]),
                    }],
                    resolved: false,
                }],
                created_at: 1,
                device_id: "synthetic".into(),
                status: Some(luna_doc::MessageStatus::Complete),
                continuation_of: None,
            }];
        });
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        composer.update(cx, |composer, cx| {
            composer.on_state_changed(cx);
            composer
                .input
                .update(cx, |input, cx| input.set_text("Unrelated prompt", cx));
            composer.wizard_skip(cx);
        });
        let key = ("async-task".to_string(), "aq-call-1".to_string());
        pump_until(cx, &runtime, |cx| {
            composer.read_with(cx, |composer, _| {
                composer
                    .failure
                    .as_ref()
                    .is_some_and(|failure| failure.as_ref().contains("delivery is pending"))
            })
        });
        composer.read_with(cx, |composer, cx| {
            assert!(composer.wizard.is_some());
            assert!(composer.async_submissions.contains_key(&key));
            assert_eq!(composer.input.read(cx).text(), "Unrelated prompt");
        });
        assert_eq!(service.calls.load(Ordering::SeqCst), 1);
        state.update(cx, |state, _| {
            if let MessagePart::AsyncQuestion { resolved, .. } = &mut state.transcript[0].parts[0] {
                *resolved = true;
            }
        });
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        composer.read_with(cx, |composer, cx| {
            assert!(composer.wizard.is_none());
            assert!(!composer.async_submissions.contains_key(&key));
            assert!(
                composer.failure.is_none(),
                "the matching Pending banner clears"
            );
            assert_eq!(composer.input.read(cx).text(), "Unrelated prompt");
        });
        // Readback may beat the RPC reply; the late Pending result must not
        // recreate a banner after the card has already been settled.
        state.update(cx, |state, _| {
            if let MessagePart::AsyncQuestion { id, resolved, .. } =
                &mut state.transcript[0].parts[0]
            {
                *id = "aq-call-2".into();
                *resolved = false;
            }
        });
        composer.update(cx, |composer, cx| {
            composer.on_state_changed(cx);
            composer.wizard_skip(cx);
        });
        state.update(cx, |state, _| {
            if let MessagePart::AsyncQuestion { resolved, .. } = &mut state.transcript[0].parts[0] {
                *resolved = true;
            }
        });
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        pump_until(cx, &runtime, |_| service.calls.load(Ordering::SeqCst) == 2);
        for _ in 0..10 {
            cx.run_until_parked();
            runtime.block_on(tokio::task::yield_now());
        }
        composer.read_with(cx, |composer, cx| {
            assert!(composer.wizard.is_none());
            assert!(
                composer.failure.is_none(),
                "late Pending reply stays settled"
            );
            assert_eq!(composer.input.read(cx).text(), "Unrelated prompt");
        });
        composer.update(cx, |composer, _| {
            composer.set_async_delivery_failure(&key, "Answer delivery is pending".into());
            composer.failure = Some("Unrelated draft failure".into());
            composer.clear_async_delivery_failure(&key);
            assert_eq!(composer.failure.as_deref(), Some("Unrelated draft failure"));
        });
    }

    #[gpui::test]
    fn uncertain_async_answer_retries_exact_command_without_touching_prompt(
        cx: &mut gpui::TestAppContext,
    ) {
        struct AsyncRetryRpc {
            calls: std::sync::atomic::AtomicUsize,
            envelopes: Mutex<Vec<serde_json::Value>>,
            methods: Mutex<Vec<String>>,
        }
        #[async_trait]
        impl RpcService for AsyncRetryRpc {
            async fn handle(
                &self,
                method: &str,
                params: serde_json::Value,
            ) -> Result<RpcReply, RpcError> {
                self.methods.lock().unwrap().push(method.into());
                match method {
                    methods::GET_COMPOSER_DRAFT => {
                        RpcReply::value(&luna_proto::ComposerDraftRead {
                            draft: None,
                            submissions: vec![],
                        })
                    }
                    methods::GET_TASK_COMMAND_SNAPSHOT => {
                        RpcReply::value(&luna_proto::TaskCommandRead {
                            snapshot: luna_proto::TaskCommandSnapshot {
                                task_id: "async-task".into(),
                                ..Default::default()
                            },
                            events: vec![],
                            reload_required: false,
                            next_task_sequence: None,
                            has_more: false,
                        })
                    }
                    methods::GET_TASK_COMMAND_RECEIPT => {
                        RpcReply::value(&Option::<luna_proto::CommandReceipt>::None)
                    }
                    methods::EXECUTE_TASK_COMMAND => {
                        self.envelopes.lock().unwrap().push(params.clone());
                        let call = self.calls.fetch_add(1, Ordering::SeqCst);
                        if call < 2 {
                            return Err(RpcError::Transport("synthetic lost reply".into()));
                        }
                        RpcReply::value(&luna_proto::CommandReceipt {
                            command_id: params["commandId"].as_str().unwrap().into(),
                            task_id: "async-task".into(),
                            intent: "respondAsyncQuestion".into(),
                            payload_hash: "synthetic".into(),
                            status: CommandReceiptStatus::Applied,
                            snapshot: luna_proto::TaskCommandSnapshot {
                                task_id: "async-task".into(),
                                ..Default::default()
                            },
                            event_sequence: None,
                            actor_id: "synthetic".into(),
                            client_id: "synthetic".into(),
                            client_session_id: "synthetic".into(),
                            execution_environment: None,
                            correlation_id: None,
                            reason: None,
                            recovery_action: None,
                            accepted_at_ms: 1,
                            committed_at_ms: Some(1),
                            observed_at_ms: 1,
                        })
                    }
                    other => Err(RpcError::UnknownMethod(other.into())),
                }
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let service = Arc::new(AsyncRetryRpc {
            calls: std::sync::atomic::AtomicUsize::new(0),
            envelopes: Mutex::new(vec![]),
            methods: Mutex::new(vec![]),
        });
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.set_test_engine(crate::state::EngineHandle::from_test_client(
                luna_rpc::memory_client(service.clone()),
            ));
            state.selected_chat = Some("async-task".into());
            state.transcript = vec![SessionMessageEntry {
                id: "assistant-question".into(),
                role: MessageRole::Assistant,
                parts: vec![MessagePart::AsyncQuestion {
                    id: "aq-call-1".into(),
                    questions: vec![luna_proto::AsyncUserInputQuestion {
                        title: "Which option?".into(),
                        options: Some(vec!["A".into(), "B".into()]),
                    }],
                    resolved: false,
                }],
                created_at: 1,
                device_id: "synthetic".into(),
                status: Some(luna_doc::MessageStatus::Complete),
                continuation_of: None,
            }];
        });
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        composer.update(cx, |composer, cx| {
            composer.on_state_changed(cx);
            assert!(composer.wizard.is_some(), "synthetic card must mount");
            composer
                .input
                .update(cx, |input, cx| input.set_text("Unrelated prompt", cx));
            composer.wizard_select(1, cx);
            composer.wizard_advance(cx);
            assert!(
                composer
                    .async_submissions
                    .contains_key(&("async-task".into(), "aq-call-1".into()))
            );
        });
        let mut uncertain = false;
        for _ in 0..100 {
            cx.run_until_parked();
            runtime.block_on(tokio::task::yield_now());
            std::thread::sleep(Duration::from_millis(1));
            uncertain = composer.read_with(cx, |composer, _| {
                composer
                    .async_submissions
                    .get(&("async-task".into(), "aq-call-1".into()))
                    .is_some_and(|submission| submission.state == AsyncDeliveryState::Uncertain)
            });
            if uncertain {
                break;
            }
        }
        assert!(
            uncertain,
            "async state={:?}, calls={}, methods={:?}, failure={:?}",
            composer.read_with(cx, |composer, _| composer
                .async_submissions
                .get(&("async-task".into(), "aq-call-1".into()))
                .map(|s| s.state)),
            service.calls.load(Ordering::SeqCst),
            service.methods.lock().unwrap(),
            composer.read_with(cx, |composer, _| composer.failure.clone())
        );
        composer.read_with(cx, |composer, cx| {
            assert!(
                composer.wizard.is_some(),
                "uncertain card remains actionable"
            );
            assert_eq!(composer.input.read(cx).text(), "Unrelated prompt");
        });
        let question_transcript = state.read_with(cx, |state, _| state.transcript.clone());
        state.update(cx, |state, _| {
            state.selected_chat = Some("another-task".into());
            state.transcript.clear();
        });
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        state.update(cx, |state, _| {
            state.selected_chat = Some("async-task".into());
            state.transcript = question_transcript;
        });
        composer.update(cx, |composer, cx| composer.on_state_changed(cx));
        composer.read_with(cx, |composer, _| {
            assert!(
                composer.wizard.is_some(),
                "route return restores the retry card"
            );
            assert!(
                composer
                    .async_submissions
                    .get(&("async-task".into(), "aq-call-1".into()))
                    .is_some_and(|submission| submission.state == AsyncDeliveryState::Uncertain)
            );
        });
        composer.update(cx, |composer, cx| composer.retry_async_submission(cx));
        let mut applied = false;
        for _ in 0..500 {
            cx.run_until_parked();
            runtime.block_on(tokio::task::yield_now());
            std::thread::sleep(Duration::from_millis(1));
            applied = composer.read_with(cx, |composer, _| {
                composer
                    .async_submissions
                    .get(&("async-task".into(), "aq-call-1".into()))
                    .is_some_and(|submission| submission.state == AsyncDeliveryState::Applied)
            });
            if applied {
                break;
            }
        }
        assert!(applied, "retry did not settle");
        let envelopes = service.envelopes.lock().unwrap();
        assert_eq!(envelopes.len(), 3);
        assert!(
            envelopes
                .iter()
                .all(|envelope| envelope["commandId"] == envelopes[0]["commandId"])
        );
        assert!(
            envelopes
                .iter()
                .all(|envelope| envelope["intent"] == envelopes[0]["intent"])
        );
        assert_eq!(
            envelopes[0]["intent"]["command"]["answers"][0]["labels"][0],
            "B"
        );
    }

    #[test]
    fn wizard_multi_select_toggles_and_stays() {
        let mut w = Wizard::new("req".into(), vec![question("q", &["a", "b", "c"], true)]);
        assert_eq!(w.select(0), WizardStep::Stay);
        assert_eq!(w.select(2), WizardStep::Stay);
        assert!(w.is_picked(0) && w.is_picked(2));
        // Toggle off.
        assert_eq!(w.select(0), WizardStep::Stay);
        assert!(!w.is_picked(0));
        let WizardStep::Done(answers) = w.advance() else {
            panic!()
        };
        assert_eq!(answers[0].labels, vec!["c"]);
    }

    #[test]
    fn wizard_number_keys_and_bounds() {
        let mut w = Wizard::new("req".into(), vec![question("q", &["a", "b"], false)]);
        assert_eq!(w.press_number(9), WizardStep::Stay, "out of range ignored");
        assert_eq!(w.press_number(0), WizardStep::Stay);
        assert_eq!(w.press_number(2), WizardStep::AutoAdvance);
        assert!(w.is_picked(1));
        assert_eq!(w.select(5), WizardStep::Stay, "bad option ix ignored");
    }

    #[test]
    fn wizard_typed_answer_overrides_and_back_pages() {
        let mut w = Wizard::new(
            "req".into(),
            vec![
                question("q1", &["a"], false),
                question("q2", &["x", "y"], false),
            ],
        );
        w.select(0);
        w.advance();
        assert_eq!(w.page, 1);
        assert!(w.back());
        assert_eq!(w.page, 0);
        assert!(!w.back(), "already at first page");
        w.advance();
        w.set_typed("  custom answer  ".into());
        let WizardStep::Done(answers) = w.advance() else {
            panic!()
        };
        assert_eq!(answers[0].labels, vec!["a"]);
        assert_eq!(
            answers[1].labels,
            vec!["custom answer"],
            "typed overrides picked, trimmed"
        );
    }

    #[test]
    fn pending_input_detection() {
        use luna_doc::MessageStatus;
        let input_part = MessagePart::Input {
            id: "in-r1".into(),
            request_id: "r1".into(),
            questions: vec![question("q", &["a"], false)],
            resolved: false,
        };
        let entry = |status: Option<MessageStatus>, parts: Vec<MessagePart>| SessionMessageEntry {
            id: "m".into(),
            role: MessageRole::Assistant,
            parts,
            created_at: 0,
            device_id: "d".into(),
            status,
            continuation_of: None,
        };
        // Streaming entry with unresolved input → panel.
        let t = vec![entry(
            Some(MessageStatus::Streaming),
            vec![input_part.clone()],
        )];
        assert_eq!(
            pending_input_request(&t).map(|(id, _)| id),
            Some("r1".into())
        );
        // DEAD entry with an unresolved input STILL gets the panel: the
        // question stays answerable until answered (the engine delivers the
        // answer as a resumed turn), so a run reaped under its question —
        // engine restart — must not orphan it (user report).
        let t = vec![entry(
            Some(MessageStatus::Aborted),
            vec![input_part.clone()],
        )];
        assert_eq!(
            pending_input_request(&t).map(|(id, _)| id),
            Some("r1".into())
        );
        // A NEWER assistant entry supersedes an unanswered question.
        let t = vec![
            entry(Some(MessageStatus::Aborted), vec![input_part.clone()]),
            SessionMessageEntry {
                id: "m2".into(),
                role: MessageRole::Assistant,
                parts: vec![MessagePart::Text {
                    id: "t2".into(),
                    text: "moved on".into(),
                }],
                created_at: 2,
                device_id: "d".into(),
                status: Some(MessageStatus::Complete),
                continuation_of: None,
            },
        ];
        assert!(pending_input_request(&t).is_none());
        // Resolved part → no panel.
        let resolved = MessagePart::Input {
            id: "in-r1".into(),
            request_id: "r1".into(),
            questions: vec![],
            resolved: true,
        };
        let t = vec![entry(
            Some(MessageStatus::Streaming),
            vec![resolved.clone()],
        )];
        assert!(pending_input_request(&t).is_none());
        assert!(pending_input_request(&[]).is_none());

        // Regression (user forensics): a steer prompt appends a USER entry
        // AFTER the streaming assistant entry — the question must still be
        // found (a last-entry-only read vanished the panel exactly when the
        // user typed, bricking the answer flow).
        let user_echo = SessionMessageEntry {
            id: "u2".into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "t".into(),
                text: "I answered".into(),
            }],
            created_at: 1,
            device_id: "d".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        };
        let t = vec![
            entry(Some(MessageStatus::Streaming), vec![input_part.clone()]),
            user_echo,
        ];
        assert_eq!(
            pending_input_request(&t).map(|(id, _)| id),
            Some("r1".into()),
            "question survives entries appended behind the streaming entry"
        );

        // Latch release: only an explicitly resolved matching part releases.
        assert!(!input_request_resolved(&t, "r1"));
        let t = vec![entry(Some(MessageStatus::Streaming), vec![resolved])];
        assert!(input_request_resolved(&t, "r1"));
        assert!(!input_request_resolved(&t, "other"));
    }
}

#[cfg(test)]
mod appshot_rebase_tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn appshot_only_draft_counts_as_content_and_removal_clears_it(cx: &mut TestAppContext) {
        let state = cx.new(|_| AppState::new());
        let composer = cx.new(|cx| Composer::new(state, cx));
        composer.update(cx, |composer, cx| {
            let shot = appshots::tests::shot();
            composer.stage_appshot(shot.clone(), cx);
            assert!(composer_has_content(
                "",
                composer.staged().len() + composer.staged_appshots().len(),
                0
            ));
            composer.remove_appshot(&shot.id, cx);
            assert!(composer.staged_appshots().is_empty());
            assert!(!composer_has_content(
                "",
                composer.staged().len() + composer.staged_appshots().len(),
                0
            ));
        });
    }

    #[gpui::test]
    fn failed_send_restores_complete_appshots_without_duplicates(cx: &mut TestAppContext) {
        let state = cx.new(|_| AppState::new());
        let composer = cx.new(|cx| Composer::new(state, cx));
        composer.update(cx, |composer, cx| {
            let original = appshots::tests::shot();
            let mut fresh = original.clone();
            fresh.id = "fresh".into();
            composer.stage_appshot_for("minted".into(), original.clone(), cx);
            composer.stage_appshot(fresh, cx);
            composer.restore_failed_appshots(&[original.clone()], "minted", "");
            assert_eq!(composer.staged_appshots().len(), 2);
            assert_eq!(
                composer.staged_appshots()[0].accessibility,
                original.accessibility
            );
            assert_eq!(
                composer.staged_appshots()[0].screenshot.id,
                original.screenshot.id
            );
            assert_eq!(composer.staged_appshots()[1].id, "fresh");
            assert!(!composer.appshots.contains_key("minted"));
        });
    }

    #[gpui::test]
    fn remote_queue_removal_recovers_both_appshot_drafts(cx: &mut TestAppContext) {
        let state = cx.new(|_| AppState::new());
        let composer = cx.new(|cx| Composer::new(state, cx));
        composer.update(cx, |composer, cx| {
            let original = appshots::tests::shot();
            let mut edited = original.clone();
            edited.id = "edited".into();
            composer.stage_appshot(edited, cx);
            composer.queue_edit_draft = Some(("original".into(), vec![], vec![original]));
            composer.editing_queued = Some("removed-row".into());
            composer
                .input
                .update(cx, |input, cx| input.set_text("edited", cx));
            composer.on_state_changed(cx);
            assert!(composer.editing_queued.is_none());
            assert_eq!(composer.staged_appshots().len(), 2);
            assert_eq!(composer.input.read(cx).text(), "original\n\nedited");
        });
    }
}

#[cfg(feature = "appshots-fixture")]
impl Composer {
    pub fn fixture_clear_appshots(&mut self, cx: &mut Context<Self>) {
        self.appshots.clear();
        cx.notify();
    }

    /// Show all three composer chip kinds in the isolated native QA fixture.
    pub fn fixture_stage_chip_preview(&mut self, text: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_text(text, cx));
    }

    /// Seed the real persistence path with neutral fixture-only images. This
    /// is used by the isolated L08 native QA app; it never reads a user file.
    pub fn fixture_stage_draft_evidence(&mut self, cx: &mut Context<Self>) {
        use base64::Engine as _;

        let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .expect("fixture PNG");
        let (attachment, _) = appshots::stage_appshot_png("fixture-attachment", png.clone())
            .expect("fixture attachment");
        let (screenshot, dimensions) =
            appshots::stage_appshot_png("fixture-appshot", png).expect("fixture appshot");
        self.add_staged(vec![attachment], cx);
        self.stage_appshot(
            CapturedAppshot {
                id: "l08-native-appshot".into(),
                app_name: "Synthetic Fixture".into(),
                bundle_identifier: Some("test.luna.drafts-fixture".into()),
                window_title: Some("L08 isolated native QA".into()),
                accessibility: appshots::AccessibilitySnapshot {
                    format_version: 1,
                    content: "Synthetic fixture context; no live screen data.".into(),
                    truncated: false,
                },
                screenshot,
                screenshot_dimensions: Some(dimensions),
                app_icon: None,
                captured_at: chrono::Utc::now(),
            },
            cx,
        );
    }
}
