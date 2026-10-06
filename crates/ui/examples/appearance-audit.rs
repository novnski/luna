//! Appearance audit: measured WCAG 2.2 contrast for every UI family, across
//! every built-in theme variant, every accent and every surface preference,
//! resolved through the production appearance path (`appearance::set_*` →
//! `Theme::install_selection`, with its derived readable foregrounds).
//!
//! Colors are measured as actually composited, not as token names: the shell
//! frost is flattened over the adverse desktop for its appearance (white
//! behind dark glass, black behind light glass) AND over the benign one, and
//! each foreground (with its real call-site opacity or readable helper) is
//! flattened over the composited surface it is painted on. The lower ratio
//! of the two backdrops is reported.
//!
//! Failure cases this run must catch (written before the fixes):
//! 1. Secondary labels faded below 4.5:1 on chrome, selected rows, canvas,
//!    raised chips, floating cards or input plates.
//! 2. Placeholder/hint text, status words, badges or syntax tokens below
//!    4.5:1 on the surface they sit on.
//! 3. Focus rings, status glyphs, active-tab indicators or drop outlines
//!    below 3:1 against their adjacent surface.
//! 4. Button labels below 4.5:1 on their solid, accent or destructive fills.
//! 5. A preference combination (accent, surface treatment) that breaks a
//!    pair a default combination passes.
//! 6. A cached floor reused after its palette changed (a hot-reloaded theme
//!    or a hardened foreground with the same outer glass): derived colors
//!    must clear the floor on freshly computed, uncached surfaces.
//! 7. The floor helpers cost enough per call to matter during paint.
//! 8. Terminal text in theme colors (default foreground, the 16 ANSI slots,
//!    dim, inverse, selected) paints below 4.5:1 on its actual cell
//!    background, or the cursor below 3:1.
//! 10. With new-thread artwork set, composer text or placeholder fall below
//!     4.5:1 over an all-white or all-black image (the extremes any artwork
//!     lies between), at the strength it is painted under them. (The target
//!     pickers sit in the frame's bottom row, off the artwork.)
//! 11. A terminal frame (160x50 cells, palette, selected and dim cases)
//!     costs enough in contrast floors to matter during paint.
//! 12. A palette-controlled terminal pair (default or ANSI16 foreground on
//!     the default or an ANSI16 background, plain, selected, dim, or redrawn
//!     above the focused cursor block) paints below 4.5:1 on the background
//!     actually shown behind it.
//! 13. Status/accent copy that a call site paints raw (warnings, errors,
//!     selected checkmarks, diff counts) reads below its floor on its own
//!     wash or surface.
//! 14. The audit reports failures but the process exits 0.
//! 15. A contrast floor moves an accent or status tone into another color
//!     family (more than 15° of hue as painted), e.g. Amber turned green.
//! 9. The terminal minimum-contrast adjustment reveals concealed (SGR 8)
//!    text, recolors block/box-drawing/braille graphics, or rewrites a
//!    program's explicit 256-color/true-color choice.
//!
//! Exempt pairs (decorative separators, illustrations, disabled controls)
//! are listed with their ratio but never fail the run.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example appearance-audit
//! Run:   target/debug/examples/appearance-audit ABSOLUTE_EVIDENCE_DIR
//! Writes contrast.json, contrast.md and appearance-audit.pass (or
//! failure.txt with every failing pair and exit 1).
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gpui::{Hsla, hsla};
use luna_theme::{AccentPreset, AccentSelection, SurfacePreference, ThemeRegistry};
use luna_ui::theme::{self, Theme};
use luna_ui::*;
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Normal-size text: 4.5:1.
    Text,
    /// Meaningful icon, indicator, focus visual or boundary: 3:1.
    NonText,
    /// Decorative or disabled: measured, never required.
    Exempt,
    /// Measured for the record (authored values, program-chosen colors);
    /// not a Luna guarantee and not called decorative.
    Info,
}

impl Kind {
    fn min(self) -> f32 {
        match self {
            Self::Text => 4.5,
            Self::NonText => 3.0,
            Self::Exempt | Self::Info => 0.0,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Text => "text 4.5",
            Self::NonText => "non-text 3.0",
            Self::Exempt => "exempt",
            Self::Info => "informational",
        }
    }
}

struct Pair {
    family: &'static str,
    element: &'static str,
    kind: Kind,
    fg: Hsla,
    bg: Hsla,
}

fn grey(level: f32) -> Hsla {
    hsla(0.0, 0.0, level, 1.0)
}

fn ratio(fg: Hsla, bg: Hsla) -> f32 {
    theme::contrast_ratio(theme::flatten(fg, bg), bg)
}

fn hex(color: Hsla) -> String {
    let rgba = gpui::Rgba::from(color);
    format!(
        "#{:02x}{:02x}{:02x}",
        (rgba.r * 255.0).round() as u8,
        (rgba.g * 255.0).round() as u8,
        (rgba.b * 255.0).round() as u8
    )
}

/// Every audited pair for `theme`, with surfaces composited over `backdrop`.
fn pairs(theme: &Theme, backdrop: Hsla) -> Vec<Pair> {
    let flat = theme::flatten;
    let chrome = flat(theme.glass(), backdrop);
    let hover = flat(theme.element_hover, chrome);
    let selected = flat(theme.element_active, chrome);
    let canvas = flat(theme.canvas_fill(), chrome);
    let tab_active = flat(theme.wash(0.09), canvas);
    // The focused group's selected tab: its accent tint over the pane.
    let tab_focused = flat(theme.tab_focused_fill(), canvas);
    let code = flat(theme.ink(0.035), canvas);
    let raised = flat(theme.surface_raised, chrome);
    let overlay = flat(theme.glass_overlay(), chrome);
    let input = flat(theme.input_glass_bg(), chrome);
    let card = flat(theme.card_glass_bg(), chrome);
    let dialog = flat(theme.surface_dialog, flat(theme.scrim(), chrome));
    let popup = theme.for_popup();
    // The composer pill: the frosted sidebar tint, or the opaque input plate,
    // over the canvas it floats on (composer.rs `composer_surface`).
    let pill = if theme.is_frost() {
        flat(theme.composer_sidebar_tint(), canvas)
    } else {
        flat(theme.input_glass_bg(), canvas)
    };
    let menu_selected = flat(theme::card_selected_bg(), overlay);
    let menu_selected_hover = flat(theme.ink(0.05), menu_selected);
    // Tab chips over the pane (canvas): resting, hovered, active
    // (`Theme::tab_chip_washes`, the fills render.rs paints).
    let [_, chip_hover, _] = theme.tab_chip_washes();
    let tab_hover = flat(chip_hover, canvas);
    let mut out = vec![
        // Navigation (left sidebar, titlebar).
        p("navigation", "row title", Kind::Text, theme.text, chrome),
        p(
            "navigation",
            "row title, hovered",
            Kind::Text,
            theme.text,
            hover,
        ),
        p(
            "navigation",
            "row title, selected",
            Kind::Text,
            theme.text,
            selected,
        ),
        p(
            "navigation",
            "row project/branch line",
            Kind::Text,
            theme.readable_muted(0.5),
            chrome,
        ),
        p(
            "navigation",
            "row project/branch line, selected",
            Kind::Text,
            theme.readable_muted(0.5),
            selected,
        ),
        p(
            "navigation",
            "section label",
            Kind::Text,
            theme.readable_muted(0.5),
            chrome,
        ),
        p(
            "navigation",
            "kind badge (Child/Side)",
            Kind::Text,
            theme.readable_on(
                theme.text_muted,
                theme.text_muted.opacity(0.08),
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.text_muted.opacity(0.08), chrome),
        ),
        p(
            "navigation",
            "kind badge, selected row",
            Kind::Text,
            theme.readable_on(
                theme.text_muted,
                theme.text_muted.opacity(0.08),
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.text_muted.opacity(0.08), selected),
        ),
        p(
            "navigation",
            "kind badge glyph",
            Kind::NonText,
            theme.readable_on(
                theme.text_muted.opacity(0.7),
                theme.text_muted.opacity(0.08),
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            flat(theme.text_muted.opacity(0.08), chrome),
        ),
        p(
            "navigation",
            "status word Failed",
            Kind::Text,
            theme.readable_tone(theme.danger, 1.0),
            chrome,
        ),
        p(
            "navigation",
            "status word Queued",
            Kind::Text,
            theme.readable_tone(theme.warning, 1.0),
            chrome,
        ),
        p(
            "navigation",
            "disclosure chevron",
            Kind::NonText,
            theme.readable_icon(theme.readable_muted(0.5)),
            chrome,
        ),
        p(
            "navigation",
            "titlebar breadcrumb separator",
            Kind::Exempt,
            theme.decorative_muted(0.4),
            chrome,
        ),
        p(
            "navigation",
            "disabled control glyph",
            Kind::Exempt,
            theme.decorative_muted(0.35),
            chrome,
        ),
        // Tooltips and rich hover cards: one opaque fill in every theme and
        // surface preference (`Theme::tooltip_surface`), nothing behind it
        // shows through.
        p(
            "tooltip",
            "tooltip title",
            Kind::Text,
            theme.text,
            theme.tooltip_surface(),
        ),
        p(
            "tooltip",
            "tooltip detail",
            Kind::Text,
            theme.text_muted,
            theme.tooltip_surface(),
        ),
        p(
            "tooltip",
            "tooltip popup-role detail",
            Kind::Text,
            theme.for_popup().text_muted,
            theme.for_popup().tooltip_surface(),
        ),
        // Workbench (canvas, tabs, panes, frame row).
        p(
            "workbench",
            "active tab title",
            Kind::Text,
            theme.tab_title(true, false),
            tab_active,
        ),
        p(
            "workbench",
            "focused group's active tab title",
            Kind::Text,
            theme.tab_title(true, true),
            tab_focused,
        ),
        p(
            "workbench",
            "focused group's active tab role glyph",
            Kind::NonText,
            theme.tab_role_icon(true),
            tab_focused,
        ),
        p(
            "workbench",
            "focused group's active tab close glyph",
            Kind::NonText,
            theme.text_muted,
            tab_focused,
        ),
        p(
            "workbench",
            "focused group's active tab close glyph, hovered",
            Kind::NonText,
            theme.text_muted,
            flat(theme.wash(0.12), tab_focused),
        ),
        p(
            "workbench",
            "Main badge, focused group's active tab",
            Kind::Text,
            theme.readable_on_tab_badge(
                theme.accent,
                theme.accent_wash,
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.accent_wash, tab_focused),
        ),
        p(
            "workbench",
            "inactive tab title",
            Kind::Text,
            theme.text_muted,
            canvas,
        ),
        p(
            "workbench",
            "Main badge",
            Kind::Text,
            theme.readable_on_tab_badge(
                theme.accent,
                theme.accent_wash,
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.accent_wash, canvas),
        ),
        p(
            "workbench",
            "Main badge, hovered tab",
            Kind::Text,
            theme.readable_on_tab_badge(
                theme.accent,
                theme.accent_wash,
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.accent_wash, tab_hover),
        ),
        p(
            "workbench",
            "Main badge, active tab",
            Kind::Text,
            theme.readable_on_tab_badge(
                theme.accent,
                theme.accent_wash,
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.accent_wash, tab_active),
        ),
        p(
            "workbench",
            "Main badge glyph",
            Kind::NonText,
            theme.readable_on_tab_badge(
                theme.accent,
                theme.accent_wash,
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            flat(theme.accent_wash, tab_active),
        ),
        p(
            "workbench",
            "Main badge glyph, resting tab",
            Kind::NonText,
            theme.readable_on_tab_badge(
                theme.accent,
                theme.accent_wash,
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            flat(theme.accent_wash, canvas),
        ),
        p(
            "workbench",
            "tab keyboard focus ring, focused group's active tab",
            Kind::NonText,
            theme.focus_ring(),
            tab_focused,
        ),
        p(
            "workbench",
            "tab keyboard focus ring",
            Kind::NonText,
            theme.focus_ring(),
            tab_active,
        ),
        p(
            "workbench",
            "tab keyboard focus ring, hovered tab",
            Kind::NonText,
            theme.focus_ring(),
            tab_hover,
        ),
        p(
            "workbench",
            "tab role glyph",
            Kind::NonText,
            theme.tab_role_icon(false),
            tab_active,
        ),
        p(
            "workbench",
            "drop preview outline",
            Kind::NonText,
            theme.readable_icon(theme.accent.opacity(0.7)),
            canvas,
        ),
        p(
            "workbench",
            "approval glyph",
            Kind::NonText,
            theme.readable_icon(theme.warning),
            canvas,
        ),
        p(
            "workbench",
            "error glyph",
            Kind::NonText,
            theme.readable_icon(theme.danger),
            canvas,
        ),
        // Workbench panes have no context row any more; the right pane's
        // child view (no workbench) still has one.
        p(
            "workbench",
            "right-pane chat context line",
            Kind::Text,
            theme.readable_muted(0.7),
            canvas,
        ),
        p(
            "workbench",
            "tab unread dot, resting tab",
            Kind::NonText,
            theme.tab_unread_dot(),
            canvas,
        ),
        p(
            "workbench",
            "tab unread dot, selected tab",
            Kind::NonText,
            theme.tab_unread_dot(),
            tab_active,
        ),
        p(
            "workbench",
            "tab unread dot, focused group's selected tab",
            Kind::NonText,
            theme.tab_unread_dot(),
            tab_focused,
        ),
        // The context-usage chip in the controls row, in each state, on
        // the composer pill it now sits on (`context_usage::tone`, floored
        // by `Theme::readable_on_composer` as painted).
        p(
            "composer",
            "context usage share, under 75%",
            Kind::Text,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(
                    luna_proto::ContextOperationStatus::Idle,
                    Some(0.30),
                    theme,
                ),
                theme::TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "composer",
            "context usage ring, under 75%",
            Kind::NonText,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(
                    luna_proto::ContextOperationStatus::Idle,
                    Some(0.30),
                    theme,
                ),
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "composer",
            "context usage share, 75% or more",
            Kind::Text,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(
                    luna_proto::ContextOperationStatus::Idle,
                    Some(0.80),
                    theme,
                ),
                theme::TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "composer",
            "context usage ring, 75% or more",
            Kind::NonText,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(
                    luna_proto::ContextOperationStatus::Idle,
                    Some(0.80),
                    theme,
                ),
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "composer",
            "context usage share, 90% or more",
            Kind::Text,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(
                    luna_proto::ContextOperationStatus::Idle,
                    Some(0.95),
                    theme,
                ),
                theme::TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "composer",
            "context usage ring, 90% or more",
            Kind::NonText,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(
                    luna_proto::ContextOperationStatus::Idle,
                    Some(0.95),
                    theme,
                ),
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "composer",
            "context usage share, compacting",
            Kind::Text,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(
                    luna_proto::ContextOperationStatus::Running,
                    None,
                    theme,
                ),
                theme::TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "composer",
            "context usage ring, compacting",
            Kind::NonText,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(
                    luna_proto::ContextOperationStatus::Running,
                    None,
                    theme,
                ),
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "composer",
            "context usage share, compaction unknown",
            Kind::Text,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(
                    luna_proto::ContextOperationStatus::Unknown,
                    None,
                    theme,
                ),
                theme::TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "composer",
            "context usage ring, compaction unknown",
            Kind::NonText,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(
                    luna_proto::ContextOperationStatus::Unknown,
                    None,
                    theme,
                ),
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "composer",
            "context usage share, not reported",
            Kind::Text,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(luna_proto::ContextOperationStatus::Idle, None, theme),
                theme::TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "composer",
            "context usage ring, not reported",
            Kind::NonText,
            theme.readable_on_composer(
                luna_ui::context_usage::tone(luna_proto::ContextOperationStatus::Idle, None, theme),
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            pill,
        ),
        p(
            "workbench",
            "frame footer checkout/branch",
            Kind::Text,
            theme.readable_muted(0.7),
            chrome,
        ),
        // Related chats and tool surfaces (right sidebar).
        p(
            "related/tools",
            "toolbar title",
            Kind::Text,
            theme.text,
            chrome,
        ),
        p(
            "related/tools",
            "toolbar icon",
            Kind::NonText,
            theme.text_muted,
            chrome,
        ),
        p(
            "related/tools",
            "Open badge",
            Kind::Text,
            theme.readable_on(
                theme.accent,
                theme.accent.opacity(0.10),
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.accent.opacity(0.10), chrome),
        ),
        p(
            "related/tools",
            "Open badge, selected row",
            Kind::Text,
            theme.readable_on(
                theme.accent,
                theme.accent.opacity(0.10),
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.accent.opacity(0.10), selected),
        ),
        p(
            "related/tools",
            "Open badge glyph",
            Kind::NonText,
            theme.readable_on(
                theme.accent,
                theme.accent.opacity(0.10),
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            flat(theme.accent.opacity(0.10), chrome),
        ),
        p(
            "related/tools",
            "diff file path",
            Kind::Text,
            theme.text_dim,
            chrome,
        ),
        p(
            "related/tools",
            "hunk header text",
            Kind::Text,
            changes::diff_hunk_text(theme),
            flat(theme.diff_hunk_bg, chrome),
        ),
        p(
            "related/tools",
            "diff +N count",
            Kind::Text,
            theme.readable_tone(changes::add_color(theme), 1.0),
            chrome,
        ),
        p(
            "related/tools",
            "diff -N count",
            Kind::Text,
            theme.readable_tone(changes::del_color(theme), 1.0),
            chrome,
        ),
        // Transcripts, markdown and code.
        p("transcript", "body text", Kind::Text, theme.text, canvas),
        p(
            "transcript",
            "timestamp/metadata",
            Kind::Text,
            theme.readable_muted(0.6),
            canvas,
        ),
        p(
            "transcript",
            "inline code",
            Kind::Text,
            theme.code_text,
            flat(theme.code_wash, canvas),
        ),
        p(
            "transcript",
            "code block text",
            Kind::Text,
            theme.text,
            code,
        ),
        p(
            "transcript",
            "diagnostic (warning muted)",
            Kind::Text,
            theme.warning_muted,
            canvas,
        ),
        // Composer and pickers.
        p("composer", "input text", Kind::Text, theme.text, pill),
        p(
            "composer",
            "placeholder",
            Kind::Text,
            theme.placeholder,
            pill,
        ),
        p(
            "composer",
            "hint/faint label",
            Kind::Text,
            theme.readable_faint(0.8),
            pill,
        ),
        // Focus shows on the send disc, not the pill: the focused, empty
        // composer's arrow takes the accent on the recessed disc.
        p(
            "composer",
            "send arrow, focused and empty",
            Kind::NonText,
            theme.readable_tone(theme.accent, 1.0),
            flat(theme::ink(0.08), pill),
        ),
        p(
            "composer",
            "selected text",
            Kind::Text,
            theme.text,
            flat(theme.selection, pill),
        ),
        p(
            "composer",
            "selected text, input field",
            Kind::Text,
            theme.text,
            flat(theme.selection, input),
        ),
        p(
            "composer",
            "focus border",
            Kind::NonText,
            theme.border_strong,
            pill,
        ),
        p("pickers", "menu row", Kind::Text, popup.text, overlay),
        p(
            "pickers",
            "menu secondary",
            Kind::Text,
            popup.text_muted,
            overlay,
        ),
        p(
            "pickers",
            "menu hint",
            Kind::Text,
            popup.text_faint,
            overlay,
        ),
        p(
            "pickers",
            "selected menu row",
            Kind::Text,
            popup.text,
            menu_selected,
        ),
        p(
            "pickers",
            "selected-row checkmark",
            Kind::NonText,
            theme.selected_row_icon(theme.accent),
            menu_selected,
        ),
        p(
            "pickers",
            "selected-row checkmark, hovered",
            Kind::NonText,
            theme.selected_row_icon(theme.accent),
            menu_selected_hover,
        ),
        p(
            "pickers",
            "handoff checkmark on menu",
            Kind::NonText,
            theme.readable_on_stacks(
                theme.accent,
                &[&[], &[theme.ink(0.05)]],
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            overlay,
        ),
        p(
            "pickers",
            "handoff checkmark on hovered row",
            Kind::NonText,
            theme.readable_on_stacks(
                theme.accent,
                &[&[], &[theme.ink(0.05)]],
                theme::NON_TEXT_CONTRAST_TARGET,
            ),
            flat(theme.ink(0.05), overlay),
        ),
        p(
            "pickers",
            "destructive menu glyph",
            Kind::NonText,
            theme.readable_icon(theme.danger),
            overlay,
        ),
        // Menus, dialogs, popovers, settings.
        p(
            "dialogs/settings",
            "dialog text",
            Kind::Text,
            theme.text,
            dialog,
        ),
        p(
            "dialogs/settings",
            "dialog secondary",
            Kind::Text,
            theme.text_muted,
            dialog,
        ),
        p(
            "dialogs/settings",
            "settings card text",
            Kind::Text,
            theme.text,
            card,
        ),
        p(
            "dialogs/settings",
            "settings card secondary",
            Kind::Text,
            theme.readable_muted(0.7),
            card,
        ),
        p(
            "dialogs/settings",
            "primary button label",
            Kind::Text,
            theme.on_solid,
            theme.solid,
        ),
        p(
            "dialogs/settings",
            "accent button label",
            Kind::Text,
            theme.on_accent,
            theme.accent_strong,
        ),
        p(
            "dialogs/settings",
            "destructive button label",
            Kind::Text,
            theme.on_danger(),
            theme.danger_strong,
        ),
        p(
            "dialogs/settings",
            "dialog button focus border",
            Kind::NonText,
            theme.focus_ring(),
            dialog,
        ),
        p(
            "dialogs/settings",
            "settings selected checkmark",
            Kind::NonText,
            theme.readable_icon(theme.accent),
            card,
        ),
        p(
            "dialogs/settings",
            "raised chip text",
            Kind::Text,
            theme.text,
            raised,
        ),
        p(
            "dialogs/settings",
            "archive illustration",
            Kind::Exempt,
            theme.decorative_muted(0.2),
            card,
        ),
        // Errors, warnings, approvals.
        p(
            "status",
            "danger muted copy",
            Kind::Text,
            theme.danger_muted,
            canvas,
        ),
        p(
            "status",
            "warning muted copy",
            Kind::Text,
            theme.warning_muted,
            chrome,
        ),
        p(
            "status",
            "success muted copy",
            Kind::Text,
            theme.success_muted,
            chrome,
        ),
        p(
            "status",
            "Run failed strip",
            Kind::Text,
            theme.readable_tone(theme.danger, 1.0),
            canvas,
        ),
        // Raw status copy, as each call site now paints it (see the audit
        // doc's call-site inventory): tone floored on its own wash, or on
        // the reading surfaces when it has none.
        p(
            "status",
            "theme import failure (Appearance)",
            Kind::Text,
            theme.readable_on(
                theme.warning,
                theme.warning.opacity(0.08),
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.warning.opacity(0.08), card),
        ),
        p(
            "status",
            "appearance error (Appearance)",
            Kind::Text,
            theme.readable_on(
                theme.danger,
                theme.danger.opacity(0.08),
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.danger.opacity(0.08), card),
        ),
        p(
            "status",
            "file watch error (Files)",
            Kind::Text,
            theme.readable_on(
                theme.warning_muted,
                theme.warning.opacity(0.045),
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.warning.opacity(0.045), canvas),
        ),
        p(
            "status",
            "preview warning (Files)",
            Kind::Text,
            theme.readable_on(
                theme.warning_muted,
                theme.warning.opacity(0.055),
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.warning.opacity(0.055), canvas),
        ),
        p(
            "status",
            "restore error (History)",
            Kind::Text,
            theme.readable_on(
                theme.danger_muted,
                theme.danger.opacity(0.05),
                theme::TEXT_CONTRAST_TARGET,
            ),
            flat(theme.danger.opacity(0.05), overlay),
        ),
        p(
            "status",
            "validation error copy on card",
            Kind::Text,
            theme.readable_tone(theme.danger, 1.0),
            card,
        ),
        p(
            "status",
            "validation error copy in dialog",
            Kind::Text,
            theme.readable_tone(theme.danger, 1.0),
            dialog,
        ),
        p(
            "status",
            "warning copy on card",
            Kind::Text,
            theme.readable_tone(theme.warning, 1.0),
            card,
        ),
        p(
            "status",
            "success copy on card",
            Kind::Text,
            theme.readable_tone(theme.success, 1.0),
            card,
        ),
        p(
            "status",
            "accent copy on menu",
            Kind::Text,
            theme.readable_tone(theme.accent, 1.0),
            overlay,
        ),
        p(
            "status",
            "danger copy on input",
            Kind::Text,
            theme.readable_tone(theme.danger, 1.0),
            input,
        ),
        // States: focus.
        p(
            "states",
            "focus edge on chrome",
            Kind::NonText,
            theme.border_strong,
            chrome,
        ),
        p(
            "states",
            "focus edge on canvas",
            Kind::NonText,
            theme.border_strong,
            canvas,
        ),
        p(
            "states",
            "accent focus ring on chrome",
            Kind::NonText,
            theme.focus_ring(),
            chrome,
        ),
        p(
            "states",
            "accent focus ring on card",
            Kind::NonText,
            theme.focus_ring(),
            card,
        ),
        p(
            "states",
            "accent focus ring on input",
            Kind::NonText,
            theme.focus_ring(),
            input,
        ),
    ];
    // Diff rows (changes.rs `diff_row_paint` / `diff_plain_text`): code,
    // markers and line numbers on the row's own ~5.5% wash, in the side
    // panel (chrome) and a central Diff tab (canvas).
    for (base_name, base) in [("panel", chrome), ("tab", canvas)] {
        for (kind_name, kind) in [
            ("added", changes::LineKind::Add),
            ("deleted", changes::LineKind::Del),
        ] {
            let (_, marker, wash, bar, number) = changes::diff_row_paint(kind, theme);
            let row = flat(wash.expect("tinted row"), base);
            let element = |what: &str| -> &'static str {
                intern(format!("{kind_name} row {what} ({base_name})"))
            };
            out.push(p(
                "related/tools",
                element("code text"),
                Kind::Text,
                changes::diff_plain_text(theme),
                row,
            ));
            out.push(p(
                "related/tools",
                element("marker"),
                Kind::Text,
                marker,
                row,
            ));
            out.push(p(
                "related/tools",
                element("line number"),
                Kind::Text,
                number,
                row,
            ));
            // Changed rows no longer paint an edge bar (UI polish audit
            // SS-02); measure one only where a palette still draws it.
            if let Some(bar) = bar {
                out.push(p(
                    "related/tools",
                    element("accent bar"),
                    Kind::NonText,
                    bar,
                    row,
                ));
            }
            for (name, color) in syntax(theme) {
                out.push(Pair {
                    family: "code/syntax",
                    element: intern(format!("{name} on {kind_name} diff row ({base_name})")),
                    kind: Kind::Text,
                    fg: color,
                    bg: row,
                });
            }
        }
    }
    // Terminal: the painted colors, through the renderer's own functions,
    // each on the background actually shown behind the glyph.
    use luna_ui::terminal::emulator::CellColor;
    use luna_ui::terminal::view::{
        TerminalCellPaint, terminal_cell_behind, terminal_cursor_glyph_fg, terminal_paint_fg,
    };
    let term = terminal_surface_over(theme, backdrop);
    let cell = |fg: CellColor, bg: CellColor, selected: bool, dim: bool| TerminalCellPaint {
        fg,
        bg,
        ch: 'a',
        selected,
        dim,
        hidden: false,
    };
    let painted = |cell: TerminalCellPaint| {
        (
            terminal_paint_fg(cell, theme, term),
            terminal_cell_behind(cell, theme, term),
        )
    };
    for (element, cell) in [
        (
            "default foreground",
            cell(CellColor::Foreground, CellColor::Background, false, false),
        ),
        (
            "selected text",
            cell(CellColor::Foreground, CellColor::Background, true, false),
        ),
        (
            "dim text (SGR 2)",
            cell(CellColor::Foreground, CellColor::Background, false, true),
        ),
        (
            "dim selected text",
            cell(CellColor::Foreground, CellColor::Background, true, true),
        ),
        (
            "inverse video",
            cell(CellColor::Background, CellColor::Foreground, false, false),
        ),
        (
            "inverse video, selected",
            cell(CellColor::Background, CellColor::Foreground, true, false),
        ),
    ] {
        let (fg, bg) = painted(cell);
        out.push(p("terminal", element, Kind::Text, fg, bg));
    }
    out.push(p(
        "terminal",
        "cursor block",
        Kind::NonText,
        theme.cursor,
        term,
    ));
    let (program_fg, program_bg) = painted(cell(
        CellColor::Rgb(0x40, 0x40, 0x40),
        CellColor::Background,
        false,
        false,
    ));
    out.push(p(
        "terminal",
        "true-color program text (#404040)",
        Kind::Info,
        program_fg,
        program_bg,
    ));
    // Every theme-palette pair a program can ask for: the 16 ANSI slots and
    // the default foreground on the default and 16 ANSI backgrounds, plain,
    // selected and dim (inverse swaps fg/bg, so it is the same matrix), and
    // the glyph redrawn above the focused cursor block.
    let colors: Vec<(String, CellColor)> =
        std::iter::once(("default fg".to_string(), CellColor::Foreground))
            .chain((0..16u8).map(|slot| {
                (
                    ANSI_NAMES[slot as usize].to_string(),
                    CellColor::Indexed(slot),
                )
            }))
            .collect();
    let backgrounds: Vec<(String, CellColor)> =
        std::iter::once(("default bg".to_string(), CellColor::Background))
            .chain((0..16u8).map(|slot| {
                (
                    ANSI_NAMES[slot as usize].to_string(),
                    CellColor::Indexed(slot),
                )
            }))
            .collect();
    for (fg_name, fg) in &colors {
        for (bg_name, bg) in &backgrounds {
            for (family, selected, dim) in [
                ("terminal ANSI fg×bg", false, false),
                ("terminal ANSI fg×bg selected", true, false),
                ("terminal ANSI fg×bg dim", false, true),
            ] {
                let (color, behind) = painted(cell(*fg, *bg, selected, dim));
                out.push(p(
                    family,
                    intern(format!("{fg_name} on {bg_name}")),
                    Kind::Text,
                    color,
                    behind,
                ));
            }
            let glyph = cell(*fg, *bg, false, false);
            out.push(p(
                "terminal ANSI cursor glyph",
                intern(format!("{fg_name} on {bg_name}")),
                Kind::Text,
                terminal_cursor_glyph_fg(glyph, theme, term),
                theme::flatten(theme.cursor, terminal_cell_behind(glyph, theme, term)),
            ));
        }
    }
    for slot in 0..16u8 {
        out.push(Pair {
            family: "terminal (authored palette)",
            element: ANSI_NAMES[slot as usize],
            kind: Kind::Info,
            fg: theme.terminal.ansi[slot as usize],
            bg: term,
        });
    }
    // New-thread artwork extremes. The frosted hero paints at 0.84 (opaque
    // 1.0); under the composer the cutout reveals it at half strength, where
    // the pill paints `artwork_composer_plate`. Nothing else sits on the
    // artwork: the target pickers live in the frame's bottom row, outside
    // the canvas the image is clipped to.
    let hero = if theme.is_frost() { 0.84 } else { 1.0 };
    let strength = hero * 0.5;
    let pill_plate = theme.artwork_composer_plate(strength);
    for (art, name_pill, name_hint) in [
        (
            grey(1.0),
            "composer text on white artwork",
            "placeholder on white artwork",
        ),
        (
            grey(0.0),
            "composer text on black artwork",
            "placeholder on black artwork",
        ),
    ] {
        let under_pill = flat(pill_plate, flat(art.opacity(strength), canvas));
        out.push(p("artwork", name_pill, Kind::Text, theme.text, under_pill));
        out.push(p(
            "artwork",
            name_hint,
            Kind::Text,
            theme.text_faint,
            under_pill,
        ));
    }
    for (name, color) in syntax(theme) {
        out.push(Pair {
            family: "code/syntax",
            element: name,
            kind: Kind::Text,
            fg: color,
            bg: code,
        });
    }
    out
}

const ANSI_NAMES: [&str; 16] = [
    "ANSI 0 black",
    "ANSI 1 red",
    "ANSI 2 green",
    "ANSI 3 yellow",
    "ANSI 4 blue",
    "ANSI 5 magenta",
    "ANSI 6 cyan",
    "ANSI 7 white",
    "ANSI 8 bright black",
    "ANSI 9 bright red",
    "ANSI 10 bright green",
    "ANSI 11 bright yellow",
    "ANSI 12 bright blue",
    "ANSI 13 bright magenta",
    "ANSI 14 bright cyan",
    "ANSI 15 bright white",
];

/// The terminal panel fill over the shell frost over `backdrop` (the view's
/// `terminal_surface` uses the adverse desktop; the audit tries both).
fn terminal_surface_over(theme: &Theme, backdrop: Hsla) -> Hsla {
    theme::flatten(
        luna_ui::terminal::view::terminal_panel_bg(theme),
        theme::flatten(theme.glass(), backdrop),
    )
}

/// The renderer's guards (failure case 9): concealed text stays invisible,
/// graphics keep their ink, program colors stay as chosen.
fn terminal_guard_failures(theme: &Theme, label: &str) -> Vec<String> {
    use luna_ui::terminal::emulator::CellColor;
    use luna_ui::terminal::view::{
        TerminalCellPaint, resolve_color, terminal_paint_fg, terminal_surface,
    };
    let surface = terminal_surface(theme);
    let cell = |fg, bg, ch, hidden| TerminalCellPaint {
        fg,
        bg,
        ch,
        selected: false,
        dim: false,
        hidden,
    };
    let mut failures = Vec::new();
    // Concealed: the emulator resolves SGR 8 to fg == bg; the floor must not
    // pull it back into view.
    let concealed = terminal_paint_fg(
        cell(CellColor::Background, CellColor::Background, 'x', true),
        theme,
        surface,
    );
    if concealed != resolve_color(CellColor::Background, theme) {
        failures.push(format!("{label}: concealed text was recolored"));
    }
    for ch in ['█', '▀', '─', '│', '⣿'] {
        let painted = terminal_paint_fg(
            cell(CellColor::Indexed(8), CellColor::Background, ch, false),
            theme,
            surface,
        );
        if painted != resolve_color(CellColor::Indexed(8), theme) {
            failures.push(format!("{label}: graphic {ch:?} was recolored"));
        }
    }
    for program in [CellColor::Rgb(0x30, 0x30, 0x30), CellColor::Indexed(236)] {
        let painted = terminal_paint_fg(
            cell(program, CellColor::Background, 'a', false),
            theme,
            surface,
        );
        if painted != resolve_color(program, theme) {
            failures.push(format!("{label}: program color {program:?} was rewritten"));
        }
    }
    failures
}

/// A generated element name, leaked once per distinct name (the matrix
/// repeats the same few hundred names across every combination).
fn intern(name: String) -> &'static str {
    thread_local! {
        static NAMES: std::cell::RefCell<std::collections::HashSet<&'static str>> =
            std::cell::RefCell::new(std::collections::HashSet::new());
    }
    NAMES.with_borrow_mut(|names| {
        if let Some(hit) = names.get(name.as_str()) {
            return *hit;
        }
        let leaked: &'static str = Box::leak(name.into_boxed_str());
        names.insert(leaked);
        leaked
    })
}

/// Hue in degrees of an opaque color as the eye receives it, or `None` when
/// it is too grey for hue to mean anything.
fn painted_hue(color: Hsla) -> Option<f32> {
    let rgba = gpui::Rgba::from(color);
    let (r, g, b) = (rgba.r, rgba.g, rgba.b);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let chroma = max - min;
    if chroma < 0.06 {
        return None;
    }
    let hue = if max == r {
        60.0 * (((g - b) / chroma).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / chroma + 2.0)
    } else {
        60.0 * ((r - g) / chroma + 4.0)
    };
    Some(hue)
}

fn hue_delta(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

/// Failure case 15: a contrast floor that recolors a semantic or accent tone
/// into another color family (the HSL interpolation toward `text` turned
/// Amber `#a16207` into green `#467915`). Each derived paint color is
/// compared with its source tone, both as derived and as painted on its
/// surface. A floor may darken, lighten, thicken or (where no lightness of
/// the hue reads) neutralize a tone; it may not move its hue.
/// Returns (check, source hue, painted hue, delta) per measurable check.
fn hue_checks(theme: &Theme, backdrop: Hsla) -> Vec<(&'static str, f32, f32, f32)> {
    use luna_ui::terminal::emulator::CellColor;
    use luna_ui::terminal::view::{TerminalCellPaint, terminal_cell_behind, terminal_paint_fg};
    let flat = theme::flatten;
    let chrome = flat(theme.glass(), backdrop);
    let canvas = flat(theme.canvas_fill(), chrome);
    let card = flat(theme.card_glass_bg(), chrome);
    let overlay = flat(theme.glass_overlay(), chrome);
    let [_, _, chip_active] = theme.tab_chip_washes();
    let tab_active = flat(chip_active, canvas);
    // The composer pill (as `pairs` composites it).
    let composer_pill = if theme.is_frost() {
        flat(theme.composer_sidebar_tint(), canvas)
    } else {
        flat(theme.input_glass_bg(), canvas)
    };
    let text = theme::TEXT_CONTRAST_TARGET;
    let non_text = theme::NON_TEXT_CONTRAST_TARGET;
    let mut checks: Vec<(&'static str, Hsla, Hsla, Hsla)> = vec![
        (
            "accent copy",
            theme.accent,
            theme.readable_tone(theme.accent, 1.0),
            canvas,
        ),
        (
            "warning copy",
            theme.warning,
            theme.readable_tone(theme.warning, 1.0),
            card,
        ),
        (
            "danger copy",
            theme.danger,
            theme.readable_tone(theme.danger, 1.0),
            card,
        ),
        (
            "success copy",
            theme.success,
            theme.readable_tone(theme.success, 1.0),
            card,
        ),
        (
            "accent icon",
            theme.accent,
            theme.readable_icon(theme.accent),
            canvas,
        ),
        (
            "warning icon",
            theme.warning,
            theme.readable_icon(theme.warning),
            canvas,
        ),
        (
            "danger icon",
            theme.danger,
            theme.readable_icon(theme.danger),
            overlay,
        ),
        (
            "Main badge",
            theme.accent,
            theme.readable_on_tab_badge(theme.accent, theme.accent_wash, text),
            flat(theme.accent_wash, tab_active),
        ),
        (
            "Main badge glyph",
            theme.accent,
            theme.readable_on_tab_badge(theme.accent, theme.accent_wash, non_text),
            flat(theme.accent_wash, tab_active),
        ),
        (
            "Open badge",
            theme.accent,
            theme.readable_on(theme.accent, theme.accent.opacity(0.10), text),
            flat(theme.accent.opacity(0.10), chrome),
        ),
        ("focus ring", theme.accent, theme.focus_ring(), tab_active),
        (
            "context usage share, warning",
            theme.warning,
            theme.readable_on_composer(theme.warning, text),
            composer_pill,
        ),
        (
            "context usage share, danger",
            theme.danger,
            theme.readable_on_composer(theme.danger, text),
            composer_pill,
        ),
        (
            "focused tab role glyph",
            theme.accent,
            theme.tab_role_icon(true),
            flat(theme.tab_focused_fill(), canvas),
        ),
        (
            "selected-row checkmark",
            theme.accent,
            theme.selected_row_icon(theme.accent),
            flat(theme::card_selected_bg(), overlay),
        ),
        (
            "warning copy on its wash",
            theme.warning,
            theme.readable_on(theme.warning, theme.warning.opacity(0.08), text),
            flat(theme.warning.opacity(0.08), card),
        ),
        (
            "danger copy on its wash",
            theme.danger,
            theme.readable_on(theme.danger, theme.danger.opacity(0.08), text),
            flat(theme.danger.opacity(0.08), card),
        ),
    ];
    for (name, kind) in [
        ("added diff marker", changes::LineKind::Add),
        ("deleted diff marker", changes::LineKind::Del),
    ] {
        let (_, marker, wash, _, _) = changes::diff_row_paint(kind, theme);
        let source = if kind == changes::LineKind::Add {
            changes::add_color(theme)
        } else {
            changes::del_color(theme)
        };
        checks.push((
            name,
            source,
            marker,
            flat(wash.expect("tinted row"), chrome),
        ));
    }
    let term = theme::flatten(
        luna_ui::terminal::view::terminal_panel_bg(theme),
        flat(theme.glass(), backdrop),
    );
    for slot in [1u8, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14] {
        for (selected, label) in [(false, "plain"), (true, "selected")] {
            let cell = TerminalCellPaint {
                fg: CellColor::Indexed(slot),
                bg: CellColor::Background,
                ch: 'a',
                selected,
                dim: false,
                hidden: false,
            };
            checks.push((
                intern(format!("terminal {} ({label})", ANSI_NAMES[slot as usize])),
                theme.terminal.ansi[slot as usize],
                terminal_paint_fg(cell, theme, term),
                terminal_cell_behind(cell, theme, term),
            ));
        }
    }
    checks
        .into_iter()
        .filter_map(|(name, source, derived, surface)| {
            let source_hue = painted_hue(Hsla { a: 1.0, ..source })?;
            // A neutral fallback (white or black) has no hue to keep.
            let painted = painted_hue(theme::flatten(derived, surface))?;
            Some((name, source_hue, painted, hue_delta(source_hue, painted)))
        })
        .collect()
}

/// The largest hue move a floor may cause, as painted: compositing a
/// translucent tone over a tinted surface moves its hue slightly; a family
/// change (amber → green is ~55°) does not stay under this.
const HUE_TOLERANCE_DEGREES: f32 = 15.0;

fn p(family: &'static str, element: &'static str, kind: Kind, fg: Hsla, bg: Hsla) -> Pair {
    Pair {
        family,
        element,
        kind,
        fg,
        bg,
    }
}

fn syntax(theme: &Theme) -> Vec<(&'static str, Hsla)> {
    let s = &theme.syntax;
    vec![
        ("comment", s.comment),
        ("keyword", s.keyword),
        ("string", s.string),
        ("number", s.number),
        ("type", s.type_name),
        ("function", s.function),
        ("property", s.property),
        ("constant", s.constant),
        ("variable", s.variable),
        ("operator", s.operator),
        ("punctuation", s.punctuation),
        ("tag", s.tag),
        ("attribute", s.attribute),
        ("markup link", s.markup_link),
        ("invalid", s.invalid),
    ]
}

/// The lowest ratio of `fg` on `theme`'s reading surfaces, computed fresh
/// (no cache) — the independent check for cached derivations.
fn fresh_worst(theme: &Theme, fg: Hsla) -> f32 {
    theme
        .reading_surfaces()
        .iter()
        .map(|(_, surface)| ratio(fg, *surface))
        .fold(f32::MAX, f32::min)
}

/// Cached helper outputs must hold on fresh surfaces, for the theme and for
/// a perturbed copy (a hot reload that changes only foreground roles).
fn cache_failures(theme: &Theme, label: &str) -> Vec<String> {
    let mut failures = Vec::new();
    let mut check = |theme: &Theme, which: &str| {
        for opacity in [0.35_f32, 0.5, 0.7, 0.9] {
            for (helper, color, min) in [
                ("readable_muted", theme.readable_muted(opacity), 4.5),
                ("readable_faint", theme.readable_faint(opacity), 4.5),
                (
                    "readable_tone",
                    theme.readable_tone(theme.warning, opacity),
                    4.5,
                ),
                (
                    "readable_icon",
                    theme.readable_icon(theme.accent.opacity(opacity)),
                    3.0,
                ),
            ] {
                let measured = fresh_worst(theme, color);
                if measured < min {
                    failures.push(format!(
                        "{label} [{which}] {helper}({opacity}) = {measured:.2}:1 on fresh surfaces (stale cache?)"
                    ));
                }
            }
        }
    };
    check(theme, "installed");
    // Same outer palette, different foregrounds: the cache must not serve
    // the installed theme's surfaces or floors for this one.
    let mut reloaded = theme.clone();
    reloaded.text_muted = theme::mix(theme.text_muted, theme.surface, 0.55);
    reloaded.text_faint = theme::mix(theme.text_faint, theme.surface, 0.55);
    check(&reloaded, "hot-reloaded foregrounds");
    failures
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: appearance-audit ABSOLUTE_EVIDENCE_DIR"))?,
    );
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in [
        "contrast.json",
        "contrast.md",
        "appearance-audit.pass",
        "failure.txt",
    ] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let root = tempfile::tempdir()?;
    let ui_data = root.path().join("ui");
    std::fs::create_dir_all(&ui_data)?;
    let result = Arc::new(Mutex::new(None::<anyhow::Result<()>>));
    let result_for_app = result.clone();
    gpui_platform::headless()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_base::init(cx);
            let settings = settings::UiSettings::default();
            settings::init(settings.clone(), ui_data.clone(), cx);
            let fonts = typography::register_fonts(cx);
            typography::init(
                settings.ui_font_family.clone(),
                settings.ui_font_size,
                settings.terminal_font_family.clone(),
                settings.terminal_font_size,
                settings.code_font_family.clone(),
                settings.code_font_size,
                fonts,
                cx,
            );
            theme_library::init(ui_data.clone(), cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                settings.theme_selection.clone(),
                settings.accent,
                settings.surface,
                cx,
            );
            let mut rows: Vec<Value> = Vec::new();
            // The terminal palette matrix is ~1,000 pairs per combination:
            // its rows are kept only when they fail, and its minimum per
            // matrix family is reported once (with where it occurred).
            let mut matrix_worst: std::collections::BTreeMap<&'static str, (f32, Value)> =
                Default::default();
            // Failure case 15: the largest hue move per check, with where.
            let mut hue_worst: std::collections::BTreeMap<&'static str, (f32, Value)> =
                Default::default();
            let mut failures: Vec<String> = Vec::new();
            let mut combos = 0usize;
            let accents: Vec<AccentSelection> = std::iter::once(AccentSelection::ThemeDefault)
                .chain(AccentPreset::ALL.into_iter().map(AccentSelection::Preset))
                .collect();
            for family in &ThemeRegistry::builtin().families {
                for variant in &family.variants {
                    let dark = variant.appearance.is_dark();
                    let (appearance, mode) = if dark {
                        (theme::Appearance::Dark, appearance::AppearanceMode::Dark)
                    } else {
                        (theme::Appearance::Light, appearance::AppearanceMode::Light)
                    };
                    for surface in SurfacePreference::ALL {
                        for (accent, diff) in accents
                            .iter()
                            .map(|accent| (accent, settings::DiffColorScheme::RedGreen))
                            .chain(std::iter::once((&AccentSelection::ThemeDefault, settings::DiffColorScheme::BlueOrange)))
                        {
                            appearance::set_diff_color_scheme(diff, cx);
                            appearance::set_mode(mode, cx);
                            appearance::set_theme(appearance, variant.id.clone(), cx);
                            appearance::set_accent(*accent, cx);
                            appearance::set_surface(surface, cx);
                            let theme = Theme::of(cx).clone();
                            if theme.variant_id.as_ref() != variant.id {
                                failures.push(format!(
                                    "{}: the production path resolved {} instead",
                                    variant.id, theme.variant_id
                                ));
                                continue;
                            }
                            combos += 1;
                            if *accent == AccentSelection::ThemeDefault {
                                failures.extend(terminal_guard_failures(

                                    &theme,

                                    &format!("{} · {surface:?}", variant.id),

                                ));
                                failures.extend(cache_failures(
                                    &theme,
                                    &format!("{} · {surface:?}", variant.id),
                                ));
                            }
                            // Failure case 15, every accent: floors keep each
                            // tone's color family, on both desktops.
                            for backdrop in [grey(0.0), grey(1.0)] {
                                // Failure case: a tooltip fill that lets anything
                                // behind it show through (normal or popup roles).
                                for (role, fill) in [
                                    ("tooltip", theme.tooltip_surface()),
                                    ("popup tooltip", theme.for_popup().tooltip_surface()),
                                ] {
                                    if fill.a < 0.999 {
                                        failures.push(format!(
                                            "{} · {:?} · {role} fill is translucent (alpha {:.3})",
                                            variant.id, surface, fill.a
                                        ));
                                    }
                                }
                                for (check, source, painted, delta) in hue_checks(&theme, backdrop) {
                                    let entry = hue_worst.entry(check).or_insert((0.0, Value::Null));
                                    if delta > entry.0 || entry.1.is_null() {
                                        *entry = (
                                            delta.max(entry.0),
                                            json!({
                                                "family": "hue preservation",
                                                "element": check,
                                                "accent": "Theme default",
                                                "accentMeasured": accent.label(),
                                                "variant": variant.id,
                                                "surface": format!("{surface:?}"),
                                                "kind": format!("hue drift <= {HUE_TOLERANCE_DEGREES}°"),
                                                "sourceHueDegrees": source,
                                                "paintedHueDegrees": painted,
                                                "hueDeltaDegrees": delta,
                                                "ratio": delta,
                                                "foreground": "",
                                                "background": "",
                                                "pass": delta <= HUE_TOLERANCE_DEGREES,
                                            }),
                                        );
                                    }
                                    if delta > HUE_TOLERANCE_DEGREES {
                                        failures.push(format!(
                                            "{} · {:?} · {} · {:?} · hue / {check}: source {source:.1}° painted {painted:.1}° (moved {delta:.1}° > {HUE_TOLERANCE_DEGREES}°)",
                                            variant.id, surface, accent.label(), diff,
                                        ));
                                    }
                                }
                            }
                            // Adverse (white behind dark glass, black behind
                            // light) and benign desktops; the worst counts.
                            let (adverse, benign) = if dark {
                                (grey(1.0), grey(0.0))
                            } else {
                                (grey(0.0), grey(1.0))
                            };
                            let worst = pairs(&theme, adverse);
                            let other = pairs(&theme, benign);
                            for (pair, alt) in worst.iter().zip(other.iter()) {
                                let (r_adverse, r_benign) =
                                    (ratio(pair.fg, pair.bg), ratio(alt.fg, alt.bg));
                                let measured = r_adverse.min(r_benign);
                                let fails =
                                    pair.kind != Kind::Exempt && measured < pair.kind.min();
                                // One row per variant/surface for the default
                                // accent; accents only add rows when they fail.
                                let matrix = pair.family.starts_with("terminal ANSI");
                                let record = (!matrix
                                    && *accent == AccentSelection::ThemeDefault
                                    && (diff == settings::DiffColorScheme::RedGreen
                                        || pair.family == "related/tools"))
                                    || fails;
                                if matrix
                                    && matrix_worst
                                        .get(pair.family)
                                        .is_none_or(|(worst, _)| measured < *worst)
                                {
                                    matrix_worst.insert(
                                        pair.family,
                                        (
                                            measured,
                                            json!({
                                                "variant": variant.id,
                                                "appearance": if dark { "dark" } else { "light" },
                                                "surface": format!("{surface:?}"),
                                                "treatment": format!("{:?}", theme.surface_treatment),
                                                "accent": "Theme default",
                                                "accentMeasured": accent.label(),
                                                "diffColors": format!("{diff:?}"),
                                                "family": pair.family,
                                                "element": format!("minimum over every pair (at {})", pair.element),
                                                "kind": pair.kind.label(),
                                                "foreground": hex(theme::flatten(pair.fg, pair.bg)),
                                                "background": hex(pair.bg),
                                                "ratio": measured,
                                                "pass": !fails,
                                            }),
                                        ),
                                    );
                                }
                                if fails {
                                    failures.push(format!(
                                        "{} · {:?} · {} · {:?} · {} / {}: {:.2}:1 < {:.1} ({} on {})",
                                        variant.id,
                                        surface,
                                        accent.label(),
                                        diff,
                                        pair.family,
                                        pair.element,
                                        measured,
                                        pair.kind.min(),
                                        hex(theme::flatten(pair.fg, pair.bg)),
                                        hex(pair.bg)
                                    ));
                                }
                                if record {
                                    rows.push(json!({
                                        "variant": variant.id,
                                        "appearance": if dark { "dark" } else { "light" },
                                        "surface": format!("{surface:?}"),
                                        "treatment": format!("{:?}", theme.surface_treatment),
                                        "accent": accent.label(),
"diffColors": format!("{diff:?}"),
                                        "family": pair.family,
                                        "element": pair.element,
                                        "kind": pair.kind.label(),
                                        "foreground": hex(theme::flatten(pair.fg, pair.bg)),
                                        "background": hex(pair.bg),
                                        "ratioAdverseBackdrop": r_adverse,
                                        "ratioBenignBackdrop": r_benign,
                                        "ratio": measured,
                                        "pass": !fails,
                                    }));
                                }
                            }
                        }
                    }
                }
            }
            // Paint-time cost of the floors: many distinct colors (cache misses
            // and hits) on the installed theme.
            appearance::set_mode(appearance::AppearanceMode::Dark, cx);
            let theme = Theme::of(cx).clone();
            let started = std::time::Instant::now();
            let calls = 4000;
            for step in 0..calls {
                let opacity = 0.2 + (step % 80) as f32 / 100.0;
                let _ = theme.readable_muted(opacity);
            }
            let per_call_us = started.elapsed().as_secs_f64() * 1e6 / calls as f64;
            // Generous for a debug build; the cache must keep repaint-scale work
            // (hundreds of calls per frame) well under a frame.
            if per_call_us > 250.0 {
                failures.push(format!("readable_muted costs {per_call_us:.1} µs per call"));
            }
        // A terminal frame: 160x50 cells over palette, selected and dim cases,
        // painted twice (cold, then warm memo).
        let terminal_frame = |theme: &Theme| {
            use luna_ui::terminal::emulator::CellColor;
            use luna_ui::terminal::view::{TerminalCellPaint, terminal_paint_fg, terminal_surface};
            let surface = terminal_surface(theme);
            let started = std::time::Instant::now();
            for cell in 0..(160 * 50) {
                let fg = match cell % 18 {
                    16 => CellColor::Foreground,
                    17 => CellColor::Rgb(0x40, 0x40, 0x40),
                    slot => CellColor::Indexed(slot as u8),
                };
                let _ = terminal_paint_fg(
                    TerminalCellPaint {
                        fg,
                        bg: if cell % 7 == 0 { CellColor::Indexed(0) } else { CellColor::Background },
                        ch: 'a',
                        selected: cell % 11 == 0,
                        dim: cell % 5 == 0,
                        hidden: false,
                    },
                    theme,
                    surface,
                );
            }
            started.elapsed().as_secs_f64() * 1e3
        };
        let cold_ms = terminal_frame(&theme);
        let warm_ms = terminal_frame(&theme);
        // Debug-build bound: a warm frame must stay far below a 16 ms frame.
        if warm_ms > 8.0 {
            failures.push(format!("terminal floors cost {warm_ms:.2} ms per 160x50 frame (warm)"));
        }
        rows.push(
            json!({ "timing": {
                "readableMutedMicrosPerCall": per_call_us,
                "calls": calls,
                "terminalFrameMsCold": cold_ms,
                "terminalFrameMsWarm": warm_ms,
                "terminalCells": 160 * 50,
            } }),
            );
            rows.extend(matrix_worst.into_values().map(|(_, row)| row));
            rows.extend(hue_worst.into_values().map(|(_, row)| row));
            let outcome = write_report(&output, &rows, &failures, combos);
            // The headless platform may end the process on quit without
            // returning from `run`, so the verdict is the exit status here,
            // after every artifact is written.
            if let Err(error) = &outcome {
                eprintln!("Appearance audit failed: {error:#}");
                std::process::exit(1);
            }
            *result_for_app.lock().unwrap() = Some(outcome);
            cx.quit();
        });
    let outcome = result
        .lock()
        .unwrap()
        .take()
        .unwrap_or_else(|| Err(anyhow::anyhow!("the audit did not run")));
    drop(root);
    if let Err(error) = outcome {
        eprintln!("Appearance audit failed: {error:#}");
        std::process::exit(1);
    }
    Ok(())
}

fn write_report(
    output: &std::path::Path,
    rows: &[Value],
    failures: &[String],
    combos: usize,
) -> anyhow::Result<()> {
    std::fs::write(
        output.join("contrast.json"),
        serde_json::to_vec_pretty(&json!({
            "combinations": combos,
            "failures": failures,
            "pairs": rows,
        }))?,
    )?;
    // A readable summary: per family/element, the minimum ratio across
    // every variant and surface preference (default accent).
    let mut summary: std::collections::BTreeMap<(String, String, String), (f64, String)> =
        Default::default();
    for row in rows {
        if row["accent"] != "Theme default" || row["family"] == "hue preservation" {
            continue;
        }
        let key = (
            row["family"].as_str().unwrap_or_default().to_string(),
            row["element"].as_str().unwrap_or_default().to_string(),
            row["kind"].as_str().unwrap_or_default().to_string(),
        );
        let ratio = row["ratio"].as_f64().unwrap_or(0.0);
        let at = format!(
            "{} {} ({} on {})",
            row["variant"].as_str().unwrap_or_default(),
            row["surface"].as_str().unwrap_or_default(),
            row["foreground"].as_str().unwrap_or_default(),
            row["background"].as_str().unwrap_or_default()
        );
        let entry = summary.entry(key).or_insert((f64::MAX, String::new()));
        if ratio < entry.0 {
            *entry = (ratio, at);
        }
    }
    let mut md = format!(
        "# Measured contrast\n\n{combos} theme × surface × accent combinations. Minimum ratio per element across all built-in variants and surface preferences (default accent), measured on the composited colors.\n\n| Family | Element | Requirement | Minimum | Worst case |\n| --- | --- | --- | --- | --- |\n"
    );
    for ((family, element, kind), (ratio, at)) in &summary {
        md.push_str(&format!(
            "| {family} | {element} | {kind} | {ratio:.2}:1 | {at} |\n"
        ));
    }
    md.push_str(
        "\n## Hue preservation\n\nLargest hue move per derived tone across every combination and both desktops (source tone vs. painted color).\n\n| Check | Largest move | Where | Source → painted hue |\n| --- | --- | --- | --- |\n",
    );
    for row in rows
        .iter()
        .filter(|row| row["family"] == "hue preservation")
    {
        md.push_str(&format!(
            "| {} | {:.1}° | {} {} ({}) | {:.1}° → {:.1}° |\n",
            row["element"].as_str().unwrap_or_default(),
            row["hueDeltaDegrees"].as_f64().unwrap_or(f64::NAN),
            row["variant"].as_str().unwrap_or_default(),
            row["surface"].as_str().unwrap_or_default(),
            row["accentMeasured"].as_str().unwrap_or_default(),
            row["sourceHueDegrees"].as_f64().unwrap_or(f64::NAN),
            row["paintedHueDegrees"].as_f64().unwrap_or(f64::NAN),
        ));
    }
    md.push_str(&format!("\n{} failing pair(s).\n", failures.len()));
    for failure in failures {
        md.push_str(&format!("- {failure}\n"));
    }
    std::fs::write(output.join("contrast.md"), md)?;
    if failures.is_empty() {
        std::fs::write(
            output.join("appearance-audit.pass"),
            format!(
                "PASS: {combos} combinations; every required text pair >= 4.5:1 and non-text pair >= 3:1 on composited surfaces over adverse and benign desktops; exempt pairs listed separately\n"
            ),
        )?;
        Ok(())
    } else {
        std::fs::write(output.join("failure.txt"), failures.join("\n") + "\n")?;
        anyhow::bail!("{} failing pair(s); see failure.txt", failures.len())
    }
}
