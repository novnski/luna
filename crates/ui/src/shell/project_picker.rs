//! The project picker: one searchable card for choosing a project. The
//! sidebar's project filter and the Board's project choices (New ticket and
//! From GitHub issue under All projects) open this same card: a search field
//! on top (`PaletteSearch` context, so ↑↓/⏎ bubble to the card), ranked rows
//! with the project's tile, name and folder, live counts on the right, and a
//! keyboard highlight.
//!
//! Child module of `shell` for the project tile and activity helpers.

use super::sidebar_projects::{
    ProjectActivity, all_projects_tile, display_project_path, open_tasks, project_tile,
    project_tint,
};
use super::*;
use gpui::{EventEmitter, FocusHandle, MouseMoveEvent, ScrollHandle, Window};

/// One activatable row, in nav order. `AddSpace` and `ManageProjects` name
/// the pinned footer's actions, not list rows: keyboard nav maps the indexes
/// past the list to them.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ProjectPickerRow {
    All,
    Space(String),
    AddSpace,
    /// The pinned footer's second action: Settings › Projects.
    ManageProjects,
}

/// What the card's host hears.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ProjectPickerEvent {
    /// A row or footer action was chosen (click or ⏎).
    Chose(ProjectPickerRow),
    /// Right-click on a project row (the sidebar's project actions).
    ContextMenu {
        space_id: String,
        position: Point<Pixels>,
    },
    /// Escape, or a press outside the card.
    Dismissed,
}

/// Which projects the card offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectSource {
    /// Every project (the sidebar filter).
    Every,
    /// Only this host's projects (the Board is owner-local).
    ThisHost,
}

/// Element ids, so each host keeps its own (the sidebar's stay
/// `spaces-menu-…`).
#[derive(Debug, Clone, Copy)]
pub(super) struct ProjectPickerIds {
    pub list_host: &'static str,
    pub list: &'static str,
    pub scrollbar: &'static str,
    pub row: &'static str,
    pub add: &'static str,
    pub manage: &'static str,
}

impl ProjectPickerIds {
    pub(super) const SIDEBAR: Self = Self {
        list_host: "spaces-menu-list-host",
        list: "spaces-menu-list",
        scrollbar: "spaces-menu-scrollbar",
        row: "spaces-menu-row",
        add: "spaces-menu-add",
        manage: "spaces-menu-manage",
    };

    pub(super) const BOARD: Self = Self {
        list_host: "board-project-picker-list-host",
        list: "board-project-picker-list",
        scrollbar: "board-project-picker-scrollbar",
        row: "board-project-picker-row",
        add: "board-project-picker-add",
        manage: "board-project-picker-manage",
    };
}

/// How a host configures the card when it opens it.
pub(super) struct ProjectPickerOptions {
    pub source: ProjectSource,
    /// The current choice (`None`: All projects, or nothing chosen yet).
    pub selected: Option<String>,
    /// Offer "All projects" first while the search is empty.
    pub all_row: bool,
    /// The pinned New project… / Manage projects… actions.
    pub footer: bool,
    /// Right-click on a row asks for the project's actions.
    pub context_menu: bool,
    pub ids: ProjectPickerIds,
    pub width: Pixels,
}

/// One rendered row: what it activates, its two text lines and its live
/// counts (`None` for All projects, which sums them in its subtitle).
struct RowDetail {
    row: ProjectPickerRow,
    label: SharedString,
    /// The folder path for a project; the total for All projects.
    subtitle: SharedString,
    /// The machine, only to tell apart same-named projects.
    tag: Option<SharedString>,
    offline: bool,
    selected: bool,
    activity: Option<ProjectActivity>,
    hint: Option<SharedString>,
    /// The project's tile colour (`None` for All projects).
    tint: Option<gpui::Hsla>,
}

/// The open card. Hosts create one per open and drop it on close.
pub(super) struct ProjectPicker {
    state: Entity<AppState>,
    options: ProjectPickerOptions,
    /// Live counts per project (the sidebar's chats), pushed by the host.
    activity: std::collections::HashMap<String, ProjectActivity>,
    /// A short right-hand note per project when there are no live counts
    /// (the Board's ticket totals).
    hints: std::collections::HashMap<String, SharedString>,
    search: Entity<ComposerInput>,
    /// Keyboard highlight: an index into [`Self::rows`], or past it while a
    /// pinned footer action holds it.
    active: usize,
    /// Tracked on the card: puts it on the keyboard dispatch path while the
    /// search input holds focus (the structure every working picker uses).
    focus: FocusHandle,
    list_scroll: ScrollHandle,
    bar: popover::MenuScrollbarState,
    /// A choice or dismissal was reported: the card is on its way out and
    /// must not drive anything else through its exit animation.
    done: bool,
    _search_events: Subscription,
}

impl EventEmitter<ProjectPickerEvent> for ProjectPicker {}

impl ProjectPicker {
    pub(super) fn new(
        state: Entity<AppState>,
        options: ProjectPickerOptions,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| ComposerInput::with_context("Search projects…", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                this.active = 0;
                cx.notify();
            }
        });
        let mut picker = Self {
            state,
            options,
            activity: Default::default(),
            hints: Default::default(),
            search,
            active: 0,
            focus: cx.focus_handle(),
            list_scroll: ScrollHandle::new(),
            bar: popover::MenuScrollbarState::default(),
            done: false,
            _search_events: search_events,
        };
        // The highlight starts ON the current choice.
        let rows = picker.rows(cx);
        picker.active = picker
            .options
            .selected
            .as_ref()
            .and_then(|id| {
                rows.iter()
                    .position(|row| matches!(row, ProjectPickerRow::Space(s) if s == id))
            })
            .unwrap_or(0);
        picker
    }

    /// The search field, which takes the keyboard when the card opens.
    pub(super) fn search_focus(&self, cx: &App) -> FocusHandle {
        self.search.read(cx).focus_handle(cx)
    }

    pub(super) fn set_activity(
        &mut self,
        activity: std::collections::HashMap<String, ProjectActivity>,
    ) {
        self.activity = activity;
    }

    pub(super) fn set_hints(&mut self, hints: std::collections::HashMap<String, SharedString>) {
        self.hints = hints;
    }

    pub(super) fn set_width(&mut self, width: Pixels) {
        self.options.width = width;
    }

    /// The projects offered, before ranking.
    fn projects<'a>(&self, state: &'a AppState) -> Vec<&'a luna_proto::Space> {
        let local = state.local_device_id.as_deref();
        state
            .spaces_sorted()
            .into_iter()
            .filter(|space| match self.options.source {
                ProjectSource::Every => true,
                ProjectSource::ThisHost => local.is_none_or(|device| space.device_id == device),
            })
            .collect()
    }

    /// The scrollable rows: "All projects" (only on an empty query: searching
    /// means hunting a project), then projects matching the search, ranked
    /// (`popover::filter_indices`). The footer actions are not rows here; the
    /// card pins them under the list.
    fn rows(&self, cx: &App) -> Vec<ProjectPickerRow> {
        let query = self.search.read(cx).text().to_string();
        let state = self.state.read(cx);
        let spaces = self.projects(state);
        let names: Vec<&str> = spaces.iter().map(|space| space.display_name()).collect();
        let mut rows = Vec::new();
        if self.options.all_row && query.trim().is_empty() {
            rows.push(ProjectPickerRow::All);
        }
        rows.extend(
            popover::filter_indices(&query, &names)
                .into_iter()
                .map(|ix| ProjectPickerRow::Space(spaces[ix].id.clone())),
        );
        rows
    }

    /// Nav slots: the rows, then the footer's two actions.
    fn nav_count(&self, rows: usize) -> usize {
        rows + if self.options.footer { 2 } else { 0 }
    }

    fn choose(&mut self, row: ProjectPickerRow, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        self.done = true;
        cx.emit(ProjectPickerEvent::Chose(row));
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        self.done = true;
        cx.emit(ProjectPickerEvent::Dismissed);
    }

    /// Card keys (bubbling from the focused search input): ↑↓ navigate, ⏎
    /// activates the highlighted row, esc closes.
    fn on_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        // The card stays mounted (and focused) through the exit animation:
        // keys must not drive a dying menu.
        if self.done {
            return;
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.dismiss(cx);
                cx.stop_propagation();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let rows = self.rows(cx);
                let count = self.nav_count(rows.len());
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                self.active = popover::menu_step(Some(self.active), count, delta).unwrap_or(0);
                // The footer renders below the scroller: only in-list rows
                // can be scrolled to.
                if self.active < rows.len() {
                    self.list_scroll.scroll_to_item(self.active);
                }
                cx.stop_propagation();
                cx.notify();
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let rows = self.rows(cx);
                let row = if self.active < rows.len() {
                    Some(rows[self.active].clone())
                } else if self.options.footer && self.active == rows.len() {
                    Some(ProjectPickerRow::AddSpace)
                } else if self.options.footer {
                    Some(ProjectPickerRow::ManageProjects)
                } else {
                    None
                };
                cx.stop_propagation();
                if let Some(row) = row {
                    self.choose(row, cx);
                }
            }
            popover::MenuKey::Backspace | popover::MenuKey::Other => {}
        }
    }

    fn details(&self, rows: Vec<ProjectPickerRow>, cx: &App) -> Vec<RowDetail> {
        let state = self.state.read(cx);
        let now = Utc::now();
        let selected = self.options.selected.clone();
        // No machine names, except to tell apart projects that share a name
        // on different machines.
        let mut name_counts: std::collections::HashMap<&str, usize> = Default::default();
        for row in &rows {
            if let ProjectPickerRow::Space(id) = row
                && let Some(space) = state.space_row(id)
            {
                *name_counts.entry(space.display_name()).or_default() += 1;
            }
        }
        let projects = self.projects(state).len();
        rows.into_iter()
            .map(|row| match row {
                ProjectPickerRow::All => {
                    let active: usize = self.activity.values().map(|project| project.active).sum();
                    RowDetail {
                        row: ProjectPickerRow::All,
                        label: SharedString::from("All projects"),
                        subtitle: format!(
                            "{} across {projects} project{}",
                            open_tasks(active),
                            if projects == 1 { "" } else { "s" }
                        )
                        .into(),
                        tag: None,
                        offline: false,
                        selected: selected.is_none(),
                        activity: None,
                        hint: None,
                        tint: None,
                    }
                }
                ProjectPickerRow::Space(id) => {
                    let is_selected = selected.as_deref() == Some(id.as_str());
                    let activity = (!self.hints.contains_key(&id))
                        .then(|| self.activity.get(&id).copied().unwrap_or_default());
                    let hint = self.hints.get(&id).cloned();
                    let tint = Some(project_tint(state, &id));
                    match state.space_row(&id) {
                        Some(space) => {
                            let (tag, offline) = state.space_device_tag(space, now);
                            let shared_name = name_counts
                                .get(space.display_name())
                                .is_some_and(|count| *count > 1);
                            RowDetail {
                                label: space.display_name().to_string().into(),
                                subtitle: display_project_path(&space.path).into(),
                                tag: shared_name.then(|| tag.into()),
                                offline,
                                selected: is_selected,
                                activity,
                                hint,
                                tint,
                                row: ProjectPickerRow::Space(id),
                            }
                        }
                        None => RowDetail {
                            row: ProjectPickerRow::Space(id),
                            label: SharedString::from("?"),
                            subtitle: SharedString::default(),
                            tag: None,
                            offline: false,
                            selected: is_selected,
                            activity,
                            hint,
                            tint,
                        },
                    }
                }
                // rows() never yields these: the card renders the footer.
                ProjectPickerRow::AddSpace | ProjectPickerRow::ManageProjects => unreachable!(),
            })
            .collect()
    }

    /// A row's live counts, right-aligned before the check column: what
    /// needs the user, what is running, or when a quiet project was last
    /// used.
    fn activity_counts(
        project: ProjectActivity,
        now: chrono::DateTime<Utc>,
        theme: &Theme,
    ) -> gpui::Div {
        let count = |tone: gpui::Hsla, glyph: AnyElement, n: usize| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(crate::typography::ui_rems(3.0))
                .text_color(theme.readable_tone(tone, 1.0))
                .child(glyph)
                .child(SharedString::from(n.to_string()))
        };
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(crate::typography::ui_rems(8.0))
            .text_size(crate::typography::ui_rems(11.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .font_features(crate::typography::tabular_figures())
            .when(project.need > 0, |el| {
                el.child(count(
                    theme.warning,
                    icon(icons::SHIELD_WARNING)
                        .size(crate::typography::ui_rems(12.0))
                        .text_color(theme.readable_icon(theme.warning))
                        .into_any_element(),
                    project.need,
                ))
            })
            .when(project.working > 0, |el| {
                // A small square: the running task's loader, at rest.
                el.child(count(
                    theme.busy,
                    div()
                        .size(crate::typography::ui_rems(6.0))
                        .rounded(px(1.5))
                        .bg(theme.busy)
                        .into_any_element(),
                    project.working,
                ))
            })
            .when(project.need == 0 && project.working == 0, |el| {
                el.child(
                    div()
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(theme.readable_faint(0.9))
                        .child(SharedString::from(
                            project
                                .last
                                .map(|last| format_time_ago(last, now))
                                .unwrap_or_default(),
                        )),
                )
            })
    }

    fn hint_text(hint: SharedString, theme: &Theme) -> gpui::Div {
        div()
            .flex_none()
            .text_size(crate::typography::ui_rems(11.0))
            .font_features(crate::typography::tabular_figures())
            .text_color(theme.readable_faint(0.9))
            .child(hint)
    }

    /// The rows' labels as fixtures read them: `label[ tag]`, and
    /// `label|subtitle|selected|counts`.
    #[cfg(feature = "appshots-fixture")]
    pub(super) fn fixture_lines(&self, cx: &App) -> (Vec<String>, Vec<String>) {
        let details = self.details(self.rows(cx), cx);
        let labels = details
            .iter()
            .map(|detail| match &detail.tag {
                Some(tag) => format!("{} {tag}", detail.label),
                None => detail.label.to_string(),
            })
            .collect();
        let lines = details
            .iter()
            .map(|detail| {
                let counts = detail.activity.map_or(String::new(), |project| {
                    format!(
                        "need={} working={} active={}",
                        project.need, project.working, project.active
                    )
                });
                format!(
                    "{}|{}|{}|{}",
                    detail.label,
                    detail.subtitle,
                    if detail.selected { "selected" } else { "" },
                    counts
                )
            })
            .collect();
        (labels, lines)
    }

    /// QA: the rows with their painted bounds, the query and the highlight.
    #[cfg(feature = "appshots-fixture")]
    pub(super) fn fixture_readback(&self, cx: &App) -> serde_json::Value {
        let details = self.details(self.rows(cx), cx);
        // Layout bounds are unscrolled: shift them to where the rows paint.
        let offset = self.list_scroll.offset().y;
        let bounds = |ix: usize| {
            self.list_scroll.bounds_for_item(ix).map(|mut bounds| {
                bounds.origin.y += offset;
                bounds
            })
        };
        serde_json::json!({
            "query": self.search.read(cx).text().to_string(),
            "active": self.active,
            "rows": details.iter().enumerate().map(|(ix, detail)| {
                let id = match &detail.row {
                    ProjectPickerRow::Space(id) => Some(id.clone()),
                    _ => None,
                };
                serde_json::json!({
                    "id": id,
                    "label": detail.label.to_string(),
                    "subtitle": detail.subtitle.to_string(),
                    "hint": detail.hint.as_ref().map(|hint| hint.to_string()),
                    "selected": detail.selected,
                    "bounds": bounds(ix).map(|b| serde_json::json!({
                        "x": f32::from(b.origin.x),
                        "y": f32::from(b.origin.y),
                        "w": f32::from(b.size.width),
                        "h": f32::from(b.size.height),
                    })),
                })
            }).collect::<Vec<_>>(),
        })
    }
}

impl popover::ScrollRailHost for ProjectPicker {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        &mut self.bar
    }

    fn rail_scroll(&self) -> Option<ScrollHandle> {
        Some(self.list_scroll.clone())
    }
}

impl Render for ProjectPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_popup();
        let theme = &theme;
        let ids = self.options.ids;
        let rows = self.rows(cx);
        let details = self.details(rows, cx);
        let active = self.active;
        let now = Utc::now();
        let context_menu = self.options.context_menu;
        let scrollbar = popover::rail(self, ids.scrollbar, theme, cx);
        // The pinned footer's keyboard-nav indexes: past the last row.
        let add_index = details.len();
        let manage_index = add_index + 1;

        let list = popover::menu_scroll_list(ids.list, &self.list_scroll)
            .flex()
            .flex_col()
            .gap(px(2.0))
            // Same scroll budget as the composer project menu.
            .max_h(crate::typography::ui_rems(224.0))
            .children(details.into_iter().enumerate().map(|(ix, detail)| {
                let RowDetail {
                    row,
                    label,
                    subtitle,
                    tag,
                    offline,
                    selected,
                    activity,
                    hint,
                    tint,
                } = detail;
                let menu_space = match &row {
                    ProjectPickerRow::Space(id) if context_menu => Some(id.clone()),
                    _ => None,
                };
                let tile = match tint {
                    Some(tint) => project_tile(tint, &label, 28.0, theme),
                    None => all_projects_tile(28.0, theme),
                };
                // Project rows show their folder (mono); All projects a sentence.
                let is_path = !matches!(row, ProjectPickerRow::All);
                let trailing = match (activity, hint) {
                    (_, Some(hint)) => Some(Self::hint_text(hint, theme).into_any_element()),
                    (Some(project), None) => {
                        Some(Self::activity_counts(project, now, theme).into_any_element())
                    }
                    (None, None) => None,
                };
                let activate = row;
                popover::menu_row_nav(theme, selected, ix == active)
                    .id((ids.row, ix))
                    .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                        if this.active != ix {
                            this.active = ix;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose(activate.clone(), cx);
                    }))
                    .when_some(menu_space, |el, space_id| {
                        el.on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                if !this.done {
                                    cx.emit(ProjectPickerEvent::ContextMenu {
                                        space_id: space_id.clone(),
                                        position: event.position,
                                    });
                                }
                            }),
                        )
                    })
                    .child(tile)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(crate::typography::ui_rems(6.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(div().min_w_0().truncate().child(label))
                                    .when_some(tag, |el, tag| {
                                        el.child(
                                            div()
                                                .max_w(gpui::relative(0.5))
                                                .min_w_0()
                                                .truncate()
                                                .text_size(crate::typography::ui_rems(11.0))
                                                .font_weight(gpui::FontWeight::NORMAL)
                                                .text_color(theme.text_muted)
                                                .child(tag),
                                        )
                                    })
                                    // Disconnected glyph, not the word.
                                    .when(offline, |el| {
                                        el.child(
                                            icon(icons::WIFI_OFF)
                                                .size(crate::typography::ui_rems(12.0))
                                                .flex_none()
                                                .text_color(
                                                    theme.readable_tone(theme.warning, 0.8),
                                                ),
                                        )
                                    }),
                            )
                            .when(!subtitle.is_empty(), |el| {
                                el.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(crate::typography::ui_rems(11.0))
                                        .font_weight(gpui::FontWeight::NORMAL)
                                        .when(is_path, |el| el.font_family(theme.font_mono.clone()))
                                        .text_color(theme.readable_faint(0.9))
                                        .child(subtitle),
                                )
                            }),
                    )
                    .children(trailing)
            }));
        let empty = add_index == 0;
        let list = popover::menu_scroll_host(ids.list_host)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.bar.set_list_hovered(*hovered) {
                    cx.notify();
                }
            }))
            .child(list)
            .children(scrollbar);

        let mut card = popover::popover_card(theme)
            .w(self.options.width)
            .track_focus(&self.focus)
            .on_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| this.on_key(event, cx)),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss(cx)))
            .flex()
            .flex_col()
            // Same 2px rhythm as the composer project menu's root.
            .gap(px(2.0))
            .child(popover::search_input_frame(
                theme,
                self.search.clone().into_any_element(),
            ))
            .child(list)
            .when(empty, |card| {
                card.child(
                    div()
                        .px(px(8.0))
                        .py(px(6.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.readable_faint(0.9))
                        .child(SharedString::from("No matching projects")),
                )
            });
        if self.options.footer {
            card = card
                // Pinned under the list: scrolling never carries the actions
                // away. Full-bleed through the card's inset (a divider
                // stopping short of the edges reads as a mistake).
                .child(
                    div()
                        .my(px(2.0))
                        .mx(px(-popover::CARD_INSET))
                        .h(px(1.0))
                        .flex_none()
                        .bg(theme.border.opacity(0.6)),
                )
                .child(Self::footer_row(
                    ids.add,
                    icons::PLUS,
                    "New project…",
                    add_index,
                    active,
                    ProjectPickerRow::AddSpace,
                    theme,
                    cx,
                ))
                .child(Self::footer_row(
                    ids.manage,
                    icons::SETTINGS,
                    "Manage projects…",
                    manage_index,
                    active,
                    ProjectPickerRow::ManageProjects,
                    theme,
                    cx,
                ));
        }
        card
    }
}

impl ProjectPicker {
    #[allow(clippy::too_many_arguments)]
    fn footer_row(
        id: &'static str,
        glyph: &'static str,
        label: &'static str,
        index: usize,
        active: usize,
        row: ProjectPickerRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        popover::menu_row_nav(theme, false, active == index)
            .id(id)
            .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                if this.active != index {
                    this.active = index;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.choose(row.clone(), cx)))
            .child(
                icon(glyph)
                    .size(crate::typography::ui_rems(12.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(label)),
            )
    }
}
