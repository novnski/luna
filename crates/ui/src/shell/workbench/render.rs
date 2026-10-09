//! Painting the workbench: the inset canvas, the split tree, each group's
//! tab strip and selected chat, the drag ghost and drop preview, dividers,
//! the empty-group affordances, the tab menu and the frame's bottom row.
//!
//! Geometry is measured at paint into [`Geometry`] (window coordinates), so
//! pointer hit-testing between frames uses exactly what was drawn.

use super::*;
use crate::motion::AnimationExt as _;

/// Height of a group's tab strip; the body starts below it.
/// The same height as the sidebars' surface toolbars, so the canvas tab
/// strip and the right sidebar's control row read as one aligned band.
pub(in crate::shell) const STRIP_HEIGHT: f32 = crate::surface_chrome::HEADER_HEIGHT;
/// Width of the fade at an overflowing tab strip's edges.
const TAB_STRIP_EDGE_FADE: f32 = 24.0;
const TAB_HEIGHT: f32 = 28.0;
const TAB_MIN_WIDTH: f32 = 72.0;
/// A group narrower than this shows a compact strip (slice 6): one Group
/// actions button instead of `+`, split and close, a smaller tab minimum and
/// tighter padding, so the active tab keeps a usable hit target.
const STRIP_COMPACT_WIDTH: f32 = 220.0;
const TAB_MIN_WIDTH_COMPACT: f32 = 40.0;
const TAB_MAX_WIDTH: f32 = 220.0;
/// Below this frame-row width the Git actions show as icon buttons.
const FOOTER_COMPACT_WIDTH: f32 = 360.0;
/// Below this frame-row width (points at the 16 px Interface size) the new
/// thread's machine and checkout chips show only their glyphs; the project
/// and branch keep their labels (the branch truncating if it must).
const TARGET_FOOTER_COMPACT_WIDTH: f32 = 560.0;
/// The canvas sits this far inside the frame (left/right/bottom).
const CANVAS_INSET: f32 = 8.0;
const CANVAS_RADIUS: f32 = 14.0;
/// The scroll viewport's rectangular clip sits this far inside the canvas's
/// outer edge on every side (border included). Clips are rectangles only, so
/// it must be at least `CANVAS_RADIUS * (1 - 1/sqrt 2)` (~4.1 px) for the
/// clip's corners to stay inside the rounded corners: content can then
/// never cut across the arc or paint over the hairline.
pub(in crate::shell) const VIEWPORT_INSET: f32 = 6.0;
/// The directional overflow fade at a viewport edge with more content.
const OVERFLOW_FADE: f32 = 28.0;
/// The frame's bottom row (checkout/branch, Git entry point).
const FRAME_FOOTER_HEIGHT: f32 = 32.0;
/// Divider hit target, centred on the 1px seam (one splitter recipe with
/// the sidebar seams, audit SH-18).
const DIVIDER_HIT: f32 = 10.0;
/// The hairline between a split's two sides.
const SPLIT_SEAM: f32 = 1.0;
/// Hover/preview fade and insertion timing (reduced motion: none).
const PREVIEW_FADE_MS: u64 = 120;
const INSERT_MS: u64 = 160;

/// The selected tab's own rounded fill, its only selection mark (no
/// underline, no outline): the focused group's active tab takes a quiet
/// accent tint, the other groups' active tabs the neutral selected fill.
/// Its title (full text, medium weight) and, when focused, its accent role
/// glyph go with it; keyboard focus keeps its own focus-visible ring.
pub(in crate::shell) fn tab_selected_fill(theme: &Theme, focused: bool) -> gpui::Hsla {
    if focused {
        theme.tab_focused_fill()
    } else {
        theme.tab_chip_washes()[2]
    }
}

/// Which viewport edges have content beyond them: `[left, right, top,
/// bottom]`, from the canvas scroll position of the last layout.
pub(in crate::shell) fn canvas_overflow(scroll: &gpui::ScrollHandle) -> [bool; 4] {
    let offset = scroll.offset();
    let max = scroll.max_offset();
    let (x, y) = (-f32::from(offset.x), -f32::from(offset.y));
    let (max_x, max_y) = (f32::from(max.x), f32::from(max.y));
    [x > 0.5, max_x - x > 0.5, y > 0.5, max_y - y > 0.5]
}

/// A soft fade at each viewport edge with more content beyond it: content
/// dissolves toward the window surface instead of being sliced by a hard
/// line. Translucent (no opaque patch over the frosted canvas), and drawn
/// as plain shapes with no hitbox, so presses, wheel scrolling and drops
/// pass straight through to the panes underneath.
fn overflow_fades(scroll: &gpui::ScrollHandle, theme: &Theme) -> Vec<AnyElement> {
    let [left, right, top, bottom] = canvas_overflow(scroll);
    let shade = theme.overflow_fade();
    let clear = shade.opacity(0.0);
    let fade = |angle: f32, from: gpui::Hsla, to: gpui::Hsla| {
        gpui::linear_gradient(
            angle,
            gpui::linear_color_stop(from, 0.0),
            gpui::linear_color_stop(to, 1.0),
        )
    };
    let mut fades = Vec::new();
    if left {
        fades.push(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(px(OVERFLOW_FADE))
                .bg(fade(90.0, shade, clear))
                .into_any_element(),
        );
    }
    if right {
        fades.push(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(OVERFLOW_FADE))
                .bg(fade(90.0, clear, shade))
                .into_any_element(),
        );
    }
    if top {
        fades.push(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top_0()
                .h(px(OVERFLOW_FADE))
                .bg(fade(180.0, shade, clear))
                .into_any_element(),
        );
    }
    if bottom {
        fades.push(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(OVERFLOW_FADE))
                .bg(fade(180.0, clear, shade))
                .into_any_element(),
        );
    }
    fades
}

/// A read-only frame-row label: a 12 pt glyph and the row's 11.5 pt text in
/// the readable muted ink (the new thread's target chips share these
/// metrics). `measure` records it for QA readback.
fn footer_label(
    path: &'static str,
    text: SharedString,
    muted: gpui::Hsla,
    measure: Option<Rc<RefCell<Geometry>>>,
) -> gpui::Div {
    let key = text.to_string();
    div()
        .relative()
        .min_w_0()
        .flex_shrink(1.0)
        .flex()
        .flex_row()
        .items_center()
        .gap(crate::typography::ui_rems(5.0))
        .child(
            icon(path)
                .size(crate::typography::ui_rems(12.0))
                .flex_none()
                .text_color(muted),
        )
        .child(div().min_w_0().truncate().child(text))
        .when_some(measure, |label, measure| {
            label.child(
                gpui::canvas(
                    move |bounds, _, _| {
                        measure
                            .borrow_mut()
                            .footer_labels
                            .push((key.clone(), bounds));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
        })
}

/// The frame row's read-only thread context: who (a non-Main tab), then
/// the checkout kind and branch. It shrinks and truncates first; the Git
/// actions keep their size.
fn footer_context_row(
    context: &FooterContext,
    theme: &Theme,
    measure: Option<Rc<RefCell<Geometry>>>,
) -> gpui::Div {
    let muted = theme.readable_muted(0.7);
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .when_some(context.who.clone(), |el, who| {
            el.child(
                div()
                    .min_w_0()
                    .flex_shrink(1.0)
                    .max_w(px(260.0))
                    .truncate()
                    .text_color(theme.text_muted)
                    .child(who),
            )
        })
        .when_some(context.checkout, |el, (path, text)| {
            el.child(footer_label(path, text.into(), muted, measure.clone()))
        })
        .when_some(context.branch.clone(), |el, branch| {
            el.child(footer_label(icons::GIT_BRANCH, branch, muted, measure))
        })
}

/// The face of a frame-row Git action (Review changes, Commit & push…):
/// outlined, 22 pt, its label dropped when the row is compact.
fn footer_button_face(
    glyph: &'static str,
    text: &'static str,
    compact: bool,
    theme: &Theme,
) -> gpui::Div {
    div()
        .h(crate::typography::ui_rems(22.0))
        .px(crate::typography::ui_rems(8.0))
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::typography::ui_rems(5.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.border)
        .child(
            icon(glyph)
                .size(crate::typography::ui_rems(12.0))
                .text_color(theme.readable_muted(0.7)),
        )
        .when(!compact, |button| button.child(text))
}

/// The departing copy of a working thread's row while the route has
/// already moved to a new thread: its context and inert Git action faces.
fn footer_thread_layer(context: &FooterContext, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .child(footer_context_row(context, theme, None))
        .when(context.git, |row| {
            row.child(footer_button_face(
                icons::GIT_BRANCH,
                "Review changes",
                context.compact,
                theme,
            ))
            .child(footer_button_face(
                icons::CHECK,
                "Commit & push…",
                context.compact,
                theme,
            ))
        })
}

/// The contained content plane every main route shares: below the overlaid
/// titlebar, inset from the chrome, a rounded hairline canvas with its
/// darker fill, and an inner viewport inset so its rectangular clip never
/// crosses a corner. `bottom` is the gutter under it (the chat frame keeps
/// its footer row there instead). Navigation stays outside, in the chrome.
pub(in crate::shell) fn contained_plane(
    theme: &Theme,
    id: &'static str,
    content: AnyElement,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .pt(px(Theme::TITLEBAR_HEIGHT))
        .px(px(CANVAS_INSET))
        .pb(px(CANVAS_INSET))
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .flex_col()
                .rounded(px(CANVAS_RADIUS))
                .border_1()
                .border_color(theme.hairline(0.08))
                .bg(theme.canvas_fill())
                .overflow_hidden()
                .p(px(VIEWPORT_INSET - 1.0))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(content),
                ),
        )
}

/// The width share of `pane_id` within the tree: the product of the saved
/// ratios along its horizontal splits (vertical splits keep the width).
pub(in crate::shell) fn pane_width_share(node: &model::Node, pane_id: &str) -> Option<f32> {
    match node {
        model::Node::Pane { id, .. } => (id == pane_id).then_some(1.0),
        model::Node::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => {
            let ratio = (*ratio as f32).clamp(model::RATIO_MIN, model::RATIO_MAX);
            let horizontal = *axis == model::Axis::Horizontal;
            pane_width_share(first, pane_id)
                .map(|share| if horizontal { share * ratio } else { share })
                .or_else(|| {
                    pane_width_share(second, pane_id).map(|share| {
                        if horizontal {
                            share * (1.0 - ratio)
                        } else {
                            share
                        }
                    })
                })
        }
    }
}

/// The workbench viewport's width inside a center column `column` wide:
/// the frame's side insets plus the canvas hairline and padding.
pub(in crate::shell) fn viewport_width_in(column: f32) -> f32 {
    (column - 2.0 * CANVAS_INSET - 2.0 * VIEWPORT_INSET).max(0.0)
}

/// ADVISORY since the fit-to-window design (slice 6): the size the saved
/// tree would need with every group at its minimum. Rendering no longer
/// enforces it (groups share the viewport); divider drags, split creation
/// and the `canvasMinimum` readback still use these minimums.
///
/// The smallest size the saved tree can take with every group at its
/// minimum (320 x 240), keeping each split's saved ratio and axis: a
/// horizontal split needs its first side's width at the ratio and its second
/// side's (plus the 1 px seam) at the rest, and the taller side's height; a
/// vertical split the same across. Ratios are read as the reducer bounds them.
pub(in crate::shell) fn min_tree_size(node: &model::Node) -> (f32, f32) {
    match node {
        model::Node::Pane { .. } => (MIN_PANE_WIDTH, MIN_PANE_HEIGHT),
        model::Node::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => {
            let ratio = (*ratio as f32).clamp(model::RATIO_MIN, model::RATIO_MAX);
            let (a, b) = (min_tree_size(first), min_tree_size(second));
            match axis {
                model::Axis::Horizontal => (
                    (a.0 / ratio).max((b.0 + SPLIT_SEAM) / (1.0 - ratio)).ceil(),
                    a.1.max(b.1),
                ),
                model::Axis::Vertical => (
                    a.0.max(b.0),
                    (a.1 / ratio).max((b.1 + SPLIT_SEAM) / (1.0 - ratio)).ceil(),
                ),
            }
        }
    }
}

/// The strip and body of a measured pane.
pub(in crate::shell) fn pane_regions(bounds: Bounds<Pixels>) -> (Bounds<Pixels>, Bounds<Pixels>) {
    let strip_height = px(STRIP_HEIGHT).min(bounds.size.height);
    let strip = Bounds {
        origin: bounds.origin,
        size: gpui::size(bounds.size.width, strip_height),
    };
    let body = Bounds {
        origin: gpui::point(bounds.origin.x, bounds.origin.y + strip_height),
        size: gpui::size(bounds.size.width, bounds.size.height - strip_height),
    };
    (strip, body)
}

/// A split node's fields, borrowed.
struct SplitView<'a> {
    id: &'a str,
    axis: model::Axis,
    ratio: f64,
    first: &'a model::Node,
    second: &'a model::Node,
}

/// What every node of one frame shares.
struct Frame {
    layout: Layout,
    multi: bool,
    main_content_width: f32,
    transcript_width: f32,
    theme: Theme,
}

/// The compact chip that follows the pointer while a tab drags.
pub(in crate::shell) struct WorkbenchTabGhost {
    title: SharedString,
    role: TabRole,
    harness: Option<luna_proto::HarnessId>,
    armed: Rc<Cell<bool>>,
}

impl Render for WorkbenchTabGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .opacity(if self.armed.get() { 1.0 } else { 0.0 })
            .h(crate::typography::ui_rems(26.0))
            .max_w(px(TAB_MAX_WIDTH))
            .px(crate::typography::ui_rems(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(crate::typography::ui_rems(6.0))
            .rounded(px(7.0))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border_strong)
            .shadow_md()
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text)
            .when_some(
                self.harness.map(crate::pickers::harness_brand_icon),
                |el, (path, tint)| {
                    el.child(
                        icon(path)
                            .size(crate::typography::ui_rems(13.0))
                            .flex_none()
                            .text_color(tint.unwrap_or(theme.text_muted)),
                    )
                },
            )
            .child(
                icon(self.role.icon())
                    .size(crate::typography::ui_rems(11.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(div().min_w_0().truncate().child(self.title.clone()))
            .child(
                div()
                    .flex_none()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(self.role.display()),
            )
    }
}

impl Shell {
    /// Whether the chat route shows the workbench frame (a chat selected).
    pub(in crate::shell) fn workbench_framed(&self, cx: &App) -> bool {
        matches!(self.route, Route::Chat) && self.state.read(cx).selected_chat.is_some()
    }

    /// The new-chat canvas (no chat selected) in the same inset frame as
    /// the workbench: one canvas, no tab strip, and the frame row holding
    /// the new thread's target pickers, so the canvas keeps its geometry
    /// when the first send opens the chat.
    pub(in crate::shell) fn render_canvas_frame(
        &mut self,
        window: &mut Window,
        main_content_width: f32,
        transcript_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.settle_workbench_drag(cx);
        let theme = Theme::of(cx).clone();
        let canvas_fill = theme.canvas_fill();
        let width = (main_content_width - 2.0 * CANVAS_INSET).max(0.0);
        // Renders (and ticks the composer's route clock) before the row
        // reads it.
        let body = self.render_main(window, width, transcript_width.min(width), 0.0, cx);
        let footer = self.render_new_thread_footer(&theme, cx);
        let canvas_geometry = self.workbench.geometry.clone();
        div()
            .id("workbench-frame")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .pt(px(Theme::TITLEBAR_HEIGHT))
            .px(px(CANVAS_INSET))
            .child(
                div()
                    .id("workbench-canvas")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .rounded(px(CANVAS_RADIUS))
                    .border_1()
                    .border_color(theme.hairline(0.08))
                    .bg(canvas_fill)
                    .overflow_hidden()
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| canvas_geometry.borrow_mut().canvas = Some(bounds),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .child(body),
            )
            .child(footer)
            .into_any_element()
    }

    /// The new thread's frame row. On the left, its target pickers
    /// (machine, project, checkout kind, branch), where a working thread
    /// names its checkout and branch; nothing on the right, since there is
    /// no task yet to review or commit. Across the route change the two
    /// rows crossfade on the composer's route clock: the departing thread's
    /// context fades out first, then the pickers fade in (the reverse of
    /// [`Self::render_workbench_footer`]).
    fn render_new_thread_footer(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let dock = self.composer_dock.borrow().frame;
        let (target, departing) = (dock.selectors(), dock.footer());
        let has_projects = !self.state.read(cx).spaces.is_empty();
        self.workbench.geometry.borrow_mut().footer_buttons.clear();
        self.workbench.geometry.borrow_mut().footer_labels.clear();
        let compact = self.target_footer_compact(theme);
        let pickers = self.composer.read(cx).pickers().clone();
        let targets = (has_projects && target > 0.0).then(|| {
            pickers.update(cx, |pickers, cx| {
                pickers.render_target_footer(true, compact, cx)
            })
        });
        if targets.is_none() {
            pickers.update(cx, |pickers, cx| pickers.release_target_footer(cx));
        }
        let leaving = self
            .workbench
            .footer_context
            .clone()
            .filter(|_| departing > 0.0);
        let row_geometry = self.workbench.geometry.clone();
        div()
            .id("workbench-footer")
            .relative()
            .h(px(FRAME_FOOTER_HEIGHT))
            .flex_none()
            .w_full()
            // The chips carry their own side padding: their glyphs line up
            // with the thread's labels, their hover plate with the canvas.
            .pr(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.readable_muted(0.7))
            .child(
                gpui::canvas(
                    move |bounds, _, _| {
                        row_geometry.borrow_mut().footer = Some(bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .when_some(targets, |row, targets| {
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .flex()
                        .flex_row()
                        .items_center()
                        .opacity(target)
                        .child(targets),
                )
            })
            .when_some(leaving, |row, context| {
                row.child(
                    footer_thread_layer(&context, theme)
                        .absolute()
                        .inset_0()
                        .px(px(6.0))
                        .opacity(departing),
                )
            })
            .into_any_element()
    }

    /// Whether the frame row is too narrow for every target label: the
    /// machine and checkout chips then show only their glyphs.
    fn target_footer_compact(&self, theme: &Theme) -> bool {
        self.workbench
            .geometry
            .borrow()
            .footer
            .is_some_and(|footer| {
                f32::from(footer.size.width) < TARGET_FOOTER_COMPACT_WIDTH * theme.ui_scale()
            })
    }

    /// The center column on the chat route with a selected chat: the inset
    /// canvas holding the split tree, and the frame's bottom row.
    pub(in crate::shell) fn render_workbench(
        &mut self,
        window: &mut Window,
        main_content_width: f32,
        transcript_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.settle_workbench_drag(cx);
        self.workbench_keep_keyboard_home(window, cx);
        if self.prune_workbench_exits() {
            crate::perf_counters::count("frame.request", "workbench_render#1", 1, 0);
            window.request_animation_frame();
        }
        let theme = Theme::of(cx).clone();
        let layout = self.workbench.layout();
        {
            let mut geometry = self.workbench.geometry.borrow_mut();
            geometry.retain(&layout);
        }
        let frame = Frame {
            multi: model::has_split(&layout),
            layout: layout.clone(),
            main_content_width,
            transcript_width,
            theme: theme.clone(),
        };
        // The saved layout always fits the viewport: groups share the
        // available width and height in their saved proportions (a resize
        // never scrolls the workbench or rewrites the layout).
        let canvas_scroll = self.workbench_canvas_scroll(&layout, cx);
        let tree = self.render_workbench_node(&layout.root, &frame, window, cx);
        let canvas_fill = theme.canvas_fill();
        let geometry = self.workbench.geometry.clone();
        let canvas_geometry = self.workbench.geometry.clone();
        let preview = self.render_workbench_preview(&theme);
        let collapsing = self.render_workbench_collapse(canvas_fill, &theme);
        let notice = self.render_workbench_notice(&theme, cx);
        let canvas = div()
            .id("workbench-canvas")
            .track_focus(&self.workbench.focus)
            .role(gpui::Role::Group)
            .aria_label(SharedString::from(format!(
                "Chat workbench: {} open",
                model::open_chat_ids(&layout).len()
            )))
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .rounded(px(CANVAS_RADIUS))
            .border_1()
            .border_color(theme.hairline(0.08))
            .bg(canvas_fill)
            .overflow_hidden()
            .p(px(VIEWPORT_INSET - 1.0))
            .on_drag_move(cx.listener(Self::on_workbench_tab_drag))
            .on_drag_move(cx.listener(Self::on_related_row_drag))
            .on_drag_move(cx.listener(Self::on_workbench_divider_drag))
            .on_drop(cx.listener(|this, drag: &WorkbenchTabDrag, window, cx| {
                this.drop_workbench_tab(drag, window, cx)
            }))
            .on_drop(cx.listener(|this, drag: &SidebarChatDrag, window, cx| {
                this.drop_related_row(drag, window, cx)
            }))
            .child(
                gpui::canvas(
                    move |bounds, _, _| canvas_geometry.borrow_mut().canvas = Some(bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(
                // The scroll viewport: inset inside the rounded hairline so
                // its rectangular clip never crosses a corner. Measured as
                // the canvas root, so drops, previews, reveal and tab
                // hit-testing all use the area content is actually shown in.
                div()
                    .id("workbench-viewport")
                    .relative()
                    .size_full()
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| geometry.borrow_mut().root = Some(bounds),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .child(
                        // The content is the saved layout at exactly the
                        // viewport's size (footer and sidebars are outside
                        // it). The scroll container stays only so its handle
                        // reads a zero range; inner scrolling (tab strips,
                        // transcripts, code blocks) is the panes' own.
                        div()
                            .id("workbench-canvas-scroll")
                            .size_full()
                            .overflow_scroll()
                            .track_scroll(&canvas_scroll)
                            .child(div().size_full().child(tree)),
                    )
                    .children(overflow_fades(&canvas_scroll, &theme))
                    .children(collapsing)
                    .children(preview),
            )
            .children(notice);
        let footer = self.render_workbench_footer(&layout, &theme, cx);
        // Any press anywhere starts a new pointer gesture, so an earlier
        // Escape no longer applies, and a press outside the canvas leaves it
        // (see `workbench_focus_vanished`). A window-level listener, not a
        // hitbox one: occluding targets (the split dividers) hide the root's.
        let shell = cx.entity().downgrade();
        let gesture_reset = gpui::canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let shell = shell.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        let position = event.position;
                        shell
                            .update(cx, |shell, _| shell.workbench_pressed_at(position))
                            .ok();
                    }
                });
            },
        )
        .absolute()
        .inset_0();
        div()
            .id("workbench-frame")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .pt(px(Theme::TITLEBAR_HEIGHT))
            .px(px(CANVAS_INSET))
            .child(gesture_reset)
            .child(canvas)
            .child(footer)
            .into_any_element()
    }

    fn render_workbench_node(
        &mut self,
        node: &model::Node,
        frame: &Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            model::Node::Pane { .. } => match model::as_pane(node) {
                Some(pane) => self.render_workbench_pane(pane, frame, window, cx),
                None => Empty.into_any_element(),
            },
            model::Node::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => self.render_workbench_split(
                SplitView {
                    id,
                    axis: *axis,
                    ratio: *ratio,
                    first,
                    second,
                },
                frame,
                window,
                cx,
            ),
        }
    }

    fn render_workbench_split(
        &mut self,
        split: SplitView<'_>,
        frame: &Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let horizontal = split.axis == model::Axis::Horizontal;
        let live = self
            .workbench
            .resize
            .as_ref()
            .filter(|(id, _)| id == split.id)
            .map(|(_, ratio)| *ratio);
        // The saved (or live-dragged) ratio as is: groups scale with the
        // window in their saved proportions, and nothing is written.
        let ratio = live
            .unwrap_or(split.ratio as f32)
            .clamp(model::RATIO_MIN, model::RATIO_MAX);
        let first = self.render_workbench_node(split.first, frame, window, cx);
        let second = self.render_workbench_node(split.second, frame, window, cx);
        let theme = &frame.theme;
        let geometry = self.workbench.geometry.clone();
        let measure_id = split.id.to_string();
        let release_id = split.id.to_string();
        let release_out_id = split.id.to_string();
        let press_split = split.id.to_string();
        let group: SharedString = format!("wb-divider-{}", split.id).into();
        let active = live.is_some();
        let seam = theme.hairline(0.08);
        // Hover raises a neutral hairline; only a live drag takes the accent.
        let hover_seam = theme.hairline(0.18);
        let highlight = theme.accent.opacity(0.55);
        let divider = div()
            .id(SharedString::from(format!("wb-divider-{}", split.id)))
            .group(group.clone())
            .absolute()
            .occlude()
            .role(gpui::Role::Splitter)
            .aria_label(if horizontal {
                "Resize panes: drag left or right, double-click to equalize"
            } else {
                "Resize panes: drag up or down, double-click to equalize"
            })
            .when(horizontal, |el| {
                el.top_0()
                    .bottom_0()
                    .left(gpui::relative(ratio))
                    .ml(px(-DIVIDER_HIT / 2.0))
                    .w(px(DIVIDER_HIT))
                    .cursor_col_resize()
            })
            .when(!horizontal, |el| {
                el.left_0()
                    .right_0()
                    .top(gpui::relative(ratio))
                    .mt(px(-DIVIDER_HIT / 2.0))
                    .h(px(DIVIDER_HIT))
                    .cursor_row_resize()
            })
            .child(
                div()
                    .absolute()
                    .when(horizontal, |el| {
                        el.top_0()
                            .bottom_0()
                            .left(px(DIVIDER_HIT / 2.0 - 1.0))
                            .w(px(2.0))
                    })
                    .when(!horizontal, |el| {
                        el.left_0()
                            .right_0()
                            .top(px(DIVIDER_HIT / 2.0 - 1.0))
                            .h(px(2.0))
                    })
                    .rounded_full()
                    .bg(if active {
                        highlight
                    } else {
                        highlight.opacity(0.0)
                    })
                    .group_hover(group, move |style| {
                        style.bg(if active { highlight } else { hover_seam })
                    }),
            )
            .on_drag(
                WorkbenchDividerDrag {
                    split_id: split.id.to_string(),
                },
                |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| DragGhost)
                },
            )
            // Where the press began: a release no drag move reached resolves
            // from here (a click without travel changes nothing).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, _| {
                    this.workbench_note_divider_press(press_split.clone(), event.position);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                    if event.click_count == 2 {
                        this.workbench_equalize_divider(&release_id, cx);
                    } else {
                        this.finish_workbench_divider(&release_id, event.position, cx);
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                    this.finish_workbench_divider(&release_out_id, event.position, cx);
                }),
            );
        div()
            .id(SharedString::from(format!("wb-split-{}", split.id)))
            .relative()
            .size_full()
            .flex()
            .when(horizontal, |el| el.flex_row())
            .when(!horizontal, |el| el.flex_col())
            .child(
                gpui::canvas(
                    move |bounds, _, _| {
                        geometry
                            .borrow_mut()
                            .splits
                            .insert(measure_id.clone(), bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .flex_none()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .when(horizontal, |el| el.h_full().w(gpui::relative(ratio)))
                    .when(!horizontal, |el| el.w_full().h(gpui::relative(ratio)))
                    .child(first),
            )
            .child(
                div()
                    .flex_none()
                    .bg(seam)
                    .when(horizontal, |el| el.w(px(SPLIT_SEAM)).h_full())
                    .when(!horizontal, |el| el.h(px(SPLIT_SEAM)).w_full()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(second),
            )
            .child(divider)
            .into_any_element()
    }

    fn render_workbench_pane(
        &mut self,
        pane: model::PaneRef<'_>,
        frame: &Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let main = frame.layout.main_chat_id.clone();
        let focused = frame.multi && frame.layout.focused_pane_id == pane.id;
        let strip = self.render_workbench_strip(pane, frame, focused, cx);
        let body: AnyElement = match pane.active_chat_id {
            Some(chat) if chat == main => {
                // Main: the shell's own transcript and composer at this
                // pane's CURRENT width (the last measurement only while the
                // column width it was taken at still holds).
                let width = self
                    .workbench_main_width_now(frame.main_content_width)
                    .unwrap_or(frame.main_content_width);
                let transcript_width = frame.transcript_width.min(width);
                div()
                    .id("wb-main-body")
                    .size_full()
                    .flex()
                    .child(self.render_main(window, width, transcript_width, 0.0, cx))
                    .into_any_element()
            }
            Some(chat) => match pane.tabs.iter().find(|tab| tab.chat_id == chat).cloned() {
                Some(tab) => self.render_workbench_tab_body(&tab, cx),
                None => self.render_workbench_empty(pane, frame, cx),
            },
            None => self.render_workbench_empty(pane, frame, cx),
        };
        let geometry = self.workbench.geometry.clone();
        let measure_id = pane.id.to_string();
        let holds_main = pane.active_chat_id == Some(main.as_str());
        let column_width = frame.main_content_width;
        let used_main_width = holds_main
            .then(|| self.workbench_main_width_now(column_width))
            .flatten();
        let focus_pane = pane.id.to_string();
        let pane_focused = frame.layout.focused_pane_id == pane.id;
        let pane_focus = self.workbench_pane_focus(pane.id, window, cx);
        if self.workbench.focus_pane.as_deref() == Some(pane.id) {
            self.workbench.focus_pane = None;
            window.focus(&pane_focus, cx);
        }
        let element = div()
            .id(SharedString::from(format!("wb-pane-{}", pane.id)))
            // Keyboard focus anywhere in the pane makes it the focused group.
            .track_focus(&pane_focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            // A press anywhere in an unfocused group focuses it (keeping its
            // selected tab; an empty group too); controls inside still get
            // their own press.
            .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                if !pane_focused {
                    this.workbench_pane_focused(&focus_pane, cx);
                }
            }))
            .child(
                gpui::canvas(
                    move |bounds, window, _| {
                        let mut geometry = geometry.borrow_mut();
                        geometry.panes.insert(measure_id.clone(), bounds);
                        if holds_main {
                            let measured = f32::from(bounds.size.width);
                            geometry.main_width = Some(measured);
                            geometry.main_width_for = Some(column_width);
                            // Laid out at another width than this frame used
                            // (a resize, a divider move): one more frame
                            // lays Main out at the measured width.
                            if used_main_width.is_none_or(|used| (used - measured).abs() > 0.5) {
                                crate::perf_counters::count(
                                    "frame.request",
                                    "workbench_render#main_width",
                                    1,
                                    0,
                                );
                                window.request_animation_frame();
                            }
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(strip)
            .child(
                div()
                    .id(SharedString::from(format!("wb-pane-body-{}", pane.id)))
                    .role(gpui::Role::TabPanel)
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(body),
            );
        if self.reduced_motion {
            element.into_any_element()
        } else {
            element
                .with_animation(
                    SharedString::from(format!("wb-pane-in-{}", pane.id)),
                    MotionSpec::new(INSERT_MS, motion::EASE_OUT).animation(),
                    |el, t| el.opacity(0.4 + 0.6 * t),
                )
                .into_any_element()
        }
    }

    fn render_workbench_strip(
        &mut self,
        pane: model::PaneRef<'_>,
        frame: &Frame,
        pane_focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = frame.theme.clone();
        {
            let mut geometry = self.workbench.geometry.borrow_mut();
            let tabs = geometry.tabs.entry(pane.id.to_string()).or_default();
            tabs.resize(pane.tabs.len(), (String::new(), Bounds::default()));
        }
        // This frame's group width (saved share of the current viewport).
        let compact = pane_width_share(&frame.layout.root, pane.id)
            .map(|share| viewport_width_in(frame.main_content_width) * share)
            .is_some_and(|width| width < STRIP_COMPACT_WIDTH);
        let tab_scroll = self.workbench_tab_scroll(pane, cx);
        let tab_min = if compact {
            TAB_MIN_WIDTH_COMPACT
        } else {
            TAB_MIN_WIDTH
        };
        let tab_cap = self.workbench_tab_cap(pane.id, tab_min, TAB_MAX_WIDTH, cx);
        let mut tabs: Vec<AnyElement> = Vec::with_capacity(pane.tabs.len());
        for (index, tab) in pane.tabs.iter().enumerate() {
            let active = pane.active_chat_id == Some(tab.chat_id.as_str());
            tabs.push(self.render_workbench_tab(
                pane.id,
                index,
                tab,
                active,
                pane_focused || !frame.multi,
                tab_cap,
                frame,
                cx,
            ));
        }
        // Closed tabs narrow and fade where they were.
        let leaving: Vec<LeavingTab> = self
            .workbench
            .leaving
            .iter()
            .filter(|leaving| leaving.pane_id == pane.id)
            .cloned()
            .collect();
        for leaving in leaving.into_iter().rev() {
            let t = (leaving.started.elapsed().as_secs_f32() * 1000.0 / TAB_EXIT_MS as f32)
                .clamp(0.0, 1.0);
            let eased = motion::EASE_OUT.eval(t);
            let title = {
                let state = self.state.read(cx);
                let tab = TabRef::chat(leaving.chat_id.clone());
                let (role, _) = tab_role(state, &frame.layout.main_chat_id, &tab);
                tab_title(state, &tab, role)
            };
            let ghost = div()
                .h(px(TAB_HEIGHT))
                .w(px(leaving.width * (1.0 - eased)))
                .flex_none()
                .overflow_hidden()
                .opacity(1.0 - eased)
                .rounded(px(7.0))
                .pl(px(8.0))
                .flex()
                .items_center()
                .text_size(crate::typography::ui_rems(12.5))
                .text_color(theme.text_muted)
                .child(
                    div()
                        .flex_none()
                        .w(px(leaving.width))
                        .truncate()
                        .child(title),
                )
                .into_any_element();
            let at = leaving.index.min(tabs.len());
            tabs.insert(at, ghost);
        }
        // Insertion marker for a strip drop, at the gap the tab will occupy.
        let marker = self
            .workbench
            .preview
            .as_ref()
            .and_then(|preview| match &preview.target {
                DropTarget::Strip { pane_id, index } if pane_id == pane.id => {
                    let geometry = self.workbench.geometry.borrow();
                    let strip_left = geometry.panes.get(pane.id)?.origin.x;
                    let tabs = geometry.visible_tabs(pane.id);
                    let dragged = self.workbench.dragging.as_deref();
                    let from = tabs
                        .iter()
                        .position(|(chat, _)| Some(chat.as_str()) == dragged);
                    // The index counts the order after the dragged tab leaves.
                    let visual = match from {
                        Some(from) if *index >= from => index + 1,
                        _ => *index,
                    };
                    let x = match tabs.get(visual) {
                        Some((_, bounds)) => bounds.left() - px(2.0),
                        None => tabs
                            .last()
                            .map(|(_, bounds)| bounds.right() + px(1.0))
                            .unwrap_or(strip_left + px(8.0)),
                    };
                    Some(f32::from(x - strip_left))
                }
                _ => None,
            });
        let creating = self
            .workbench
            .creating_child
            .as_ref()
            .is_some_and(|request| request.pane_id == pane.id);
        let can_close_pane = frame.multi && !pane.holds(&frame.layout.main_chat_id);
        let pane_id = pane.id.to_string();
        let split_pane = pane.id.to_string();
        let close_pane = pane.id.to_string();
        let plus_pane = pane.id.to_string();
        // `+` opens the creation chooser (source, kind, provider, explicit
        // Create); it never creates by itself.
        // The configured chord, not a static default label.
        let plus = strip_button_titled(
            SharedString::from(format!("wb-new-child-{}", pane.id)),
            "New thread or side chat…",
            self.shortcut_badge(crate::settings::ShortcutId::NewThreadChooser),
            Some("Choose a fresh New thread or a Side chat that carries this group's chat context. Nothing is created until you choose Create."),
            icons::PLUS,
            &theme,
        )
        .when(creating, |el| el.opacity(0.5).cursor_default())
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.workbench_open_chooser(
                Some(plus_pane.clone()),
                super::CreationKind::NewThread,
                cx,
            );
        }));
        let plus_geometry = self.workbench.geometry.clone();
        let plus_measure = pane.id.to_string();
        let plus = plus.child(
            gpui::canvas(
                move |bounds, _, _| {
                    plus_geometry
                        .borrow_mut()
                        .plus
                        .insert(plus_measure.clone(), bounds);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        );
        let plus: AnyElement = if creating {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(loaders::gradient_spinner(
                    "wb-new-child-spinner",
                    &theme,
                    2.0,
                    cx.entity_id(),
                    cx,
                ))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child("Creating…"),
                )
                .into_any_element()
        } else {
            plus.into_any_element()
        };
        let split = strip_button_titled(
            SharedString::from(format!("wb-split-right-{}", pane.id)),
            "New thread in a split",
            self.shortcut_badge(crate::settings::ShortcutId::NewThreadSplit),
            Some(
                "A fresh thread from this group's chat, in a new group to the right. Its context is not carried over; nothing starts until you send.",
            ),
            icons::SPLIT_COLUMNS,
            &theme,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.workbench_new_child_in_split(split_pane.clone(), cx);
        }));
        // Opt-in layout probes (slice 6): the strip's controls per group.
        let split = if crate::perf_counters::probes_enabled() {
            split.relative().child(crate::perf_counters::probe(format!(
                "strip:{}:split",
                pane.id
            )))
        } else {
            split
        };
        let close = can_close_pane.then(|| {
            strip_button(
                SharedString::from(format!("wb-close-pane-{}", pane.id)),
                "Close group",
                Some("Hides its chats; reopen them from Related chats"),
                icons::CLOSE,
                &theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.workbench_close_pane(&close_pane, cx);
            }))
            .when(crate::perf_counters::probes_enabled(), |close| {
                close.relative().child(crate::perf_counters::probe(format!(
                    "strip:{}:close",
                    pane.id
                )))
            })
        });
        // Compact: one Group actions button opens the tab menu with the
        // group's actions (`+`, split, close) first; nothing is lost.
        let more_pane = pane.id.to_string();
        let more_chat = pane.active_chat_id.map(str::to_string).unwrap_or_default();
        let more = strip_button(
            SharedString::from(format!("wb-group-more-{}", pane.id)),
            "Group actions",
            Some("New thread or side chat, New thread in a split, Close group, and this tab's actions"),
            icons::MORE_HORIZONTAL,
            &theme,
        )
        .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
            cx.stop_propagation();
            this.open_workbench_group_menu(
                more_chat.clone(),
                more_pane.clone(),
                event.position(),
                cx,
            );
        }))
        .when(crate::perf_counters::probes_enabled(), |more| {
            more.relative()
                .child(crate::perf_counters::probe(format!("strip:{}:more", pane.id)))
        });
        let more_geometry = self.workbench.geometry.clone();
        let more_measure = pane.id.to_string();
        let more = more.relative().child(
            gpui::canvas(
                move |bounds, _, _| {
                    more_geometry
                        .borrow_mut()
                        .group_actions
                        .insert(more_measure.clone(), bounds);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        );
        {
            // Only the painted control keeps readback bounds: `+` while
            // wide, Group actions while compact.
            let mut geometry = self.workbench.geometry.borrow_mut();
            if compact {
                geometry.plus.remove(pane.id);
            } else {
                geometry.group_actions.remove(pane.id);
            }
        }
        let available = self.workbench_available();
        let accent = theme.accent;
        let view_geometry = self.workbench.geometry.clone();
        let view_pane = pane_id.clone();
        div()
            .id(SharedString::from(format!("wb-strip-{}", pane_id)))
            .relative()
            .h(px(STRIP_HEIGHT))
            .flex_none()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(if compact { 2.0 } else { 4.0 }))
            .pl(px(if compact { 2.0 } else { 6.0 }))
            .pr(px(if compact { 2.0 } else { 6.0 }))
            // No rule under the strip: the tabs, the selected tab's fill
            // and the chat's own context line read as one header of the
            // conversation below (only the split seam separates chats).
            .child(
                // The visible tab viewport (measured outside the scrolled
                // content): tabs scrolled past it are clipped for hits.
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .relative()
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| {
                                view_geometry
                                    .borrow_mut()
                                    .tab_view
                                    .insert(view_pane.clone(), bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    // Overflowing tabs dissolve at the strip's edges instead
                    // of being cut mid-glyph (audit SH-03).
                    .child(
                        crate::edge_fade::edge_faded(
                            TAB_STRIP_EDGE_FADE,
                            false,
                            false,
                            div()
                                .id(SharedString::from(format!("wb-tabs-{}", pane_id)))
                                .role(gpui::Role::TabList)
                                .aria_label("Chat tabs")
                                .size_full()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(2.0))
                                .overflow_x_scroll()
                                .track_scroll(&tab_scroll)
                                .children(tabs),
                        )
                        .fade_overflow_x(&tab_scroll),
                    ),
            )
            .when(!compact, |el| {
                el.child(plus)
                    .when(available, |el| el.child(split))
                    .children(close)
            })
            .when(compact, |el| el.child(more))
            .when_some(marker, |el, x| {
                el.child(
                    div()
                        .absolute()
                        .top(px((STRIP_HEIGHT - TAB_HEIGHT) / 2.0 - 1.0))
                        .left(px(x))
                        .w(px(2.0))
                        .h(px(TAB_HEIGHT + 2.0))
                        .rounded_full()
                        .bg(accent),
                )
            })
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_workbench_tab(
        &mut self,
        pane_id: &str,
        index: usize,
        tab: &TabRef,
        active: bool,
        strip_focused: bool,
        max_width: f32,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = &frame.theme;
        let now = Utc::now();
        let (role, title, harness, status, heading, details) = {
            let state = self.state.read(cx);
            let (role, _) = tab_role(state, &frame.layout.main_chat_id, tab);
            let title = tab_title(state, tab, role);
            let chat = state.chats.iter().find(|chat| chat.id == tab.chat_id);
            let harness = chat
                .and_then(|chat| chat.config.as_ref())
                .map(|config| config.harness);
            let status = chat
                .map(|chat| state.display_status_for(chat, now))
                .unwrap_or(luna_proto::ChatIndicator::Idle);
            // The source from lineage (never tab placement), and what
            // context the tab carries (see `tab_help`).
            let (heading, details) = tab_help(state, &frame.layout.main_chat_id, tab);
            (role, title, harness, status, heading, details)
        };
        let is_main = role == TabRole::Main;
        let dragging = self.workbench.dragging.as_deref() == Some(tab.chat_id.as_str());
        let group: SharedString = format!("wb-tab-{}", tab.chat_id).into();
        let geometry = self.workbench.geometry.clone();
        let measure_pane = pane_id.to_string();
        let measure_chat = tab.chat_id.clone();
        let click_chat = tab.chat_id.clone();
        let middle_chat = tab.chat_id.clone();
        let close_chat = tab.chat_id.clone();
        let menu_chat = tab.chat_id.clone();
        let menu_pane = pane_id.to_string();
        let tooltip_title = heading.clone();
        let tooltip_details = details.clone();
        let accessible: SharedString = format!(
            "{}{}: {title}",
            role.display(),
            match status {
                luna_proto::ChatIndicator::Working => ", running",
                luna_proto::ChatIndicator::AwaitingApproval => ", needs approval",
                luna_proto::ChatIndicator::AwaitingInput => ", needs input",
                luna_proto::ChatIndicator::Errored => ", failed",
                luna_proto::ChatIndicator::Completed => ", unread",
                _ => "",
            }
        )
        .into();
        // Each status has its own shape (spinner, shield, bubble, triangle),
        // never color alone, at the non-text contrast floor.
        let status_glyph = |glyph: &'static str, tone: gpui::Hsla| {
            icon(glyph)
                .size(px(11.0))
                .flex_none()
                .text_color(theme.readable_icon(tone))
                .into_any_element()
        };
        let status_mark: Option<AnyElement> = match status {
            luna_proto::ChatIndicator::Working => Some(
                loaders::mini_glyph_spinner(
                    format!("wb-tab-working-{}", tab.chat_id),
                    2.0,
                    theme.glyph,
                    cx.entity_id(),
                    cx,
                )
                .into_any_element(),
            ),
            luna_proto::ChatIndicator::AwaitingApproval => {
                Some(status_glyph(icons::SHIELD_WARNING, theme.warning))
            }
            luna_proto::ChatIndicator::AwaitingInput => {
                Some(status_glyph(icons::CHAT_ROUND_LINE, theme.warning))
            }
            luna_proto::ChatIndicator::Errored => {
                Some(status_glyph(icons::DANGER_TRIANGLE, theme.danger))
            }
            // Finished while you were elsewhere (the sidebar's unread): a
            // small dot in the configured accent, gone once it is seen.
            luna_proto::ChatIndicator::Completed => Some(
                div()
                    .size(px(6.0))
                    .flex_none()
                    .rounded_full()
                    .bg(theme.tab_unread_dot())
                    .into_any_element(),
            ),
            luna_proto::ChatIndicator::Idle => None,
        };
        let drag = WorkbenchTabDrag {
            tab: tab.clone(),
            role,
            title: title.clone(),
            harness,
            main: frame.layout.main_chat_id.clone(),
            generation: self.state.read(cx).engine_generation(),
            armed: Rc::new(Cell::new(false)),
        };
        let press_chat = tab.chat_id.clone();
        let key_chat = tab.chat_id.clone();
        let tab_focus = self.workbench_tab_focus(&tab.chat_id, cx);
        let focus_ring = theme.focus_ring();
        let focused_active = active && strip_focused;
        let selected_fill = tab_selected_fill(theme, strip_focused);
        let main_badge = is_main.then(|| {
            div()
                .flex_none()
                .h(px(16.0))
                .px(px(5.0))
                .flex()
                .items_center()
                .gap(px(3.0))
                .rounded(px(4.0))
                .bg(theme.accent_wash)
                .text_size(crate::typography::ui_rems(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.readable_on_tab_badge(
                    theme.accent,
                    theme.accent_wash,
                    crate::theme::TEXT_CONTRAST_TARGET,
                ))
                .child(icon(icons::HOME).size(px(10.0)).flex_none().text_color(
                    theme.readable_on_tab_badge(
                        theme.accent,
                        theme.accent_wash,
                        crate::theme::NON_TEXT_CONTRAST_TARGET,
                    ),
                ))
                .child("Main")
        });
        let close_slot = (!is_main).then(|| {
            div()
                .id(SharedString::from(format!("wb-tab-close-{}", tab.chat_id)))
                .role(gpui::Role::Button)
                .aria_label(SharedString::from(format!("Close tab: {title}")))
                .size(px(crate::surface_chrome::CONTROL_S))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(crate::surface_chrome::RADIUS_S))
                .opacity(if active { 1.0 } else { 0.0 })
                .group_hover(group.clone(), |style| style.opacity(1.0))
                .hover_fill(
                    format!("wb-tab-close-{}", tab.chat_id),
                    gpui::transparent_black(),
                    crate::theme::fill(crate::theme::FillTier::Secondary),
                )
                // The tab owns a drag payload: claim the close press first.
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.workbench_close_tab(&close_chat, cx);
                }))
                .child(
                    icon(icons::CLOSE)
                        .size(px(crate::surface_chrome::ICON_S))
                        .text_color(theme.text_muted),
                )
        });
        let chip = div()
            .id(SharedString::from(format!("wb-tab-{}", tab.chat_id)))
            .debug_selector(|| format!("wb-tab-{index}"))
            .group(group)
            .role(gpui::Role::Tab)
            .aria_label(accessible)
            .aria_selected(active)
            .track_focus(&tab_focus)
            .tab_index(0)
            .relative()
            .h(px(TAB_HEIGHT))
            .min_w(px(TAB_MIN_WIDTH))
            // Never wider than the strip's viewport: the active tab's title
            // truncates and its close control stays inside the strip.
            .max_w(px(max_width))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pl(px(8.0))
            .pr(px(if is_main { 8.0 } else { 4.0 }))
            .rounded(px(8.0))
            // The shared 2 px inset focus ring (audit SH-19), no border
            // scaffolding.
            .focus_visible(move |style| crate::theme::focus_ring(style, focus_ring))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(12.5))
            // One weight for every tab (audit SH-22): activation reads
            // through the fill and text colour, so the strip never reflows.
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.tab_title(active, strip_focused))
            .when(active, |el| el.bg(selected_fill))
            .when(!active, |el| {
                el.hover_wash(format!("wb-tab-{}", tab.chat_id))
            })
            .when(dragging, |el| el.opacity(0.6))
            .tooltip(move |_, cx| {
                crate::pickers::ControlTooltip::view(
                    tooltip_title.clone(),
                    Some(tooltip_details.clone()),
                    cx,
                )
            })
            .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.workbench_focus_tab(&click_chat, cx);
            }))
            .on_key_down(crate::swallow_activation_key)
            // Arrow keys move between this group's tabs (selection follows).
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    let modifiers = &event.keystroke.modifiers;
                    if modifiers.platform || modifiers.control || modifiers.alt || modifiers.shift {
                        return;
                    }
                    if this.workbench_step_tab(&key_chat, event.keystroke.key.as_str(), window, cx)
                    {
                        cx.stop_propagation();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, _, cx| {
                    this.workbench_close_tab(&middle_chat, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.open_workbench_menu(
                        menu_chat.clone(),
                        menu_pane.clone(),
                        event.position,
                        cx,
                    );
                }),
            )
            // Where the press began: the drag arms only after the pointer
            // travels from here, and the source tab stays put until then.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, _| {
                    this.workbench_note_press(press_chat.clone(), event.position);
                }),
            )
            .when(self.workbench_available(), |el| {
                el.on_drag(drag, |payload, _offset, _window, cx| {
                    cx.stop_propagation();
                    let payload = payload.clone();
                    cx.new(|_| WorkbenchTabGhost {
                        title: payload.title,
                        role: payload.role,
                        harness: payload.harness,
                        armed: payload.armed,
                    })
                })
            })
            .when_some(
                harness.map(crate::pickers::harness_brand_icon),
                |el, (path, tint)| {
                    el.child(
                        icon(path)
                            .size(px(14.0))
                            .flex_none()
                            .text_color(tint.unwrap_or(theme.text_muted)),
                    )
                },
            )
            .children(main_badge)
            .when(!is_main, |el| {
                el.child(
                    icon(role.icon())
                        .size(px(11.0))
                        .flex_none()
                        .text_color(theme.tab_role_icon(focused_active)),
                )
            })
            .child(div().min_w_0().truncate().child(title))
            .children(status_mark)
            .children(close_slot)
            .child(
                gpui::canvas(
                    move |bounds, _, _| {
                        if let Some(slot) = geometry
                            .borrow_mut()
                            .tabs
                            .get_mut(&measure_pane)
                            .and_then(|tabs| tabs.get_mut(index))
                        {
                            *slot = (measure_chat.clone(), bounds);
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );
        if self.reduced_motion {
            chip.into_any_element()
        } else {
            chip.with_animation(
                SharedString::from(format!("wb-tab-in-{}", tab.chat_id)),
                MotionSpec::new(INSERT_MS, motion::EASE_OUT).animation(),
                |el, t| el.opacity(t),
            )
            .into_any_element()
        }
    }

    /// A non-main tab's body: its live view (forks included: a side thread
    /// is an ordinary thread with its own composer).
    fn render_workbench_tab_body(&mut self, tab: &TabRef, cx: &mut Context<Self>) -> AnyElement {
        let view = self.workbench_view(tab, cx);
        self.workbench_take_focus_request(&tab.chat_id, cx);
        div()
            .id(SharedString::from(format!("wb-view-{}", tab.chat_id)))
            .size_full()
            .child(view)
            .into_any_element()
    }

    /// An intentionally empty group: useful next steps, not a blank box.
    fn render_workbench_empty(
        &mut self,
        pane: model::PaneRef<'_>,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = &frame.theme;
        let pane_id = pane.id.to_string();
        let close_id = pane.id.to_string();
        let can_close = frame.multi;
        div()
            .id(SharedString::from(format!("wb-empty-{}", pane.id)))
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(crate::typography::ui_rems(12.0))
            .text_color(theme.text_muted)
            .child(
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .child("Drag a tab or a Related chats row here, or start one."),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(crate::typography::ui_rems(8.0))
                    .child(
                        popover::btn_ghost(theme, "Open related chat", "wb-btn-open-related-chat")
                            .id(SharedString::from(format!("wb-empty-related-{}", pane.id)))
                            .role(gpui::Role::Button)
                            .aria_label("Open Related chats")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.add_work_surface(RightSurface::RelatedChats, cx);
                            })),
                    )
                    .child(
                        popover::btn_ghost(theme, "New thread", "wb-btn-new-child-chat")
                            .id(SharedString::from(format!(
                                "wb-empty-new-child-{}",
                                pane.id
                            )))
                            .role(gpui::Role::Button)
                            .aria_label("New thread")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.workbench_new_child(Some(pane_id.clone()), cx);
                            })),
                    )
                    .when(can_close, |el| {
                        el.child(
                            popover::btn_ghost(theme, "Close group", "wb-btn-close-group")
                                .id(SharedString::from(format!("wb-empty-close-{}", pane.id)))
                                .role(gpui::Role::Button)
                                .aria_label("Close this empty group")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.workbench_close_pane(&close_id, cx);
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    /// A collapsed branch's ghost, shrinking toward its outer edge.
    fn render_workbench_collapse(&self, fill: gpui::Hsla, theme: &Theme) -> Vec<AnyElement> {
        let Some(root) = self.workbench.geometry.borrow().root else {
            return Vec::new();
        };
        self.workbench
            .collapse
            .iter()
            .map(|exit| {
                let t = (exit.started.elapsed().as_secs_f32() * 1000.0 / COLLAPSE_MS as f32)
                    .clamp(0.0, 1.0);
                let keep = 1.0 - motion::EASE_OUT.eval(t);
                let bounds = exit.bounds;
                let (x, y) = (
                    bounds.origin.x - root.origin.x,
                    bounds.origin.y - root.origin.y,
                );
                let (w, h) = (bounds.size.width, bounds.size.height);
                let (left, top, width, height) = match exit.toward {
                    Edge::Left => (x, y, w * keep, h),
                    Edge::Right => (x + w * (1.0 - keep), y, w * keep, h),
                    Edge::Top => (x, y, w, h * keep),
                    Edge::Bottom => (x, y + h * (1.0 - keep), w, h * keep),
                };
                div()
                    .absolute()
                    .left(left)
                    .top(top)
                    .w(width)
                    .h(height)
                    .opacity(keep)
                    .bg(fill)
                    .border_1()
                    .border_color(theme.hairline(0.06))
                    .into_any_element()
            })
            .collect()
    }

    /// The exact destination rectangle, tinted and outlined, with a label.
    fn render_workbench_preview(&self, theme: &Theme) -> Option<AnyElement> {
        let preview = self.workbench.preview.as_ref()?;
        let geometry = self.workbench.geometry.borrow();
        let root = geometry.root?;
        let rect: Bounds<Pixels> = match &preview.target {
            // The insertion marker in the strip is the preview.
            DropTarget::Strip { .. } => return None,
            DropTarget::Join { pane_id } => pane_regions(*geometry.panes.get(pane_id.as_str())?).1,
            DropTarget::Split {
                node_id,
                edge,
                outer,
            } => {
                let base = if *outer {
                    root
                } else {
                    pane_regions(*geometry.panes.get(node_id.as_str())?).1
                };
                half(base, *edge)
            }
        };
        let accent = theme.accent;
        let label = preview.label.clone();
        let overlay = div()
            .id("wb-drop-preview")
            .absolute()
            .left(rect.origin.x - root.origin.x)
            .top(rect.origin.y - root.origin.y)
            .w(rect.size.width)
            .h(rect.size.height)
            .p(crate::typography::ui_rems(4.0))
            .child(
                div()
                    .size_full()
                    .rounded(px(10.0))
                    .bg(accent.opacity(0.10))
                    .border_2()
                    .border_color(theme.readable_icon(accent.opacity(0.7)))
                    .flex()
                    .items_start()
                    .justify_center()
                    .pt(crate::typography::ui_rems(12.0))
                    .child(
                        div()
                            .px(crate::typography::ui_rems(8.0))
                            .py(crate::typography::ui_rems(3.0))
                            .rounded(px(6.0))
                            .bg(theme.surface_raised)
                            .border_1()
                            .border_color(accent.opacity(0.6))
                            .text_size(crate::typography::ui_rems(11.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(label),
                    ),
            );
        Some(if self.reduced_motion {
            overlay.into_any_element()
        } else {
            overlay
                .with_animation(
                    SharedString::from(format!("wb-preview-{:?}", preview.target)),
                    MotionSpec::new(PREVIEW_FADE_MS, motion::EASE).animation(),
                    |el, t| el.opacity(t),
                )
                .into_any_element()
        })
    }

    /// A child creation failure (with Retry) or a layout notice, inline at
    /// the canvas bottom-left. The main workspace stays usable.
    fn render_workbench_notice(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(failure) = self.workbench.child_failure.clone() {
            let unresolved = self.workbench_child_unresolved();
            let (retry_label, retry_aria) = if unresolved {
                ("Check status", "Check whether the new thread was created")
            } else {
                ("Retry", "Retry creating the new thread")
            };
            let blocked = (unresolved && self.workbench.child_blocked).then(|| {
                div()
                    .id("wb-child-blocked")
                    .text_color(theme.text_muted)
                    .child("A new thread can't start until this one's status is checked.")
            });
            return Some(
                div()
                    .id("wb-child-failure")
                    .absolute()
                    .left(px(12.0))
                    .bottom(px(12.0))
                    .max_w(crate::typography::ui_rems(420.0))
                    .px(crate::typography::ui_rems(10.0))
                    .py(crate::typography::ui_rems(8.0))
                    .rounded(px(8.0))
                    .bg(theme.surface_raised)
                    .border_1()
                    .border_color(theme.danger.opacity(0.5))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(crate::typography::ui_rems(8.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(failure.message.clone())
                            .children(blocked),
                    )
                    .child(
                        popover::btn_ghost(theme, retry_label, "wb-btn-retry")
                            .id("wb-child-retry")
                            .role(gpui::Role::Button)
                            .aria_label(retry_aria)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.workbench_retry_child(cx);
                            })),
                    )
                    .when(!unresolved, |el| {
                        el.child(
                            div()
                                .id("wb-child-failure-dismiss")
                                .role(gpui::Role::Button)
                                .aria_label("Dismiss")
                                // A 24 pt hit box that keeps its 18 pt
                                // footprint (audit DS-19).
                                .size(crate::typography::ui_rems(24.0))
                                .m(px(-3.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(crate::theme::radius::SM))
                                .cursor_pointer()
                                .hover_wash("wb-child-failure-dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.workbench_dismiss_child_failure(cx)
                                }))
                                .child(
                                    icon(icons::CLOSE)
                                        .size(crate::typography::ui_rems(11.0))
                                        .text_color(theme.text_muted),
                                ),
                        )
                    })
                    .into_any_element(),
            );
        }
        if let Some(reason) = self
            .workbench
            .unavailable
            .clone()
            .or_else(|| self.workbench.load_error.clone())
        {
            return Some(
                div()
                    .id("wb-load-notice")
                    .absolute()
                    .left(px(12.0))
                    .bottom(px(12.0))
                    .max_w(crate::typography::ui_rems(460.0))
                    .px(crate::typography::ui_rems(10.0))
                    .py(crate::typography::ui_rems(8.0))
                    .rounded(px(8.0))
                    .bg(theme.surface_raised)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(crate::typography::ui_rems(8.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(format!("Main stays open here. Tabs and splits: {reason}")),
                    )
                    .child(
                        popover::btn_ghost(theme, "Retry", "wb-btn-load-retry")
                            .id("wb-load-retry")
                            .aria_label("Retry loading the chat layout")
                            .on_click(cx.listener(|this, _, _, cx| this.retry_workbench_load(cx))),
                    )
                    .into_any_element(),
            );
        }
        let notice = self.workbench.notice.clone()?;
        Some(
            div()
                .id("wb-notice")
                .absolute()
                .left(px(12.0))
                .bottom(px(12.0))
                .max_w(crate::typography::ui_rems(420.0))
                .px(crate::typography::ui_rems(10.0))
                .py(crate::typography::ui_rems(6.0))
                .rounded(px(8.0))
                .bg(theme.surface_raised)
                .border_1()
                .border_color(theme.border)
                .flex()
                .flex_row()
                .items_center()
                .gap(crate::typography::ui_rems(8.0))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child(div().flex_1().min_w_0().child(notice))
                .child(
                    div()
                        .id("wb-notice-dismiss")
                        .role(gpui::Role::Button)
                        .aria_label("Dismiss")
                        .size(crate::typography::ui_rems(18.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.workbench.notice = None;
                            cx.notify();
                        }))
                        .child({
                            let geometry = self.workbench.geometry.clone();
                            gpui::canvas(
                                move |bounds, _, _| {
                                    geometry.borrow_mut().notice_dismiss = Some(bounds);
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .inset_0()
                        })
                        .child(
                            icon(icons::CLOSE)
                                .size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The frame's bottom row: the focused chat's checkout and branch on the
    /// left; on the right the existing Git entry point for that same
    /// checkout (it never acts on a chat that is not the one named here).
    /// Arriving from a new thread, its target pickers (inert now: the target
    /// is fixed) fade out first on the composer's route clock, then this
    /// row fades in, so the two never show at once.
    fn render_workbench_footer(
        &mut self,
        layout: &Layout,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = layout
            .focused_chat_id()
            .unwrap_or(layout.main_chat_id.as_str())
            .to_string();
        let (who, checkout, branch, has_checkout, change_request) = {
            let state = self.state.read(cx);
            let chat = state.chats.iter().find(|chat| chat.id == focused);
            let space = chat.and_then(|chat| state.space_for_chat(chat));
            let worktree = chat
                .and_then(|chat| chat.cwd.as_deref())
                .zip(space.map(|space| space.path.as_str()))
                .is_some_and(|(cwd, root)| cwd != root);
            let checkout = space.filter(|space| space.git_detected).map(|_| {
                if worktree {
                    (icons::FOLDER_WITH_FILES, "Worktree")
                } else {
                    (icons::FOLDER, "Local checkout")
                }
            });
            let branch = chat
                .and_then(|chat| crate::change_requests::conversation_branch(chat, &state.spaces))
                .map(str::trim)
                .filter(|branch| !branch.is_empty())
                .map(SharedString::from);
            let has_checkout = chat.is_some_and(|chat| chat.cwd.is_some());
            let (role, _) = tab_role(state, &layout.main_chat_id, &TabRef::chat(focused.clone()));
            let who: Option<SharedString> = (role != TabRole::Main).then(|| {
                let title = tab_title(state, &TabRef::chat(focused.clone()), role);
                if title.as_ref() == role.display()
                    || title
                        .as_ref()
                        .starts_with(&format!("{} · ", role.display()))
                {
                    title
                } else {
                    format!("{} · {}", role.display(), title).into()
                }
            });
            (
                who,
                checkout,
                branch,
                has_checkout,
                chat.and_then(|chat| state.change_request_for_chat(chat).cloned()),
            )
        };
        let muted = theme.readable_muted(0.7);
        // Too narrow for labelled actions (both sidebars open on a small
        // window): icon buttons, still named by their tooltip and label.
        let footer_width = self
            .workbench
            .geometry
            .borrow()
            .footer
            .map(|footer| f32::from(footer.size.width));
        let compact = footer_width.is_some_and(|width| width < FOOTER_COMPACT_WIDTH);
        let context = FooterContext {
            who: who.clone(),
            checkout,
            branch,
            git: has_checkout,
            compact,
        };
        self.workbench.footer_context = Some(context.clone());
        // The route clock ticks only while a pane renders Main's own
        // composer (`render_main`). With Main out of view (another tab active
        // in every group) it holds whatever frame it last had, even the new
        // thread the window opened on, so the row then shows the thread.
        let main_shown = model::panes(layout)
            .iter()
            .any(|pane| pane.active_chat_id == Some(layout.main_chat_id.as_str()));
        let dock = self.composer_dock.borrow().frame;
        let (target, thread) = if main_shown && dock.docked {
            (dock.selectors(), dock.footer())
        } else {
            (0.0, 1.0)
        };
        let thread_shown = thread > 0.0;
        // Both act on the FOCUSED chat's own checkout through the existing
        // Changes/Git workflow, bound to that chat (never the selection);
        // their labels name the chat when it is not Main.
        let target_name: SharedString = who.unwrap_or_else(|| SharedString::from("Main"));
        let footer_geometry = self.workbench.geometry.clone();
        let footer_button = |id: &'static str,
                             glyph: &'static str,
                             text: &'static str,
                             label: SharedString| {
            let measure = footer_geometry.clone();
            footer_button_face(glyph, text, compact, theme)
                .id(id)
                .role(gpui::Role::Button)
                .aria_label(label.clone())
                .tab_index(0)
                .cursor_pointer()
                .hover_wash(id)
                .on_key_down(crate::swallow_activation_key)
                .tooltip(move |_, cx| crate::pickers::ControlTooltip::view(label.clone(), None, cx))
                .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
                .relative()
                .child(
                    gpui::canvas(
                        move |bounds, _, _| {
                            measure
                                .borrow_mut()
                                .footer_buttons
                                .insert(id.to_string(), bounds);
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
        };
        let review_chat = focused.clone();
        let commit_chat = focused.clone();
        let git: Option<[AnyElement; 2]> = (has_checkout && thread_shown).then(|| {
            [
                footer_button(
                    "wb-footer-review",
                    icons::GIT_BRANCH,
                    "Review changes",
                    format!("Review changes in {target_name}'s checkout").into(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.add_diff_surface_for(review_chat.clone(), cx);
                }))
                .into_any_element(),
                footer_button(
                    "wb-footer-commit",
                    icons::CHECK,
                    "Commit & push…",
                    format!("Commit and push from {target_name}'s checkout (preview first)").into(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(changes) = this.add_diff_surface_for(commit_chat.clone(), cx) {
                        changes.update(cx, |changes, cx| changes.open_git_actions(cx));
                    }
                }))
                .into_any_element(),
            ]
        });
        // Balance panes: global to the layout, so it leads the row; only
        // with a split (a single group has nothing to balance).
        let balance = (thread_shown && model::has_split(layout)).then(|| {
            let measure = self.workbench.geometry.clone();
            div()
                .id("wb-balance-panes")
                .role(gpui::Role::Button)
                .aria_label(
                    "Balance panes: make every group the same size, keeping the arrangement",
                )
                .tab_index(0)
                .relative()
                .size(px(22.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .border_1()
                .border_color(gpui::transparent_black())
                .cursor_pointer()
                .hover_wash("wb-balance-panes")
                .focus_visible({
                    let ring = theme.focus_ring();
                    move |style| crate::theme::focus_ring(style, ring)
                })
                .on_key_down(crate::swallow_activation_key)
                .tooltip(|_, cx| {
                    crate::pickers::ControlTooltip::view(
                        "Balance panes",
                        Some("Every group the same size; the arrangement stays.".into()),
                        cx,
                    )
                })
                .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
                .on_click(cx.listener(|this, _, _, cx| this.workbench_balance_panes(cx)))
                .child(icon(icons::WIDGET).size(px(13.0)).text_color(muted))
                .child(
                    gpui::canvas(
                        move |bounds, _, _| measure.borrow_mut().balance = Some(bounds),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
        });
        {
            let mut geometry = self.workbench.geometry.borrow_mut();
            if balance.is_none() {
                geometry.balance = None;
            }
            if git.is_none() {
                geometry.footer_buttons.clear();
            }
            geometry.footer_labels.clear();
        }
        // The departing new thread's target, inert.
        let compact_targets = self.target_footer_compact(theme);
        let pickers = self.composer.read(cx).pickers().clone();
        let targets = (target > 0.0).then(|| {
            pickers.update(cx, |pickers, cx| {
                pickers.render_target_footer(false, compact_targets, cx)
            })
        });
        if targets.is_none() {
            pickers.update(cx, |pickers, cx| pickers.release_target_footer(cx));
        }
        let row_geometry = self.workbench.geometry.clone();
        let label_geometry = self.workbench.geometry.clone();
        div()
            .id("workbench-footer")
            .relative()
            .h(px(FRAME_FOOTER_HEIGHT))
            .flex_none()
            .w_full()
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(muted)
            .child(
                gpui::canvas(
                    move |bounds, _, _| {
                        row_geometry.borrow_mut().footer = Some(bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .when(thread_shown, |row| {
                row.child(
                    div()
                        .size_full()
                        .px(px(6.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(12.0))
                        .opacity(thread)
                        .children(balance)
                        .child(footer_context_row(&context, theme, Some(label_geometry)))
                        .when_some(change_request, |el, summary| {
                            el.child(crate::change_requests::pull_request_badge(
                                "workbench-pull-request".into(),
                                summary,
                                crate::change_requests::ChangeRequestBadgeSurface::Composer,
                                theme,
                            ))
                        })
                        .children(git.into_iter().flatten()),
                )
            })
            .when_some(targets, |row, targets| {
                row.child(
                    div()
                        .absolute()
                        .inset_0()
                        .pr(px(6.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .opacity(target)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .flex()
                                .flex_row()
                                .items_center()
                                .child(targets),
                        ),
                )
            })
            .into_any_element()
    }

    /// The tab context menu: explicit alternatives to every drag gesture.
    pub(in crate::shell) fn render_workbench_menu(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // Rows re-record their bounds as this render paints them.
        self.workbench.geometry.borrow_mut().menu_rows.clear();
        let menu = self.workbench.menu.get()?.clone();
        let theme = Theme::of(cx).clone();
        let layout = self.workbench.layout();
        let is_main = menu.chat_id == layout.main_chat_id;
        let (role, parent) = tab_role(
            self.state.read(cx),
            &layout.main_chat_id,
            &model::tab(&layout, &menu.chat_id)
                .cloned()
                .unwrap_or_else(|| TabRef::chat(menu.chat_id.clone())),
        );
        // A child's parent or a fork's source, while it still exists: the
        // tab has no context row, so its menu leads there.
        let parent = matches!(role, TabRole::Child | TabRole::Side)
            .then_some(parent)
            .filter(|parent| {
                !parent.is_empty()
                    && self
                        .state
                        .read(cx)
                        .chats
                        .iter()
                        .any(|chat| chat.id == *parent)
            });
        let others = model::pane(&layout, &menu.pane_id).is_some_and(|pane| pane.tabs.len() > 1);
        let reopen = model::newest_reopenable(&layout).is_some();
        let closing = self.workbench.menu.closing_since();
        let row = |id: &'static str, glyph: &'static str, text: &'static str| {
            popover::menu_row(&theme, false, format!("wb-menu-{id}"))
                .id(id)
                .role(gpui::Role::MenuItem)
                .aria_label(text)
                .child(
                    icon(glyph)
                        .size(crate::typography::ui_rems(16.0))
                        .text_color(theme.text_muted),
                )
                .child(SharedString::from(text))
                // Opt-in probe (slice 6): which rows the menu painted.
                .when(crate::perf_counters::probes_enabled(), |row| {
                    row.relative()
                        .child(crate::perf_counters::probe(format!("wb-menu:{id}")))
                })
                // QA readback (`menuRows`): each painted row's bounds.
                .relative()
                .child({
                    let geometry = self.workbench.geometry.clone();
                    gpui::canvas(
                        move |bounds, _, _| geometry.borrow_mut().menu_rows.push((id, bounds)),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0()
                })
        };
        let mut card = popover::popover_card(&theme)
            .w(crate::typography::ui_rems(232.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_workbench_menu(cx)))
            .flex()
            .flex_col();
        // A compact strip's Group actions: the strip's own `+`, split and
        // close lead the menu (same actions, same availability).
        if menu.group {
            if model::pane(&layout, &menu.pane_id).is_some() {
                let pane = menu.pane_id.clone();
                card = card.child(
                    row("wb-menu-new-child", icons::PLUS, "New thread or side chat…").on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.close_workbench_menu(cx);
                            this.workbench_open_chooser(
                                Some(pane.clone()),
                                super::CreationKind::NewThread,
                                cx,
                            );
                        }),
                    ),
                );
                if self.workbench_available() {
                    let pane = menu.pane_id.clone();
                    card = card.child(
                        row(
                            "wb-menu-new-split",
                            icons::SPLIT_COLUMNS,
                            "New thread in a split",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_workbench_menu(cx);
                            this.workbench_new_child_in_split(pane.clone(), cx);
                        })),
                    );
                }
                let closable = model::has_split(&layout)
                    && model::pane(&layout, &menu.pane_id)
                        .is_some_and(|pane| !pane.holds(&layout.main_chat_id));
                if closable {
                    let pane = menu.pane_id.clone();
                    card = card.child(
                        row("wb-menu-close-group", icons::CLOSE, "Close group").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.close_workbench_menu(cx);
                                this.workbench_close_pane(&pane, cx);
                            }),
                        ),
                    );
                }
            }
            if !menu.chat_id.is_empty() {
                card = card.child(popover::menu_separator());
            }
        }
        // An empty group's Group actions has no tab to act on.
        let tab = !menu.chat_id.is_empty();
        if tab && !is_main {
            let chat = menu.chat_id.clone();
            card = card.child(row("wb-menu-close", icons::CLOSE, "Close tab").on_click(
                cx.listener(move |this, _, _, cx| {
                    this.close_workbench_menu(cx);
                    this.workbench_close_tab(&chat, cx);
                }),
            ));
        }
        if others {
            let pane = menu.pane_id.clone();
            let keep = menu.chat_id.clone();
            card = card.child(
                row(
                    "wb-menu-close-others",
                    icons::CLOSE_CIRCLE,
                    "Close other tabs (keeps Main)",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_workbench_menu(cx);
                    this.workbench_close_others(&pane, &keep, cx);
                })),
            );
        }
        if tab {
            card = card.child(popover::menu_separator());
        }
        for (id, edge, text) in [
            ("wb-menu-split-right", Edge::Right, "Move to split right"),
            ("wb-menu-split-left", Edge::Left, "Move to split left"),
            ("wb-menu-split-down", Edge::Bottom, "Move to split below"),
            ("wb-menu-split-up", Edge::Top, "Move to split above"),
        ]
        .into_iter()
        .filter(|_| tab)
        {
            let chat = menu.chat_id.clone();
            card = card.child(row(id, icons::SPLIT_COLUMNS, text).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.close_workbench_menu(cx);
                    this.workbench_move_to_split(&chat, edge, cx);
                },
            )));
        }
        if reopen {
            card = card.child(popover::menu_separator()).child(
                row("wb-menu-reopen", icons::RESTART, "Reopen closed tab").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.close_workbench_menu(cx);
                        this.workbench_reopen(None, cx);
                    },
                )),
            );
        }
        if let Some(parent) = parent.filter(|_| tab) {
            let text = if role == TabRole::Side {
                "Go to source chat"
            } else {
                "Go to parent chat"
            };
            card = card.child(popover::menu_separator()).child(
                row("wb-menu-show-parent", role.icon(), text).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.close_workbench_menu(cx);
                        this.workbench_show_parent(parent.clone(), cx);
                    },
                )),
            );
        }
        // A fork (by lineage) can send its findings back to its source.
        if tab && self.findings_destination(&menu.chat_id, cx).is_some() {
            let fork = menu.chat_id.clone();
            card = card.child(
                row(
                    "wb-menu-send-findings",
                    icons::QUEUE_SEND,
                    "Send findings to source…",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_workbench_menu(cx);
                    this.open_findings_dialog(fork.clone(), cx);
                })),
            );
        }
        // Any thread but Main (a fork too) can become its own workbench Main.
        if tab && role != TabRole::Main {
            let chat = menu.chat_id.clone();
            card = card.child(popover::menu_separator()).child(
                row(
                    "wb-menu-open-main",
                    icons::ARROW_UP_RIGHT,
                    "Open as its own workspace",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_workbench_menu(cx);
                    this.open_pane_chat_as_main(chat.clone(), cx);
                })),
            );
        }
        Some(popover::menu_at(
            "workbench-tab-menu",
            menu.position,
            card.into_any_element(),
            closing,
        ))
    }
}

/// A small icon button for the tab strip's trailing controls.
fn strip_button(
    id: SharedString,
    label: &'static str,
    detail: Option<&'static str>,
    glyph: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    strip_button_titled(id, label, None, detail, glyph, theme)
}

/// [`strip_button`] whose tooltip also names its configured chord.
fn strip_button_titled(
    id: SharedString,
    label: &'static str,
    chord: Option<String>,
    detail: Option<&'static str>,
    glyph: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let shortcut: Option<SharedString> = chord.map(Into::into);
    div()
        .id(id.clone())
        .role(gpui::Role::Button)
        .aria_label(label)
        .tab_index(0)
        .relative()
        // Above any tab scrolled beneath the strip's trailing controls.
        .occlude()
        .size(px(crate::surface_chrome::CONTROL_M))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(crate::surface_chrome::RADIUS_M))
        .cursor_pointer()
        .hover_wash(id)
        .when_some(shortcut.clone(), |el, keys| el.aria_keyshortcuts(keys))
        .tooltip(move |_, cx| {
            crate::pickers::ControlTooltip::with_shortcut(
                label,
                shortcut.clone(),
                detail.map(SharedString::from),
                cx,
            )
        })
        .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
        .on_key_down(crate::swallow_activation_key)
        .child(
            icon(glyph)
                .size(px(crate::surface_chrome::ICON_M))
                .text_color(theme.text_muted),
        )
}

/// The half of `bounds` a split on `edge` gives the new pane.
fn half(bounds: Bounds<Pixels>, edge: Edge) -> Bounds<Pixels> {
    let width = bounds.size.width;
    let height = bounds.size.height;
    match edge {
        Edge::Left => Bounds {
            origin: bounds.origin,
            size: gpui::size(width / 2.0, height),
        },
        Edge::Right => Bounds {
            origin: gpui::point(bounds.origin.x + width / 2.0, bounds.origin.y),
            size: gpui::size(width / 2.0, height),
        },
        Edge::Top => Bounds {
            origin: bounds.origin,
            size: gpui::size(width, height / 2.0),
        },
        Edge::Bottom => Bounds {
            origin: gpui::point(bounds.origin.x, bounds.origin.y + height / 2.0),
            size: gpui::size(width, height / 2.0),
        },
    }
}
