//! Shared scaffolding for the settings pages, so every page reads as the same
//! product surface. The design spec is
//! `docs/design/settings-polish-2026-10-06/spec.md`; in short:
//!
//! - A page is a [`page_column`] holding a [`page_title_bar`] (title plus the
//!   page's own actions at the right), an optional [`page_subtitle`], then
//!   sections separated by [`SECTION_GAP`].
//! - A section is ONE card ([`section_card`]) whose title lives inside it, in
//!   a [`section_header`] (title, description, trailing actions), followed by
//!   [`card_row`]s divided by hairlines. A title is never left floating
//!   outside the card it names.
//! - A row is [`row_text`] (title + wrapping description) and a trailing
//!   [`row_controls`] column. Rows wrap, so at narrow widths or large
//!   Interface sizes the controls drop under the text instead of squeezing it.
//! - Every size that sits next to text is an interface rem
//!   ([`crate::typography::ui_rems`]), so the whole page follows Appearance →
//!   Interface size. Only hairlines, corners and shadows stay in pixels.

use gpui::{AnyElement, Context, Pixels, ScrollHandle, SharedString, div, prelude::*, px};

use crate::popover::{self, MenuScrollbarMetrics, MenuScrollbarState, ScrollRailHost};
use crate::theme::{Theme, ink};

/// Owned scroll + floating-scrollbar state for one settings page.
///
/// This is the dedicated settings scroll container state. It wraps the same
/// `MenuScrollbarState` treatment as the model-picker (`pickers.rs`) and the
/// composer popups (`composer.rs`): the rail is hidden until hover/drag and
/// floats above content without consuming layout width.
///
/// Every settings page follows the same shape: a `scroll: PageScroll` field,
/// a [`ScrollRailHost`] impl on the page delegating here, and a root
/// `.relative()` host carrying only the list-hover `on_hover` around the
/// `.overflow_y_scroll().track_scroll(&self.scroll.scroll)` page, with
/// [`popover::rail`] supplying the rail and all of its listeners.
pub struct PageScroll {
    /// Tracked by the page's scrolling list directly — including the appshots
    /// half of the shortcuts page, which renders from its own file.
    pub scroll: ScrollHandle,
    bar: MenuScrollbarState,
}

impl Default for PageScroll {
    fn default() -> Self {
        Self {
            scroll: ScrollHandle::new(),
            bar: MenuScrollbarState::default(),
        }
    }
}

impl ScrollRailHost for PageScroll {
    fn rail_bar(&mut self) -> &mut MenuScrollbarState {
        &mut self.bar
    }

    fn rail_scroll(&self) -> Option<ScrollHandle> {
        Some(self.scroll.clone())
    }
}

impl PageScroll {
    pub fn set_list_hovered(&mut self, hovered: bool) -> bool {
        self.bar.set_list_hovered(hovered)
    }

    fn set_bar_hovered(&mut self, hovered: bool) -> bool {
        self.bar.set_bar_hovered(hovered)
    }

    fn begin_press(&mut self, pointer_y: Pixels) -> bool {
        self.bar.begin_press(&self.scroll, pointer_y)
    }

    fn drag_to(&self, pointer_y: Pixels) -> bool {
        self.bar.drag_to(&self.scroll, pointer_y)
    }

    fn end_press(&mut self) -> bool {
        self.bar.end_press()
    }

    /// Rewind to the top and drop the rail's activity baseline — what a page
    /// flip must do when one [`PageScroll`] is rerouted at a different list
    /// (shortcuts ↔ appshots), so the new page opens unscrolled and the
    /// offset jump is not read back as scrolling.
    pub fn reset(&mut self) {
        popover::reset_menu_scroll(&self.scroll, &mut self.bar);
    }

    /// Records scroll activity, then computes the rail geometry. Call once
    /// per render ([`rail`] pairs this with the hide-countdown scheduling).
    fn metrics(&mut self) -> Option<MenuScrollbarMetrics> {
        self.bar.note_scroll(&self.scroll);
        self.bar.metrics(&self.scroll)
    }
}

/// [`popover::rail`] for a [`PageScroll`] that is not its view's only rail:
/// `popover::rail` binds to the view's single [`ScrollRailHost`] impl, and a
/// page carrying a second, menu-local scroll host (the appearance page's
/// interface-font dropdown) cannot route that impl at both. The wiring
/// mirrors [`popover::rail`]; `reach` re-borrows the state from the view
/// inside the strip's pointer listeners, which fire with a `&mut V`.
pub fn rail<V: 'static>(
    scroll: &mut PageScroll,
    id: &'static str,
    theme: &Theme,
    cx: &mut Context<V>,
    reach: impl Fn(&mut V) -> &mut PageScroll + Copy + 'static,
) -> Option<AnyElement> {
    let metrics = scroll.metrics()?;
    popover::schedule_scrollbar_hide(&mut scroll.bar, cx);
    let strip = scroll.bar.render_rail(theme, metrics)?;
    Some(
        strip
            .id(id)
            .on_hover(cx.listener(move |view, hovered: &bool, _, cx| {
                if reach(view).set_bar_hovered(*hovered) {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |view, event: &gpui::MouseDownEvent, _, cx| {
                    if reach(view).begin_press(event.position.y) {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }),
            )
            .on_drag(popover::MenuScrollbarDrag, |_, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| popover::MenuScrollbarDragGhost)
            })
            .on_drag_move(cx.listener(
                move |view, event: &gpui::DragMoveEvent<popover::MenuScrollbarDrag>, _, cx| {
                    if reach(view).drag_to(event.event.position.y) {
                        cx.notify();
                    }
                },
            ))
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(move |view, _: &gpui::MouseUpEvent, _, cx| {
                    reach(view).end_press();
                    cx.notify();
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(move |view, _: &gpui::MouseUpEvent, _, cx| {
                    reach(view).end_press();
                    cx.notify();
                }),
            )
            .into_any_element(),
    )
}

/// Shared typography for a settings component's title and description. The
/// Shortcuts page established this compact rhythm; list-style settings reuse
/// it instead of drifting by page.
pub const ROW_TITLE_SIZE: f32 = 13.0;
pub const ROW_DESCRIPTION_SIZE: f32 = 12.0;

/// Space between sections on a page, and between the page header and the
/// first section.
pub const SECTION_GAP: f32 = 24.0;
/// Inner padding of section headers and rows (horizontal).
pub const ROW_PAD_X: f32 = 16.0;
/// Inner padding of rows (vertical).
pub const ROW_PAD_Y: f32 = 12.0;
/// Minimum width of a row's text column before its controls wrap under it.
pub const ROW_TEXT_MIN: f32 = 220.0;

/// Centered page column. Width and margins are interface rems: a larger
/// Interface size widens the column instead of cramming the same text into it.
pub fn page_column() -> gpui::Div {
    div()
        .w_full()
        .max_w(crate::typography::ui_rems(720.0))
        .mx_auto()
        .px(crate::typography::ui_rems(32.0))
        .pt(crate::typography::ui_rems(28.0))
        .pb(crate::typography::ui_rems(72.0))
        .flex()
        .flex_col()
}

/// The page's title bar: the title (and an optional count) at the left, the
/// page's own actions (Refresh, a device picker) at the right on the title's
/// center line. It wraps, so a narrow window moves the actions under the
/// title instead of overlapping it.
pub fn page_title_bar(
    theme: &Theme,
    title: &str,
    count: Option<usize>,
    actions: Vec<AnyElement>,
) -> gpui::Div {
    div()
        .w_full()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_x(crate::typography::ui_rems(16.0))
        .gap_y(crate::typography::ui_rems(8.0))
        .child(page_header(theme, title, count))
        .when(!actions.is_empty(), |bar| {
            bar.child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(crate::typography::ui_rems(8.0))
                    .children(actions),
            )
        })
}

/// Page headline row: `flex items-baseline gap-2.5` — `text-base font-semibold`
/// title + `text-[13px]` count sharing a baseline (luna settings.devices.tsx).
pub fn page_header(theme: &Theme, title: &str, count: Option<usize>) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_baseline()
        .gap(px(10.0))
        // The page title is the top of a clear ladder (audit DS-22):
        // title1 20/25 semibold over 13 pt group headers and rows.
        .child(
            div()
                .text_size(crate::typography::TextStyle::Title1.size())
                .line_height(crate::typography::TextStyle::Title1.line_height())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(SharedString::from(title.to_string())),
        )
        .when_some(count, |el, count| {
            el.child(
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.readable_muted(0.7))
                    .child(SharedString::from(format!("{count}"))),
            )
        })
}

/// Subtitle under the page title: body text in the secondary tone, held to
/// a readable measure.
pub fn page_subtitle(theme: &Theme, copy: impl Into<SharedString>) -> gpui::Div {
    div()
        .mt(crate::typography::ui_rems(6.0))
        .max_w(crate::typography::ui_rems(600.0))
        .text_size(crate::typography::ui_rems(13.0))
        .line_height(crate::typography::ui_rems(19.0))
        .text_color(theme.readable_muted(0.9))
        .child(copy.into())
}

/// Small label above a group of controls (`text-[13px] font-medium`) — the
/// "Theme" caption over a picker, not a page headline.
pub fn field_label(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    // A group header belongs to the group below it (audit DS-07): 24 pt
    // above (with the column's 12 pt gap), 8 pt to its own card.
    div()
        .mt(px(12.0))
        .mb(px(-4.0))
        .text_size(crate::typography::ui_rems(13.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.readable_muted(0.85))
        .child(label.into())
}

/// A row of equally-sized preview cards for picking one of N *visual* options.
///
/// Deliberately knows nothing about themes: the caller supplies each preview as
/// an arbitrary element and picks however many cards it wants, so the same
/// control works for a density picker, a layout picker or anything else where
/// the choice is easier to show than to describe. Pair with [`option_card`].
pub fn option_card_row() -> gpui::Div {
    div().flex().flex_row().items_start().gap(px(16.0)).w_full()
}

/// Default height of an [`option_card`] preview frame.
pub const OPTION_CARD_HEIGHT: f32 = 148.0;
/// Corner radius of the preview frame.
///
/// Public because the preview has to round *itself* to this. gpui content masks
/// are axis-aligned rectangles, so `overflow_hidden` on the frame clips to its
/// bounding box and not to its corner radius — a preview that paints its own
/// background will square off the corners and cover the frame's border with it.
pub const OPTION_CARD_RADIUS: f32 = 6.0;

/// One card in an [`option_card_row`]: a fixed-height preview frame with a quiet
/// selected edge and caption underneath. There is deliberately no outer card
/// or ring; the preview itself is the control.
///
/// `preview` fills the frame and **must round its own corners** to
/// [`OPTION_CARD_RADIUS`] if it paints a background — see that constant.
///
/// Returns a plain `Div` like the rest of this module — the caller adds `.id(..)`
/// and `.on_click(..)`, so selection behaviour stays with the page that owns the
/// state.
pub fn option_card(
    theme: &Theme,
    icon_path: &'static str,
    label: impl Into<SharedString>,
    selected: bool,
    preview: AnyElement,
) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.0))
        .cursor_pointer()
        .child(
            crate::button::selection_ring(
                selected,
                OPTION_CARD_RADIUS,
                theme,
                div()
                    .h(px(OPTION_CARD_HEIGHT))
                    .w_full()
                    .rounded(px(OPTION_CARD_RADIUS))
                    .overflow_hidden()
                    .border_1()
                    .border_color(theme.border)
                    .child(preview),
            )
            .w_full(),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(13.0))
                .font_weight(if selected {
                    gpui::FontWeight::MEDIUM
                } else {
                    gpui::FontWeight::NORMAL
                })
                // Accent is reserved for the ring (audit DS-16).
                .text_color(if selected {
                    theme.text
                } else {
                    theme.text_muted
                })
                .child(crate::icons::icon(icon_path).size(px(16.0)).flex_none())
                .child(label.into()),
        )
}

/// Section card: one rounded group on the card tone, thinned to a
/// translucent tint over glass so it reads as frost instead of a solid slab
/// ([`Theme::card_glass_bg`]). Put its title inside, with
/// [`section_header`], then [`card_row`]s.
pub fn section_card(theme: &Theme) -> gpui::Div {
    div()
        .mt(crate::typography::ui_rems(SECTION_GAP))
        .rounded(px(12.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.card_glass_bg())
        .overflow_hidden()
        .flex()
        .flex_col()
}

/// A section's title block, inside its card: the title (title3 15/20
/// semibold), an optional wrapping description, and optional trailing actions
/// on the title's line. Follow it with `card_row(theme, false)` rows so a
/// hairline separates the header from the content.
pub fn section_header(
    theme: &Theme,
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    actions: Vec<AnyElement>,
) -> gpui::Div {
    use crate::typography::{TextStyle, TypeStyleExt as _};
    div()
        .w_full()
        .px(crate::typography::ui_rems(ROW_PAD_X))
        .pt(crate::typography::ui_rems(14.0))
        .pb(crate::typography::ui_rems(12.0))
        .flex()
        .flex_row()
        .flex_wrap()
        .items_start()
        .gap_x(crate::typography::ui_rems(12.0))
        .gap_y(crate::typography::ui_rems(8.0))
        .child(
            div()
                .flex_1()
                .min_w(crate::typography::ui_rems(ROW_TEXT_MIN))
                .flex()
                .flex_col()
                .gap(crate::typography::ui_rems(3.0))
                .child(
                    div()
                        .type_style(TextStyle::Title3)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(title.into()),
                )
                .when_some(description, |column, description| {
                    column.child(row_description(theme, description))
                }),
        )
        .when(!actions.is_empty(), |header| {
            header.child(row_controls().children(actions))
        })
}

/// The text column of a row: a title and an optional description that wraps.
/// It holds a minimum width, so when the row can't fit it beside the controls
/// the controls wrap below it rather than squeezing the text.
pub fn row_text(
    theme: &Theme,
    title: impl Into<SharedString>,
    description: Option<SharedString>,
) -> gpui::Div {
    div()
        .flex_1()
        .min_w(crate::typography::ui_rems(ROW_TEXT_MIN))
        .flex()
        .flex_col()
        .gap(crate::typography::ui_rems(2.0))
        .child(row_title(theme, title))
        .when_some(description, |column, description| {
            column.child(row_description(theme, description))
        })
}

/// A row's description: footnote 12/16 in the secondary tone, wrapping.
pub fn row_description(theme: &Theme, copy: impl Into<SharedString>) -> gpui::Div {
    div()
        .min_w_0()
        .text_size(crate::typography::ui_rems(ROW_DESCRIPTION_SIZE))
        .line_height(crate::typography::ui_rems(16.0))
        .text_color(theme.readable_muted(0.85))
        .child(copy.into())
}

/// A row's trailing control column: controls on one line at the right,
/// vertically centred. When the row wraps it stays right-aligned.
pub fn row_controls() -> gpui::Div {
    div()
        .flex_none()
        .ml_auto()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_end()
        .gap(crate::typography::ui_rems(8.0))
}

/// A full-width block under a row's text, for controls too wide for the
/// trailing column (chip groups, lists, editors). Place it as a child of the
/// [`card_row`]: the row wraps it onto its own line and its gap spaces it, so
/// the block adds no margin of its own.
pub fn row_block() -> gpui::Div {
    div()
        .w_full()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(crate::typography::ui_rems(8.0))
}

/// Fine print under a section card (what a setting affects, a caveat):
/// footnote text aligned with the card's content edge.
pub fn section_footer(theme: &Theme, copy: impl Into<SharedString>) -> gpui::Div {
    div()
        .mt(crate::typography::ui_rems(8.0))
        .px(crate::typography::ui_rems(ROW_PAD_X))
        .text_size(crate::typography::ui_rems(ROW_DESCRIPTION_SIZE))
        .line_height(crate::typography::ui_rems(16.0))
        .text_color(theme.readable_muted(0.8))
        .child(copy.into())
}

/// One card row: `border-t border-border px-5 py-3.5 first:border-t-0`.
pub fn card_row(theme: &Theme, first: bool) -> gpui::Div {
    // One row anatomy (audit DS-08), in interface rems: 16 pt sides, 12 pt
    // above and below, 12 pt between tile, text and controls, at least
    // 48 pt tall. It wraps: with a [`row_text`] column the controls drop
    // under the text when they no longer fit beside it. The row itself has
    // no hover: it isn't a control (audit DS-05).
    div()
        .w_full()
        .min_h(crate::typography::ui_rems(48.0))
        .px(crate::typography::ui_rems(ROW_PAD_X))
        .py(crate::typography::ui_rems(ROW_PAD_Y))
        .when(!first, |el| el.border_t_1().border_color(theme.border))
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_x(crate::typography::ui_rems(12.0))
        .gap_y(crate::typography::ui_rems(10.0))
}

/// The identity tile on a row: `size-9 rounded-[10px] border bg-white/[0.03]`
/// around a 16px icon.
pub fn row_tile(theme: &Theme, icon_path: &'static str) -> gpui::Div {
    div()
        .flex_none()
        .size(crate::typography::ui_rems(28.0))
        .rounded(px(7.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.fill(crate::theme::FillTier::Quaternary))
        .flex()
        .items_center()
        .justify_center()
        .child(
            crate::icons::icon(icon_path)
                .size(crate::typography::ui_rems(16.0))
                .text_color(theme.text_muted),
        )
}

/// Row title. These metrics intentionally match the Shortcuts rows, whose
/// title/description rhythm is the reference for the other settings cards.
pub fn row_title(theme: &Theme, title: impl Into<SharedString>) -> gpui::Div {
    // Wraps rather than truncating: a clipped setting name is unreadable,
    // and the row grows instead (it has no fixed height).
    div()
        .min_w_0()
        .text_size(crate::typography::ui_rems(ROW_TITLE_SIZE))
        .line_height(crate::typography::ui_rems(18.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text)
        .child(title.into())
}

/// The quiet meta line under a row title. Light appearance keeps the authored
/// muted tone intact; fading it over pale cards made useful copy hard to read.
pub fn meta_line(theme: &Theme, fragments: Vec<AnyElement>) -> gpui::Div {
    let mut line = div()
        .min_w_0()
        .mt(px(Theme::TEXT_STACK_GAP))
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_x(px(8.0))
        .gap_y(px(2.0))
        .text_size(crate::typography::ui_rems(ROW_DESCRIPTION_SIZE))
        // The secondary tier, held to the text floor (audit DS-13); a raw
        // 65% fade dropped below it on frosted cards.
        .text_color(theme.readable_muted(0.8));
    let mut first = true;
    for fragment in fragments {
        if !first {
            line = line.child(
                div()
                    .text_color(if theme.appearance.is_light() {
                        theme.text_muted
                    } else {
                        theme.text_muted.opacity(0.3)
                    })
                    .child(SharedString::from("·")),
            );
        }
        // A fragment may wrap but never overflow into the control column
        // (audit DS-03): it can shrink below its one-line width.
        line = line.child(div().min_w_0().flex_shrink(1.0).child(fragment));
        first = false;
    }
    line
}

/// Right-anchored badge pill: `rounded-full border px-2 py-0.5 text-[10.5px]`.
pub fn badge(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex_none()
        .px(crate::typography::ui_rems(8.0))
        .py(crate::typography::ui_rems(2.0))
        .rounded_full()
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_muted)
        .child(label.into())
}

/// Emerald status pill (the Accounts "Active" badge:
/// `bg-emerald-400/[0.12] text-emerald-300/90`).
pub fn badge_active(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    let emerald = theme.success;
    let emerald_text = theme.success_muted; // emerald-300
    div()
        .flex_none()
        .px(crate::typography::ui_rems(8.0))
        .py(crate::typography::ui_rems(2.0))
        .rounded_full()
        .bg(emerald.opacity(0.12))
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.readable_tone(emerald_text, 0.9))
        .child(label.into())
}

/// Display-only toggle switch: a pill whose knob slides right and whose track
/// fills with the accent when on. State is owned by the parent row — the
/// caller adds `.id(..)` and `.on_click(..)`. `key` names this switch for its
/// motion and must be unique on screen (use the control's id).
pub fn toggle_switch(theme: &Theme, on: bool, key: &str) -> gpui::Div {
    // macOS-style switch (audit DS-06): the accent track when on and a white
    // knob with a soft shadow, in macOS proportions. The knob travels and the
    // track crossfades only when the value flips; a page that mounts with the
    // switch on shows it on (see `motion::follow`).
    const W: f32 = 34.0;
    const H: f32 = 20.0;
    const KNOB: f32 = 16.0;
    let track_off = if theme.appearance.is_light() {
        theme.text_muted.opacity(0.45)
    } else {
        ink(0.18)
    };
    let t = crate::motion::follow(
        &format!("switch-{key}"),
        if on { 1.0 } else { 0.0 },
        crate::motion::DISCLOSURE,
    );
    div()
        .flex_none()
        .w(px(W))
        .h(px(H))
        .rounded_full()
        .bg(crate::motion::mix(track_off, theme.accent_strong, t))
        .relative()
        .child(
            div()
                .absolute()
                .top(px((H - KNOB) / 2.0))
                .left(px(crate::motion::lerp(2.0, W - KNOB - 2.0, t)))
                .size(px(KNOB))
                .rounded_full()
                .bg(gpui::white())
                .shadow_sm(),
        )
}

/// [`toggle_switch`] as a complete control: an announced, focusable Switch
/// labelled `label`. GPUI's keyboard click delivers Enter/Space to the same
/// `toggle` as the pointer, exactly once. The focus ring is a transparent 2px
/// border pulled back by a matching negative margin, so focusing the switch
/// never moves its row.
pub fn switch_control<V: 'static>(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    on: bool,
    theme: &Theme,
    cx: &mut Context<V>,
    toggle: impl Fn(&mut V, &mut Context<V>) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let ring = theme.focus_ring();
    let id: gpui::ElementId = id.into();
    let key = id.to_string();
    div()
        .id(id)
        .role(gpui::Role::Switch)
        .aria_label(label)
        .aria_toggled(if on {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        })
        .tab_index(0)
        .flex_none()
        .m(px(-2.0))
        .rounded_full()
        .border_2()
        .border_color(gpui::transparent_black())
        .focus_visible(move |style| style.border_color(ring))
        .cursor_pointer()
        .on_click(cx.listener(move |view, _, _, cx| toggle(view, cx)))
        .on_key_down(crate::swallow_activation_key)
        .child(toggle_switch(theme, on, &key))
}

/// The Settings control-column action: a medium ghost button from the shared
/// kit (audit DS-02), with its faint resting plate, eased hover, focus ring
/// and disabled look. The caller adds the label, accessible name and click.
pub fn ghost_action(
    id: impl Into<SharedString>,
    enabled: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    crate::button::button(
        id,
        crate::button::ButtonKind::Ghost,
        crate::button::ControlSize::Md,
        enabled,
        theme,
    )
}

/// The dismissible red error strip (`flex items-start gap-2 rounded-xl border
/// border-red-400/20 bg-red-400/[0.06] text-red-300/90` with a leading
/// `DangerTriangle mt-0.5 size-4`).
pub fn error_strip(theme: &Theme, message: impl Into<SharedString>) -> gpui::Div {
    let red = theme.danger; // red-400
    let red_text = theme.danger_muted; // red-300
    div()
        .mt(crate::typography::ui_rems(16.0))
        .px(crate::typography::ui_rems(16.0))
        .py(crate::typography::ui_rems(12.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(red.opacity(0.2))
        .bg(red.opacity(0.06))
        .text_size(crate::typography::ui_rems(12.5))
        .text_color(theme.readable_tone(red_text, 0.9))
        .flex()
        .flex_row()
        .items_start()
        .gap(px(8.0))
        .child(
            div().flex_none().mt(crate::typography::ui_rems(2.0)).child(
                crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                    .size(crate::typography::ui_rems(16.0))
                    .text_color(theme.readable_tone(red_text, 0.9)),
            ),
        )
        .child(div().min_w_0().child(message.into()))
}

/// The amber warning strip (`flex items-start gap-2 border-amber-400/20
/// bg-amber-400/[0.06] text-amber-200/90` with a leading `DangerTriangle
/// mt-0.5 size-3.5`).
pub fn warning_strip(theme: &Theme, message: impl Into<SharedString>) -> gpui::Div {
    let amber = theme.warning; // amber-400
    let amber_text = theme.warning_muted; // amber-200
    div()
        .mt(crate::typography::ui_rems(8.0))
        .px(crate::typography::ui_rems(16.0))
        .py(crate::typography::ui_rems(10.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(amber.opacity(0.2))
        .bg(amber.opacity(0.06))
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.readable_tone(amber_text, 0.9))
        .flex()
        .flex_row()
        .items_start()
        .gap(px(8.0))
        .child(
            div().flex_none().mt(crate::typography::ui_rems(2.0)).child(
                crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                    .size(crate::typography::ui_rems(14.0))
                    .text_color(theme.readable_tone(amber_text, 0.9)),
            ),
        )
        .child(div().min_w_0().child(message.into()))
}
