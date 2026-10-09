//! Session navigation — the horizontal tab strip is gone (wing 2026-08-10):
//! the activity sidebar IS the session list, and the titlebar names the
//! selected session (harness brand icon + title). A `+` new-session button
//! lives in the titlebar's left control cluster while an existing session is
//! selected. `UiSettings.open_tabs` is legacy — no longer read or written.

use super::*;

/// The chat one step from `selected` in the sidebar `order`, wrapping at both
/// ends. Pure.
///
/// With nothing selected — the new-session canvas — cycling enters the list at
/// the end it would have wrapped to: the first row going forward, the last
/// going back. A selection that has since left the list (archived from another
/// device mid-cycle) is treated the same way rather than dead-ending.
pub(super) fn cycle_target(
    order: &[String],
    selected: Option<&str>,
    forward: bool,
) -> Option<String> {
    if order.is_empty() {
        return None;
    }
    let at = selected.and_then(|id| order.iter().position(|c| c == id));
    let next = match (at, forward) {
        (Some(at), true) => (at + 1) % order.len(),
        (Some(at), false) => (at + order.len() - 1) % order.len(),
        (None, true) => 0,
        (None, false) => order.len() - 1,
    };
    Some(order[next].clone())
}

pub(super) fn right_pane_expand_icon(expanded: bool) -> &'static str {
    if expanded {
        icons::COLLAPSE_ARROWS
    } else {
        icons::EXPAND_ARROWS
    }
}

impl Shell {
    /// Navigation requests focus once the destination composer renders.
    pub(super) fn focus_composer(&mut self, cx: &mut Context<Self>) {
        self.composer.update(cx, |composer, cx| {
            composer.focus_pending = true;
            cx.notify();
        });
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab: step through the sidebar's Sessions list in
    /// the order it is drawn. Selection is immediate (no MRU overlay held open
    /// on the modifier) — one press, one session.
    ///
    /// Chat-scoped chrome, like the panel toggles: gpui dispatches a matched
    /// binding before any `on_key_down`, so an unscoped cycle would fire
    /// underneath Settings (yanking the user off the page mid-record, since
    /// these are the very keys the shortcuts table invites them to press) or
    /// underneath the add-space palette, stranding the overlay over a session
    /// they never picked.
    pub(super) fn cycle_session(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Chat) || self.overlay_owns_keyboard(cx) {
            return;
        }
        // The same list `render_active_rows` draws and the jump shortcuts
        // count — one function, so neither can drift from the screen.
        let order = self.sidebar_visible_order(cx);
        let selected = self.state.read(cx).selected_chat.clone();
        if let Some(target) = cycle_target(&order, selected.as_deref(), forward) {
            self.open_chat(target, cx);
        }
    }

    /// Boot landing: the most recent active chat after workspace and session
    /// watches sync. Finished histories land on the same canvas as Cmd+N;
    /// explicit navigation always wins.
    pub(super) fn boot_select_chat(&mut self, cx: &mut Context<Self>) {
        {
            let state = self.state.read(cx);
            if !state.chats_synced
                || !state.spaces_synced
                || !state.sessions_synced()
                || state.selected_chat.is_some()
                || state.auto_selected
            {
                return;
            }
        }
        let now = Utc::now();
        let mut attention_task_keys = self.activity_task_keys(cx);
        attention_task_keys.extend(self.settlement_blocking_task_keys());
        let placed = self.effective_placements(cx);
        let first = {
            let state = self.state.read(cx);
            // Only a chat the sidebar shows as its own row: a child or fork
            // folded under its parent (even one whose parent is archived) is
            // never a landing, so a workspace with no visible active task
            // opens on the new-session canvas.
            state
                .overview_chats(now)
                .into_iter()
                .find(|(status, chat)| {
                    spaces::sidebar_row_visible(chat, state, &placed)
                        && !spaces::is_settled_chat(
                            chat,
                            *status,
                            now,
                            self.settings.sidebar_auto_settle_after_days,
                            attention_task_keys
                                .contains(&(chat.device_id.clone(), chat.id.clone())),
                        )
                })
                .map(|(_, c)| c.id.clone())
        };
        if let Some(first) = first {
            self.focus_composer(cx);
            self.state
                .update(cx, |s, cx| s.select_chat(Some(first), cx));
        } else if !matches!(self.route, Route::Page(_)) {
            // A restored sidebar page (Board, Automations) stays open.
            self.open_new_session(cx);
        }
    }

    /// Open a session from the sidebar: select it, the main area follows.
    pub(crate) fn open_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.open_chat_with_activity(chat_id, false, cx);
    }

    pub(super) fn open_activity_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.open_chat_with_activity(chat_id, true, cx);
    }

    fn open_chat_with_activity(
        &mut self,
        chat_id: String,
        from_activity: bool,
        cx: &mut Context<Self>,
    ) {
        // Explicit navigation supersedes any delayed pane promotion.
        self.pending_open_main = None;
        // A child opened directly stays visible in the sidebar, nested under
        // its parent, while selected (see `spaces::folded_child_chats`). The
        // legacy `collapsed_child_chats` preference is preserved untouched.
        self.dismiss_command_palette(cx);
        self.route = Route::Chat;
        self.focus_composer(cx);
        let review_since = if from_activity {
            let state = self.state.read(cx);
            state
                .chats
                .iter()
                .find(|chat| chat.id == chat_id)
                .and_then(|chat| self.review_attention_updated_at(&chat_id, &chat.device_id))
        } else {
            None
        };
        self.state.update(cx, |state, cx| {
            if from_activity {
                state.select_chat_from_activity(chat_id, review_since, cx);
            } else {
                state.select_chat(Some(chat_id), cx);
            }
        });
        cx.notify();
    }

    /// `+` in the titlebar: open the new-session canvas. A set sidebar filter
    /// re-homes the canvas onto that project; under "All" the current pick
    /// (the last selected project, restored from composer defaults) stands.
    pub(super) fn open_new_session(&mut self, cx: &mut Context<Self>) {
        self.dismiss_command_palette(cx);
        self.route = Route::Chat;
        self.focus_composer(cx);
        let defaults = crate::settings::composer::ComposerDefaults::load(&self.data_dir);
        let target = {
            let state = self.state.read(cx);
            self.settings
                .space_filter
                .clone()
                .filter(|id| state.space_row(id).is_some())
                .or_else(|| {
                    state
                        .selected_space
                        .clone()
                        .filter(|id| state.space_row(id).is_some())
                })
                .or_else(|| defaults.project.filter(|id| state.space_row(id).is_some()))
                .or_else(|| state.spaces_sorted().first().map(|space| space.id.clone()))
        };
        self.state.update(cx, |s, cx| {
            if let Some(target) = target {
                s.select_space(Some(target), cx);
            }
            s.select_chat(None, cx);
        });
        cx.notify();
    }

    /// "Side chat of ‹main chat›" ahead of a side chat's title: opens that
    /// actual parent. A parent that no longer exists is named, not linked.
    fn render_parent_breadcrumb(
        &self,
        (parent_id, title, archived, origin): (Option<String>, SharedString, bool, &'static str),
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = theme.text_muted;
        let tooltip_title: SharedString = format!("Side chat of {title}").into();
        let crumb = div()
            .id("parent-breadcrumb")
            .max_w(crate::typography::ui_rems(240.0))
            .min_w_0()
            .flex_none()
            .h(crate::typography::ui_rems(22.0))
            .px(crate::typography::ui_rems(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(crate::typography::ui_rems(4.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(gpui::transparent_black())
            .bg(crate::theme::wash(0.06))
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.readable_muted(0.8))
            // Inside the titlebar drag region: carve out its rect.
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .tooltip(move |_, cx| {
                crate::pickers::ControlTooltip::view(tooltip_title.clone(), Some(origin.into()), cx)
            })
            .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
            .child(
                icon(icons::BOT)
                    .size(crate::typography::ui_rems(12.0))
                    .flex_none()
                    .text_color(muted),
            )
            .child(div().flex_none().child("Side chat of"))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text.opacity(0.85))
                    .child(title.clone()),
            )
            .when(archived, |el| {
                el.child(div().flex_none().child("· archived"))
            });
        match parent_id {
            Some(parent_id) => crumb
                .role(gpui::Role::Button)
                .aria_label(SharedString::from(format!("Open main chat: {title}")))
                .tab_index(0)
                .cursor_pointer()
                .hover_wash("parent-breadcrumb")
                .focus_visible({
                    let ring = theme.focus_ring();
                    move |style| crate::theme::focus_ring(style, ring)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_chat(parent_id.clone(), cx);
                }))
                .on_key_down(crate::swallow_activation_key)
                .into_any_element(),
            None => crumb
                .aria_label(SharedString::from(
                    "Side chat of a chat that is no longer available",
                ))
                .into_any_element(),
        }
    }

    /// The unified titlebar in chat mode:
    /// `[new-session +] [harness icon + session title] … [toggle-changes]`.
    /// Replaces the tab strip; inherits its titlebar duties (drag region,
    /// animated left inset, the toggle-changes button on git projects).
    pub(super) fn render_session_title_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        // The canvas titles as NOTHING (user request — a "New session"
        // header over the empty canvas was noise); the bar keeps its height,
        // drag region, and buttons. A session appends its target as a muted
        // "project @ device" tag right of the title (the composer footer no
        // longer carries it).
        // A child opened as the main chat names its actual parent (from its
        // record's lineage, never a placement): `(parent id if it still
        // exists, title, archived)`.
        let parent_crumb: Option<(Option<String>, SharedString, bool, &'static str)> = {
            let state = self.state.read(cx);
            state.selected_chat_row().and_then(|chat| {
                let origin = super::related_chats::parent_origin(chat);
                super::placement::lineage_parent_id(chat).map(|parent_id| {
                    match state.chats.iter().find(|row| row.id == parent_id) {
                        Some(parent) => (
                            Some(parent_id.to_string()),
                            transcript::single_line(
                                parent.title.as_deref().unwrap_or("Untitled chat"),
                            )
                            .into(),
                            parent.archived,
                            origin,
                        ),
                        None => (None, "an unavailable chat".into(), false, origin),
                    }
                })
            })
        };
        let (title, target, _harness, on_canvas): (
            SharedString,
            Option<SharedString>,
            Option<luna_proto::HarnessId>,
            bool,
        ) = {
            let state = self.state.read(cx);
            match state.selected_chat_row() {
                Some(chat) => {
                    let folder = chat
                        .space_id
                        .as_deref()
                        .and_then(|id| state.space_row(id))
                        .map(|s| s.display_name().to_string())
                        .unwrap_or_else(|| "Unknown project".to_string());
                    let device = state
                        .device_name(&chat.device_id)
                        .unwrap_or("Unknown device");
                    // Context only (audit SH-05): the chat title already
                    // names the tab right below, so the titlebar carries
                    // where it runs — project, then device.
                    (
                        SharedString::from(folder),
                        Some(SharedString::from(device.to_string())),
                        chat.config.as_ref().map(|c| c.harness),
                        false,
                    )
                }
                None => (SharedString::from(""), None, None, true),
            }
        };

        let sidebar_now = self.sidebar_now();

        // Same glide as the old strip: content starts at the inset card's
        // left edge while the sidebar is open, and slides toward the control
        // cluster as it collapses.
        let content_left = (sidebar_now + Theme::SPACE_LG).max(self.title_bar_content_start());

        // Trailing titlebar section. With the changes pane open this is the
        // PANE'S HEADER — a strip exactly as wide as the pane carrying its
        // controls (scope dropdown, ref selector, fold-all from the Changes
        // entity; expand + close shell-side). It lives up here because the
        // titlebar overlay owns this band's hit-testing: controls mounted in
        // the pane itself would sit under the drag region and never see a
        // click. Closed, it is just the stable open/close toggle. Hidden on
        // the new-session canvas (user request) — nothing to diff yet.
        let takeover = !on_canvas && self.right_pane_open(cx) && self.right_pane_expanded;
        // In takeover the title hides and the strip owns the whole band, so
        // the row's left inset pulls back to the sidebar seam — the title
        // inset would push the scope dropdown off the pane's own left gutter
        // (user report: misaligned dead space). With the sidebar COLLAPSED
        // the seam is the window edge, where the traffic lights + nav
        // cluster overlay lives — the strip must still clear it, but only
        // just: `title_bar_content_start` carries the identity-group margin the
        // strip doesn't want (it brings its own 8px pad), and doubling up
        // read as a hole after the `+` (user report).
        let row_left = if takeover {
            // The surface tabs must LEFT-ALIGN with the pane's own rows (the
            // diff options and stats strip carry an 8px box gutter off the
            // seam — user report: rows started at different insets). The
            // strip's width is capped to `avail`, which subtracts the row's
            // 8px child gap — pulling row_left 8 LEFT of the seam cancels
            // that, so the uncapped strip starts exactly at the seam and its
            // own 8px pad lands the first chip on the pane gutter. The
            // window-control cluster still wins while the sidebar is
            // collapsed (the chips clear it instead of underlapping).
            let cluster_end = self.title_bar_content_start() - TITLEBAR_IDENTITY_GAP - 14.0;
            (sidebar_now - 8.0).max(cluster_end)
        } else {
            content_left
        };
        let trailing: Option<gpui::AnyElement> = if on_canvas {
            None
        } else {
            let right_open = self.right_pane_open(cx);
            let mut controls = div()
                .id("right-titlebar-controls")
                .flex_none()
                .h_full()
                .flex()
                .flex_row()
                .items_center();
            if right_open {
                let right_now = self.eval_tween(self.right_tween, self.right_target(cx));
                let pr = self.titlebar_right_pad(TITLEBAR_ACTION_EDGE_INSET);
                // The row's own left padding is part of its content box: a strip
                // wider than what's left after it overflows and clips at the right
                // edge (flex_none never shrinks) — cap to the available width. The
                // row's 8px child gaps sit OUTSIDE the strip's width (one before
                // the strip in takeover, two with the title row present): without
                // budgeting them the capped strip overflows by exactly one gap and
                // the buttons slide right on expand (user report).
                let gap_budget = if takeover { 8.0 } else { 16.0 };
                let avail = self.viewport_width - row_left - pr - gap_budget;
                // The right pane's SURFACE TABS (t3 RightPanelTabs) — the diff
                // options that used to live here moved into the pane's own
                // second row; expand stays in this band (user request).
                let tabs = self.render_right_tab_strip(cx);
                // The toggle is the fixed right-edge anchor, like the left
                // sidebar control. Only the tabs + expand section reveals to
                // its left; including the toggle in this animated width
                // compressed both icons into the same clipped box at open.
                let animated_width = ((right_now - pr).min(avail) - 28.0).max(0.0);
                controls = controls.child(
                    div()
                        .w(px(animated_width))
                        .h_full()
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(crate::typography::ui_rems(4.0))
                        .overflow_hidden()
                        // 8 + the trigger's own 8px pad = the pane's 16px
                        // text gutter. The 4px right padding is the stable
                        // gap before the fixed toggle.
                        .pl(crate::typography::ui_rems(8.0))
                        .pr(crate::typography::ui_rems(4.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .overflow_hidden()
                                .child(tabs),
                        )
                        .child(header_icon_button(
                            "expand-changes",
                            right_pane_expand_icon(self.right_pane_expanded),
                            ChromeTip::new(
                                if self.right_pane_expanded {
                                    "Restore panel width"
                                } else {
                                    "Expand panel"
                                },
                                None,
                            ),
                            &theme,
                            cx.listener(|this, _, _, cx| this.toggle_right_pane_expand(cx)),
                        )),
                );
            }
            // Keep the trigger mounted at one fixed position while the pane
            // controls reveal to its left.
            Some(
                controls
                    .child(header_icon_button(
                        "toggle-changes",
                        icons::SIDEBAR_MINIMALISTIC,
                        ChromeTip::new(
                            if right_open {
                                "Hide right sidebar"
                            } else {
                                "Show right sidebar"
                            },
                            self.shortcut_badge(ShortcutId::ToggleChanges),
                        ),
                        &theme,
                        cx.listener(|this, _, _, cx| this.toggle_right_pane(cx)),
                    ))
                    .into_any_element(),
            )
        };

        let inner = div()
            .size_full()
            .flex()
            .items_center()
            .pt(px(Theme::TITLEBAR_TOP_PAD))
            .gap(crate::typography::ui_rems(8.0))
            .pl(px(row_left))
            .pr(px(self.titlebar_right_pad(TITLEBAR_ACTION_EDGE_INSET)))
            // In panel takeover the header strip spans the whole band — the
            // title would sit UNDER it (both flex_none, the row overflows and
            // paint order stacks them), so it hides for the duration.
            .when(!takeover, |el| {
                el.child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(crate::typography::ui_rems(6.0))
                        .when_some(parent_crumb, |el, crumb| {
                            el.child(self.render_parent_breadcrumb(crumb, &theme, cx))
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(crate::typography::ui_rems(12.0))
                                        .text_color(theme.decorative_muted(0.4))
                                        .child("›"),
                                )
                        })
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::typography::ui_rems(13.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(if on_canvas {
                                    theme.readable_muted(0.7)
                                } else {
                                    theme.text.opacity(0.9)
                                })
                                .child(title),
                        )
                        .when_some(target, |el, target| {
                            el.child(
                                div()
                                    .flex_none()
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .text_color(theme.decorative_muted(0.4))
                                    .child("›"),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .text_color(theme.readable_muted(0.6))
                                    .child(target),
                            )
                        }),
                )
            })
            .child(div().flex_1())
            .children(trailing);

        // The unified window titlebar: full-width on the glass shell, ABOVE
        // the inset card. No bottom border — the card's own hairline is the
        // separation; the glass gutter shows between.
        let bar = div().h(px(Theme::TITLEBAR_HEIGHT)).flex_none().child(inner);
        self.titlebar_drag_region("chat-titlebar", bar, cx)
            .into_any_element()
    }
}

#[cfg(test)]
mod cycle_tests {
    use super::*;

    fn order(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn steps_forward_and_back_through_the_list() {
        let list = order(&["a", "b", "c"]);
        assert_eq!(cycle_target(&list, Some("a"), true).as_deref(), Some("b"));
        assert_eq!(cycle_target(&list, Some("b"), true).as_deref(), Some("c"));
        assert_eq!(cycle_target(&list, Some("c"), false).as_deref(), Some("b"));
        assert_eq!(cycle_target(&list, Some("b"), false).as_deref(), Some("a"));
    }

    #[test]
    fn wraps_at_both_ends() {
        let list = order(&["a", "b", "c"]);
        assert_eq!(cycle_target(&list, Some("c"), true).as_deref(), Some("a"));
        assert_eq!(cycle_target(&list, Some("a"), false).as_deref(), Some("c"));
    }

    #[test]
    fn a_single_session_cycles_to_itself() {
        // Not a no-op by accident: with one row both directions must resolve,
        // so the shortcut never looks broken by dead-ending on `None`.
        let list = order(&["only"]);
        assert_eq!(
            cycle_target(&list, Some("only"), true).as_deref(),
            Some("only")
        );
        assert_eq!(
            cycle_target(&list, Some("only"), false).as_deref(),
            Some("only")
        );
    }

    #[test]
    fn no_selection_enters_the_list_from_the_matching_end() {
        let list = order(&["a", "b", "c"]);
        assert_eq!(cycle_target(&list, None, true).as_deref(), Some("a"));
        assert_eq!(cycle_target(&list, None, false).as_deref(), Some("c"));
        assert_eq!(
            cycle_target(&list, Some("gone"), true).as_deref(),
            Some("a")
        );
        assert_eq!(
            cycle_target(&list, Some("gone"), false).as_deref(),
            Some("c")
        );
    }

    #[test]
    fn an_empty_list_has_nothing_to_select() {
        assert_eq!(cycle_target(&[], None, true), None);
        assert_eq!(cycle_target(&[], Some("a"), true), None);
    }

    // Cycling walks the rows the sidebar is drawing, not every chat: that
    // guarantee is structural now — `cycle_session` reads the same
    // `AppState::sidebar_chats` the sidebar and the jump shortcuts read, and
    // `jump_slots_count_the_rows_the_sidebar_draws` (state.rs) covers the
    // space-filter behaviour for all of them.
}
