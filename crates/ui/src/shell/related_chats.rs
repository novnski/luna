//! Related chats: one right-pane list of the chats a conversation spawned.
//!
//! Two kinds share it, each marked on its row:
//! - **Child chats** start with fresh context, linked to the conversation:
//!   the user opens a blank one with `+` (New child chat), or the
//!   conversation's agent starts one (`create_thread`).
//! - **Side chats** are persistent forks the user started from a copy of
//!   the conversation's context. Once created a fork is an ordinary thread
//!   (also a sidebar row); its source is a link, never a lifetime.
//!
//! The library covers the whole lineage below a conversation: children and
//! forks, forks of forks and children of forks, each naming its actual
//! source when that is not this conversation. Rows are the sidebar's own
//! task row ([`Shell::render_chat_row`]) with its ordinary actions, in its
//! sections (Pinned, the active rows, Settled, Archived). With the
//! workbench a row opens a center tab; without it, a child drills into the
//! child panel ([`super::child_chats`]) and a fork opens as the chat.

use super::side_chats::SideCreateSnapshot;
use super::*;
use std::collections::{HashMap, HashSet};

/// Who started a related chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelatedKind {
    /// A child of the parent conversation, with fresh context: started by
    /// its agent (`create_thread`) or by the user (`+`, New child chat).
    Child,
    /// Started by the user from a copy of the parent's context.
    Side,
    /// An ordinary chat the user listed beside this one: no lineage.
    Alongside,
}

impl RelatedKind {
    /// What the UI calls this kind.
    pub(super) fn display(self) -> &'static str {
        match self {
            Self::Child => "New thread",
            Self::Side => "Side chat",
            Self::Alongside => "Alongside",
        }
    }

    /// The stable identifier QA readback uses.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Child => "Child chat",
            Self::Side => "Side chat",
            Self::Alongside => "Alongside",
        }
    }

    pub(super) fn icon(self) -> &'static str {
        match self {
            Self::Child => icons::BOT,
            Self::Side => icons::FORK,
            Self::Alongside => icons::SPLIT_COLUMNS,
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Self::Child => "A new thread from this chat; its context is not carried over",
            // A nested row says its actual source separately.
            Self::Side => "Carries this chat's context, captured when it was created",
            Self::Alongside => "Your own chat, listed beside this one (not its child)",
        }
    }
}

/// Who started a child, from its engine-recorded provisioning: the parent's
/// agent or the user. Older rows without provisioning say neither.
pub(super) fn child_origin(chat: &luna_proto::Chat) -> &'static str {
    match chat
        .child_thread
        .as_ref()
        .and_then(|child| child.provisioning.as_ref())
        .map(|provisioning| provisioning.created_by)
    {
        Some(luna_proto::ChildCreator::User) => {
            "A new thread you started from this chat; its context is not carried over"
        }
        Some(luna_proto::ChildCreator::Agent) => {
            "A new thread this chat's agent started; its context is not carried over"
        }
        None => "A new thread from this chat; its context is not carried over",
    }
}

/// The breadcrumb's account of a child's parent, from the same record.
pub(super) fn parent_origin(chat: &luna_proto::Chat) -> &'static str {
    match chat
        .child_thread
        .as_ref()
        .and_then(|child| child.provisioning.as_ref())
        .map(|provisioning| provisioning.created_by)
    {
        Some(luna_proto::ChildCreator::User) => {
            "This thread's parent: you started this new thread from it"
        }
        Some(luna_proto::ChildCreator::Agent) => {
            "This thread's parent: its agent started this new thread"
        }
        None => "This thread's parent: the chat it was created from",
    }
}

/// One rendered row before sectioning: pins keep their synced order; the
/// rest sort newest first.
struct RelatedEntry {
    id: String,
    section: RelatedSection,
    pin: Option<String>,
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
    /// state; clicking one returns.
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
            .hover_wash(format!("{id}-back"))
            .focus_visible({
                let ring = theme.focus_ring();
                move |style| crate::theme::focus_ring(style, ring)
            })
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
        let id: SharedString = id.into();
        div()
            .id(id.clone())
            .role(gpui::Role::Button)
            .aria_label(label)
            .tab_index(0)
            .h(px(crate::surface_chrome::CONTROL_SIZE))
            .px(crate::typography::ui_rems(6.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(crate::typography::ui_rems(4.0))
            .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
            .border_1()
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text_muted)
            .hover_wash(id)
            .focus_visible({
                let ring = theme.focus_ring();
                move |style| crate::theme::focus_ring(style, ring)
            })
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
                let mut header = spaces::sidebar_disclosure_header(
                    SharedString::from(format!("related-section-{key}")),
                    theme,
                    label,
                    chevron,
                );
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
    /// Each chat's directly related chats: its children (except those
    /// listed in the sidebar), the forks started from it (materialized:
    /// ordinary rows), and the chats placed alongside it. A fork is listed
    /// under its source even when it is placed elsewhere too.
    fn related_edges(&self, cx: &App) -> HashMap<String, Vec<(String, RelatedKind)>> {
        let placed = self.effective_placements(cx);
        let state = self.state.read(cx);
        let mut edges: HashMap<String, Vec<(String, RelatedKind)>> = HashMap::new();
        for chat in &state.chats {
            let mut push = |parent: &str, kind: RelatedKind| {
                if parent != chat.id {
                    edges
                        .entry(parent.to_string())
                        .or_default()
                        .push((chat.id.clone(), kind));
                }
            };
            if let Some(source) = super::side_chats::fork_source(state, &chat.id) {
                push(source, RelatedKind::Side);
            } else if let Some(parent) = super::placement::lineage_parent_id(chat)
                && !placed.is_promoted(&chat.id)
            {
                push(parent, RelatedKind::Child);
                continue;
            }
            // Effective docks already name an existing, unarchived host.
            if let Some(host) = placed.host(&chat.id) {
                push(host, RelatedKind::Alongside);
            }
        }
        edges
    }

    /// What `parent`'s library lists, in lineage order: `(chat, kind, the
    /// actual source when it is not `parent`)`. Children and forks bring
    /// their own descendants (a fork of a fork, a child of a fork); an
    /// alongside chat's descendants stay in its own library. Main's library
    /// also lists the forks of every chat in its workbench (open or
    /// recently closed), so a closed fork tab stays reopenable here.
    fn related_tree(
        &self,
        parent: &str,
        edges: &HashMap<String, Vec<(String, RelatedKind)>>,
        cx: &App,
    ) -> Vec<(String, RelatedKind, Option<String>)> {
        fn visit(
            node: &str,
            parent: &str,
            edges: &HashMap<String, Vec<(String, RelatedKind)>>,
            seen: &mut HashSet<String>,
            out: &mut Vec<(String, RelatedKind, Option<String>)>,
        ) {
            for (id, kind) in edges.get(node).into_iter().flatten() {
                if *kind == RelatedKind::Alongside && node != parent {
                    continue;
                }
                if !seen.insert(id.clone()) {
                    continue;
                }
                out.push((
                    id.clone(),
                    *kind,
                    (node != parent).then(|| node.to_string()),
                ));
                if *kind != RelatedKind::Alongside {
                    visit(id, parent, edges, seen, out);
                }
            }
        }
        let mut seen = HashSet::from([parent.to_string()]);
        let mut out = Vec::new();
        visit(parent, parent, edges, &mut seen, &mut out);
        if self.workbench.layout().main_chat_id == parent {
            let state = self.state.read(cx);
            for id in self.workbench_chat_ids() {
                if id == parent {
                    continue;
                }
                // A fork in the workbench lists here with its descendants.
                if let Some(source) = super::side_chats::fork_source(state, &id)
                    && super::side_chats::side_chat_first_class(state, &id)
                    && seen.insert(id.clone())
                {
                    let via = (source != parent).then(|| source.to_string());
                    out.push((id.clone(), RelatedKind::Side, via));
                    visit(&id, parent, edges, &mut seen, &mut out);
                }
                // So do the forks started from any chat in it.
                for (fork, kind) in edges.get(&id).into_iter().flatten() {
                    if *kind == RelatedKind::Side && seen.insert(fork.clone()) {
                        out.push((fork.clone(), RelatedKind::Side, Some(id.clone())));
                        visit(fork, parent, edges, &mut seen, &mut out);
                    }
                }
            }
        }
        out
    }

    /// Non-archived rows each chat's Related chats lists (see
    /// [`Self::related_tree`]), deduplicated by id.
    pub(super) fn related_chat_counts(&self, cx: &App) -> HashMap<String, usize> {
        let edges = self.related_edges(cx);
        let archived: HashSet<&str> = self
            .state
            .read(cx)
            .chats
            .iter()
            .filter(|chat| chat.archived)
            .map(|chat| chat.id.as_str())
            .collect();
        let mut parents: HashSet<String> = edges.keys().cloned().collect();
        let main = self.workbench.layout().main_chat_id.clone();
        if !main.is_empty() {
            parents.insert(main);
        }
        parents
            .into_iter()
            .filter_map(|parent| {
                let count = self
                    .related_tree(&parent, &edges, cx)
                    .iter()
                    .filter(|(id, _, _)| !archived.contains(id.as_str()))
                    .count();
                (count > 0).then_some((parent, count))
            })
            .collect()
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

    /// The Related chats surface: the open child when one was opened (no
    /// workbench), else the list.
    pub(super) fn render_related_chats(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let side = self.ensure_side_chat_panel(cx).read(cx).snapshot();
        // With the workbench, conversations open as center tabs: this pane is
        // the library and never renders a second conversation.
        if self.workbench_is_open(&self.active_chat).is_some() {
            return self.render_related_list(side, cx);
        }
        let child = self.ensure_child_chat_panel(cx);
        if child.read(cx).open_chat_id().is_some() {
            return child.into_any_element();
        }
        self.render_related_list(side, cx)
    }

    fn render_related_list(
        &mut self,
        side: SideCreateSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let parent = self.active_chat.clone();
        let mut attention = self.activity_task_keys(cx);
        attention.extend(self.settlement_blocking_task_keys());
        let auto_settle = self.settings.sidebar_auto_settle_after_days;
        let show_harness = self.settings.sidebar_show_harness;
        let by_device = self.settings.sidebar_organization == SidebarOrganization::ByDevice;
        let edges = self.related_edges(cx);
        let tree = self.related_tree(&parent, &edges, cx);
        let (parent_title, parent_project, related) = {
            let state = self.state.read(cx);
            let parent_chat = state.chats.iter().find(|chat| chat.id == parent);
            let title = parent_chat
                .and_then(|chat| chat.title.clone())
                .unwrap_or_else(|| "Conversation".into());
            let project = parent_chat
                .and_then(|chat| state.space_for_chat(chat))
                .map(|space| space.display_name().to_string())
                .unwrap_or_else(|| "Unknown project".into());
            let title_of = |id: &str| {
                state
                    .chats
                    .iter()
                    .find(|chat| chat.id == id)
                    .and_then(|chat| chat.title.as_deref())
                    .map(transcript::single_line)
                    .unwrap_or_else(|| "another chat".into())
            };
            let related: Vec<(luna_proto::Chat, RelatedKind, Option<String>)> = tree
                .iter()
                .filter_map(|(id, kind, via)| {
                    let chat = state.chats.iter().find(|chat| chat.id == *id)?;
                    Some((chat.clone(), *kind, via.as_deref().map(title_of)))
                })
                .collect();
            (transcript::single_line(&title), project, related)
        };
        let mut entries: Vec<RelatedEntry> = Vec::new();

        for (chat, kind, via) in related {
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
            let untitled = match kind {
                RelatedKind::Child => "New thread",
                RelatedKind::Side => "Side chat",
                RelatedKind::Alongside => "Untitled chat",
            };
            let title: SharedString =
                transcript::single_line(chat.title.as_deref().unwrap_or(untitled)).into();
            let origin = match kind {
                RelatedKind::Child => child_origin(&chat),
                _ => kind.description(),
            };
            // A nested row (a fork of a fork, a child of a fork) names its
            // actual source.
            let origin = match &via {
                Some(source) => format!("{origin} · from {source}"),
                None => origin.to_string(),
            };
            let details = spaces::sidebar_thread_tooltip(
                &chat,
                title.clone(),
                project.clone().into(),
                device.into(),
                branch.clone(),
                Some(origin.into()),
            );
            let element = self.render_chat_row(
                ChatRowProps {
                    id: chat.id.clone(),
                    title: title.clone(),
                    time_ago: format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), now)
                        .into(),
                    space_name: project.into(),
                    branch: branch.filter(|_| self.settings.sidebar_show_branch),
                    note: via.as_ref().map(|source| format!("from {source}").into()),
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
                    // Every related row is an ordinary thread: open, pin,
                    // settle, archive and delete like any chat.
                    surface: ChatRowSurface::ChildPane,
                    kind: Some(kind),
                    can_settle,
                    settled: settled && !chat.archived,
                    // Nested rows are listed here already.
                    child_chats: 0,
                    jump_label: None,
                    search_query: None,
                    details: Some(details),
                    // Drag to the left sidebar: a child is listed there (still
                    // this chat's child); an alongside chat returns there; a
                    // fork is placed there like any chat.
                    drag: Some(SidebarChatDrag {
                        chat_id: chat.id.clone(),
                        origin: DragOrigin::Related(kind),
                        scope: if chat.pinned_order.is_some() {
                            SidebarDragScope::Pinned
                        } else {
                            SidebarDragScope::Unpinned(by_device.then(|| chat.device_id.clone()))
                        },
                        title,
                        project: parent_project.clone().into(),
                        harness: chat.config.as_ref().map(|config| config.harness),
                    }),
                    workbench_open: self.workbench_is_open(&chat.id),
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
                pin: chat.pinned_order.clone(),
                recency_ms: chat
                    .last_message_at
                    .unwrap_or(chat.created_at)
                    .timestamp_millis(),
                element,
            });
        }

        entries.sort_by(|left, right| {
            match (&left.pin, &right.pin) {
                (Some(left_key), Some(right_key)) => {
                    luna_proto::compare_pin_order(left_key, right_key)
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
            vec![new],
            &theme,
            cx,
        );

        let quiet = |text: SharedString| {
            div()
                .px(px(Theme::SPACE_SM))
                .py(crate::typography::ui_rems(6.0))
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(theme.text_muted)
                .child(text)
        };
        // Drop a sidebar row here to list it alongside this chat (a child
        // listed in the sidebar returns to its parent's list).
        let drop_tint = theme.accent.opacity(0.06);
        let mut list = div()
            .id("related-chats-list")
            .on_drop::<SidebarChatDrag>(cx.listener(|this, drag: &SidebarChatDrag, _, cx| {
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
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(Theme::SPACE_SM))
            .pt(crate::typography::ui_rems(4.0))
            .pb(crate::typography::ui_rems(12.0))
            .flex()
            .flex_col();
        if let Some(operation) = side.operation {
            list = list.child(quiet(operation.into()));
        }
        if let Some(error) = side.error.clone() {
            list = list.child(
                div()
                    .px(px(Theme::SPACE_SM))
                    .py(crate::typography::ui_rems(6.0))
                    .child(crate::settings::widgets::error_strip(&theme, error)),
            );
        }
        if let (Some(snapshot), false) = (side.pending_create, side.busy) {
            // Unresolved: the side chat may already exist (it then appears in
            // this list). Retry is the same request; a new one is a
            // deliberate choice.
            list = list.child(
                div()
                    .px(px(Theme::SPACE_SM))
                    .py(crate::typography::ui_rems(6.0))
                    .flex()
                    .flex_col()
                    .gap(crate::typography::ui_rems(6.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(div().text_color(theme.text).child("Side chat not confirmed"))
                    .child("Your last request may have created it; if it did, it appears in this list. Retry repeats the same request and never makes a second one.")
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(crate::typography::ui_rems(4.0))
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
        } else if side.snapshot_alternative && side.error.is_some() && !side.busy {
            list = list.child(
                div()
                    .px(px(Theme::SPACE_SM))
                    .pb(crate::typography::ui_rems(6.0))
                    .child(
                        Self::related_header_button(
                            "related-chats-new-snapshot",
                            icons::PLUS,
                            "Start with recent context",
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.start_side_chat(true, cx))),
                    ),
            );
        }
        if empty && side.error.is_none() && !side.busy {
            list = list.child(self.render_related_empty(&theme, cx));
        }
        list = list.children(sections);
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
                .gap(crate::typography::ui_rems(8.0))
                .child(
                    icon(kind.icon())
                        .size(crate::typography::ui_rems(13.0))
                        .flex_none()
                        .mt(px(1.0))
                        .text_color(theme.text_muted),
                )
                .child(div().flex_1().min_w_0().child(text))
        };
        div()
            .px(px(Theme::SPACE_SM))
            .py(crate::typography::ui_rems(10.0))
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(8.0))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text_muted)
            .child(
                div()
                    .text_color(theme.text)
                    .child("No related chats yet"),
            )
            .child(kind_line(
                RelatedKind::Child,
                "New threads start fresh: they don't carry this chat's context. Start one with +, or this chat's agent may start one.",
            ))
            .child(kind_line(
                RelatedKind::Side,
                "Side chats carry this chat's context, captured when you start one; later messages here are not copied.",
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
}

/// Headless QA reads the related lists, the sidebar order and the pane's
/// navigation state through this hook.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    /// Write this profile's saved placements as-is (no validation or
    /// pruning), to exercise recovery from a saved invalid state.
    pub fn fixture_set_chat_placements(
        &mut self,
        placements: crate::settings::ChatPlacements,
        cx: &mut Context<Self>,
    ) {
        if let Some(scope) = self.placement_scope(cx) {
            self.settings.chat_placements.insert(scope, placements);
            self.schedule_save(cx);
            cx.notify();
        }
    }

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
            "placementScope": self.placement_scope(cx),
            "placements": self
                .placement_scope(cx)
                .and_then(|scope| self.settings.chat_placements.get(&scope).cloned()),
            "sidebarChildRows": self
                .sidebar_visible_order(cx)
                .into_iter()
                .filter(|id| {
                    state
                        .chats
                        .iter()
                        .find(|chat| chat.id == *id)
                        .and_then(super::placement::lineage_parent_id)
                        .is_some()
                })
                .collect::<Vec<_>>(),
            "lineage": state
                .chats
                .iter()
                .filter_map(|chat| {
                    super::placement::lineage_parent_id(chat)
                        .map(|parent| (chat.id.clone(), serde_json::json!(parent)))
                })
                .collect::<serde_json::Map<_, _>>(),
            // Each fork's source (a link: the source may be deleted).
            "forkSources": state
                .fork_source_rows
                .iter()
                .map(|(id, source)| (id.clone(), serde_json::json!(source)))
                .chain(
                    state
                        .side_chat_rows
                        .iter()
                        .map(|(id, row)| (id.clone(), serde_json::json!(row.parent_id))),
                )
                .collect::<serde_json::Map<_, _>>(),
            // Service side chats only (their tabs carry the side-parent hint).
            "serviceSideChats": state.side_chat_rows.keys().cloned().collect::<Vec<_>>(),
            // Each library's rows as the lineage builds them:
            // `[chat, kind, source when nested]`.
            "relatedTrees": ({
                let edges = self.related_edges(cx);
                let mut parents: Vec<String> = edges.keys().cloned().collect();
                parents.sort();
                parents
                    .into_iter()
                    .map(|parent| {
                        let rows = self
                            .related_tree(&parent, &edges, cx)
                            .into_iter()
                            .map(|(id, kind, via)| serde_json::json!([id, kind.label(), via]))
                            .collect::<Vec<_>>();
                        (parent, serde_json::json!(rows))
                    })
                    .collect::<serde_json::Map<_, _>>()
            }),
            "selected": state.selected_chat,
            "paneIdentity": self
                .child_chat_panel
                .as_ref()
                .and_then(|panel| panel.read(cx).open_identity(cx)),
            "relatedCounts": self.related_chat_counts(cx),
            "pendingActions": self.pending_task_actions.keys().cloned().collect::<Vec<_>>(),
        })
    }
}
