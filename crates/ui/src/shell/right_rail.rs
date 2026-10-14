//! The right sidebar's rail and its places
//! (docs/design/right-sidebar-redesign-2026-10-11).
//!
//! Every place has a fixed spot on a thin rail along the sidebar's right
//! edge: the chat's own places (Overview, Agents, Related, Automations)
//! above the line, workspace tools (Files, Changes, Terminal, Browser) below
//! it. The rail is part of the sidebar and slides in and out with it, so a
//! closed sidebar shows nothing. Clicking a place shows it; clicking the
//! active one closes the sidebar. Badges mark running agents, related chats
//! that need you and open terminals. A workspace place lists its open
//! instances in its own tab row. Nothing navigates back.

use super::*;
use crate::button::{ButtonKind, ControlSize};

/// The rail's width, inside the sidebar's own width.
pub const RAIL_WIDTH: f32 = 44.0;

/// What the right-sidebar QA fixture reads back: painted bounds by label and
/// the strings each surface rendered (off until the fixture opts in).
#[cfg(feature = "appshots-fixture")]
#[derive(Default)]
pub(super) struct RightProbe {
    pub bounds: std::rc::Rc<
        std::cell::RefCell<std::collections::HashMap<String, gpui::Bounds<gpui::Pixels>>>,
    >,
    pub marks: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, Vec<String>>>>,
}

/// A rail badge: a count on a tone, or a bare dot.
#[derive(Clone, Copy, PartialEq)]
enum RailBadge {
    Count(usize, RailTone),
    Dot,
}

#[derive(Clone, Copy, PartialEq)]
enum RailTone {
    /// Work in progress (running agents).
    Accent,
    /// Something waits for the user.
    Warning,
}

impl Shell {
    /// A rail click: open the sidebar on `place`, or close it when `place`
    /// is already showing. A workspace place with nothing open opens its
    /// first instance (Files browses, Changes diffs the working tree when
    /// there is Git, Terminal restores or starts a shell, Browser opens a
    /// page).
    pub(super) fn toggle_right_place(
        &mut self,
        place: RightPlace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_chat.is_empty() {
            return;
        }
        if self.right_pane_open(cx) && self.right_place(cx) == place {
            self.toggle_right_pane(cx);
            return;
        }
        self.show_right_place(place, cx);
        if self.resolved_right_active(cx) != RightSurface::Home(place) {
            return;
        }
        match place {
            RightPlace::Files => self.add_files_surface(window, cx),
            RightPlace::Changes if self.space_git_detected(cx) => self.add_diff_surface(cx),
            RightPlace::Terminal => self.add_terminal_surface(cx),
            RightPlace::Browser => self.add_browser_surface(None, window, cx),
            _ => {}
        }
    }

    /// Open the sidebar on `place` (never closing it): what the place last
    /// showed, else its first open tab, else its own page. Every former
    /// entry point to the Agents, Related or Automations tabs lands here.
    pub(super) fn show_right_place(&mut self, place: RightPlace, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        if !self.right_pane_open(cx) {
            self.toggle_right_pane(cx);
        }
        let key = self.panel_key(cx);
        let rows = self.right_surface_rows(cx);
        let open = |surface: &RightSurface| rows.iter().any(|(row, _, _, _)| row == surface);
        let target = self
            .right_place_memory
            .get(&key)
            .and_then(|memory| memory.get(&place))
            .copied()
            .filter(open)
            .or_else(|| {
                // The Agents place opens on its list, not on a transcript.
                (place.is_workspace())
                    .then(|| {
                        rows.iter()
                            .map(|(surface, _, _, _)| *surface)
                            .find(|surface| RightPlace::of(*surface) == place)
                    })
                    .flatten()
            })
            .unwrap_or(RightSurface::Home(place));
        self.set_right_active(target, cx);
    }

    /// The rail: one button per place, a line between the chat's places
    /// and the workspace tools, each with its badge.
    pub(super) fn render_right_rail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let open = self.right_pane_open(cx);
        let current = self.right_place(cx);
        let badges = self.rail_badges(cx);
        let mut column = div()
            .id("right-rail")
            .role(gpui::Role::TabList)
            .aria_label("Right sidebar")
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(2.0))
            .pt(px(Theme::TITLEBAR_HEIGHT + 6.0))
            .pb(px(10.0));
        // CORE-12: the enabled extensions' task panels follow the workspace
        // tools, after their own line; the host's places never move.
        let contributed = self.contributed_panels();
        let first_contributed = contributed.first().copied();
        for place in RightPlace::ALL.into_iter().chain(contributed) {
            if place == RightPlace::Files || Some(place) == first_contributed {
                column = column.child(
                    div()
                        .w(px(18.0))
                        .h(px(1.0))
                        .my(px(6.0))
                        .bg(theme.separator(false)),
                );
            }
            let active = open && current == place;
            let badge = badges.get(&place).copied();
            let key = self.place_key(place);
            let label = self.place_label(place);
            let id = SharedString::from(format!("rail-{key}"));
            let accessible: SharedString = match badge {
                Some(RailBadge::Count(n, RailTone::Accent)) => {
                    format!("{label}, {n} running").into()
                }
                Some(RailBadge::Count(n, RailTone::Warning)) => {
                    format!("{label}, {n} need you").into()
                }
                Some(RailBadge::Dot) => format!("{label}, active").into(),
                None => self.place_accessible_label(place),
            };
            let tip: SharedString = if active {
                format!("Hide {label}").into()
            } else {
                label.clone()
            };
            let button = div()
                .id(id.clone())
                .debug_selector(|| format!("rail-{key}"))
                .role(gpui::Role::Tab)
                .aria_label(accessible)
                .aria_selected(active)
                .tab_index(0)
                .relative()
                .size(px(34.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(9.0))
                .cursor_pointer()
                .focus_visible({
                    let ring = theme.focus_ring();
                    move |style| crate::theme::focus_ring(style, ring)
                })
                .when(active, |el| {
                    el.bg(crate::theme::fill(crate::theme::FillTier::Secondary))
                })
                .when(!active, |el| el.hover_wash(id.clone()))
                .tooltip(move |_, cx| crate::pickers::ControlTooltip::view(tip.clone(), None, cx))
                .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
                .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                    window.prevent_default()
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.toggle_right_place(place, window, cx);
                }))
                .on_key_down(crate::swallow_activation_key)
                .child(
                    icon(self.place_icon(place))
                        .size(px(18.0))
                        .text_color(if active {
                            theme.text
                        } else {
                            theme.readable_muted(0.75)
                        }),
                )
                // The active place's marker on the rail's outer edge.
                .when(active, |el| {
                    el.child(
                        div()
                            .absolute()
                            .right(px(-5.0))
                            .top(px(9.0))
                            .bottom(px(9.0))
                            .w(px(2.0))
                            .rounded_full()
                            .bg(theme.accent),
                    )
                })
                .when_some(badge, |el, badge| el.child(Self::rail_badge(badge, &theme)))
                .children(self.right_probe_child(|| format!("rail:{key}")));
            column = column.child(button);
        }
        #[cfg(feature = "appshots-fixture")]
        self.right_mark("rail-badges", || {
            RightPlace::ALL
                .iter()
                .filter_map(|place| {
                    badges.get(place).map(|badge| match badge {
                        RailBadge::Count(n, RailTone::Accent) => {
                            format!("{}:{n}:running", place.key())
                        }
                        RailBadge::Count(n, RailTone::Warning) => {
                            format!("{}:{n}:needs-you", place.key())
                        }
                        RailBadge::Dot => format!("{}:dot", place.key()),
                    })
                })
                .collect()
        });
        column.into_any_element()
    }

    fn rail_badge(badge: RailBadge, theme: &Theme) -> AnyElement {
        let base = div()
            .absolute()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_2()
            .border_color(theme.surface);
        match badge {
            RailBadge::Dot => base
                .top(px(5.0))
                .right(px(5.0))
                .size(px(9.0))
                .bg(theme.readable_tone(theme.success, 1.0))
                .into_any_element(),
            RailBadge::Count(count, tone) => {
                let (fill, text) = match tone {
                    RailTone::Accent => (theme.accent, gpui::white()),
                    RailTone::Warning => (theme.warning, gpui::black()),
                };
                base.top(px(-1.0))
                    .right(px(-2.0))
                    .h(px(17.0))
                    .min_w(px(17.0))
                    .px(px(4.0))
                    .bg(fill)
                    .text_size(px(10.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(text)
                    .child(if count > 9 {
                        "9+".to_string()
                    } else {
                        count.to_string()
                    })
                    .into_any_element()
            }
        }
    }

    /// The rail's badges: running agents, related chats that need the user,
    /// and a dot while the chat has terminals open.
    fn rail_badges(&self, cx: &App) -> std::collections::HashMap<RightPlace, RailBadge> {
        let mut badges = std::collections::HashMap::new();
        let running = super::agents::running_agent_count(&self.state.read(cx).transcript);
        if running > 0 {
            badges.insert(
                RightPlace::Agents,
                RailBadge::Count(running, RailTone::Accent),
            );
        }
        let needs_you = self.related_needs_you(cx);
        if needs_you > 0 {
            badges.insert(
                RightPlace::Related,
                RailBadge::Count(needs_you, RailTone::Warning),
            );
        }
        let key = self.panel_key(cx);
        if self.right_tabs.get(&key).is_some_and(|tabs| {
            tabs.iter()
                .any(|tab| matches!(tab, RightSurface::Terminal(_)))
        }) {
            badges.insert(RightPlace::Terminal, RailBadge::Dot);
        }
        badges
    }

    /// The titlebar band over the sidebar: the place's name and its actions.
    pub(super) fn render_right_place_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let place = self.right_place(cx);
        let mut actions: Vec<AnyElement> = Vec::new();
        match place {
            RightPlace::Related if !self.active_chat.is_empty() => {
                actions.push(self.render_side_chat_action(cx));
            }
            RightPlace::Changes if self.space_git_detected(cx) => {
                actions.push(
                    Self::related_header_button(
                        "right-changes-history",
                        icons::GIT_BRANCH,
                        "History",
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.show_history_surface(cx)))
                    .children(self.right_probe_child(|| "header:history".to_string()))
                    .into_any_element(),
                );
            }
            _ => {}
        }
        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .pl(px(4.0))
            .child(
                div()
                    .id("right-place-title")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(13.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(self.place_label(place))
                    .children(self.right_probe_child(|| "header:title".to_string())),
            )
            .children(actions)
            .into_any_element()
    }

    /// History: the open History tab, or a new one.
    fn show_history_surface(&mut self, cx: &mut Context<Self>) {
        let key = self.panel_key(cx);
        let existing = self.right_tabs.get(&key).and_then(|tabs| {
            tabs.iter().copied().find(|surface| {
                matches!(surface, RightSurface::Diff(id) if self
                    .diffs
                    .get(id)
                    .is_some_and(|changes| changes.read(cx).is_history()))
            })
        });
        match existing {
            Some(surface) => self.set_right_active(surface, cx),
            None => self.add_history_surface(cx),
        }
    }

    /// A place's own page: the Overview, the agents list, the related
    /// chats, the automations, or a workspace place's empty state.
    pub(super) fn render_place_home(
        &mut self,
        place: RightPlace,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        #[cfg(feature = "appshots-fixture")]
        self.right_mark("home", || vec![place.key().to_string()]);
        let body: AnyElement = match place {
            RightPlace::Overview => self.render_overview(&theme, cx),
            RightPlace::Agents => self.render_agents_section(cx).0,
            RightPlace::Related => {
                let (chats, _) = self.render_related_section(cx);
                let tickets = self.render_tickets_section(cx).map(|(rows, summary)| {
                    div()
                        .flex()
                        .flex_col()
                        .pt(crate::typography::ui_rems(10.0))
                        .child(Self::place_subheading(
                            format!("Tickets · {summary}"),
                            &theme,
                        ))
                        .child(rows)
                });
                div()
                    .flex()
                    .flex_col()
                    .child(chats)
                    .children(tickets)
                    .into_any_element()
            }
            // The panel scrolls itself (its editor keeps its height).
            RightPlace::Automations => {
                let scope = self.automations_conversation_scope(cx);
                return self.automations_outlet(scope, false, cx).into_any_element();
            }
            // CORE-12: the host renders the extension's view for this task.
            RightPlace::Contributed(key) => {
                return self.render_contribution_panel(key, cx);
            }
            RightPlace::Files
            | RightPlace::Changes
            | RightPlace::Terminal
            | RightPlace::Browser => {
                return self.render_place_empty(place, &theme, cx);
            }
        };
        let drop_tint = theme.accent.opacity(0.06);
        let scroller = div()
            .id(SharedString::from(format!("right-place-{}", place.key())))
            .size_full()
            .overflow_y_scroll()
            .px(px(Theme::SPACE_SM))
            .pt(crate::typography::ui_rems(2.0))
            .pb(crate::typography::ui_rems(16.0))
            .flex()
            .flex_col()
            .text_color(theme.text)
            .when(place == RightPlace::Related, |el| {
                // Drop a sidebar row here to list it alongside this chat (a
                // child listed in the sidebar returns to its parent's list).
                el.on_drop::<SidebarChatDrag>(cx.listener(|this, drag: &SidebarChatDrag, _, cx| {
                    this.sidebar_drag = None;
                    if drag.origin == DragOrigin::Sidebar {
                        this.dock_beside_active(drag, cx);
                    }
                    cx.notify();
                }))
                .drag_over::<SidebarChatDrag>(move |style, drag, _, _| {
                    if drag.origin == DragOrigin::Sidebar {
                        style.bg(drop_tint)
                    } else {
                        style
                    }
                })
            })
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex_none()
                    .child(body)
                    .children(self.right_probe_child(|| format!("home-body:{}", place.key()))),
            );
        // The page's visible frame (its content scrolls inside it).
        div()
            .relative()
            .size_full()
            .child(scroller)
            .when(place == RightPlace::Related, |el| {
                el.children(self.board_probe_child(|| "related-chats-viewport".to_string()))
            })
            .children(self.right_probe_child(|| "home-viewport".to_string()))
            .into_any_element()
    }

    fn place_subheading(label: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
        div()
            .px(px(Theme::SPACE_SM))
            .pt(crate::typography::ui_rems(6.0))
            .pb(crate::typography::ui_rems(4.0))
            .text_size(crate::typography::ui_rems(11.5))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.readable_faint(0.9))
            .child(label.into())
    }

    /// The Overview: the chat's Status, then one row per chat place with its
    /// live summary; a row opens that place (the rail follows).
    fn render_overview(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let status = self.render_status_section(theme, cx);
        let (running, agents_total) = {
            let state = self.state.read(cx);
            let (rows, total) = super::agents::agent_rows(&state.transcript);
            (
                super::agents::running_agent_count(&state.transcript),
                total.max(rows.len()),
            )
        };
        let related = self.active_related_chat_count(cx);
        let tickets = {
            let owner = self.related_tickets_owner(cx);
            self.board_chat.related(&owner).len()
        };
        let needs_you = self.related_needs_you(cx);
        let agents_summary = match (running, agents_total) {
            (0, 0) => "No agents yet".to_string(),
            (0, total) => format!("{total} observed"),
            (running, total) => format!("{running} running · {total} observed"),
        };
        let related_summary = format!(
            "{related} chat{} · {tickets} ticket{}",
            if related == 1 { "" } else { "s" },
            if tickets == 1 { "" } else { "s" }
        );
        let glance = [
            (
                RightPlace::Agents,
                agents_summary,
                (running > 0).then(|| (format!("{running} running"), RailTone::Accent)),
            ),
            (
                RightPlace::Related,
                related_summary,
                (needs_you > 0).then(|| (format!("{needs_you} need you"), RailTone::Warning)),
            ),
            (
                RightPlace::Automations,
                "Schedules and triggers for this chat".to_string(),
                None,
            ),
        ];
        let mut rows = div().flex().flex_col().gap(px(2.0));
        for (place, summary, pill) in glance {
            let key = place.key();
            let id = SharedString::from(format!("overview-open-{key}"));
            rows = rows.child(
                div()
                    .id(id.clone())
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(format!("{}: {summary}", place.label())))
                    .tab_index(0)
                    .w_full()
                    .min_h(crate::typography::ui_rems(40.0))
                    .px(px(Theme::SPACE_SM))
                    .py(crate::typography::ui_rems(4.0))
                    .rounded(px(crate::theme::radius::MD))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(crate::typography::ui_rems(10.0))
                    .cursor_pointer()
                    .hover_wash(id.clone())
                    .focus_visible({
                        let ring = theme.focus_ring();
                        move |style| crate::theme::focus_ring(style, ring)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.show_right_place(place, cx)))
                    .on_key_down(crate::swallow_activation_key)
                    .child(
                        icon(place.icon())
                            .size(crate::typography::ui_rems(15.0))
                            .flex_none()
                            .text_color(theme.text_muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .child(place.label()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(crate::typography::ui_rems(11.5))
                                    .text_color(theme.readable_muted(0.8))
                                    .child(summary),
                            ),
                    )
                    .when_some(pill, |row, (label, tone)| {
                        let tone = match tone {
                            RailTone::Accent => theme.accent,
                            RailTone::Warning => theme.warning,
                        };
                        row.child(
                            div()
                                .flex_none()
                                .h(crate::typography::ui_rems(20.0))
                                .px(crate::typography::ui_rems(7.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .bg(tone.opacity(0.16))
                                .text_size(crate::typography::ui_rems(11.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme.readable_tone(tone, 1.0))
                                .child(label),
                        )
                    })
                    .child(
                        icon(icons::ALT_ARROW_RIGHT)
                            .size(crate::typography::ui_rems(11.0))
                            .flex_none()
                            .text_color(theme.readable_faint(0.8)),
                    )
                    .children(self.right_probe_child(|| format!("overview:{key}"))),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(10.0))
            .child(
                div()
                    .rounded(px(crate::theme::radius::LG))
                    .border_1()
                    .border_color(theme.separator(false))
                    .bg(crate::theme::fill(crate::theme::FillTier::Quaternary))
                    .p(px(4.0))
                    .child(status),
            )
            .child(rows)
            .into_any_element()
    }

    /// A workspace place with nothing open: why, and what opens one.
    fn render_place_empty(
        &mut self,
        place: RightPlace,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let git = self.space_git_detected(cx);
        let restoring = place == RightPlace::Terminal
            && self.terminal_request.as_deref() == Some(self.active_chat.as_str());
        let (message, action): (SharedString, Option<&'static str>) = match place {
            RightPlace::Files => ("No files open.".into(), Some("Browse files")),
            RightPlace::Changes if !git => ("No Git repository in this folder.".into(), None),
            RightPlace::Changes => ("No diff open.".into(), Some("Diff the working tree")),
            RightPlace::Terminal if restoring => ("Restoring saved terminals…".into(), None),
            RightPlace::Terminal => ("No terminals open.".into(), Some("New terminal")),
            RightPlace::Browser => ("No pages open.".into(), Some("New page")),
            _ => (SharedString::default(), None),
        };
        let notice = (place == RightPlace::Terminal)
            .then(|| {
                self.terminal_picker_notice
                    .as_ref()
                    .filter(|(chat, _)| chat == &self.active_chat)
                    .map(|(_, notice)| notice.clone())
            })
            .flatten();
        #[cfg(feature = "appshots-fixture")]
        self.right_mark("empty", || vec![format!("{}|{message}", place.key())]);
        div()
            .id(SharedString::from(format!("right-empty-{}", place.key())))
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(crate::typography::ui_rems(10.0))
            .p(px(16.0))
            .child(
                icon(place.icon())
                    .size(px(22.0))
                    .text_color(theme.readable_faint(0.8)),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(12.5))
                    .text_color(theme.text_muted)
                    .child(message),
            )
            .when_some(notice, |el, notice| {
                el.child(
                    div()
                        .max_w(px(280.0))
                        .text_center()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.readable_muted(0.8))
                        .child(notice),
                )
            })
            .when_some(action, |el, label| {
                el.child(
                    crate::button::button(
                        SharedString::from(format!("right-empty-open-{}", place.key())),
                        ButtonKind::Secondary,
                        ControlSize::Sm,
                        true,
                        theme,
                    )
                    .aria_label(label)
                    .child(crate::button::glyph(icons::PLUS, ControlSize::Sm))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| match place {
                        RightPlace::Files => this.add_files_surface(window, cx),
                        RightPlace::Changes => this.add_diff_surface(cx),
                        RightPlace::Terminal => this.add_new_terminal_surface(cx),
                        RightPlace::Browser => this.add_browser_surface(None, window, cx),
                        _ => {}
                    }))
                    .children(self.right_probe_child(|| format!("empty-open:{}", place.key()))),
                )
            })
            .into_any_element()
    }

    /// The Status section: the focused conversation's checkout (branch,
    /// worktree or local checkout, pull request) and the Git actions that
    /// act on that same checkout.
    fn render_status_section(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let focused = self
            .workbench_execution_focus(cx)
            .unwrap_or_else(|| self.active_chat.clone());
        let (branch, checkout, git, cwd, change_request) = {
            let state = self.state.read(cx);
            let chat = state.chats.iter().find(|chat| chat.id == focused);
            let space = chat.and_then(|chat| state.space_for_chat(chat));
            let worktree = chat
                .and_then(|chat| chat.cwd.as_deref())
                .zip(space.map(|space| space.path.as_str()))
                .is_some_and(|(cwd, root)| cwd != root);
            let git = space.is_some_and(|space| space.git_detected);
            let checkout = if worktree {
                (icons::FOLDER_WITH_FILES, "Worktree")
            } else {
                (icons::FOLDER, "Local checkout")
            };
            let branch = chat
                .and_then(|chat| crate::change_requests::conversation_branch(chat, &state.spaces))
                .map(str::trim)
                .filter(|branch| !branch.is_empty())
                .map(|branch| SharedString::from(branch.to_string()));
            let cwd = chat
                .and_then(|chat| chat.cwd.clone())
                .or_else(|| space.map(|space| space.path.clone()));
            (
                branch,
                checkout,
                git,
                cwd,
                chat.and_then(|chat| state.change_request_for_chat(chat).cloned()),
            )
        };
        let row = |id: &'static str| {
            div()
                .id(id)
                .w_full()
                .min_w_0()
                .min_h(crate::typography::ui_rems(28.0))
                .px(px(Theme::SPACE_SM))
                .flex()
                .flex_row()
                .items_center()
                .gap(crate::typography::ui_rems(8.0))
                .text_size(crate::typography::ui_rems(12.5))
        };
        let lead = |glyph: &'static str| {
            icon(glyph)
                .size(crate::typography::ui_rems(13.0))
                .flex_none()
                .text_color(theme.text_muted)
        };
        let mut body = div().w_full().flex().flex_col().gap(px(2.0));
        if !git {
            let folder: SharedString = cwd
                .map(SharedString::from)
                .unwrap_or_else(|| "This chat's folder".into());
            body = body.child(
                row("overview-status-folder")
                    .child(lead(icons::FOLDER))
                    .child(div().flex_1().min_w_0().truncate().child(folder)),
            );
            body = body.child(
                row("overview-status-no-git")
                    .child(div().w(crate::typography::ui_rems(13.0)).flex_none())
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(theme.readable_muted(0.8))
                            .child("No Git repository in this folder"),
                    ),
            );
            return body.into_any_element();
        }
        let (checkout_glyph, checkout_label) = checkout;
        // The branch leads when the checkout reports one; otherwise the row
        // names the checkout itself.
        let (row_glyph, row_text, chip) = match branch {
            Some(branch) => (
                icons::GIT_BRANCH,
                branch,
                Some((checkout_glyph, checkout_label)),
            ),
            None => (checkout_glyph, SharedString::from(checkout_label), None),
        };
        body = body.child(
            row("overview-status-branch")
                .child(lead(row_glyph))
                .child(div().flex_1().min_w_0().truncate().child(row_text))
                .when_some(chip, |row, (glyph, label)| {
                    row.child(
                        div()
                            .flex_none()
                            .h(crate::typography::ui_rems(20.0))
                            .px(crate::typography::ui_rems(6.0))
                            .rounded(px(crate::theme::radius::SM))
                            .bg(crate::theme::fill(crate::theme::FillTier::Tertiary))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(crate::typography::ui_rems(4.0))
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.readable_muted(0.85))
                            .child(
                                icon(glyph)
                                    .size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(label),
                    )
                }),
        );
        if let Some(summary) = change_request {
            let title = transcript::single_line(&summary.title);
            body = body.child(
                row("overview-status-pr")
                    .child(lead(icons::PULL_REQUEST))
                    .child(crate::change_requests::pull_request_badge(
                        "overview-status-pr-badge".into(),
                        summary,
                        crate::change_requests::ChangeRequestBadgeSurface::Sidebar,
                        theme,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(title),
                    ),
            );
        }
        let review_chat = focused.clone();
        let commit_chat = focused;
        body.child(
            div()
                .px(px(Theme::SPACE_SM))
                .pt(crate::typography::ui_rems(4.0))
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(crate::typography::ui_rems(6.0))
                .child(
                    crate::button::button(
                        "overview-review-changes",
                        ButtonKind::Secondary,
                        ControlSize::Sm,
                        true,
                        theme,
                    )
                    .aria_label("Review changes")
                    .relative()
                    .child(crate::button::glyph(icons::GIT_DIFF, ControlSize::Sm))
                    .child("Review changes")
                    .children(self.right_probe_child(|| "overview-review".into()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.add_diff_surface_for(review_chat.clone(), cx);
                    })),
                )
                .child(
                    crate::button::button(
                        "overview-commit-push",
                        ButtonKind::Secondary,
                        ControlSize::Sm,
                        true,
                        theme,
                    )
                    .aria_label("Commit and push (preview first)")
                    .relative()
                    .child(crate::button::glyph(icons::CHECK, ControlSize::Sm))
                    .child("Commit & push…")
                    .children(self.right_probe_child(|| "overview-commit".into()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(changes) = this.add_diff_surface_for(commit_chat.clone(), cx) {
                            changes.update(cx, |changes, cx| changes.open_git_actions(cx));
                        }
                    })),
                ),
        )
        .into_any_element()
    }

    /// A probe child recording its parent's painted bounds as `label`, for
    /// the right-sidebar QA fixture only.
    pub(super) fn right_probe_child(&self, label: impl FnOnce() -> String) -> Option<AnyElement> {
        #[cfg(feature = "appshots-fixture")]
        if let Some(probe) = &self.right_probe {
            let bounds = probe.bounds.clone();
            let label = label();
            return Some(
                gpui::canvas(
                    move |painted, _, _| {
                        bounds.borrow_mut().insert(label.clone(), painted);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
                .into_any_element(),
            );
        }
        #[cfg(not(feature = "appshots-fixture"))]
        let _ = label;
        None
    }

    #[cfg(feature = "appshots-fixture")]
    fn right_mark(&self, key: &str, values: impl FnOnce() -> Vec<String>) {
        if let Some(probe) = &self.right_probe {
            probe.marks.borrow_mut().insert(key.to_string(), values());
        }
    }
}

/// The right-sidebar fixture drives the production rail, places and tab
/// rows through these hooks.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_enable_right_probe(&mut self, cx: &mut Context<Self>) {
        self.right_probe = Some(RightProbe::default());
        cx.notify();
    }

    /// What the right sidebar holds and painted: route state, the place,
    /// every open tab (with its place), marks and probed bounds.
    pub fn fixture_right_sidebar_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        let bounds = self
            .right_probe
            .as_ref()
            .map(|probe| {
                probe
                    .bounds
                    .borrow()
                    .iter()
                    .map(|(label, b)| {
                        (
                            label.clone(),
                            serde_json::json!([
                                f32::from(b.origin.x),
                                f32::from(b.origin.y),
                                f32::from(b.size.width),
                                f32::from(b.size.height)
                            ]),
                        )
                    })
                    .collect::<serde_json::Map<_, _>>()
            })
            .unwrap_or_default();
        let marks = self
            .right_probe
            .as_ref()
            .map(|probe| serde_json::json!(*probe.marks.borrow()))
            .unwrap_or(serde_json::Value::Null);
        serde_json::json!({
            "activeChat": self.active_chat,
            "rightPaneOpen": self.right_pane_open(cx),
            "rightActive": format!("{:?}", self.resolved_right_active(cx)),
            "place": self.right_place(cx).key(),
            "expanded": self.right_pane_expanded,
            "viewport": self.viewport_width,
            "tabs": self
                .right_surface_rows(cx)
                .iter()
                .map(|(surface, title, _, _)| serde_json::json!([
                    format!("{surface:?}"),
                    RightPlace::of(*surface).key(),
                    title.to_string()
                ]))
                .collect::<Vec<_>>(),
            "tabMenuOpen": self.right_tab_menu.get().map(|surface| format!("{surface:?}")),
            "bounds": bounds,
            "marks": marks,
        })
    }

    /// Forget probed bounds so the next paint records only what still shows.
    pub fn fixture_clear_right_probe(&mut self, cx: &mut Context<Self>) {
        if let Some(probe) = &self.right_probe {
            probe.bounds.borrow_mut().clear();
            probe.marks.borrow_mut().clear();
        }
        cx.notify();
    }

    /// Open the sidebar on `place` through the production entry path.
    pub fn fixture_show_right_place(&mut self, place: RightPlace, cx: &mut Context<Self>) {
        self.show_right_place(place, cx);
    }
}
