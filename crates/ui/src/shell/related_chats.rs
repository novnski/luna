//! Related chats: one right-pane list of the chats a conversation spawned.
//!
//! Two kinds share it, each marked on its row:
//! - **Child chats** are started by the conversation's agent, with fresh
//!   context. The agent creates and manages them; the user follows them.
//! - **Side chats** are started by the user from a copy of the
//!   conversation's context. Only the user creates (and ends) them.
//!
//! Rows are the sidebar's own task row ([`Shell::render_chat_row`]) in its
//! sections (Pinned, the active rows, Settled, Archived). A row drills into
//! that chat's pane view: the child panel ([`super::child_chats`]) or the side
//! panel ([`super::side_chats`]); Back returns here, and Back from here
//! returns to the surface menu.

use super::side_chats::SideListSnapshot;
use super::*;
use luna_proto::ChatIndicator;

/// Who started a related chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelatedKind {
    /// Started by the parent conversation's agent, with fresh context.
    Child,
    /// Started by the user from a copy of the parent's context.
    Side,
}

impl RelatedKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Child => "Child chat",
            Self::Side => "Side chat",
        }
    }

    pub(super) fn icon(self) -> &'static str {
        match self {
            Self::Child => icons::BOT,
            Self::Side => icons::GIT_FORK,
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Self::Child => "Started by this chat's agent, with fresh context",
            Self::Side => "Started by you from a copy of this chat's context",
        }
    }
}

/// One side chat for the list: its list metadata merged by id with its
/// `WatchChats` row. A row published before (or without) a list page still
/// lists, from the chat row; the row's state is the fresher one.
struct SideItem {
    id: String,
    state: String,
    title: SharedString,
    /// Last activity: the later of the chat row's last message and the
    /// list's `lastActivityMs`, never before creation.
    recency_ms: i64,
    expires_at_ms: i64,
    messages: Option<usize>,
    harness: Option<luna_proto::HarnessId>,
    model: Option<String>,
}

/// One rendered row before sectioning: pins keep their own order (synced
/// task pins first, then side chats by pin time); the rest sort newest first.
struct RelatedEntry {
    id: String,
    section: RelatedSection,
    pin: Option<(u8, String)>,
    recency_ms: i64,
    element: AnyElement,
}

/// Sections of a related-chat list, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RelatedSection {
    Pinned,
    Active,
    Settled,
    Archived,
}

impl RelatedSection {
    fn label(self) -> &'static str {
        match self {
            Self::Pinned => "Pinned",
            Self::Active => "Active",
            Self::Settled => "Settled",
            Self::Archived => "Archived",
        }
    }

    /// Settled and Archived start folded, like the sidebar's shelves.
    fn collapsible(self) -> bool {
        matches!(self, Self::Settled | Self::Archived)
    }
}

impl Shell {
    /// Show the surface menu over this chat's open tabs. The tabs keep their
    /// state (a parked side chat keeps its lease); clicking one returns.
    pub(super) fn show_surface_picker(&mut self, cx: &mut Context<Self>) {
        self.suspend_subagent_watches(Some(RightSurface::Picker), cx);
        self.suspend_file_images(cx);
        let key = self.panel_key(cx);
        self.panels.update(&key, |panel| panel.picker_open = true);
        cx.notify();
    }

    /// Toolbar of a related-chat list: Back to the surface menu, the title,
    /// then optional trailing controls.
    pub(super) fn render_related_list_header(
        &self,
        id: &'static str,
        title: SharedString,
        trailing: Vec<AnyElement>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let muted = theme.text_muted;
        let border_strong = theme.border_strong;
        let hover = crate::theme::ink(0.06);
        let back = div()
            .id(SharedString::from(format!("{id}-back")))
            .role(gpui::Role::Button)
            .aria_label("Back to panels")
            .tab_index(0)
            .size(px(crate::surface_chrome::CONTROL_SIZE))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
            .border_1()
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(border_strong))
            .tooltip(|_, cx| crate::pickers::ControlTooltip::view("Back to panels", None, cx))
            .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
            .on_click(cx.listener(|this, _, _, cx| this.show_surface_picker(cx)))
            .on_key_down(crate::swallow_activation_key)
            .child(
                icon(icons::ALT_ARROW_LEFT)
                    .size(px(crate::surface_chrome::ICON_SIZE))
                    .text_color(muted),
            );
        crate::surface_chrome::toolbar(theme)
            .child(back)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(12.0))
                    .child(title),
            )
            .children(trailing)
    }

    /// A small text button in a related-list toolbar.
    pub(super) fn related_header_button(
        id: impl Into<SharedString>,
        glyph: &'static str,
        label: &'static str,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let muted = theme.text_muted;
        let border_strong = theme.border_strong;
        let hover = crate::theme::ink(0.06);
        div()
            .id(id.into())
            .role(gpui::Role::Button)
            .aria_label(label)
            .tab_index(0)
            .h(px(crate::surface_chrome::CONTROL_SIZE))
            .px(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.0))
            .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
            .border_1()
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text_muted)
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(border_strong))
            .on_key_down(crate::swallow_activation_key)
            .child(
                icon(glyph)
                    .size(px(crate::surface_chrome::ICON_SIZE))
                    .text_color(muted),
            )
            .child(label)
    }

    /// Sidebar-style sections. `Active` has no header unless pins precede it;
    /// Settled and Archived fold behind a disclosure header with a count.
    pub(super) fn render_related_sections(
        &mut self,
        list: &'static str,
        sections: Vec<(RelatedSection, Vec<(String, AnyElement)>)>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        #[cfg(feature = "appshots-fixture")]
        self.fixture_related_rows.insert(
            list,
            sections
                .iter()
                .flat_map(|(section, rows)| {
                    rows.iter()
                        .map(move |(id, _)| (section.label().to_string(), id.clone()))
                })
                .collect(),
        );
        let has_pins = sections
            .iter()
            .any(|(section, rows)| *section == RelatedSection::Pinned && !rows.is_empty());
        let mut out = Vec::new();
        for (section, rows) in sections {
            if rows.is_empty() {
                continue;
            }
            let count = rows.len();
            let key = format!("{list}:{}", section.label());
            let open = !section.collapsible() || self.related_expanded.contains(&key);
            let show_header = section != RelatedSection::Active || has_pins;
            if show_header {
                let label: SharedString = if open {
                    section.label().into()
                } else {
                    format!("{} ({count})", section.label()).into()
                };
                let chevron = if section.collapsible() {
                    self.sidebar_disclosure_chevron(&format!("related:{key}"), open, theme)
                } else {
                    div().into_any_element()
                };
                let mut header = spaces::sidebar_disclosure_header(theme, label, chevron)
                    .id(SharedString::from(format!("related-section-{key}")));
                if section.collapsible() {
                    header = header.on_click(cx.listener(move |this, _, _, cx| {
                        if !this.related_expanded.remove(&key) {
                            this.related_expanded.insert(key.clone());
                        }
                        cx.notify();
                    }));
                } else {
                    header = header.cursor(gpui::CursorStyle::Arrow);
                }
                out.push(
                    div()
                        .w_full()
                        .pt(px(if out.is_empty() { 4.0 } else { 12.0 }))
                        .child(header)
                        .into_any_element(),
                );
            }
            if open {
                out.push(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .pt(px(if show_header { 4.0 } else { 0.0 }))
                        .children(rows.into_iter().map(|(_, row)| row))
                        .into_any_element(),
                );
            }
        }
        out
    }
}

impl Shell {
    /// Non-archived child and side chats per parent, deduplicated by id. Side
    /// chats come from `WatchChats` side rows (every non-deleted one, active
    /// or ended, on current engines) and every side list loaded this run.
    pub(super) fn related_chat_counts(&self, cx: &App) -> std::collections::HashMap<String, usize> {
        use std::collections::{HashMap, HashSet};
        let mut ids: HashMap<&str, HashSet<&str>> = HashMap::new();
        for chat in self
            .state
            .read(cx)
            .chats
            .iter()
            .filter(|chat| !chat.archived)
        {
            if let Some(child) = &chat.child_thread
                && child.parent_task_id != chat.id
            {
                ids.entry(child.parent_task_id.as_str())
                    .or_default()
                    .insert(chat.id.as_str());
            }
        }
        let side_rows = self
            .state
            .read(cx)
            .side_chat_rows
            .iter()
            .map(|(id, row)| (row.parent_id.as_str(), id.as_str()));
        let side_lists = self
            .side_chat_index
            .iter()
            .flat_map(|(parent, sides)| sides.iter().map(move |id| (parent.as_str(), id.as_str())));
        let gone = &self.state.read(cx).side_chat_gone;
        for (parent, id) in side_rows.chain(side_lists) {
            if gone.contains(id) {
                continue;
            }
            let archived = self
                .settings
                .side_chat_marks
                .get(id)
                .is_some_and(|marks| marks.archived);
            if !archived {
                ids.entry(parent).or_default().insert(id);
            }
        }
        ids.into_iter()
            .map(|(parent, set)| (parent.to_string(), set.len()))
            .collect()
    }

    /// This parent's side chats: list metadata (every loaded page) merged by
    /// id with the side rows `WatchChats` publishes for it.
    fn side_items(&self, side: &SideListSnapshot, parent: &str, cx: &App) -> Vec<SideItem> {
        let state = self.state.read(cx);
        let chat = |id: &str| state.chats.iter().find(|chat| chat.id == id);
        let activity = |id: &str| {
            chat(id).map(|chat| {
                chat.last_message_at
                    .unwrap_or(chat.created_at)
                    .timestamp_millis()
            })
        };
        let mut items: Vec<SideItem> = side
            .items
            .iter()
            .filter(|meta| !state.side_chat_gone.contains(&meta.id))
            .map(|meta| {
                let title = meta
                    .preview
                    .clone()
                    .or_else(|| {
                        meta.anchor_text
                            .as_deref()
                            .map(transcript::single_line)
                            .filter(|text| !text.trim().is_empty())
                            .map(|text| format!("“{}”", text.trim()))
                    })
                    .unwrap_or_else(|| meta.title.clone());
                SideItem {
                    id: meta.id.clone(),
                    state: state
                        .side_chat_rows
                        .get(&meta.id)
                        .and_then(|row| row.state.clone())
                        .unwrap_or_else(|| meta.state.clone()),
                    title: title.into(),
                    recency_ms: activity(meta.id.as_str())
                        .unwrap_or_default()
                        .max(meta.last_activity_ms)
                        .max(meta.created_at_ms),
                    expires_at_ms: meta.expires_at_ms,
                    messages: Some(meta.message_count),
                    harness: Some(meta.config.harness),
                    model: meta.config.model.clone(),
                }
            })
            .collect();
        for (id, row) in &state.side_chat_rows {
            if row.parent_id != parent || items.iter().any(|item| item.id == *id) {
                continue;
            }
            let Some(chat) = chat(id.as_str()) else {
                continue;
            };
            items.push(SideItem {
                id: id.clone(),
                state: row.state.clone().unwrap_or_else(|| "active".into()),
                title: transcript::single_line(chat.title.as_deref().unwrap_or("Side chat")).into(),
                recency_ms: chat
                    .last_message_at
                    .unwrap_or(chat.created_at)
                    .timestamp_millis(),
                expires_at_ms: 0,
                messages: None,
                harness: chat.config.as_ref().map(|config| config.harness),
                model: chat.config.as_ref().and_then(|config| config.model.clone()),
            });
        }
        items
    }

    pub(super) fn active_related_chat_count(&self, cx: &App) -> usize {
        if self.active_chat.is_empty() {
            return 0;
        }
        self.related_chat_counts(cx)
            .get(&self.active_chat)
            .copied()
            .unwrap_or(0)
    }

    /// The Related chats surface: the open chat when a row was opened (the
    /// most recently opened kind when both are), else the list.
    pub(super) fn render_related_chats(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let child = self.ensure_child_chat_panel(cx);
        let side = self.ensure_side_chat_panel(cx);
        let child_open = child.read(cx).open_chat_id().is_some();
        let side_open = side.read(cx).has_page();
        if side_open && (self.related_focus == RelatedKind::Side || !child_open) {
            return side.into_any_element();
        }
        if child_open {
            return child.into_any_element();
        }
        let snapshot = side.read(cx).list_snapshot();
        self.render_related_list(snapshot, cx)
    }

    fn render_related_list(
        &mut self,
        side: SideListSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let parent = self.active_chat.clone();
        let mut attention = self.activity_task_keys(cx);
        attention.extend(self.settlement_blocking_task_keys());
        let auto_settle = self.settings.sidebar_auto_settle_after_days;
        let show_harness = self.settings.sidebar_show_harness;
        let (parent_title, parent_project, device, children) = {
            let state = self.state.read(cx);
            let parent_chat = state.chats.iter().find(|chat| chat.id == parent);
            let title = parent_chat
                .and_then(|chat| chat.title.clone())
                .unwrap_or_else(|| "Conversation".into());
            let project = parent_chat
                .and_then(|chat| state.space_for_chat(chat))
                .map(|space| space.display_name().to_string())
                .unwrap_or_else(|| "Unknown project".into());
            let device = state
                .local_device_id
                .as_deref()
                .and_then(|id| state.device_name(id))
                .unwrap_or("This device")
                .to_string();
            let children: Vec<luna_proto::Chat> = state
                .chats
                .iter()
                .filter(|chat| {
                    chat.id != parent
                        && chat
                            .child_thread
                            .as_ref()
                            .is_some_and(|child| child.parent_task_id == parent)
                })
                .cloned()
                .collect();
            (transcript::single_line(&title), project, device, children)
        };
        let mut entries: Vec<RelatedEntry> = Vec::new();

        for chat in children {
            let (status, project, device, branch, change_request) = {
                let state = self.state.read(cx);
                (
                    state.display_status_for(&chat, now),
                    state
                        .space_for_chat(&chat)
                        .map(|space| space.display_name().to_string())
                        .unwrap_or_else(|| "Unknown project".into()),
                    state
                        .device_name(&chat.device_id)
                        .unwrap_or("Unknown device")
                        .to_string(),
                    crate::change_requests::conversation_branch(&chat, &state.spaces)
                        .map(str::trim)
                        .filter(|branch| !branch.is_empty())
                        .map(SharedString::from),
                    state.change_request_for_chat(&chat).cloned(),
                )
            };
            let has_attention = attention.contains(&(chat.device_id.clone(), chat.id.clone()));
            let pending = self.pending_task_actions.get(&chat.id).copied();
            let settled = matches!(pending, Some(TaskConfirmation::Settle))
                || spaces::is_settled_chat(&chat, status, now, auto_settle, has_attention);
            let can_settle =
                !settled && spaces::can_manually_settle_chat(&chat, status, has_attention, false);
            let archived = chat.archived || matches!(pending, Some(TaskConfirmation::Archive));
            let title: SharedString =
                transcript::single_line(chat.title.as_deref().unwrap_or("New child chat")).into();
            let details = spaces::sidebar_thread_tooltip(
                &chat,
                title.clone(),
                project.clone().into(),
                device.into(),
                branch.clone(),
                Some(RelatedKind::Child.description().into()),
            );
            let element = self.render_chat_row(
                ChatRowProps {
                    id: chat.id.clone(),
                    title,
                    time_ago: format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), now)
                        .into(),
                    space_name: project.into(),
                    branch: branch.filter(|_| self.settings.sidebar_show_branch),
                    note: None,
                    change_request: change_request
                        .filter(|_| self.settings.sidebar_show_pull_request),
                    harness: show_harness
                        .then(|| chat.config.as_ref().map(|config| config.harness))
                        .flatten(),
                    status,
                    status_label: None,
                    selected: false,
                    archived: chat.archived,
                    pinned: chat.pinned_order.is_some(),
                    surface: ChatRowSurface::ChildPane,
                    kind: Some(RelatedKind::Child),
                    can_settle,
                    settled: settled && !chat.archived,
                    // A grandchild list belongs to that child: open it there.
                    child_chats: 0,
                    jump_label: None,
                    search_query: None,
                    details: Some(details),
                    drag: None,
                },
                &theme,
                cx,
            );
            let section = if archived {
                RelatedSection::Archived
            } else if chat.pinned_order.is_some() {
                RelatedSection::Pinned
            } else if settled {
                RelatedSection::Settled
            } else {
                RelatedSection::Active
            };
            entries.push(RelatedEntry {
                id: chat.id.clone(),
                section,
                pin: chat.pinned_order.clone().map(|key| (0, key)),
                recency_ms: chat
                    .last_message_at
                    .unwrap_or(chat.created_at)
                    .timestamp_millis(),
                element,
            });
        }

        for item in self.side_items(&side, &parent, cx) {
            let marks = self
                .settings
                .side_chat_marks
                .get(&item.id)
                .copied()
                .unwrap_or_default();
            let (status, status_label) = match item.state.as_str() {
                "preparing" => (ChatIndicator::Working, Some("Preparing")),
                "closing" => (
                    ChatIndicator::Working,
                    Some(if side.persistent { "Ending" } else { "Saving" }),
                ),
                "creationFailed" | "closeFailed" => (ChatIndicator::Errored, Some("Failed")),
                _ => (ChatIndicator::Idle, None),
            };
            // Older engines expire a side chat's lease; a persistent one is
            // writable until the user ends it.
            let live = matches!(item.state.as_str(), "active" | "preparing")
                && (side.persistent
                    || item.expires_at_ms == 0
                    || now.timestamp_millis() < item.expires_at_ms);
            let state = match item.state.as_str() {
                "active" if side.persistent => None,
                _ if side.parked.contains(&item.id) => Some("Writable"),
                state => Some(super::side_chats::side_state_label(state)),
            };
            let count = item.messages.map(|messages| match messages {
                0 => "No messages yet".to_string(),
                1 => "1 message".to_string(),
                count => format!("{count} messages"),
            });
            let note: SharedString = match (state, count) {
                (Some(state), Some(count)) => format!("{state} · {count}"),
                (Some(state), None) => state.to_string(),
                (None, Some(count)) => count,
                (None, None) => "Active".to_string(),
            }
            .into();
            let at = chrono::DateTime::from_timestamp_millis(item.recency_ms).unwrap_or(now);
            let model: SharedString = {
                let provider = item
                    .harness
                    .map(super::child_chats::provider_label)
                    .unwrap_or("Provider");
                match item.model.as_deref().filter(|model| !model.is_empty()) {
                    Some(model) => format!("{provider} · {model}").into(),
                    None => provider.into(),
                }
            };
            let details = SidebarThreadTooltip {
                title: item.title.clone(),
                project: parent_project.clone().into(),
                device: device.clone().into(),
                model: Some(model),
                branch: None,
                status: Some(format!("{} · {note}", RelatedKind::Side.description()).into()),
            };
            let pinned_at = marks.pinned_at_ms;
            let element = self.render_chat_row(
                ChatRowProps {
                    id: item.id.clone(),
                    title: item.title,
                    time_ago: format_time_ago(at, now).into(),
                    space_name: parent_project.clone().into(),
                    branch: None,
                    note: Some(note),
                    change_request: None,
                    harness: item.harness.filter(|_| show_harness),
                    status,
                    status_label,
                    selected: false,
                    archived: marks.archived,
                    pinned: pinned_at.is_some(),
                    surface: ChatRowSurface::SidePane,
                    kind: Some(RelatedKind::Side),
                    can_settle: (side.persistent || !live)
                        && !marks.settled
                        && !marks.archived
                        && pinned_at.is_none()
                        && status == ChatIndicator::Idle,
                    settled: marks.settled && !marks.archived,
                    child_chats: 0,
                    jump_label: None,
                    search_query: None,
                    details: Some(details),
                    drag: None,
                },
                &theme,
                cx,
            );
            let section = if marks.archived {
                RelatedSection::Archived
            } else if pinned_at.is_some() {
                RelatedSection::Pinned
            } else if marks.settled {
                RelatedSection::Settled
            } else {
                RelatedSection::Active
            };
            entries.push(RelatedEntry {
                id: item.id,
                section,
                pin: pinned_at.map(|at| (1, format!("{at:020}"))),
                recency_ms: item.recency_ms,
                element,
            });
        }

        entries.sort_by(|left, right| {
            match (&left.pin, &right.pin) {
                (Some((left_kind, left_key)), Some((right_kind, right_key))) => {
                    left_kind.cmp(right_kind).then_with(|| {
                        if *left_kind == 0 {
                            luna_proto::compare_pin_order(left_key, right_key)
                        } else {
                            left_key.cmp(right_key)
                        }
                    })
                }
                _ => right.recency_ms.cmp(&left.recency_ms),
            }
            .then_with(|| left.id.cmp(&right.id))
        });
        let empty = entries.is_empty();
        let mut by_section: Vec<(RelatedSection, Vec<(String, AnyElement)>)> = [
            RelatedSection::Pinned,
            RelatedSection::Active,
            RelatedSection::Settled,
            RelatedSection::Archived,
        ]
        .into_iter()
        .map(|section| (section, Vec::new()))
        .collect();
        for entry in entries {
            if let Some((_, rows)) = by_section
                .iter_mut()
                .find(|(section, _)| *section == entry.section)
            {
                rows.push((entry.id, entry.element));
            }
        }
        let sections = self.render_related_sections("related", by_section, &theme, cx);

        let refresh = Self::related_icon_button(
            "related-chats-refresh",
            icons::REFRESH,
            "Refresh side chats",
            &theme,
        )
        .when(side.busy, |el| el.opacity(0.5))
        .on_click(cx.listener(|this, _, _, cx| this.refresh_side_chats(cx)))
        .into_any_element();
        // While a create is unresolved the header retries that same request;
        // a genuinely new side chat is the notice's explicit choice below.
        let new = match side.pending_create {
            Some(snapshot) => Self::related_header_button(
                "related-chats-retry-side",
                icons::REFRESH,
                "Retry side chat",
                &theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.start_side_chat(snapshot, cx))),
            None => Self::related_header_button(
                "related-chats-new-side",
                icons::PLUS,
                "New side chat",
                &theme,
            )
            .on_click(cx.listener(|this, _, _, cx| this.start_side_chat(false, cx))),
        }
        .when(side.busy, |el| el.opacity(0.5))
        .into_any_element();
        let header = self.render_related_list_header(
            "related-chats",
            format!("Related chats · {parent_title}").into(),
            vec![refresh, new],
            &theme,
            cx,
        );

        let quiet = |text: SharedString| {
            div()
                .px(px(Theme::SPACE_SM))
                .py(px(6.0))
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(theme.text_muted)
                .child(text)
        };
        let mut list = div()
            .id("related-chats-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(Theme::SPACE_SM))
            .pt(px(4.0))
            .pb(px(12.0))
            .flex()
            .flex_col();
        if let Some(operation) = side.operation {
            list = list.child(quiet(operation.into()));
        }
        if let Some(error) = side.error.clone() {
            list = list.child(
                div()
                    .px(px(Theme::SPACE_SM))
                    .py(px(6.0))
                    .child(crate::settings::widgets::error_strip(&theme, error)),
            );
        }
        if let (Some(snapshot), false) = (side.pending_create, side.busy) {
            // Unresolved: the side chat may already exist. Inspect first;
            // retry is the same request; a new one is a deliberate choice.
            list = list.child(
                div()
                    .px(px(Theme::SPACE_SM))
                    .py(px(6.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(div().text_color(theme.text).child("Side chat not confirmed"))
                    .child("Your last request may have created it. Refresh to check before starting another; Retry repeats the same request and never makes a second one.")
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(4.0))
                            .child(
                                Self::related_header_button(
                                    "related-chats-unresolved-refresh",
                                    icons::REFRESH,
                                    "Refresh to check",
                                    &theme,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.refresh_side_chats(cx))),
                            )
                            .child(
                                Self::related_header_button(
                                    "related-chats-unresolved-retry",
                                    icons::RESTART,
                                    "Retry same request",
                                    &theme,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_side_chat(snapshot, cx)
                                })),
                            )
                            .child(
                                Self::related_header_button(
                                    "related-chats-unresolved-new",
                                    icons::PLUS,
                                    "Start a new side chat",
                                    &theme,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.start_new_side_chat_instead(cx)
                                })),
                            ),
                    ),
            );
        } else if side.error.is_some() && !side.busy {
            list = list.child(
                div()
                    .px(px(Theme::SPACE_SM))
                    .pb(px(6.0))
                    .flex()
                    .flex_row()
                    .gap(px(4.0))
                    .child(
                        Self::related_header_button(
                            "related-chats-retry",
                            icons::REFRESH,
                            "Refresh",
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_side_chats(cx))),
                    )
                    .when(side.snapshot_alternative, |el| {
                        el.child(
                            Self::related_header_button(
                                "related-chats-new-snapshot",
                                icons::PLUS,
                                "Start with recent context",
                                &theme,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.start_side_chat(true, cx))),
                        )
                    }),
            );
        }
        if empty && side.loaded && side.error.is_none() {
            list = list.child(self.render_related_empty(&theme, cx));
        }
        list = list.children(sections);
        if !side.loaded && side.error.is_none() {
            list = list.child(quiet("Loading side chats…".into()));
        } else if side.more {
            list = list.child(quiet("Loading earlier side chats…".into()));
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.text)
            .child(header)
            .child(list)
            .into_any_element()
    }

    /// Nothing related yet: say what each kind is, and offer the one the
    /// user can start.
    fn render_related_empty(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let kind_line = |kind: RelatedKind, text: &'static str| {
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(8.0))
                .child(
                    icon(kind.icon())
                        .size(px(13.0))
                        .flex_none()
                        .mt(px(1.0))
                        .text_color(theme.text_muted),
                )
                .child(div().flex_1().min_w_0().child(text))
        };
        div()
            .px(px(Theme::SPACE_SM))
            .py(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text_muted)
            .child(
                div()
                    .text_color(theme.text)
                    .child("No related chats yet"),
            )
            .child(kind_line(
                RelatedKind::Child,
                "Child chats appear when this chat's agent starts one. Each begins with fresh context.",
            ))
            .child(kind_line(
                RelatedKind::Side,
                "Side chats are yours: ask something on the side from a copy of this chat's context.",
            ))
            .child(
                div().pt(px(2.0)).child(
                    Self::related_header_button(
                        "related-chats-empty-new-side",
                        icons::PLUS,
                        "New side chat",
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.start_side_chat(false, cx))),
                ),
            )
            .into_any_element()
    }

    /// An icon-only toolbar control with a tooltip naming it.
    fn related_icon_button(
        id: &'static str,
        glyph: &'static str,
        label: &'static str,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let border_strong = theme.border_strong;
        let hover = crate::theme::ink(0.06);
        div()
            .id(id)
            .role(gpui::Role::Button)
            .aria_label(label)
            .tab_index(0)
            .size(px(crate::surface_chrome::CONTROL_SIZE))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
            .border_1()
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(border_strong))
            .tooltip(move |_, cx| crate::pickers::ControlTooltip::view(label, None, cx))
            .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
            .on_key_down(crate::swallow_activation_key)
            .child(
                icon(glyph)
                    .size(px(crate::surface_chrome::ICON_SIZE))
                    .text_color(theme.text_muted),
            )
    }
}

/// Headless QA reads the related lists, the sidebar order and the pane's
/// navigation state through this hook.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_related_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        let key = self.panel_key(cx);
        let state = self.state.read(cx);
        let mut pinned: Vec<&luna_proto::Chat> = state
            .chats
            .iter()
            .filter(|chat| !chat.archived && chat.pinned_order.is_some())
            .collect();
        pinned.sort_by(|left, right| {
            spaces::compare_sidebar_chats(SidebarSort::LastUpdated, left, right)
        });
        serde_json::json!({
            "sidebarOrder": self.sidebar_visible_order(cx),
            "sidebarSort": format!("{:?}", self.settings.sidebar_sort),
            "manualOrder": self.settings.sidebar_manual_order,
            "pinnedOrder": pinned.iter().map(|chat| chat.id.clone()).collect::<Vec<_>>(),
            "hover": self.chat_status_hover,
            "drag": self.sidebar_drag.as_ref().map(|drag| serde_json::json!({
                "chat": drag.chat_id,
                "target": drag.target.as_ref().map(|(anchor, place)| serde_json::json!([anchor, format!("{place:?}")])),
            })),
            "notice": self.sidebar_notice.as_ref().map(|notice| notice.message.to_string()),
            "rightActive": format!("{:?}", self.resolved_right_active(cx)),
            "pickerOpen": self.panels.get(&key).picker_open,
            "rightTabs": self
                .right_tabs
                .get(&key)
                .map(|tabs| tabs.iter().map(|tab| format!("{tab:?}")).collect::<Vec<_>>())
                .unwrap_or_default(),
            "related": self
                .fixture_related_rows
                .iter()
                .map(|(list, rows)| {
                    (
                        list.to_string(),
                        serde_json::json!(
                            rows.iter()
                                .map(|(section, id)| serde_json::json!([section, id]))
                                .collect::<Vec<_>>()
                        ),
                    )
                })
                .collect::<serde_json::Map<_, _>>(),
            "childOpen": self
                .child_chat_panel
                .as_ref()
                .and_then(|panel| panel.read(cx).open_chat_id().map(str::to_string)),
            "sideMarks": self.settings.side_chat_marks,
            "relatedCounts": self.related_chat_counts(cx),
            "pendingActions": self.pending_task_actions.keys().cloned().collect::<Vec<_>>(),
        })
    }
}
