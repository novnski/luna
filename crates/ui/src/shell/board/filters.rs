//! Header and toolbar (A8): the project selector (one project, or All
//! projects aggregated without merging ownership or execution targets), the
//! Board/List switch, search, the Needs attention toggle, provider/source
//! filters, From GitHub issue and New issue. Filters are applied by the
//! engine (`BoardListFilters`) and saved as presentation preferences
//! (`UiSettings.board`).
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use luna_proto::{BoardScope, BoardSourceKind, HarnessId};

use super::page::{BoardMenu, BoardPage, MenuAction, MenuEntry, ProjectIntent};
use crate::button::{ButtonKind, ControlSize, Segment};
use crate::icons;
use crate::settings::{BoardViewMode, widgets};
use crate::theme::Theme;
use crate::typography::ui_rems;

/// Providers offered by the provider filter.
const PROVIDERS: [HarnessId; 3] = [HarnessId::ClaudeCode, HarnessId::Codex, HarnessId::Opencode];
/// Sources offered by the source filter.
const SOURCES: [(BoardSourceKind, &str); 4] = [
    (BoardSourceKind::Local, "Written manually"),
    (BoardSourceKind::Chat, "From a chat"),
    (BoardSourceKind::Authoring, "Discuss with AI"),
    (BoardSourceKind::GitHub, "GitHub issue"),
];

impl BoardPage {
    pub(super) fn render_header(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = self.header_count(cx);
        let subtitle = self.header_subtitle(cx);
        let toolbar = self.render_toolbar(theme, window, cx);
        div()
            .id("board-header")
            .flex()
            .flex_col()
            .gap(ui_rems(12.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(widgets::page_title_bar(theme, "Board", Some(count), vec![]))
                    .child(widgets::page_subtitle(theme, subtitle)),
            )
            .child(toolbar)
            .into_any_element()
    }

    /// The title's count: the engine's total when exact, else what is
    /// loaded.
    pub(super) fn header_count(&self, cx: &gpui::App) -> usize {
        let model = self.model.read(cx);
        model
            .board_total()
            .filter(|_| model.has_more())
            .map(|total| (total as usize).max(model.items().len()))
            .unwrap_or(model.items().len())
    }

    /// The header's help, saying how much is loaded while a column has more.
    pub(super) fn header_subtitle(&self, cx: &gpui::App) -> String {
        let model = self.model.read(cx);
        let help = "Move a ticket to Ready to start it; move it back to Backlog to stop it.";
        if !model.has_more() {
            return format!("{help} Done is set when checks and the review pass.");
        }
        let loaded = model.items().len();
        match model.board_total() {
            Some(total) => format!(
                "Showing {loaded} of {} tickets; Load more at the end of a column. {help}",
                (total as usize).max(loaded)
            ),
            None => format!(
                "Showing the first {loaded} matching tickets; Load more at the end of a column. \
                 {help}"
            ),
        }
    }

    fn render_toolbar(
        &mut self,
        theme: &Theme,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project = self.render_project_select(theme, cx);
        let view = self.render_view_switch(theme, cx);
        let search = self.render_search(theme);
        let filters = self.filters(cx);
        let attention = crate::button::chip(
            "board-attention",
            filters.attention_only,
            ControlSize::Md,
            true,
            theme,
        )
        .aria_label("Needs attention only")
        .gap(ui_rems(6.0))
        .child(crate::button::glyph(
            icons::DANGER_TRIANGLE,
            ControlSize::Md,
        ))
        .child("Needs attention")
        .on_click(cx.listener(|this, _, _, cx| {
            this.update_filters(
                |filters| filters.attention_only = !filters.attention_only,
                cx,
            );
        }))
        .into_any_element();
        let more_filters = self.render_filters_select(theme, cx);
        let github = self.render_github_button(theme, cx);
        let new_issue = self.render_new_issue_button(theme, cx);
        let controls = vec![
            ("board-project", project),
            ("board-view", view),
            ("board-search", search),
            ("board-attention", attention),
            ("board-filters", more_filters),
            ("board-github", github),
            ("board-new-issue", new_issue),
        ];
        let labels: Vec<String> = controls
            .iter()
            .map(|(label, _)| (*label).to_owned())
            .collect();
        let geometry = self.geometry.clone();
        div()
            .on_children_prepainted(move |bounds, _, _| {
                let mut geometry = geometry.borrow_mut();
                for (label, bounds) in labels.iter().zip(bounds) {
                    geometry.controls.insert(label.clone(), bounds);
                }
            })
            .id("board-toolbar")
            .role(gpui::Role::Toolbar)
            .aria_label("Board toolbar")
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(ui_rems(8.0))
            .children(controls.into_iter().map(|(_, control)| control))
            .into_any_element()
    }

    fn render_project_select(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let label = self.scope_label(cx);
        let open = self.menu.get() == Some(&BoardMenu::Project);
        let mut trigger = crate::button::select_frame(
            "board-project",
            ControlSize::Md,
            open && self.menu.is_open(),
            true,
            theme,
        )
        .aria_label(SharedString::from(format!("Project: {label}")))
        .relative()
        .max_w(ui_rems(240.0))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, _| {
                this.menu
                    .note_trigger_press_matching(|menu| *menu == BoardMenu::Project)
            }),
        )
        .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
            if this.menu.take_press_was_open() {
                return;
            }
            this.open_menu(BoardMenu::Project, event.is_keyboard(), cx);
        }))
        .child(crate::button::glyph(icons::FOLDER, ControlSize::Md))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(SharedString::from(label)),
        )
        .child(crate::button::select_chevron(theme));
        if open {
            let entries = self.menu_entries(&BoardMenu::Project, cx);
            let closing = self.menu.closing_since();
            let card = self.render_menu_card(entries, 260.0, theme, cx);
            trigger = trigger.child(crate::popover::anchored_menu_below(
                "board-project-menu",
                card.into_any_element(),
                closing,
            ));
        }
        trigger.into_any_element()
    }

    fn render_view_switch(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let selected = match self.effective_view() {
            BoardViewMode::Board => 0,
            BoardViewMode::List => 1,
        };
        let narrow = self.narrow;
        let switch = crate::button::segmented(
            "board-view",
            vec![
                Segment::new("board", "Board").glyph(icons::KANBAN),
                Segment::new("list", "List").glyph(icons::LIST),
            ],
            selected,
            150.0,
            ControlSize::Md,
            theme,
            cx.listener(|this, ix: &usize, _, cx| {
                let view = if *ix == 0 {
                    BoardViewMode::Board
                } else {
                    BoardViewMode::List
                };
                this.set_view(view, cx);
            }),
        );
        if narrow {
            switch
                .tooltip(crate::popover::text_tooltip(
                    "The window is narrow, so the Board shows a list",
                ))
                .tooltip_show_delay(crate::popover::TOOLTIP_DELAY)
                .into_any_element()
        } else {
            switch.into_any_element()
        }
    }

    fn render_search(&mut self, theme: &Theme) -> AnyElement {
        div()
            .id("board-search")
            .role(gpui::Role::Search)
            .h(ui_rems(ControlSize::Md.height()))
            .w(ui_rems(220.0))
            .min_w(ui_rems(140.0))
            .flex_shrink(1.0)
            .flex()
            .flex_row()
            .items_center()
            .gap(ui_rems(6.0))
            .px(ui_rems(8.0))
            .rounded(px(ControlSize::Md.radius()))
            .border_1()
            .border_color(theme.separator(true))
            .bg(theme.fill(crate::theme::FillTier::Quaternary))
            .overflow_hidden()
            .child(
                icons::icon(icons::MAGNIFER)
                    .size(ui_rems(13.0))
                    .flex_none()
                    .text_color(theme.readable_muted(0.8)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui_rems(12.0))
                    .child(self.search.clone()),
            )
            .into_any_element()
    }

    fn render_filters_select(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active_filter_count(cx);
        let label = if active == 0 {
            "Filters".to_owned()
        } else {
            format!("Filters · {active}")
        };
        let open = self.menu.get() == Some(&BoardMenu::Filters);
        let mut trigger = crate::button::select_frame(
            "board-filters",
            ControlSize::Md,
            open && self.menu.is_open(),
            true,
            theme,
        )
        .aria_label(SharedString::from(if active == 0 {
            "Filters: provider and source".to_owned()
        } else {
            format!("Filters: {active} active")
        }))
        .relative()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, _| {
                this.menu
                    .note_trigger_press_matching(|menu| *menu == BoardMenu::Filters)
            }),
        )
        .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
            if this.menu.take_press_was_open() {
                return;
            }
            this.open_menu(BoardMenu::Filters, event.is_keyboard(), cx);
        }))
        .child(crate::button::glyph(icons::FILTER, ControlSize::Md))
        .child(div().flex_none().child(SharedString::from(label)))
        .child(crate::button::select_chevron(theme));
        if open {
            let entries = self.menu_entries(&BoardMenu::Filters, cx);
            let closing = self.menu.closing_since();
            let card = self.render_menu_card(entries, 240.0, theme, cx);
            trigger = trigger.child(crate::popover::anchored_menu_below(
                "board-filters-menu",
                card.into_any_element(),
                closing,
            ));
        }
        trigger.into_any_element()
    }

    fn render_github_button(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let menu = self.menu.get().cloned();
        let choosing = matches!(menu, Some(BoardMenu::ChooseProject(ProjectIntent::GitHub)));
        let mut button = crate::button::button(
            "board-github",
            ButtonKind::Secondary,
            ControlSize::Md,
            true,
            theme,
        )
        .aria_label("From GitHub issue")
        .relative()
        .gap(ui_rems(6.0))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, _| {
                this.menu.note_trigger_press_matching(|menu| {
                    *menu == BoardMenu::ChooseProject(ProjectIntent::GitHub)
                })
            }),
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if this.menu.take_press_was_open() {
                return;
            }
            this.open_github(None, cx);
        }))
        .child(crate::button::glyph(icons::INBOX_IN, ControlSize::Md))
        .child("From GitHub issue");
        if choosing && let Some(menu) = menu {
            let entries = self.menu_entries(&menu, cx);
            let closing = self.menu.closing_since();
            let card = self.render_menu_card(entries, 240.0, theme, cx);
            button = button.child(crate::popover::anchored_menu_below(
                "board-choose-project-menu",
                card.into_any_element(),
                closing,
            ));
        }
        button.into_any_element()
    }

    fn render_new_issue_button(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let menu = self.menu.get().cloned();
        // Add existing chat lists its chats here.
        let anchored = matches!(menu, Some(BoardMenu::AddChat { .. }));
        let mut button = crate::button::button(
            "board-new-issue",
            ButtonKind::Primary,
            ControlSize::Md,
            true,
            theme,
        )
        .aria_label("New issue")
        .relative()
        .gap(ui_rems(6.0))
        .on_click(cx.listener(|this, _, window, cx| {
            let project_id = this.scope(cx).project_id().map(str::to_owned);
            this.open_new_ticket(project_id, window, cx);
        }))
        .child(crate::button::glyph(icons::PLUS, ControlSize::Md).text_color(theme.on_solid))
        .child("New issue");
        if anchored && let Some(menu) = menu {
            let entries = self.menu_entries(&menu, cx);
            let closing = self.menu.closing_since();
            let card = self.render_menu_card(entries, 280.0, theme, cx);
            button = button.child(crate::popover::anchored_menu_below(
                "board-create-menu",
                card.into_any_element(),
                closing,
            ));
        }
        button.into_any_element()
    }

    pub(super) fn project_menu_entries(&self, cx: &gpui::App) -> Vec<MenuEntry> {
        let scope = self.scope(cx);
        let summaries = self.model.read(cx).projects().to_vec();
        let counts = |project_id: &str| {
            summaries
                .iter()
                .find(|summary| summary.project_id == project_id)
                .map(|summary| summary.backlog + summary.ready + summary.done)
        };
        let mut all = MenuEntry::new(
            "scope-all",
            "All projects",
            MenuAction::Scope(BoardScope::AllProjects),
        );
        all.checked = scope == BoardScope::AllProjects;
        all.glyph = Some(icons::LAYERS_MINIMALISTIC);
        all.heading = Some("Board scope");
        let mut entries = vec![all];
        for (ix, (id, name)) in self.local_projects(cx).into_iter().enumerate() {
            let mut entry = MenuEntry::new(
                format!("scope-{id}"),
                name,
                MenuAction::Scope(BoardScope::Project {
                    project_id: id.clone(),
                }),
            );
            entry.checked = scope.project_id() == Some(id.as_str());
            entry.glyph = Some(icons::FOLDER);
            entry.hint = counts(&id).map(|count| count.to_string());
            if ix == 0 {
                entry.heading = Some("Projects");
            }
            entries.push(entry);
        }
        entries
    }

    pub(super) fn filter_menu_entries(&self, cx: &gpui::App) -> Vec<MenuEntry> {
        let filters = self.filters(cx);
        let mut entries = Vec::new();
        let mut attention = MenuEntry::new(
            "filter-attention",
            "Needs attention only",
            MenuAction::ToggleAttention,
        );
        attention.checked = filters.attention_only;
        attention.heading = Some("Attention");
        entries.push(attention);
        let mut any = MenuEntry::new(
            "filter-provider-any",
            "Any provider",
            MenuAction::Provider(None),
        );
        any.checked = filters.provider.is_none();
        any.heading = Some("Provider");
        entries.push(any);
        for harness in PROVIDERS {
            let mut entry = MenuEntry::new(
                format!("filter-provider-{harness:?}").to_lowercase(),
                super::super::child_chats::provider_label(harness),
                MenuAction::Provider(Some(harness)),
            );
            entry.checked = filters.provider == Some(harness);
            entry.glyph = Some(crate::pickers::harness_brand_icon(harness).0);
            entries.push(entry);
        }
        let mut any = MenuEntry::new("filter-source-any", "Any source", MenuAction::Source(None));
        any.checked = filters.source.is_none();
        any.heading = Some("Source");
        entries.push(any);
        for (source, label) in SOURCES {
            let mut entry = MenuEntry::new(
                format!("filter-source-{source:?}").to_lowercase(),
                label,
                MenuAction::Source(Some(source)),
            );
            entry.checked = filters.source == Some(source);
            entries.push(entry);
        }
        let mut archived = MenuEntry::new(
            "filter-archived",
            "Show archived tickets",
            MenuAction::ToggleArchived,
        );
        archived.checked = self.show_archived;
        archived.heading = Some("Archive");
        entries.push(archived);
        let mut clear = MenuEntry::new(
            "filter-clear",
            "Clear all filters",
            MenuAction::ClearFilters,
        );
        clear.enabled = self.any_filter(cx);
        entries.push(clear);
        entries
    }
}
