//! The shared control kit (audit DS-02 / DS-16 / SH-10): buttons, icon
//! buttons, pop-up triggers and segmented controls in three sizes, so every
//! push button on a pane shares one height, corner and type size.
//!
//! | Size | Height | Radius | Type | Side pad | Glyph |
//! | ---- | ------ | ------ | ---- | -------- | ----- |
//! | `Sm` | 24     | 6      | 11   | 8        | 12    |
//! | `Md` | 28     | 8      | 12   | 10       | 14    |
//! | `Lg` | 32     | 8      | 13   | 12       | 16    |
//!
//! `Sm` is for toolbars, inline row actions and chips, `Md` for the Settings
//! control column and panel headers, `Lg` for dialogs and the sidebar footer.
//! Every control eases its hover through [`HoverFill`] and draws the shared
//! focus ring.
//!
//! The ladder is quoted in points at the default 16 px interface size. Heights,
//! padding, gaps and glyphs are interface rems, so every control grows and
//! shrinks with Appearance → Interface size alongside its label; only corners
//! and hairlines stay in pixels.

use gpui::{
    AnyElement, Div, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, Stateful, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use crate::interaction::HoverFill;
use crate::motion;
use crate::theme::{FillTier, Theme};

/// The control size ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlSize {
    Sm,
    Md,
    Lg,
}

impl ControlSize {
    pub const fn height(self) -> f32 {
        match self {
            ControlSize::Sm => 24.0,
            ControlSize::Md => 28.0,
            ControlSize::Lg => 32.0,
        }
    }

    pub const fn radius(self) -> f32 {
        match self {
            ControlSize::Sm => crate::theme::radius::SM,
            ControlSize::Md | ControlSize::Lg => crate::theme::radius::MD,
        }
    }

    pub const fn text(self) -> f32 {
        match self {
            ControlSize::Sm => 11.0,
            ControlSize::Md => 12.0,
            ControlSize::Lg => 13.0,
        }
    }

    const fn line_height(self) -> f32 {
        match self {
            ControlSize::Sm => 14.0,
            ControlSize::Md => 16.0,
            ControlSize::Lg => 18.0,
        }
    }

    pub const fn pad_x(self) -> f32 {
        match self {
            ControlSize::Sm => 8.0,
            ControlSize::Md => 10.0,
            ControlSize::Lg => 12.0,
        }
    }

    pub const fn glyph(self) -> f32 {
        match self {
            ControlSize::Sm => 12.0,
            ControlSize::Md => 14.0,
            ControlSize::Lg => 16.0,
        }
    }
}

/// Button emphasis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// The one solid action in a group.
    Primary,
    /// A filled, outlined push button.
    Secondary,
    /// A quiet action with a faint resting plate, so it never reads as body
    /// text.
    Ghost,
    /// A destructive action.
    Danger,
}

struct KindColors {
    rest: Hsla,
    hover: Hsla,
    fg: Hsla,
    hover_fg: Hsla,
    border: Option<Hsla>,
}

fn kind_colors(kind: ButtonKind, theme: &Theme) -> KindColors {
    match kind {
        ButtonKind::Primary => KindColors {
            rest: theme.text,
            hover: theme.text.opacity(0.86),
            fg: theme.on_solid,
            hover_fg: theme.on_solid,
            border: None,
        },
        ButtonKind::Secondary => KindColors {
            rest: theme.fill(FillTier::Quaternary),
            hover: theme.fill(FillTier::Tertiary),
            fg: theme.text,
            hover_fg: theme.text,
            border: Some(theme.separator(true)),
        },
        ButtonKind::Ghost => KindColors {
            rest: theme.fill(FillTier::Quaternary),
            hover: theme.fill(FillTier::Tertiary),
            fg: theme.readable_muted(0.95),
            hover_fg: theme.text,
            border: None,
        },
        ButtonKind::Danger => KindColors {
            rest: theme.danger_strong,
            hover: theme.danger_strong.opacity(0.86),
            fg: theme.on_danger(),
            hover_fg: theme.on_danger(),
            border: None,
        },
    }
}

/// The frame every control shares: id, role, height, corner, type and the
/// focus ring. A resting transparent border keeps outlined and plain kinds the
/// same size.
fn control_frame(id: ElementId, size: ControlSize, enabled: bool, theme: &Theme) -> Stateful<Div> {
    control_frame_inner(id, size, enabled, true, theme)
}

/// `swallow_keys`: push buttons stop Enter/Space key-downs so GPUI's single
/// keyboard click is the only activation. Pop-up triggers own their keys
/// (arrows, type-ahead, Enter to open) and opt out.
fn control_frame_inner(
    id: ElementId,
    size: ControlSize,
    enabled: bool,
    swallow_keys: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let ring = theme.focus_ring();
    div()
        .id(id)
        .role(gpui::Role::Button)
        .flex_none()
        .h(crate::typography::ui_rems(size.height()))
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(crate::typography::ui_rems(if size == ControlSize::Sm {
            4.0
        } else {
            6.0
        }))
        .rounded(px(size.radius()))
        .border_1()
        .border_color(gpui::transparent_black())
        .text_size(crate::typography::ui_rems(size.text()))
        .line_height(crate::typography::ui_rems(size.line_height()))
        .font_weight(FontWeight::MEDIUM)
        .when(enabled, |el| {
            el.tab_index(0)
                .cursor_pointer()
                .focus_visible(move |style| crate::theme::focus_ring(style, ring))
        })
        .when(enabled && swallow_keys, |el| {
            el.on_key_down(crate::swallow_activation_key)
        })
        .when(!enabled, |el| el.cursor_default().opacity(0.45))
}

/// A push button. The caller adds the label (and an optional leading
/// [`glyph`]), the accessible name and the click handler.
pub fn button(
    id: impl Into<SharedString>,
    kind: ButtonKind,
    size: ControlSize,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    button_keyed(ElementId::from(id.clone()), id, kind, size, enabled, theme)
}

/// [`button`] for a caller whose element id is not a plain string: `key` is
/// the hover-fade key and must be unique on screen.
pub fn button_keyed(
    id: ElementId,
    key: SharedString,
    kind: ButtonKind,
    size: ControlSize,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let colors = kind_colors(kind, theme);
    let frame = control_frame(id, size, enabled, theme)
        .px(crate::typography::ui_rems(size.pad_x()))
        .when_some(colors.border, |el, border| el.border_color(border));
    if enabled {
        frame.hover_fill_fg(key, colors.rest, colors.hover, colors.fg, colors.hover_fg)
    } else {
        frame.bg(colors.rest).text_color(colors.fg)
    }
}

/// A square glyph-only button: transparent at rest, the shared hover wash
/// under the pointer. The caller adds the accessible name and tooltip.
pub fn icon_button(
    id: impl Into<SharedString>,
    glyph_path: &'static str,
    size: ControlSize,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    // The glyph brightens with the hover like a label would; an SVG takes
    // only its own colour, so it rides the same fade explicitly.
    let tone = if enabled {
        motion::hover_blend(&id, theme.readable_muted(0.95), theme.text)
    } else {
        theme.readable_muted(0.95)
    };
    let frame = control_frame(ElementId::from(id.clone()), size, enabled, theme)
        .w(crate::typography::ui_rems(size.height()))
        .child(glyph(glyph_path, size).text_color(tone));
    if enabled {
        frame.hover_fill_fg(
            id,
            gpui::transparent_black(),
            theme.fill(FillTier::Tertiary),
            theme.readable_muted(0.95),
            theme.text,
        )
    } else {
        frame.text_color(theme.readable_muted(0.95))
    }
}

/// A leading glyph sized for `size`, in the theme's secondary glyph tone.
/// GPUI paints an SVG only with its own text colour, never the button's, so
/// the glyph carries one: inside a Primary or Danger button, chain
/// `.text_color(theme.on_solid)` / `.text_color(theme.on_danger())`.
pub fn glyph(path: &'static str, size: ControlSize) -> gpui::Svg {
    crate::icons::icon(path)
        .size(crate::typography::ui_rems(size.glyph()))
        .flex_none()
        .text_color(crate::theme::current_glyph_tone())
}

/// A pop-up button: a secondary control showing the current choice and one
/// chevron. The caller adds the click that opens the menu.
pub fn select(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    size: ControlSize,
    open: bool,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    select_frame(id, size, open, enabled, theme)
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .child(select_chevron(theme))
}

/// The pop-up button's single chevron: `alt-arrow-down` at 14 pt.
pub fn select_chevron(theme: &Theme) -> gpui::Svg {
    crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
        .size(crate::typography::ui_rems(14.0))
        .flex_none()
        .text_color(theme.readable_muted(0.85))
}

/// The pop-up button's frame without content, for triggers that show more
/// than a label (a palette preview, a glyph). Add [`select_chevron`] last.
/// The trigger keeps its own key handling (no Enter/Space swallowing).
pub fn select_frame(
    id: impl Into<SharedString>,
    size: ControlSize,
    open: bool,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let colors = kind_colors(ButtonKind::Secondary, theme);
    let frame = control_frame_inner(ElementId::from(id.clone()), size, enabled, false, theme)
        .pl(crate::typography::ui_rems(size.pad_x()))
        .pr(crate::typography::ui_rems(size.pad_x() - 2.0))
        .justify_start()
        .gap(crate::typography::ui_rems(8.0))
        .font_weight(FontWeight::NORMAL)
        .when_some(colors.border, |el, border| el.border_color(border))
        .aria_expanded(open);
    if enabled {
        frame.hover_fill_fg(
            id,
            if open { colors.hover } else { colors.rest },
            colors.hover,
            colors.fg,
            colors.fg,
        )
    } else {
        frame.bg(colors.rest).text_color(colors.fg)
    }
}

/// A selectable chip for a choice among many (projects, models): a quiet
/// filled control that takes the accent wash and edge when chosen. Two to
/// five fixed options use [`segmented`] instead.
pub fn chip(
    id: impl Into<SharedString>,
    selected: bool,
    size: ControlSize,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let (rest, hover, fg, hover_fg, border) = if selected {
        (
            theme.accent_wash,
            theme.accent_wash,
            theme.readable_tone(theme.accent, 1.0),
            theme.readable_tone(theme.accent, 1.0),
            theme.accent.opacity(0.55),
        )
    } else {
        (
            theme.fill(FillTier::Quaternary),
            theme.fill(FillTier::Tertiary),
            theme.readable_muted(0.95),
            theme.text,
            theme.separator(false),
        )
    };
    let frame = control_frame(ElementId::from(id.clone()), size, enabled, theme)
        .px(crate::typography::ui_rems(size.pad_x()))
        .border_color(border)
        .font_weight(if selected {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        })
        .aria_toggled(if selected {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        });
    if enabled {
        frame.hover_fill_fg(id, rest, hover, fg, hover_fg)
    } else {
        frame.bg(rest).text_color(fg)
    }
}

/// The selection ring for swatches and option cards (audit DS-16): a 2 pt
/// accent ring 2 pt outside `child`, following its corner (`inner_radius`
/// + 4). Unselected, the ring's space is reserved so choosing never shifts
/// the layout.
pub fn selection_ring(
    selected: bool,
    inner_radius: f32,
    theme: &Theme,
    child: impl IntoElement,
) -> Div {
    div()
        .flex_none()
        .p(px(2.0))
        .border_2()
        .border_color(if selected {
            theme.accent
        } else {
            gpui::transparent_black()
        })
        .rounded(px(inner_radius + 4.0))
        .child(child)
}

/// One option of a [`segmented`] control.
pub struct Segment {
    pub key: SharedString,
    pub label: SharedString,
    pub glyph: Option<&'static str>,
}

impl Segment {
    pub fn new(key: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            glyph: None,
        }
    }

    pub fn glyph(mut self, glyph: &'static str) -> Self {
        self.glyph = Some(glyph);
        self
    }
}

/// Track inset around the thumb.
const SEGMENT_INSET: f32 = 2.0;

/// A segmented control (audit DS-16): equal-width options on one track, with
/// a single thumb that slides to the chosen option ([`motion::QUICK`]) rather
/// than each option swapping its own fill. `on_select` receives the chosen
/// index; pass `cx.listener(|view, ix: &usize, window, cx| ..)` to reach a view.
///
/// `width` is the whole control's width in points at the default interface
/// size; like the height it is an interface rem, so labels keep their room at
/// every Interface size. Options share it equally and the thumb is placed by
/// fraction, so nothing has to be measured.
#[allow(clippy::too_many_arguments)]
pub fn segmented(
    id: impl Into<SharedString>,
    segments: Vec<Segment>,
    selected: usize,
    width: f32,
    size: ControlSize,
    theme: &Theme,
    on_select: impl Fn(&usize, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let count = segments.len().max(1);
    let selected = selected.min(count - 1);
    let position = motion::follow(&format!("{id}-thumb"), selected as f32, motion::QUICK);
    let thumb_radius = crate::theme::radius::inner(size.radius(), SEGMENT_INSET);
    let thumb_fill = if theme.appearance.is_light() {
        gpui::white()
    } else {
        theme.fill(FillTier::Primary)
    };
    let on_select = std::rc::Rc::new(on_select);
    let ring = theme.focus_ring();
    let options: Vec<AnyElement> = segments
        .into_iter()
        .enumerate()
        .map(|(ix, segment)| {
            let chosen = ix == selected;
            let on_select = on_select.clone();
            div()
                .id(ElementId::from(SharedString::from(format!(
                    "{id}-{}",
                    segment.key
                ))))
                .role(gpui::Role::RadioButton)
                .aria_label(segment.label.clone())
                .aria_toggled(if chosen {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .tab_index(0)
                .relative()
                .flex_1()
                .min_w_0()
                .h_full()
                .px(crate::typography::ui_rems(6.0))
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(crate::typography::ui_rems(5.0))
                .rounded(px(thumb_radius))
                .cursor_pointer()
                .focus_visible(move |style| crate::theme::focus_ring(style, ring))
                .on_key_down(crate::swallow_activation_key)
                .hover_fg(
                    format!("{id}-{}", segment.key),
                    if chosen {
                        theme.text
                    } else {
                        theme.readable_muted(0.9)
                    },
                    theme.text,
                )
                .font_weight(if chosen {
                    FontWeight::MEDIUM
                } else {
                    FontWeight::NORMAL
                })
                .on_click(move |_, window, cx| on_select(&ix, window, cx))
                .when_some(segment.glyph, |el, path| {
                    el.child(glyph(path, ControlSize::Sm).text_color(if chosen {
                        theme.text
                    } else {
                        theme.readable_muted(0.9)
                    }))
                })
                .child(div().min_w_0().truncate().child(segment.label))
                .into_any_element()
        })
        .collect();
    let thumb = div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(gpui::relative(position / count as f32))
        .w(gpui::relative(1.0 / count as f32))
        .rounded(px(thumb_radius))
        .bg(thumb_fill)
        .when(theme.appearance.is_light(), |el| {
            el.border_1()
                .border_color(theme.separator(false))
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::hsla(0.0, 0.0, 0.0, 0.08),
                    offset: gpui::point(px(0.0), px(1.0)),
                    blur_radius: px(2.0),
                    spread_radius: px(0.0),
                    inset: false,
                }])
        });
    div()
        .id(ElementId::from(id.clone()))
        .role(gpui::Role::RadioGroup)
        .flex_none()
        .w(crate::typography::ui_rems(width))
        .h(crate::typography::ui_rems(size.height()))
        .p(px(SEGMENT_INSET))
        .rounded(px(size.radius()))
        .bg(theme.fill(FillTier::Quaternary))
        .border_1()
        .border_color(theme.separator(false))
        .text_size(crate::typography::ui_rems(size.text()))
        .child(
            // The content box the thumb's fractions are taken against.
            div().relative().size_full().child(thumb).child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_row()
                    .children(options),
            ),
        )
}
