//! Project-aware command, conversation, and recorded-content search.
use super::*;
use crate::appearance::AppearanceMode;

const CHAT_RESULT_LIMIT: usize = 30;
const RESULTS_FADE_BAND: f32 = 18.0;

pub(super) struct CommandPalette {
    pub(super) search: Entity<ComposerInput>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    pub(super) active: usize,
    selection: PaletteSelection,
    enter_press: EnterPress,
    // Claim focus during mount so the shell does not restore the composer
    // while this input is still absent from the dispatch tree.
    focus_pending: bool,
    scroll: gpui::ScrollHandle,
    _search_events: Subscription,
}

// X11 suppresses synthetic repeat releases but sends repeated keydowns with
// is_held=false. Keep our own latch until the physical key is released.
#[derive(Default)]
struct EnterPress {
    down: bool,
}

impl EnterPress {
    fn press(&mut self, is_held: bool) -> bool {
        let was_down = std::mem::replace(&mut self.down, true);
        !was_down && !is_held
    }

    fn release(&mut self) {
        self.down = false;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResultGroup {
    CurrentChats,
    CurrentContent,
    AllContent,
    OtherChats,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EntryIdentity {
    NewChat,
    NewProject,
    Settings,
    Activity,
    Theme,
    SearchAllContent,
    SearchCurrentContent,
    Chat(String),
    History(String),
    LoadOlder(ResultGroup),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PaletteSelection {
    Default,
    Bound {
        identity: EntryIdentity,
        owner_task: Option<String>,
    },
    Cleared,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Entry {
    NewChat,
    NewProject,
    Settings,
    Activity,
    Theme(AppearanceMode),
    SearchAllContent,
    SearchCurrentContent,
    Chat {
        id: String,
        group: ResultGroup,
    },
    History {
        event_id: String,
        group: ResultGroup,
    },
    LoadOlder(ResultGroup),
}

impl Entry {
    fn is_ordinary_action(&self) -> bool {
        matches!(
            self,
            Self::NewChat | Self::NewProject | Self::Settings | Self::Activity | Self::Theme(_)
        )
    }

    fn is_search_result(&self) -> bool {
        matches!(self, Self::Chat { .. } | Self::History { .. })
    }

    fn identity(&self) -> EntryIdentity {
        match self {
            Self::NewChat => EntryIdentity::NewChat,
            Self::NewProject => EntryIdentity::NewProject,
            Self::Settings => EntryIdentity::Settings,
            Self::Activity => EntryIdentity::Activity,
            Self::Theme(_) => EntryIdentity::Theme,
            Self::SearchAllContent => EntryIdentity::SearchAllContent,
            Self::SearchCurrentContent => EntryIdentity::SearchCurrentContent,
            Self::Chat { id, .. } => EntryIdentity::Chat(id.clone()),
            Self::History { event_id, .. } => EntryIdentity::History(event_id.clone()),
            Self::LoadOlder(group) => EntryIdentity::LoadOlder(*group),
        }
    }

    fn action(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::NewChat => Some(("New chat", icons::PEN_NEW_SQUARE)),
            Self::NewProject => Some(("New project", icons::FOLDER)),
            Self::Settings => Some(("Open settings", icons::SETTINGS_MINIMALISTIC)),
            Self::Activity => Some(("Toggle activity view", icons::BELL)),
            Self::Theme(mode) => Some((
                match mode {
                    AppearanceMode::System => "Switch to system theme",
                    AppearanceMode::Light => "Switch to light theme",
                    AppearanceMode::Dark => "Switch to dark theme",
                },
                mode.icon(),
            )),
            Self::SearchAllContent => Some(("Search content in all projects", icons::GLOBE)),
            Self::SearchCurrentContent => {
                Some(("Search content in current project", icons::FOLDER))
            }
            Self::Chat { .. } | Self::History { .. } | Self::LoadOlder(_) => None,
        }
    }

    fn group(&self) -> Option<ResultGroup> {
        match self {
            Self::Chat { group, .. } | Self::History { group, .. } => Some(*group),
            Self::LoadOlder(group) => Some(*group),
            _ => None,
        }
    }
}

fn matches_query(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query.split_whitespace().all(|word| text.contains(word))
}

fn actions_for(query: &str, is_dark: bool) -> Vec<Entry> {
    [
        Entry::NewChat,
        Entry::NewProject,
        Entry::Settings,
        Entry::Activity,
        Entry::Theme(if is_dark {
            AppearanceMode::Light
        } else {
            AppearanceMode::Dark
        }),
    ]
    .into_iter()
    .filter(|entry| matches_query(query, entry.action().unwrap().0))
    .collect()
}

impl Shell {
    pub(super) fn reset_command_palette_key_state(&mut self) {
        if let Some(palette) = self.command_palette.as_mut() {
            palette.enter_press.release();
        }
    }

    pub(super) fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_some() {
            self.close_command_palette(window, cx);
            return;
        }
        self.add_space = None;
        let search = cx.new(|cx| {
            ComposerInput::with_context("Search commands, chats, and content…", "PaletteSearch", cx)
        });
        let events = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(palette) = this.command_palette.as_mut() {
                    palette.active = 0;
                    palette.selection = PaletteSelection::Default;
                    palette.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                }
                this.begin_session_history_search(cx);
                cx.notify();
            }
        });
        let previous_focus = window.focused(cx);
        self.command_palette = Some(CommandPalette {
            search,
            focus: cx.focus_handle(),
            previous_focus,
            active: 0,
            selection: PaletteSelection::Default,
            enter_press: EnterPress::default(),
            focus_pending: true,
            scroll: gpui::ScrollHandle::new(),
            _search_events: events,
        });
        cx.notify();
    }

    pub(super) fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(palette) = self.command_palette.take() {
            if let Some(focus) = palette.previous_focus {
                window.focus(&focus, cx);
            }
            self.reset_session_history_search(cx);
            cx.notify();
        }
    }

    pub(super) fn dismiss_command_palette(&mut self, cx: &mut Context<Self>) {
        if self.command_palette.take().is_some() {
            self.reset_session_history_search(cx);
        }
    }

    pub(super) fn command_palette_query(&self, cx: &App) -> Option<String> {
        self.command_palette
            .as_ref()
            .map(|palette| palette.search.read(cx).text().trim().to_string())
            .filter(|query| !query.is_empty())
    }

    pub(super) fn command_entries(&self, cx: &App) -> Vec<Entry> {
        let Some(palette) = &self.command_palette else {
            return Vec::new();
        };
        let query = palette.search.read(cx).text().trim().to_lowercase();
        let mut entries = actions_for(&query, Theme::of(cx).appearance.is_dark());
        if !query.is_empty() {
            entries.push(if self.history_search_global {
                Entry::SearchCurrentContent
            } else {
                Entry::SearchAllContent
            });
        }
        let state = self.state.read(cx);
        let current_project = match self.settings.space_filter.as_deref() {
            Some(crate::settings::PERSONAL_SPACE_FILTER) => None,
            Some(space_id) => Some(space_id.to_string()),
            None => state.selected_space.clone(),
        };
        let content_tasks = self
            .history_search_results
            .iter()
            .map(|event| event.task_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        // Metadata search deliberately includes archived chats and ignores the
        // sidebar's collapsed groups. Scope only controls which stable section
        // receives the match.
        let mut chats: Vec<_> = state
            .chats
            .iter()
            .filter(|chat| {
                let project = state
                    .space_for_chat(chat)
                    .map(|s| s.display_name())
                    .unwrap_or("~");
                let device = state.device_name(&chat.device_id).unwrap_or("");
                let branch =
                    crate::change_requests::conversation_branch(chat, &state.spaces).unwrap_or("");
                let pr = state
                    .change_request_for_chat(chat)
                    .map(|pr| {
                        format!(
                            "#{} {} {} {}",
                            pr.number, pr.title, pr.head_ref, pr.base_ref
                        )
                    })
                    .unwrap_or_default();
                matches_query(
                    &query,
                    &format!(
                        "{} {project} {device} {branch} {pr}",
                        chat.title.as_deref().unwrap_or("New session")
                    ),
                )
            })
            .collect();
        chats.sort_by(|a, b| spaces::compare_sidebar_chats(self.settings.sidebar_sort, a, b));
        let (current_chats, other_chats): (Vec<_>, Vec<_>) = chats
            .into_iter()
            .partition(|chat| chat.space_id == current_project);
        // A matching indexed event is the more precise canonical row for its
        // task. Suppress the parallel title row while retaining every useful
        // event and its continuation.
        entries.extend(
            current_chats
                .into_iter()
                .filter(|chat| !content_tasks.contains(chat.id.as_str()))
                .take(CHAT_RESULT_LIMIT)
                .map(|chat| Entry::Chat {
                    id: chat.id.clone(),
                    group: ResultGroup::CurrentChats,
                }),
        );
        let history_group = if self
            .history_search_scope_key
            .as_ref()
            .is_some_and(|scope| scope.scope == HistorySearchScope::Global)
            || (self.history_search_scope_key.is_none() && self.history_search_global)
        {
            ResultGroup::AllContent
        } else {
            ResultGroup::CurrentContent
        };
        entries.extend(
            self.history_search_results
                .iter()
                .map(|event| Entry::History {
                    event_id: event.event_id.clone(),
                    group: history_group,
                }),
        );
        if self.history_search_next_cursor.is_some() {
            entries.push(Entry::LoadOlder(history_group));
        }
        if !query.is_empty() {
            entries.extend(
                other_chats
                    .into_iter()
                    .filter(|chat| !content_tasks.contains(chat.id.as_str()))
                    .take(CHAT_RESULT_LIMIT)
                    .map(|chat| Entry::Chat {
                        id: chat.id.clone(),
                        group: ResultGroup::OtherChats,
                    }),
            );
        }
        entries
    }

    fn command_entry_owner(&self, entry: &Entry) -> Option<String> {
        match entry {
            Entry::Chat { id, .. } => Some(id.clone()),
            Entry::History { event_id, .. } => self
                .history_search_results
                .iter()
                .find(|event| &event.event_id == event_id)
                .map(|event| event.task_id.clone()),
            _ => None,
        }
    }

    fn command_selection_for(&self, entry: &Entry) -> PaletteSelection {
        PaletteSelection::Bound {
            identity: entry.identity(),
            owner_task: self.command_entry_owner(entry),
        }
    }

    fn command_default_index(entries: &[Entry]) -> Option<usize> {
        entries
            .iter()
            .position(Entry::is_ordinary_action)
            .or_else(|| entries.iter().position(Entry::is_search_result))
    }

    fn command_selected_index(&self, entries: &[Entry]) -> Option<usize> {
        let palette = self.command_palette.as_ref()?;
        match &palette.selection {
            PaletteSelection::Default => Self::command_default_index(entries),
            PaletteSelection::Bound { .. } => Some(palette.active),
            PaletteSelection::Cleared => None,
        }
    }

    fn reconcile_command_palette_selection(&mut self, entries: &[Entry]) {
        let Some(selection) = self
            .command_palette
            .as_ref()
            .map(|palette| palette.selection.clone())
        else {
            return;
        };
        let resolution = match selection {
            PaletteSelection::Default => Some((
                Self::command_default_index(entries),
                PaletteSelection::Default,
            )),
            PaletteSelection::Cleared => Some((None, PaletteSelection::Cleared)),
            PaletteSelection::Bound {
                identity,
                owner_task,
            } => {
                if let Some(index) = entries
                    .iter()
                    .position(|entry| entry.identity() == identity)
                {
                    Some((
                        Some(index),
                        PaletteSelection::Bound {
                            identity,
                            owner_task,
                        },
                    ))
                } else if let Some(owner_task) = owner_task {
                    entries
                        .iter()
                        .position(|entry| {
                            self.command_entry_owner(entry).as_deref() == Some(owner_task.as_str())
                        })
                        .map(|index| (Some(index), self.command_selection_for(&entries[index])))
                        .or(Some((None, PaletteSelection::Cleared)))
                } else {
                    Some((None, PaletteSelection::Cleared))
                }
            }
        };
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        if let Some((index, selection)) = resolution {
            if let Some(index) = index {
                palette.active = index;
            }
            palette.selection = selection;
        }
    }

    fn activate_command(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        match entry {
            Entry::Theme(mode) => {
                // Keep the palette open so this ordinary action updates to its next state.
                crate::appearance::set_mode(mode, cx);
                cx.notify();
                return;
            }
            Entry::SearchAllContent => {
                self.history_search_global = true;
                if let Some(palette) = self.command_palette.as_mut() {
                    palette.active = 0;
                    palette.selection = PaletteSelection::Default;
                    palette.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                }
                self.begin_session_history_search(cx);
                return;
            }
            Entry::SearchCurrentContent => {
                self.history_search_global = false;
                if let Some(palette) = self.command_palette.as_mut() {
                    palette.active = 0;
                    palette.selection = PaletteSelection::Default;
                    palette.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                }
                self.begin_session_history_search(cx);
                return;
            }
            Entry::LoadOlder(_) => {
                self.load_more_session_history(cx);
                return;
            }
            _ => {}
        }
        let history_event = if let Entry::History { ref event_id, .. } = entry {
            self.history_search_results
                .iter()
                .find(|event| &event.event_id == event_id)
                .cloned()
        } else {
            None
        };
        self.close_command_palette(window, cx);
        match entry {
            Entry::NewChat => self.open_new_session(cx),
            Entry::NewProject => self.open_add_space(cx),
            Entry::Settings => self.open_settings(SettingsSection::DEFAULT, cx),
            Entry::Activity => self.toggle_activity(window, cx),
            Entry::Theme(_) => unreachable!(),
            Entry::Chat { id, .. } => self.open_chat(id, cx),
            Entry::History { .. } => {
                if let Some(event) = history_event {
                    self.open_session_history_result(event, cx);
                }
            }
            Entry::SearchAllContent | Entry::SearchCurrentContent | Entry::LoadOlder(_) => {
                unreachable!()
            }
        }
    }

    fn command_group_label(&self, group: ResultGroup, cx: &Context<Self>) -> SharedString {
        let state = self.state.read(cx);
        let label_for = |project_id: Option<&str>| -> String {
            project_id
                .and_then(|id| state.spaces.iter().find(|space| space.id == id))
                .map(|space| space.display_name().to_string())
                .unwrap_or_else(|| {
                    if project_id.is_none() {
                        "Personal".into()
                    } else {
                        "Current project".into()
                    }
                })
        };
        let label = match group {
            ResultGroup::CurrentChats => {
                let project_id = match self.settings.space_filter.as_deref() {
                    Some(crate::settings::PERSONAL_SPACE_FILTER) => None,
                    Some(space_id) => Some(space_id),
                    None => state.selected_space.as_deref(),
                };
                format!("{} · Chats", label_for(project_id))
            }
            ResultGroup::CurrentContent => {
                let project_id = self
                    .current_session_history_request(None, cx)
                    .and_then(|request| request.project_id);
                format!("{} · Recorded content", label_for(project_id.as_deref()))
            }
            ResultGroup::AllContent => "All projects · Recorded content".into(),
            ResultGroup::OtherChats => "Other projects · Matching chats".into(),
        };
        label.into()
    }

    fn render_command_history_row(
        &self,
        event_id: &str,
        group: ResultGroup,
        active: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let event = self
            .history_search_results
            .iter()
            .find(|event| event.event_id == event_id)?
            .clone();
        let project = {
            let state = self.state.read(cx);
            event
                .project_id
                .as_ref()
                .and_then(|id| state.spaces.iter().find(|space| &space.id == id))
                .map(|space| space.display_name().to_string())
                .unwrap_or_else(|| "Personal".into())
        };
        let outcome = match event.outcome {
            ActivityOutcome::Pending => "Pending",
            ActivityOutcome::Succeeded => "Done",
            ActivityOutcome::Failed => "Failed",
            ActivityOutcome::Interrupted => "Interrupted",
            ActivityOutcome::Unknown => "Unknown",
        };
        let metadata = if event.source.available_here {
            format!(
                "Recorded content · {project} · {} · {outcome}",
                event.model.as_deref().unwrap_or("Unknown model")
            )
        } else {
            format!("Recorded content · {project} · Other device")
        };
        let entry = Entry::History {
            event_id: event.event_id.clone(),
            group,
        };
        Some(
            popover::menu_row(theme, active, format!("command-history-{}", event.event_id))
                .id(SharedString::from(format!(
                    "command-history-{}",
                    event.event_id
                )))
                .rounded(px(popover::PALETTE_ITEM_RADIUS))
                .role(gpui::Role::Button)
                .aria_label(event.summary.clone())
                .min_h(px(42.0))
                .py(px(5.0))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.activate_command(entry.clone(), window, cx)
                }))
                .child(
                    icon(icons::LIST)
                        .size(px(16.0))
                        .flex_none()
                        .text_color(theme.text_muted),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .truncate()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text)
                                .child(SharedString::from(transcript::single_line(&event.summary))),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(crate::typography::ui_rems(9.5))
                                .text_color(theme.text_faint)
                                .child(SharedString::from(metadata)),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn command_palette_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match event.keystroke.key.as_str() {
            "up" | "down" => {
                let entries = self.command_entries(cx);
                self.reconcile_command_palette_selection(&entries);
                if !entries.is_empty() {
                    let next = self.command_selected_index(&entries).map_or_else(
                        || {
                            if event.keystroke.key == "down" {
                                0
                            } else {
                                entries.len() - 1
                            }
                        },
                        |active| {
                            if event.keystroke.key == "down" {
                                (active + 1) % entries.len()
                            } else {
                                (active + entries.len() - 1) % entries.len()
                            }
                        },
                    );
                    let selection = self.command_selection_for(&entries[next]);
                    if let Some(palette) = self.command_palette.as_mut() {
                        palette.active = next;
                        palette.selection = selection;
                        palette.scroll.scroll_to_item(next);
                    }
                    cx.notify();
                }
            }
            "enter" => {
                let activate = self
                    .command_palette
                    .as_mut()
                    .is_some_and(|palette| palette.enter_press.press(event.is_held));
                if activate {
                    let entries = self.command_entries(cx);
                    self.reconcile_command_palette_selection(&entries);
                    if let Some(entry) = self
                        .command_selected_index(&entries)
                        .and_then(|index| entries.get(index))
                        .cloned()
                    {
                        self.activate_command(entry, window, cx);
                    }
                }
            }
            "escape" => self.close_command_palette(window, cx),
            _ => return false,
        }
        true
    }

    pub(super) fn render_command_palette(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let entries = self.command_entries(cx);
        self.reconcile_command_palette_selection(&entries);
        let palette = self.command_palette.as_mut()?;
        if std::mem::take(&mut palette.focus_pending) {
            window.focus(&palette.search.focus_handle(cx), cx);
        }
        let active = match &palette.selection {
            PaletteSelection::Default => Self::command_default_index(&entries),
            PaletteSelection::Bound { .. } => Some(palette.active),
            PaletteSelection::Cleared => None,
        };
        let search = palette.search.clone();
        let query = search.read(cx).text().to_string();
        let focus = palette.focus.clone();
        let scroll = palette.scroll.clone();
        let theme = Theme::of(cx).for_popup();
        let mut rows = Vec::new();
        let mut previous_group = None;
        for (ix, entry) in entries.iter().enumerate() {
            // End spacing belongs to the content, so it scrolls out of the
            // fade instead of leaving a permanent gutter beside the chrome.
            let mut row = div()
                .id(("command-result", ix))
                .flex_none()
                .when(ix == 0, |row| row.pt(px(8.0)))
                .when(ix + 1 == entries.len(), |row| row.pb(px(8.0)));
            if let Some(group) = entry.group()
                && previous_group != Some(group)
            {
                if ix > 0 {
                    row = row.child(spaces::sidebar_separator(&theme).w_full().my(px(7.0)));
                }
                row = row.child(
                    div()
                        .id(SharedString::from(format!("command-section-{group:?}")))
                        .px(px(10.0))
                        .pb(px(4.0))
                        .text_size(crate::typography::ui_rems(9.5))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text_faint)
                        .child(self.command_group_label(group, cx)),
                );
                previous_group = Some(group);
            }
            let content = if let Some((label, glyph)) = entry.action() {
                let shortcut = match entry {
                    Entry::NewChat | Entry::NewProject => {
                        let id = if *entry == Entry::NewChat {
                            ShortcutId::NewSession
                        } else {
                            ShortcutId::NewProject
                        };
                        let combo = self.settings.keymap.get(id);
                        let valid = Keystroke::parse(&platform_combo(combo)).is_ok();
                        Some(crate::settings::badge_combo(if valid {
                            combo
                        } else {
                            id.default_combo()
                        }))
                    }
                    Entry::Settings => Some(crate::settings::badge_combo("mod-,")),
                    Entry::Activity => {
                        let combo = self.settings.keymap.get(ShortcutId::OpenActivity);
                        (!combo.is_empty()).then(|| crate::settings::badge_combo(combo))
                    }
                    _ => None,
                };
                let entry = entry.clone();
                popover::menu_row(&theme, active == Some(ix), format!("command-action-{ix}"))
                    .id(("command-action", ix))
                    .rounded(px(popover::PALETTE_ITEM_RADIUS))
                    .role(gpui::Role::Button)
                    .aria_label(label)
                    .min_h(px(30.0))
                    .py(px(4.0))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_command(entry.clone(), window, cx)
                    }))
                    .child(
                        icon(glyph)
                            .size(px(16.0))
                            .flex_none()
                            .text_color(theme.text_muted),
                    )
                    .child(div().flex_1().min_w_0().child(popover::search_highlight(
                        label.into(),
                        Some(&query),
                        &theme,
                    )))
                    .when_some(shortcut, |row, shortcut| {
                        row.child(popover::kbd_hint(&theme, &shortcut))
                    })
                    .into_any_element()
            } else if let Entry::Chat { id, .. } = entry {
                let state = self.state.read(cx);
                let chat = state.chats.iter().find(|chat| &chat.id == id)?;
                let project = match (state.space_for_chat(chat), chat.space_id.as_deref()) {
                    (Some(space), _) => space.display_name(),
                    (None, None) => "~",
                    _ => "?",
                };
                let folder = match state.device_name(&chat.device_id) {
                    Some(device) => format!("{project} @ {device}"),
                    None => project.to_string(),
                };
                let branch = self
                    .settings
                    .sidebar_show_branch
                    .then(|| crate::change_requests::conversation_branch(chat, &state.spaces))
                    .flatten()
                    .map(str::trim)
                    .filter(|branch| !branch.is_empty())
                    .map(SharedString::from);
                let pr = self
                    .settings
                    .sidebar_show_pull_request
                    .then(|| state.change_request_for_chat(chat).cloned())
                    .flatten();
                let harness = self
                    .settings
                    .sidebar_show_harness
                    .then(|| chat.config.as_ref().map(|c| c.harness))
                    .flatten();
                self.render_chat_row(
                    id.clone(),
                    transcript::single_line(chat.title.as_deref().unwrap_or("New session")).into(),
                    format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), Utc::now())
                        .into(),
                    folder.into(),
                    branch,
                    pr,
                    harness,
                    state.display_status_for(chat, Utc::now()),
                    active == Some(ix),
                    chat.archived,
                    None,
                    Some(&query),
                    &theme,
                    cx,
                )
            } else if let Entry::History { event_id, group } = entry {
                self.render_command_history_row(event_id, *group, active == Some(ix), &theme, cx)?
            } else if let Entry::LoadOlder(group) = entry {
                let entry = Entry::LoadOlder(*group);
                popover::menu_row(&theme, active == Some(ix), "command-history-load-older")
                    .id("command-history-load-older")
                    .rounded(px(popover::PALETTE_ITEM_RADIUS))
                    .role(gpui::Role::Button)
                    .aria_label("Load older recorded content")
                    .min_h(px(30.0))
                    .justify_center()
                    .text_color(theme.text_muted)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_command(entry.clone(), window, cx)
                    }))
                    .child(if self.history_search_loading {
                        "Loading older…"
                    } else {
                        "Load older"
                    })
                    .into_any_element()
            } else {
                unreachable!()
            };
            rows.push(row.child(div().px(px(8.0)).child(content)));
        }
        let has_search_results = entries
            .iter()
            .any(|entry| matches!(entry, Entry::Chat { .. } | Entry::History { .. }));
        let history_status: Option<(SharedString, bool)> = if !query.trim().is_empty() {
            if self.history_search_loading {
                Some(("Searching recorded content…".into(), false))
            } else if let Some(error) = self.history_search_error.clone() {
                Some((
                    format!("Recorded content unavailable · {error}").into(),
                    true,
                ))
            } else if !has_search_results {
                Some((
                    "No matching chats or recorded content in this scope".into(),
                    false,
                ))
            } else {
                None
            }
        } else {
            None
        };
        let height = (f32::from(viewport.height) - 180.0).clamp(100.0, 360.0);
        let body = div()
            .id("command-results")
            .min_h_0()
            .max_h(px(height))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .gap(px(SIDEBAR_LIST_GAP))
            .children(rows)
            .when_some(history_status, |el, (status, danger)| {
                el.child(
                    div()
                        .id("command-history-status")
                        .px(px(16.0))
                        .py(px(8.0))
                        .text_size(crate::typography::ui_rems(10.0))
                        .text_color(if danger {
                            theme.danger
                        } else {
                            theme.text_faint
                        })
                        .child(status),
                )
            })
            .when(entries.is_empty(), |el| {
                el.child(
                    div()
                        .w_full()
                        .py(px(24.0))
                        .px(px(16.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(crate::typography::ui_rems(13.0))
                        .child("No results")
                        .child(
                            div()
                                .text_color(theme.text_muted)
                                .child("Try a command, chat title, project, or device."),
                        ),
                )
            });
        let body = crate::edge_fade::edge_faded(RESULTS_FADE_BAND, true, true, body)
            .fade_overflow_y(&scroll);
        let card = div()
            .id("command-palette")
            .track_focus(&focus)
            .w(px(560.0_f32.min(f32::from(viewport.width) - 32.0)))
            .flex()
            .flex_col()
            .rounded(px(16.0))
            .border_1()
            .border_color(theme.border)
            .when(!theme.is_frost(), |el| el.shadow_lg())
            .bg(popover::surface_bg(&theme))
            .text_color(theme.text)
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if this.command_palette_key_down(event, window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .on_key_up(cx.listener(|this, event: &gpui::KeyUpEvent, _, cx| {
                if event.keystroke.key == "enter" {
                    if let Some(palette) = this.command_palette.as_mut() {
                        palette.enter_press.release();
                    }
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.close_command_palette(window, cx)),
            )
            .child(
                div()
                    .min_h(px(44.0))
                    .flex_none()
                    .px(px(16.0))
                    .py(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .border_b_1()
                    .border_color(crate::theme::hairline(0.06))
                    .child(popover::palette_search_icon(&theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(crate::typography::ui_rems(14.0))
                            .child(search),
                    )
                    .child(popover::kbd_hint(
                        &theme,
                        &crate::settings::badge_combo("mod-k"),
                    )),
            )
            .child(body)
            .child(
                div()
                    .flex_none()
                    .px(px(16.0))
                    .py(px(7.0))
                    .border_t_1()
                    .border_color(crate::theme::hairline(0.06))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(12.0))
                    .child(command_key_hint(&theme, "↑ ↓", "Navigate"))
                    .child(command_key_hint(&theme, "↵", "Select"))
                    .child(command_key_hint(&theme, "Esc", "Close")),
            );
        // Match the composer's 16px backdrop blur, including its opaque fallback.
        let card = crate::frost::frosted(16.0, crate::frost::MENU_BLUR, card);
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(px(0.0), px(0.0)))
                    .child(
                        div()
                            .occlude()
                            .w(viewport.width)
                            .h(viewport.height)
                            // Match glass modals: quiet the background while
                            // preserving its color through the frosted palette.
                            .bg(popover::scrim_alpha(0.35))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(card),
                    ),
            )
            .priority(2)
            .into_any_element(),
        )
    }
}

fn command_key_hint(theme: &Theme, keys: &str, label: &'static str) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(5.0))
        .child(popover::kbd_hint(theme, keys))
        .child(
            div()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_muted)
                .child(label),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::AppearanceMode;

    #[test]
    fn x11_unflagged_enter_repeats_activate_once_until_release() {
        let mut enter = EnterPress::default();
        // The pinned X11 backend drops synthetic repeat releases and emits
        // every repeated KeyDownEvent with is_held=false.
        assert!(enter.press(false));
        for _ in 0..35 {
            assert!(!enter.press(false));
        }
        enter.release();
        assert!(enter.press(false));
    }

    #[test]
    fn flagged_enter_repeats_do_not_activate() {
        let mut enter = EnterPress::default();
        assert!(!enter.press(true));
        assert!(!enter.press(false));
        enter.release();
        assert!(enter.press(false));
        assert!(!enter.press(true));
    }

    #[test]
    fn action_search_hides_empty_section_and_preserves_order() {
        assert_eq!(
            actions_for("", true),
            vec![
                Entry::NewChat,
                Entry::NewProject,
                Entry::Settings,
                Entry::Theme(AppearanceMode::Light)
            ]
        );
        assert_eq!(
            actions_for("new", true),
            vec![Entry::NewChat, Entry::NewProject]
        );
        assert_eq!(actions_for("settings", true), vec![Entry::Settings]);
        assert_eq!(
            actions_for("theme", true),
            vec![Entry::Theme(AppearanceMode::Light)]
        );
        assert!(actions_for("deployment", true).is_empty());
    }

    #[test]
    fn theme_action_targets_the_opposite_resolved_appearance() {
        assert_eq!(
            actions_for("theme", true),
            vec![Entry::Theme(AppearanceMode::Light)]
        );
        assert_eq!(
            actions_for("theme", false),
            vec![Entry::Theme(AppearanceMode::Dark)]
        );
        assert_eq!(
            actions_for("light", true),
            vec![Entry::Theme(AppearanceMode::Light)]
        );
        assert_eq!(
            actions_for("dark", false),
            vec![Entry::Theme(AppearanceMode::Dark)]
        );
    }

    #[test]
    fn search_matches_words_across_chat_metadata() {
        assert!(matches_query(
            "mac auth",
            "Fix authentication Luna @ MacBook main"
        ));
        assert!(matches_query("  ", "Any chat"));
        assert!(!matches_query("mac windows", "Luna @ MacBook"));
    }
}
